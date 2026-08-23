//! A small non-interactive surface, so the GUI is not the only way in.
//!
//! The thing being replaced was a shell function, and shell functions get
//! called from other scripts. Keeping `--list` / `--apply` means nothing that
//! used the old one has to grow a display to keep working.

use crate::{apply, render, scan};

const USAGE: &str = "\
optination — pick a cursor theme, with previews

USAGE:
    optination                     open the picker
    optination --list              list every theme found (name, format, shapes)
    optination --apply <THEME> [SIZE]   apply for this session
    optination --save  <THEME> [SIZE]   apply and persist to the Hyprland config
    optination --current           print the active theme and size
    optination --check [SIZE]      report which preview shapes each theme is missing

Sizes default to the current cursor size.
";

/// Returns `Some(exit_code)` when the arguments were handled without a GUI.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (flag, rest) = args.split_first()?;

    let size_or_current = |rest: &[String]| -> u32 {
        rest.first()
            .and_then(|s| s.parse().ok())
            .or_else(|| apply::current().1)
            .unwrap_or(24)
    };

    let resolve = |name: &str| -> Result<scan::Theme, i32> {
        scan::themes()
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                eprintln!("optination: no cursor theme named {name:?} (try --list)");
                1
            })
    };

    match flag.as_str() {
        "-h" | "--help" => {
            print!("{USAGE}");
            Some(0)
        }
        "-V" | "--version" => {
            println!("optination {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        "--current" => {
            let (theme, size) = apply::current();
            println!(
                "{} {}",
                theme.unwrap_or_else(|| "unknown".into()),
                size.map(|s| s.to_string()).unwrap_or_else(|| "?".into())
            );
            Some(0)
        }
        "--list" => {
            use std::io::Write;
            // Piping into `head` closes stdout early; that is a normal way to
            // use a listing, not a crash.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for theme in scan::themes() {
                let line = format!(
                    "{:<40} {:<11} {:>4} shapes{}",
                    theme.name,
                    theme.format.label(),
                    render::shape_count(&theme),
                    if theme.user_installed { "  (user)" } else { "" }
                );
                if writeln!(out, "{line}").is_err() {
                    break;
                }
            }
            Some(0)
        }
        "--check" => {
            let size = size_or_current(rest);
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
            Some(0)
        }
        "--apply" | "--save" => {
            let Some(name) = rest.first() else {
                eprintln!("optination: {flag} needs a theme name");
                return Some(2);
            };
            let theme = match resolve(name) {
                Ok(t) => t,
                Err(code) => return Some(code),
            };
            let size = size_or_current(&rest[1..]);

            if let Err(e) = apply::live(&theme.name, size).and_then(|_| apply::session(&theme.name, size)) {
                eprintln!("optination: {e}");
                return Some(1);
            }
            if flag == "--save" {
                match apply::persist(&theme.name, size) {
                    Ok(path) => println!("saved {} @ {}px to {}", theme.name, size, path.display()),
                    Err(e) => {
                        eprintln!("optination: {e}");
                        return Some(1);
                    }
                }
            } else {
                println!("{} @ {}px (session only)", theme.name, size);
            }
            Some(0)
        }
        other => {
            eprintln!("optination: unknown option {other:?}\n");
            eprint!("{USAGE}");
            Some(2)
        }
    }
}
