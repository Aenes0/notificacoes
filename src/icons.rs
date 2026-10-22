//! Ícones de app: tema de ícones (spec freedesktop), `.desktop`, PNG/SVG rasterizados.
//! Tudo que vem do disco é tratado como não confiável: falha vira `None`, nunca pânico.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Cursor;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::model::{ImageHint, Notification};
use crate::render::theme::{Rgba, latest_version_dir};

/// Tema padrão do COSMIC e último recurso da spec.
const COSMIC_THEME: &str = "Cosmic";
const FALLBACK_THEME: &str = "hicolor";
/// Entradas do cache de pixmaps rasterizados.
const LRU_CAPACITY: usize = 32;
/// Maior arquivo de ícone lido (bytes) e maior lado aceito (pixels).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_SIDE: u32 = 8192;
/// Área máxima de um PNG decodificado (16 Mpx = 64 MiB em RGBA).
const MAX_SOURCE_AREA: u64 = 16_777_216;
const MAX_TARGET_SIDE: u32 = 4096;

#[derive(Debug, Clone)]
pub struct IconRoots { pub app_dirs: Vec<PathBuf>, pub icon_dirs: Vec<PathBuf> }

impl IconRoots {
    /// Mesmos diretórios que a Central usa: `$XDG_DATA_HOME`, `$XDG_DATA_DIRS` e os exports do flatpak.
    pub fn from_env() -> IconRoots {
        let env = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
        let home = env("HOME").map(PathBuf::from);
        let data_home = env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let data_dirs = env("XDG_DATA_DIRS").unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));

        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.extend(data_home);
        dirs.extend(std::env::split_paths(&data_dirs));
        dirs.extend(home.as_ref().map(|h| h.join(".local/share/flatpak/exports/share")));
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        let mut seen = HashSet::new();
        dirs.retain(|d| d.is_absolute() && seen.insert(d.clone()));

        let mut icon_dirs: Vec<PathBuf> = home.iter().map(|h| h.join(".icons")).collect();
        icon_dirs.extend(dirs.iter().map(|d| d.join("icons")));
        icon_dirs.push(PathBuf::from("/usr/share/pixmaps"));
        IconRoots { app_dirs: dirs.iter().map(|d| d.join("applications")).collect(), icon_dirs }
    }
}

/// Campos do `[Desktop Entry]` de um `.desktop`; `id`, `name` e `wm_class` já em minúsculas.
#[derive(Debug, Clone)]
pub struct DesktopEntry { pub id: String, pub name: String, pub wm_class: String, pub icon: String }

/// Ícone do app a partir do `.desktop`: pelo id que o app informa, senão pelo nome do app.
pub fn desktop_icon(entries: &[DesktopEntry], app: &str, desktop_entry: Option<&str>) -> Option<String> {
    let app = app.trim().to_lowercase();
    let wanted = desktop_entry.map(|e| e.trim().trim_end_matches(".desktop").to_lowercase()).filter(|e| !e.is_empty());
    let by_hint = wanted.and_then(|id| {
        entries
            .iter()
            .find(|e| e.id == id)
            .or_else(|| entries.iter().find(|e| e.id.rsplit('.').next() == Some(id.as_str())))
    });
    let found = by_hint.or_else(|| {
        if app.is_empty() {
            return None;
        }
        let dashed = app.replace(' ', "-");
        entries
            .iter()
            .find(|e| e.name == app)
            .or_else(|| entries.iter().find(|e| e.wm_class == app || e.id == dashed))
            .or_else(|| entries.iter().find(|e| e.id.rsplit('.').next() == Some(dashed.as_str())))
    });
    found.map(|e| e.icon.clone())
}

fn read_text(path: &Path) -> Option<String> {
    String::from_utf8(crate::fsread::read_regular(path, MAX_FILE_BYTES).ok()?).ok()
}

/// Todos os `.desktop` dos diretórios de apps, na ordem dos diretórios; só o grupo `[Desktop Entry]`
/// (o `Icon=` de uma `[Desktop Action]` não conta). Entradas sem `Icon` são descartadas.
pub fn load_entries(roots: &IconRoots) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    for dir in &roots.app_dirs {
        let Ok(files) = std::fs::read_dir(dir) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            let Some(text) = read_text(&path) else { continue };
            let mut entry = DesktopEntry {
                id: path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase(),
                name: String::new(),
                wm_class: String::new(),
                icon: String::new(),
            };
            for line in text.lines().skip_while(|l| l.trim() != "[Desktop Entry]").skip(1).take_while(|l| !l.starts_with('[')) {
                match line.split_once('=') {
                    Some(("Name", v)) => entry.name = v.trim().to_lowercase(),
                    Some(("StartupWMClass", v)) => entry.wm_class = v.trim().to_lowercase(),
                    Some(("Icon", v)) => entry.icon = v.trim().to_owned(),
                    _ => {}
                }
            }
            if !entry.icon.is_empty() {
                entries.push(entry);
            }
        }
    }
    entries
}

// ---------------------------------------------------------------- tema de ícones

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirKind { Fixed, Scalable, Threshold }

#[derive(Debug, Clone)]
struct SubDir { path: String, size: i64, scale: i64, kind: DirKind, min: i64, max: i64, threshold: i64 }

impl SubDir {
    /// `DirectorySizeDistance` da spec: 0 quando o diretório serve ao tamanho pedido.
    fn distance(&self, size: i64, scale: i64) -> i64 {
        let want = size * scale;
        match self.kind {
            DirKind::Fixed => (self.size * self.scale - want).abs(),
            DirKind::Scalable => range_distance(want, self.min * self.scale, self.max * self.scale),
            DirKind::Threshold => {
                range_distance(want, (self.size - self.threshold) * self.scale, (self.size + self.threshold) * self.scale)
            }
        }
    }
}

fn range_distance(want: i64, low: i64, high: i64) -> i64 {
    if want < low { low - want } else if want > high { want - high } else { 0 }
}

#[derive(Debug, Default)]
struct ThemeIndex { inherits: Vec<String>, dirs: Vec<SubDir> }

/// INI mínimo: seções e pares `chave=valor`; comentários (`#`) e linhas soltas são ignorados.
fn parse_ini(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.to_owned(), Vec::new()));
        } else if let (Some((k, v)), Some(section)) = (line.split_once('='), sections.last_mut()) {
            section.1.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    sections
}

fn load_theme(roots: &IconRoots, name: &str) -> Option<ThemeIndex> {
    if name.is_empty() || name.contains('/') || name == ".." {
        return None;
    }
    let text = roots.icon_dirs.iter().find_map(|dir| read_text(&dir.join(name).join("index.theme")))?;
    let sections = parse_ini(&text);
    let field = |section: &[(String, String)], key: &str| section.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let main = &sections.iter().find(|(n, _)| n == "Icon Theme")?.1;
    let list = |key: &str| -> Vec<String> {
        field(main, key).map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    };
    let inherits = list("Inherits");
    let dirs = list("Directories")
        .into_iter()
        .chain(list("ScaledDirectories"))
        .filter(|path| !path.split('/').any(|part| part == ".."))
        .filter_map(|path| {
            let section = &sections.iter().find(|(n, _)| *n == path)?.1;
            let num = |key: &str| field(section, key).and_then(|v| v.parse::<i64>().ok());
            let size = num("Size")?;
            let kind = match field(section, "Type").as_deref() {
                Some("Fixed") => DirKind::Fixed,
                Some("Scalable") => DirKind::Scalable,
                _ => DirKind::Threshold,
            };
            Some(SubDir {
                path,
                size,
                scale: num("Scale").filter(|s| *s > 0).unwrap_or(1),
                kind,
                min: num("MinSize").unwrap_or(size),
                max: num("MaxSize").unwrap_or(size),
                threshold: num("Threshold").unwrap_or(2),
            })
        })
        .collect();
    Some(ThemeIndex { inherits, dirs })
}

/// Cadeia da spec: o tema, seus `Inherits` (recursivo, sem repetir), `Cosmic` e `hicolor`.
fn theme_chain(roots: &IconRoots, theme: &str) -> Vec<(String, ThemeIndex)> {
    fn visit(roots: &IconRoots, name: &str, seen: &mut HashSet<String>, out: &mut Vec<(String, ThemeIndex)>) {
        if !seen.insert(name.to_owned()) {
            return;
        }
        let Some(index) = load_theme(roots, name) else { return };
        let parents = index.inherits.clone();
        out.push((name.to_owned(), index));
        for parent in parents {
            visit(roots, &parent, seen, out);
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for name in [theme, COSMIC_THEME, FALLBACK_THEME] {
        visit(roots, name, &mut seen, &mut out);
    }
    out
}

fn probe(roots: &IconRoots, theme: &str, sub: &str, name: &str) -> Option<PathBuf> {
    roots.icon_dirs.iter().find_map(|dir| {
        ["png", "svg"].iter().map(|ext| dir.join(theme).join(sub).join(format!("{name}.{ext}"))).find(|p| p.is_file())
    })
}

/// `file://` com `%XX` ou caminho absoluto → o caminho; qualquer outra coisa → `None`.
fn path_from_name(name: &str) -> Option<PathBuf> {
    let raw = match name.strip_prefix("file://") {
        Some(rest) => decode_percent(rest),
        None => OsString::from(name),
    };
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

fn decode_percent(s: &str) -> OsString {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    OsString::from_vec(out)
}

/// Procura o ícone pela spec de temas; `name` que é caminho absoluto ou `file://` vale por si.
pub fn find_icon(roots: &IconRoots, theme: &str, name: &str, size: u32, scale: u32) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') || name.starts_with("file://") {
        return path_from_name(name).filter(|p| p.is_file());
    }
    if name.contains('/') || name == ".." {
        return None;
    }
    let (size, scale) = (i64::from(size), i64::from(scale.max(1)));
    let chain = theme_chain(roots, theme);

    // Por tema, na ordem da cadeia: a exata, senão a de menor distância dentro dele; achou, retorna.
    for (theme_name, index) in &chain {
        let mut dirs: Vec<(i64, &SubDir)> = index.dirs.iter().map(|s| (s.distance(size, scale), s)).collect();
        dirs.sort_by_key(|(d, _)| *d);
        if let Some(path) = dirs.into_iter().find_map(|(_, s)| probe(roots, theme_name, &s.path, name)) {
            return Some(path);
        }
    }
    // Último recurso: os `pixmaps`, sem tema.
    roots
        .icon_dirs
        .iter()
        .filter(|d| d.file_name().is_some_and(|n| n == "pixmaps"))
        .find_map(|dir| ["png", "svg"].iter().map(|ext| dir.join(format!("{name}.{ext}"))).find(|p| p.is_file()))
}

/// `CosmicTk/v*/icon_theme` (string RON entre aspas); padrão `Cosmic`. `cosmic_dir` = `~/.config/cosmic`.
pub fn icon_theme_name(cosmic_dir: &Path) -> String {
    latest_version_dir(&cosmic_dir.join("com.system76.CosmicTk"))
        .and_then(|dir| read_text(&dir.join("icon_theme")))
        .and_then(|text| text.trim().strip_prefix('"').and_then(|rest| rest.split('"').next()).map(str::to_owned))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| COSMIC_THEME.to_owned())
}

// ---------------------------------------------------------------- rasterização

/// Rasteriza PNG ou SVG contido em `px`×`px` (mantém a proporção; o lado maior vale `px`).
/// Com `tint`, troca a cor e preserva o alfa (ícones `-symbolic`).
pub fn rasterize(path: &Path, px: u32, tint: Option<Rgba>) -> Option<Pixmap> {
    if px == 0 || px > MAX_TARGET_SIDE {
        return None;
    }
    let bytes = crate::fsread::read_regular(path, MAX_FILE_BYTES).ok()?;
    // SVGZ: o usvg descompacta gzip sem limite (bomba de descompressão). Temas não usam svgz.
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return None;
    }
    let mut pixmap = if bytes.starts_with(b"\x89PNG") {
        scale_to_fit(decode_png(&bytes)?, px)?
    } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        render_svg(&bytes, px)?
    } else {
        return None;
    };
    if let Some(tint) = tint {
        apply_tint(&mut pixmap, tint);
    }
    Some(pixmap)
}

fn decode_png(bytes: &[u8]) -> Option<Pixmap> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    // Antes de qualquer alocação: o cabeçalho pode ser forjado.
    if width == 0
        || height == 0
        || width > MAX_SOURCE_SIDE
        || height > MAX_SOURCE_SIDE
        || u64::from(width) * u64::from(height) > MAX_SOURCE_AREA
    {
        return None;
    }
    let (color, depth) = reader.output_color_type();
    if depth != png::BitDepth::Eight {
        return None;
    }
    let stride = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    let mut pixmap = Pixmap::new(width, height)?;
    let row_bytes = width as usize * 4;
    // Linha a linha direto no pixmap: não existe um segundo buffer da imagem inteira.
    // No Adam7 cada linha é de um passe (mais curta): vai para um scratch de uma linha e é espalhada.
    let mut scratch = vec![0u8; row_bytes];
    let mut y = 0usize;
    while let Some(row) = reader.next_interlaced_row().ok()? {
        match row.interlace() {
            png::InterlaceInfo::Adam7(info) => {
                let n = row.data().len() / stride;
                let used = scratch.get_mut(..n * 4)?;
                convert_row(color, row.data(), stride, used)?;
                png::expand_interlaced_row(pixmap.data_mut(), row_bytes, used, info, 32);
            }
            _ => {
                let dst = pixmap.data_mut().chunks_exact_mut(row_bytes).nth(y)?;
                convert_row(color, row.data(), stride, dst)?;
                y += 1;
            }
        }
    }
    Some(pixmap)
}

/// Uma linha decodificada (8 bits) → RGBA pré-multiplicado; `None` se faltar dado.
fn convert_row(color: png::ColorType, src: &[u8], stride: usize, dst: &mut [u8]) -> Option<()> {
    let premul = |c: u8, a: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
    let ok = match color {
        png::ColorType::Rgba => fill(dst, src, stride, |p| [premul(p[0], p[3]), premul(p[1], p[3]), premul(p[2], p[3]), p[3]]),
        png::ColorType::Rgb => fill(dst, src, stride, |p| [p[0], p[1], p[2], 255]),
        png::ColorType::GrayscaleAlpha => fill(dst, src, stride, |p| [premul(p[0], p[1]), premul(p[0], p[1]), premul(p[0], p[1]), p[1]]),
        png::ColorType::Grayscale => fill(dst, src, stride, |p| [p[0], p[0], p[0], 255]),
        png::ColorType::Indexed => false,
    };
    ok.then_some(())
}

/// Converte `src` (pixels de `stride` bytes) em RGBA pré-multiplicado; `false` se faltar dado.
fn fill(out: &mut [u8], src: &[u8], stride: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> bool {
    if src.len() < out.len() / 4 * stride {
        return false;
    }
    for (dst, px) in out.chunks_exact_mut(4).zip(src.chunks_exact(stride)) {
        dst.copy_from_slice(&f(px));
    }
    true
}

/// Reduz pela metade (média 2×2) enquanto a fonte tiver mais que o dobro do alvo, depois escala.
fn scale_to_fit(mut src: Pixmap, px: u32) -> Option<Pixmap> {
    while src.width().max(src.height()) >= px.saturating_mul(2) && src.width() >= 2 && src.height() >= 2 {
        src = halve(&src)?;
    }
    let side = src.width().max(src.height());
    if side == px {
        return Some(src);
    }
    let ratio = px as f32 / side as f32;
    let (w, h) = (((src.width() as f32 * ratio).round() as u32).max(1), ((src.height() as f32 * ratio).round() as u32).max(1));
    let mut out = Pixmap::new(w, h)?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
    let transform = Transform::from_scale(w as f32 / src.width() as f32, h as f32 / src.height() as f32);
    out.draw_pixmap(0, 0, src.as_ref(), &paint, transform, None);
    Some(out)
}

fn halve(src: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (src.width() / 2, src.height() / 2);
    let mut out = Pixmap::new(w, h)?;
    let stride = src.width() as usize * 4;
    let data = src.data();
    for y in 0..h as usize {
        for x in 0..w as usize {
            for c in 0..4 {
                let at = |dy: usize, dx: usize| data.get((2 * y + dy) * stride + (2 * x + dx) * 4 + c).copied().map_or(0, u32::from);
                let avg = (at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4;
                if let Some(slot) = out.data_mut().get_mut((y * w as usize + x) * 4 + c) {
                    *slot = avg as u8;
                }
            }
        }
    }
    Some(out)
}

fn render_svg(bytes: &[u8], px: u32) -> Option<Pixmap> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let (w, h) = (tree.size().width(), tree.size().height());
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let ratio = px as f32 / w.max(h);
    let (tw, th) = (((w * ratio).round() as u32).max(1), ((h * ratio).round() as u32).max(1));
    let mut pixmap = Pixmap::new(tw, th)?;
    let transform = Transform::from_scale(tw as f32 / w, th as f32 / h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

/// Troca a cor mantendo a forma: alfa = alfa do pixel × alfa da tinta.
fn apply_tint(pixmap: &mut Pixmap, tint: Rgba) {
    let scale = |c: u8, a: u32| ((u32::from(c) * a + 127) / 255) as u8;
    for px in pixmap.data_mut().chunks_exact_mut(4) {
        let alpha = (u32::from(px[3]) * u32::from(tint.3) + 127) / 255;
        px.copy_from_slice(&[scale(tint.0, alpha), scale(tint.1, alpha), scale(tint.2, alpha), alpha as u8]);
    }
}

// ---------------------------------------------------------------- fachada

type CacheKey = (String, u32, Option<Rgba>);

/// Resolve e guarda ícones. As entradas `.desktop` são lidas na primeira necessidade.
pub struct Icons {
    roots: IconRoots,
    theme: String,
    entries: Option<Vec<DesktopEntry>>,
    /// LRU: o mais recente no fim.
    cache: Vec<(CacheKey, Option<Pixmap>)>,
}

fn is_symbolic(name: &str) -> bool {
    let stem = name.strip_suffix(".png").or_else(|| name.strip_suffix(".svg")).unwrap_or(name);
    stem.ends_with("-symbolic")
}

impl Icons {
    pub fn new(roots: IconRoots, theme: String) -> Icons {
        Icons { roots, theme, entries: None, cache: Vec::new() }
    }

    /// Ordem: `desktop-entry` → nome do app (ambos via `.desktop`) → `app_icon` → `ImageHint::IconName`/`File`.
    pub fn app_icon(&mut self, n: &Notification, px: u32, tint: Rgba) -> Option<Pixmap> {
        let roots = &self.roots;
        let entries = self.entries.get_or_insert_with(|| load_entries(roots));
        let mut candidates: Vec<String> = Vec::new();
        candidates.extend(desktop_icon(entries, &n.app_name, n.hints.desktop_entry.as_deref()));
        candidates.push(n.app_icon.clone());
        match &n.hints.image {
            Some(ImageHint::IconName(name)) => candidates.push(name.clone()),
            Some(ImageHint::File(path)) => candidates.push(path.to_string_lossy().into_owned()),
            Some(ImageHint::Data(_)) | None => {}
        }
        candidates.into_iter().find_map(|name| self.named(&name, px, tint))
    }

    /// Ícone do cartão redigido (modo privado): o do `.desktop`, senão `app_icon` só se for nome
    /// de ícone. Caminho ou URI em `app_icon` e a imagem da notificação ficam de fora: costumam
    /// ser a foto de quem mandou ou o conteúdo da mensagem.
    pub fn app_icon_redacted(&mut self, n: &Notification, px: u32, tint: Rgba) -> Option<Pixmap> {
        let roots = &self.roots;
        let entries = self.entries.get_or_insert_with(|| load_entries(roots));
        let mut candidates: Vec<String> = Vec::new();
        candidates.extend(desktop_icon(entries, &n.app_name, n.hints.desktop_entry.as_deref()));
        let name = n.app_icon.trim();
        if !name.contains('/') && !name.starts_with("file:") {
            candidates.push(name.to_owned());
        }
        candidates.into_iter().find_map(|name| self.named(&name, px, tint))
    }

    /// Ícone por nome, caminho ou URI. `-symbolic` leva a tinta.
    pub fn named(&mut self, name: &str, px: u32, tint: Rgba) -> Option<Pixmap> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let key: CacheKey = (name.to_owned(), px, is_symbolic(name).then_some(tint));
        if let Some(at) = self.cache.iter().position(|(k, _)| *k == key) {
            let entry = self.cache.remove(at);
            let hit = entry.1.clone();
            self.cache.push(entry);
            return hit;
        }
        let pixmap = find_icon(&self.roots, &self.theme, name, px, 1).and_then(|path| rasterize(&path, px, key.2));
        if self.cache.len() >= LRU_CAPACITY {
            self.cache.remove(0);
        }
        self.cache.push((key, pixmap.clone()));
        pixmap
    }

    /// Solta o cache (tela vazia). As entradas `.desktop` ficam.
    pub fn clear(&mut self) {
        self.cache = Vec::new();
    }

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hints, ImageHint};
    use std::fs;

    fn fixtures() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures") }
    fn roots() -> IconRoots {
        IconRoots { app_dirs: vec![fixtures().join("applications")], icon_dirs: vec![fixtures().join("icons")] }
    }
    fn entries() -> Vec<DesktopEntry> { load_entries(&roots()) }
    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notificacoes-icons-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn notif(app: &str, icon: &str, hints: Hints) -> Notification {
        Notification {
            id: 1, app_name: app.into(), app_icon: icon.into(), summary: String::new(), body: String::new(),
            actions: vec![], hints, expire_timeout: -1,
        }
    }
    fn px(p: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let c = p.pixel(x, y).unwrap().demultiply();
        (c.red(), c.green(), c.blue(), c.alpha())
    }

    #[test] fn desktop_entry_hint_wins() {
        assert_eq!(desktop_icon(&entries(), "Qualquer", Some("org.exemplo.Chat")).as_deref(), Some("app-a"));
    }
    #[test] fn app_name_matches_name_or_wm_class() {
        assert_eq!(desktop_icon(&entries(), "Chat", None).as_deref(), Some("app-a"));
        assert_eq!(desktop_icon(&entries(), "chatapp", None).as_deref(), Some("app-a"));
    }
    #[test] fn flatpak_style_suffix_matches() {
        assert_eq!(desktop_icon(&entries(), "", Some("Chat")).as_deref(), Some("app-a"));
    }
    #[test] fn desktop_action_icon_is_ignored() {
        assert_eq!(desktop_icon(&entries(), "", Some("brave-abc")).as_deref(), Some("brave-browser"));
    }
    #[test] fn exact_size_png_found() {
        assert_eq!(find_icon(&roots(), "Teste", "app-a", 48, 1), Some(fixtures().join("icons/Teste/48x48/apps/app-a.png")));
    }
    #[test] fn inherits_then_hicolor() {
        assert_eq!(find_icon(&roots(), "Teste", "app-b", 48, 1), Some(fixtures().join("icons/Pai/scalable/apps/app-b.svg")));
        assert_eq!(find_icon(&roots(), "Teste", "app-c", 48, 1), Some(fixtures().join("icons/hicolor/32x32/apps/app-c.png")));
    }
    #[test] fn absolute_path_and_file_uri() {
        let real = fixtures().join("icons/Teste/48x48/apps/app-a.png");
        let s = real.to_str().unwrap();
        assert_eq!(find_icon(&roots(), "Teste", s, 48, 1), Some(real.clone()));
        assert_eq!(find_icon(&roots(), "Teste", &format!("file://{s}"), 48, 1), Some(real));
        assert_eq!(find_icon(&roots(), "Teste", "/nao/existe.png", 48, 1), None);
        assert_eq!(find_icon(&roots(), "Teste", "file:///nao/existe.png", 48, 1), None);
    }
    #[test] fn svg_rasterized_to_requested_size() {
        let p = rasterize(&fixtures().join("icons/Pai/scalable/apps/app-b.svg"), 40, Some(Rgba(0, 255, 0, 255))).unwrap();
        assert_eq!((p.width(), p.height()), (40, 40));
        assert_eq!(px(&p, 20, 20), (0, 255, 0, 255));
    }
    #[test] fn symbolic_is_tinted() {
        let path = find_icon(&roots(), "Teste", "algo-symbolic", 16, 1).unwrap();
        let p = rasterize(&path, 16, Some(Rgba(255, 0, 0, 255))).unwrap();
        assert_eq!(px(&p, 8, 8), (255, 0, 0, 255));
        let mut icons = Icons::new(roots(), "Teste".into());
        let p = icons.named("algo-symbolic", 16, Rgba(255, 0, 0, 255)).unwrap();
        assert_eq!(px(&p, 8, 8), (255, 0, 0, 255));
    }
    #[test] fn missing_icon_is_none() {
        assert_eq!(find_icon(&roots(), "Teste", "nao-existe-mesmo", 48, 1), None);
        let mut icons = Icons::new(roots(), "Teste".into());
        assert!(icons.named("nao-existe-mesmo", 48, Rgba(0, 0, 0, 255)).is_none());
        assert!(rasterize(Path::new("/nao/existe.png"), 48, None).is_none());
    }
    #[test] fn icon_theme_from_cosmic_config() {
        let d = tempdir("tema");
        assert_eq!(icon_theme_name(&d), "Cosmic");
        let v1 = d.join("com.system76.CosmicTk/v1");
        fs::create_dir_all(&v1).unwrap();
        fs::write(v1.join("icon_theme"), "\"Breeze-Fino\"").unwrap();
        assert_eq!(icon_theme_name(&d), "Breeze-Fino");
        let _ = fs::remove_dir_all(&d);
    }

    #[test] fn png_is_scaled_to_fit() {
        let p = rasterize(&fixtures().join("icons/Teste/48x48/apps/app-a.png"), 24, None).unwrap();
        assert_eq!((p.width(), p.height()), (24, 24));
        assert_eq!(px(&p, 12, 12), (0x30, 0x80, 0xe0, 255));
    }
    #[test] fn app_icon_order_and_fallbacks() {
        let mut icons = Icons::new(roots(), "Teste".into());
        let tint = Rgba(255, 255, 255, 255);
        let hints = Hints { desktop_entry: Some("org.exemplo.Chat".into()), ..Hints::default() };
        let p = icons.app_icon(&notif("x", "app-b", hints), 48, tint).unwrap();
        assert_eq!(px(&p, 24, 24), (0x30, 0x80, 0xe0, 255)); // app-a (desktop) vence app-b
        let p = icons.app_icon(&notif("Chat", "app-b", Hints::default()), 48, tint).unwrap();
        assert_eq!(px(&p, 24, 24), (0x30, 0x80, 0xe0, 255)); // por nome do app
        let p = icons.app_icon(&notif("Desconhecido", "app-b", Hints::default()), 48, tint).unwrap();
        assert_eq!(px(&p, 24, 24), (0xe0, 0x30, 0x30, 255)); // pelo app_icon
        let hints = Hints { image: Some(ImageHint::IconName("app-c".into())), ..Hints::default() };
        let p = icons.app_icon(&notif("Desconhecido", "", hints), 32, tint).unwrap();
        assert_eq!(px(&p, 16, 16), (0x30, 0xe0, 0x80, 255)); // IconName
        assert!(icons.app_icon(&notif("Desconhecido", "", Hints::default()), 32, tint).is_none());
        icons.clear();
    }
    #[test] fn redacted_icon_skips_paths_and_image_hints() {
        let mut icons = Icons::new(roots(), "Teste".into());
        let tint = Rgba(255, 255, 255, 255);
        let real = fixtures().join("icons/Teste/48x48/apps/app-a.png");
        let path = real.to_str().unwrap();
        // Caminho ou URI em app_icon pode ser a foto de quem mandou: fica de fora.
        assert!(icons.app_icon_redacted(&notif("Desconhecido", path, Hints::default()), 48, tint).is_none());
        let uri = format!("file://{path}");
        assert!(icons.app_icon_redacted(&notif("Desconhecido", &uri, Hints::default()), 48, tint).is_none());
        // A imagem da notificação (arquivo ou nome) também.
        let hints = Hints { image: Some(ImageHint::File(real.clone())), ..Hints::default() };
        assert!(icons.app_icon_redacted(&notif("Desconhecido", "", hints), 48, tint).is_none());
        let hints = Hints { image: Some(ImageHint::IconName("app-c".into())), ..Hints::default() };
        assert!(icons.app_icon_redacted(&notif("Desconhecido", "", hints), 32, tint).is_none());
        // Nome de ícone em app_icon e o ícone do .desktop valem.
        let p = icons.app_icon_redacted(&notif("Desconhecido", "app-b", Hints::default()), 48, tint).unwrap();
        assert_eq!(px(&p, 24, 24), (0xe0, 0x30, 0x30, 255));
        let hints = Hints { desktop_entry: Some("org.exemplo.Chat".into()), ..Hints::default() };
        let p = icons.app_icon_redacted(&notif("x", path, hints), 48, tint).unwrap();
        assert_eq!(px(&p, 24, 24), (0x30, 0x80, 0xe0, 255));
    }
    #[test] fn lru_is_bounded() {
        let mut icons = Icons::new(roots(), "Teste".into());
        for size in 8..8 + 40 { let _ = icons.named("app-a", size, Rgba(0, 0, 0, 255)); }
        assert!(icons.cache_len() <= 32);
        icons.clear();
        assert_eq!(icons.cache_len(), 0);
    }
    #[test] fn threshold_and_distance_pick_closest() {
        let d = tempdir("dist");
        let t = d.join("T");
        for sub in ["16x16/apps", "64x64/apps"] { fs::create_dir_all(t.join(sub)).unwrap(); fs::write(t.join(sub).join("i.png"), b"").unwrap(); }
        fs::write(t.join("index.theme"), "[Icon Theme]\nInherits=\nDirectories=16x16/apps,64x64/apps\n[16x16/apps]\nSize=16\nType=Fixed\n[64x64/apps]\nSize=64\nType=Fixed\n").unwrap();
        let r = IconRoots { app_dirs: vec![], icon_dirs: vec![d.clone()] };
        assert_eq!(find_icon(&r, "T", "i", 20, 1), Some(t.join("16x16/apps/i.png")));
        assert_eq!(find_icon(&r, "T", "i", 48, 1), Some(t.join("64x64/apps/i.png")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test] fn closest_in_user_theme_beats_exact_in_hicolor() {
        let d = tempdir("ordem");
        for (theme, sub) in [("T", "64x64/apps"), ("hicolor", "48x48/apps")] {
            fs::create_dir_all(d.join(theme).join(sub)).unwrap();
            fs::write(d.join(theme).join(sub).join("x.png"), b"").unwrap();
        }
        fs::write(d.join("T/index.theme"), "[Icon Theme]\nDirectories=64x64/apps\n[64x64/apps]\nSize=64\nType=Fixed\n").unwrap();
        fs::write(d.join("hicolor/index.theme"), "[Icon Theme]\nDirectories=48x48/apps\n[48x48/apps]\nSize=48\nType=Fixed\n").unwrap();
        let r = IconRoots { app_dirs: vec![], icon_dirs: vec![d.clone()] };
        assert_eq!(find_icon(&r, "T", "x", 48, 1), Some(d.join("T/64x64/apps/x.png")));
        let _ = fs::remove_dir_all(&d);
    }
    #[test] fn svgz_is_refused() {
        let d = tempdir("svgz");
        let gz: &[u8] = &[0x1f,0x8b,0x08,0x00,0x00,0x00,0x00,0x00,0x02,0xff,0x75,0x8c,0xb1,0x0e,0x83,0x30,0x0c,0x44,0xf7,0x7e,0x85,0xe5,0xee,0xd8,0x2a,0x0c,0xa8,0x4a,0x32,0xf0,0x2b,0x25,0x4d,0x22,0x85,0x52,0x81,0x15,0xe7,0xf3,0x09,0x62,0x46,0x77,0xc3,0x1b,0xee,0x9d,0xd9,0x4b,0x80,0xba,0xe4,0xdf,0x6e,0x31,0x8a,0xfc,0xdf,0x44,0xaa,0xda,0x69,0xdf,0xad,0x5b,0xa0,0x17,0x33,0x53,0x5b,0x20,0x68,0x9a,0x25,0x5a,0x1c,0x46,0x84,0xe8,0x53,0x88,0x72,0x71,0x49,0x5e,0xa7,0xb5,0x5a,0x64,0x60,0x18,0xc6,0x56,0x74,0x66,0xf3,0x1f,0xb9,0x13,0xbe,0x29,0x67,0x8b,0x4f,0xcf,0x7d,0x0b,0x92,0x33,0xe7,0xbd,0x7b,0x1c,0xe0,0xae,0x38,0x0c,0x87,0x00,0x00,0x00];
        let path = d.join("a.svg");
        fs::write(&path, gz).unwrap();
        assert!(rasterize(&path, 32, None).is_none());
        let _ = fs::remove_dir_all(&d);
    }
    #[test] fn png_with_forged_huge_header_is_refused() {
        let d = tempdir("pngbomba");
        let hdr: &[u8] = &[0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0x00,0x00,0x00,0x0d,0x49,0x48,0x44,0x52,0x00,0x00,0x20,0x00,0x00,0x00,0x20,0x00,0x08,0x06,0x00,0x00,0x00,0x72,0xaa,0xca,0x59]; // IHDR 8192x8192 RGBA (67 Mpx), sem dados
        let path = d.join("a.png");
        fs::write(&path, hdr).unwrap();
        assert!(rasterize(&path, 32, None).is_none());
        let _ = fs::remove_dir_all(&d);
    }
    #[test] fn traversal_is_rejected() {
        let d = tempdir("travessia");
        let icons = d.join("icons");
        fs::create_dir_all(icons.join("T/48x48/apps")).unwrap();
        fs::create_dir_all(icons.join("hicolor/48x48/apps")).unwrap();
        fs::create_dir_all(d.join("x")).unwrap();
        fs::write(d.join("etc-passwd"), b"").unwrap();
        fs::write(icons.join("T/48x48/apps/org.exemplo.app.png"), b"").unwrap();
        fs::write(d.join("x/index.theme"), "[Icon Theme]\nDirectories=48x48/apps\n[48x48/apps]\nSize=48\nType=Fixed\n").unwrap();
        fs::create_dir_all(d.join("x/48x48/apps")).unwrap();
        fs::write(d.join("x/48x48/apps/vazado.png"), b"").unwrap();
        fs::write(icons.join("T/index.theme"), "[Icon Theme]\nInherits=../x\nDirectories=48x48/apps,../../x/48x48/apps\n[48x48/apps]\nSize=48\nType=Fixed\n[../../x/48x48/apps]\nSize=48\nType=Fixed\n").unwrap();
        let r = IconRoots { app_dirs: vec![], icon_dirs: vec![icons.clone()] };
        for nome in ["../../etc-passwd", "../x/48x48/apps/vazado", "a/b", "..", "."] {
            assert_eq!(find_icon(&r, "T", nome, 48, 1), None, "{nome}");
        }
        assert_eq!(find_icon(&r, "T", "vazado", 48, 1), None);
        assert_eq!(find_icon(&r, "../x", "vazado", 48, 1), None);
        assert_eq!(find_icon(&r, "T", "org.exemplo.app", 48, 1), Some(icons.join("T/48x48/apps/org.exemplo.app.png")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test] fn adam7_png_is_decoded() {
        let dir = fixtures().join("png");
        for kind in ["rgba", "pal", "rgba16"] {
            let plain = rasterize(&dir.join(format!("{kind}-plano.png")), 21, None).unwrap();
            let inter = rasterize(&dir.join(format!("{kind}-adam7.png")), 21, None)
                .unwrap_or_else(|| panic!("{kind}: PNG entrelaçado não decodificou"));
            assert_eq!((inter.width(), inter.height()), (21, 13), "{kind}");
            assert_eq!(plain.data(), inter.data(), "{kind}");
            if kind != "pal" { assert!(plain.data().chunks_exact(4).any(|p| p[3] != 0 && p[3] != 255), "{kind}: fixture sem alfa parcial"); }
        }
    }

    #[test]
    fn fifo_and_directory_never_block_icon_readers() {
        use crate::fsread::testutil::{make_fifo, tempdir, within_1s};
        let d = tempdir("icons");
        let png = make_fifo(&d, "i.png");
        let desktop = make_fifo(&d, "a.desktop");
        let dir = d.clone();
        assert!(within_1s(move || rasterize(&png, 16, None)).is_none());
        assert!(within_1s(move || read_text(&desktop)).is_none());
        assert!(within_1s(move || rasterize(&dir, 16, None)).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
