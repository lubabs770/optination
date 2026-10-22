import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

// The "browse online" tab: the pling / gnome-look cursor catalog, searched and
// installed through the engine (`--store`, `--install`). Installing puts the
// new theme straight on the pointer, unsaved, so trying it costs nothing —
// the status strip above says so, and Esc still goes back.
Item {
  id: store

  // The Picker: colour tokens, the engine path, and pick()/tryInstalled().
  required property var ui

  property var items: []
  property int page: 0
  property int total: 0
  property string query: ""
  property string sort: "downloads"
  property int current: -1
  property int shot: 0
  property bool loaded: false
  property bool appending: false
  property string error: ""
  // "id:n" of the download in flight.
  property string installing: ""
  // Catalog picture URL → cached PNG path ("" when it could not be shown).
  // Pictures are never loaded from the network here: the engine fetches,
  // checks and shrinks them (`--previews`), and only its PNG is shown.
  property var shots: ({})
  property var shotQueue: []
  // URLs already queued or fetched, so none is asked for twice.
  property var shotAsked: ({})

  readonly property var item: current >= 0 && current < items.length ? items[current] : null
  readonly property var installable: item ? item.files.filter(f => f.installable) : []
  readonly property int hiddenFiles: item ? item.files.length - installable.length : 0
  readonly property bool hasMore: items.length < total

  readonly property var sorts: [
    { id: "downloads", label: "most downloaded" },
    { id: "rating", label: "top rated" },
    { id: "new", label: "newest" },
    { id: "name", label: "a–z" }
  ]

  // ----------------------------------------------------------- behaviour ---

  function opened() {
    if (!loaded && !storeProc.running) fetch(false)
  }

  function fetch(append) {
    if (!ui.engine) return
    if (storeProc.running) {
      fetchPending = true
      return
    }
    error = ""
    appending = append
    const nextPage = append ? page + 1 : 0
    const cmd = [ui.engine, "--store"]
    const q = query.trim()
    if (q) cmd.push(q)
    cmd.push("--page", String(nextPage), "--sort", sort, "--json")
    storeProc.command = cmd
    storeProc.running = true
  }
  property bool fetchPending: false

  function select(i) {
    if (i < 0 || i >= items.length) return
    // Ask first: the placeholder reads the queue when `current` changes.
    wantPreviews(items[i].previews, true)
    current = i
    shot = 0
    results.positionViewAtIndex(i, ListView.Contain)
  }

  function preferredFile(it) {
    return it ? it.files.find(f => f.preferred) || it.files.find(f => f.installable) || null : null
  }

  function install(it, file) {
    if (!it || !file || installing) return
    if (file.installedAs) {
      ui.pick(file.installedAs)
      ui.notice = "trying " + file.installedAs
      return
    }
    error = ""
    installing = it.id + ":" + file.n
    installProc.command = [ui.engine, "--install", String(it.id), String(file.n), "--json"]
    installProc.running = true
  }

  function openPage(it) {
    if (it && /^https:\/\/www\.pling\.com\/p\/[0-9]+$/.test(it.page))
      Quickshell.execDetached(["xdg-open", it.page])
  }

  // Keys while this tab is showing; true when the key was used.
  function handleKey(event) {
    const key = event.key
    const text = event.text
    if (text === "/") {
      field.forceActiveFocus()
    } else if (text === "j" || key === Qt.Key_Down) {
      select(current + 1)
      if (current >= items.length - 3 && hasMore && !storeProc.running) fetch(true)
    } else if (text === "k" || key === Qt.Key_Up) {
      select(Math.max(0, current - 1))
    } else if (text === "l" || key === Qt.Key_Right) {
      if (item && item.previews.length) shot = (shot + 1) % item.previews.length
    } else if (text === "h" || key === Qt.Key_Left) {
      if (item && item.previews.length) shot = (shot + item.previews.length - 1) % item.previews.length
    } else if (key === Qt.Key_Return || key === Qt.Key_Enter) {
      install(item, preferredFile(item))
    } else if (text === "o") {
      openPage(item)
    } else {
      return false
    }
    return true
  }

  function kb(size) {
    return size >= 1024 ? (size / 1024).toFixed(1) + " MB" : size + " KB"
  }

  function count(n) {
    return n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? Math.round(n / 1e3) + "k" : String(n)
  }

  // Only pling's own image host, over https, is ever handed to the engine.
  function imageUrl(u) {
    return typeof u === "string" && u.startsWith("https://images.pling.com/") && u.length < 512
      && !/[\s"'<>\\]/.test(u) ? u : ""
  }

  // Queue pictures for the engine; `first` puts them ahead of the rest.
  function wantPreviews(urls, first) {
    const fresh = urls.map(imageUrl).filter(u => u && !shotAsked[u])
    if (!fresh.length) return
    for (const u of fresh) shotAsked[u] = true
    shotQueue = first ? fresh.concat(shotQueue) : shotQueue.concat(fresh)
    if (!previewProc.running) nextPreviews()
  }

  function nextPreviews() {
    if (!ui.engine || !shotQueue.length) return
    // Small batches, so pictures appear as they arrive.
    previewProc.batch = shotQueue.slice(0, 8)
    shotQueue = shotQueue.slice(8)
    previewProc.command = [ui.engine, "--previews"].concat(previewProc.batch, ["--json"])
    previewProc.running = true
  }

  // Asked for, and the engine has not answered yet.
  function previewPending(u) {
    const url = imageUrl(u)
    return url !== "" && shotAsked[url] === true && !(url in shots)
  }

  // The local file for a catalog picture, once the engine has made it.
  function previewSource(u) {
    const path = shots[imageUrl(u)]
    return path ? ui.fileUrl(path) : ""
  }

  function isItem(it) {
    return it && typeof it.id === "number" && typeof it.name === "string" && it.name.length < 200
      && typeof it.author === "string" && typeof it.summary === "string"
      && Array.isArray(it.previews) && Array.isArray(it.files) && Array.isArray(it.installed)
      && it.files.every(f => f && typeof f.n === "number" && typeof f.name === "string"
        && typeof f.installedAs === "string")
  }

  // -------------------------------------------------------------- engine ---

  Process {
    id: storeProc
    stdout: StdioCollector {
      onStreamFinished: {
        const page = store.ui.parse(text, {}, store.ui.maxListBytes)
        if (!Array.isArray(page.items)) return
        const rows = page.items.filter(store.isItem).slice(0, 200)
        store.total = typeof page.total === "number" ? page.total : rows.length
        store.page = typeof page.page === "number" ? page.page : 0
        store.items = store.appending ? store.items.concat(rows) : rows
        store.loaded = true
        if (!store.appending) store.select(rows.length ? 0 : -1)
        store.wantPreviews(rows.filter(r => r.previews.length).map(r => r.previews[0]), false)
      }
    }
    stderr: StdioCollector {
      onStreamFinished: if (text.trim()) store.error = store.ui.oneLine(text.replace(/^optination: /, ""))
    }
    onExited: {
      if (store.fetchPending) {
        store.fetchPending = false
        store.fetch(false)
      }
    }
  }

  Process {
    id: installProc
    stdout: StdioCollector { id: installOut }
    stderr: StdioCollector { id: installErr }
    onExited: function(code) {
      const done = code === 0 ? store.ui.parse(installOut.text, {}, store.ui.maxSmallBytes) : {}
      const names = Array.isArray(done.installed) ? done.installed.filter(store.ui.isSafeTheme) : []
      const [id, n] = store.installing.split(":").map(Number)
      store.installing = ""
      if (code !== 0 || names.length === 0) {
        store.error = store.ui.oneLine(installErr.text.replace(/^optination: /, "")) || "installing failed"
        return
      }
      // Mark the row and file installed without another round trip.
      store.items = store.items.map(it => {
        if (it.id !== id) return it
        const copy = Object.assign({}, it)
        copy.installed = it.installed.concat(names.filter(x => !it.installed.includes(x)))
        copy.files = it.files.map(f => f.n === n ? Object.assign({}, f, { installedAs: names[0] }) : f)
        return copy
      })
      store.ui.tryInstalled(names[0])
    }
  }

  Process {
    id: previewProc
    property var batch: []
    stdout: StdioCollector { id: previewOut }
    onExited: {
      const rows = store.ui.parse(previewOut.text, [], store.ui.maxSmallBytes)
      const next = Object.assign({}, store.shots)
      for (const u of batch) next[u] = ""
      for (const r of rows) {
        if (r && typeof r.url === "string" && batch.includes(r.url) && typeof r.path === "string")
          next[r.url] = r.path
      }
      store.shots = next
      store.nextPreviews()
    }
  }

  Timer { id: searchDebounce; interval: 350; onTriggered: store.fetch(false) }

  // -------------------------------------------------------------- layout ---

  RowLayout {
    anchors.fill: parent
    spacing: 0

    // ---------------------------------------------------- results ---
    ColumnLayout {
      Layout.preferredWidth: store.ui.listWidth
      Layout.fillHeight: true
      spacing: 12

      Rectangle {
        Layout.fillWidth: true
        Layout.topMargin: 16
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        Layout.preferredHeight: 40
        color: store.ui.faint
        border.width: 1
        border.color: field.activeFocus ? store.ui.tint(store.ui.fg, 0.6) : store.ui.outline
        radius: store.ui.tokens.radius

        RowLayout {
          anchors.fill: parent
          anchors.leftMargin: 12
          anchors.rightMargin: 12
          spacing: 10
          Text {
            textFormat: Text.PlainText
            text: "›"
            color: store.ui.tokens.accent
            font.family: store.ui.font
            font.pixelSize: 15
            font.bold: true
          }
          TextInput {
            id: field
            Layout.fillWidth: true
            color: store.ui.fg
            selectionColor: store.ui.tint(store.ui.tokens.accent, 0.35)
            font.family: store.ui.font
            font.pixelSize: 13
            clip: true
            maximumLength: 100
            text: store.query
            onTextEdited: {
              store.query = text
              searchDebounce.restart()
            }
            Accessible.name: "Search the online cursor catalog"
            Keys.onPressed: function(event) {
              if (event.key === Qt.Key_Escape) {
                if (store.query) {
                  store.query = ""
                  store.fetch(false)
                } else {
                  store.ui.focusKeys()
                }
              } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                searchDebounce.stop()
                store.fetch(false)
                store.ui.focusKeys()
              } else if (event.key === Qt.Key_Down) {
                store.select(store.current + 1)
              } else if (event.key === Qt.Key_Up) {
                store.select(Math.max(0, store.current - 1))
              } else {
                return
              }
              event.accepted = true
            }
            Text {
              textFormat: Text.PlainText
              anchors.verticalCenter: parent.verticalCenter
              visible: !field.text
              text: "search pling / gnome-look…"
              color: store.ui.tokens.muted
              font: field.font
            }
            MouseArea {
              anchors.fill: parent
              cursorShape: Qt.IBeamCursor
              onPressed: function(mouse) {
                field.forceActiveFocus()
                mouse.accepted = false
              }
            }
          }
          Text {
            textFormat: Text.PlainText
            text: storeProc.running && !store.appending ? "searching…"
              : store.loaded ? store.total + (store.total === 1 ? " theme" : " themes") : ""
            color: store.ui.tokens.muted
            font.family: store.ui.font
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
          model: store.sorts
          delegate: FlatButton {
            required property var modelData
            ui: store.ui
            label: modelData.label
            checkable: true
            checked: store.sort === modelData.id
            implicitHeight: 28
            fontSize: 12
            onActivated: {
              store.sort = modelData.id
              store.fetch(false)
              store.ui.focusKeys()
            }
          }
        }
      }

      ListView {
        id: results
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        Layout.bottomMargin: 16
        clip: true
        spacing: 2
        model: store.items
        boundsBehavior: Flickable.StopAtBounds
        onAtYEndChanged: if (atYEnd && store.hasMore && store.loaded && !storeProc.running) store.fetch(true)

        delegate: Rectangle {
          id: hit
          required property var modelData
          required property int index
          readonly property bool current: index === store.current
          width: ListView.view.width
          height: 68
          color: current ? store.ui.tokens.selectedBackground
            : hitMouse.containsMouse ? store.ui.tint(store.ui.fg, 0.04) : "transparent"
          border.width: 1
          border.color: current ? store.ui.tint(store.ui.fg, 0.25) : "transparent"
          radius: store.ui.tokens.radius

          RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 10
            spacing: 12

            Rectangle {
              Layout.preferredWidth: 80
              Layout.preferredHeight: 54
              color: store.ui.deep
              border.width: 1
              border.color: store.ui.line
              radius: store.ui.tokens.radius
              clip: true
              Image {
                anchors.fill: parent
                anchors.margins: 1
                source: store.previewSource(hit.modelData.previews[0])
                fillMode: Image.PreserveAspectCrop
                sourceSize.width: 160
                asynchronous: true
                smooth: true
              }
            }

            ColumnLayout {
              Layout.fillWidth: true
              spacing: 3
              Text {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: hit.modelData.name
                color: hit.current ? store.ui.tokens.selectedText : store.ui.fg
                font.family: store.ui.font
                font.pixelSize: 13
                elide: Text.ElideRight
              }
              Text {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: "by " + hit.modelData.author + " · " + store.count(hit.modelData.downloads)
                  + " downloads · " + hit.modelData.score + "%"
                color: store.ui.tokens.muted
                font.family: store.ui.font
                font.pixelSize: 11
                elide: Text.ElideRight
              }
            }

            Badge {
              ui: store.ui
              visible: hit.modelData.installed.length > 0
              small: true
              label: "installed"
            }
          }

          MouseArea {
            id: hitMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
              store.select(hit.index)
              store.ui.focusKeys()
            }
          }
        }

        footer: Item {
          width: ListView.view.width
          height: store.hasMore || store.appending ? 44 : 0
          Text {
            textFormat: Text.PlainText
            anchors.centerIn: parent
            visible: parent.height > 0
            text: storeProc.running ? "loading more…" : "scroll for more"
            color: store.ui.tokens.muted
            font.family: store.ui.font
            font.pixelSize: 11
          }
        }

        Text {
          textFormat: Text.PlainText
          anchors.centerIn: parent
          width: parent.width - 20
          horizontalAlignment: Text.AlignHCenter
          wrapMode: Text.WordWrap
          visible: store.items.length === 0 && !storeProc.running
          text: store.error ? store.error
            : store.loaded ? "nothing in the catalog matches “" + store.query + "”"
            : ""
          color: store.error ? store.ui.tokens.urgent : store.ui.tokens.muted
          font.family: store.ui.font
          font.pixelSize: 13
        }
      }
    }

    Rectangle { Layout.preferredWidth: 1; Layout.fillHeight: true; color: store.ui.line }

    // ----------------------------------------------------- detail ---
    ColumnLayout {
      Layout.fillWidth: true
      Layout.fillHeight: true
      Layout.margins: 20
      Layout.leftMargin: 24
      Layout.rightMargin: 24
      spacing: 16

      Rectangle {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.minimumHeight: 200
        color: store.ui.deep
        border.width: 1
        border.color: store.ui.line
        radius: store.ui.tokens.radius
        clip: true

        Image {
          id: shotImage
          anchors.fill: parent
          anchors.margins: 12
          source: store.item ? store.previewSource(store.item.previews[store.shot]) : ""
          fillMode: Image.PreserveAspectFit
          asynchronous: true
          smooth: true
          mipmap: true
        }
        Text {
          textFormat: Text.PlainText
          anchors.centerIn: parent
          visible: shotImage.status !== Image.Ready
          text: !store.item ? (storeProc.running ? "searching the catalog…" : "")
            : shotImage.status === Image.Loading || store.previewPending(store.item.previews[store.shot])
              ? "loading preview…"
            : "no preview picture"
          color: store.ui.tokens.muted
          font.family: store.ui.font
          font.pixelSize: 13
        }

        Row {
          anchors.bottom: parent.bottom
          anchors.horizontalCenter: parent.horizontalCenter
          anchors.bottomMargin: 10
          spacing: 6
          visible: store.item !== null && store.item.previews.length > 1
          Repeater {
            model: store.item ? store.item.previews.length : 0
            delegate: Rectangle {
              required property int index
              width: 22
              height: 6
              color: index === store.shot ? store.ui.tokens.accent : store.ui.tint(store.ui.fg, 0.25)
              MouseArea {
                anchors.fill: parent
                anchors.margins: -6
                cursorShape: Qt.PointingHandCursor
                onClicked: store.shot = parent.index
              }
            }
          }
        }
      }

      // Title and facts.
      ColumnLayout {
        Layout.fillWidth: true
        visible: store.item !== null
        spacing: 6

        RowLayout {
          Layout.fillWidth: true
          spacing: 12
          Text {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: store.item ? store.item.name : ""
            color: store.ui.fg
            font.family: store.ui.font
            font.pixelSize: 16
            font.bold: true
            elide: Text.ElideRight
          }
          FlatButton {
            ui: store.ui
            label: "open on pling.com ↗"
            implicitHeight: 28
            fontSize: 11
            onActivated: store.openPage(store.item)
          }
        }
        Text {
          textFormat: Text.PlainText
          Layout.fillWidth: true
          text: store.item ? "by " + store.item.author + " · " + store.count(store.item.downloads)
            + " downloads · rated " + store.item.score + "% · updated " + store.item.changed : ""
          color: store.ui.tokens.muted
          font.family: store.ui.font
          font.pixelSize: 12
          elide: Text.ElideRight
        }
        Text {
          textFormat: Text.PlainText
          Layout.fillWidth: true
          visible: text !== ""
          text: store.item && store.item.summary !== store.item.name ? store.item.summary : ""
          color: store.ui.fg
          font.family: store.ui.font
          font.pixelSize: 12
          wrapMode: Text.WordWrap
          maximumLineCount: 2
          elide: Text.ElideRight
        }
      }

      // Downloads that can become a Linux cursor theme.
      ColumnLayout {
        Layout.fillWidth: true
        visible: store.item !== null
        spacing: 6

        RowLayout {
          Layout.fillWidth: true
          spacing: 12
          Text {
            textFormat: Text.PlainText
            text: "VARIANTS"
            color: store.ui.tokens.accent
            font.family: store.ui.font
            font.pixelSize: 11
            font.letterSpacing: 1.3
          }
          Text {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: store.hiddenFiles > 0
              ? store.hiddenFiles + " Windows / macOS / image-only download" + (store.hiddenFiles === 1 ? "" : "s") + " hidden"
              : ""
            color: store.ui.tokens.muted
            font.family: store.ui.font
            font.pixelSize: 11
            elide: Text.ElideRight
          }
          Text {
            textFormat: Text.PlainText
            text: "installs to ~/.local/share/icons"
            color: store.ui.tokens.muted
            font.family: store.ui.font
            font.pixelSize: 11
          }
        }

        Text {
          textFormat: Text.PlainText
          visible: store.item !== null && store.installable.length === 0
          text: "no Linux cursor archive here — try “open on pling.com”"
          color: store.ui.tokens.muted
          font.family: store.ui.font
          font.pixelSize: 12
        }

        ListView {
          id: variants
          Layout.fillWidth: true
          Layout.preferredHeight: Math.min(contentHeight, 4 * 40)
          visible: store.installable.length > 0
          clip: true
          spacing: 4
          model: store.installable
          boundsBehavior: Flickable.StopAtBounds
          delegate: Rectangle {
            id: variant
            required property var modelData
            readonly property bool busy: store.installing === (store.item ? store.item.id + ":" + modelData.n : "")
            readonly property bool live: modelData.installedAs !== "" && modelData.installedAs === store.ui.liveTheme
            width: ListView.view.width
            height: 36
            color: modelData.preferred ? store.ui.tint(store.ui.fg, 0.04) : "transparent"
            border.width: 1
            border.color: store.ui.line
            radius: store.ui.tokens.radius

            RowLayout {
              anchors.fill: parent
              anchors.leftMargin: 12
              anchors.rightMargin: 4
              spacing: 10
              Text {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: variant.modelData.name
                color: store.ui.fg
                font.family: store.ui.font
                font.pixelSize: 12
                elide: Text.ElideMiddle
              }
              Text {
                textFormat: Text.PlainText
                visible: variant.modelData.preferred && variant.modelData.installedAs === ""
                text: "enter"
                color: store.ui.tokens.muted
                font.family: store.ui.font
                font.pixelSize: 10
              }
              Text {
                textFormat: Text.PlainText
                text: store.kb(variant.modelData.sizeKb)
                color: store.ui.tokens.muted
                font.family: store.ui.font
                font.pixelSize: 11
              }
              Badge {
                ui: store.ui
                visible: variant.live
                small: true
                label: "live"
              }
              FlatButton {
                ui: store.ui
                implicitHeight: 28
                implicitWidth: 112
                fontSize: 11
                primary: variant.modelData.installedAs === "" && !variant.busy
                busy: store.installing !== "" && !variant.busy
                label: variant.busy ? "installing…"
                  : variant.modelData.installedAs === "" ? "install"
                  : variant.live ? "on pointer" : "try it"
                onActivated: store.install(store.item, variant.modelData)
              }
            }
          }
        }
      }

      Text {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: store.error !== "" && store.items.length > 0
        text: store.error
        color: store.ui.tokens.urgent
        font.family: store.ui.font
        font.pixelSize: 12
        wrapMode: Text.WordWrap
      }
    }
  }
}
