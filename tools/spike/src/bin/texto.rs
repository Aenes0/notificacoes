//! Spike: custo do `cosmic-text` (RSS e tempo do `FontSystem::new()`) e emoji colorido.
//!
//! Imprime `VmRSS` antes e depois de `FontSystem::new()`, o tempo da criação, e o `VmRSS` depois
//! de compor e rasterizar a frase de teste a 12 px. Grava `docs/verificacao/spike-texto.png`
//! (tamanho real) e `docs/verificacao/spike-texto-4x.png` (ampliado 4×, vizinho mais próximo,
//! só para olhar). Rode na raiz do projeto, ou passe o diretório de saída como argumento.

use std::time::Instant;

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{Paint, Pixmap, Rect, Transform};

const FRASE: &str = "Olá 👋 مرحبا 你好 שלום नमस्ते";
const TAMANHO_PX: f32 = 12.0;
const ALTURA_LINHA_PX: f32 = 16.0;
const LARGURA: u32 = 320;
const ALTURA: u32 = 24;
const AMPLIACAO: u32 = 4;

fn vm_rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else { return 0 };
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn main() {
    let saida = std::env::args().nth(1).unwrap_or_else(|| "docs/verificacao".to_string());

    let antes = vm_rss_kb();
    let t0 = Instant::now();
    let mut fonts = FontSystem::new();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let depois_fs = vm_rss_kb();
    println!("texto: rss_kb_antes = {antes}");
    println!("texto: rss_kb_depois_fontsystem = {depois_fs}");
    println!("texto: ms_fontsystem = {ms:.1}");
    println!("texto: faces_no_indice = {}", fonts.db().len());

    let t1 = Instant::now();
    let mut buffer = Buffer::new(&mut fonts, Metrics::new(TAMANHO_PX, ALTURA_LINHA_PX));
    buffer.set_size(Some(LARGURA as f32), Some(ALTURA as f32));
    buffer.set_text(FRASE, &Attrs::new(), Shaping::Advanced, None);
    buffer.shape_until_scroll(&mut fonts, false);

    let Some(mut pixmap) = Pixmap::new(LARGURA, ALTURA) else {
        eprintln!("texto: pixmap não criado");
        std::process::exit(1);
    };
    pixmap.fill(tiny_skia::Color::from_rgba8(0x1e, 0x1f, 0x24, 0xff));
    let mut cache = SwashCache::new();
    let mut cromaticos: u64 = 0;
    let branco = Color::rgb(0xf2, 0xf2, 0xf5);
    buffer.draw(&mut fonts, &mut cache, branco, |x, y, w, h, c: Color| {
        if c.a() == 0 {
            return;
        }
        let (r, g, b) = (i32::from(c.r()), i32::from(c.g()), i32::from(c.b()));
        let spread = r.max(g).max(b) - r.min(g).min(b);
        if spread > 40 && c.a() > 64 {
            cromaticos += u64::from(w) * u64::from(h);
        }
        let Some(rect) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) else { return };
        let mut paint = Paint::default();
        paint.set_color_rgba8(c.r(), c.g(), c.b(), c.a());
        paint.anti_alias = false;
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    });
    let ms_render = t1.elapsed().as_secs_f64() * 1000.0;
    let depois_render = vm_rss_kb();
    println!("texto: ms_compor_rasterizar = {ms_render:.1}");
    println!("texto: rss_kb_depois_render = {depois_render}");
    println!("texto: pixels_cromaticos = {cromaticos}");

    for run in buffer.layout_runs() {
        let mut familias: Vec<String> = Vec::new();
        for g in run.glyphs {
            let nome = fonts
                .db()
                .face(g.font_id)
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
                .unwrap_or_else(|| "?".to_string());
            if !familias.contains(&nome) {
                familias.push(nome);
            }
        }
        println!("texto: fontes_usadas = {familias:?}");
    }

    let real = format!("{saida}/spike-texto.png");
    if let Err(e) = pixmap.save_png(&real) {
        eprintln!("texto: falha ao gravar {real}: {e}");
    }
    if let Some(mut grande) = Pixmap::new(LARGURA * AMPLIACAO, ALTURA * AMPLIACAO) {
        let src = pixmap.data();
        let dst = grande.data_mut();
        let (lw, gw) = (LARGURA as usize, (LARGURA * AMPLIACAO) as usize);
        for gy in 0..(ALTURA * AMPLIACAO) as usize {
            for gx in 0..gw {
                let s = ((gy / AMPLIACAO as usize) * lw + gx / AMPLIACAO as usize) * 4;
                let d = (gy * gw + gx) * 4;
                dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
        let ampliado = format!("{saida}/spike-texto-4x.png");
        if let Err(e) = grande.save_png(&ampliado) {
            eprintln!("texto: falha ao gravar {ampliado}: {e}");
        }
    }
    println!("texto: gravado {real}");
}
