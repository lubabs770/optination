//! `optination` with no arguments: open the overlay, or close it if it is open.
//!
//! Inside Omarchy the overlay is a plugin loaded by the running omarchy-shell,
//! which already is a Quickshell process — starting a second one there is
//! exactly what Omarchy plugins must never do. Everywhere else the same QML
//! runs as its own short-lived Quickshell instance that quits when closed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const PLUGIN_ID: &str = "io.github.lubabs770.optination";

fn quiet(program: &str, args: &[&str]) -> Option<std::process::Output> {
    Command::new(program).args(args).stdin(Stdio::null()).output().ok()
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    quiet(program, args).is_some_and(|o| o.status.success())
}

/// The shell answers, and lists this plugin as enabled.
fn omarchy_plugin_enabled() -> bool {
    if !succeeds("omarchy-shell", &["shell", "ping"]) {
        return false;
    }
    let Some(out) = quiet("omarchy-plugin-list", &[]) else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).lines().any(|line| {
        let mut cols = line.split_whitespace();
        cols.next() == Some(PLUGIN_ID) && cols.next() == Some("enabled")
    })
}

/// Where install.sh puts the QML; `OPTINATION_QML` points at a checkout instead.
fn qml_dir() -> PathBuf {
    std::env::var_os("OPTINATION_QML").map(PathBuf::from).unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/usr/share"))
            .join("optination/qml")
    })
}

pub fn run() -> i32 {
    if omarchy_plugin_enabled() {
        return if succeeds("omarchy-shell", &["shell", "toggle", PLUGIN_ID]) {
            0
        } else {
            eprintln!("optination: omarchy-shell refused to toggle {PLUGIN_ID}");
            1
        };
    }

    let shell = qml_dir().join("shell.qml");
    if !shell.is_file() {
        eprintln!(
            "optination: overlay files not found at {} (run install.sh, or set OPTINATION_QML)",
            shell.display()
        );
        return 1;
    }
    let shell = shell.to_string_lossy();

    // An instance already up means the overlay is open: toggle closes it.
    if succeeds("quickshell", &["ipc", "-p", &shell, "call", "optination", "toggle"]) {
        return 0;
    }
    match Command::new("quickshell")
        .args(["--no-duplicate", "--daemonize", "-p", &shell])
        .stdin(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => 0,
        Ok(status) => status.code().unwrap_or(1),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("optination: the overlay needs Quickshell (pacman -S quickshell)");
            1
        }
        Err(e) => {
            eprintln!("optination: could not start quickshell: {e}");
            1
        }
    }
}
