import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

// The whole picker, drawn once for both hosts. The host owns the window and
// the colour tokens; this owns everything inside, and talks to the optination
// binary for theme data, rendered shapes and applying a choice.
//
// Clicking a theme or changing the size applies it live (session only), the
// way the old app did. Apply persists it; Esc / Revert puts back what was
// active when the picker opened.
Item {
  id: root

  required property var tokens
  signal dismissed()

  // ------------------------------------------------------------- tokens ---

  readonly property color fg: tokens.foreground
  readonly property color deep: Qt.darker(tokens.background, 1.6)
  readonly property color line: tint(fg, 0.15)
  readonly property color outline: tint(fg, 0.4)
  readonly property color faint: tint(fg, 0.04)
  readonly property string font: tokens.fontFamily

  function tint(c, a) {
    return Qt.rgba(c.r, c.g, c.b, a)
  }

  // --------------------------------------------------------------- state ---

  readonly property string home: Quickshell.env("HOME")
  readonly property string stateDir: (Quickshell.env("XDG_STATE_HOME") || home + "/.local/state") + "/optination"
  // Where the engine writes rendered shapes; the only place images load from.
  readonly property string cacheRoot: (Quickshell.env("XDG_CACHE_HOME") || home + "/.cache") + "/optination/"
  property string engine: ""
  property bool engineChecked: false
  readonly property bool engineMissing: engineChecked && engine === ""

  // Upper bounds on everything read from a process or file, so a broken or
  // hostile engine cannot balloon the shell's memory.
  readonly property int maxListBytes: 2 * 1024 * 1024
  readonly property int maxSmallBytes: 64 * 1024
  readonly property int maxThemes: 5000
  readonly property int maxErrorChars: 300

  property var themes: []
  property var thumbs: ({})
  property var shapes: ({})
  property var filters: ({})
  property string query: ""
  property string selected: ""
  property int size: 24
  property string originalTheme: ""
  property int originalSize: 24
  property string stalePath: ""
  property string error: ""
  property bool copied: false
  property bool searching: false
  property int listWidth: 420
  property bool dragging: false

  readonly property int sizeMin: 16
  readonly property int sizeMax: 96
  readonly property var ticks: [16, 24, 32, 48, 64, 96]
  readonly property var visibleThemes: filterThemes(themes, query, filters)
  readonly property var selectedTheme: themes.find(t => t.name === selected) || null
  readonly property string command: "hyprctl setcursor " + selected + " " + size

  // Chips inside a group are OR-ed, groups are AND-ed, so "hyprcursor" plus
  // "dark" narrows to dark hyprcursor themes rather than to nothing.
  readonly property var chips: [
    { id: "x11", label: "xcursor", group: "format" },
    { id: "hypr", label: "hyprcursor", group: "format" },
    { id: "light", label: "light", group: "tone" },
    { id: "dark", label: "dark", group: "tone" },
    { id: "standard", label: "standard", group: "variant" },
    { id: "mirrored", label: "mirrored", group: "variant" },
    { id: "system", label: "system", group: "source" },
    { id: "user", label: "user", group: "source" }
  ]

  function chipMatches(id, t) {
    switch (id) {
    case "x11": return t.x11
    case "hypr": return t.hypr
    case "light": return t.tone === "light"
    case "dark": return t.tone === "dark"
    case "standard": return !t.mirrored
    case "mirrored": return t.mirrored
    case "system": return !t.userInstalled
    case "user": return t.userInstalled
    }
    return true
  }

  function filterThemes(list, needle, active) {
    const q = needle.toLowerCase()
    const groups = {}
    for (const c of chips) {
      if (active[c.id]) (groups[c.group] = groups[c.group] || []).push(c.id)
    }
    return list.filter(t => {
      for (const g in groups) {
        if (!groups[g].some(id => chipMatches(id, t))) return false
      }
      if (!q) return true
      return t.name.toLowerCase().includes(q)
        || t.comment.toLowerCase().includes(q)
        || t.format.toLowerCase().includes(q)
        || (t.tone && t.tone.includes(q))
    })
  }

  function meta(t) {
    let parts = [t.format, t.shapes + " shapes"]
    if (t.tone) parts.push(t.tone)
    if (t.mirrored) parts.push("mirrored")
    if (t.userInstalled) parts.push("user")
    return parts.join(" · ")
  }

  function clampSize(v) {
    return Math.max(sizeMin, Math.min(sizeMax, Math.round(v)))
  }

  function clampWidth(w) {
    return Math.max(260, Math.min(760, Math.round(w)))
  }

  // Only PNGs inside our own cache directory become image sources.
  function fileUrl(path) {
    if (typeof path !== "string" || !path.startsWith(cacheRoot) || !path.endsWith(".png")
        || path.includes("/../") || path.includes("\n"))
      return ""
    return "file://" + path.split("/").map(encodeURIComponent).join("/")
  }

  function oneLine(text) {
    const first = String(text).trim().split("\n")[0]
    return first.length > maxErrorChars ? first.slice(0, maxErrorChars) + "…" : first
  }

  // ----------------------------------------------------------- lifecycle ---

  function start() {
    error = ""
    query = ""
    searching = false
    copied = false
    Quickshell.execDetached(["mkdir", "-p", stateDir])
    if (engine) load()
    else resolveProc.running = true
    Qt.callLater(() => keys.forceActiveFocus())
  }

  function load() {
    currentProc.running = true
    listProc.running = true
    thumbsProc.running = true
    staleProc.running = true
  }

  // Put back what was active when the picker opened. Safe to call twice.
  function restoreOriginal() {
    applyDebounce.stop()
    if (!engine || !originalTheme) return
    if (selected === originalTheme && size === originalSize) return
    selected = originalTheme
    size = originalSize
    Quickshell.execDetached([engine, "--apply", originalTheme, String(originalSize)])
  }

  function revert() {
    restoreOriginal()
    dismissed()
  }

  function save() {
    if (!selected || saveProc.running) return
    applyDebounce.stop()
    error = ""
    saveProc.command = [engine, "--save", selected, String(size)]
    saveProc.running = true
  }

  function pick(name) {
    if (name === selected) return
    selected = name
    requestPreview()
    applyDebounce.restart()
  }

  function setSize(v) {
    const next = clampSize(v)
    if (next === size) return
    size = next
    requestPreview()
    applyDebounce.restart()
  }

  function move(step) {
    const list = visibleThemes
    if (list.length === 0) return
    let i = list.findIndex(t => t.name === selected)
    i = i < 0 ? 0 : Math.max(0, Math.min(list.length - 1, i + step))
    pick(list[i].name)
    themeList.positionViewAtIndex(i, ListView.Contain)
  }

  // Scroll the list so the selected theme is in view; the list and the active
  // theme arrive from separate processes, in either order, so both call this.
  function revealSelected() {
    const i = visibleThemes.findIndex(t => t.name === selected)
    if (i >= 0) Qt.callLater(() => themeList.positionViewAtIndex(i, ListView.Center))
  }

  function copyCommand() {
    Quickshell.execDetached(["wl-copy", command])
    copied = true
    copyReset.restart()
  }

  function requestPreview() {
    renderDebounce.restart()
  }

  property bool renderPending: false
  function renderNow() {
    if (!engine || !selected) return
    if (renderProc.running) {
      renderPending = true
      return
    }
    renderProc.command = [engine, "--render", selected, String(size), "--json"]
    renderProc.running = true
  }

  property bool applyPending: false
  function applyNow() {
    if (!engine || !selected) return
    if (applyProc.running) {
      applyPending = true
      return
    }
    applyProc.command = [engine, "--apply", selected, String(size)]
    applyProc.running = true
  }

  // JSON of the same shape as `fallback` (array or object), within `limit`
  // bytes; anything else is the fallback.
  function parse(text, fallback, limit) {
    if (text.length > limit) return fallback
    try {
      const value = JSON.parse(text)
      const ok = value !== null && typeof value === "object"
        && Array.isArray(value) === Array.isArray(fallback)
      return ok ? value : fallback
    } catch (e) {
      return fallback
    }
  }

  function isThemeRow(t) {
    return t && typeof t.name === "string" && t.name.length > 0 && t.name.length < 256
      && typeof t.comment === "string" && typeof t.format === "string"
      && typeof t.tone === "string" && typeof t.shapes === "number"
  }

  // ----------------------------------------------------------- processes ---

  // Never trust a folder handed over by the host shell: find the binary the
  // way a user's own shell would, then fall back to the install location.
  // The engine is found the way a user's own shell would find it, never via a
  // folder the host hands over. Printing nothing means it is not installed.
  Process {
    id: resolveProc
    command: ["sh", "-c", "if [ -n \"$OPTINATION_BIN\" ] && [ -x \"$OPTINATION_BIN\" ]; then printf %s \"$OPTINATION_BIN\"; "
      + "elif command -v optination >/dev/null 2>&1; then command -v optination; "
      + "elif [ -x \"$HOME/.local/bin/optination\" ]; then printf %s \"$HOME/.local/bin/optination\"; fi"]
    stdout: StdioCollector {
      onStreamFinished: {
        const path = text.trim()
        root.engine = path.startsWith("/") && !path.includes("\n") ? path : ""
        root.engineChecked = true
        if (root.engine) root.load()
      }
    }
  }

  Process {
    id: currentProc
    command: [root.engine, "--current", "--json"]
    stdout: StdioCollector {
      onStreamFinished: {
        const cur = root.parse(text, {}, root.maxSmallBytes)
        root.originalTheme = typeof cur.theme === "string" ? cur.theme : ""
        root.originalSize = root.clampSize(typeof cur.size === "number" ? cur.size : 24)
        root.size = root.originalSize
        if (root.originalTheme) root.selected = root.originalTheme
        else if (!root.selected && root.themes.length > 0) root.selected = root.themes[0].name
        root.revealSelected()
        root.requestPreview()
      }
    }
  }

  Process {
    id: listProc
    command: [root.engine, "--list", "--json"]
    stdout: StdioCollector {
      onStreamFinished: {
        const rows = root.parse(text, [], root.maxListBytes)
        root.themes = rows.filter(root.isThemeRow).slice(0, root.maxThemes)
        if (root.themes.length === 0 && text.trim() !== "[]")
          root.error = "the optination engine did not answer with a theme list — is it up to date?"
        // Only fall back to the first theme once the active one is known to be unknown.
        if (!root.selected && !currentProc.running && root.themes.length > 0)
          root.selected = root.themes[0].name
        root.revealSelected()
        root.requestPreview()
      }
    }
    stderr: StdioCollector {
      onStreamFinished: if (text.trim()) root.error = root.oneLine(text)
    }
  }

  Process {
    id: thumbsProc
    command: [root.engine, "--thumbs", "--json"]
    stdout: StdioCollector {
      onStreamFinished: {
        const map = {}
        for (const t of root.parse(text, [], root.maxListBytes).slice(0, root.maxThemes)) {
          if (t && typeof t.name === "string") map[t.name] = t.path
        }
        root.thumbs = map
      }
    }
  }

  Process {
    id: staleProc
    command: [root.engine, "--stale", "--json"]
    stdout: StdioCollector {
      onStreamFinished: {
        const path = root.parse(text, {}, root.maxSmallBytes).path
        root.stalePath = typeof path === "string" ? path : ""
      }
    }
  }

  Process {
    id: renderProc
    stdout: StdioCollector {
      onStreamFinished: {
        const map = {}
        for (const s of root.parse(text, [], root.maxSmallBytes)) {
          if (s && typeof s.slot === "string") map[s.slot] = s.path
        }
        root.shapes = map
      }
    }
    onExited: {
      if (root.renderPending) {
        root.renderPending = false
        root.renderNow()
      }
    }
  }

  Process {
    id: applyProc
    stderr: StdioCollector {
      onStreamFinished: root.error = root.oneLine(text)
    }
    onExited: {
      if (root.applyPending) {
        root.applyPending = false
        root.applyNow()
      }
    }
  }

  Process {
    id: saveProc
    stderr: StdioCollector {
      id: saveErr
    }
    onExited: function(code) {
      if (code === 0) {
        root.originalTheme = root.selected
        root.originalSize = root.size
        root.dismissed()
      } else {
        root.error = root.oneLine(saveErr.text) || "saving failed"
      }
    }
  }

  Timer { id: renderDebounce; interval: 40; onTriggered: root.renderNow() }
  Timer { id: applyDebounce; interval: 120; onTriggered: root.applyNow() }
  Timer { id: copyReset; interval: 1400; onTriggered: root.copied = false }

  FileView {
    id: uiState
    path: root.stateDir + "/ui.json"
    printErrors: false
    atomicWrites: true
    onLoaded: {
      const saved = root.parse(text(), {}, 4096)
      if (typeof saved.listWidth === "number") root.listWidth = root.clampWidth(saved.listWidth)
    }
  }

  // ------------------------------------------------------------ keyboard ---

  Item {
    id: keys
    focus: true
    Keys.onPressed: function(event) {
      const key = event.key
      const text = event.text
      if (key === Qt.Key_Escape) {
        root.revert()
      } else if (key === Qt.Key_Return || key === Qt.Key_Enter) {
        root.save()
      } else if (text === "/") {
        root.searching = true
        search.forceActiveFocus()
      } else if (text === "j" || key === Qt.Key_Down) {
        root.move(1)
      } else if (text === "k" || key === Qt.Key_Up) {
        root.move(-1)
      } else if (text === "[") {
        root.setSize(root.size - 1)
      } else if (text === "]") {
        root.setSize(root.size + 1)
      } else {
        return
      }
      event.accepted = true
    }
  }

  // -------------------------------------------------------------- layout ---

  Rectangle {
    anchors.fill: parent
    color: root.tokens.scrim
  }

  // A click outside the card is a cancel.
  MouseArea {
    anchors.fill: parent
    onClicked: root.revert()
  }

  Rectangle {
    id: card
    anchors.centerIn: parent
    width: Math.min(parent.width - 96, 1320)
    height: Math.min(parent.height - 96, 860)
    color: root.tokens.background
    border.color: root.tokens.border
    border.width: root.tokens.borderWidth
    radius: root.tokens.radius
    clip: true

    MouseArea { anchors.fill: parent }

    ColumnLayout {
      anchors.fill: parent
      anchors.margins: card.border.width
      spacing: 0

      // Header: name, count, the stale-config fact when there is one, keys.
      RowLayout {
        Layout.fillWidth: true
        Layout.preferredHeight: 48
        Layout.leftMargin: 20
        Layout.rightMargin: 20
        spacing: 16

        Text {
          textFormat: Text.PlainText
          text: "optination"
          color: root.fg
          font.family: root.font
          font.pixelSize: 14
          font.bold: true
        }
        Text {
          textFormat: Text.PlainText
          text: "cursor themes · " + root.themes.length + " installed"
          color: root.tokens.muted
          font.family: root.font
          font.pixelSize: 13
        }
        Text {
          textFormat: Text.PlainText
          visible: root.stalePath !== ""
          text: "! stale XCURSOR_* in hyprland.conf — inert since the Lua port"
          color: root.tokens.muted
          font.family: root.font
          font.pixelSize: 12
          elide: Text.ElideRight
          Layout.fillWidth: true
          Layout.maximumWidth: implicitWidth
        }
        Item { Layout.fillWidth: true }
        Repeater {
          model: [["/", "search"], ["j k", "browse"], ["[ ]", "size"], ["enter", "apply"], ["esc", "revert"]]
          delegate: Row {
            required property var modelData
            spacing: 6
            Rectangle {
              width: keyText.implicitWidth + 12
              height: keyText.implicitHeight + 4
              color: "transparent"
              border.width: 1
              border.color: root.tint(root.fg, 0.25)
              radius: root.tokens.radius
              Text {
                id: keyText
                textFormat: Text.PlainText
                anchors.centerIn: parent
                text: modelData[0]
                color: root.fg
                font.family: root.font
                font.pixelSize: 12
              }
            }
            Text {
              textFormat: Text.PlainText
              anchors.verticalCenter: parent.verticalCenter
              text: modelData[1]
              color: root.tokens.muted
              font.family: root.font
              font.pixelSize: 12
            }
          }
        }
      }

      Rectangle { Layout.fillWidth: true; Layout.preferredHeight: 1; color: root.line }

      RowLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 0

        // ------------------------------------------------ theme list ---
        ColumnLayout {
          Layout.preferredWidth: root.listWidth
          Layout.fillHeight: true
          spacing: 12

          Rectangle {
            Layout.fillWidth: true
            Layout.topMargin: 16
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            Layout.preferredHeight: 40
            color: root.faint
            border.width: 1
            border.color: search.activeFocus ? root.tint(root.fg, 0.6) : root.outline
            radius: root.tokens.radius

            RowLayout {
              anchors.fill: parent
              anchors.leftMargin: 12
              anchors.rightMargin: 12
              spacing: 10
              Text {
                textFormat: Text.PlainText
                text: "›"
                color: root.tokens.accent
                font.family: root.font
                font.pixelSize: 15
                font.bold: true
              }
              TextInput {
                id: search
                Layout.fillWidth: true
                color: root.fg
                selectionColor: root.tint(root.tokens.accent, 0.35)
                font.family: root.font
                font.pixelSize: 13
                clip: true
                text: root.query
                onTextEdited: root.query = text
                Accessible.name: "Search cursor themes"
                Keys.onPressed: function(event) {
                  if (event.key === Qt.Key_Escape) {
                    if (root.query) root.query = ""
                    else keys.forceActiveFocus()
                  } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                    keys.forceActiveFocus()
                  } else if (event.key === Qt.Key_Down) {
                    root.move(1)
                  } else if (event.key === Qt.Key_Up) {
                    root.move(-1)
                  } else {
                    return
                  }
                  event.accepted = true
                }
                Text {
                  textFormat: Text.PlainText
                  anchors.verticalCenter: parent.verticalCenter
                  visible: !search.text
                  text: "search themes…"
                  color: root.tokens.muted
                  font: search.font
                }
                MouseArea {
                  anchors.fill: parent
                  cursorShape: Qt.IBeamCursor
                  onPressed: function(mouse) {
                    search.forceActiveFocus()
                    mouse.accepted = false
                  }
                }
              }
              Text {
                textFormat: Text.PlainText
                text: root.visibleThemes.length + " shown"
                color: root.tokens.muted
                font.family: root.font
                font.pixelSize: 11
              }
            }
          }

          Flow {
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            spacing: 6
            Repeater {
              model: root.chips
              delegate: FlatButton {
                required property var modelData
                ui: root
                label: modelData.label
                checkable: true
                checked: root.filters[modelData.id] === true
                implicitHeight: 28
                fontSize: 12
                onActivated: {
                  const next = Object.assign({}, root.filters)
                  next[modelData.id] = !next[modelData.id]
                  root.filters = next
                }
              }
            }
          }

          ListView {
            id: themeList
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            Layout.bottomMargin: 16
            clip: true
            spacing: 2
            model: root.visibleThemes
            boundsBehavior: Flickable.StopAtBounds

            delegate: Rectangle {
              id: row
              required property var modelData
              readonly property bool current: modelData.name === root.selected
              width: ListView.view.width
              height: 56
              color: current ? root.tokens.selectedBackground
                : rowMouse.containsMouse ? root.tint(root.fg, 0.04) : "transparent"
              border.width: 1
              border.color: current ? root.tint(root.fg, 0.25) : "transparent"
              radius: root.tokens.radius

              RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 10
                spacing: 12

                Rectangle {
                  Layout.preferredWidth: 40
                  Layout.preferredHeight: 40
                  color: root.deep
                  border.width: 1
                  border.color: root.line
                  radius: root.tokens.radius
                  Image {
                    anchors.centerIn: parent
                    width: 28
                    height: 28
                    source: root.fileUrl(root.thumbs[row.modelData.name])
                    fillMode: Image.PreserveAspectFit
                    sourceSize.width: 64
                    sourceSize.height: 64
                    smooth: true
                    mipmap: true
                    asynchronous: true
                  }
                }

                ColumnLayout {
                  Layout.fillWidth: true
                  spacing: 3
                  Text {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: row.modelData.name
                    color: row.current ? root.tokens.selectedText : root.fg
                    font.family: root.font
                    font.pixelSize: 13
                    elide: Text.ElideRight
                  }
                  Text {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: root.meta(row.modelData)
                    color: root.tokens.muted
                    font.family: root.font
                    font.pixelSize: 11
                    elide: Text.ElideRight
                  }
                }

                Rectangle {
                  visible: row.modelData.name === root.originalTheme
                  Layout.preferredWidth: inUse.implicitWidth + 12
                  Layout.preferredHeight: inUse.implicitHeight + 4
                  color: "transparent"
                  border.width: 1
                  border.color: root.tint(root.fg, 0.35)
                  radius: root.tokens.radius
                  Text {
                    id: inUse
                    textFormat: Text.PlainText
                    anchors.centerIn: parent
                    text: "in use"
                    color: root.tokens.muted
                    font.family: root.font
                    font.pixelSize: 10
                  }
                }
              }

              MouseArea {
                id: rowMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                  root.pick(row.modelData.name)
                  keys.forceActiveFocus()
                }
              }
            }

            Text {
              textFormat: Text.PlainText
              anchors.centerIn: parent
              width: parent.width - 20
              horizontalAlignment: Text.AlignHCenter
              wrapMode: Text.WordWrap
              visible: root.engineMissing
              text: "The optination engine is not installed.\nGet it from github.com/lubabs770/optination and run install.sh."
              color: root.tokens.muted
              font.family: root.font
              font.pixelSize: 13
            }

            Text {
              textFormat: Text.PlainText
              anchors.centerIn: parent
              visible: root.themes.length > 0 && root.visibleThemes.length === 0
              text: "no themes match — clear a filter"
              color: root.tokens.muted
              font.family: root.font
              font.pixelSize: 13
            }
          }
        }

        // ---------------------------------------------- resize grip ---
        Item {
          id: grip
          Layout.preferredWidth: 9
          Layout.fillHeight: true
          Layout.leftMargin: -4
          Layout.rightMargin: -4
          z: 1

          Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: root.dragging ? 2 : 1
            height: parent.height
            color: root.dragging ? root.tokens.accent : root.line
          }

          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.SplitHCursor
            property real startX: 0
            property int startWidth: 0
            onPressed: function(mouse) {
              startX = mapToItem(root, mouse.x, 0).x
              startWidth = root.listWidth
              root.dragging = true
            }
            onPositionChanged: function(mouse) {
              root.listWidth = root.clampWidth(startWidth + mapToItem(root, mouse.x, 0).x - startX)
            }
            onReleased: {
              root.dragging = false
              uiState.setText(JSON.stringify({ listWidth: root.listWidth }))
            }
          }
        }

        // ------------------------------------------ preview + controls ---
        ColumnLayout {
          Layout.fillWidth: true
          Layout.fillHeight: true
          Layout.margins: 20
          Layout.leftMargin: 24
          Layout.rightMargin: 24
          spacing: 20

          Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: root.deep
            border.width: 1
            border.color: root.line
            radius: root.tokens.radius
            clip: true

            Canvas {
              id: dots
              anchors.fill: parent
              anchors.topMargin: 44
              onWidthChanged: requestPaint()
              onHeightChanged: requestPaint()
              onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                ctx.fillStyle = root.tint(root.fg, 0.09)
                for (let x = 8; x < width; x += 16)
                  for (let y = 8; y < height; y += 16)
                    ctx.fillRect(x, y, 1.5, 1.5)
              }
            }

            RowLayout {
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.top: parent.top
              height: 44
              anchors.leftMargin: 14
              anchors.rightMargin: 14
              spacing: 12

              Text {
                textFormat: Text.PlainText
                text: "PREVIEW"
                color: root.tokens.accent
                font.family: root.font
                font.pixelSize: 11
                font.letterSpacing: 1.3
              }
              Text {
                textFormat: Text.PlainText
                text: root.selected
                color: root.fg
                font.family: root.font
                font.pixelSize: 13
              }
              Text {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: root.selectedTheme ? root.meta(root.selectedTheme) : ""
                color: root.tokens.muted
                font.family: root.font
                font.pixelSize: 11
                elide: Text.ElideRight
              }
              Rectangle {
                Layout.preferredWidth: pxText.implicitWidth + 16
                Layout.preferredHeight: pxText.implicitHeight + 6
                color: "transparent"
                border.width: 1
                border.color: root.tint(root.fg, 0.25)
                radius: root.tokens.radius
                Text {
                  id: pxText
                  textFormat: Text.PlainText
                  anchors.centerIn: parent
                  text: root.size + " px"
                  color: root.fg
                  font.family: root.font
                  font.pixelSize: 12
                }
              }
            }

            Rectangle {
              anchors.left: parent.left
              anchors.right: parent.right
              y: 44
              height: 1
              color: root.tint(root.fg, 0.08)
            }

            ColumnLayout {
              anchors.centerIn: dots
              spacing: 44

              Item {
                Layout.alignment: Qt.AlignHCenter
                Layout.preferredWidth: 256
                Layout.preferredHeight: Math.min(256, bigPointer.implicitHeight)
                Image {
                  id: bigPointer
                  anchors.centerIn: parent
                  source: root.fileUrl(root.shapes["pointer"])
                  width: Math.min(256, implicitWidth)
                  height: Math.min(256, implicitHeight)
                  fillMode: Image.PreserveAspectFit
                  cache: false
                  smooth: true
                }
              }

              Grid {
                Layout.alignment: Qt.AlignHCenter
                columns: 5
                spacing: 1
                Repeater {
                  model: ["text", "link", "resize", "busy", "no-drop"]
                  delegate: Rectangle {
                    id: cell
                    required property string modelData
                    readonly property string path: root.shapes[modelData] || ""
                    width: 112
                    height: 112
                    color: root.deep
                    border.width: 1
                    border.color: root.tint(root.fg, 0.12)

                    Image {
                      anchors.centerIn: parent
                      anchors.verticalCenterOffset: -10
                      visible: cell.path !== ""
                      source: root.fileUrl(cell.path)
                      width: Math.min(80, implicitWidth)
                      height: Math.min(80, implicitHeight)
                      fillMode: Image.PreserveAspectFit
                      cache: false
                      smooth: true
                    }
                    Text {
                      textFormat: Text.PlainText
                      anchors.centerIn: parent
                      anchors.verticalCenterOffset: -10
                      visible: cell.path === "" && root.selected !== ""
                      text: "—"
                      color: root.tokens.muted
                      font.family: root.font
                      font.pixelSize: 16
                    }
                    Text {
                      textFormat: Text.PlainText
                      anchors.horizontalCenter: parent.horizontalCenter
                      anchors.bottom: parent.bottom
                      anchors.bottomMargin: 8
                      text: cell.modelData
                      color: root.tokens.muted
                      font.family: root.font
                      font.pixelSize: 11
                    }
                  }
                }
              }
            }
          }

          // ------------------------------------------------- size ---
          ColumnLayout {
            Layout.fillWidth: true
            spacing: 12

            RowLayout {
              Layout.fillWidth: true
              spacing: 12
              Text {
                textFormat: Text.PlainText
                text: "SIZE"
                color: root.tokens.accent
                font.family: root.font
                font.pixelSize: 11
                font.letterSpacing: 1.3
              }
              Text {
                textFormat: Text.PlainText
                text: "slider snaps to even · − + step one pixel"
                color: root.tokens.muted
                font.family: root.font
                font.pixelSize: 11
              }
              Item { Layout.fillWidth: true }
              Text {
                textFormat: Text.PlainText
                text: "XCURSOR_SIZE · HYPRCURSOR_SIZE"
                color: root.tokens.muted
                font.family: root.font
                font.pixelSize: 11
              }
            }

            RowLayout {
              Layout.fillWidth: true
              spacing: 12

              FlatButton {
                ui: root
                label: "−"
                implicitWidth: 32
                implicitHeight: 32
                fontSize: 16
                Accessible.name: "One pixel smaller"
                onActivated: root.setSize(root.size - 1)
              }

              Item {
                id: slider
                Layout.fillWidth: true
                Layout.preferredHeight: 32
                readonly property real fraction: (root.size - root.sizeMin) / (root.sizeMax - root.sizeMin)

                Rectangle {
                  anchors.verticalCenter: parent.verticalCenter
                  width: parent.width
                  height: 2
                  color: root.tint(root.fg, 0.18)
                }
                Rectangle {
                  anchors.verticalCenter: parent.verticalCenter
                  width: parent.width * slider.fraction
                  height: 2
                  color: root.tokens.accent
                }
                Rectangle {
                  anchors.verticalCenter: parent.verticalCenter
                  x: parent.width * slider.fraction - width / 2
                  width: 10
                  height: 20
                  color: root.fg
                }
                MouseArea {
                  anchors.fill: parent
                  cursorShape: Qt.PointingHandCursor
                  function seek(x) {
                    const f = Math.max(0, Math.min(1, x / width))
                    root.setSize(Math.round((root.sizeMin + f * (root.sizeMax - root.sizeMin)) / 2) * 2)
                  }
                  onPressed: function(mouse) { seek(mouse.x) }
                  onPositionChanged: function(mouse) { seek(mouse.x) }
                }
              }

              FlatButton {
                ui: root
                label: "+"
                implicitWidth: 32
                implicitHeight: 32
                fontSize: 16
                Accessible.name: "One pixel larger"
                onActivated: root.setSize(root.size + 1)
              }
            }

            GridLayout {
              Layout.fillWidth: true
              columns: root.ticks.length
              columnSpacing: 6
              Repeater {
                model: root.ticks
                delegate: FlatButton {
                  required property int modelData
                  ui: root
                  Layout.fillWidth: true
                  label: String(modelData)
                  checkable: true
                  checked: modelData === root.size
                  implicitHeight: 30
                  fontSize: 12
                  onActivated: root.setSize(modelData)
                }
              }
            }
          }

          // ----------------------------------------------- footer ---
          Rectangle { Layout.fillWidth: true; Layout.preferredHeight: 1; color: root.tint(root.fg, 0.08) }

          RowLayout {
            Layout.fillWidth: true
            spacing: 12

            Row {
              visible: root.selected !== ""
              spacing: 7
              Text {
                textFormat: Text.PlainText
                text: "$"
                color: root.tokens.accent
                font.family: root.font
                font.pixelSize: 12
              }
              Text {
                textFormat: Text.PlainText
                text: root.command
                color: root.tokens.muted
                font.family: root.font
                font.pixelSize: 12
              }
            }
            FlatButton {
              ui: root
              visible: root.selected !== ""
              label: root.copied ? "copied" : "copy"
              implicitHeight: 28
              fontSize: 11
              Accessible.name: "Copy command"
              onActivated: root.copyCommand()
            }
            Text {
              textFormat: Text.PlainText
              Layout.fillWidth: true
              visible: root.error !== ""
              text: root.error
              color: root.tokens.urgent
              font.family: root.font
              font.pixelSize: 12
              elide: Text.ElideRight
            }
            Item { Layout.fillWidth: root.error === "" }
            FlatButton {
              ui: root
              label: "revert"
              onActivated: root.revert()
            }
            FlatButton {
              ui: root
              label: saveProc.running ? "saving…" : "apply"
              primary: true
              onActivated: root.save()
            }
          }
        }
      }
    }
  }
}
