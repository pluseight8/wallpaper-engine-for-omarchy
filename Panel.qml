import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// Library browser and property editor, opened from the bar widget.
//
// It owns no state of its own: every action is an `omawe` call and the daemon
// stays the single source of truth. Wallpaper titles and property labels come
// from Workshop authors, so every Text showing them is PlainText.
Panel {
  id: root
  moduleName: "org.pluseight.omawe"
  ipcTarget: "org.pluseight.omawe"
  manageIpc: false

  // Injected by BarWidget.qml.
  property var anchorItem: null
  property var hostWidget: null
  property var strings: null
  property var status: ({})
  readonly property var barIdentity: hostWidget || root

  property var wallpapers: []
  property var config: ({})
  property string query: ""
  property string kind: ""
  property bool favOnly: false
  property int cursor: 0
  // "*" = all monitors, otherwise a monitor name.
  property string monitor: "*"
  property string view: "library"
  property var props: []
  property var presets: []
  // A text field has focus: keys go to it instead of driving the grid.
  property bool editing: false

  readonly property int columns: 4
  readonly property color fg: bar ? bar.foreground : Commons.Color.foreground
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property var monitorArgs: monitor === "*" ? [] : ["--monitor", monitor]
  // The wallpaper the editor works on: the one on the chosen monitor.
  readonly property string currentId: {
    var mons = status.monitors || {}
    if (monitor !== "*" && mons[monitor]) return mons[monitor].id
    return status.id || ""
  }
  readonly property var filtered: {
    var q = query.toLowerCase()
    return wallpapers.filter(function(w) {
      return w.compat !== "unsupported"
        && (kind === "" || w.kind === kind)
        && (!favOnly || w.fav)
        && (q === "" || w.title.toLowerCase().indexOf(q) !== -1)
    })
  }

  function tr(key) { return strings ? strings.t(key) : key }

  // ---- Daemon calls: one short-lived process each, reply handed to `done`.
  Component {
    id: call
    Process {
      id: p
      property var done: null
      running: true
      stdout: StdioCollector {
        onStreamFinished: {
          if (p.done) p.done(text)
          p.destroy()
        }
      }
    }
  }

  function omawe(args, done) {
    call.createObject(root, { command: ["omawe"].concat(args), done: done || null })
  }

  function parse(text) {
    try { return JSON.parse(text).data } catch (e) { return null }
  }

  function reload() {
    omawe(["list", "--json", "--thumbs"], function(t) { root.wallpapers = root.parse(t) || [] })
    omawe(["config", "--json"], function(t) { root.config = (root.parse(t) || {}).config || {} })
  }

  function loadProps() {
    if (currentId === "") { props = []; presets = []; return }
    omawe(["props", currentId, "--json"].concat(monitorArgs), function(t) {
      var d = root.parse(t) || {}
      root.props = d.props || []
      root.presets = d.presets || []
    })
  }

  function apply(w) {
    if (w) omawe(["apply", w.id].concat(monitorArgs))
  }

  function toggleFav(w) {
    omawe(["fav", w.id], function() { root.reload() })
  }

  function setProp(key, value) {
    omawe(["prop", currentId, key, String(value)].concat(monitorArgs))
  }

  function preset(action, name) {
    var args = ["preset", action, currentId]
    if (name) args.push(name)
    omawe(args.concat(monitorArgs), function() { root.loadProps() })
  }

  function setConfig(key, value) {
    omawe(["set", key, String(value), "--json"], function(t) { root.config = root.parse(t) || root.config })
  }

  // ---- Keyboard
  function move(dx, dy) {
    var next = cursor + dx + dy * columns
    if (next >= 0 && next < filtered.length) cursor = next
  }

  function type(text) {
    if (view !== "library") return
    if (text === "\b") query = query.slice(0, -1)
    else if (text >= " ") query += text
    cursor = 0
  }

  // ---- Property helpers
  // Authors put HTML in labels; show the words, not the markup.
  function label(p) {
    var t = String(p.text || "").replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim()
    return t === "" || t.indexOf("ui_") === 0 ? p.key : t
  }

  function hex(value) {
    var c = String(value).split(" ").map(function(v) {
      var n = Math.max(0, Math.min(255, Math.round(parseFloat(v) * 255)))
      return (n < 16 ? "0" : "") + n.toString(16)
    })
    return c.length === 3 ? "#" + c.join("") : "#000000"
  }

  function unhex(text) {
    var m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(text.trim())
    return m ? [1, 2, 3].map(function(i) { return (parseInt(m[i], 16) / 255).toFixed(4) }).join(" ") : ""
  }

  onCurrentIdChanged: if (opened && view === "props") loadProps()
  onMonitorChanged: if (view === "props") loadProps()
  onViewChanged: if (view === "props") loadProps()

  function open() {
    view = "library"
    query = ""
    reload()
    root.controller.show()
  }

  function close() {
    root.controller.hide()
  }

  function toggle() {
    if (root.opened) root.close()
    else root.open()
  }

  function switchPanel(direction) {
    if (root.bar && typeof root.bar.switchPanelFrom === "function")
      return root.bar.switchPanelFrom(root.barIdentity, direction)
    return false
  }

  component Label: Text {
    color: root.fg
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
    textFormat: Text.PlainText
    elide: Text.ElideRight
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    centerOnBar: false
    focusTarget: panel.loadedContent
    contentWidth: panel.fittedContentWidth(Style.space(780))
    contentHeight: panel.fittedContentHeight(Style.space(600))

    content: PanelKeyCatcher {
      id: keys
      anchors.fill: parent
      implicitHeight: Style.space(600)
      blocked: root.editing
      onMoveRequested: function(dx, dy) { root.move(dx, dy) }
      onActivateRequested: root.apply(root.filtered[root.cursor])
      onCloseRequested: root.view === "props" ? root.view = "library" : root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      // The catcher turns "x" into a delete request; here it is just a letter.
      onDeleteRequested: root.type("x")
      onTextKey: function(text) { root.type(text) }

      Column {
        anchors.fill: parent
        spacing: Style.spacing.sm

        // ---- Toolbar
        Row {
          id: toolbar
          width: parent.width
          height: Style.spacing.controlHeight
          spacing: Style.spacing.sm

          Label {
            width: parent.width - typeBox.width - monitorBox.width - buttons.width - parent.spacing * 3
            anchors.verticalCenter: parent.verticalCenter
            opacity: root.query === "" ? 0.5 : 1
            text: root.view === "props"
              ? ((root.status.monitors || {})[root.monitor] || root.status).title || root.tr("panel.none")
              : root.query !== "" ? "󰍉  " + root.query
              : "󰍉  " + root.tr("panel.search")
          }

          Dropdown {
            id: typeBox
            width: Style.space(130)
            showLabel: false
            visible: root.view === "library"
            value: root.kind
            options: [
              { value: "", label: root.tr("panel.all") },
              { value: "scene", label: "Scene" },
              { value: "video", label: "Video" },
              { value: "web", label: "Web" },
              { value: "preset", label: "Preset" }
            ]
            onChanged: function(value) { root.kind = value; root.cursor = 0 }
          }

          Dropdown {
            id: monitorBox
            width: Style.space(150)
            showLabel: false
            value: root.monitor
            options: [{ value: "*", label: root.tr("panel.allMonitors") }].concat(
              Object.keys(root.status.monitors || {}).map(function(m) { return { value: m, label: m } }))
            onChanged: function(value) { root.monitor = value }
          }

          Row {
            id: buttons
            spacing: Style.spacing.sm
            Button {
              iconText: "󰓎"
              selected: root.favOnly
              visible: root.view === "library"
              tooltipText: root.tr("panel.favourites")
              onClicked: { root.favOnly = !root.favOnly; root.cursor = 0 }
            }
            Button {
              iconText: "󰒟"
              tooltipText: root.tr("panel.random")
              onClicked: root.omawe(["random"].concat(root.monitorArgs))
            }
            Button {
              iconText: root.status.state === "paused" ? "󰐊" : "󰏤"
              tooltipText: root.tr("panel.pause")
              onClicked: root.omawe(["toggle"])
            }
            Button {
              text: root.view === "props" ? root.tr("panel.library") : root.tr("panel.properties")
              selected: root.view === "props"
              onClicked: root.view = root.view === "props" ? "library" : "props"
            }
          }
        }

        // ---- Library
        GridView {
          id: grid
          visible: root.view === "library"
          width: parent.width
          height: parent.height - toolbar.height - footer.height - parent.spacing * 2
          clip: true
          cellWidth: Math.floor(width / root.columns)
          cellHeight: Math.round(cellWidth * 9 / 16) + Style.space(22)
          model: root.filtered
          currentIndex: root.cursor
          onCurrentIndexChanged: positionViewAtIndex(currentIndex, GridView.Contain)

          delegate: Item {
            id: cell
            required property var modelData
            required property int index
            readonly property bool current: modelData.id === root.currentId
            width: grid.cellWidth
            height: grid.cellHeight

            Rectangle {
              id: frame
              anchors.fill: parent
              anchors.margins: Style.space(3)
              radius: Style.cornerRadius
              color: cell.index === root.cursor ? Style.selectedFillFor(root.fg, Commons.Color.accent) : "transparent"
              border.width: cell.current ? Math.max(2, Style.space(2)) : 0
              border.color: Commons.Color.accent

              Image {
                id: picture
                anchors { left: parent.left; right: parent.right; top: parent.top; margins: Style.space(4) }
                height: Math.round(width * 9 / 16)
                // The cached thumbnail when there is one, the author's preview until then.
                source: "file://" + (cell.modelData.thumb || cell.modelData.preview)
                sourceSize: Qt.size(480, 270)
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                cache: false
                clip: true
              }

              Label {
                anchors { left: parent.left; right: parent.right; bottom: parent.bottom; margins: Style.space(4) }
                font.pixelSize: Style.font.caption
                text: (cell.modelData.fav ? "󰓎 " : "") + (cell.modelData.compat === "crash" ? "󰀦 " : "") + cell.modelData.title
              }
            }

            MouseArea {
              anchors.fill: parent
              acceptedButtons: Qt.LeftButton | Qt.RightButton
              onClicked: function(mouse) {
                root.cursor = cell.index
                if (mouse.button === Qt.RightButton) root.toggleFav(cell.modelData)
                else root.apply(cell.modelData)
              }
            }
          }
        }

        // ---- Property editor
        ListView {
          id: editor
          visible: root.view === "props"
          width: parent.width
          height: parent.height - toolbar.height - footer.height - parent.spacing * 2
          clip: true
          spacing: Style.spacing.sm
          model: root.props

          header: Column {
            width: editor.width
            spacing: Style.spacing.sm
            bottomPadding: Style.spacing.sm

            Row {
              spacing: Style.spacing.sm
              height: Style.spacing.controlHeight
              TextField {
                id: presetName
                width: Style.space(200)
                placeholderText: root.tr("panel.presetName")
                onActiveFocusChanged: root.editing = activeFocus
                Keys.onEscapePressed: keys.forceActiveFocus()
                onAccepted: saveButton.clicked()
              }
              Button {
                id: saveButton
                text: root.tr("panel.save")
                enabled: presetName.text.trim() !== ""
                onClicked: { root.preset("save", presetName.text.trim()); presetName.text = ""; keys.forceActiveFocus() }
              }
              Dropdown {
                width: Style.space(200)
                showLabel: false
                visible: root.presets.length > 0
                value: ""
                options: [{ value: "", label: root.tr("panel.presets") }].concat(
                  root.presets.map(function(name) { return { value: name, label: name } }))
                onChanged: function(value) { if (value !== "") root.preset("load", value) }
              }
              Button {
                text: root.tr("panel.reset")
                onClicked: root.preset("reset")
              }
            }

            Label {
              width: parent.width
              opacity: 0.6
              font.pixelSize: Style.font.caption
              text: root.props.length === 0 ? root.tr("panel.noProps") : root.tr("panel.restart")
            }
          }

          delegate: Item {
            id: row
            required property var modelData
            readonly property string kind: modelData.type
            readonly property bool heading: kind === "group" || kind === "text"
            width: editor.width
            height: Math.max(name.implicitHeight, control.height)

            Label {
              id: name
              anchors.verticalCenter: parent.verticalCenter
              width: row.heading ? parent.width : parent.width * 0.45
              wrapMode: row.heading ? Text.Wrap : Text.NoWrap
              opacity: row.heading ? 0.6 : 1
              font.bold: row.kind === "group"
              text: root.label(row.modelData)
            }

            Loader {
              id: control
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              width: parent.width * 0.5
              sourceComponent: row.kind === "slider" ? slider
                : row.kind === "bool" ? toggle
                : row.kind === "combo" ? combo
                : row.kind === "color" ? colour
                : row.kind === "textinput" ? input
                : null
            }

            Component {
              id: slider
              Row {
                spacing: Style.spacing.sm
                height: Style.spacing.controlHeight
                PanelSlider {
                  id: track
                  width: parent.width - shown.width - parent.spacing
                  anchors.verticalCenter: parent.verticalCenter
                  bar: root.bar
                  minimum: Number(row.modelData.min) || 0
                  maximum: row.modelData.max === null ? 1 : Number(row.modelData.max)
                  step: Number(row.modelData.step) || (maximum - minimum) / 100
                  value: Number(row.modelData.value)
                  onReleased: function(value) { root.setProp(row.modelData.key, value) }
                }
                Label {
                  id: shown
                  width: Style.space(48)
                  anchors.verticalCenter: parent.verticalCenter
                  horizontalAlignment: Text.AlignRight
                  text: Commons.I18n.formatNumber(Math.round(track.liveValue * 100) / 100)
                }
              }
            }

            Component {
              id: toggle
              Item {
                height: Style.spacing.controlHeight
                ToggleSwitch {
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  checked: row.modelData.value === "1" || row.modelData.value === "true"
                  onToggled: { checked = !checked; root.setProp(row.modelData.key, checked ? 1 : 0) }
                }
              }
            }

            Component {
              id: combo
              Dropdown {
                showLabel: false
                value: row.modelData.value
                options: (row.modelData.options || []).map(function(o) {
                  return { value: String(o.value), label: String(o.label).replace(/<[^>]*>/g, "") }
                })
                onChanged: function(value) { root.setProp(row.modelData.key, value) }
              }
            }

            Component {
              id: colour
              Row {
                spacing: Style.spacing.sm
                height: Style.spacing.controlHeight
                Rectangle {
                  width: height
                  height: parent.height
                  radius: Style.cornerRadius
                  color: field.acceptableColor || "#000000"
                  border.width: 1
                  border.color: Style.normalBorderFor(root.fg, Commons.Color.accent)
                }
                TextField {
                  id: field
                  // Only a complete #rrggbb is a colour; anything else keeps the last good swatch.
                  readonly property string acceptableColor: root.unhex(text) !== "" ? (text.trim().indexOf("#") === 0 ? text.trim() : "#" + text.trim()) : ""
                  width: parent.width - parent.height - parent.spacing
                  text: root.hex(row.modelData.value)
                  onActiveFocusChanged: root.editing = activeFocus
                  Keys.onEscapePressed: keys.forceActiveFocus()
                  onAccepted: {
                    var value = root.unhex(text)
                    if (value !== "") root.setProp(row.modelData.key, value)
                    keys.forceActiveFocus()
                  }
                }
              }
            }

            Component {
              id: input
              TextField {
                text: row.modelData.value
                onActiveFocusChanged: root.editing = activeFocus
                Keys.onEscapePressed: keys.forceActiveFocus()
                onAccepted: { root.setProp(row.modelData.key, text); keys.forceActiveFocus() }
              }
            }
          }
        }

        // ---- Sound and summary
        Row {
          id: footer
          width: parent.width
          height: Style.spacing.controlHeight
          spacing: Style.spacing.md

          ToggleSwitch {
            anchors.verticalCenter: parent.verticalCenter
            checked: root.config.silent === false
            onToggled: root.setConfig("silent", root.config.silent === false)
          }
          Label {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tr("panel.sound")
          }
          PanelSlider {
            width: Style.space(140)
            anchors.verticalCenter: parent.verticalCenter
            bar: root.bar
            minimum: 0
            maximum: 100
            step: 5
            integer: true
            enabled: root.config.silent === false
            opacity: enabled ? 1 : 0.4
            value: root.config.volume || 0
            onReleased: function(value) { root.setConfig("volume", Math.round(value)) }
          }
          ToggleSwitch {
            anchors.verticalCenter: parent.verticalCenter
            checked: root.config.audio_processing === true
            onToggled: root.setConfig("audio_processing", root.config.audio_processing !== true)
          }
          Label {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tr("panel.reactive")
          }
          ToggleSwitch {
            anchors.verticalCenter: parent.verticalCenter
            checked: root.config.automute === true
            onToggled: root.setConfig("automute", root.config.automute !== true)
          }
          Label {
            anchors.verticalCenter: parent.verticalCenter
            text: root.tr("panel.automute")
          }
        }
      }
    }
  }
}
