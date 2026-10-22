//! Conexão Wayland (sctk 0.20, layer-shell): globais, uma superfície por chave e os handlers.
//!
//! Ponteiro, cursor e tokens de ativação em `pointer` (sem teclado). Ociosa, a conexão não tem
//! nenhuma superfície e o laço não acorda por ela: o compositor só manda eventos de ponteiro com o
//! cursor numa superfície nossa.
//!
//! Saída: o primeiro cartão abre sem saída (o cosmic-comp escolhe a ativa); o primeiro
//! `surface_enter` de um cartão fixa a "saída da pilha", onde os cartões seguintes abrem enquanto
//! houver cartão. Pilha vazia esquece a saída.
//!
//! Escala: inteira, do `preferred_buffer_scale` de cada superfície (o sctk deriva das saídas em
//! compositores antigos e chama `scale_factor_changed`). Antes de a superfície saber a dela, vale a
//! última escala vista, senão a maior entre as saídas, senão 1.

pub mod pointer;
pub mod surface;
pub mod toplevel;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1, xdg_activation_v1,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm, registry_handlers,
};
use std::time::Instant;

use tiny_skia::Pixmap;

use self::pointer::Pointer;
use self::surface::{Surface, pad_to_scale};
use self::toplevel::ToplevelWatch;
use crate::State;
use crate::render::card::Rect;

const NAMESPACE: &str = "notificacoes";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceKey {
    Card(u32),
    /// A pílula OSD: ancorada embaixo no centro, sem entrada, uma só por vez.
    Pill,
}

pub struct Ui {
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    shm: Shm,
    layer_shell: LayerShell,
    seat: SeatState,
    /// `wp_cursor_shape_manager_v1`, se o compositor tiver.
    cursor_shape: Option<CursorShapeManager>,
    /// `xdg_activation_v1`, se o compositor tiver.
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    pointers: Vec<Pointer>,
    /// O ponteiro que entrou por último numa superfície nossa.
    active_pointer: Option<wl_pointer::WlPointer>,
    /// Pedidos de token à espera de resposta, por notificação.
    tokens: Vec<(u32, xdg_activation_token_v1::XdgActivationTokenV1)>,
    qh: QueueHandle<State>,
    surfaces: Vec<(SurfaceKey, Surface)>,
    /// Saída onde o primeiro cartão da pilha entrou.
    stack_output: Option<wl_output::WlOutput>,
    /// Última escala preferida vista em qualquer superfície; 0 = nenhuma ainda.
    last_scale: u32,
    /// Tela cheia em foco (modo privado); inscrito só com cartão de app na tela.
    toplevels: ToplevelWatch,
    /// `NOTIFICACOES_DEBUG=1`: imprime a latência da pílula.
    debug: bool,
    /// Chegada do aviso da pílula que ainda não teve `commit`.
    pill_t0: Option<Instant>,
}

impl Ui {
    /// Conecta ao compositor, liga os globais e põe a fila Wayland no calloop.
    pub fn connect(handle: &LoopHandle<'static, State>) -> Result<Ui, Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh)?;
        let layer_shell = LayerShell::bind(&globals, &qh)?;
        let shm = Shm::bind(&globals, &qh)?;
        let cursor_shape = CursorShapeManager::bind(&globals, &qh)
            .inspect_err(|e| eprintln!("notificacoes: sem wp_cursor_shape_v1 ({e}); o cursor fica como vier"))
            .ok();
        let activation = globals
            .bind(&qh, 1..=1, ())
            .inspect_err(|e| eprintln!("notificacoes: sem xdg_activation_v1 ({e}); ações sem token"))
            .ok();
        let toplevels = ToplevelWatch::new(&globals, qh.clone(), conn.display());
        let ui = Ui {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            compositor,
            shm,
            layer_shell,
            seat: SeatState::new(&globals, &qh),
            cursor_shape,
            activation,
            pointers: Vec::new(),
            active_pointer: None,
            tokens: Vec::new(),
            qh,
            surfaces: Vec::new(),
            stack_output: None,
            last_scale: 0,
            toplevels,
            debug: std::env::var_os("NOTIFICACOES_DEBUG").is_some_and(|v| v == "1"),
            pill_t0: None,
        };
        WaylandSource::new(conn, queue).insert(handle.clone()).map_err(|e| e.error)?;
        Ok(ui)
    }

    /// Layer surface Overlay, sem teclado, sem zona exclusiva; entrada só em `input` (lógico,
    /// coordenadas da superfície; `None` = região vazia). Uma superfície antiga com a mesma chave
    /// fecha antes, na hora. Commit sem buffer: o desenho sai no primeiro `configure`.
    pub fn open(
        &mut self,
        key: SurfaceKey,
        anchor: Anchor,
        size: (u32, u32),
        margin: (i32, i32, i32, i32),
        input: Option<Rect>,
        stack_output: bool,
    ) {
        self.close(key);
        let output = if stack_output { self.stack_output.as_ref() } else { None };
        let wl = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, wl, Layer::Overlay, Some(NAMESPACE), output);
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_margin(margin.0, margin.1, margin.2, margin.3);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        self.apply_input(&layer, input);
        layer.commit();
        self.surfaces.push((key, Surface::new(layer, size)));
    }

    /// Anexa o quadro (ou guarda até o `configure`). `damage` em px lógicos; `None` = tudo.
    pub fn present(&mut self, key: SurfaceKey, pm: &Pixmap, scale: u32, damage: Option<Rect>) {
        let scale = scale.max(1);
        let shm = &self.shm;
        let Some(surface) = self.surfaces.iter_mut().find(|(k, _)| *k == key).map(|(_, s)| s) else { return };
        let padded = pad_to_scale(pm, scale);
        surface.present(shm, padded.as_ref().unwrap_or(pm), scale, damage);
        // Já configurada: o `commit` saiu agora. Senão sai no `configure`.
        if key == SurfaceKey::Pill && surface.is_configured() {
            self.report_pill();
        }
    }

    /// Marca a chegada do aviso da pílula (só com `NOTIFICACOES_DEBUG=1`).
    pub fn mark_pill(&mut self, arrived: Instant) {
        if self.debug {
            self.pill_t0 = Some(arrived);
        }
    }

    fn report_pill(&mut self) {
        if let Some(t0) = self.pill_t0.take() {
            eprintln!("notificacoes: pilula: {:.1} ms Notify→commit", t0.elapsed().as_secs_f64() * 1000.0);
        }
    }

    /// Margens (topo, direita, base, esquerda), aplicadas no próximo commit, que sai já.
    pub fn set_margin(&mut self, key: SurfaceKey, margin: (i32, i32, i32, i32)) {
        if let Some(s) = self.get(key) {
            s.layer.set_margin(margin.0, margin.1, margin.2, margin.3);
            s.layer.commit();
        }
    }

    /// Troca a região de entrada (o cartão mudou de altura).
    pub fn set_input(&mut self, key: SurfaceKey, input: Option<Rect>) {
        let Some(at) = self.surfaces.iter().position(|(k, _)| *k == key) else { return };
        let Some((_, s)) = self.surfaces.get(at) else { return };
        self.apply_input(&s.layer, input);
    }

    /// Destrói a superfície, o buffer e o pool. Sem cartão, a pilha esquece a saída.
    pub fn close(&mut self, key: SurfaceKey) {
        self.surfaces.retain(|(k, _)| *k != key);
        if !self.surfaces.iter().any(|(k, _)| matches!(k, SurfaceKey::Card(_))) {
            self.stack_output = None;
        }
    }

    /// Escala do próximo desenho de `key`: a preferida dela; senão a última vista; senão a
    /// maior das saídas; senão 1.
    pub fn scale(&self, key: SurfaceKey) -> u32 {
        if let Some((_, s)) = self.surfaces.iter().find(|(k, _)| *k == key)
            && s.preferred_scale > 0
        {
            return s.preferred_scale;
        }
        if self.last_scale > 0 {
            return self.last_scale;
        }
        self.outputs
            .outputs()
            .filter_map(|o| self.outputs.info(&o))
            .map(|i| u32::try_from(i.scale_factor).unwrap_or(1))
            .max()
            .unwrap_or(1)
            .max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    fn get(&mut self, key: SurfaceKey) -> Option<&mut Surface> {
        self.surfaces.iter_mut().find(|(k, _)| *k == key).map(|(_, s)| s)
    }

    fn key_of(&self, wl: &wl_surface::WlSurface) -> Option<SurfaceKey> {
        self.surfaces.iter().find(|(_, s)| s.layer.wl_surface() == wl).map(|(k, _)| *k)
    }

    fn key_of_layer(&self, layer: &LayerSurface) -> Option<SurfaceKey> {
        self.surfaces.iter().find(|(_, s)| &s.layer == layer).map(|(k, _)| *k)
    }

    fn apply_input(&self, layer: &LayerSurface, input: Option<Rect>) {
        match Region::new(&self.compositor) {
            // A região é copiada no `set_input_region`; destruí-la logo depois é permitido.
            Ok(region) => {
                if let Some(r) = input {
                    let (x, y) = (r.x.floor() as i32, r.y.floor() as i32);
                    let (w, h) = ((r.x + r.w).ceil() as i32 - x, (r.y + r.h).ceil() as i32 - y);
                    region.add(x, y, w, h);
                }
                layer.set_input_region(Some(region.wl_region()));
            }
            Err(e) => eprintln!("notificacoes: região de entrada não criada: {e}"),
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, factor: i32) {
        let factor = u32::try_from(factor).unwrap_or(1).max(1);
        let Some(ui) = self.ui_mut() else { return };
        ui.last_scale = factor;
        let Some(key) = ui.key_of(wl) else { return };
        let Some(s) = ui.get(key) else { return };
        if s.preferred_scale == factor {
            return;
        }
        s.preferred_scale = factor;
        self.on_surface_scale(key);
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        let Some(ui) = self.ui_mut() else { return };
        if ui.stack_output.is_none() && matches!(ui.key_of(wl), Some(SurfaceKey::Card(_))) {
            ui.stack_output = Some(output.clone());
        }
    }

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.ui_or_unreachable().outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if let Some(ui) = self.ui_mut()
            && ui.stack_output.as_ref() == Some(&output)
        {
            ui.stack_output = None;
        }
    }
}

impl LayerShellHandler for State {
    /// O compositor fechou a superfície (ex.: a saída sumiu): ela sai da tela como se expirasse.
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        let Some(ui) = self.ui_mut() else { return };
        let Some(key) = ui.key_of_layer(layer) else { return };
        ui.close(key);
        self.on_surface_closed(key);
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        let Some(ui) = self.ui_mut() else { return };
        let Some(key) = ui.key_of_layer(layer) else { return };
        let shm = &ui.shm;
        // O tamanho é nosso (ancorado em dois lados): o configure só libera o primeiro desenho.
        if let Some((_, s)) = ui.surfaces.iter_mut().find(|(k, _)| *k == key) {
            s.on_configure(shm);
        }
        if key == SurfaceKey::Pill {
            ui.report_pill();
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.ui_or_unreachable().shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.ui_or_unreachable().registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);
