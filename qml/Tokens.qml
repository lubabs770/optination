import QtQuick

// The only thing the two hosts disagree about: where the colours come from.
// The Omarchy plugin binds these to the shell's live theme; the standalone
// shell fills them from colors.toml when there is one. The defaults are what
// a plain Hyprland box without either gets.
QtObject {
  property color background: "#101315"
  property color foreground: "#cacccc"
  property color accent: "#7d82d9"
  property color muted: "#707880"
  property color urgent: "#c95f5f"
  property color scrim: Qt.rgba(background.r, background.g, background.b, 0.5)
  property color border: foreground
  property color selectedBackground: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.08)
  property color selectedText: accent
  property int borderWidth: 2
  property int radius: 0
  property string fontFamily: "JetBrainsMono Nerd Font"
}
