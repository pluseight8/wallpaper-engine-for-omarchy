import QtQuick
import Quickshell.Io
import qs.Commons as Commons
import qs.Ui

// Shows what omawed is doing. The daemon pushes a status line on every change
// (`omawe watch`), so nothing here runs on an interval while it is up.
BarWidget {
  id: root
  moduleName: "org.pluseight.omawe"

  // {} while the daemon is unreachable.
  property var status: ({})
  readonly property string state: status.state || "down"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  function run(command) {
    if (root.bar) root.bar.run(command)
  }

  Process {
    id: watcher
    command: ["omawe", "watch"]
    running: true
    stdout: SplitParser {
      onRead: line => {
        try { root.status = JSON.parse(line) } catch (e) {}
      }
    }
    onExited: {
      root.status = ({})
      retry.start()
    }
  }

  // Only ticks while the daemon is down.
  Timer {
    id: retry
    interval: 10000
    onTriggered: watcher.running = true
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.state === "paused" ? "󰏤" : root.state === "stopped" ? "󰓛" : "󰸉"
    dimmed: root.state !== "running"
    // The bar renders tooltips as plain text, so a wallpaper title cannot inject markup.
    tooltipText: {
      if (root.state === "down") return strings.t("down")
      if (root.state === "off") return strings.t("off")
      var label = strings.t(root.status.reason ? "reason." + root.status.reason : "state." + root.state)
      return (root.status.title || "") + " — " + label
    }

    onPressed: function(mouseButton) {
      if (mouseButton === Qt.LeftButton) root.run("omawe pick")
      else if (mouseButton === Qt.RightButton) root.run("omawe toggle")
      else if (mouseButton === Qt.MiddleButton) root.run("omawe random")
    }
    onWheelMoved: function(delta) {
      root.run(delta > 0 ? "omawe prev" : "omawe next")
    }
  }
}
