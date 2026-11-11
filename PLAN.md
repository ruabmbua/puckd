# puckd — Steam Controller (2026) userspace driver, rough plan

A small Rust daemon for the 2nd-gen Steam Controller through its "Puck" dongle
(USB `28de:1304`). Kernels before 7.3 bind it to `hid-generic`, so it only works as
the controller's built-in mouse/keyboard ("lizard") mode. Steam's own Desktop mode
doesn't help on Hyprland: its mouse emulation goes through XTest/XWayland and never
reaches native Wayland windows. Its gamepad mode does work: that is a uinput X360 pad.

Goal: without Steam,
- a **gamepad mode** with a uinput Xbox 360 pad,
- a **desktop mode** with a uinput mouse and keyboard (real devices, so they work on any compositor),
- **long-press Menu** to switch between them, with a haptic tick to confirm,
- later, an on-screen keyboard as a wlr-layer-shell overlay.

## Status (2026-11-11)

Implemented: milestones 1–7 plus rumble, paddles/touches (extended profile), motion device,
trackpad haptics, battery status files and the config file with SIGHUP reload. See README.md.

Verified on hardware (Puck, kernel 7.2): haptic sides are 0 = left, 1 = right, 2 = both
(3 = nothing; tick and click both work); bit 0x40 is the right center button (≡, named
`menu` here, Start) and 0x4000 the left one (`view`, Back), the reverse of SDL's bit names; slot discovery (interfaces 2..5; interface 6 excluded
by its report descriptor), report decoding (0x42), battery (0x43), lizard off, uinput FF upload →
40 ms rumble resend → stop, all haptic output reports accepted, SDL 3.4 mappings for all three
profiles (paddles included), back-off when an SDL3 HIDAPI client opens the Puck, and reclaim.

Needs a human with the controller in hand:
- that the Puck's own mouse/keyboard evdev nodes stay quiet (lizard off)
- trackpad feel: pointer speed/accel, scroll direction, gyro-mouse axes and signs
- wireless disconnect/connect (0x79) on controller sleep/wake

Next:
- **UHID DualSense profile**: uinput can't set `uniq`, so SDL never pairs the motion
  device with the pad (it matches with EVIOCGUNIQ). A UHID virtual DualSense (054c:0ce6, answering
  feature reports 0x05 calibration / 0x09 pairing / 0x20 firmware, rumble from output report
  0x02) makes `hid-playstation` create the pad, motion sensors and touchpad with a real uniq.
  Proton/Steam/SDL then get gyro and touchpad. This is how InputPlumber does it.
- OSK: done (milestone 8), with a settings tab. Possible extras: F-keys/numpad layer, word
  suggestions, per-app key repeat.

## Pass-through mode (branch `uhid-passthrough`)

How it works now: [docs/internals.md](docs/internals.md#steam-pass-through). Below: history
and open points.

Goal: puckd owns the hardware exclusively; Steam and SDL games get a byte-level clone of the
controller, so their native Steam Controller support (gyro, trackpads, haptics) works.
puckd keeps desktop mode, the overlay and its buttons, and decides who gets the input.

Findings (2026-11-11, `puckd uhid-clone` spike, Steam + Outer Wilds, usbmon baseline):
- A uhid clone of a dongle slot (28de:1304) is **rejected**: SDL's triton driver accepts
  dongles only with `interface_number` 2..5, and a uhid device has no USB parent
  (interface -1). Steam acts the same: it never sends 0x83/0x87, lizard mode stays on.
- The same slot cloned as a **wired controller (28de:1302)**, same descriptor, bus USB,
  **works**: Steam lists it, configures it (0x87 settings, 0x83 attributes, 0xae serial),
  sends haptics (output 0x81/0x85), and games see it through Steam Input. No fake USB
  device (dummy_hcd) needed.
- Steam on the real dongle: 0x02 `b4` status on interface 6 (control) first, then 0x87
  lizard-off to every slot (EPIPE on empty ones), then per connected slot 0x83, 0xae,
  0x02 `f3`, 0x01 `ed` (string attributes `user/wireless_transport`, `esb/bond`), 0xc1,
  0xdc, 0xe2. Report ID 2 messages are for the dongle itself.
- The Puck also has a CDC-ACM serial console (interface 0, `/dev/ttyACM*`, a `dongle~$`
  shell with firmware logs); Steam opens it.
- Steam's rule `KERNELS=="000[356]:28DE:*"` gives the user access to the clone's hidraw.

Implemented: `puckd broker` (fd passing; socket-activated, exits when idle; access from
`/etc/puckd/broker.toml`: a group, default `puckd`, plus a user list),
`72-puckd-exclusive.rules`, a clone per connected controller (dongle slot → wired persona,
wired/BLE keep their identity), the View + ≡ Menu hand-over chord (hidden from puckd and the
clone, single presses held back 80 ms), config `passthrough` / `handover_buttons`, banners.
Clients: Steam takes over (puckd steps aside until the chord or until Steam lets go);
anything else (SDL3 games) shares: native controller in gamepad mode with puckd's buttons
filtered out, a neutral controller in desktop mode / overlay, no uinput pad meanwhile. On
release puckd sends LOAD_DEFAULT_SETTINGS (0x8E) and SET_DEFAULT_DIGITAL_MAPPINGS (0x85)
(IDs from the classic Steam Controller table; 0x87 and 0x9F match it), then its settings.
Untested: the share mode with an SDL3 game, 0x8E/0x85 on this controller, BLE clones.
Open: kernel 7.3 hid-steam.

Tested on hardware (2026-11-11): Steam takes over the clone (banner), the chord hands the
controller back and forth, and while puckd has it, Steam lists puckd's uinput pad. Except
the extended profile: Steam ignores an evdev pad with Valve's IDs (28de:1205, it expects
hidraw for those), so that profile never shows in Steam's controller list. With
pass-through this matters less (Steam gets the real thing); a uhid Steam Deck persona
would fix it if needed.


Original plan:
1. In the daemon: one wired-persona clone per *connected* controller (any link), created
   on connect, destroyed on disconnect. Dongle-level traffic (report 2, 0x79/0x7b) stays
   with puckd. No uinput pad in this mode (else games see two controllers).
2. Arbitration: gamepad mode forwards input to the clone; desktop mode / overlay open
   sends neutral state. puckd keeps the switch and overlay buttons. Steam's 0x87 settings
   pass through, except lizard mode stays off; puckd's keepalive continues.
3. Exclusive access: udev rule sorted before 73-seat-late that drops `uaccess` from the
   real hidraw nodes (and the ttyACM console); a small root broker (`puckd broker`,
   system service) opens them plus /dev/uhid and passes the fds to the user daemon over a
   unix socket (SCM_RIGHTS), checking only the peer uid. Not a security boundary, just
   keeps other programs from tripping over the hardware.
4. Kernel 7.3 hid-steam will bind the real device too: unbind or block it in this mode.

## Prior art (read first)

- `konikvranik/steam-puck-bridge` (C, about 850 lines, no deps): gamepad part done, including
  rumble. No mouse mode. Its `docs/protocol.md` has the protocol notes below.
- SDL3 `src/joystick/hidapi/SDL_hidapi_steam_triton.c`: the reference for the protocol.
- `iczero/steam-controller-stuff`: protocol notes.
- Linux 7.3 `drivers/hid/hid-steam.c`: has Triton support. Check whether it already does
  a gamepad/mouse switch on a long press of Options, as it does for the Deck. If so, this
  daemon's value is the OSK and the polish.

## Protocol essentials (from steam-puck-bridge `docs/protocol.md`; verify against SDL)

- Devices: `/sys/class/hidraw/*`, VID `28DE`, PID `1304`/`1305` on USB **interfaces 2..5**
  (one per controller slot). Interface 6 is the dongle. The wired/BT controller is `1302`/`1303`.
- **Lizard mode off**: 64-byte feature report `01 87 03 09 00 00` followed by zeros
  (report ID 1, SET_SETTINGS_VALUES, len 3, SETTING_LIZARD_MODE=9, value 0 u16 LE).
  Resend it **every 3 s or less**, or a firmware watchdog turns lizard mode back on.
  To go back to the controller's own desktop mode, the same report with value 1 *probably*
  works (unverified). We don't need that if we do the mouse ourselves.
- **Input reports**: `0x42` (USB/dongle), `0x45` (BLE), `0x47` (newer, adds a u16 trackpad
  timestamp at offset 18, which shifts the pads by +2). Offsets include the report ID:
  `1` seq u8, `2` buttons u32, `6/8` triggers L/R s16 (0..32767), `10/12` left stick X/Y,
  `14/16` right stick X/Y (Y is up-positive, so negate it for evdev), `18/20` left pad X/Y,
  `22` left pressure, `24/26` right pad X/Y, `28` right pressure, `30` IMU timestamp u32,
  `34..38` accel XYZ, `40..44` gyro XYZ (s16).
- `0x46`/`0x79`: wireless status, byte 1: `1` disconnect, `2` connect. `0x43`: battery.
- **Buttons** (u32): A 0x1, B 0x2, X 0x4, Y 0x8, QAM 0x10, R3 0x20, View 0x40, R4 0x80, R5 0x100,
  RB 0x200, D↓ 0x400, D→ 0x800, D← 0x1000, D↑ 0x2000, **Menu 0x4000**, L3 0x8000, Steam 0x10000,
  L4 0x20000, L5 0x40000, LB 0x80000, R-stick touch 0x100000, R-pad touch 0x200000,
  R-pad click 0x400000, RT click 0x800000, L-stick touch 0x1000000, L-pad touch 0x2000000,
  L-pad click 0x4000000, LT click 0x8000000, R-grip 0x10000000, L-grip 0x20000000.
- **Rumble**: output report `80 00 0000 <u16 left speed> 00 <u16 right speed> 00`
  (10 bytes). It stops by itself after about 50 ms, so resend every 40 ms while active.
  A short pulse makes a good "mode switched" tick.

## Architecture

```
hidraw slot fds ──read──▶ decode(Report) ──▶ State ──▶ mode router ──▶ uinput pad
      ▲                                                  │        └──▶ uinput mouse+kbd
      └── keepalive (lizard off, 3 s) / haptics ◀────────┘
```

- One thread (or a small epoll loop) per hidraw slot, or one poll over all of them. Keep it
  simple and synchronous. No async needed.
- Crates: `evdev` (uinput VirtualDevice), `nix`/`libc` (hidraw `HIDIOCSFEATURE` ioctl,
  plain `read`/`write` for reports), `udev` or a sysfs scan for discovery and hotplug.
- One **pad** per connected controller: name "Microsoft X-Box 360 pad", ids `045e:028e`, so
  SDL/gilrs pick their stock mappings. FF_RUMBLE goes to the rumble report.
- One **mouse+keyboard** device, shared:
  - right pad → relative pointer: delta of finger position while touched, with
    acceleration; no motion on touch-down; optional inertia/flick
  - right-pad click / RT → left button, LT → right button, left pad → scroll wheel
    (smooth: `REL_WHEEL_HI_RES`), d-pad → arrows, A → Enter, B → Escape, Y → OSK toggle (later)
  - optional: gyro → pointer while a grip is touched
- **Mode switch**: Menu held ≥ 600 ms → toggle, haptic tick, swallow that Menu press.
  Start in desktop mode. In desktop mode the pad device still exists but stays idle, so
  games don't see a hot-unplug.
- **Steam coexistence**: when Steam opens the device, back off (close the fds, stop the
  keepalive) and reclaim once Steam is gone. The C bridge looks for a process named
  `steam`. Better might be to check whether any other process has the hidraw node open.
- Permissions: the user already has ACL rw on the Puck's hidraw nodes and on `/dev/uinput`
  (via udev uaccess). Ship a udev rule anyway, and a systemd **user** unit.

## Milestones

1. `--dump`: find the slots and hexdump reports. Check the offsets above against real input.
2. Lizard mode off plus keepalive. Check that the Puck's mouse/keyboard evdev nodes go quiet.
3. uinput pad. Test with `evtest`, then thoth/gilrs (thoth ignores `--no-gamepads`
   unless it's passed), then an SDL game.
4. uinput mouse/keyboard, and tune the trackpad feel.
5. Long-press Menu switch and the haptic tick.
6. Hotplug: dongle unplug/replug, controller sleep/wake (`0x46`/`0x79`), several controllers.
7. Steam back-off and reclaim.
8. (Later) OSK: `smithay-client-toolkit` layer-shell overlay, keys picked with the pads and
   typed through `zwp_virtual_keyboard_v1` or the uinput keyboard.

## Beyond the basics: expose everything the hardware has

The aim is to beat Steam's virtual X360 pad, which drops the gyro, the pads, the paddles and
the battery. Use the evdev conventions that `hid-playstation` and `hid-steam` (Deck) already use,
so SDL, Wine and games pick them up without extra setup.

- **Rumble** (uinput `EV_FF`): handle `FF_RUMBLE` and `FF_PERIODIC` uploads/erases and
  `FF_GAIN`. Run a 40 ms resend loop while any effect plays, honour `replay.length` and
  `replay.delay`, and stop all effects when the client closes the device.
- **Trackpad haptics**, as Steam has them: tiny ticks while the finger moves in mouse mode
  (one per N px), a click pulse on pad click, a tick on mode switch and on scroll detents.
  Haptic reports `0x81`..`0x85` exist but nobody documents them yet. Read SDL's
  Triton driver and the 7.3 `hid-steam` for their layouts, or dump Steam's traffic
  (usbmon/Wireshark on the Puck while Steam does haptics).
- **Motion sensors**: a separate uinput device with `INPUT_PROP_ACCELEROMETER`, `ABS_X/Y/Z`
  for accel and `ABS_RX/RY/RZ` for gyro, with resolution set (units/g, units/°/s) and
  `MSC_TIMESTAMP` from the IMU timestamp. Name it "<pad name> Motion Sensors", same uniq/phys
  as the pad, which is how SDL pairs it with the pad (as for DualSense / Deck).
- **Trackpads as touchpads** (gamepad mode): a uinput multitouch device per pad
  (`ABS_MT_POSITION_X/Y`, `ABS_MT_PRESSURE`, `BTN_TOUCH`, `BTN_LEFT` for the click), or both pads
  as two slots of one device, as `hid-playstation` does for the DS4/DualSense pad. Libinput
  ignores it as a pointer if it carries `ID_INPUT_JOYSTICK`, or tag it with
  `LIBINPUT_IGNORE_DEVICE`.
- **Paddles L4/L5/R4/R5, QAM, Steam button**: `BTN_TRIGGER_HAPPY1..`, `BTN_MODE`. Optionally
  map paddles to face buttons per config, for games that don't know paddles.
- **Analog triggers plus digital clicks**: `ABS_Z/RZ` analog. The full-pull click is a separate
  button (`BTN_TRIGGER_HAPPY*`), or it fires the trigger at 100 %.
- **Capacitive touches** (sticks, pads, grips): expose them as buttons too, which helps
  "gyro only while thumb on stick" setups.
- **Battery**: uinput can't make a `power_supply`. Publish the level over D-Bus as a UPower-like
  object, or simpler, write `$XDG_RUNTIME_DIR/puckd/<slot>.json` for a waybar module and send a
  low-battery `notify-send`.
- **Settings over feature reports**: `SET_SETTINGS_VALUES` (`0x87`) takes more settings than
  lizard mode, such as trackpad modes, haptic intensity, maybe IMU enable. Collect the setting
  IDs from SDL's `controller_constants.h`.
- **Config file** (`~/.config/puckd.toml`): mouse speed and acceleration, the long-press time,
  haptic strength, paddle mapping, gyro-mouse enable, deadzones. Re-read it on SIGHUP.
- **Per-slot identity**: give each controller's devices a stable `uniq` (the serial,
  `U: Uniq=FXB…` in `/proc/bus/input/devices`) so SDL/Steam/games recognise them across
  reconnects.

Milestones for these come after 7, roughly in this order: rumble, motion device, paddles and
touches, haptics, trackpads as touchpads, battery, config.

## Gotchas

- Without the 3 s keepalive, lizard mode comes back and every input doubles up.
- On kernel 7.3+, `hid-steam` claims the device but hands over to whoever opens hidraw
  (that's how Steam works with it). Test that the daemon still gets the raw reports.
- Games that see both our pad and a 7.3 `hid-steam` pad get double input. Prefer one source.
- Don't kill Steam or games from the daemon. Only back off.
