"use strict"

const test = require("node:test")
const assert = require("node:assert/strict")
const Model = require("./Model.js")

test("mbps is one decimal of kbps/1000", () => {
  assert.equal(Model.mbps(300000), "300.0")
  assert.equal(Model.mbps(0), "0.0")
  assert.equal(Model.mbps(null), "0.0")
})

test("dur switches at 1 ms", () => {
  assert.equal(Model.dur(15), "15 µs")
  assert.equal(Model.dur(2321), "2.3 ms")
  assert.equal(Model.dur(1000), "1.0 ms")
})

test("tail keeps short fingerprints whole", () => {
  assert.equal(Model.tail("abc"), "abc")
  assert.equal(Model.tail("0123456789abcdef"), "…6789abcdef")
  assert.equal(Model.tail(""), "—")
})

test("hero phrases rotate and wrap", () => {
  const a = Model.heroPhrase(0)
  const b = Model.heroPhrase(Model.ACTIVE_PHRASES.length)
  assert.equal(a, b)
  assert.ok(a.length > 0)
  assert.equal(Model.heroPhrase(-1), Model.ACTIVE_PHRASES[Model.ACTIVE_PHRASES.length - 1])
})

test("hero title names the client only while streaming", () => {
  assert.equal(Model.heroTitle("streaming", "iPad"), "iPad")
  assert.equal(Model.heroTitle("idle", "iPad"), "Punktfunk")
  assert.equal(Model.heroTitle("stopped"), "Punktfunk")
})

test("hero meta is the phrase while streaming", () => {
  assert.equal(Model.heroMeta("streaming", "Pushing pixels"), "Pushing pixels")
  assert.equal(Model.heroMeta("idle"), "Ready to stream")
  assert.equal(Model.heroMeta("stopped"), "Host is stopped")
})

test("captureMode follows the stored pin", () => {
  assert.equal(Model.captureMode("DP-2"), "mirror")
  assert.equal(Model.captureMode("  HDMI-A-1  "), "mirror")
  assert.equal(Model.captureMode(""), "dedicated")
  assert.equal(Model.captureMode(null), "dedicated")
})

test("pairing expands only when someone waits", () => {
  assert.equal(Model.pairingExpanded(true), true)
  assert.equal(Model.pairingExpanded(false), false)
})

test("arm row shows while the host is up", () => {
  assert.equal(Model.showArmRow("idle"), true)
  assert.equal(Model.showArmRow("streaming"), true)
  assert.equal(Model.showArmRow("stopped"), false)
})

test("pairing hides when the host is down and nobody waits", () => {
  assert.deepEqual(
    Model.visibleSections({ state: "stopped", needsYou: false }),
    ["session", "devices", "display"]
  )
  assert.ok(Model.visibleSections({ state: "idle" }).includes("pairing"))
  assert.ok(Model.visibleSections({ state: "stopped", needsYou: true }).includes("pairing"))
})

test("session facts are idle inventory until a stream exists", () => {
  const idle = Model.sessionFacts({
    state: "idle",
    armed: true,
    summary: { native_paired_clients: 2, version: "0.35.0" }
  })
  assert.deepEqual(idle, [
    { k: "Devices paired", v: "2" },
    { k: "Pairing", v: "open" },
    { k: "Host", v: "0.35.0" }
  ])

  const live = Model.sessionFacts({
    state: "streaming",
    sessions: 1,
    stream: { width: 1920, height: 1080, fps: 120, bitrate_kbps: 40000, time_to_first_frame_ms: 18 }
  })
  assert.equal(live[0].v, "1920 × 1080")
  assert.equal(live[2].v, "40.0 Mbps")
  assert.equal(live[3].v, "18 ms")
})

test("session actions are stop, then end-game when a title is up", () => {
  assert.deepEqual(Model.sessionActions(false, true), [])
  assert.deepEqual(Model.sessionActions(true, false), [{ id: "stop", label: "Stop the session" }])
  assert.equal(Model.sessionActions(true, true)[1].id, "end")
})

test("pairing rows are arm, pin, then the queue", () => {
  const rows = Model.pairingRows(true, true, [{ id: 3, name: "tv" }])
  assert.deepEqual(rows.map((r) => r.kind), ["arm", "pin", "pending"])
  assert.equal(rows[2].device.id, 3)
  assert.deepEqual(Model.pairingRows(false, true, []).map((r) => r.kind), ["pin"])
  assert.deepEqual(Model.pairingRows(false, false, []), [])
})

test("display rows are dedicated, this screen, then presets", () => {
  const rows = Model.displayRows([{ id: "hotdesk", name: "Hotdesk" }])
  assert.equal(rows[0].kind, "dedicated")
  assert.equal(rows[1].kind, "mirror")
  assert.equal(rows[2].kind, "preset")
  assert.equal(rows[2].preset.id, "hotdesk")
  assert.equal(Model.allPresets([{ id: "a" }], [{ id: "b" }]).length, 2)
})

test("sparkPoints drops nulls so a flat series still draws", () => {
  assert.deepEqual(
    Model.sparkPoints([{ target: 1 }, { target: null }, { target: 3 }], "target"),
    [1, 3]
  )
})
