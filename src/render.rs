//! Decoding cursor shapes to RGBA bitmaps for on-screen preview.
//!
//! Both formats hand back *premultiplied* RGBA; `cache` converts to straight
//! alpha when it writes PNGs — do not "fix" it here, or every
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
        self.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 0)
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
    for px in src.as_chunks::<4>().0 {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    out
}

/// Big animated themes (Bibata's `watch` at every size up to 256) run a few
/// MiB; anything past this is not a cursor.
const XCURSOR_FILE_LIMIT: u64 = 32 * 1024 * 1024;

/// Walk the TOC before handing the file to `xcursor`, which trusts it: it
/// reserves `ntoc` slots up front, allocates `4 × width × height` from each
/// image header before reading, and copies an image once per TOC entry that
/// points at it. Requiring the declared pixels of all entries to fit in the
/// file bounds every one of those to the file's own size.
fn xcursor_sane(bytes: &[u8]) -> bool {
    let u32_at = |at: usize| -> Option<u64> {
        Some(u64::from(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)))
    };
    let (Some(header), Some(ntoc)) = (u32_at(4), u32_at(12)) else {
        return false;
    };
    let len = bytes.len() as u64;
    if header + ntoc * 12 > len {
        return false;
    }
    let mut pixels: u64 = 0;
    for i in 0..ntoc {
        let toc = (header + i * 12) as usize;
        if u32_at(toc) != Some(0xfffd_0002) {
            continue;
        }
        let Some(pos) = u32_at(toc + 8) else { return false };
        let (Some(w), Some(h)) = (u32_at(pos as usize + 16), u32_at(pos as usize + 20)) else {
            return false;
        };
        pixels += 4 * w.min(0x8000) * h.min(0x8000);
        if pixels > len {
            return false;
        }
    }
    true
}

fn load_xcursor(theme_dir: &Path, names: &[&str], target: u32) -> Option<Bitmap> {
    let cursors = theme_dir.join("cursors");
    for name in names {
        let file = cursors.join(name);
        let Ok(mut f) = std::fs::File::open(&file) else {
            continue;
        };
        let bytes = capped_read(&mut f, XCURSOR_FILE_LIMIT)?;
        if !xcursor_sane(&bytes) {
            return None;
        }
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

const HLC_META_LIMIT: u64 = 64 * 1024;
const HLC_ASSET_LIMIT: u64 = 4 * 1024 * 1024;
/// A compressed PNG declares its own dimensions, and the output buffer is ours
/// to allocate, so `png::Limits` does not cover it. Cursor PNGs top out at 256
/// in practice; this leaves headroom for HiDPI themes.
const HLC_PNG_MAX_SIDE: u32 = 1024;

fn capped_read(r: &mut impl Read, limit: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(limit + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= limit).then_some(buf)
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
        let raw = capped_read(&mut meta, HLC_META_LIMIT)?;
        let text = String::from_utf8(raw).ok()?;
        let sizes = parse_sizes(&text);
        wanted = sizes
            .iter()
            .find(|e| e.size == 0)
            .or_else(|| sizes.iter().filter(|e| e.size >= target).min_by_key(|e| e.size))
            .or_else(|| sizes.iter().max_by_key(|e| e.size))
            .map(|e| e.file.clone());
    }

    // No parsable meta: fall back to whatever asset the archive holds.
    let name = match wanted {
        Some(n) => n,
        None => (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .find(|n| n.ends_with(".svg") || n.ends_with(".png"))?,
    };

    let mut entry = zip.by_name(&name).ok()?;
    let buf = capped_read(&mut entry, HLC_ASSET_LIMIT)?;
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
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some(Bitmap { width: w, height: h, rgba: pixmap.take(), premultiplied: true })
}

fn decode_png(data: &[u8]) -> Option<Bitmap> {
    let limits = png::Limits { bytes: 16 * 1024 * 1024 };
    let decoder = png::Decoder::new_with_limits(std::io::Cursor::new(data), limits);
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w > HLC_PNG_MAX_SIDE || h > HLC_PNG_MAX_SIDE {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());

    // Normalize to 8-bit RGBA; cursor PNGs are RGBA already, but grayscale and
    // palette variants exist and cost little to handle.
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
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
    if bitmap.as_ref().is_none_or(Bitmap::is_blank)
        && theme.format.has_x11()
        && let Some(fallback) = load_xcursor(&theme.path, slot.names, target)
    {
        bitmap = Some(fallback);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("optination-render-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_hlc(dir: &Path, name: &str, meta: &[u8], asset_name: &str, asset: &[u8]) {
        std::fs::create_dir_all(dir.join("hyprcursors")).unwrap();
        let path = dir.join("hyprcursors").join(format!("{name}.hlc"));
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("meta.hl", Default::default()).unwrap();
        std::io::Write::write_all(&mut zip, meta).unwrap();
        zip.start_file::<_, ()>(asset_name, Default::default()).unwrap();
        std::io::Write::write_all(&mut zip, asset).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn hlc_rejects_oversized_asset() {
        let dir = test_dir("big-asset");
        let big = vec![0u8; HLC_ASSET_LIMIT as usize + 1];
        write_hlc(&dir, "left_ptr", b"define_size = 0, bomb.png\n", "bomb.png", &big);
        assert!(hlc_asset(&dir.join("hyprcursors/left_ptr.hlc"), 24).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hlc_rejects_oversized_meta() {
        let dir = test_dir("big-meta");
        let big_meta = vec![b'#'; HLC_META_LIMIT as usize + 1];
        write_hlc(&dir, "left_ptr", &big_meta, "cursor.svg", b"<svg/>");
        assert!(hlc_asset(&dir.join("hyprcursors/left_ptr.hlc"), 24).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&vec![0; (w * h * 4) as usize]).unwrap();
        out
    }

    #[test]
    fn png_rejects_oversized_dimensions() {
        assert!(decode_png(&png_bytes(HLC_PNG_MAX_SIDE + 1, 1)).is_none());
        assert!(decode_png(&png_bytes(32, 32)).is_some());
    }

    /// An Xcursor file whose `ntoc` TOC entries all point at one `side`² image.
    fn xcursor_bytes(ntoc: u32, side: u32) -> Vec<u8> {
        let mut b = Vec::new();
        let header = 16u32;
        let image = header + ntoc * 12;
        for v in [u32::from_le_bytes(*b"Xcur"), header, 0x1_0000, ntoc] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..ntoc {
            for v in [0xfffd_0002, side, image] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        for v in [36, 0xfffd_0002, side, 1, side, side, 0, 0, 0] {
            b.extend_from_slice(&u32::to_le_bytes(v));
        }
        b.extend(std::iter::repeat_n(0u8, (side * side * 4) as usize));
        b
    }

    #[test]
    fn xcursor_rejects_repeated_toc_entries() {
        let ok = xcursor_bytes(1, 24);
        assert!(xcursor_sane(&ok));
        assert_eq!(xcursor::parser::parse_xcursor(&ok).map(|v| v.len()), Some(1));
        assert!(!xcursor_sane(&xcursor_bytes(1000, 24)));
    }

    #[test]
    fn xcursor_rejects_inflated_header() {
        let mut b = xcursor_bytes(1, 24);
        let at = 16 + 12 + 16; // image width
        b[at..at + 8].copy_from_slice(&[0xff, 0x7f, 0, 0, 0xff, 0x7f, 0, 0]);
        assert!(!xcursor_sane(&b));
    }

    #[test]
    fn hlc_accepts_normal_asset() {
        let dir = test_dir("normal");
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"/>";
        write_hlc(&dir, "left_ptr", b"define_size = 0, cursor.svg\n", "cursor.svg", svg);
        let asset = hlc_asset(&dir.join("hyprcursors/left_ptr.hlc"), 24);
        assert!(matches!(asset, Some(Asset::Svg(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
