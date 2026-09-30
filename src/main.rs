//! optination — a cursor theme chooser that shows you the cursors.
//!
//! Replaces a numbered-list shell prompt whose two real failings were that it
//! could not show you what you were picking, and that it only knew about
//! Xcursor themes — leaving every hyprcursor theme on the system invisible.
//!
//! This binary is the engine: it finds themes, renders their shapes and
//! applies them. The picker itself is a QML overlay under `qml/` that drives
//! it through the command line; `launch` opens that overlay.

mod apply;
mod cache;
mod cli;
mod launch;
mod render;
mod scan;

fn main() {
    let code = cli::run().unwrap_or_else(launch::run);
    std::process::exit(code);
}
