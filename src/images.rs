//! Decodificação de imagens de notificação com limites. Tudo aqui é entrada não confiável:
//! tamanho de arquivo, dimensões e buffers são checados antes de alocar; falha vira `ImageError`.

use std::io::Cursor;
use std::path::Path;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::model::{Estilo, Notification, RawImage};
use crate::render::card::Format;

/// Caixa máxima da miniatura (em pixels lógicos, antes da escala).
pub const THUMB_MAX: (u32, u32) = (320, 180);
/// Maior arquivo lido, maior área decodificada e maior lado aceito.
pub const MAX_FILE_BYTES: u64 = 32 << 20;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_SIDE: u32 = 16_384;
/// Imagem com os dois lados abaixo disso vira o ícone do cartão comum.
pub const SMALL_IMAGE: u32 = 128;
/// Acima disso, devolve a memória ao sistema depois de decodificar.
const TRIM_THRESHOLD: u64 = 1 << 20;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[derive(Debug)]
pub enum ImageError {
    TooBig,
    Unsupported,
    Invalid(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::TooBig => write!(f, "imagem grande demais"),
            ImageError::Unsupported => write!(f, "formato de imagem não suportado"),
            ImageError::Invalid(why) => write!(f, "imagem inválida: {why}"),
            ImageError::Io(e) => write!(f, "erro de leitura: {e}"),
        }
    }
}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> Self {
        ImageError::Io(e)
    }
}

/// Dimensões declaradas dentro dos limites? Chamado antes de qualquer alocação.
fn check_dims(width: u64, height: u64) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::Invalid("dimensão zero"));
    }
    if width > u64::from(MAX_SIDE) || height > u64::from(MAX_SIDE) || width * height > MAX_PIXELS {
        return Err(ImageError::TooBig);
    }
    Ok(())
}

fn premul(c: u8, a: u8) -> u8 {
    ((u16::from(c) * u16::from(a) + 127) / 255) as u8
}

/// Imagem crua do hint `image-data` (8 bits, 3 ou 4 canais) → pixmap pré-multiplicado.
pub fn decode_raw(img: &RawImage) -> Result<Pixmap, ImageError> {
    if img.bits_per_sample != 8 || !matches!(img.channels, 3 | 4) {
        return Err(ImageError::Unsupported);
    }
    let (Ok(w), Ok(h), Ok(stride)) = (u32::try_from(img.width), u32::try_from(img.height), usize::try_from(img.rowstride)) else {
        return Err(ImageError::Invalid("dimensões negativas"));
    };
    check_dims(u64::from(w), u64::from(h))?;
    let ch = img.channels as usize;
    let row = w as usize * ch;
    if stride < row {
        return Err(ImageError::Invalid("rowstride menor que a linha"));
    }
    let needed = stride
        .checked_mul(h as usize - 1)
        .and_then(|n| n.checked_add(row))
        .ok_or(ImageError::Invalid("tamanho estoura"))?;
    if img.data.len() < needed {
        return Err(ImageError::Invalid("buffer menor que o declarado"));
    }
    let mut pixmap = Pixmap::new(w, h).ok_or(ImageError::Invalid("pixmap"))?;
    let dst_rows = pixmap.data_mut().chunks_exact_mut(w as usize * 4);
    for (y, dst) in dst_rows.enumerate() {
        let src = img.data.get(y * stride..y * stride + row).ok_or(ImageError::Invalid("linha fora do buffer"))?;
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(ch)) {
            let a = if ch == 4 { s[3] } else { 255 };
            d.copy_from_slice(&[premul(s[0], a), premul(s[1], a), premul(s[2], a), a]);
        }
    }
    Ok(pixmap)
}

/// PNG/JPEG de arquivo (tipo pela assinatura). Tamanho do arquivo e dimensões são checados antes de decodificar.
pub fn decode_file(path: &Path) -> Result<Pixmap, ImageError> {
    let bytes = crate::fsread::read_regular(path, MAX_FILE_BYTES).map_err(|e| match e.kind() {
        std::io::ErrorKind::InvalidInput => ImageError::Unsupported,
        std::io::ErrorKind::FileTooLarge => ImageError::TooBig,
        _ => ImageError::Io(e),
    })?;
    let result = if bytes.starts_with(&PNG_SIGNATURE) {
        decode_png(&bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(&bytes)
    } else {
        Err(ImageError::Unsupported)
    };
    let big = bytes.len() as u64 > TRIM_THRESHOLD;
    drop(bytes);
    if big {
        // SAFETY: malloc_trim só devolve páginas livres do heap; não toca em memória em uso.
        unsafe { libc::malloc_trim(0) };
    }
    result
}

fn decode_png(bytes: &[u8]) -> Result<Pixmap, ImageError> {
    // O IHDR vem logo após a assinatura: checa as dimensões antes mesmo de o decodificador existir
    // (um cabeçalho forjado sem IDAT nem chega ao `read_info`).
    let ihdr = bytes.get(12..24).filter(|h| h.starts_with(b"IHDR")).ok_or(ImageError::Invalid("sem IHDR"))?;
    let dim = |at: usize| ihdr.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    if let (Some(w), Some(h)) = (dim(4), dim(8)) {
        check_dims(u64::from(w), u64::from(h))?;
    }
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| ImageError::Invalid("cabeçalho PNG"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    // Antes de qualquer alocação: o cabeçalho pode ser forjado.
    check_dims(u64::from(width), u64::from(height))?;
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return Err(ImageError::Unsupported);
    }
    let stride = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return Err(ImageError::Unsupported),
    };
    let mut pixmap = Pixmap::new(width, height).ok_or(ImageError::Invalid("pixmap"))?;
    let row_bytes = width as usize * 4;
    // Linha a linha direto no pixmap: não existe um segundo buffer da imagem inteira.
    // No Adam7 cada linha é de um passe (mais curta): vai para um scratch de uma linha e é espalhada.
    let mut scratch = vec![0u8; row_bytes];
    let mut y = 0usize;
    const TRUNCATED: ImageError = ImageError::Invalid("PNG truncado");
    while let Some(row) = reader.next_interlaced_row().map_err(|_| ImageError::Invalid("dados PNG"))? {
        match row.interlace() {
            png::InterlaceInfo::Adam7(info) => {
                let n = row.data().len() / stride;
                let used = scratch.get_mut(..n * 4).ok_or(TRUNCATED)?;
                convert_row(color, row.data(), stride, used).ok_or(TRUNCATED)?;
                png::expand_interlaced_row(pixmap.data_mut(), row_bytes, used, info, 32);
            }
            _ => {
                let dst = pixmap.data_mut().chunks_exact_mut(row_bytes).nth(y).ok_or(TRUNCATED)?;
                convert_row(color, row.data(), stride, dst).ok_or(TRUNCATED)?;
                y += 1;
            }
        }
    }
    Ok(pixmap)
}

/// Uma linha decodificada (8 bits) → RGBA pré-multiplicado; `None` se faltar dado.
fn convert_row(color: png::ColorType, src: &[u8], stride: usize, dst: &mut [u8]) -> Option<()> {
    if src.len() < dst.len() / 4 * stride {
        return None;
    }
    for (d, p) in dst.chunks_exact_mut(4).zip(src.chunks_exact(stride)) {
        let px = match color {
            png::ColorType::Rgba => [premul(p[0], p[3]), premul(p[1], p[3]), premul(p[2], p[3]), p[3]],
            png::ColorType::Rgb => [p[0], p[1], p[2], 255],
            png::ColorType::GrayscaleAlpha => [premul(p[0], p[1]), premul(p[0], p[1]), premul(p[0], p[1]), p[1]],
            png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
            png::ColorType::Indexed => return None,
        };
        d.copy_from_slice(&px);
    }
    Some(())
}

/// Dimensões do primeiro marcador SOF, lidas à mão percorrendo os segmentos (sem decodificar nada).
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2usize;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            0xFF => at += 1, // preenchimento
            0x01 | 0xD0..=0xD8 => at += 2,
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let seg = bytes.get(at + 4..at + 9)?;
                let h = u32::from(u16::from_be_bytes([seg[1], seg[2]]));
                let w = u32::from(u16::from_be_bytes([seg[3], seg[4]]));
                return Some((w, h));
            }
            0xD9 | 0xDA => return None,
            _ => {
                let len = usize::from(u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]));
                at += 2 + len.max(2);
            }
        }
    }
}

fn decode_jpeg(bytes: &[u8]) -> Result<Pixmap, ImageError> {
    // Cabeçalho próprio primeiro: não depende de o decodificador aceitar um arquivo incompleto.
    if let Some((w, h)) = jpeg_dims(bytes) {
        check_dims(u64::from(w), u64::from(h))?;
    }
    let mut decoder = zune_jpeg::JpegDecoder::new(Cursor::new(bytes));
    decoder.decode_headers().map_err(|_| ImageError::Invalid("cabeçalho JPEG"))?;
    let info = decoder.info().ok_or(ImageError::Invalid("cabeçalho JPEG"))?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    // O corpo só é decodificado depois de o cabeçalho passar nos limites.
    check_dims(u64::from(w), u64::from(h))?;
    let size = decoder.output_buffer_size().ok_or(ImageError::TooBig)?;
    let comps = size / (w as usize * h as usize);
    if !matches!(comps, 1 | 3) || size != w as usize * h as usize * comps {
        return Err(ImageError::Unsupported);
    }
    let mut out = vec![0u8; size];
    decoder.decode_into(&mut out).map_err(|_| ImageError::Invalid("dados JPEG"))?;
    let mut pixmap = Pixmap::new(w, h).ok_or(ImageError::Invalid("pixmap"))?;
    for (d, p) in pixmap.data_mut().chunks_exact_mut(4).zip(out.chunks_exact(comps)) {
        let px = if comps == 3 { [p[0], p[1], p[2], 255] } else { [p[0], p[0], p[0], 255] };
        d.copy_from_slice(&px);
    }
    Ok(pixmap)
}

/// Cabe em `max` mantendo a proporção; nunca amplia.
pub fn fit(src: &Pixmap, max: (u32, u32)) -> Pixmap {
    let (w, h) = (u64::from(src.width()), u64::from(src.height()));
    let (mw, mh) = (u64::from(max.0), u64::from(max.1));
    if w == 0 || h == 0 || (w <= mw && h <= mh) {
        return src.clone();
    }
    let (tw, th) = if w * mh > h * mw {
        (mw, ((h * mw + w / 2) / w).max(1))
    } else {
        (((w * mh + h / 2) / h).max(1), mh)
    };
    scale_to(src, tw as u32, th as u32).unwrap_or_else(|| src.clone())
}

/// Reduz pela metade (média 2×2) enquanto a fonte tiver o dobro do alvo nos dois eixos, depois escala.
fn scale_to(src: &Pixmap, tw: u32, th: u32) -> Option<Pixmap> {
    let mut cur = src.clone();
    while cur.width() / 2 >= tw && cur.height() / 2 >= th && cur.width() >= 2 && cur.height() >= 2 {
        cur = halve(&cur)?;
    }
    if cur.width() == tw && cur.height() == th {
        return Some(cur);
    }
    let mut out = Pixmap::new(tw, th)?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
    let transform = Transform::from_scale(tw as f32 / cur.width() as f32, th as f32 / cur.height() as f32);
    out.draw_pixmap(0, 0, cur.as_ref(), &paint, transform, None);
    Some(out)
}

fn halve(src: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (src.width() as usize / 2, src.height() as usize / 2);
    let mut out = Pixmap::new(w as u32, h as u32)?;
    let stride = src.width() as usize * 4;
    let data = src.data();
    for (y, row) in out.data_mut().chunks_exact_mut(w * 4).enumerate() {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            for (c, slot) in px.iter_mut().enumerate() {
                let at = |dy: usize, dx: usize| data.get((2 * y + dy) * stride + (2 * x + dx) * 4 + c).copied().map_or(0, u32::from);
                *slot = ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    Some(out)
}

/// Image se estilo Imagem ou (automático e imagem com algum lado ≥ `SMALL_IMAGE`);
/// sem imagem decodificada não há miniatura, então cai em Card/Compact.
pub fn choose_format(n: &Notification, decoded: Option<&Pixmap>) -> Format {
    let big = decoded.is_some_and(|p| p.width() >= SMALL_IMAGE || p.height() >= SMALL_IMAGE);
    match (n.hints.estilo, decoded) {
        (Some(Estilo::Imagem), Some(_)) => Format::Image,
        (Some(Estilo::Compacto), _) => Format::Compact,
        (None | Some(Estilo::Cartao | Estilo::Osd), Some(_)) if big => Format::Image,
        _ => Format::Card,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hints, HintValue, Notification, RawImage};
    use std::path::PathBuf;
    use tiny_skia::Pixmap;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/images").join(name)
    }
    fn blank(w: u32, h: u32) -> Pixmap {
        Pixmap::new(w, h).expect("pixmap")
    }
    fn notif(estilo: Option<&str>) -> Notification {
        let raw = estilo.map(|e| vec![("x-estilo".to_string(), HintValue::Str(e.to_string()))]).unwrap_or_default();
        Notification {
            id: 1, app_name: "app".into(), app_icon: String::new(), summary: "s".into(), body: String::new(),
            actions: vec![], hints: Hints::parse(raw), expire_timeout: -1,
        }
    }
    fn raw(ch: i32, bits: i32, stride: i32, data: Vec<u8>) -> RawImage {
        RawImage { width: 2, height: 2, rowstride: stride, has_alpha: ch == 4, bits_per_sample: bits, channels: ch, data }
    }
    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in bytes {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }
    fn tmp(tag: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("notificacoes-images-{}-{tag}", std::process::id()));
        std::fs::write(&p, bytes).expect("write tmp");
        p
    }

    #[test]
    fn png_and_jpeg_decode() {
        let p = decode_file(&fixture("print.png")).expect("png");
        assert_eq!((p.width(), p.height()), (800, 450));
        let j = decode_file(&fixture("foto.jpg")).expect("jpeg");
        assert_eq!((j.width(), j.height()), (400, 300));
        // JPEG é opaco.
        assert!(j.data().chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn interlaced_and_palette_png_decode() {
        let p = decode_file(&fixture("entrelacado.png")).expect("adam7");
        assert_eq!((p.width(), p.height()), (50, 30));
        // Gradiente azul→branco: nenhuma linha ficou vazia (transparente).
        assert!(p.data().chunks_exact(4).all(|px| px[3] == 255));
        let a = decode_file(&fixture("avatar.png")).expect("palette");
        assert_eq!((a.width(), a.height()), (64, 64));
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        let f = |w, h| { let o = fit(&blank(w, h), THUMB_MAX); (o.width(), o.height()) };
        assert_eq!(f(800, 450), (320, 180));
        assert_eq!(f(400, 300), (240, 180));
        assert_eq!(f(64, 64), (64, 64));
        assert_eq!(f(100, 4000), (5, 180));
    }

    #[test]
    fn raw_rgb_and_rgba_decode() {
        // 2×2 RGB, rowstride 8 (folga de 2 bytes por linha).
        let rgb = vec![10, 20, 30, 40, 50, 60, 0, 0, 70, 80, 90, 100, 110, 120, 0, 0];
        let p = decode_raw(&raw(3, 8, 8, rgb)).expect("rgb");
        assert_eq!(&p.data()[..8], &[10, 20, 30, 255, 40, 50, 60, 255]);
        assert_eq!(&p.data()[8..], &[70, 80, 90, 255, 100, 110, 120, 255]);
        // 2×2 RGBA, rowstride 10; alfa 128 pré-multiplica.
        let rgba = vec![200, 100, 50, 128, 255, 255, 255, 255, 9, 9, 0, 0, 0, 0, 0, 0, 0, 255, 1, 2, 3, 4, 9, 9];
        let img = RawImage { width: 2, height: 2, rowstride: 10, has_alpha: true, bits_per_sample: 8, channels: 4, data: rgba };
        let p = decode_raw(&img).expect("rgba");
        assert_eq!(&p.data()[..8], &[100, 50, 25, 128, 255, 255, 255, 255]);
        assert_eq!(&p.data()[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn raw_image_with_short_buffer_is_rejected() {
        assert!(matches!(decode_raw(&raw(3, 8, 6, vec![0; 11])), Err(ImageError::Invalid(_))));
        assert!(matches!(decode_raw(&raw(3, 8, 5, vec![0; 100])), Err(ImageError::Invalid(_))));
        let neg = RawImage { width: -1, height: 2, rowstride: 6, has_alpha: false, bits_per_sample: 8, channels: 3, data: vec![0; 16] };
        assert!(matches!(decode_raw(&neg), Err(ImageError::Invalid(_))));
    }

    #[test]
    fn raw_image_bad_channels_rejected() {
        assert!(matches!(decode_raw(&raw(2, 8, 4, vec![0; 16])), Err(ImageError::Unsupported)));
        assert!(matches!(decode_raw(&raw(3, 16, 12, vec![0; 24])), Err(ImageError::Unsupported)));
    }

    #[test]
    fn raw_image_over_limits_rejected() {
        let huge = RawImage { width: 20_000, height: 20_000, rowstride: 60_000, has_alpha: false, bits_per_sample: 8, channels: 3, data: vec![] };
        assert!(matches!(decode_raw(&huge), Err(ImageError::TooBig)));
    }

    #[test]
    fn png_over_pixel_limit_rejected_before_decode() {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut file = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        file.extend_from_slice(&ihdr);
        file.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        assert_eq!(file.len(), 33);
        let p = tmp("forjado.png", &file);
        let got = decode_file(&p);
        let _ = std::fs::remove_file(&p);
        assert!(matches!(got, Err(ImageError::TooBig)), "{got:?}");
    }

    #[test]
    fn jpeg_over_pixel_limit_rejected_before_decode() {
        // SOI + SOF0 declarando 30000×30000, 3 componentes; sem corpo.
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 17, 8];
        j.extend_from_slice(&30_000u16.to_be_bytes());
        j.extend_from_slice(&30_000u16.to_be_bytes());
        j.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        let p = tmp("forjado.jpg", &j);
        let got = decode_file(&p);
        let _ = std::fs::remove_file(&p);
        assert!(matches!(got, Err(ImageError::TooBig)), "{got:?}");
    }

    #[test]
    fn garbage_file_is_unsupported() {
        let got = decode_file(&fixture("quebrado.png"));
        assert!(matches!(got, Err(ImageError::Unsupported | ImageError::Invalid(_))), "{got:?}");
    }

    #[test]
    fn truncated_png_is_invalid() {
        let bytes = std::fs::read(fixture("print.png")).expect("read");
        let p = tmp("truncado.png", &bytes[..bytes.len() / 2]);
        let got = decode_file(&p);
        let _ = std::fs::remove_file(&p);
        assert!(matches!(got, Err(ImageError::Invalid(_))), "{got:?}");
    }

    #[test]
    fn fifo_and_directory_are_unsupported_without_blocking() {
        use crate::fsread::testutil::{make_fifo, tempdir, within_1s};
        let d = tempdir("images");
        let fifo = make_fifo(&d, "f.png");
        let got = within_1s(move || decode_file(&fifo));
        assert!(matches!(got, Err(ImageError::Unsupported)), "{got:?}");
        let dir = d.clone();
        let got = within_1s(move || decode_file(&dir));
        assert!(matches!(got, Err(ImageError::Unsupported)), "{got:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_is_io_error() {
        assert!(matches!(decode_file(&fixture("nao-existe.png")), Err(ImageError::Io(_))));
    }

    #[test]
    fn small_image_becomes_card_icon() {
        let avatar = decode_file(&fixture("avatar.png")).expect("avatar");
        assert_eq!(choose_format(&notif(None), Some(&avatar)), Format::Card);
        let print = decode_file(&fixture("print.png")).expect("print");
        assert_eq!(choose_format(&notif(None), Some(&print)), Format::Image);
        assert_eq!(choose_format(&notif(None), None), Format::Card);
    }

    #[test]
    fn estilo_imagem_forces_image_format() {
        let avatar = decode_file(&fixture("avatar.png")).expect("avatar");
        assert_eq!(choose_format(&notif(Some("imagem")), Some(&avatar)), Format::Image);
        assert_eq!(choose_format(&notif(Some("compacto")), Some(&avatar)), Format::Compact);
        // Sem imagem decodificada não há miniatura: cai no cartão comum.
        assert_eq!(choose_format(&notif(Some("imagem")), None), Format::Card);
    }
}
