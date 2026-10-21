import QtQuick

// Outlined, square, monospace — the Omarchy control look. `ui` is the Picker,
// which owns the colour tokens.
Rectangle {
  id: button

  required property var ui
  property string label: ""
  property bool primary: false
  property bool checked: false
  property bool checkable: false
  // A destructive action waiting for its confirming click.
  property bool danger: false
  // Shown but not clickable, e.g. while a download runs.
  property bool busy: false
  property int fontSize: 13
  signal activated()

  implicitHeight: 36
  implicitWidth: text.implicitWidth + 32
  radius: ui.tokens.radius
  border.width: 1
  opacity: busy ? 0.55 : 1
  border.color: danger ? ui.tokens.urgent : primary || checked ? ui.tokens.accent : ui.outline
  color: danger ? ui.tint(ui.tokens.urgent, 0.15)
    : primary ? ui.tokens.accent
    : checked || mouse.containsMouse ? ui.tint(ui.fg, 0.08)
    : ui.faint

  Accessible.role: Accessible.Button
  Accessible.name: label
  Accessible.checkable: checkable
  Accessible.checked: checked

  Text {
    id: text
    textFormat: Text.PlainText
    anchors.centerIn: parent
    text: button.label
    color: button.danger ? button.ui.tokens.urgent
      : button.primary ? button.ui.tokens.background
      : button.checked ? button.ui.tokens.accent
      : button.ui.fg
    font.family: button.ui.font
    font.pixelSize: button.fontSize
    font.bold: button.primary
  }

  MouseArea {
    id: mouse
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: button.busy ? Qt.BusyCursor : Qt.PointingHandCursor
    onClicked: if (!button.busy) button.activated()
  }
}
