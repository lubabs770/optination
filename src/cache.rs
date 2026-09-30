//! Rendered shapes on disk, for the QML overlay to load as plain images.
//!
//! The overlay cannot hold decoded pixels the way the old Slint UI did, so every
//! shape it shows is a PNG under `$XDG_CACHE_HOME/optination/`. A PNG is reused
//! while it is newer than its theme directory; reinstalling or editing a theme
//! bumps the directory's mtime and the next request re-renders it.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;

use crate::render::{self, Bitmap};
use crate::scan::Theme;

/// Edge of a list-row thumbnail. The row shows it at about half this, so it
/// stays sharp on a 2x display.
pub const THUMB_PX: u32 = 64;

fn root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("optination")
}

/// Theme names are directory names, so they are already path-safe in practice;
/// this only guards against a name that would climb out of the cache.
fn component(name: &str) -> String {
    name.replace(['/', '\\'], "_").trim_start_matches('.').to_string()
}

/// The preview draws the pointer larger than the other shapes, per the design.
pub fn preview_target(slot: usize, size: u32) -> u32 {
    let scale = if slot == 0 { 1.9 } else { 1.25 };
    (f64::from(size) * scale).round().max(1.0) as u32
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn fresh(png: &Path, theme_dir: &Path) -> bool {
    match (mtime(png), mtime(theme_dir)) {
        (Some(png), Some(theme)) => png >= theme,
        _ => false,
    }
}

/// PNG wants straight alpha; Xcursor and tiny-skia hand back premultiplied.
fn straight_alpha(bitmap: &Bitmap) -> Vec<u8> {
    if !bitmap.premultiplied {
        return bitmap.rgba.clone();
    }
    bitmap
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            if a == 0 {
                return [0, 0, 0, 0];
            }
            let un = |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            [un(r), un(g), un(b), a]
        })
        .collect()
}

fn write_png(path: &Path, bitmap: &Bitmap) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write beside the target and rename, so the overlay never loads half a file.
    let tmp = path.with_extension("png.tmp");
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(&tmp)?), bitmap.width, bitmap.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(&straight_alpha(bitmap)))
        .map_err(std::io::Error::other)?;
    std::fs::rename(tmp, path)
}

/// Marker for a shape the theme does not have, so a miss is cached too.
fn missing_marker(png: &Path) -> PathBuf {
    png.with_extension("missing")
}

/// One cached shape: its PNG path, or `None` when the theme lacks it.
fn cached(theme: &Theme, png: PathBuf, draw: impl FnOnce() -> Option<Bitmap>) -> Option<PathBuf> {
    if fresh(&png, &theme.path) {
        return Some(png);
    }
    let marker = missing_marker(&png);
    if fresh(&marker, &theme.path) {
        return None;
    }
    match draw() {
        Some(bitmap) => write_png(&png, &bitmap).ok().map(|()| png),
        None => {
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = File::create(&marker);
            None
        }
    }
}

/// Every preview slot of one theme at one cursor size, in `render::SLOTS` order.
pub fn preview(theme: &Theme, size: u32) -> Vec<Option<PathBuf>> {
    let dir = root().join(component(&theme.name)).join(size.to_string());
    render::SLOTS
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let target = preview_target(i, size);
            cached(theme, dir.join(format!("{}.png", slot.label)), || {
                render::shape(theme, slot, target)
            })
        })
        .collect()
}

/// The pointer thumbnail for every theme, rendered in parallel.
pub fn thumbs(themes: &[Theme]) -> Vec<Option<PathBuf>> {
    themes
        .par_iter()
        .map(|theme| {
            let png = root().join(component(&theme.name)).join("thumb.png");
            cached(theme, png, || render::shape(theme, &render::SLOTS[0], THUMB_PX))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_cannot_escape_the_cache() {
        assert_eq!(component("../../etc"), "_.._etc");
        assert_eq!(component("Bibata-Modern-Ice"), "Bibata-Modern-Ice");
    }

    #[test]
    fn pointer_is_drawn_larger_than_the_rest() {
        assert_eq!(preview_target(0, 32), 61);
        assert_eq!(preview_target(3, 32), 40);
    }

    #[test]
    fn straight_alpha_undoes_premultiplication() {
        let bitmap = Bitmap { width: 1, height: 1, rgba: vec![64, 0, 0, 128], premultiplied: true };
        assert_eq!(straight_alpha(&bitmap), vec![128, 0, 0, 128]);
    }

    #[test]
    fn stale_png_is_not_fresh() {
        let dir = std::env::temp_dir().join(format!("optination-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let theme = dir.join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        assert!(!fresh(&png, &theme), "theme changed after the png was written");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&png, b"x").unwrap();
        assert!(fresh(&png, &theme));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
