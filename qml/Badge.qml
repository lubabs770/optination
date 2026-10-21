import QtQuick

// A small state label: "saved", "live now", "installed". Filled means the
// durable state (what is saved); outlined means the passing one.
Rectangle {
  id: badge

  required property var ui
  property string label: ""
  property bool filled: false
  property bool small: false

  implicitWidth: text.implicitWidth + (small ? 12 : 16)
  implicitHeight: text.implicitHeight + (small ? 4 : 6)
  radius: ui.tokens.radius
  border.width: 1
  border.color: ui.tokens.accent
  color: filled ? ui.tokens.accent : "transparent"

  Text {
    id: text
    textFormat: Text.PlainText
    anchors.centerIn: parent
    text: badge.label
    color: badge.filled ? badge.ui.tokens.background : badge.ui.tokens.accent
    font.family: badge.ui.font
    font.pixelSize: badge.small ? 10 : 11
    font.bold: badge.filled
  }
}
