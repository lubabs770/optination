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

fn managed_block(theme: &str, size: u32) -> String {
    format!(
        "{BEGIN}\n\
         -- Written by optination. Edit the theme here or re-run the app.\n\
         hl.env(\"XCURSOR_THEME\", \"{theme}\")\n\
         hl.env(\"XCURSOR_SIZE\", \"{size}\")\n\
         hl.env(\"HYPRCURSOR_THEME\", \"{theme}\")\n\
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
