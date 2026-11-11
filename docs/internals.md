# puckd internals

Notes for developers: how the parts work and why. For using puckd see the
[manual](manual.md); for open work and protocol notes see [PLAN.md](../PLAN.md).

- [Main loop](#main-loop)
- [Steam pass-through](#steam-pass-through)
  - [Exclusive access: udev rule and broker](#exclusive-access-udev-rule-and-broker)
  - [The clone](#the-clone)
  - [Who uses the clone](#who-uses-the-clone)
  - [Hand-over chord](#hand-over-chord)
  - [Lizard mode and settings](#lizard-mode-and-settings)
  - [What Steam sends](#what-steam-sends)
  - [Debugging](#debugging)

## Main loop

`Daemon::run` polls the controller nodes, puckd's virtual pads, pass-through clones, the
overlay thread's wake fd and an inotify watch on `/dev`. With no controller connected it
sleeps until one of them fires (no timeout):

- Devices: `hid::scan` runs at start and when inotify reports a `hidraw*` node created,
  deleted or changed (IN_ATTRIB: udev applying ACLs). A node that can't be opened yet is
  retried after 2 s. Without inotify, scanning falls back to every 2 s.
- A dongle slot's node stays open with no controller on it; the controller's wireless
  connect report wakes the loop.
- The `/proc` scan for other openers (back-off, pass-through holders) runs every
  `backoff_check_ms` only while a slot is busy (`Slot::busy`: connected, backed off, or
  lent to a client), and right away when one becomes busy.
- Focus changes are only acted on (preset popup, auto switch, switching back) while puckd
  drives a controller (`any_active`); otherwise `focus_pending` defers the decision until
  one connects.

## Steam pass-through

The full install ([README](../README.md#full-install-steam-gets-the-real-controller)) makes
puckd the only process that opens the controller, and gives Steam and SDL games a
byte-level copy of it. Code: `src/broker.rs`, `src/uhid.rs`, `src/passthrough.rs`, and the
pass-through parts of `src/controller.rs`.

### Exclusive access: udev rule and broker

Steam's `60-steam-input.rules` and our `70-puckd.rules` tag the real hidraw nodes `uaccess`,
which `73-seat-late.rules` turns into an ACL for the logged-in user. Steam runs as that
user, so file permissions can't tell Steam and puckd apart. Instead:

- `dist/72-puckd-exclusive.rules` removes `uaccess` from the real nodes (Puck 1304/1305,
  wired 1302 by USB attributes, BLE 1303 by `KERNELS`), before `73-seat-late`. Nothing
  running as the user can open them. The clones have no USB parent and bus 0003 with PID
  1302, so they don't match and keep Steam's `KERNELS=="000[356]:28DE:*"` access.
- `puckd broker` (root) opens real hidraw nodes and `/dev/uhid` (root-only) on request
  and passes the fds over `/run/puckd/broker.sock` (`SOCK_SEQPACKET`, `SCM_RIGHTS`). One
  request per connection: the text `hidrawN` or `uhid`, answered with `ok` + fd or an error
  text. It only hands out Valve controller nodes (`hid::is_controller_node`, which also
  rejects our own clones).
- Access: `SO_PEERCRED`, checked against `/etc/puckd/broker.toml` (re-read per request):
  root, the listed `users`, or members of `group` (default `puckd`). Membership counts from
  the peer process's groups (`/proc/<pid>/status`) or from the group database
  (`getgrouplist`), so `usermod -aG` works without a new login. Not a security boundary:
  the point is that other programs don't trip over the hardware.
- Socket activation: `puckd-broker.socket` owns the socket; the service gets it as fd 3
  (`LISTEN_FDS`) and exits after 60 s without requests. fds already handed out stay valid.
  Run by hand, the broker binds the socket itself and stays.
- The user daemon tries a direct `open()` first. `EACCES` → broker. A node obtained through
  the broker marks the slot **exclusive** (`HidDevice::exclusive`): only then is a clone
  offered. Without the broker the old behaviour (back off when someone opens the node)
  stays.

### The clone

One uhid device per *connected* controller, created on connect and removed on disconnect
(`passthrough::Clone`). puckd forwards every input report from the controller to it and
serves the clone's output reports and GET/SET_REPORT requests against the real controller.

Identity:
- **Dongle slots become a wired controller (28de:1302, bus USB).** SDL's
  `SDL_hidapi_steam_triton.c` accepts dongle PIDs only on USB interfaces 2..5
  (`interface_number >= 2 && <= 5`), and a uhid device has no USB interface (hidapi reports
  -1). Steam behaves the same: a clone with the dongle's identity got two queries and was
  never configured (no `0x83`, no `0x87`, lizard mode stayed on). As 1302 it is listed,
  configured and driven fully. The controller protocol (feature messages in report 1,
  state report `0x42`) is the same; dongle-level messages (report 2, `b4` status, wireless
  `0x79`/`0x46`) stay with puckd.
- Wired and BLE controllers keep their own identity (BLE controllers are uhid devices from
  BlueZ already, which Steam accepts). Untested.
- Report descriptor: the real node's. `uniq`: the controller's serial. `phys`:
  `puckd/<serial>/<slot>/passthrough`; the `puckd/` prefix (`hid::OWN_PHYS_PREFIX`) keeps
  `hid::scan` and the broker from treating a clone as a controller.

Not forwarded: lizard reports `0x40`/`0x41` (the clone's hid-generic input devices would
move the pointer a second time) and wireless status.

### Who uses the clone

A client counts as attached on its first output or SET_REPORT. Who it is comes from `/proc`
(`backoff::openers` on the clone's hidraw node), Steam preferred (`Opener::is_steam`).

- **Steam takes over** (`Client::takes_over`): puckd drops its uinput pad, releases desktop
  outputs, skips `route()`, and forwards raw reports (minus the chord buttons). Steam Input
  replaces everything puckd does, so splitting buttons between them would only confuse.
- **Anything else shares** (SDL3 games, Wine hidraw): `route()` runs as usual, and the
  clone gets what a game should get: in gamepad mode the report with puckd's filtered
  buttons (QAM removed, switch button as tap / long-press), otherwise a neutral report
  (`neutralize`: buttons, triggers, sticks, trackpads, accel and gyro zeroed; sequence and
  timestamps kept). The uinput pad goes away (the game would see two controllers) and so
  does the gamepad-mode trackpad mouse.
- If Steam opens a shared clone, the share becomes a take-over.
- On take-over the daemon closes the overlay if Steam's controller owned it (`is_active`
  turns false; `Model::hide` drops a name entry or mapping capture) and dismisses a pending
  preset popup when no controller puckd drives is left to answer it. Banners stay: they
  time out.
- The overlay header (`menu::status`, `StatusCtx::native`) says "Gamepad mode, emulated"
  or "Gamepad mode, native for <game>".
- Release: `check_passthrough` (every `backoff_check_ms`, 2 s) finds no process holding the
  clone open. UHID_CLOSE doesn't help: the compositor keeps the clone's hid-generic input
  devices open.

### Hand-over chord

`passthrough::Chord`, default View + ≡ Menu (`handover_buttons`), held 1 s:

- Both down: hidden from `route()` and the clone; fires once after 1 s.
- One down alone: hidden for 80 ms (`CHORD_WINDOW`) in case the other follows, then passed
  through. Released within the window: replayed as a 60 ms press (`CHORD_TAP`).
- After the chord, its buttons stay hidden until all are up.

Firing while the clone exists: the clone is destroyed (`held`), so Steam sees an unplug and
drops its virtual gamepad. A paused-but-present clone was considered and rejected: Steam's
idle virtual gamepad would stay next to puckd's pad. `held_from` remembers the client's pid;
when that process exits, `held` clears and the clone comes back for the next Steam. Fired
with no client: held until the chord again.

### Lizard mode and settings

- puckd owns lizard mode. While Steam has the controller, puckd still sends lizard-off every
  keepalive (only that setting). Reason: Steam stops its own keepalive early while shutting
  down but keeps the clone open, and the firmware's ~3 s watchdog brought lizard mode back
  before puckd noticed Steam had let go.
- Clients can't turn lizard mode on: `keep_lizard_off` rewrites setting 9 in their
  SET_SETTINGS_VALUES (`0x87`) to 0.
- On release puckd sends `LOAD_DEFAULT_SETTINGS` (`0x8E`) and
  `SET_DEFAULT_DIGITAL_MAPPINGS` (`0x85`), then its own settings. Steam sets trackpad and
  IMU modes and clears the lizard mappings (`0x81`). The IDs come from the classic Steam
  Controller's table; `0x87` and `0x9F` (turn off) match it. Not yet confirmed on this
  controller.

### What Steam sends

From a usbmon capture with Steam driving the real Puck (feature reports, report ID first):

1. `02 b4` to interface 6 (the Puck's control interface): status, `02 b4 01 01`.
2. `01 87 03 09 00 00` (lizard off) to every slot interface; empty slots answer EPIPE.
3. Per connected slot: `02 b4` (`… 01 02` = connected), `01 83` (attributes), `01 ae 15 01`
   (unit serial), `02 83` and `02 f3 …` (dongle), `01 ed <len> <string>` (string attributes
   `user/wireless_transport`, `esb/bond`), `01 c1`, `01 dc`, `01 e2`, settings via `0x87`
   (setting `0x30` IMU, `0x07`/`0x08` trackpad modes, `0x2d`, `0x32`, `0x22`, `0x23`,
   `0x54`, `0x55`).
4. Then `01 87 03 09 00 00` every 3 s, haptics as output reports `0x81` and `0x85`.

Steam also opens the Puck's CDC-ACM serial console (interface 0, `/dev/ttyACM*`, a
`dongle~$` shell with firmware logs). The exclusive rule leaves it alone.

### Debugging

- `sudo puckd uhid-clone [--bus usb|ble|virtual] [--pid HEX] all|hidrawN...` (daemon
  stopped, real nodes' ACLs removed with `setfacl -b`): clones nodes with any identity and
  logs every request clients send. This is how the 1302 persona was found.
- Steam against the real hardware: `sudo modprobe usbmon` and
  `sudo cat /sys/kernel/debug/usb/usbmon/<bus>u` (bus from `/sys/class/hidraw/hidrawN/device`).
- `journalctl -u puckd-broker` shows each fd handed out or refused.
