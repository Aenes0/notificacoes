use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ::dbus::Message;
use ::dbus::message::MessageType;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode as PollMode, PostAction, RegistrationToken};

use crate::config::Config;
use crate::dbus::conn::Bus;
use crate::dbus::server::{self, CAPABILITIES, Call, NAME};
use crate::headless::Headless;
use crate::markup::plain;
use crate::model::{HintValue, Hints, parse_actions};
use crate::render::card::Hit;
use crate::store::{Effect, NewNotification, Store};
use crate::timing::display_duration;
use crate::watch::{WatchTag, Watcher, dnd_dir, read_dnd};
use crate::wayland::{SurfaceKey, Ui};
use crate::screen::WaylandScreen;
use crate::sound::{Player, SoundChoice};

// Os `allow` restantes cobrem itens que só as tarefas seguintes consomem (tela, som, USB).
#[allow(dead_code)]
mod config;
mod dbus;
mod fsread;
mod headless;
mod icons;
mod images;
mod markup;
mod model;
mod panel;
mod presenter;
mod privacy;
#[allow(dead_code)]
mod render;
mod screen;
mod sound;
#[allow(dead_code)]
mod store;
mod timing;
mod usb;
mod watch;
mod wayland;

/// Onde as notificações aparecem.
pub enum Screen {
    Headless(Headless),
    Wayland(Box<WaylandScreen>),
}

impl Screen {
    fn show(&mut self, id: u32, total: Duration, now: Instant, store: &Store) {
        match self {
            Screen::Headless(h) => h.show(id, total, now),
            Screen::Wayland(w) => w.show(id, total, now, store),
        }
    }

    fn update(&mut self, id: u32, total: Duration, now: Instant, store: &Store) {
        match self {
            Screen::Headless(h) => h.update(id, total, now),
            Screen::Wayland(w) => w.update(id, total, now, store),
        }
    }

    fn hide(&mut self, id: u32) {
        match self {
            Screen::Headless(h) => h.hide(id),
            Screen::Wayland(w) => w.hide(id),
        }
    }

    fn rearm(&mut self) {
        match self {
            Screen::Headless(h) => h.rearm(),
            Screen::Wayland(w) => w.rearm(),
        }
    }

    fn on_timer(&mut self, now: Instant) -> Vec<u32> {
        match self {
            Screen::Headless(h) => h.on_timer(now),
            Screen::Wayland(w) => w.on_timer(now),
        }
    }
}

/// Evento de ponteiro numa superfície nossa (posição lógica, coordenadas da superfície). `Press` e
/// `Release` são só do botão esquerdo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEv {
    Enter(f32, f32, u32 /*serial*/),
    Motion(f32, f32),
    Leave,
    Press(u32 /*serial*/),
    Release,
}

/// Ação à espera do token de ativação (ou do prazo, aí sem token).
pub struct PendingActivation {
    pub id: u32,
    pub key: String,
    pub deadline: Instant,
}

/// Quanto se espera pelo token antes de invocar a ação sem ele.
const TOKEN_WAIT: Duration = Duration::from_millis(200);

pub struct State {
    store: Store,
    cfg: Config,
    bus: Option<Bus>,
    screen: Screen,
    /// Não perturbe, como o COSMIC o gravou (mantido pelo vigia; sem vigia, relido a cada `Notify`).
    dnd: bool,
    /// Diretório do arquivo `do_not_disturb`.
    dnd_dir: PathBuf,
    /// O inotify está vivo. Se falhou, o arquivo é lido a cada `Notify`.
    dnd_watched: bool,
    handle: LoopHandle<'static, State>,
    /// `NOTIFICACOES_DEBUG=1`: linhas de depuração no stderr (usadas pelos testes de integração).
    debug: bool,
    _panel_fd: Option<OwnedFd>,
    /// Ações à espera do token de ativação.
    pending: Vec<PendingActivation>,
    /// Timer do prazo do token mais próximo; só existe com pedido pendente.
    token_timer: Option<(RegistrationToken, Instant)>,
    /// Tocador de som (um `pw-play` por vez, colhido por pidfd).
    player: Player,
    /// Avisos de USB (só no modo normal e com udev).
    usb: Option<usb::Usb>,
}

impl State {
    pub fn on_message(&mut self, msg: Message) {
        match msg.msg_type() {
            MessageType::MethodCall => match server::decode(&msg) {
                Ok(call) => self.on_call(msg, call),
                Err(e) => {
                    let text = CString::new(e.text).unwrap_or_default();
                    let reply = msg.error(&e.name.into(), &text);
                    self.reply(&msg, reply);
                }
            },
            MessageType::Signal => self.on_signal(&msg),
            // Respostas e erros (inclusive das nossas próprias chamadas) não interessam.
            MessageType::MethodReturn | MessageType::Error => {}
        }
    }

    fn on_signal(&self, msg: &Message) {
        let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
        if !matches!(member.as_str(), "NameAcquired" | "NameLost") {
            return;
        }
        if msg.read1::<&str>().is_ok_and(|name| name == NAME) {
            eprintln!("notificacoes: {member} {NAME}");
        }
    }

    pub fn on_call(&mut self, msg: Message, call: Call) {
        match call {
            Call::Notify { app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout } => {
                let app = self.debug.then(|| app_name.clone());
                let new = NewNotification {
                    app_name,
                    app_icon,
                    summary,
                    body: plain(&body),
                    actions: parse_actions(actions),
                    hints: Hints::parse(hints),
                    expire_timeout,
                };
                let dnd = self.current_dnd();
                let (id, effects) = self.store.notify(new, replaces_id, dnd);
                self.reply(&msg, msg.method_return().append1(id));
                if let Some(app) = app {
                    eprintln!("notificacoes: notify id={id} app={app}");
                }
                self.apply_effects(effects);
            }
            Call::CloseNotification(id) => {
                let effects = self.store.close(id);
                self.reply(&msg, msg.method_return());
                self.apply_effects(effects);
            }
            Call::InvokeAction(id, key) => {
                self.reply(&msg, msg.method_return());
                self.activate(id, key, None);
            }
            Call::GetCapabilities => self.reply(&msg, msg.method_return().append1(CAPABILITIES.to_vec())),
            Call::GetServerInformation => self.reply(
                &msg,
                msg.method_return().append3("notificacoes", "Aenes", env!("CARGO_PKG_VERSION")).append1("1.2"),
            ),
            Call::Introspect => {
                let path = msg.path().map(|p| p.to_string()).unwrap_or_default();
                self.reply(&msg, msg.method_return().append1(server::introspect_xml(&path)));
            }
            Call::Ping => self.reply(&msg, msg.method_return()),
            Call::GetMachineId => {
                let reply = match machine_id() {
                    Some(id) => msg.method_return().append1(id),
                    None => msg.error(&"org.freedesktop.DBus.Error.Failed".into(), c"machine-id ilegível"),
                };
                self.reply(&msg, reply);
            }
        }
    }

    /// O "não perturbe" de agora: o do vigia, ou o arquivo lido na hora se não há vigia.
    fn current_dnd(&mut self) -> bool {
        if !self.dnd_watched {
            self.dnd = read_dnd(&self.dnd_dir);
        }
        self.dnd
    }

    /// Eventos do inotify: `Dnd` relê o arquivo (ligado = tudo que está na tela some sem sinal);
    /// `Config` recarrega a configuração.
    pub fn on_watch(&mut self, tags: &[WatchTag]) {
        for tag in tags {
            match tag {
                WatchTag::Dnd => {
                    self.dnd = read_dnd(&self.dnd_dir);
                    if self.dnd {
                        let effects = self.store.hide_all();
                        self.apply_effects(effects);
                    }
                }
                WatchTag::Config => self.apply_config(config::load()),
            }
        }
    }

    /// Configuração nova: o limite de cartões vale já; os tempos, para os próximos cartões
    /// (`duration_of` lê `self.cfg`).
    fn apply_config(&mut self, cfg: Config) {
        let effects = self.store.set_max_visible(cfg.max_visible);
        self.cfg = cfg;
        self.apply_effects(effects);
    }

    fn reply(&self, to: &Message, reply: Message) {
        if let Some(bus) = &self.bus {
            bus.reply(to, reply);
        }
    }

    /// Funil único dos efeitos do Store: sinais na ordem recebida; tela para o resto.
    pub fn apply_effects(&mut self, effects: Vec<Effect>) {
        let now = Instant::now();
        for effect in effects {
            match effect {
                Effect::Show(id) => {
                    if let Some(total) = self.duration_of(id) {
                        self.screen.show(id, total, now, &self.store);
                        self.play_sound(id);
                    }
                }
                Effect::Update(id) => {
                    if let Some(total) = self.duration_of(id) {
                        self.screen.update(id, total, now, &self.store);
                        self.play_sound(id);
                    }
                }
                Effect::Hide(id) => self.screen.hide(id),
                Effect::Closed(..) | Effect::ActionInvoked(..) | Effect::ActivationToken(..) => {
                    if let Some(bus) = &self.bus {
                        bus.emit(&effect);
                    }
                }
            }
        }
        self.screen.rearm();
    }

    /// Toca o som de `id` (cartões no `Show` e no `Update`; pílula, `suppress-sound`, não perturbe e
    /// `som = nao` ficam em silêncio dentro de `sound::choose`).
    fn play_sound(&mut self, id: u32) {
        let dnd = self.current_dnd();
        let choice = match self.store.get(id) {
            Some(n) => sound::choose(n, &self.cfg, dnd),
            None => return,
        };
        let file = match choice {
            SoundChoice::Silent => return,
            SoundChoice::File(path) => path,
            SoundChoice::Name(name) => match sound::resolve(&sound::sound_dirs(), &name) {
                Some(path) => path,
                None => return,
            },
        };
        self.player.play(&self.handle, &self.cfg.player, &file);
    }

    fn duration_of(&self, id: u32) -> Option<Duration> {
        self.store.get(id).map(|n| display_duration(n.level(), n.expire_timeout, &self.cfg.durations))
    }

    /// O tempo de tela de `id` acabou.
    pub fn on_expire(&mut self, id: u32) {
        let effects = self.store.expire(id);
        self.apply_effects(effects);
    }

    /// O timer da tela venceu: expira os vencidos e rearma.
    pub fn on_screen_timer(&mut self) {
        for id in self.screen.on_timer(Instant::now()) {
            self.on_expire(id);
        }
        self.screen.rearm();
    }

    /// Evento de ponteiro numa superfície nossa. A pílula não recebe ponteiro.
    pub fn on_pointer(&mut self, key: SurfaceKey, ev: PointerEv) {
        let (Screen::Wayland(w), SurfaceKey::Card(id)) = (&mut self.screen, key) else { return };
        match w.on_pointer(id, ev, Instant::now(), &self.store) {
            Some((Hit::Close, _)) => {
                let effects = self.store.dismiss(id);
                self.apply_effects(effects);
            }
            Some((Hit::Default, serial)) => self.activate(id, "default".to_owned(), serial.map(|s| (s, key))),
            Some((Hit::Action(k), serial)) => self.activate(id, k, serial.map(|s| (s, key))),
            // Pausa/retomada mudam o próximo despertar; movimento e press não.
            None if matches!(ev, PointerEv::Enter(..) | PointerEv::Leave) => self.screen.rearm(),
            None => {}
        }
    }

    /// Pede o token de ativação e invoca a ação quando ele chegar (ou no prazo, sem token). Sem
    /// tela Wayland, sem `xdg_activation_v1` ou sem ação com essa chave (o Store só dispensa ou
    /// ignora), invoca já. Um pedido por notificação de cada vez.
    fn activate(&mut self, id: u32, key: String, click: Option<(u32, SurfaceKey)>) {
        if self.pending.iter().any(|p| p.id == id) {
            return;
        }
        let has_key = self.store.get(id).is_some_and(|n| n.actions.iter().any(|a| a.key == key));
        let asked = has_key
            && match &mut self.screen {
                Screen::Wayland(w) => w.ui.request_token(id, click),
                Screen::Headless(_) => false,
            };
        if !asked {
            let effects = self.store.invoke(id, &key, None);
            self.apply_effects(effects);
            return;
        }
        self.pending.push(PendingActivation { id, key, deadline: Instant::now() + TOKEN_WAIT });
        self.rearm_tokens();
    }

    /// O token de `id` chegou (`Some`) ou o prazo venceu (`None`): invoca a ação pendente.
    pub fn on_token(&mut self, id: u32, token: Option<String>) {
        let Some(at) = self.pending.iter().position(|p| p.id == id) else { return };
        let p = self.pending.remove(at);
        if let Some(ui) = self.ui_mut() {
            ui.drop_token(id);
        }
        self.rearm_tokens();
        let effects = self.store.invoke(id, &p.key, token);
        self.apply_effects(effects);
    }

    fn on_token_deadline(&mut self) {
        let now = Instant::now();
        let due: Vec<u32> = self.pending.iter().filter(|p| p.deadline <= now).map(|p| p.id).collect();
        for id in due {
            self.on_token(id, None);
        }
        self.rearm_tokens();
    }

    /// Exatamente um timer no prazo mais próximo, ou nenhum sem pedido pendente.
    fn rearm_tokens(&mut self) {
        let next = self.pending.iter().map(|p| p.deadline).min();
        if next == self.token_timer.map(|(_, at)| at) {
            return;
        }
        if let Some((token, _)) = self.token_timer.take() {
            self.handle.remove(token);
        }
        let Some(at) = next else { return };
        match self.handle.insert_source(Timer::from_deadline(at), |_, _, state: &mut State| {
            // Este timer cai com o `Drop`: não pode ser removido de novo no rearme.
            state.token_timer = None;
            state.on_token_deadline();
            TimeoutAction::Drop
        }) {
            Ok(token) => self.token_timer = Some((token, at)),
            Err(e) => eprintln!("notificacoes: o timer do token não registrou: {}", e.error),
        }
    }

    /// A tela Wayland, se for ela.
    pub(crate) fn ui_mut(&mut self) -> Option<&mut Ui> {
        match &mut self.screen {
            Screen::Wayland(w) => Some(&mut w.ui),
            Screen::Headless(_) => None,
        }
    }

    /// Para os handlers do sctk que precisam devolver referência: eventos Wayland só chegam pela
    /// fila criada em `Ui::connect`, que só existe com a tela Wayland.
    pub(crate) fn ui_or_unreachable(&mut self) -> &mut Ui {
        match &mut self.screen {
            Screen::Wayland(w) => &mut w.ui,
            Screen::Headless(_) => unreachable!("evento Wayland sem tela Wayland"),
        }
    }

    /// A superfície de `key` passou a preferir outra escala (o `Ui` já sabe qual).
    pub(crate) fn on_surface_scale(&mut self, key: SurfaceKey) {
        match (&mut self.screen, key) {
            (Screen::Wayland(w), SurfaceKey::Card(id)) => w.rescale(id, &self.store, Instant::now()),
            (Screen::Wayland(w), SurfaceKey::Pill) => w.rescale_pill(&self.store),
            (Screen::Headless(_), _) => {}
        }
    }

    /// O compositor fechou a superfície de `key` (já solta pelo `Ui`): sai da tela como se expirasse.
    pub(crate) fn on_surface_closed(&mut self, key: SurfaceKey) {
        let id = match (&self.screen, key) {
            (_, SurfaceKey::Card(id)) => id,
            (Screen::Wayland(w), SurfaceKey::Pill) => match w.pill_id() {
                Some(id) => id,
                None => return,
            },
            (Screen::Headless(_), SurfaceKey::Pill) => return,
        };
        eprintln!("notificacoes: o compositor fechou a superfície do aviso {id}");
        self.screen.hide(id);
        self.on_expire(id);
    }
}

/// Intervalo entre os avisos do `--demo` e a espera depois do último até sair.
const DEMO_STEP: Duration = Duration::from_millis(1500);
const DEMO_TAIL: Duration = Duration::from_secs(16);

/// Cartão normal com 2 ações, corpo de 3 linhas com emoji, atenção, crítico e compacto (USB).
fn demo_notifications() -> Vec<NewNotification> {
    let s = |v: &str| HintValue::Str(v.to_owned());
    let n = |app: &str, icon: &str, summary: &str, body: &str, actions: &[&str], hints: Vec<(&str, HintValue)>| {
        NewNotification {
            app_name: app.to_owned(),
            app_icon: icon.to_owned(),
            summary: summary.to_owned(),
            body: body.to_owned(),
            actions: parse_actions(actions.iter().map(|a| (*a).to_owned()).collect()),
            hints: Hints::parse(hints.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
            expire_timeout: -1,
        }
    };
    vec![
        n(
            "Firefox",
            "firefox",
            "Nova mensagem de Maria",
            "Oi! Você vem para o almoço amanhã?",
            &["default", "Abrir", "reply", "Responder", "archive", "Arquivar"],
            vec![],
        ),
        n(
            "Telegram",
            "org.telegram.desktop",
            "Grupo da família 🎉",
            "Mãe: Feliz aniversário!! 🎂🎈 Que o seu dia seja incrível, cheio de alegria e de gente querida \
             por perto. Hoje à noite tem bolo aqui em casa, venham todos! 😄 Tragam as crianças também.",
            &[],
            vec![],
        ),
        n(
            "Energia",
            "battery-caution-symbolic",
            "Bateria fraca",
            "Restam 15% (cerca de 40 min). Ligue o carregador.",
            &[],
            vec![("x-tom", s("atencao"))],
        ),
        n(
            "Discos",
            "drive-harddisk-symbolic",
            "Disco quase cheio",
            "Restam 2,1 GB livres em /home.",
            &[],
            vec![("urgency", HintValue::Int(2))],
        ),
        n(
            "Dispositivos USB",
            "drive-removable-media-symbolic",
            "Armazenamento USB conectado",
            "SanDisk Ultra 64 GB",
            &[],
            vec![("x-estilo", s("compacto")), ("category", s("device.added"))],
        ),
    ]
}

/// `--demo`: sem D-Bus; um aviso a cada `DEMO_STEP` direto no Store, e sai `DEMO_TAIL` depois do último.
fn start_demo(event_loop: &EventLoop<'static, State>) -> Result<(), String> {
    let signal = event_loop.get_signal();
    let mut items = demo_notifications().into_iter();
    event_loop
        .handle()
        .insert_source(Timer::from_duration(DEMO_STEP), move |_, _, state: &mut State| match items.next() {
            Some(n) => {
                let (_, effects) = state.store.notify(n, 0, false);
                state.apply_effects(effects);
                TimeoutAction::ToDuration(if items.len() == 0 { DEMO_TAIL } else { DEMO_STEP })
            }
            None => {
                signal.stop();
                TimeoutAction::Drop
            }
        })
        .map(|_| ())
        .map_err(|e| e.error.to_string())
}

/// Vigia o "não perturbe" do COSMIC e a pasta da config no laço. `false` = não vigia (loga o motivo).
fn start_watch(handle: &LoopHandle<'static, State>, dnd: &Path, config_dir: &Path) -> bool {
    let watches = [(dnd.to_path_buf(), WatchTag::Dnd), (config_dir.to_path_buf(), WatchTag::Config)];
    let watcher = match Watcher::new(&watches) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("notificacoes: inotify indisponível ({e}); o não perturbe será lido a cada aviso");
            return false;
        }
    };
    let source = Generic::new(watcher, Interest::READ, PollMode::Level);
    let inserted = handle.insert_source(source, |_, watcher, state: &mut State| {
        // SAFETY: só drena eventos; o fd não é fechado nem substituído.
        let tags = unsafe { watcher.get_mut() }.drain();
        state.on_watch(&tags);
        Ok(PostAction::Continue)
    });
    match inserted {
        Ok(_) => true,
        Err(e) => {
            eprintln!("notificacoes: o vigia não entrou no laço ({}); o não perturbe será lido a cada aviso", e.error);
            false
        }
    }
}

fn machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Cartões na tela e o nome no barramento.
    Normal,
    /// Só o barramento (testes de integração).
    SemTela,
    /// Só a tela, com avisos de exemplo; sai sozinho.
    Demo,
}

fn main() -> ExitCode {
    // Antes de abrir qualquer fd: o número herdado não pode colidir com os nossos.
    let panel_fd = panel::hold_panel_fd();

    let mut mode = Mode::Normal;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--sem-tela" => mode = Mode::SemTela,
            "--demo" => mode = Mode::Demo,
            other => eprintln!("notificacoes: argumento desconhecido ignorado: {other}"),
        }
    }

    let cfg = config::load();
    let mut event_loop: EventLoop<'static, State> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            eprintln!("notificacoes: laço de eventos não iniciou: {e}");
            return ExitCode::FAILURE;
        }
    };
    let handle = event_loop.handle();
    // A tela antes do barramento: sem Wayland o daemon sai sem ter tomado o nome.
    let screen = match mode {
        Mode::SemTela => Screen::Headless(Headless::new(handle.clone())),
        Mode::Normal | Mode::Demo => match Ui::connect(&handle) {
            Ok(ui) => Screen::Wayland(Box::new(WaylandScreen::new(ui, &cfg, handle.clone()))),
            Err(e) => {
                eprintln!("notificacoes: sem conexão Wayland: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let bus = if mode == Mode::Demo {
        None
    } else {
        match Bus::session(&handle) {
            Ok(bus) => Some(bus),
            Err(e) => {
                eprintln!("notificacoes: sem barramento de sessão ({e}); seguindo sem D-Bus");
                None
            }
        }
    };
    // logind (barramento de sistema): também em `--sem-tela`, que é como os testes o exercitam.
    if mode != Mode::Demo
        && let Err(e) = crate::dbus::logind::connect(&handle)
    {
        eprintln!("notificacoes: sem barramento de sistema para o logind ({e}); seguindo sem ele");
    }
    if mode == Mode::Demo
        && let Err(e) = start_demo(&event_loop)
    {
        eprintln!("notificacoes: a demo não começou: {e}");
        return ExitCode::FAILURE;
    }
    let cosmic_dir = config::config_home().join("cosmic");
    let dnd_path = dnd_dir(&cosmic_dir);
    let config_dir = config::config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    let dnd_watched = start_watch(&handle, &dnd_path, &config_dir);
    let usb = match mode {
        Mode::Normal => usb::Usb::start(&handle)
            .inspect_err(|e| eprintln!("notificacoes: udev indisponível ({e}); sem avisos de USB"))
            .ok(),
        Mode::SemTela | Mode::Demo => None,
    };
    let mut state = State {
        store: Store::new(cfg.max_visible),
        cfg,
        bus,
        screen,
        dnd: read_dnd(&dnd_path),
        dnd_dir: dnd_path,
        dnd_watched,
        handle,
        debug: std::env::var_os("NOTIFICACOES_DEBUG").is_some_and(|v| v == "1"),
        _panel_fd: panel_fd,
        pending: Vec::new(),
        token_timer: None,
        player: Player::new(),
        usb,
    };
    // Só a fonte Wayland devolve erro ao laço: a conexão com o compositor caiu.
    if let Err(e) = event_loop.run(None, &mut state, |_| {}) {
        eprintln!("notificacoes: o laço de eventos falhou: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::Durations;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notificacoes-main-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Estado sem tela e sem barramento; o laço fica vivo junto.
    fn state(dnd_dir: PathBuf, watched: bool) -> (EventLoop<'static, State>, State) {
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();
        let state = State {
            store: Store::new(4),
            cfg: Config { sound: false, ..Config::default() },
            bus: None,
            screen: Screen::Headless(Headless::new(handle.clone())),
            dnd: false,
            dnd_dir,
            dnd_watched: watched,
            handle,
            debug: false,
            _panel_fd: None,
            pending: Vec::new(),
            token_timer: None,
            player: Player::new(),
            usb: None,
        };
        (event_loop, state)
    }

    fn nn(summary: &str) -> NewNotification {
        NewNotification {
            app_name: "app".into(),
            app_icon: String::new(),
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            hints: Hints::default(),
            expire_timeout: -1,
        }
    }

    fn add(state: &mut State, summary: &str) -> u32 {
        let dnd = state.current_dnd();
        let (id, effects) = state.store.notify(nn(summary), 0, dnd);
        state.apply_effects(effects);
        id
    }

    #[test]
    fn dnd_turning_on_hides_visible_without_closing() {
        let dir = temp("dnd_on");
        let (_l, mut s) = state(dir.clone(), true);
        add(&mut s, "a");
        add(&mut s, "b");
        assert_eq!(s.store.stack(), vec![1, 2]);
        std::fs::write(dir.join("do_not_disturb"), "true\n").unwrap();
        s.on_watch(&[WatchTag::Dnd]);
        assert!(s.dnd);
        assert!(s.store.stack().is_empty());
        assert!(s.store.get(1).is_some() && s.store.get(2).is_some(), "vão para a lista oculta");
        // Com o não perturbe ligado, um aviso novo é aceito mas não aparece.
        add(&mut s, "c");
        assert!(s.store.stack().is_empty());
    }

    #[test]
    fn dnd_turning_off_keeps_screen_and_shows_next() {
        let dir = temp("dnd_off");
        let (_l, mut s) = state(dir.clone(), true);
        std::fs::write(dir.join("do_not_disturb"), "true").unwrap();
        s.on_watch(&[WatchTag::Dnd]);
        std::fs::write(dir.join("do_not_disturb"), "false").unwrap();
        s.on_watch(&[WatchTag::Dnd]);
        assert!(!s.dnd);
        add(&mut s, "d");
        assert_eq!(s.store.stack().len(), 1);
    }

    #[test]
    fn without_watcher_dnd_is_read_on_every_notify() {
        let dir = temp("dnd_sem_vigia");
        let (_l, mut s) = state(dir.clone(), false);
        assert!(!s.current_dnd());
        std::fs::write(dir.join("do_not_disturb"), "true").unwrap();
        assert!(s.current_dnd());
        add(&mut s, "x");
        assert!(s.store.stack().is_empty(), "aceito, mas oculto");
        std::fs::write(dir.join("do_not_disturb"), "false").unwrap();
        assert!(!s.current_dnd());
    }

    #[test]
    fn with_watcher_dnd_is_not_reread_on_notify() {
        let dir = temp("dnd_com_vigia");
        let (_l, mut s) = state(dir.clone(), true);
        std::fs::write(dir.join("do_not_disturb"), "true").unwrap();
        assert!(!s.current_dnd(), "só o evento do vigia atualiza");
    }

    #[test]
    fn config_reload_shrinks_stack_and_changes_durations() {
        let (_l, mut s) = state(temp("cfg"), true);
        for n in ["a", "b", "c"] {
            add(&mut s, n);
        }
        let cfg = Config {
            max_visible: 2,
            durations: Durations { normal: Duration::from_secs(1), ..Durations::default() },
            ..Config::default()
        };
        s.apply_config(cfg);
        assert_eq!(s.store.stack(), vec![2, 3], "o mais antigo sai da tela");
        assert_eq!(s.duration_of(3), Some(Duration::from_secs(1)));
    }
}
