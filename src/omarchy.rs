//! Reading the accent colour out of the current Omarchy theme.
//!
//! Omarchy keeps the active theme as a symlink under
//! `~/.local/state/omarchy/current/theme`, with a `colors.toml` that names an
//! `accent` and a `mode`. "Follow Omarchy theme" maps that accent onto the
//! nearest of the six accents in the design handoff, which is as close as this
//! app can get without shipping a full tonal-palette generator.

use std::path::PathBuf;

/// The `primary` of each handoff accent, light mode, in the order the UI shows
/// them: Cyan, Purple, Green, Amber, Rose, Indigo. Kept in step with
/// `ui/skin.slint` by hand — there are six of them and they do not move.
const ACCENT_PRIMARIES: [(u8, u8, u8); 6] = [
    (0x00, 0x65, 0x8e),
    (0x67, 0x50, 0xa4),
    (0x2c, 0x6c, 0x47),
    (0x8f, 0x4c, 0x00),
    (0x9a, 0x40, 0x57),
    (0x3b, 0x5b, 0xbf),
];

fn colors_path() -> Option<PathBuf> {
    let path = dirs::home_dir()?.join(".local/state/omarchy/current/theme/colors.toml");
    path.is_file().then_some(path)
}

fn parse_hex(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value.trim().trim_matches('"').trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// Perceptually weighted RGB distance. Good enough to tell cyan from amber,
/// which is all this has to do.
fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
    3 * d(a.0, b.0) + 4 * d(a.1, b.1) + 2 * d(a.2, b.2)
}

/// `(accent index, dark mode)` from the active Omarchy theme.
pub fn accent() -> Option<(usize, bool)> {
    let text = std::fs::read_to_string(colors_path()?).ok()?;

    let mut rgb = None;
    let mut dark = true;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "accent" => rgb = parse_hex(value),
            "mode" => dark = !value.trim().trim_matches('"').eq_ignore_ascii_case("light"),
            _ => {}
        }
    }

    let rgb = rgb?;
    let index = ACCENT_PRIMARIES
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| distance(rgb, **candidate))
        .map(|(i, _)| i)?;
    Some((index, dark))
}
