---
title: Multi-seat contract
description: What the host promises a multi-seat supervisor — the environment a seat host is started with, the registry marker that reserves display connectors, and what the host does differently on a seat. Reference for the opt-in punktfunk-seats add-on.
---

**Windows only, and nothing here is installed or enabled by default.** A normal punktfunk install
has one host on the machine's console session and behaves exactly as it always has. This page
describes the contract that a separate, opt-in supervisor uses to run *several* hosts on one
Windows box, one per seat, so that several people can play on it at once.

## What Windows requires

Several people using one Windows machine at the same time is a Windows licensing question before it
is a punktfunk one, and punktfunk does not change the answer.

- **Windows Server**, plus a **Remote Desktop Services CAL for every seat**. Per-Device CALs
  usually suit fixed seats better than Per-User, since a seat is a place rather than a person.
- **A client edition of Windows serves one session at a time.** Multi-seat on Windows 10 or 11 is
  not a configuration this software provides; a single seat works there and is a legitimate setup.

`check-seat-display.ps1` names the edition it finds so this is caught at install rather than after.
If you are sizing a deployment, price the CALs first — they usually dominate.

The supervisor is [`punktfunk-seats`](https://git.unom.io/unom/punktfunk-seats), a separate program
with its own installer. It owns the seat accounts, the Windows sessions and everything else about
how a seat comes to exist. The host never learns any of that. This page exists so the two can be
built and versioned apart.

**Contract version: `SEAT_CONTRACT 1`.** The supervisor checks it against `punktfunk-host --version`
and refuses a host it does not understand.

## The reservation marker

```
HKLM\SOFTWARE\Punktfunk\Seats
```

If that key exists, display connectors **12 through 15 are reserved for seats**. The console host
then allocates only connectors 0 through 11, and each seat host owns exactly one reserved
connector.

The key lives under `HKLM` and is written only by the elevated seats installer or service. That is
deliberate: a seat host runs as an ordinary process, and if the reservation could be turned on from
a seat's own environment, a seat could grant itself the right to manage displays across the whole
box. A seat that names a connector without the marker present fails to start rather than falling
back.

**With the marker absent, everything below is inert.** No host reads a seat variable, no behaviour
changes, and a machine that never installs the add-on cannot tell the difference.

## The environment a seat host is started with

The supervisor launches an ordinary `punktfunk-host serve` and sets these. None of them are meant
to be set by hand.

| Variable | Meaning |
|---|---|
| `PUNKTFUNK_SEAT_SESSION` | `1` marks this host as belonging to a seat rather than the console. |
| `PUNKTFUNK_SEAT_ID` | The seat's identity, 32 lowercase hexadecimal characters. Any other value is refused. |
| `PUNKTFUNK_SEAT_DISPLAY_SLOT` | The reserved connector this seat owns, `12` to `15`. Refused without the marker, or outside that range. |

A seat host is also given the ordinary per-host settings it needs to not collide with its
neighbours: its own `PUNKTFUNK_CONFIG_DIR`, `PUNKTFUNK_MGMT_BIND`, `PUNKTFUNK_NATIVE_PORT` and
`PUNKTFUNK_HOST_NAME`. Those are documented on the [configuration](/docs/configuration) page and mean the
same thing here as anywhere else. Each seat is an independent host on the network with its own
pairing and its own name.

## What the host does differently on a seat

- **It owns one display connector.** A seat host creates its virtual display on the connector it
  was given and nothing else. It never issues the device-wide clear that a lone host uses to reap
  monitors left by a crash, because on a seats box those monitors belong to somebody.
- **It takes its own driver lock.** Hosts on different connectors no longer contend for one lock,
  so they start independently, and two hosts that somehow claim the same connector still refuse to
  run rather than fighting over it.
- **Launches go to its own session.** Games, hook commands and the tray start in the Windows
  session this host is in, not the console's. On the console host that is the same session, which
  is why this changed nothing when it landed.
- **Its audio endpoints are its own.** The minted speaker and microphone devices carry the seat's
  id, so a seat finds its own devices and never adopts a neighbour's. A seat host also leaves the
  machine's default playback and recording devices alone, since those are shared by the whole box.
- **Its virtual pointer is its own.** The resident HID mouse that keeps Windows drawing a cursor
  into the stream is named after the seat's connector, so each seat mints its own instead of the
  second one finding the first's name taken. Virtual gamepads are not partitioned this way yet: a
  seat can still be refused a pad another host on the box holds.
- **No status tray.** The tray is a per-user, per-session icon and the supervisor is the control
  surface for seats, so a seat host does not supervise one.

## Checking a seat can actually stream

```
punktfunk-host spike --source virtual --hdr --seconds 5
```

This creates a virtual display, captures it, encodes through the display driver and writes the
result. It exits non-zero if any part of that fails: no driver, no captured frame, no hardware
encoder, no 10-bit path, or no encoded output. The supervisor runs it inside a seat's session
before it reports the seat as healthy, and it is the quickest way to answer "why is this seat not
working" by hand.

Drop `--hdr` to ask the same question without requiring a 10-bit path.

## The seat display driver is a second package

A seat's display is created and started by Windows' own terminal-services stack, and that stack
starts exactly one thing: a driver claiming the hardware id `RdpIdd_IndirectDisplay`. Claiming it
**takes over every RDP session on the machine**, ordinary Remote Desktop and administrator sessions
included. So it cannot ride the package everyone installs, and the build produces two:

| Package | Claims | Installed by |
|---|---|---|
| `pf_vdisplay.{inf,cat}` | the console display device | every punktfunk install, unchanged |
| `pf_vdisplay_seats.{inf,cat}` | the seat display ids | the seats add-on, on an explicit choice |

Both come from the same source and the same signing key. A machine that never installs the add-on
never claims the id, and its Remote Desktop behaves exactly as it always did.

**Losing that claim is silent.** Windows ranks our package and its own `rdpidd.inf` identically, so
the id is decided on the newer driver date — and a seat whose display went to Microsoft's adapter
still logs in, still looks fine, and simply never streams. Check it rather than assume it:

```
powershell -File check-seat-display.ps1
```

Run with at least one seat connected, since the devnodes only exist while a session is. It reports
which driver owns each live seat display, compares both driver dates and warns before the tie can
be lost, and names the Windows edition — a client edition serves one session at a time however
well the display works.

## Seat audio needs a driver on disk

A seat with no sound card has nothing to loopback-capture, so the host mints its own render
endpoint per seat. Minting binds Valve's Remote Play streaming drivers, which means
`SteamStreamingSpeakers.inf` and `SteamStreamingMicrophone.inf` have to be present — either
already bound to a device, or in Steam's driver directory. Steam never has to run. Without them
a seat starts, streams video and is silently mute.

```
powershell -File check-seat-audio.ps1
```

A virtual cable is not a substitute. The wiring plan refuses a cable as a loopback source,
because capturing one re-records whatever is written into it, and `PUNKTFUNK_MIC_DEVICE` only
pins the microphone. Both address the microphone, not desktop audio.

## What the host never knows

How a Windows session is created, which account a seat runs as, that RDP exists, and anything
about the supervisor's own configuration. Those are the add-on's concern, and keeping them out of
the host is why the add-on can be installed, updated and removed on its own.
