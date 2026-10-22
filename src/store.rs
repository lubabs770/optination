//! Browsing and installing cursor themes from the OpenDesktop / pling catalog.
//!
//! The catalog is the one gnome-look.org and the KDE "Get New Cursors" dialog
//! read: the OCS API, category 107 ("Cursors"), about 1,200 themes with preview
//! pictures and direct download links. Network access goes through `curl` and
//! extraction through `bsdtar`, both run as argument vectors.
//!
//! Installing is the part that touches untrusted data, so it is strict:
//!
//! * https only, redirects included, with a size cap and a timeout;
//! * the download's md5 is checked when the catalog publishes one;
//! * the archive is listed before it is unpacked, and refused if it is huge
//!   or holds an absurd number of entries;
//! * it is unpacked into an empty staging directory, with bsdtar's default
//!   refusal of `..` and absolute paths left on;
//! * only directories that look like a cursor theme are kept, and only if
//!   every entry inside is a plain file, directory, or a symlink that stays
//!   inside the theme;
//! * the theme name must pass the same check `--save` applies, so whatever is
//!   installed can also be saved.
//!
//! Preview pictures are untrusted too; `preview` fetches and re-encodes them
//! so the overlay never decodes one straight off the network.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;

use crate::apply::safe_name;
use crate::scan;

const API: &str = "https://api.opendesktop.org/ocs/v1/content/data";
const CATEGORY: &str = "107";
pub const PAGE_SIZE: u32 = 24;
const MAX_API_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 600 * 1024 * 1024;
const MAX_ENTRIES: usize = 40_000;
/// Marks a theme directory as installed from the catalog, and from which item.
pub const SOURCE_FILE: &str = ".optination-source.json";

const ARCHIVES: &[&str] = &[
    ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tar.zst", ".tar", ".zip", ".7z",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub items: Vec<Item>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u64,
    pub name: String,
    pub author: String,
    pub summary: String,
    pub downloads: u64,
    /// 0–100, as the site shows it.
    pub score: u64,
    /// YYYY-MM-DD of the last change.
    pub changed: String,
    pub previews: Vec<String>,
    pub page: String,
    pub files: Vec<FileEntry>,
    /// Names of installed themes that came from this item.
    pub installed: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// The catalog's 1-based download slot.
    pub n: u32,
    pub name: String,
    pub size_kb: u64,
    /// An archive that could hold a Linux cursor theme.
    pub installable: bool,
    /// The one `--install ID` takes when no file is named.
    pub preferred: bool,
    /// The installed theme this file became, or "" when it is not installed.
    pub installed_as: String,
}

#[derive(Serialize, Debug)]
pub struct Installed {
    pub installed: Vec<String>,
}

// ------------------------------------------------------------------ fetch ---

pub(crate) fn curl(url: &str, max_bytes: u64, timeout_s: u32, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    // `-q` must come first: it stops ~/.curlrc from changing any of the below.
    cmd.args(["-q", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5"])
        .args(["--max-filesize", &max_bytes.to_string()])
        .args(["--max-time", &timeout_s.to_string()])
        .args(["--connect-timeout", "10"])
        .args(["-A", concat!("optination/", env!("CARGO_PKG_VERSION"))])
        .stdin(Stdio::null());
    if let Some(path) = out {
        cmd.arg("-o").arg(path);
    }
    cmd.arg("--").arg(url);
    let output = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "browsing needs curl".to_string(),
        _ => format!("curl: {e}"),
    })?;
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(6 | 7) => "could not reach the catalog — are you online?".into(),
            Some(28) => "the catalog timed out".into(),
            Some(63) => "the download is larger than optination allows".into(),
            _ => {
                let msg = String::from_utf8_lossy(&output.stderr);
                format!("download failed: {}", msg.trim().lines().next().unwrap_or("curl error"))
            }
        });
    }
    if output.stdout.len() as u64 > max_bytes {
        return Err("the catalog answered with more data than expected".into());
    }
    Ok(output.stdout)
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The OCS API emits invalid JSON escapes (`\'`) in some descriptions. Drop
/// the backslash of any escape JSON does not define, inside strings only.
fn repair_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        match (in_string, c) {
            (false, '"') => {
                in_string = true;
                out.push(c);
            }
            (false, _) => out.push(c),
            (true, '"') => {
                in_string = false;
                out.push(c);
            }
            (true, '\\') => match chars.next() {
                Some(e @ ('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')) => {
                    out.push('\\');
                    out.push(e);
                }
                Some(other) => out.push(other),
                None => {}
            },
            (true, _) => out.push(c),
        }
    }
    out
}

fn api(url: &str) -> Result<Value, String> {
    let body = curl(url, MAX_API_BYTES, 25, None)?;
    let text = String::from_utf8_lossy(&body);
    let value: Value = serde_json::from_str(&repair_json(&text))
        .map_err(|_| "the catalog sent something optination could not read".to_string())?;
    if value["statuscode"].as_u64() != Some(100) {
        let msg = value["message"].as_str().unwrap_or("");
        return Err(format!("the catalog refused the request{}", if msg.is_empty() { String::new() } else { format!(": {msg}") }));
    }
    Ok(value)
}

// ------------------------------------------------------------------ parse ---

fn num(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0)
}

fn text(v: &Value, max: usize) -> String {
    let s = v.as_str().unwrap_or("").trim();
    let s: String = s.chars().filter(|c| !c.is_control()).take(max).collect();
    s
}

/// Preview pictures are only ever taken from pling's image host.
pub(crate) fn allowed_preview(s: &str) -> bool {
    s.starts_with("https://images.pling.com/")
        && s.len() < 512
        && s.bytes().all(|b| b.is_ascii_graphic())
        && !s.contains("..")
}

fn preview_url(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    allowed_preview(s).then(|| s.to_string())
}

pub fn installable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ARCHIVES.iter().any(|ext| lower.ends_with(ext))
        && !lower.contains("windows")
        && !lower.contains(".cape")
        && !lower.contains("png")
        && !lower.contains("win10")
        && !lower.contains("win11")
        && !lower.ends_with(".7z")
}

fn files(raw: &Value) -> Vec<FileEntry> {
    let mut out = Vec::new();
    for n in 1..=64u32 {
        let Some(name) = raw[format!("downloadname{n}")].as_str() else {
            break;
        };
        if raw[format!("downloadlink{n}")].as_str().unwrap_or("").is_empty() {
            continue;
        }
        let tags = raw[format!("downloadtags{n}")].as_str().unwrap_or("");
        let name = text(&Value::from(name), 160);
        out.push(FileEntry {
            n,
            installable: installable_name(&name) && !tags.contains("text/html"),
            size_kb: num(&raw[format!("downloadsize{n}")]),
            name,
            preferred: false,
            installed_as: String::new(),
        });
    }
    let preferred = default_file(&out).map(|f| f.n);
    for f in &mut out {
        f.preferred = Some(f.n) == preferred;
    }
    out
}

/// The download to take when none is named. Tarballs are the Linux
/// convention (a zip is more often a Windows or macOS bundle), and the
/// shortest name is usually the plain variant: `Nordzy-cursors.tar.gz`
/// rather than `Nordzy-cursors-lefthand.tar.gz`.
pub fn default_file(files: &[FileEntry]) -> Option<&FileEntry> {
    files
        .iter()
        .filter(|f| f.installable)
        .min_by_key(|f| (f.name.to_ascii_lowercase().ends_with(".zip"), f.name.len(), f.n))
}

/// Strip the archive extension and any `01-` ordering prefix, lowercased:
/// `02-Layan-cursors.tar.xz` → `layan-cursors`.
fn stem(file: &str) -> String {
    let lower = file.to_ascii_lowercase();
    let mut s = lower.as_str();
    for ext in ARCHIVES {
        if let Some(rest) = s.strip_suffix(ext) {
            s = rest;
            break;
        }
    }
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit());
    let s = s.strip_prefix('-').unwrap_or(s);
    s.to_string()
}

fn item(raw: &Value, themes: &[(String, Option<Source>)]) -> Option<Item> {
    let id = num(&raw["id"]);
    if id == 0 {
        return None;
    }
    let mut files = files(raw);
    // A file is installed when optination recorded installing it, or when a
    // theme with the archive's name is already on disk.
    for f in files.iter_mut().filter(|f| f.installable) {
        let stem = stem(&f.name);
        if let Some((name, _)) = themes.iter().find(|(name, source)| {
            source.as_ref().is_some_and(|s| s.id == id && s.file == f.name) || name.to_ascii_lowercase() == stem
        }) {
            f.installed_as = name.clone();
        }
    }
    let stems: BTreeSet<String> = files.iter().filter(|f| f.installable).map(|f| stem(&f.name)).collect();
    let installed = themes
        .iter()
        .filter(|(name, source)| {
            source.as_ref().is_some_and(|s| s.id == id) || stems.contains(&name.to_ascii_lowercase())
        })
        .map(|(name, _)| name.clone())
        .collect();
    let previews = ["previewpic1", "previewpic2", "previewpic3"]
        .iter()
        .filter_map(|k| preview_url(&raw[*k]))
        .collect();
    Some(Item {
        id,
        name: text(&raw["name"], 120),
        author: text(&raw["personid"], 60),
        summary: text(&raw["summary"], 300),
        downloads: num(&raw["downloads"]),
        score: num(&raw["score"]).min(100),
        changed: text(&raw["changed"], 10),
        previews,
        page: format!("https://www.pling.com/p/{id}"),
        files,
        installed,
    })
}

/// Installed themes with the catalog item each came from, if known.
fn installed_sources() -> Vec<(String, Option<Source>)> {
    scan::themes().into_iter().map(|t| (t.name.clone(), source_of(&t.path))).collect()
}

/// Where optination installed a theme from.
pub struct Source {
    pub id: u64,
    pub file: String,
}

pub fn source_of(theme_dir: &Path) -> Option<Source> {
    let text = fs::read_to_string(theme_dir.join(SOURCE_FILE)).ok()?;
    if text.len() > 4096 {
        return None;
    }
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Source { id: v["id"].as_u64()?, file: v["file"].as_str().unwrap_or("").to_string() })
}

pub fn sort_mode(sort: &str) -> Option<&'static str> {
    match sort {
        "downloads" | "down" => Some("down"),
        "rating" | "high" => Some("high"),
        "new" | "newest" => Some("new"),
        "name" | "alpha" => Some("alpha"),
        _ => None,
    }
}

pub fn search(query: &str, page: u32, sort: &str) -> Result<Page, String> {
    let mode = sort_mode(sort).ok_or_else(|| format!("unknown sort {sort:?} (downloads, rating, new, name)"))?;
    let mut url = format!(
        "{API}?format=json&categories={CATEGORY}&pagesize={PAGE_SIZE}&page={page}&sortmode={mode}"
    );
    let query = query.trim();
    if !query.is_empty() {
        url.push_str("&search=");
        url.push_str(&encode(&query.chars().take(100).collect::<String>()));
    }
    let value = api(&url)?;
    let themes = installed_sources();
    let items = value["data"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| item(r, &themes)).collect())
        .unwrap_or_default();
    Ok(Page { total: num(&value["totalitems"]), page, page_size: PAGE_SIZE, items })
}

// ---------------------------------------------------------------- install ---

fn icons_dir() -> Result<PathBuf, String> {
    dirs::data_dir().map(|d| d.join("icons")).ok_or_else(|| "no home directory".to_string())
}

fn work_dir(id: u64, n: u32) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("optination/downloads")
        .join(format!("{id}-{n}-{}", std::process::id()))
}

fn md5_hex(path: &Path) -> Result<String, String> {
    use md5::{Digest, Md5};
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Md5::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Count entries and total size from `bsdtar -tv` before unpacking anything.
fn check_listing(archive: &Path) -> Result<(), String> {
    let out = Command::new("bsdtar")
        .arg("-tvf")
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "installing needs bsdtar (pacman -S libarchive)".to_string(),
            _ => format!("bsdtar: {e}"),
        })?;
    if !out.status.success() {
        return Err("the download is not an archive optination can open".into());
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut entries = 0usize;
    let mut total = 0u64;
    for line in listing.lines() {
        entries += 1;
        // ls-style: mode links owner group size month day time name
        total += line.split_whitespace().nth(4).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    }
    if entries > MAX_ENTRIES {
        return Err(format!("the archive holds {entries} entries — too many to be a cursor theme"));
    }
    if total > MAX_UNPACKED_BYTES {
        return Err("the archive unpacks to more than optination allows".into());
    }
    Ok(())
}

fn extract(archive: &Path, into: &Path) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let status = Command::new("bsdtar")
        .args(["-x", "--no-same-owner", "--no-same-permissions", "--no-acls", "--no-xattrs", "--no-fflags"])
        .arg("-f")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("bsdtar: {e}"))?;
    if !status.success() {
        return Err("the archive could not be unpacked".into());
    }
    Ok(())
}

/// Resolve `target` (a symlink found in `link_dir`) lexically, and say whether
/// it stays inside `root`. Absolute targets never do.
fn link_stays_inside(root: &Path, link_dir: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let Ok(rel) = link_dir.strip_prefix(root) else {
        return false;
    };
    let mut depth: Vec<Component> = rel.components().collect();
    for part in target.components() {
        match part {
            Component::Normal(_) => depth.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Every entry under a theme is a file, a directory, or an inward symlink.
fn validate_tree(root: &Path) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            seen += 1;
            if seen > MAX_ENTRIES {
                return Err("too many files in the theme".into());
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                let target = fs::read_link(&path).map_err(|e| e.to_string())?;
                if !link_stays_inside(root, &dir, &target) {
                    return Err(format!(
                        "{} links outside the theme",
                        path.strip_prefix(root).unwrap_or(&path).display()
                    ));
                }
            } else if kind.is_dir() {
                stack.push(path);
            } else if !kind.is_file() {
                return Err("the theme contains a special file".into());
            }
        }
    }
    Ok(())
}

/// Directories under `staging` that are cursor themes, outermost first.
fn find_themes(staging: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(staging.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            if scan::classify(&path).is_some() {
                found.push(path);
            } else if depth < 4 {
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &dest)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn move_tree(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Cache and data dirs on different filesystems.
    copy_tree(from, to).map_err(|e| {
        let _ = fs::remove_dir_all(to);
        format!("{}: {e}", to.display())
    })
}

/// Directories 0755, files 0644: nothing a theme ships is left executable.
fn make_readable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o644));
            }
        }
    }
}

/// Download catalog item `id`, file `n` (or the first installable one), and
/// install every cursor theme inside it into `~/.local/share/icons`.
pub fn install(id: u64, n: Option<u32>) -> Result<Installed, String> {
    // Download links are signed and expire, so always ask for a fresh one.
    let value = api(&format!("{API}/{id}?format=json")).map_err(|e| {
        if e.starts_with("the catalog refused") { format!("no catalog item {id}") } else { e }
    })?;
    let raw = value["data"]
        .as_array()
        .and_then(|rows| rows.first())
        .ok_or_else(|| format!("no catalog item {id}"))?;
    if num(&raw["typeid"]) != 107 {
        return Err(format!("catalog item {id} is not a cursor theme"));
    }
    let entries = files(raw);
    let file = match n {
        Some(n) => entries.iter().find(|f| f.n == n).ok_or_else(|| format!("item {id} has no file {n}"))?,
        None => default_file(&entries).ok_or_else(|| "this item has no Linux cursor archive to install".to_string())?,
    };
    if !file.installable {
        return Err(format!("{} is not a Linux cursor archive", file.name));
    }
    let link = raw[format!("downloadlink{}", file.n)].as_str().unwrap_or("");
    if !link.starts_with("https://") {
        return Err("the catalog gave no https download link".into());
    }
    let md5 = raw[format!("downloadmd5sum{}", file.n)].as_str().unwrap_or("").to_ascii_lowercase();

    let work = work_dir(id, file.n);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let result = (|| {
        let ext = ARCHIVES
            .iter()
            .find(|e| file.name.to_ascii_lowercase().ends_with(*e))
            .copied()
            .unwrap_or(".archive");
        let archive = work.join(format!("download{ext}"));
        curl(link, MAX_DOWNLOAD_BYTES, 300, Some(&archive))?;
        if md5.len() == 32 && md5_hex(&archive)? != md5 {
            return Err("the download does not match the catalog's checksum".to_string());
        }
        check_listing(&archive)?;
        let staging = work.join("staging");
        extract(&archive, &staging)?;
        place(&staging, id, &file.name)
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

/// Validate and move every theme found under `staging` into the icons dir.
fn place(staging: &Path, id: u64, file_name: &str) -> Result<Installed, String> {
    let themes = find_themes(staging);
    if themes.is_empty() {
        return Err(format!("{file_name} has no Linux cursor theme in it"));
    }
    let icons = icons_dir()?;
    fs::create_dir_all(&icons).map_err(|e| format!("{}: {e}", icons.display()))?;

    // Check everything before moving anything, so a bad theme in a bundle
    // does not leave half of it installed.
    let mut plan = Vec::new();
    for dir in &themes {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if !safe_name(&name) || matches!(name.as_str(), "default" | "hicolor" | "locolor") {
            return Err(format!("{name:?} is not a theme name optination can install"));
        }
        validate_tree(dir)?;
        let dest = icons.join(&name);
        if dest.symlink_metadata().is_ok() && source_of(&dest).is_none() {
            return Err(format!(
                "{name} is already installed in {} — remove it there first",
                icons.display()
            ));
        }
        plan.push((dir.clone(), dest, name));
    }

    let mut installed = Vec::new();
    for (dir, dest, name) in plan {
        make_readable(&dir);
        let record = serde_json::json!({ "id": id, "file": file_name });
        fs::write(dir.join(SOURCE_FILE), record.to_string()).map_err(|e| e.to_string())?;
        // Reinstalling something we installed: swap it out.
        if dest.symlink_metadata().is_ok() {
            fs::remove_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        move_tree(&dir, &dest)?;
        installed.push(name);
    }
    Ok(Installed { installed })
}

/// Delete a user-installed theme. System themes belong to the package manager.
pub fn remove(name: &str, saved: Option<&str>) -> Result<PathBuf, String> {
    let theme = scan::themes()
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("no cursor theme named {name:?}"))?;
    if !theme.user_installed {
        return Err(format!("{name} is a system theme — remove it with your package manager"));
    }
    if saved == Some(theme.name.as_str()) {
        return Err(format!("{name} is your saved cursor — save another one first"));
    }
    let home = dirs::home_dir().ok_or("no home directory")?;
    let allowed = [icons_dir()?, home.join(".icons")];
    let parent = theme.path.parent().unwrap_or(Path::new("/"));
    if !allowed.iter().any(|a| a == parent) || !safe_name(&theme.name) {
        return Err(format!("refusing to remove {}", theme.path.display()));
    }
    let meta = theme.path.symlink_metadata().map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        fs::remove_file(&theme.path)
    } else {
        fs::remove_dir_all(&theme.path)
    }
    .map_err(|e| format!("{}: {e}", theme.path.display()))?;
    Ok(theme.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_json_drops_invalid_escapes_inside_strings_only() {
        let bad = r#"{"a":"it\'s \"ok\" A\\n","b":1}"#;
        let fixed = repair_json(bad);
        let v: Value = serde_json::from_str(&fixed).unwrap();
        assert_eq!(v["a"], "it's \"ok\" A\\n");
        assert_eq!(v["b"], 1);
    }

    #[test]
    fn stems_match_installed_theme_names() {
        assert_eq!(stem("02-Layan-cursors.tar.xz"), "layan-cursors");
        assert_eq!(stem("Bibata-Modern-Ice.tar.xz"), "bibata-modern-ice");
        assert_eq!(stem("oreo-blue-cursors.tar.gz"), "oreo-blue-cursors");
    }

    #[test]
    fn windows_and_non_archives_are_not_installable() {
        assert!(installable_name("WhiteSur-cursors.tar.xz"));
        assert!(installable_name("x.ZIP"));
        assert!(!installable_name("Bibata-Modern-Ice-Windows.zip"));
        assert!(!installable_name("Bibata-Cursor"));
        assert!(!installable_name("readme.txt"));
        assert!(!installable_name("Nordzy.cape.zip"));
        assert!(!installable_name("Nordzy-cursors-PNGs.zip"));
    }

    #[test]
    fn previews_only_from_pling_images() {
        assert!(preview_url(&Value::from("https://images.pling.com/cache/a.png")).is_some());
        assert!(preview_url(&Value::from("http://images.pling.com/a.png")).is_none());
        assert!(preview_url(&Value::from("https://evil.example/a.png")).is_none());
        assert!(preview_url(&Value::from("https://images.pling.com.evil/a.png")).is_none());
        assert!(preview_url(&Value::from("https://images.pling.com/a b.png")).is_none());
    }

    #[test]
    fn symlinks_must_stay_inside_the_theme() {
        let root = Path::new("/t/Theme");
        let cursors = root.join("cursors");
        assert!(link_stays_inside(root, &cursors, Path::new("left_ptr")));
        assert!(link_stays_inside(root, &cursors, Path::new("../cursors/left_ptr")));
        assert!(!link_stays_inside(root, &cursors, Path::new("../../Other/cursors/x")));
        assert!(!link_stays_inside(root, &cursors, Path::new("/etc/passwd")));
        assert!(!link_stays_inside(root, root, Path::new("..")));
    }

    #[test]
    fn default_file_prefers_the_plain_tarball() {
        let f = |n, name: &str| FileEntry {
            n,
            name: name.into(),
            size_kb: 1,
            installable: installable_name(name),
            preferred: false,
            installed_as: String::new(),
        };
        let files = [
            f(2, "Nordzy-cursor.zip"),
            f(5, "Nordzy-cursors-lefthand.tar.gz"),
            f(12, "Nordzy-cursors.tar.gz"),
            f(13, "Nordzy.cape.zip"),
        ];
        assert_eq!(default_file(&files).map(|f| f.n), Some(12));
    }

    #[test]
    fn query_is_percent_encoded() {
        assert_eq!(encode("nord blue&x=1"), "nord%20blue%26x%3D1");
    }

    #[test]
    fn place_rejects_outward_links_and_installs_nothing() {
        let base = std::env::temp_dir().join(format!("optination-place-{}", std::process::id()));
        let theme = base.join("pkg/Evil-cursors/cursors");
        fs::create_dir_all(&theme).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", theme.join("left_ptr")).unwrap();
        let err = place(&base, 1, "evil.tar.gz").unwrap_err();
        assert!(err.contains("links outside"), "{err}");
        assert!(icons_dir().map(|d| !d.join("Evil-cursors").exists()).unwrap_or(true));
        fs::remove_dir_all(base).unwrap();
    }
}
