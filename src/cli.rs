//! The command-line surface: the engine the overlay drives, and what scripts use.
//!
//! The thing being replaced was a shell function, and shell functions get
//! called from other scripts. Keeping `--list` / `--apply` means nothing that
//! used the old one has to grow a display to keep working. `--json` turns the
//! read-only commands into the machine format the QML overlay parses.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

use crate::{apply, cache, render, scan};

const USAGE: &str = "\
optination — pick a cursor theme, with previews

USAGE:
    optination                          open (or close) the picker overlay
    optination --list                   list every theme found (name, format, shapes)
    optination --apply <THEME> [SIZE]   apply for this session
    optination --save  <THEME> [SIZE]   apply and persist to the Hyprland config
    optination --current                print the active theme and size
    optination --check [SIZE]           report which preview shapes each theme is missing
    optination --render <THEME> <SIZE>  render the preview shapes to the cache
    optination --thumbs                 render every theme's list thumbnail to the cache
    optination --stale                  report a stale XCURSOR_* line in hyprland.conf

    --json   machine-readable output for --list, --current, --render, --thumbs, --stale

Sizes default to the current cursor size.
";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeJson<'a> {
    name: &'a str,
    comment: &'a str,
    format: &'static str,
    x11: bool,
    hypr: bool,
    /// "light", "dark", or "" when the name does not say.
    tone: &'static str,
    mirrored: bool,
    user_installed: bool,
    shapes: usize,
}

impl<'a> ThemeJson<'a> {
    fn new(theme: &'a scan::Theme) -> Self {
        Self {
            name: &theme.name,
            comment: &theme.comment,
            format: theme.format.label(),
            x11: theme.format.has_x11(),
            hypr: theme.format.has_hypr(),
            tone: theme.tone.label(),
            mirrored: theme.mirrored,
            user_installed: theme.user_installed,
            shapes: render::shape_count(theme),
        }
    }
}

#[derive(Serialize)]
struct CurrentJson {
    theme: Option<String>,
    size: Option<u32>,
}

#[derive(Serialize)]
struct SlotJson {
    slot: &'static str,
    path: Option<PathBuf>,
}

#[derive(Serialize)]
struct ThumbJson<'a> {
    name: &'a str,
    path: Option<PathBuf>,
}

#[derive(Serialize)]
struct StaleJson {
    path: Option<PathBuf>,
}

fn print_json(value: &impl Serialize) -> i32 {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match serde_json::to_writer(&mut out, value).map(|()| writeln!(out)) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("optination: {e}");
            1
        }
    }
}

/// Returns `Some(exit_code)` when the arguments were handled, `None` when
/// there were none and the caller should open the overlay instead.
pub fn run() -> Option<i32> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    let (flag, rest) = args.split_first()?;
    Some(dispatch(flag, rest, json))
}

fn size_or_current(rest: &[String]) -> u32 {
    rest.first()
        .and_then(|s| s.parse().ok())
        .or_else(|| apply::current().1)
        .unwrap_or(24)
}

fn resolve(name: &str) -> Result<scan::Theme, i32> {
    scan::themes()
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            eprintln!("optination: no cursor theme named {name:?} (try --list)");
            1
        })
}

fn dispatch(flag: &str, rest: &[String], json: bool) -> i32 {
    match flag {
        "-h" | "--help" => {
            print!("{USAGE}");
            0
        }
        "-V" | "--version" => {
            println!("optination {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "--current" => {
            let (theme, size) = apply::current();
            if json {
                return print_json(&CurrentJson { theme, size });
            }
            println!(
                "{} {}",
                theme.unwrap_or_else(|| "unknown".into()),
                size.map(|s| s.to_string()).unwrap_or_else(|| "?".into())
            );
            0
        }
        "--list" => list(json),
        "--check" => check(size_or_current(rest)),
        "--render" => {
            let Some(name) = rest.first() else {
                eprintln!("optination: --render needs a theme name");
                return 2;
            };
            let theme = match resolve(name) {
                Ok(t) => t,
                Err(code) => return code,
            };
            let size = size_or_current(&rest[1..]);
            let slots: Vec<SlotJson> = cache::preview(&theme, size)
                .into_iter()
                .zip(render::SLOTS)
                .map(|(path, slot)| SlotJson { slot: slot.label, path })
                .collect();
            if json {
                return print_json(&slots);
            }
            for s in slots {
                let path = s.path.map_or_else(|| "-".into(), |p| p.display().to_string());
                println!("{:<8} {path}", s.slot);
            }
            0
        }
        "--thumbs" => {
            let themes = scan::themes();
            let paths = cache::thumbs(&themes);
            let thumbs: Vec<ThumbJson> = themes
                .iter()
                .zip(paths)
                .map(|(t, path)| ThumbJson { name: &t.name, path })
                .collect();
            if json {
                return print_json(&thumbs);
            }
            let drawn = thumbs.iter().filter(|t| t.path.is_some()).count();
            println!("{drawn} of {} thumbnails cached", thumbs.len());
            0
        }
        "--stale" => {
            let path = apply::stale_conf();
            if json {
                return print_json(&StaleJson { path });
            }
            match path {
                Some(p) => println!("stale XCURSOR_* in {} — inert since the Lua port", p.display()),
                None => println!("no stale cursor env"),
            }
            0
        }
        "--apply" | "--save" => apply_or_save(flag, rest),
        other => {
            eprintln!("optination: unknown option {other:?}\n");
            eprint!("{USAGE}");
            2
        }
    }
}

fn list(json: bool) -> i32 {
    let themes = scan::themes();
    if json {
        let rows: Vec<ThemeJson> = themes.iter().map(ThemeJson::new).collect();
        return print_json(&rows);
    }
    // Piping into `head` closes stdout early; that is a normal way to use a
    // listing, not a crash.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for theme in &themes {
        let line = format!(
            "{:<40} {:<11} {:>4} shapes{}",
            theme.name,
            theme.format.label(),
            render::shape_count(theme),
            if theme.user_installed { "  (user)" } else { "" }
        );
        if writeln!(out, "{line}").is_err() {
            break;
        }
    }
    0
}

fn check(size: u32) -> i32 {
    let mut incomplete = 0;
    for theme in scan::themes() {
        let missing: Vec<&str> = render::preview(&theme, size)
            .iter()
            .zip(render::SLOTS)
            .filter(|(bitmap, _)| bitmap.is_none())
            .map(|(_, slot)| slot.label)
            .collect();
        if missing.is_empty() {
            continue;
        }
        incomplete += 1;
        println!("{:<40} missing: {}", theme.name, missing.join(", "));
    }
    println!("{incomplete} theme(s) with an undrawable preview slot at {size}px");
    0
}

fn apply_or_save(flag: &str, rest: &[String]) -> i32 {
    let Some(name) = rest.first() else {
        eprintln!("optination: {flag} needs a theme name");
        return 2;
    };
    let theme = match resolve(name) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let size = size_or_current(&rest[1..]);

    if let Err(e) = apply::live(&theme.name, size).and_then(|()| apply::session(&theme.name, size)) {
        eprintln!("optination: {e}");
        return 1;
    }
    if flag == "--save" {
        match apply::persist(&theme.name, size) {
            Ok(path) => println!("saved {} @ {}px to {}", theme.name, size, path.display()),
            Err(e) => {
                eprintln!("optination: {e}");
                return 1;
            }
        }
    } else {
        println!("{} @ {}px (session only)", theme.name, size);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_json_uses_camel_case_and_plain_labels() {
        let theme = scan::Theme {
            name: "Bibata-Modern-Ice".into(),
            path: PathBuf::from("/nonexistent"),
            format: scan::Format::Both,
            comment: "ice".into(),
            user_installed: true,
            tone: scan::Tone::Unstated,
            mirrored: false,
        };
        let v = serde_json::to_value(ThemeJson::new(&theme)).unwrap();
        assert_eq!(v["name"], "Bibata-Modern-Ice");
        assert_eq!(v["userInstalled"], true);
        assert_eq!(v["x11"], true);
        assert_eq!(v["hypr"], true);
        assert_eq!(v["tone"], "");
        assert_eq!(v["shapes"], 0);
        assert!(v.get("path").is_none(), "paths stay out of the listing");
    }
}
