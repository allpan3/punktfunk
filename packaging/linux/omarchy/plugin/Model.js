// Phrases, figures, and which sections the panel shows. QML imports this;
// node:test loads the same file through `module.exports`.

var ACTIVE_PHRASES = [
  "Pushing pixels",
  "Slicing frames",
  "Holding the stream",
  "Guarding the desk",
  "Lighting the couch",
  "Braiding packets",
  "Watching the bitrate",
  "Keeping the latency"
]

function mbps(kbps) {
  return (Number(kbps || 0) / 1000).toFixed(1)
}

function dur(us) {
  var n = Number(us || 0)
  return n >= 1000 ? (n / 1000).toFixed(1) + " ms" : Math.round(n) + " µs"
}

function tail(fp) {
  var s = String(fp || "")
  return s.length > 10 ? "…" + s.slice(-10) : (s || "—")
}

function heroPhraseCount(phrases) {
  return (phrases || ACTIVE_PHRASES).length
}

function heroPhrase(index, phrases) {
  var list = phrases || ACTIVE_PHRASES
  if (!list.length) return ""
  var n = list.length
  var i = ((Number(index) || 0) % n + n) % n
  return list[i]
}

function heroTitle(state, clientName) {
  if (state === "streaming" && clientName) return String(clientName)
  return "Punktfunk"
}

function heroMeta(state, phrase) {
  if (state === "streaming") return String(phrase || "")
  if (state === "idle") return "Ready to stream"
  return "Host is stopped"
}

function captureMode(captureMonitor) {
  var cm = String(captureMonitor || "").trim()
  return cm ? "mirror" : "dedicated"
}

function sessionLive(state, sessions, games) {
  return state === "streaming" || Number(sessions || 0) > 0 || (games && games.length > 0)
}

function showArmRow(state) {
  return state === "idle" || state === "streaming"
}

function pairingExpanded(needsYou) {
  return !!needsYou
}

function sectionVisible(section, snap) {
  var s = snap || {}
  if (section === "session") return true
  if (section === "pairing") return showArmRow(s.state) || !!s.needsYou
  if (section === "devices") return true
  if (section === "display") return true
  return false
}

function visibleSections(snap) {
  var names = ["session", "pairing", "devices", "display"]
  var out = []
  for (var i = 0; i < names.length; i++) {
    if (sectionVisible(names[i], snap)) out.push(names[i])
  }
  return out
}

function sessionFacts(snap) {
  var s = snap || {}
  var stream = s.stream || null
  var summary = s.summary || {}
  if (s.state === "stopped") return []
  if (sessionLive(s.state, s.sessions, s.games) && stream) {
    var out = [
      { k: "Resolution", v: stream.width + " × " + stream.height },
      { k: "Frame rate", v: stream.fps + " fps" },
      { k: "Bitrate", v: mbps(stream.bitrate_kbps) + " Mbps" }
    ]
    if (Number(stream.time_to_first_frame_ms || 0) > 0)
      out.push({ k: "First frame", v: stream.time_to_first_frame_ms + " ms" })
    return out
  }
  return [
    { k: "Devices paired", v: String(summary.native_paired_clients || 0) },
    { k: "Pairing", v: s.armed ? "open" : "closed" },
    { k: "Host", v: summary.version || "—" }
  ]
}

function sessionActions(live, hasGame) {
  if (!live) return []
  var out = [{ id: "stop", label: "Stop the session" }]
  if (hasGame) out.push({ id: "end", label: "End the game" })
  return out
}

function pairingRows(arm, pinPending, pendingDevices) {
  var out = []
  if (arm) out.push({ kind: "arm" })
  if (pinPending) out.push({ kind: "pin" })
  var list = pendingDevices || []
  for (var i = 0; i < list.length; i++)
    out.push({ kind: "pending", device: list[i] })
  return out
}

function displayRows(presets) {
  var out = [
    { kind: "dedicated", label: "Dedicated display", detail: "Stream a virtual display at the client's size" },
    { kind: "mirror", label: "This screen", detail: "Stream the primary monitor on this box" }
  ]
  var list = presets || []
  for (var i = 0; i < list.length; i++)
    out.push({ kind: "preset", preset: list[i] })
  return out
}

function allPresets(builtIn, custom) {
  return (builtIn || []).concat(custom || [])
}

function sparkPoints(history, key) {
  var rows = history || []
  var out = []
  for (var i = 0; i < rows.length; i++) {
    var v = rows[i] ? rows[i][key] : null
    if (v !== null && v !== undefined) out.push(Number(v))
  }
  return out
}

if (typeof module !== "undefined") {
  module.exports = {
    ACTIVE_PHRASES: ACTIVE_PHRASES,
    mbps: mbps,
    dur: dur,
    tail: tail,
    heroPhraseCount: heroPhraseCount,
    heroPhrase: heroPhrase,
    heroTitle: heroTitle,
    heroMeta: heroMeta,
    captureMode: captureMode,
    sessionLive: sessionLive,
    showArmRow: showArmRow,
    pairingExpanded: pairingExpanded,
    sectionVisible: sectionVisible,
    visibleSections: visibleSections,
    sessionFacts: sessionFacts,
    sessionActions: sessionActions,
    pairingRows: pairingRows,
    displayRows: displayRows,
    allPresets: allPresets,
    sparkPoints: sparkPoints
  }
}
