#!/usr/bin/env bash
# Install optination for the current user.
#
#   ./install.sh [path/to/optination]
#
# The binary defaults to the one beside this script (CI bundle), else
# target/release/optination. QML goes to ~/.local/share/optination/qml. On Omarchy the same
# QML folder is linked in as a shell plugin and enabled, so `optination` opens
# it inside omarchy-shell instead of starting a second Quickshell. Removing the
# plugin (omarchy plugin remove) only unlinks it.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A CI bundle ships the binary beside this script; a checkout builds it.
default_bin="$here/target/release/optination"
[[ -f $here/optination ]] && default_bin="$here/optination"
bin="${1:-$default_bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
plugin_id="io.github.lubabs770.optination"
plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/$plugin_id"

if [[ ! -f $bin ]]; then
  echo "install.sh: no binary at $bin — build it (CI artifact or cargo build --release) and pass its path" >&2
  exit 1
fi

install -Dm755 "$bin" "$HOME/.local/bin/optination"

# The plugin folder: manifest at its root, QML under qml/ — the same layout as
# the repo, so `omarchy plugin add <repo>` and this script install alike.
rm -rf "$data/optination/qml"
mkdir -p "$data/optination"
cp -r "$here/qml" "$data/optination/qml"
install -Dm644 "$here/manifest.json" "$data/optination/manifest.json"

install -Dm644 "$here/share/optination.desktop" "$data/applications/optination.desktop"
install -Dm644 "$here/share/optination.svg" "$data/icons/hicolor/scalable/apps/optination.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

if command -v omarchy >/dev/null; then
  mkdir -p "$(dirname "$plugin_dir")"
  if [[ -e $plugin_dir && ! -L $plugin_dir ]]; then
    echo "install.sh: $plugin_dir exists and is not our link; leaving it alone" >&2
  else
    ln -sfn "$data/optination" "$plugin_dir"
    omarchy-shell -q shell rescanPlugins
    omarchy plugin enable "$plugin_id" >/dev/null
    echo "enabled the Omarchy plugin $plugin_id"
  fi
fi

if ! command -v quickshell >/dev/null; then
  echo "note: the overlay needs Quickshell outside Omarchy (pacman -S quickshell)" >&2
fi

echo "installed optination $("$HOME/.local/bin/optination" --version | cut -d' ' -f2)"
