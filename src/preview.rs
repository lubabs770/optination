//! Catalog preview pictures, made safe for the shell to show.
//!
//! The overlay runs inside the desktop shell, so anything it decodes is
//! decoded in the shell's memory. A preview picture comes from whoever
//! uploaded the theme, so the overlay never loads one from the network.
//! The engine fetches it instead:
//!
//! * through the same curl transport as everything else (https only, a size
//!   cap, a timeout);
//! * decoded here by Rust decoders with a memory limit, after the header's
//!   dimensions are checked and before any pixel buffer is allocated;
//! * PNG text and ICC chunks are skipped, so compressed metadata is never
//!   inflated, and only the first frame of an animation is read;
//! * shrunk to at most `MAX_OUT_W` × `MAX_OUT_H` and written as a fresh,
//!   metadata-free PNG in the cache.
//!
//! The overlay then loads only that local file, which is small by construction.

use std::fs::{self, File};
use std::io::{BufWriter, Cursor};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::store;

/// Largest picture file the engine will download.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Largest picture, in pixels, the engine will decode (4096 × 4096).
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;
/// Largest side the engine will decode.
const MAX_SOURCE_SIDE: u32 = 8192;
/// Decoder allocation cap: one RGBA frame of the largest allowed picture,
/// plus room for the decoder's own buffers.
const MAX_DECODE_BYTES: u64 = MAX_SOURCE_PIXELS * 4 + 16 * 1024 * 1024;
/// What the overlay gets. The detail pane shows previews at about 400 px,
/// so this stays sharp on a 2x display.
pub const MAX_OUT_W: u32 = 800;
pub const MAX_OUT_H: u32 = 600;
/// Most pictures one call will fetch.
pub const MAX_URLS: usize = 80;
/// A cached preview not used for this long is deleted.
const KEEP_FOR: Duration = Duration::from_secs(14 * 24 * 3600);

pub struct Rgba {
    pub width: u32,
    pub height: u32,
    /// Straight (not premultiplied) alpha.
    pub pixels: Vec<u8>,
}

fn dir() -> PathBuf {
    // The leading dot keeps it apart from theme directories, whose cache
    // names never start with one.
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("optination/.store-previews")
}

fn cache_path(url: &str) -> PathBuf {
    use md5::{Digest, Md5};
    let hex: String = Md5::digest(url.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    dir().join(format!("{hex}.png"))
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the picture is empty".into());
    }
    if width > MAX_SOURCE_SIDE
        || height > MAX_SOURCE_SIDE
        || u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS
    {
        return Err(format!("the picture is {width}×{height}, larger than optination shows"));
    }
    Ok(())
}

// ----------------------------------------------------------------- decode ---

fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let limits = png::Limits { bytes: MAX_DECODE_BYTES as usize };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("png: {e}"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    check_size(width, height)?;
    let size = reader.output_buffer_size().ok_or("png: the picture is too large")?;
    let mut buf = vec![0; size];
    let out = reader.next_frame(&mut buf).map_err(|e| format!("png: {e}"))?;
    buf.truncate(out.buffer_size());
    let pixels = match out.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            buf.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect()
        }
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("png: palette was not expanded".into()),
    };
    // An APNG's first frame may be smaller than the canvas; use what was decoded.
    let (width, height) = (out.width, out.height);
    if pixels.len() != width as usize * height as usize * 4 {
        return Err("png: unexpected pixel data".into());
    }
    Ok(Rgba { width, height, pixels })
}

fn decode_jpeg(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(MAX_DECODE_BYTES as usize);
    decoder.read_info().map_err(|e| format!("jpeg: {e}"))?;
    let info = decoder.info().ok_or("jpeg: no header")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_size(width, height)?;
    let buf = decoder.decode().map_err(|e| format!("jpeg: {e}"))?;
    use jpeg_decoder::PixelFormat::*;
    let pixels: Vec<u8> = match info.pixel_format {
        RGB24 => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        L8 => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        // Big-endian 16-bit grey: keep the high byte.
        L16 => buf.as_chunks::<2>().0.iter().flat_map(|&[g, _]| [g, g, g, 255]).collect(),
        CMYK32 => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[c, m, y, k]| {
                let ch = |v: u8| (u32::from(v) * u32::from(k) / 255) as u8;
                [ch(c), ch(m), ch(y), 255]
            })
            .collect(),
    };
    if pixels.len() != width as usize * height as usize * 4 {
        return Err("jpeg: unexpected pixel data".into());
    }
    Ok(Rgba { width, height, pixels })
}

fn decode_gif(bytes: &[u8]) -> Result<Rgba, String> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(NonZeroU64::new(MAX_DECODE_BYTES).unwrap()));
    let mut decoder = options.read_info(Cursor::new(bytes)).map_err(|e| format!("gif: {e}"))?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    check_size(width, height)?;
    let frame = decoder.read_next_frame().map_err(|e| format!("gif: {e}"))?.ok_or("gif: no frames")?;
    // Place the first frame on a transparent canvas of the screen size,
    // cropping whatever part of it falls outside.
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
    if frame.buffer.len() != fw * fh * 4 {
        return Err("gif: unexpected pixel data".into());
    }
    let (left, top) = (usize::from(frame.left), usize::from(frame.top));
    for y in 0..fh {
        let cy = top + y;
        if cy >= height as usize {
            break;
        }
        let visible = fw.min((width as usize).saturating_sub(left));
        if visible == 0 {
            break;
        }
        let src = &frame.buffer[y * fw * 4..(y * fw + visible) * 4];
        let at = (cy * width as usize + left) * 4;
        pixels[at..at + visible * 4].copy_from_slice(src);
    }
    Ok(Rgba { width, height, pixels })
}

/// Decode a PNG, JPEG or GIF, told apart by their first bytes, not the URL.
pub fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes)
    } else {
        Err("the picture is not a PNG, JPEG or GIF".into())
    }
}

// ----------------------------------------------------------------- resize ---

/// Box-filter `img` down to fit inside `max_w` × `max_h`, keeping its shape.
/// Colour is averaged weighted by alpha, so transparent edges do not darken.
pub fn fit(img: Rgba, max_w: u32, max_h: u32) -> Rgba {
    let scale = f64::min(f64::from(max_w) / f64::from(img.width), f64::from(max_h) / f64::from(img.height));
    if scale >= 1.0 {
        return img;
    }
    let dw = ((f64::from(img.width) * scale).round() as u32).max(1);
    let dh = ((f64::from(img.height) * scale).round() as u32).max(1);
    let (sw, sh) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(dw as usize * dh as usize * 4);
    for oy in 0..dh as usize {
        let y0 = oy * sh / dh as usize;
        let y1 = ((oy + 1) * sh / dh as usize).max(y0 + 1);
        for ox in 0..dw as usize {
            let x0 = ox * sw / dw as usize;
            let x1 = ((ox + 1) * sw / dw as usize).max(x0 + 1);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for p in img.pixels[(y * sw + x0) * 4..(y * sw + x1) * 4].as_chunks::<4>().0 {
                    let pa = u64::from(p[3]);
                    r += u64::from(p[0]) * pa;
                    g += u64::from(p[1]) * pa;
                    b += u64::from(p[2]) * pa;
                    a += pa;
                    n += 1;
                }
            }
            let avg = |c: u64| c.checked_div(a).unwrap_or(0) as u8;
            out.extend_from_slice(&[avg(r), avg(g), avg(b), ((a + n / 2) / n) as u8]);
        }
    }
    Rgba { width: dw, height: dh, pixels: out }
}

// ------------------------------------------------------------------ cache ---

fn write_png(path: &Path, img: &Rgba) -> Result<(), String> {
    let parent = path.parent().ok_or("no cache directory")?;
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    // Write beside the target and rename, so the overlay never loads half a file.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), img.width, img.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&img.pixels).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Fetch one preview into the cache and return its path.
pub fn fetch(url: &str) -> Result<PathBuf, String> {
    if !store::allowed_preview(url) {
        return Err("not a catalog preview address".into());
    }
    let path = cache_path(url);
    if path.is_file() {
        // Touch it, so pruning keeps what is still being looked at.
        if let Ok(f) = File::options().append(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        return Ok(path);
    }
    let bytes = store::curl(url, MAX_FILE_BYTES, 30, None)?;
    let img = fit(decode(&bytes)?, MAX_OUT_W, MAX_OUT_H);
    write_png(&path, &img)?;
    Ok(path)
}

/// Delete previews nobody has looked at for a while.
fn prune() {
    let Ok(entries) = fs::read_dir(dir()) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Fetch several previews, a few at a time. Each result is the cached path
/// or why that one could not be shown.
pub fn fetch_all(urls: &[String]) -> Vec<Result<PathBuf, String>> {
    prune();
    // A handful of downloads at once is quick enough, and keeps the engine's
    // own peak memory to a few decode buffers.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build();
    let work = || urls.par_iter().take(MAX_URLS).map(|u| fetch(u)).collect();
    match pool {
        Ok(pool) => pool.install(work),
        Err(_) => urls.iter().take(MAX_URLS).map(|u| fetch(u)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32, extra: impl FnOnce(&mut png::Writer<&mut Vec<u8>>)) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            extra(&mut writer);
            writer.write_image_data(&vec![200; (width * height * 4) as usize]).unwrap();
        }
        out
    }

    #[test]
    fn decodes_a_small_png() {
        let img = decode(&png_bytes(3, 2, |_| {})).unwrap();
        assert_eq!((img.width, img.height, img.pixels.len()), (3, 2, 24));
    }

    #[test]
    fn refuses_huge_dimensions_before_decoding() {
        // A valid header claiming 20000×20000, with almost no data behind it:
        // the classic decompression bomb. It must fail on the header check.
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&20000u32.to_be_bytes());
        ihdr.extend_from_slice(&20000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let crc = crc32(&ihdr);
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(&ihdr);
        bytes.extend_from_slice(&crc.to_be_bytes());
        let err = decode(&bytes).err().unwrap();
        assert!(err.contains("larger than") || err.contains("png"), "{err}");
    }

    #[test]
    fn ignores_compressed_text_chunks() {
        // A zTXt chunk that would inflate to a lot of text is skipped, not read.
        let bytes = png_bytes(2, 2, |w| {
            w.write_text_chunk(&png::text_metadata::ZTXtChunk::new("Comment", "x".repeat(1 << 20))).unwrap();
        });
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn rejects_unknown_formats() {
        assert!(decode(b"<html>not a picture</html>").is_err());
        assert!(decode(b"RIFF\0\0\0\0WEBPVP8 ").is_err());
    }

    #[test]
    fn fit_shrinks_and_keeps_shape() {
        let img = Rgba { width: 1600, height: 900, pixels: vec![255; 1600 * 900 * 4] };
        let out = fit(img, MAX_OUT_W, MAX_OUT_H);
        assert_eq!((out.width, out.height), (800, 450));
        assert!(out.pixels.iter().all(|&b| b == 255));
        let small = fit(Rgba { width: 10, height: 10, pixels: vec![0; 400] }, 800, 600);
        assert_eq!(small.width, 10);
    }

    #[test]
    fn fetch_refuses_other_hosts() {
        assert!(fetch("https://evil.example/a.png").is_err());
        assert!(fetch("http://images.pling.com/a.png").is_err());
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
}
