# punktfunk — an Omarchy shell plugin

Pair devices, watch sessions and control your [Punktfunk](https://punktfunk.com) host from the
Omarchy bar, without opening a browser. Same shape as the first-party Tailscale / network / audio
panels: a hero, then one scrolling column of sections.

- **bar-widget** — the two-ring mark, filled while streaming, dim when the host is stopped, with a
  badge when a device is waiting. Click opens the panel. Right-click stops a live session, or opens
  the web console when nothing is streaming.
- **panel** — one scrolling column:
  - *Hero* — rotating phrases while streaming; the trailing switch starts and stops the user unit.
  - *SESSION* — facts, stop / end-game, and a compact sparkline while live.
  - *PAIRING* — expands when a device waits; an arm row when the host is idle.
  - *DEVICES* — both planes, unpair on the row.
  - *DISPLAY* — Dedicated / This screen (`punktfunk-omarchy mode`), then the presets.
- **service** — one long-lived event stream that drives both of the above. The pairing toast is
  the `pairing-pending` hook's (it carries Approve / Deny); the panel only updates its badge.

Keyboard: `j` / `k` move the cursor, Enter activates, Esc closes, Tab switches panels.

## Install

The host package ships these files at `/usr/share/punktfunk/omarchy/plugin/`, and
[`punktfunk-omarchy setup`](https://punktfunk.com/docs/omarchy) offers to install them into
`~/.config/omarchy/plugins/punktfunk` and enable the widget; `punktfunk-omarchy remove` reverses
both. Requires a Punktfunk host on the same machine — `punktfunk-host` on `PATH`. Omarchy 4.0 or
newer.

## How it talks to the host

Every call is `punktfunk-host ctl <verb> --json`, spawned as a process. **The QML never speaks
HTTPS, never holds the operator token, and never sees the host's certificate.**

That is a deliberate boundary, not an implementation detail. A shell plugin runs *unsandboxed
inside `omarchy-shell`*, and the management API's admin surface — the pending queue, the PIN,
unpair, session control — is exactly the surface worth protecting. So the credential stays where it
already lives: `punktfunk-host ctl` reads the operator token and the host's certificate from the
0700 config directory in its own process, pins the certificate **before** sending the token, and
prints JSON on stdout. If something that is not your host answers on the management port, `ctl`
exits 4 with no credential transmitted, and this plugin shows that state instead of a plausible
"host not running".

`Service.qml` is the only spawn site. Reading `run()` answers "can this plugin leak a secret?" in
about forty lines.

One process runs continuously: `ctl watch`, in `Service.qml`. Exactly one, because the host caps
concurrent event streams and the web console holds one of them. It reconnects by itself and emits a
`ctl.resync` line when it fell behind the host's catch-up ring, which is the plugin's cue to
re-snapshot rather than trust what it has.

### The one thing that polls

The host publishes no periodic stats, so the sparkline polls `ctl stats` every two seconds, and
**only** while the panel is open. Everything else is event-driven or refreshed on open.

## Traps

Every one of these cost a debugging session, and none of them logs anything useful:

- **A missing `open: root.opened` on a panel fails in total silence.** No QML warning, no log line —
  the panel simply never creates its layer surface while the bar icon keeps working. If a panel
  stops opening and nothing is logged, check that binding first.
- **The manifest shape is asymmetric.** Omarchy uses `kinds: [...]` + `entryPoints: { … }`, and the
  entry-point key is camelCase (`barWidget`) while the kind is hyphenated (`bar-widget`).
  `omarchy plugin validate` catches it — run it before trusting an edit.
- **Quickshell's `Process` does not search `PATH`.** It reports *the binary could not be found* for a
  program that is on `PATH` and executable. Every spawn here goes through `sh -c 'exec "$@"' sh …`,
  which does the lookup without re-quoting our argv.
- **`parent.<property>` does not resolve inside `StdioCollector`.** Assign through an explicit `id`
  or the whole call chain silently returns nothing.

### Displays: Dedicated, This screen, then presets

Dedicated and This screen call `punktfunk-omarchy mode`. The rows below pick the virtual-display
preset and show the policy it puts in force. The section deliberately does **not** list live virtual
displays: on this compositor there are never any to list. A wlroots capture arrives over a sandboxed
portal handle the host cannot re-open per attach, so `vdisplay::registry` passes those displays
through rather than owning them, and `/display/state` comes back empty.

The same limit is why the section says a display cannot outlive a disconnect under Hyprland: several
preset summaries promise exactly that, and this compositor cannot deliver it.

### Sparkline

**Target** is the encoder bitrate, where adaptive bitrate has settled. The sparkline is that number
over time, because one figure cannot tell a bitrate that has sat at 300 from one that just collapsed
to it. The window is client-side: the panel keeps what its own two-second poll saw, fills while the
panel is open, holds about three minutes, and is cleared when a session ends so a chart never draws
a line between two unrelated streams.

### Known limitation: one watcher per monitor

A bar-widget is instantiated once per output, so a two-monitor box runs two `ctl watch` streams
rather than the one this is designed around. The host caps concurrent event streams at 32 and the
web console holds one, so this is comfortably within budget for any realistic desktop — but it is
not what the code says it does, and folding the watcher into a shared singleton is the fix if it
ever matters.

## Reversing it

```sh
omarchy plugin remove punktfunk
```

The plugin owns no state — it stores nothing, and removing it changes nothing about your host, your
pairings or your firewall.

## Licence

MIT OR Apache-2.0, same as Punktfunk.
