//! Spike: layer surface 300×120 magenta opaco, ancorada embaixo à direita (margem 24).
//!
//! `camada [--top] [--no-bloqueio] [--segundos N]`
//! - `--top`: camada Top (padrão Overlay);
//! - `--no-bloqueio`: marca a superfície com `cosmic_session_lock_layer_manager_v1.set_show_on_lock`;
//! - `--segundos N`: some e sai depois de N s (padrão 20).

use std::time::Duration;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
    SurfaceKind,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm, registry_handlers,
};

/// Protocolo vendorizado do `main` do cosmic-protocols (não existe no 0.2.0 do crates.io).
pub mod lock_layer {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::layer_shell::v1::client::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::layer_shell::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic-session-lock-layer-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/cosmic-session-lock-layer-v1.xml");
}

use lock_layer::cosmic_session_lock_layer_manager_v1::CosmicSessionLockLayerManagerV1;

const WIDTH: u32 = 300;
const HEIGHT: u32 = 120;
const MARGIN: i32 = 24;
/// `#FF00FF` opaco em Argb8888 little-endian (B, G, R, A).
const MAGENTA_BGRA: [u8; 4] = [0xFF, 0x00, 0xFF, 0xFF];

struct Args {
    top: bool,
    no_bloqueio: bool,
    segundos: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { top: false, no_bloqueio: false, segundos: 20 };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--top" => args.top = true,
            "--no-bloqueio" => args.no_bloqueio = true,
            "--segundos" => {
                let v = it.next().ok_or("--segundos precisa de um número")?;
                args.segundos = v.parse().map_err(|_| format!("--segundos inválido: {v}"))?;
            }
            outro => return Err(format!("argumento desconhecido: {outro}")),
        }
    }
    Ok(args)
}

struct State {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    layer: LayerSurface,
    pool: SlotPool,
    buffer: Option<Buffer>,
    exit: bool,
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("camada: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&args) {
        eprintln!("camada: erro: {e}");
        std::process::exit(1);
    }
}

fn bind_lock_manager(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<CosmicSessionLockLayerManagerV1> {
    match globals.bind::<CosmicSessionLockLayerManagerV1, _, _>(qh, 1..=1, ()) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("camada: global cosmic_session_lock_layer_manager_v1 indisponível: {e}");
            None
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let lock_manager = if args.no_bloqueio { bind_lock_manager(&globals, &qh) } else { None };

    let layer_kind = if args.top { Layer::Top } else { Layer::Overlay };
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, layer_kind, Some("notificacoes-spike"), None);
    layer.set_anchor(Anchor::BOTTOM | Anchor::RIGHT);
    layer.set_size(WIDTH, HEIGHT);
    layer.set_margin(0, MARGIN, MARGIN, 0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(0);
    if let Some(m) = &lock_manager {
        // Duplamente armazenado: vale no próximo commit.
        if let SurfaceKind::Wlr(wlr) = layer.kind() {
            m.set_show_on_lock(wlr);
            eprintln!("camada: set_show_on_lock enviado");
        } else {
            eprintln!("camada: layer surface não é wlr; set_show_on_lock não enviado");
        }
    }
    layer.commit();

    let pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize, &shm)?;
    let mut state = State {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        layer,
        pool,
        buffer: None,
        exit: false,
    };

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(conn, queue).insert(event_loop.handle()).map_err(|e| e.error)?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(args.segundos)), |_, _, st: &mut State| {
            st.exit = true;
            TimeoutAction::Drop
        })
        .map_err(|e| e.error)?;

    eprintln!(
        "camada: camada={} show_on_lock={} segundos={}",
        if args.top { "Top" } else { "Overlay" },
        lock_manager.is_some(),
        args.segundos
    );
    while !state.exit {
        event_loop.dispatch(None, &mut state)?;
    }
    eprintln!("camada: fim");
    Ok(())
}

impl State {
    fn draw(&mut self) {
        let (w, h) = (WIDTH as i32, HEIGHT as i32);
        let (buffer, canvas) = match self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("camada: buffer não criado: {e}");
                return;
            }
        };
        for px in canvas.chunks_exact_mut(4) {
            px.copy_from_slice(&MAGENTA_BGRA);
        }
        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if let Err(e) = buffer.attach_to(surface) {
            eprintln!("camada: attach falhou: {e}");
            return;
        }
        self.layer.commit();
        self.buffer = Some(buffer);
    }
}

impl Dispatch<CosmicSessionLockLayerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &CosmicSessionLockLayerManagerV1,
        _: <CosmicSessionLockLayerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        eprintln!("camada: compositor fechou a superfície");
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        eprintln!("camada: configure {:?}", cfg.new_size);
        self.draw();
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);
