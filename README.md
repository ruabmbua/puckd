# puckd

Userspace driver for the 2nd-gen Steam Controller (2026) and its "Puck" dongle
(`28de:1304`), without Steam. Real uinput devices, so it works on any compositor
(including native Wayland windows on Hyprland, where Steam's XTest mouse doesn't reach).

- **Gamepad mode**: a virtual Xbox pad per controller, with rumble.
- **Desktop mode**: a shared virtual mouse and keyboard (trackpad pointer with
  acceleration and inertia, trackpad/stick scrolling, configurable buttons, haptic ticks).
- **Hold Menu (600 ms)** to switch modes. You feel a click when it switches, and a short banner shows the new mode and the hold-to-switch hint (unless the overlay is open: its header shows the mode). A short press still works as a normal button.
- **On-screen keyboard + settings menu** (QAM button, any mode): a wlr-layer-shell overlay
  that types into the focused window and changes the emulated controller and settings at runtime.
- **Steps aside** when Steam or an SDL3 game with native Steam Controller support opens
  the controller, and takes over again when it closes.

## Setup

### 1. Build and install

```sh
cargo install --path .          # installs ~/.cargo/bin/puckd
```

### 2. Permissions (udev)

puckd runs as your user. It needs read/write access to the Puck's hidraw nodes and to
`/dev/uinput`. The rule gives both to the logged-in user (systemd `uaccess`):

```sh
sudo install -m644 dist/70-puckd.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
puckd list                      # every slot should say "rw ok"
```

Replug the Puck if `list` still reports permission errors.

### 3. Run it with your session

Pick **one** of these. All of them need a single running instance: puckd drives every
controller and every dongle slot. The gamepad and desktop modes work on any compositor.
The on-screen keyboard and settings overlay needs wlr-layer-shell (Hyprland, Sway, river,
niri, KDE Plasma). Per-app presets need wlr-foreign-toplevel-management (Hyprland, Sway,
river, niri; **not** KDE, so the overlay works there but per-app switching doesn't).

**systemd user service** (any compositor; restarts on failure, `reload` re-reads the config):

```sh
install -Dm644 dist/puckd.service ~/.config/systemd/user/puckd.service
systemctl --user daemon-reload
systemctl --user enable --now puckd
journalctl --user -u puckd -f    # logs
```

The service doesn't need `WAYLAND_DISPLAY`: if it is missing, the overlay connects to the
first `wayland-*` socket in `$XDG_RUNTIME_DIR`, and retries until the compositor is up. If
you run several Wayland sessions at once, import the right display into systemd from your
compositor's startup, e.g.
`dbus-update-activation-environment --systemd WAYLAND_DISPLAY`.

**Hyprland**: in `~/.config/hypr/hyprland.conf`:

```ini
exec-once = puckd
```

**Sway**: in `~/.config/sway/config`:

```
exec puckd
```

**KDE Plasma (Wayland)**: autostart entry:

```sh
install -Dm644 dist/puckd.desktop ~/.config/autostart/puckd.desktop
```

Or use *System Settings → Autostart → Add… → Add Application*. If `~/.cargo/bin` is not in
the session's PATH, put the full path in `Exec=`.

### 4. Turn off your desktop's pointer acceleration for puckd's mouse

puckd shapes the trackpad pointer itself (filtering, acceleration, ignoring lift-off and
click wobble). Your compositor would add its own acceleration on top (libinput's
"adaptive" profile by default), which makes fast and slow movements unpredictable. Set
the **"puckd Mouse"** device to a flat profile. This affects only that virtual device,
not your real mouse:

**Hyprland** (`hyprland.conf`; `hyprctl devices` lists it as `puckd-mouse`):

```ini
device {
    name = puckd-mouse
    accel_profile = flat
    sensitivity = 0
}
```

**Sway** (`swaymsg -t get_inputs` lists it as `4617:23489:puckd_Mouse`):

```
input "4617:23489:puckd_Mouse" {
    accel_profile flat
    pointer_accel 0
}
```

**KDE Plasma**: *System Settings → Mouse & Touchpad → Mouse*, select **puckd Mouse**,
and set *Pointer acceleration* to *None*. Leave *Pointer speed* in the middle.

Then adjust speed and acceleration in puckd (overlay → Controller → Pointer speed /
acceleration, or `[desktop]` in the config).

### 5. Configure (optional)

Most things can be changed live in the overlay (press QAM): presets, button mapping,
pointer and scroll speed, haptics and so on. "Save settings" writes `~/.config/puckd.toml`.
For everything else:

```sh
cp puckd.example.toml ~/.config/puckd.toml
puckd check-config
systemctl --user reload puckd     # or: pkill -HUP puckd
```

Files puckd uses:

| Path | What |
|---|---|
| `~/.config/puckd.toml` | settings, the Default preset (`[gamepad]`), controller names |
| `~/.config/puckd/presets/*.toml` | named presets |
| `~/.config/puckd/app-presets.toml` | which app uses which preset |
| `$XDG_RUNTIME_DIR/puckd/<slot>.json` | live status (name, battery, mode) for bars |

Steam: puckd steps aside while Steam (or an SDL3 game with native Steam Controller
support) has the controller open, and takes over again when it closes
(`general.backoff`).

## Gamepad profiles

| `gamepad.profile` | Looks like | Exposes |
|---|---|---|
| `xbox360` (default) | Microsoft X-Box 360 pad `045e:028e` | standard Xbox layout, rumble. Works everywhere (SDL, Wine/Proton, native evdev). |
| `xbox-elite` | Xbox One Elite 2 pad `045e:0b00` (version `0x0407`) | plus the 4 back paddles; QAM as an extra button |
| `extended` | Steam Deck controller as `hid-steam` presents it, `28de:1205` | plus paddles (`BTN_GRIP*`), QAM (`BTN_BASE`), trigger clicks (`BTN_TL2/TR2`), pad clicks (`BTN_THUMB/THUMB2`), analog trackpads (`ABS_HAT0*`/`ABS_HAT1*`), d-pad as buttons; extras after the Deck layout: capacitive touches (`BTN_TRIGGER_HAPPY1-6`), pad pressure (`ABS_HAT3*`), and a "… Motion Sensors" device |

The identities are chosen so that devices are found in the mapping databases. That's SDL's
built-in one and the community SDL_GameControllerDB, which gilrs/bevy and Steam's SDL use.
For unknown devices, SDL and gilrs guess differently: SDL takes `BTN_X` as X, while gilrs
follows the kernel spec, where the same code is North (Y). That gave swapped X/Y and dead
triggers. Checked with SDL 3.4 and gilrs 0.11: all three profiles map face buttons,
triggers and paddles the same way (gilrs has no paddle buttons; it reports them as
`Unknown`). `puckd pad-preview PROFILE` creates a pad without a controller for such checks.

Buttons can be remapped for any profile (`[gamepad.remap] l4 = "a"`, or the overlay's
Button mapping page). For games that get even the standard layouts wrong, save the profile
plus mapping as a **preset** (see below).

## Desktop mode defaults

| Input | Action |
|---|---|
| right trackpad | pointer (acceleration, inertia after a flick) |
| right pad click, RT | left click; hold the pad click (or RT) and move to drag / select text |
| LT | right click |
| left pad click, R3 | middle click |
| left trackpad, left stick | scroll (smooth, hi-res wheel) |
| right stick | pointer |
| A / B / X / Y | Enter / Esc / Backspace / Space |
| d-pad | arrow keys |
| LB / RB | back / forward |
| View | Tab |
| L3 | Super |
| hold Menu | switch to gamepad mode |

Everything is rebindable in `[desktop.buttons]` (key combos like `"ctrl+w"` work too).
Gyro mouse (while a grip is touched) is available with `desktop.gyro_mouse = true`.

## On-screen keyboard and settings

Press **QAM** (⋯) to open/close the overlay in any mode (`general.osk_button`). It needs a
compositor with wlr-layer-shell (Hyprland, Sway, river, niri, ...) and never takes keyboard
focus: keys go through puckd's virtual keyboard into the focused window. Key labels come from
your compositor's keymap, so non-US layouts show the right characters (dead keys included).

| Input | Keyboard tab |
|---|---|
| left / right trackpad | cursor over the left / right part of the keyboard |
| trackpad click or LT / RT | type the key under that cursor |
| d-pad / left stick, A | move the focus, type the focused key |
| B / X / Y / Menu (≡) | Backspace / Space / Shift / Enter |
| LB / RB | switch between Keyboard and Controller (the ⚙ key also opens Controller) |

Shift, Ctrl, Alt, Super and AltGr are sticky: tap once for the next key, twice to lock.
You can also click keys with a mouse. QAM always belongs to the overlay while puckd drives
the controller; it never reaches games.

The **Controller** tab shows battery and mode, and has three sections:

- **Preset (how games see the controller)**: the active preset, the button mapping, and
  the emulated controller, motion device, rumble strength and motor swap. Also
  "Trackpads as mouse in gamepad mode" (on by default): the Xbox profiles have no
  trackpads, so in gamepad mode the right pad still moves the pointer and the left pad
  scrolls, with your desktop settings. Pad clicks keep their desktop bindings unless the
  button mapping uses them. Changing the
  profile replaces the virtual pad (games see an unplug and a replug).
- **Settings (shared by all presets)**: mode, pointer speed/acceleration/inertia, scroll
  speed and direction, gyro mouse, haptics, and when to step aside. Switching presets
  never changes these. "Restore default settings" resets them (not the preset, not the
  current mode). "Save settings" writes them to `puckd.toml`, keeping comments and other keys.
- **Tools**: the input test, and "Turn off controller".

The top right shows the controller's name, the active preset, the mode and the battery.
**Controller names** (in Settings) are keyed by the controller's own serial number, so a
name follows the controller to whichever dongle slot it connects to. Names are saved right
away into `[names]` in `puckd.toml` and also show up in the logs and the status files.

D-pad up/down selects, left/right or A changes, B goes back.

**Presets** page:

- **Default** always exists. It is the `[gamepad]` section of `puckd.toml` and its button
  mapping is never changed from the overlay. Changing other preset values while on Default
  offers "Save changes as Default". Changing a button mapping turns the settings into a
  "new preset (unsaved)", which can only be saved under a new name.
- Named presets live in `~/.config/puckd/presets/<name>.toml`. Once you change something,
  the page shows "X (modified)" and offers "Save changes to X", "Save as new preset…" and
  "Discard changes". "Rename…" and "Delete" are there too.
- **Names** are typed with the overlay keyboard or a real keyboard: a name field replaces
  the tab bar, and keys go into the field instead of the focused app. The overlay holds
  keyboard focus only while the field is open. ≡ or Enter saves, Esc cancels, and B or
  Backspace deletes a character. The field starts with a suggestion from the focused window: its title for
  Steam games, otherwise the app id (`org.mozilla.firefox` → `firefox`).
- **Per-app presets**: "Use X for <app>" ties the active preset to the focused window's
  Wayland app_id (Steam games under XWayland are `steam_app_<id>`). Focusing that app then
  shows a popup at the bottom of the screen (above the overlay if it is open), "Allow
  switching to preset X for <app>?" with **A: always for this session** (it then switches automatically
  whenever you come back to that app, until puckd restarts), **X: once** (asks again
  next time) and **B: no** (doesn't ask again this session). It closes on its own after
  10 s without remembering anything. While it shows, A, X and B go to the popup only, an
  open overlay gets no input at all, and the answering press doesn't reach anything else.
  Leaving the app returns to the preset you last chose by hand, unless you have unsaved
  changes. `general.preset_switch = "auto"` switches without asking, `"off"` never.
  This needs wlr-foreign-toplevel-management (Hyprland, Sway, river, niri, ...); the
  associations live in `~/.config/puckd/app-presets.toml`.

The other two sub-pages:

- **Button mapping**: for each physical button, what it acts as in gamepad mode (itself,
  nothing, or another button). Changes apply live without recreating the pad. On an
  entry, **A** waits for you to press the button it should act as. Any button counts,
  including A, B and the d-pad; with no press for 5 s it cancels. **X** opens a list of all
  targets to pick from, and **Y** resets the entry.

The overlay remembers where you were: closing it with QAM and opening it again brings you
back to the same page and entry. So you can switch between a game and, say, the button
mapping page while you tune it.
- **Input test**: a live view of everything the controller sends (buttons, sticks with
  touch, triggers with full-pull click, trackpads with touch position, pressure and click,
  grips, accelerometer, gyro), for testing the hardware. All buttons are shown there
  instead of acting, so **LB + RB together** go back. QAM can't be tested there because it
  always toggles the overlay.

If the daemon runs as a systemd user service without `WAYLAND_DISPLAY`, it uses the first
`wayland-*` socket in `$XDG_RUNTIME_DIR`. `puckd osk-preview [settings|mapping|presets|input]
[prompt|capture|list|name]` shows the overlay without a controller.

## Diagnostics

```sh
puckd list                      # slots and permissions
puckd dump [1]                  # decoded input (add --raw or --imu); lizard mode is off meanwhile
puckd haptic-test               # plays each rumble/haptic effect with a description
puckd power-off [SLOT]          # turns a controller off
puckd -v                        # debug logging
```

Tuning the trackpad mouse: `puckd mouse-study` walks you through short tasks (resting,
sweeps, flicks, precise moves, clicks, scrolling) and records the raw trackpad data, what
puckd emitted, and where the cursor actually went (Hyprland). `puckd replay <dir>`
replays such a recording through the current code and prints per-task numbers (drift,
movement after lifting, movement around clicks), so tuning changes can be compared on the
same input.

Status for bars: `$XDG_RUNTIME_DIR/puckd/<slot>.json` (name, serial, connected, mode, battery, charge state).

## Known limitations

- **Gyro isn't visible to SDL games.** SDL pairs a "Motion Sensors" device with its pad
  through the evdev `uniq` string, and uinput can't set `uniq`. The device is there for raw
  evdev readers. The fix is a UHID-based profile (e.g. a virtual DualSense, so `hid-playstation`
  creates the pad, motion sensors and touchpad with a proper `uniq`). See PLAN.md.
- `backoff = "any"` also steps aside for SDL3 games that drive the controller natively
  (they then get the full controller through SDL). Use `"steam"` to step aside only for Steam.
- Not yet tested on kernel 7.3+, where `hid-steam` handles the controller too. For the
  older controllers `hid-steam` goes quiet while a hidraw client has the device open,
  which would prevent double input. Check it with `evtest` once 7.3 is here.
