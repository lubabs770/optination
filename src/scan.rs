//! Discovery of cursor themes on disk.
//!
//! Two on-disk formats coexist on a modern Hyprland box and the old shell
//! script only knew about one of them:
//!
//! * **Xcursor** — `<theme>/cursors/<shape>`, binary Xcursor files holding
//!   pre-rasterized ARGB images at a handful of nominal sizes.
//! * **hyprcursor** — a manifest plus `<theme>/hyprcursors/<shape>.hlc`, where
//!   each `.hlc` is a zip of `meta.hl` and one or more SVGs. hyprcursor accepts
//!   the manifest as either hyprlang (`manifest.hl`) or TOML (`manifest.toml`);
//!   both are in the wild, so both count.
//!
//! A theme may ship either, or both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Xcursor,
    Hyprcursor,
    Both,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Xcursor => "X11",
            Format::Hyprcursor => "hypr",
            Format::Both => "X11 + hypr",
        }
    }

    pub fn has_hypr(self) -> bool {
        matches!(self, Format::Hyprcursor | Format::Both)
    }

    pub fn has_x11(self) -> bool {
        matches!(self, Format::Xcursor | Format::Both)
    }
}

/// Whether a theme's name declares it as a light or dark variant. Plenty of
/// themes declare neither, and guessing from the pixels would be worse than
/// saying nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Light,
    Dark,
    Unstated,
}

impl Tone {
    pub fn label(self) -> &'static str {
        match self {
            Tone::Light => "light",
            Tone::Dark => "dark",
            Tone::Unstated => "",
        }
    }
}

/// Read the tone off the theme name. `-dark` and `-light`/`-white` are the two
/// conventions that actually appear; anything else stays unstated.
fn tone_of(name: &str) -> Tone {
    let name = name.to_lowercase();
    let has = |needle: &str| {
        name.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word == needle)
    };
    if has("dark") {
        Tone::Dark
    } else if has("light") || has("white") {
        Tone::Light
    } else {
        Tone::Unstated
    }
}

/// Mirrored (left-handed) variants. Two naming conventions in the wild:
/// Bibata's `-Right` suffix and Nordzy's `-lefthand`. Both mean the same thing —
/// the pointer tips the other way.
fn mirrored_of(name: &str) -> bool {
    let name = name.to_lowercase();
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| matches!(word, "right" | "lefthand" | "righthand"))
        || name.contains("left-hand")
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// Directory name — this is what `hyprctl setcursor` and `gsettings` want.
    pub name: String,
    pub path: PathBuf,
    pub format: Format,
    /// `Comment=` from index.theme, or `description =` from manifest.hl.
    pub comment: String,
    /// True when found under $HOME rather than a system directory.
    pub user_installed: bool,
    pub tone: Tone,
    /// A left-handed / mirrored variant.
    pub mirrored: bool,
}

/// Search path, in XDG icon lookup order. Earlier entries win on name collision
/// — a user-installed theme shadows a system one of the same name, which is how
/// the icon spec says it should resolve.
fn search_dirs() -> Vec<(PathBuf, bool)> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push((home.join(".local/share/icons"), true));
        dirs.push((home.join(".icons"), true));
    }
    for sys in ["/usr/share/icons", "/usr/local/share/icons"] {
        dirs.push((PathBuf::from(sys), false));
    }
    dirs
}

/// hyprcursor reads either `manifest.hl` (hyprlang) or `manifest.toml`.
/// Checking only for the former drops real themes — `sweet-cursors-hyprcursor`
/// ships TOML.
fn manifest(dir: &Path) -> Option<PathBuf> {
    ["manifest.hl", "manifest.toml"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
}

fn classify(dir: &Path) -> Option<Format> {
    let x11 = dir.join("cursors").is_dir();
    let hypr = dir.join("hyprcursors").is_dir() && manifest(dir).is_some();
    match (x11, hypr) {
        (true, true) => Some(Format::Both),
        (true, false) => Some(Format::Xcursor),
        (false, true) => Some(Format::Hyprcursor),
        (false, false) => None,
    }
}

/// Pull a `Key=Value` (index.theme) or `key = value` (manifest.hl) field.
fn read_field(path: &Path, keys: &[&str]) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let (k, v) = line.split_once('=')?;
        let k = k.trim();
        if keys.iter().any(|want| k.eq_ignore_ascii_case(want)) {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn describe(dir: &Path, format: Format) -> String {
    let from_index = read_field(&dir.join("index.theme"), &["Comment"]);
    let from_manifest = if format.has_hypr() {
        manifest(dir).and_then(|m| read_field(&m, &["description"]))
    } else {
        None
    };
    from_index.or(from_manifest).unwrap_or_default()
}

/// Every usable cursor theme on the system, sorted case-insensitively by name.
pub fn themes() -> Vec<Theme> {
    // BTreeMap keyed by lowercased name: dedupes, and since we insert in search
    // order with `or_insert`, the first (highest-priority) hit sticks.
    let mut found: BTreeMap<String, Theme> = BTreeMap::new();

    for (root, user_installed) in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `is_dir` follows symlinks, which is deliberate: some themes are
            // installed as links into /usr/share.
            if !path.is_dir() {
                continue;
            }
            let Some(format) = classify(&path) else {
                continue;
            };
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // `default` is a pointer-to-another-theme, not a theme; `hicolor`
            // and `locolor` are icon themes that happen to carry a cursors dir.
            if matches!(name, "default" | "hicolor" | "locolor") {
                continue;
            }
            found.entry(name.to_lowercase()).or_insert_with(|| Theme {
                name: name.to_string(),
                comment: describe(&path, format),
                path: path.clone(),
                format,
                user_installed,
                tone: tone_of(name),
                mirrored: mirrored_of(name),
            });
        }
    }

    found.into_values().collect()
}
