import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

// One bar icon and one panel: Panel, BarIconButton, KeyboardPanel, Service.
// Hero plus SESSION / PAIRING / DEVICES / DISPLAY in one scrolling column.
// Service.qml is the only spawn site — ctl, never HTTPS, never the token.
Panel {
  id: root
  moduleName: "punktfunk"
  manageIpc: true
  ipcTarget: "punktfunk"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color accent: bar && bar.accent ? bar.accent : Color.accent
  readonly property color hoverFill: bar ? Style.hoverFillFor(bar.foreground, Color.accent) : "transparent"
  readonly property color selectedFill: bar ? Style.selectedFillFor(bar.foreground, Color.accent) : "transparent"

  readonly property bool needsYou: service.pending > 0 || service.pinPending
  readonly property bool live: Model.sessionLive(service.state, service.sessions, service.games)
  readonly property bool showArm: Model.showArmRow(service.state)
  readonly property bool showPairing: Model.sectionVisible("pairing", { state: service.state, needsYou: needsYou })
  readonly property var sessionActionModel: Model.sessionActions(live, service.games.length > 0)
  readonly property var pairingModel: Model.pairingRows(showArm, service.pinPending, service.pendingDevices)
  readonly property var devices: service.nativeClients.concat(service.gamestreamClients)
  readonly property var presetList: Model.allPresets(service.displayPresets, service.customPresets)
  readonly property var displayModel: Model.displayRows(presetList)
  readonly property var facts: Model.sessionFacts({
    state: service.state,
    sessions: service.sessions,
    games: service.games,
    stream: service.stream,
    armed: service.armed,
    summary: service.summary
  })
  readonly property var spark: Model.sparkPoints(service.history, "target")

  property string focusSection: "header"
  property int selectedIndex: 0
  property bool cursorActive: false
  property bool actionFocused: false
  property int phraseIndex: 0
  property var pinFieldRef: null
  readonly property bool headerHasCursor: cursorActive && focusSection === "header"
  readonly property string heroPhraseText: Model.heroPhrase(phraseIndex)
  readonly property string toggleHint: service.hostEnabled ? "Stop the host" : "Start the host"

  readonly property color glyphColor: {
    if (service.pinMismatch || needsYou) return urgent
    return service.state === "stopped" ? Qt.darker(barForeground, 1.55) : barForeground
  }

  readonly property var cursorSections: {
    var out = []
    if (sessionActionModel.length > 0) out.push("session")
    if (pairingModel.length > 0) out.push("pairing")
    if (devices.length > 0) out.push("devices")
    if (displayModel.length > 0) out.push("display")
    return out
  }

  function sectionCount(section) {
    if (section === "session") return sessionActionModel.length
    if (section === "pairing") return pairingModel.length
    if (section === "devices") return devices.length
    if (section === "display") return displayModel.length
    return 0
  }

  Service { id: service }

  onOpenedChanged: if (opened) {
    cursorActive = false
    actionFocused = false
    if (panelFlick) panelFlick.contentY = 0
    if (needsYou) { focusSection = "pairing"; selectedIndex = 0 }
    else { focusSection = "header"; selectedIndex = 0 }
    service.refresh()
    service.refreshClients()
    service.refreshDisplays()
    Qt.callLater(function() { keyCatcher.forceActiveFocus() })
  }

  function ensureCursor() {
    if (focusSection === "header") return
    var sections = cursorSections
    if (!sections.length) { focusSection = "header"; return }
    if (sections.indexOf(focusSection) < 0) {
      focusSection = sections[0]
      selectedIndex = 0
      actionFocused = false
      return
    }
    var n = sectionCount(focusSection)
    if (n <= 0) { focusSection = "header"; return }
    if (selectedIndex > n - 1) selectedIndex = n - 1
    if (selectedIndex < 0) selectedIndex = 0
  }

  function moveCursor(delta) {
    var sections = cursorSections
    if (focusSection === "header") {
      if (delta > 0 && sections.length > 0) {
        focusSection = sections[0]
        selectedIndex = 0
        actionFocused = false
      }
      return
    }
    if (!sections.length) { focusSection = "header"; return }
    var sIdx = sections.indexOf(focusSection)
    if (sIdx < 0) { focusSection = sections[0]; selectedIndex = 0; return }
    var idx = selectedIndex
    var max = sectionCount(focusSection) - 1
    if (delta > 0) {
      if (idx < max) { selectedIndex = idx + 1; actionFocused = false; return }
      if (sIdx < sections.length - 1) {
        focusSection = sections[sIdx + 1]
        selectedIndex = 0
        actionFocused = false
      }
    } else {
      if (idx > 0) { selectedIndex = idx - 1; actionFocused = false; return }
      if (sIdx > 0) {
        focusSection = sections[sIdx - 1]
        selectedIndex = sectionCount(focusSection) - 1
        actionFocused = false
      } else {
        focusSection = "header"
        actionFocused = false
      }
    }
  }

  function moveCursorH(delta) {
    if (focusSection !== "devices" && focusSection !== "pairing") return
    if (delta > 0) actionFocused = true
    else if (delta < 0) actionFocused = false
  }

  function activateCursor() {
    if (focusSection === "header") {
      service.setHostEnabled(!service.hostEnabled)
      settle.restart()
      return
    }
    if (focusSection === "session") {
      var act = sessionActionModel[selectedIndex]
      if (!act) return
      service.run([act.id === "end" ? "end-game" : "stop-session"], function() { service.refresh() })
      return
    }
    if (focusSection === "pairing") {
      var row = pairingModel[selectedIndex]
      if (!row) return
      if (row.kind === "arm") {
        service.run(service.armed ? ["pair", "disarm"] : ["pair", "arm"], function() { service.refresh() })
      } else if (row.kind === "pin") {
        if (root.pinFieldRef) root.pinFieldRef.forceActiveFocus()
      } else if (row.kind === "pending") {
        if (actionFocused)
          service.run(["deny", String(row.device.id)], function() { service.refresh() })
        else
          service.run(["approve", String(row.device.id)], function() {
            service.refresh(); service.refreshClients()
          })
      }
      return
    }
    if (focusSection === "devices") {
      var dev = devices[selectedIndex]
      if (dev && dev.fingerprint)
        service.run(["unpair", dev.fingerprint], function() { service.refreshClients(); service.refresh() })
      return
    }
    if (focusSection === "display") {
      var drow = displayModel[selectedIndex]
      if (!drow) return
      if (drow.kind === "preset") {
        service.setDisplayPreset(drow.preset.id)
      } else {
        service.setCaptureMode(drow.kind)
        settle.restart()
      }
    }
  }

  function scrollItemIntoView(item) {
    if (!panelFlick || !item) return
    Qt.callLater(function() {
      if (!item) return
      var margin = Style.space(6)
      var point = item.mapToItem(panelFlick.contentItem, 0, 0)
      var top = point.y
      var bottom = top + item.height
      var viewTop = panelFlick.contentY
      var viewBottom = viewTop + panelFlick.height
      var maxY = Math.max(0, panelFlick.contentHeight - panelFlick.height)
      if (top < viewTop + margin) panelFlick.contentY = Math.max(0, top - margin)
      else if (bottom > viewBottom - margin)
        panelFlick.contentY = Math.min(maxY, bottom + margin - panelFlick.height)
    })
  }

  function scrollCursorIntoView() {
    if (focusSection === "session" && sessionColumn)
      scrollItemIntoView(sessionColumn.children[selectedIndex])
    else if (focusSection === "pairing" && pairingColumn)
      scrollItemIntoView(pairingColumn.children[selectedIndex])
    else if (focusSection === "devices" && deviceColumn)
      scrollItemIntoView(deviceColumn.children[selectedIndex])
    else if (focusSection === "display" && displayColumn)
      scrollItemIntoView(displayColumn.children[selectedIndex])
  }

  function setHeaderCursor() {
    cursorActive = true
    focusSection = "header"
    selectedIndex = 0
    actionFocused = false
  }

  function setSectionCursor(section, index) {
    cursorActive = true
    focusSection = section
    selectedIndex = index
    actionFocused = false
  }

  onSelectedIndexChanged: scrollCursorIntoView()
  onFocusSectionChanged: scrollCursorIntoView()
  onCursorSectionsChanged: ensureCursor()

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      Item {
        LensMark {
          anchors.centerIn: parent
          markSize: Style.space(11)
          markColor: root.glyphColor
          filled: service.state === "streaming"
          visible: !service.pinMismatch
        }
        Text {
          anchors.centerIn: parent
          visible: service.pinMismatch
          text: "󰀦"
          color: root.glyphColor
          font.family: root.fontFamily
          font.pixelSize: Style.space(11)
        }
        Rectangle {
          visible: root.needsYou
          anchors { right: parent.right; top: parent.top }
          width: Style.space(6); height: width; radius: width / 2
          color: root.urgent
        }
      }
    }
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.RightButton) {
        if (service.state === "streaming")
          service.run(["stop-session"], function() { service.refresh() })
        else
          service.openConsole()
      } else {
        root.toggle()
      }
    }
  }

  Timer {
    id: statsPoll
    interval: 2000
    repeat: true
    triggeredOnStart: true
    running: root.opened && service.state === "streaming"
    onTriggered: service.refreshStats()
  }

  Timer {
    id: settle
    interval: 1500
    onTriggered: {
      service.refresh()
      service.refreshClients()
      service.refreshDisplays()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(400))
    contentHeight: panel.fittedContentHeight(column.implicitHeight, Style.space(560))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: !!(root.pinFieldRef && root.pinFieldRef.activeFocus)
      onMoveRequested: function(dx, dy) {
        if (!root.cursorActive) { root.cursorActive = true; return }
        if (dy !== 0) root.moveCursor(dy)
        else if (dx !== 0) root.moveCursorH(dx)
      }
      onActivateRequested: if (root.cursorActive) root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Flickable {
        id: panelFlick
        anchors.fill: parent
        contentWidth: width
        contentHeight: column.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.VerticalFlick
        interactive: contentHeight > height
        ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

        Column {
          id: column
          width: panelFlick.width
          spacing: Style.space(12)

          Item {
            id: header
            width: parent.width
            implicitHeight: hero.implicitHeight
            readonly property bool ringVisible: root.headerHasCursor
            function focusHero() { root.setHeaderCursor() }

            PanelHero {
              id: hero
              width: parent.width
              title: Model.heroTitle(service.state, service.summary.client_name)
              meta: Model.heroMeta(service.state, root.heroPhraseText)
              detail: service.stream ? String(service.stream.codec || "").toUpperCase() : ""
              foreground: root.foreground
              fontFamily: root.fontFamily
              iconOpacity: service.hostEnabled ? 1.0 : 0.5
              iconComponent: Component {
                LensMark {
                  markSize: Style.font.display
                  markColor: service.state === "stopped" ? root.dim : root.foreground
                  filled: service.state === "streaming"
                }
              }
              trailingControl: Component {
                ToggleSwitch {
                  id: powerSwitch
                  checked: service.hostEnabled
                  busy: settle.running
                  hasCursor: header.ringVisible
                  foreground: hero.foreground
                  onHovered: function(on) { if (on) header.focusHero() }
                  onToggled: {
                    service.setHostEnabled(!service.hostEnabled)
                    settle.restart()
                  }
                  PanelToolTip {
                    visible: powerSwitch.containsMouse
                    text: root.toggleHint
                    fontFamily: hero.fontFamily
                  }
                }
              }
            }
          }

          Text {
            visible: service.pinMismatch
            width: parent.width
            wrapMode: Text.Wrap
            color: root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: "The process answering the management port did not present this host's certificate, so no credential was sent. Either the host regenerated its identity, or something else is on that port."
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            PanelSectionHeader {
              text: "SESSION"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            GridLayout {
              visible: root.facts.length > 0
              width: parent.width
              columns: 2
              columnSpacing: Style.spacing.md
              rowSpacing: Style.spacing.sm

              Repeater {
                model: root.facts
                Column {
                  Layout.fillWidth: true
                  Layout.preferredWidth: 1
                  spacing: 0
                  Text {
                    width: parent.width
                    text: modelData.k
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                  }
                  Text {
                    width: parent.width
                    text: modelData.v
                    color: root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    elide: Text.ElideRight
                  }
                }
              }
            }

            Text {
              visible: root.live && service.games.length === 0
              width: parent.width
              wrapMode: Text.Wrap
              text: "No game was launched through Punktfunk — this is the desktop."
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Repeater {
              model: service.games
              Column {
                width: parent.width
                spacing: 0
                Text {
                  width: parent.width
                  text: modelData.title || "Desktop"
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.body
                  elide: Text.ElideRight
                }
                Text {
                  width: parent.width
                  text: (modelData.client || "—") + " · " + (modelData.plane || "")
                      + (modelData.state === "grace" ? " · reconnecting" : "")
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  elide: Text.ElideRight
                }
              }
            }

            Column {
              id: sessionColumn
              width: parent.width
              spacing: Style.space(6)

              Repeater {
                model: root.sessionActionModel
                ActionRow {
                  required property var modelData
                  required property int index
                  width: sessionColumn.width
                  rowIndex: index
                  label: modelData.label
                }
              }
            }

            Column {
              visible: root.live
              width: parent.width
              spacing: 2

              RowLayout {
                width: parent.width
                Text {
                  text: "Target"
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                }
                Item { Layout.fillWidth: true }
                Text {
                  text: service.stream
                          ? Model.mbps(service.stream.bitrate_kbps) + " Mbps"
                          : "—"
                  color: root.foreground
                  font.family: "monospace"
                  font.pixelSize: Style.font.caption
                }
              }

              Canvas {
                id: spark
                width: parent.width
                height: Style.space(30)
                readonly property var pts: root.spark
                onPtsChanged: requestPaint()
                onPaint: {
                  var ctx = getContext("2d"); ctx.reset()
                  var vals = pts
                  if (!vals || vals.length < 2) return
                  var lo = Math.min.apply(null, vals), hi = Math.max.apply(null, vals)
                  if (hi - lo < 1e-6) { lo -= 1; hi += 1 }
                  var pad = Style.space(3)
                  var h = height - pad * 2
                  var xOf = function(n) { return n / (vals.length - 1) * width }
                  var yOf = function(v) { return pad + h - (v - lo) / (hi - lo) * h }
                  ctx.beginPath()
                  ctx.moveTo(xOf(0), yOf(vals[0]))
                  for (var i = 1; i < vals.length; i++) ctx.lineTo(xOf(i), yOf(vals[i]))
                  ctx.lineTo(xOf(vals.length - 1), height)
                  ctx.lineTo(xOf(0), height)
                  ctx.closePath()
                  ctx.fillStyle = Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.16)
                  ctx.fill()
                  ctx.beginPath()
                  ctx.moveTo(xOf(0), yOf(vals[0]))
                  for (i = 1; i < vals.length; i++) ctx.lineTo(xOf(i), yOf(vals[i]))
                  ctx.strokeStyle = String(root.accent)
                  ctx.lineWidth = 1.5
                  ctx.lineJoin = "round"
                  ctx.stroke()
                }
                Connections {
                  target: root
                  function onAccentChanged() { spark.requestPaint() }
                }
              }

              Text {
                visible: root.spark.length < 2
                width: parent.width
                text: "collecting…"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }

          PanelSeparator {
            visible: root.showPairing
            foreground: root.foreground
          }

          Column {
            visible: root.showPairing
            width: parent.width
            spacing: Style.space(10)

            PanelSectionHeader {
              text: "PAIRING"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Text {
              visible: !root.needsYou
              width: parent.width
              wrapMode: Text.Wrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: service.armed
                      ? (service.pairingPin
                           ? "Pairing is open — enter " + service.pairingPin + " on the device"
                           : "Pairing is open")
                      : "Open a pairing window, then add this host on the device."
            }

            Column {
              id: pairingColumn
              width: parent.width
              spacing: Style.space(6)

              Repeater {
                model: root.pairingModel
                PairingRow {
                  required property var modelData
                  required property int index
                  width: pairingColumn.width
                  row: modelData
                  rowIndex: index
                }
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            PanelSectionHeader {
              text: "DEVICES"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Text {
              visible: root.devices.length === 0
              width: parent.width
              wrapMode: Text.Wrap
              text: "No devices are paired yet. Open a pairing window, then add this host on the device."
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Column {
              id: deviceColumn
              width: parent.width
              spacing: Style.space(6)

              Repeater {
                model: root.devices
                DeviceRow {
                  required property var modelData
                  required property int index
                  width: deviceColumn.width
                  device: modelData
                  rowIndex: index
                }
              }
            }
          }

          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            PanelSectionHeader {
              text: "DISPLAY"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Column {
              id: displayColumn
              width: parent.width
              spacing: Style.space(6)

              Repeater {
                model: root.displayModel
                DisplayRow {
                  required property var modelData
                  required property int index
                  width: displayColumn.width
                  row: modelData
                  rowIndex: index
                }
              }
            }

            Text {
              visible: text.length > 0
              width: parent.width
              wrapMode: Text.Wrap
              text: {
                var e = service.displayEffective || {}
                if (!e.topology) return ""
                return "In force: " + e.topology + " topology · " + e.identity + " identity · "
                     + e.mode_conflict + " on a mode clash · up to " + e.max_displays + " displays."
              }
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Text {
              visible: root.presetList.length > 0
              width: parent.width
              wrapMode: Text.Wrap
              text: "A display that outlives a disconnect needs a compositor whose capture survives one. Under Hyprland the capture is a portal handle, so the display is torn down with the session whichever preset is picked."
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }
        }
      }
    }
  }

  Timer {
    id: phraseTimer
    interval: 2800
    running: root.opened && service.state === "streaming"
    repeat: true
    onTriggered: phraseSwap.restart()
  }

  SequentialAnimation {
    id: phraseSwap
    PropertyAnimation {
      target: hero; property: "metaOpacity"
      to: 0.0; duration: 180; easing.type: Easing.OutQuad
    }
    ScriptAction {
      script: root.phraseIndex = (root.phraseIndex + 1) % Model.heroPhraseCount()
    }
    PropertyAnimation {
      target: hero; property: "metaOpacity"
      to: 1.0; duration: 260; easing.type: Easing.InQuad
    }
  }

  // Geometry is the brand mark's own: equal circles, centers a diagonal ≈ 1.46 r apart.
  component LensMark: Item {
    id: lens
    property color markColor
    property bool filled: false
    property int markSize: Style.space(11)
    implicitWidth: markSize
    implicitHeight: markSize
    width: markSize
    height: markSize

    Canvas {
      id: mark
      anchors.fill: parent
      onPaint: {
        var ctx = getContext("2d"); ctx.reset()
        var u = width / 100
        ctx.strokeStyle = String(lens.markColor)
        ctx.fillStyle = String(lens.markColor)
        ctx.lineWidth = 10 * u
        ctx.beginPath(); ctx.arc(34 * u, 66 * u, 29 * u, 0, 2 * Math.PI); ctx.stroke()
        ctx.beginPath(); ctx.arc(66 * u, 34 * u, 29 * u, 0, 2 * Math.PI); ctx.stroke()
        if (lens.filled) {
          ctx.save()
          ctx.beginPath(); ctx.arc(34 * u, 66 * u, 29 * u, 0, 2 * Math.PI); ctx.clip()
          ctx.beginPath(); ctx.arc(66 * u, 34 * u, 29 * u, 0, 2 * Math.PI); ctx.fill()
          ctx.restore()
        }
      }
      Connections {
        target: lens
        function onMarkColorChanged() { mark.requestPaint() }
        function onFilledChanged() { mark.requestPaint() }
      }
    }
  }

  component ActionRow: CursorSurface {
    id: actionRow
    property int rowIndex: 0
    property string label: ""

    hasCursor: root.cursorActive && root.focusSection === "session" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.scrollItemIntoView(actionRow)
    foreground: root.foreground
    fill: root.hoverFill
    implicitHeight: actionInner.implicitHeight + Style.spacing.xl

    Text {
      id: actionInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      text: actionRow.label
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      elide: Text.ElideRight
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onEntered: root.setSectionCursor("session", actionRow.rowIndex)
      onClicked: {
        root.setSectionCursor("session", actionRow.rowIndex)
        root.activateCursor()
      }
    }
  }

  component PairingRow: CursorSurface {
    id: pairingRow
    property var row: null
    property int rowIndex: 0
    readonly property string kind: row ? String(row.kind || "") : ""
    readonly property var device: row && row.device ? row.device : null
    readonly property bool rowSelected: root.cursorActive && root.focusSection === "pairing" && root.selectedIndex === rowIndex
    readonly property bool isPending: kind === "pending"
    readonly property bool isPin: kind === "pin"
    readonly property bool isArm: kind === "arm"

    hasCursor: rowSelected && !root.actionFocused
    onHasCursorChanged: if (hasCursor) root.scrollItemIntoView(pairingRow)
    current: isArm && service.armed
    foreground: root.foreground
    fill: root.hoverFill
    currentFill: root.selectedFill
    implicitHeight: pairingInner.implicitHeight + Style.spacing.xl

    Item {
      id: pairingInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(8)
      implicitHeight: Math.max(pairingInfo.implicitHeight, pairingBtn.implicitHeight, pinField.implicitHeight)

      Column {
        id: pairingInfo
        visible: !pairingRow.isPin
        anchors.left: parent.left
        anchors.right: pairingBtn.visible ? pairingBtn.left : parent.right
        anchors.rightMargin: pairingBtn.visible ? Style.space(8) : 0
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Text {
          width: parent.width
          text: pairingRow.isArm
                  ? (service.armed ? "Close the pairing window" : "Open a pairing window")
                  : (pairingRow.device && pairingRow.device.name ? pairingRow.device.name : "(unnamed device)")
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          elide: Text.ElideRight
        }
        Text {
          visible: pairingRow.isPending
          width: parent.width
          text: Model.tail(pairingRow.device ? pairingRow.device.fingerprint : "")
              + " · " + ((pairingRow.device && pairingRow.device.age_secs) || 0) + "s ago"
          color: root.dim
          font.family: "monospace"
          font.pixelSize: Style.font.caption
        }
      }

      TextField {
        id: pinField
        visible: pairingRow.isPin
        anchors.left: parent.left
        anchors.right: pairingBtn.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        placeholderText: "Moonlight PIN"
        Component.onCompleted: if (pairingRow.isPin) root.pinFieldRef = pinField
        Component.onDestruction: if (root.pinFieldRef === pinField) root.pinFieldRef = null
        onActiveFocusChanged: if (activeFocus) root.setSectionCursor("pairing", pairingRow.rowIndex)
        Keys.onPressed: function(event) {
          if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            if (text.length > 0)
              service.run(["pin", text], function() { text = ""; service.refresh() })
            event.accepted = true
          } else if (event.key === Qt.Key_Escape) {
            keyCatcher.forceActiveFocus()
            event.accepted = true
          }
        }
      }

      PanelActionButton {
        id: pairingBtn
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        visible: pairingRow.isPending || pairingRow.isPin
        iconText: pairingRow.isPending ? (root.actionFocused && pairingRow.rowSelected ? "󰅖" : "󰄬") : "󰄬"
        tooltipText: pairingRow.isPending
                       ? (root.actionFocused && pairingRow.rowSelected ? "Deny" : "Approve")
                       : "Submit the PIN"
        foreground: pairingRow.isPending && root.actionFocused && pairingRow.rowSelected
                      ? root.urgent : root.foreground
        hasCursor: pairingRow.rowSelected && root.actionFocused
        onHovered: function(on) {
          if (!on) return
          root.cursorActive = true
          root.focusSection = "pairing"
          root.selectedIndex = pairingRow.rowIndex
          root.actionFocused = pairingRow.isPending
        }
        onClicked: {
          if (pairingRow.isPin) {
            if (pinField.text.length > 0)
              service.run(["pin", pinField.text], function() { pinField.text = ""; service.refresh() })
            return
          }
          if (!pairingRow.device) return
          if (root.actionFocused)
            service.run(["deny", String(pairingRow.device.id)], function() { service.refresh() })
          else
            service.run(["approve", String(pairingRow.device.id)], function() {
              service.refresh(); service.refreshClients()
            })
        }
      }
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: pairingRow.isPin ? Qt.IBeamCursor : Qt.PointingHandCursor
      z: -1
      onEntered: root.setSectionCursor("pairing", pairingRow.rowIndex)
      onClicked: {
        root.setSectionCursor("pairing", pairingRow.rowIndex)
        if (pairingRow.isPin) pinField.forceActiveFocus()
        else root.activateCursor()
      }
    }
  }

  component DeviceRow: CursorSurface {
    id: deviceRow
    property var device: null
    property int rowIndex: 0
    readonly property bool isNative: device && (device.access_level !== undefined || device.name !== undefined)
    readonly property bool rowSelected: root.cursorActive && root.focusSection === "devices" && root.selectedIndex === rowIndex

    hasCursor: rowSelected && !root.actionFocused
    onHasCursorChanged: if (hasCursor) root.scrollItemIntoView(deviceRow)
    foreground: root.foreground
    fill: root.hoverFill
    implicitHeight: deviceInner.implicitHeight + Style.spacing.xl

    HoverHandler { id: deviceHover }

    Item {
      id: deviceInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(8)
      implicitHeight: Math.max(deviceInfo.implicitHeight, unpairBtn.implicitHeight)

      Column {
        id: deviceInfo
        anchors.left: parent.left
        anchors.right: unpairBtn.visible ? unpairBtn.left : parent.right
        anchors.rightMargin: unpairBtn.visible ? Style.space(8) : 0
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Text {
          width: parent.width
          text: (deviceRow.device && (deviceRow.device.name || deviceRow.device.label)) || "(unnamed device)"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          elide: Text.ElideRight
        }
        Text {
          width: parent.width
          text: Model.tail(deviceRow.device ? deviceRow.device.fingerprint : "")
              + " · " + (deviceRow.isNative ? "punktfunk" : "moonlight")
              + (deviceRow.device && deviceRow.device.access_level ? " · " + deviceRow.device.access_level : "")
          color: root.dim
          font.family: "monospace"
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      PanelActionButton {
        id: unpairBtn
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        visible: deviceHover.hovered || deviceRow.rowSelected
        iconText: "󰗨"
        tooltipText: "Unpair this device"
        foreground: root.urgent
        hasCursor: deviceRow.rowSelected && root.actionFocused
        onHovered: function(on) {
          if (!on) return
          root.cursorActive = true
          root.focusSection = "devices"
          root.selectedIndex = deviceRow.rowIndex
          root.actionFocused = true
        }
        onClicked: {
          if (!deviceRow.device || !deviceRow.device.fingerprint) return
          service.run(["unpair", deviceRow.device.fingerprint], function() {
            service.refreshClients(); service.refresh()
          })
        }
      }
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      acceptedButtons: Qt.NoButton
      onEntered: root.setSectionCursor("devices", deviceRow.rowIndex)
    }
  }

  component DisplayRow: CursorSurface {
    id: displayRow
    property var row: null
    property int rowIndex: 0
    readonly property string kind: row ? String(row.kind || "") : ""
    readonly property var preset: row && row.preset ? row.preset : null
    readonly property bool isMode: kind === "dedicated" || kind === "mirror"
    readonly property bool isCurrent: isMode
      ? service.captureMode === kind
      : !!(preset && service.displayPreset === preset.id)
    readonly property string title: isMode
      ? (row.label || "")
      : ((preset && (preset.name || preset.id)) || "")
    readonly property string subtitle: isMode
      ? (row.detail || "")
      : ((preset && (preset.summary || (preset.fields ? "Saved preset." : ""))) || "")

    hasCursor: root.cursorActive && root.focusSection === "display" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.scrollItemIntoView(displayRow)
    current: isCurrent
    foreground: root.foreground
    fill: root.hoverFill
    currentFill: root.selectedFill
    implicitHeight: displayInner.implicitHeight + Style.spacing.xl

    Row {
      id: displayInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(6)
      anchors.rightMargin: Style.space(6)
      spacing: Style.space(8)

      Text {
        text: displayRow.isCurrent ? "󰄬" : " "
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        width: Style.space(22)
        horizontalAlignment: Text.AlignHCenter
        anchors.verticalCenter: parent.verticalCenter
      }

      Column {
        width: parent.width - Style.space(30)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Text {
          width: parent.width
          text: displayRow.title
          color: displayRow.isCurrent || displayRow.hasCursor ? root.foreground : root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          font.bold: displayRow.isCurrent
          elide: Text.ElideRight
        }
        Text {
          visible: displayRow.subtitle.length > 0
          width: parent.width
          text: displayRow.subtitle
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.Wrap
        }
      }
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onEntered: root.setSectionCursor("display", displayRow.rowIndex)
      onClicked: {
        root.setSectionCursor("display", displayRow.rowIndex)
        root.activateCursor()
      }
    }
  }
}
