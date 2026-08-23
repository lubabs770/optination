//! Decoding cursor shapes to RGBA bitmaps for on-screen preview.
//!
//! Both formats hand back *premultiplied* RGBA, which is what Slint's
//! `from_rgba8_premultiplied` wants — do not "fix" this to plain RGBA or every
//! anti-aliased edge picks up a dark halo.

use std::io::Read;
use std::path::Path;

use crate::scan::{Format, Theme};

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Xcursor and tiny-skia produce premultiplied alpha; PNG does not.
    pub premultiplied: bool,
}

impl Bitmap {
    /// Fully transparent output. It happens — a shape file that decodes cleanly
    /// but draws nothing, usually a placeholder the theme never filled in. It
    /// has to be treated as absent, or the UI shows an unexplained hole.
    pub fn is_blank(&self) -> bool {
        self.rgba.chunks_exact(4).all(|px| px[3] == 0)
    }
}

/// One preview slot: a human label plus the shape names to try, in order.
/// Themes disagree about naming (X11 legacy vs. CSS), so each slot lists both.
pub struct Slot {
    pub label: &'static str,
    pub names: &'static [&'static str],
}

pub const SLOTS: &[Slot] = &[
    Slot { label: "pointer", names: &["left_ptr", "default", "arrow", "top_left_arrow"] },
    Slot { label: "text", names: &["xterm", "text", "ibeam"] },
    Slot { label: "link", names: &["hand2", "pointer", "pointing_hand", "hand1", "hand"] },
    Slot { label: "resize", names: &["sb_h_double_arrow", "ew-resize", "size_hor", "h_double_arrow"] },
    Slot { label: "busy", names: &["watch", "wait", "left_ptr_watch", "progress"] },
    Slot { label: "no-drop", names: &["crossed_circle", "not-allowed", "no-drop", "forbidden"] },
];

// ---------------------------------------------------------------- Xcursor ---

/// Xcursor stores each pixel as a little-endian ARGB u32, i.e. BGRA in byte
/// order. The `xcursor` crate's `pixels_rgba` is the raw file order despite the
/// name, and its `pixels_argb` rotates on the assumption the input was RGBA —
/// so neither field is actually RGBA. Swap R and B ourselves.
fn bgra_to_rgba(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    out
}

fn load_xcursor(theme_dir: &Path, names: &[&str], target: u32) -> Option<Bitmap> {
    let cursors = theme_dir.join("cursors");
    for name in names {
        let file = cursors.join(name);
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let images = xcursor::parser::parse_xcursor(&bytes)?;
        // Prefer the smallest nominal size that is >= target so we scale down,
        // never up; fall back to the largest available.
        let best = images
            .iter()
            .filter(|i| i.size >= target)
            .min_by_key(|i| i.size)
            .or_else(|| images.iter().max_by_key(|i| i.size))?;
        return Some(Bitmap {
            width: best.width,
            height: best.height,
            rgba: bgra_to_rgba(&best.pixels_rgba),
            premultiplied: true,
        });
    }
    None
}

// ------------------------------------------------------------ hyprcursor ---

/// An asset inside a `.hlc`. hyprcursor themes are not all vector: roughly a
/// third of the ones in the wild ship pre-rendered PNGs at fixed sizes.
enum Asset {
    Svg(Vec<u8>),
    Png(Vec<u8>),
}

/// One `define_size = <size>, <file>[, <delay>]` line.
struct SizeEntry {
    size: u32,
    file: String,
}

fn parse_sizes(meta: &str) -> Vec<SizeEntry> {
    let mut out = Vec::new();
    for line in meta.lines() {
        let Some(rest) = line.trim().strip_prefix("define_size") else {
            continue;
        };
        let Some(rest) = rest.trim().strip_prefix('=') else {
            continue;
        };
        let mut parts = rest.split(',').map(str::trim);
        let Some(size) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(file) = parts.next() else { continue };
        out.push(SizeEntry { size, file: file.to_string() });
    }
    out
}

/// Choose the best asset for a shape out of its `.hlc` (a zip of `meta.hl` plus
/// assets).
///
/// `define_size = 0` means scalable, which always wins. Otherwise take the
/// smallest fixed size at or above the target so the preview scales down rather
/// than up, falling back to the largest available.
fn hlc_asset(hlc: &Path, target: u32) -> Option<Asset> {
    let file = std::fs::File::open(hlc).ok()?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;

    let mut wanted: Option<String> = None;
    if let Ok(mut meta) = zip.by_name("meta.hl") {
        let mut text = String::new();
        if meta.read_to_string(&mut text).is_ok() {
            let sizes = parse_sizes(&text);
            wanted = sizes
                .iter()
                .find(|e| e.size == 0)
                .or_else(|| sizes.iter().filter(|e| e.size >= target).min_by_key(|e| e.size))
                .or_else(|| sizes.iter().max_by_key(|e| e.size))
                .map(|e| e.file.clone());
        }
    }

    // No parsable meta: fall back to whatever asset the archive holds.
    let name = match wanted {
        Some(n) => n,
        None => (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .find(|n| n.ends_with(".svg") || n.ends_with(".png"))?,
    };

    let mut entry = zip.by_name(&name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    match () {
        _ if name.ends_with(".svg") => Some(Asset::Svg(buf)),
        _ if name.ends_with(".png") => Some(Asset::Png(buf)),
        _ => None,
    }
}

fn render_svg(svg: &[u8], target: u32) -> Option<Bitmap> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = target as f32 / size.width().max(size.height());
    let w = (size.width() * scale).ceil().max(1.0) as u32;
    let h = (size.height() * scale).ceil().max(1.0) as u32;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(Bitmap { width: w, height: h, rgba: pixmap.take(), premultiplied: true })
}

fn decode_png(data: &[u8]) -> Option<Bitmap> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());

    // Normalize to 8-bit RGBA; cursor PNGs are RGBA already, but grayscale and
    // palette variants exist and cost little to handle.
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            buf.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        _ => return None,
    };

    Some(Bitmap { width: info.width, height: info.height, rgba, premultiplied: false })
}

fn load_hyprcursor(theme_dir: &Path, names: &[&str], target: u32) -> Option<Bitmap> {
    let dir = theme_dir.join("hyprcursors");
    let asset = names
        .iter()
        .map(|n| dir.join(format!("{n}.hlc")))
        .find(|p| p.is_file())
        .and_then(|p| hlc_asset(&p, target))?;

    match asset {
        Asset::Svg(data) => render_svg(&data, target),
        Asset::Png(data) => decode_png(&data),
    }
}

// ---------------------------------------------------------------- public ---

/// Render one preview slot for a theme, at roughly `target` pixels.
///
/// hyprcursor is tried first for themes that ship both: it is vector, so it is
/// the one that stays sharp at any size, and it is what Hyprland itself will
/// actually use.
pub fn shape(theme: &Theme, slot: &Slot, target: u32) -> Option<Bitmap> {
    let mut bitmap = None;
    if theme.format.has_hypr() {
        bitmap = load_hyprcursor(&theme.path, slot.names, target);
    }
    if bitmap.as_ref().is_none_or(Bitmap::is_blank) && theme.format.has_x11() {
        if let Some(fallback) = load_xcursor(&theme.path, slot.names, target) {
            bitmap = Some(fallback);
        }
    }
    bitmap.filter(|b| !b.is_blank())
}

/// Every slot for a theme. Missing shapes come back as `None` so the UI can
/// leave a gap rather than silently shifting the others along.
pub fn preview(theme: &Theme, target: u32) -> Vec<Option<Bitmap>> {
    SLOTS.iter().map(|slot| shape(theme, slot, target)).collect()
}

/// Count of distinct shapes a theme defines — a rough completeness signal, and
/// the thing that tells you a theme will fall back to Adwaita half the time.
pub fn shape_count(theme: &Theme) -> usize {
    let dir = match theme.format {
        Format::Hyprcursor => theme.path.join("hyprcursors"),
        _ => theme.path.join("cursors"),
    };
    std::fs::read_dir(dir).map(|d| d.flatten().count()).unwrap_or(0)
}
