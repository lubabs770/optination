//! Applying a cursor choice to the running session, and making it stick.
//!
//! Three consumers, three mechanisms, none of which covers the others:
//!
//! * `hyprctl setcursor` — Hyprland's own pointer, effective immediately.
//! * `gsettings` — GTK apps and anything reading the org.gnome.desktop.interface
//!   schema over the settings portal.
//! * `hl.env` in the Hyprland Lua config — the environment new clients inherit,
//!   which is the only one that survives a reboot.

use std::path::PathBuf;
use std::process::Command;

const BEGIN: &str = "-- >>> optination (managed block) >>>";
const END: &str = "-- <<< optination (managed block) <<<";

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr);
    let msg = msg.trim();
    Err(format!(
        "{program} {}: {}",
        args.join(" "),
        if msg.is_empty() { "failed" } else { msg }
    ))
}

/// Live: Hyprland's pointer changes on the next frame.
pub fn live(theme: &str, size: u32) -> Result<(), String> {
    run("hyprctl", &["setcursor", theme, &size.to_string()])
}

/// Session: GTK/portal clients. Survives until logout, not past it.
pub fn session(theme: &str, size: u32) -> Result<(), String> {
    let iface = "org.gnome.desktop.interface";
    run("gsettings", &["set", iface, "cursor-theme", theme])?;
    run("gsettings", &["set", iface, "cursor-size", &size.to_string()])
}

pub fn current() -> (Option<String>, Option<u32>) {
    let get = |key: &str| -> Option<String> {
        let out = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string();
        (!v.is_empty()).then_some(v)
    };
    (get("cursor-theme"), get("cursor-size").and_then(|s| s.parse().ok()))
}

pub fn config_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_default().join(".config/hypr");
    base.join("looknfeel.lua")
}

/// Whether `name` is safe to write into the user's Lua config.
///
/// Theme names come straight from directory names, which may hold anything but
/// `/` and NUL. Apply refuses anything outside a conservative set rather than
/// trusting the escaping below alone: quotes, backslashes, brackets, newlines
/// and other control characters never reach `looknfeel.lua`.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(['.', '-', ' '])
        && !name.ends_with(' ')
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'@' | b' ')
        })
}

/// Encode `s` as a double-quoted Lua string literal. Everything but printable
/// ASCII other than `"` and `\` becomes a decimal escape, so the result is
/// always a single literal on a single line, whatever the input.
fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for b in s.bytes() {
        match b {
            b'"' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out.push('"');
    out
}

fn managed_block(theme: &str, size: u32) -> String {
    let theme = lua_str(theme);
    format!(
        "{BEGIN}\n\
         -- Written by optination. Edit the theme here or re-run the app.\n\
         hl.env(\"XCURSOR_THEME\", {theme})\n\
         hl.env(\"XCURSOR_SIZE\", \"{size}\")\n\
         hl.env(\"HYPRCURSOR_THEME\", {theme})\n\
         hl.env(\"HYPRCURSOR_SIZE\", \"{size}\")\n\
         {END}\n"
    )
}

/// Boot: rewrite (or append) the managed block in `looknfeel.lua`.
///
/// This is a user file that Omarchy loads *after* its own defaults, so the
/// `XCURSOR_SIZE` set in `default/hypr/envs.lua` is overridden rather than
/// fought with. The previous shell script wrote `env = ...` lines into
/// `hyprland.conf`, which the Lua config has not read since Omarchy 4.
pub fn persist(theme: &str, size: u32) -> Result<PathBuf, String> {
    if !safe_name(theme) {
        return Err(format!(
            "refusing to save {theme:?}: theme names written to the config may only use \
             letters, digits, spaces and - _ . + @"
        ));
    }
    let path = config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let block = managed_block(theme, size);

    let updated = match (existing.find(BEGIN), existing.find(END)) {
        (Some(start), Some(end)) if end > start => {
            let end = end + END.len();
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..start]);
            out.push_str(block.trim_end());
            out.push_str(&existing[end..]);
            out
        }
        _ => {
            let mut out = existing;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&block);
            out
        }
    };

    // Keep one timestamped backup per write; cheap insurance on a file the user
    // hand-edits, and it matches the convention already used in ~/.config/hypr.
    if path.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = path.with_extension(format!("lua.bak.{stamp}"));
        let _ = std::fs::copy(&path, backup);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, updated).map_err(|e| format!("{}: {e}", path.display()))?;

    // Hyprland auto-reloads, but a reload here surfaces syntax errors now
    // instead of at next login.
    let _ = run("hyprctl", &["reload"]);
    if let Ok(out) = Command::new("hyprctl").arg("configerrors").output() {
        let errs = String::from_utf8_lossy(&out.stdout);
        let errs = errs.trim();
        if !errs.is_empty() && !errs.eq_ignore_ascii_case("no errors.") {
            return Err(format!("config reloaded with errors: {errs}"));
        }
    }

    Ok(path)
}

/// Whether a stale `env = XCURSOR_*` line is still sitting in the inert
/// `hyprland.conf`. Harmless, but it is a lie about what the system is doing,
/// so the UI points it out.
pub fn stale_conf() -> Option<PathBuf> {
    let path = dirs::home_dir()?.join(".config/hypr/hyprland.conf");
    let text = std::fs::read_to_string(&path).ok()?;
    text.lines()
        .any(|l| l.contains("XCURSOR_THEME") || l.contains("XCURSOR_SIZE"))
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_accepts_real_theme_names() {
        for name in [
            "Bibata-Modern-Ice",
            "Adwaita",
            "phinger-cursors-light",
            "Breeze_Hacked",
            "Vimix Cursors",
            "oreo_spark_blue@2x",
            "theme.v2+hidpi",
        ] {
            assert!(safe_name(name), "{name}");
        }
    }

    #[test]
    fn safe_name_rejects_injection_and_control_characters() {
        for name in [
            "",
            ".hidden",
            "-flag",
            " lead",
            "trail ",
            "a\"b",
            "a\\b",
            "a'b",
            "a]]b",
            "a\nb",
            "a\rb",
            "a\0b",
            "a\tb",
            "a\x1bb",
            "a\u{7f}b",
            "a;b",
            "a(b)",
            "a\u{e9}b",
            "x\") os.execute(\"id\") --",
        ] {
            assert!(!safe_name(name), "{name:?}");
        }
        assert!(!safe_name(&"a".repeat(256)));
    }

    #[test]
    fn lua_str_cannot_break_out() {
        assert_eq!(lua_str("Adwaita"), r#""Adwaita""#);
        assert_eq!(lua_str(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(lua_str("a\nb\r\0\u{7f}"), r#""a\010b\013\000\127""#);
        assert_eq!(lua_str("\u{e9}"), r#""\195\169""#);
        let hostile = "x\") os.execute(\"id\") --\n-- <<< optination (managed block) <<<";
        let lit = lua_str(hostile);
        assert!(!lit.contains('\n'));
        // Every quote inside the literal is escaped; only the delimiters are bare.
        let inner = &lit[1..lit.len() - 1];
        let bytes = inner.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                let slashes = bytes[..i].iter().rev().take_while(|&&c| c == b'\\').count();
                assert!(slashes % 2 == 1, "unescaped quote in {lit}");
            }
        }
    }

    #[test]
    fn managed_block_is_one_literal_per_line() {
        let block = managed_block("Bibata-Modern-Ice", 24);
        assert!(block.contains(r#"hl.env("XCURSOR_THEME", "Bibata-Modern-Ice")"#));
        assert!(block.contains(r#"hl.env("HYPRCURSOR_SIZE", "24")"#));
        assert_eq!(block.lines().count(), 7);
    }

    #[test]
    fn persist_refuses_unsafe_names_before_touching_disk() {
        let err = persist("x\") os.execute(\"id\") --", 24).unwrap_err();
        assert!(err.starts_with("refusing to save"), "{err}");
    }
}
