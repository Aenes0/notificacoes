//! Som das notificações: resolução pelo tema de som (COSMIC, depois freedesktop), escolha por
//! categoria/dicas e o tocador (`pw-play`) como processo filho colhido por pidfd no calloop, sem thread.

use std::cell::Cell;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use rustix::process::{Pid, PidfdFlags, pidfd_open};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::config::Config;
use crate::model::Notification;

/// Temas de som, em ordem de preferência.
pub const THEMES: [&str; 2] = ["COSMIC", "freedesktop"];
const EXTENSIONS: [&str; 3] = ["oga", "ogg", "wav"];

/// `$XDG_DATA_HOME/sounds` e `<cada XDG_DATA_DIRS>/sounds`.
pub fn sound_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let home = env("HOME").map(PathBuf::from);
    let data_home = env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share")));
    let data_dirs = env("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs: Vec<PathBuf> = data_home.into_iter().map(|d| d.join("sounds")).collect();
    dirs.extend(std::env::split_paths(&data_dirs).filter(|p| !p.as_os_str().is_empty()).map(|p| p.join("sounds")));
    dirs
}

/// Subdiretórios do tema (`Directories=` do `index.theme`); sem o arquivo ou a chave, `stereo`.
fn theme_directories(theme_dir: &Path) -> Vec<String> {
    let listed = std::fs::read_to_string(theme_dir.join("index.theme")).ok().and_then(|text| {
        text.lines().find_map(|l| l.trim().strip_prefix("Directories=").map(|v| v.trim().to_owned()))
    });
    let dirs: Vec<String> = listed
        .map(|v| v.split([' ', ',']).filter(|s| !s.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    if dirs.is_empty() { vec!["stereo".to_owned()] } else { dirs }
}

fn find_exact(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for theme in THEMES {
        for base in dirs {
            let theme_dir = base.join(theme);
            for sub in theme_directories(&theme_dir) {
                // Subdiretório vindo de arquivo: nada de sair do tema.
                if sub.split('/').any(|c| c == ".." || c.is_empty()) {
                    continue;
                }
                for ext in EXTENSIONS {
                    let path = theme_dir.join(&sub).join(format!("{name}.{ext}"));
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Acha o arquivo do som `name`; sem achar, tira o último `-sufixo` e tenta de novo.
pub fn resolve(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let mut current = name;
    loop {
        if let Some(path) = find_exact(dirs, current) {
            return Some(path);
        }
        current = current.rsplit_once('-')?.0;
        if current.is_empty() {
            return None;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SoundChoice {
    Silent,
    File(PathBuf),
    Name(String),
}

/// O que tocar para `n`. Silêncio: `som = nao`, não perturbe, `suppress-sound`, pílula.
pub fn choose(n: &Notification, cfg: &Config, dnd: bool) -> SoundChoice {
    if !cfg.sound || dnd || n.hints.suppress_sound || n.is_pill() {
        return SoundChoice::Silent;
    }
    if let Some(file) = &n.hints.sound_file
        && file.is_file()
    {
        return SoundChoice::File(file.clone());
    }
    if let Some(name) = &n.hints.sound_name {
        return SoundChoice::Name(name.clone());
    }
    match cfg.sound_for_category(n.hints.category.as_deref()) {
        Some(name) => SoundChoice::Name(name.to_owned()),
        None => SoundChoice::Silent,
    }
}

/// Toca um som por vez: um filho `pw-play` colhido quando o pidfd fica legível.
pub struct Player {
    playing: Rc<Cell<bool>>,
    last_pid: Option<u32>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Player {
        Player { playing: Rc::new(Cell::new(false)), last_pid: None }
    }

    /// Pid do último filho lançado (para os testes).
    #[cfg(test)]
    pub fn last_pid(&self) -> Option<u32> {
        self.last_pid
    }

    /// Lança `player file`. `false` se ainda toca o anterior ou se não deu para lançar.
    pub fn play<S: 'static>(&mut self, handle: &LoopHandle<'static, S>, player: &str, file: &Path) -> bool {
        if self.playing.get() {
            return false;
        }
        let mut words = player.split_whitespace();
        let Some(program) = words.next() else {
            return false;
        };
        let mut child = match Command::new(program)
            .args(words)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!("notificacoes: tocador '{program}' não iniciou: {e}");
                return false;
            }
        };
        let pid = child.id();
        let fd: Option<OwnedFd> = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .and_then(|p| pidfd_open(p, PidfdFlags::empty()).ok());
        let Some(fd) = fd else {
            eprintln!("notificacoes: pidfd do tocador indisponível; som descartado");
            let _ = child.kill();
            let _ = child.wait();
            return false;
        };
        let playing = Rc::clone(&self.playing);
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        let inserted = handle.insert_source(source, move |_, _, _| {
            let _ = child.wait();
            playing.set(false);
            Ok(PostAction::Remove)
        });
        match inserted {
            Ok(_) => {
                self.playing.set(true);
                self.last_pid = Some(pid);
                true
            }
            Err(e) => {
                eprintln!("notificacoes: pidfd não entrou no laço: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::EventLoop;
    use std::time::{Duration, Instant};

    use crate::model::{Hints, Notification, Estilo};

    fn fixtures() -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sounds")]
    }

    fn notif(hints: Hints) -> Notification {
        Notification {
            id: 1, app_name: "app".into(), app_icon: String::new(), summary: "s".into(), body: String::new(),
            actions: Vec::new(), hints, expire_timeout: -1,
        }
    }

    fn cat(c: &str) -> Hints {
        Hints { category: Some(c.to_owned()), ..Hints::default() }
    }

    #[test]
    fn resolves_in_cosmic_then_freedesktop() {
        let d = fixtures();
        assert!(resolve(&d, "device-added").unwrap().ends_with("COSMIC/stereo/notification/device-added.oga"));
        assert!(resolve(&d, "bell").unwrap().ends_with("freedesktop/stereo/bell.oga"));
    }

    #[test]
    fn name_fallback_drops_suffix() {
        let p = resolve(&fixtures(), "device-added-usb").unwrap();
        assert!(p.ends_with("device-added.oga"));
    }

    #[test]
    fn unknown_sound_is_none() {
        assert_eq!(resolve(&fixtures(), "nao-existe-mesmo"), None);
        assert_eq!(resolve(&fixtures(), ""), None);
        assert_eq!(resolve(&fixtures(), "../x"), None);
    }

    #[test]
    fn silence_rules() {
        let cfg = Config::default();
        assert_eq!(choose(&notif(Hints::default()), &cfg, false), SoundChoice::Name("message".into()));
        let off = Config { sound: false, ..Config::default() };
        assert_eq!(choose(&notif(Hints::default()), &off, false), SoundChoice::Silent);
        assert_eq!(choose(&notif(Hints::default()), &cfg, true), SoundChoice::Silent);
        let sup = Hints { suppress_sound: true, ..Hints::default() };
        assert_eq!(choose(&notif(sup), &cfg, false), SoundChoice::Silent);
        let pill = Hints { estilo: Some(Estilo::Osd), ..Hints::default() };
        assert_eq!(choose(&notif(pill), &cfg, false), SoundChoice::Silent);
        // categoria desligada na config → sem som
        let mut c2 = Config::default();
        c2.sounds.push(("x.y".into(), None));
        assert_eq!(choose(&notif(cat("x.y")), &c2, false), SoundChoice::Silent);
    }

    #[test]
    fn sound_file_must_exist_and_be_regular() {
        let cfg = Config::default();
        let file = fixtures()[0].join("COSMIC/index.theme");
        let h = Hints { sound_file: Some(file.clone()), ..Hints::default() };
        assert_eq!(choose(&notif(h), &cfg, false), SoundChoice::File(file));
        for bad in [fixtures()[0].join("nao-existe.oga"), fixtures()[0].clone()] {
            let h = Hints { sound_file: Some(bad), sound_name: Some("bell".into()), ..Hints::default() };
            assert_eq!(choose(&notif(h), &cfg, false), SoundChoice::Name("bell".into()));
        }
        let h = Hints { sound_file: Some(fixtures()[0].clone()), ..Hints::default() };
        assert_eq!(choose(&notif(h), &cfg, false), SoundChoice::Name("message".into()));
    }

    #[test]
    fn category_map_and_default() {
        let cfg = Config::default();
        assert_eq!(choose(&notif(cat("device.added")), &cfg, false), SoundChoice::Name("device-added".into()));
        assert_eq!(choose(&notif(Hints::default()), &cfg, false), SoundChoice::Name("message".into()));
        let named = Hints { sound_name: Some("bell".into()), ..cat("device.added") };
        assert_eq!(choose(&notif(named), &cfg, false), SoundChoice::Name("bell".into()));
    }

    fn threads() -> usize {
        std::fs::read_to_string("/proc/self/status").unwrap().lines()
            .find_map(|l| l.strip_prefix("Threads:")).unwrap().trim().parse().unwrap()
    }

    fn run_for(el: &mut EventLoop<'static, ()>, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            el.dispatch(Some(Duration::from_millis(20)), &mut ()).unwrap();
        }
    }

    #[test]
    fn player_spawned_and_reaped_without_thread() {
        let mut el: EventLoop<'static, ()> = EventLoop::try_new().unwrap();
        let h = el.handle();
        let mut p = Player::new();
        let before = threads();
        assert!(p.play(&h, "true", Path::new("/dev/null")));
        let pid = p.last_pid().unwrap();
        assert!(threads() <= before, "play criou thread");
        run_for(&mut el, 300);
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "zumbi ou processo vivo");
        assert!(threads() <= before, "play criou thread");
    }

    #[test]
    fn sound_skipped_while_previous_playing() {
        let mut el: EventLoop<'static, ()> = EventLoop::try_new().unwrap();
        let h = el.handle();
        let mut p = Player::new();
        // `sleep 0.3 0` dorme 0,3 s: o "arquivo" é o último argumento.
        assert!(p.play(&h, "sleep 0.3", Path::new("0")));
        assert!(!p.play(&h, "sleep 0.3", Path::new("0")));
        run_for(&mut el, 500);
        assert!(p.play(&h, "true", Path::new("0")));
    }
}
