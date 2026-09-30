import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

// optination outside Omarchy: its own short-lived Quickshell instance. It
// opens on start and quits when the picker is dismissed, so there is no idle
// daemon; `optination` run again while it is up asks it to close over IPC.
ShellRoot {
  id: shellRoot

  property Tokens tokens: Tokens {}

  // Omarchy's palette is used when it is there (e.g. Omarchy without the
  // plugin enabled); anything else keeps the Tokens defaults.
  FileView {
    path: Quickshell.env("HOME") + "/.local/state/omarchy/current/theme/colors.toml"
    printErrors: false
    onLoaded: {
      const colors = {}
      for (const line of text().split("\n")) {
        const m = line.match(/^\s*([a-z_]+)\s*=\s*"(#[0-9a-fA-F]{6,8})"/)
        if (m) colors[m[1]] = m[2]
      }
      if (colors.background) shellRoot.tokens.background = colors.background
      if (colors.foreground) shellRoot.tokens.foreground = colors.foreground
      if (colors.accent) shellRoot.tokens.accent = colors.accent
      if (colors.muted) shellRoot.tokens.muted = colors.muted
      if (colors.red) shellRoot.tokens.urgent = colors.red
    }
  }

  PanelWindow {
    visible: true
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "optination"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive

    Picker {
      id: picker
      anchors.fill: parent
      tokens: shellRoot.tokens
      Component.onCompleted: start()
      onDismissed: Qt.quit()
    }
  }

  IpcHandler {
    target: "optination"
    function toggle(): void {
      picker.revert()
    }
  }
}
