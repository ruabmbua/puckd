# puckd user manual

This manual covers everyday use. For installation see the [README](../README.md#installation)
and the setup page for your desktop.

- [Modes](#modes)
- [Desktop mode](#desktop-mode)
- [Gamepad mode](#gamepad-mode)
- [The overlay](#the-overlay)
  - [On-screen keyboard](#on-screen-keyboard)
  - [Controller settings](#controller-settings)
  - [Presets](#presets)
  - [Button mapping](#button-mapping)
  - [Input test](#input-test)
- [Haptics](#haptics)
- [When Steam is running](#when-steam-is-running)
- [Troubleshooting](#troubleshooting)

## Modes

puckd has two modes, per controller:

- **Desktop mode** (default at start): the controller is a mouse and keyboard.
- **Gamepad mode**: the controller is a game controller (an Xbox pad by default).

**Hold the Menu button (≡, right of center) for about half a second** to switch. You feel a
click, and a short banner shows the new mode:

![Mode switch banner](img/mode-banner.png)

A short press of Menu still works as a normal button. The overlay's Controller tab also has
a *Mode* entry.

## Desktop mode

| Input | Action |
|---|---|
| right trackpad | move the pointer |
| right pad click, or RT | left click; hold it and move to drag or select text |
| LT | right click |
| left pad click, R3 | middle click |
| left trackpad (or left stick) | scroll; swipe faster to scroll faster |
| right stick | move the pointer |
| A / B / X / Y | Enter / Esc / Backspace / Space |
| d-pad | arrow keys |
| LB / RB | back / forward |
| View / L3 | Tab / Super |
| QAM (⋯) | open the overlay |

The trackpad pointer is filtered: a resting thumb doesn't drift, lifting your thumb doesn't
fling the pointer, and pressing down for a click doesn't move it. When you hold a pad click,
the drag starts once your thumb clearly moves.

If fast and slow movements feel unpredictable, check that your desktop's pointer
acceleration is off for the "puckd Mouse" device (see your desktop's setup page). Then tune
*Pointer speed* and *Pointer acceleration* in the overlay.

All buttons can be rebound in `~/.config/puckd.toml` under `[desktop.buttons]`; see
`puckd.example.toml`.

## Gamepad mode

Games see a regular game controller. The emulated controller is part of the
[preset](#presets):

| Emulated controller | Use it for |
|---|---|
| Xbox 360 (default) | everything; the most compatible |
| Xbox Elite 2 (paddles) | games that support the Elite's four back paddles (L4/L5/R4/R5) |
| Extended (Steam Deck layout) | games and tools that understand the Steam Deck's extra buttons and trackpads |

With the Xbox controllers, the trackpads still move the mouse and scroll in gamepad mode
(*Trackpads as mouse in gamepad mode*, on by default). Turn it off in a preset if a game
reacts badly to the moving cursor.

Rumble works in games through the normal force-feedback interface.

## The overlay

Press **QAM (⋯)** to open or close the overlay, in either mode. It sits at the bottom of
the screen and never takes keyboard focus: what you type goes to the window behind it.
While it is open, the controller only controls the overlay; games don't see that input.
Closing and reopening it brings you back to the same page and entry.

**LB / RB** switch between the *Keyboard* and *Controller* tabs. On the Controller pages,
the d-pad moves through entries, left/right or A changes a value, and **B** goes back.

### On-screen keyboard

![On-screen keyboard](img/keyboard.png)

- Each trackpad moves a cursor over its half of the keyboard (left: blue, right: orange).
  **Click the pad, or pull its trigger**, to type the key under the cursor.
- Or move the white focus with the d-pad or left stick and press **A**.
- **B** Backspace, **X** Space, **Y** Shift, **≡** Enter.
- Shift, Ctrl, Alt, Super and AltGr are sticky: tap once for the next key, twice to lock.
- The labels follow your keyboard layout (QWERTZ, accents and dead keys included).
- You can also click keys with a mouse.

### Controller settings

![Controller tab](img/controller.png)

The header shows the controller's name, the active preset, the mode and the battery. The
tab has three sections:

- **Preset (how games see the controller)**: opens [Presets](#presets) and
  [Button mapping](#button-mapping); the emulated controller, motion sensor device, rumble
  strength and motor swap; trackpads as mouse in gamepad mode. These belong to the active
  preset.
- **Settings (shared by all presets)**: the controller's name, the mode, pointer speed,
  acceleration and inertia, scroll speed and direction, gyro mouse, haptics, when to
  step aside for Steam, and connect / disconnect banners. *Save settings* writes them to `~/.config/puckd.toml`; *Restore
  default settings* resets them (not the preset).
- **Tools**: [Input test](#input-test), and *Turn off controller*.

**Connect / disconnect banners**: when a controller connects or disconnects (turned off,
out of range, unplugged), puckd shows a short banner like the mode-switch one. Turning a
controller off with *Turn off controller* shows none, and neither does stepping aside for
Steam.

**Controller name**: puckd recognizes each controller by its serial number, so a name stays
with the controller whichever dongle slot it uses. Names are typed with the keyboard (see
below) and saved immediately.

### Presets

A preset is how the controller appears to games: the emulated controller, its button
mapping, rumble and the gamepad-mode trackpad option. Your personal settings (pointer
speed and so on) are not part of presets and stay the same when you switch.

![Presets](img/presets.png)

- **Default** always exists. Its button mapping can't be changed; remapping a button while
  on Default starts a new, unsaved preset.
- After changing anything, the page shows *X (modified)*: **Save changes to X**,
  **Save as new preset…**, or **Discard changes**. Presets can also be renamed and deleted.
- **Load** switches to another preset. The list shows each preset's emulated controller and
  the apps it is linked to.

**Naming**: *Save as new preset…* and *Rename…* open a name field above the keyboard. Type
with the overlay keyboard or with your real keyboard; **Enter** or **≡** saves, **Esc**
cancels. The field starts with a suggestion from the window in focus (a Steam game's title,
otherwise the app's name).

![Typing a preset name](img/name-entry.png)

**Per-app presets**: under *Apps*, *Use for <app> (focused)* links the active preset to the
app in focus (the window behind the overlay). Apps already linked are listed below it;
switching one off removes the link. When a linked app gets focus, puckd asks:

![Switch preset popup](img/preset-popup.png)

- **A, Always for this session**: switch now and every time you come back to that app,
  until puckd restarts.
- **X, Once**: switch now, ask again next time.
- **B, No**: keep the current settings and don't ask again this session.

The question disappears after 10 seconds. When you leave the app, puckd switches back to the
preset you last chose by hand (unless you changed something without saving). The behaviour
can be changed under *When an app with a preset gets focus*: ask, automatic or off.
Per-app presets need a compositor that reports the focused window (Sway, Hyprland, …;
not KDE).

### Button mapping

For games that get buttons wrong, each physical button can act as another one, or as nothing,
in gamepad mode. The mapping belongs to the active preset.

![Button mapping](img/mapping.png)

- **A** on an entry: press the button it should act as. Any button works, including A, B and
  the d-pad. After 5 seconds without a press it cancels.

  ![Waiting for a button](img/mapping-capture.png)

- **X** on an entry: pick the target from a grid (the d-pad moves, A chooses, B cancels).

  ![Choosing from the grid](img/mapping-picker.png)

- **Y** on an entry: reset it to itself.

Changes apply immediately, so you can switch between the game and the overlay to try them.

### Input test

Shows everything the controller sends: buttons, sticks (with touch), triggers (with the
full-pull click), both trackpads (touch position, pressure, click), grips and motion
sensors. Every button is shown here instead of acting, so press **LB and RB together** to
go back. QAM always opens and closes the overlay, so it can't be tested here.

![Input test](img/input-test.png)

## Haptics

- A light tick while the pointer moves and per scroll step.
- A click when a trackpad clicks in, and a lighter tick when it releases, on the pad you
  used; also for mouse clicks with the triggers.
- A click on both pads when the mode switches.
- Ticks as the overlay's trackpad cursors move over keys, and clicks when typing.

Turn them off or down under *Haptics* in the Controller tab (or `[haptics]` in the config).

## When Steam is running

puckd steps aside while Steam (or a game that drives the Steam Controller itself, through
SDL3) has the controller open: its virtual devices go away and the other program has the
controller to itself. When it closes the controller, puckd takes over again. Set
*Step aside for* to *steam* to step aside only for Steam, or *never*.

**Exclusive mode** (experimental; [setup](../README.md#optional-exclusive-mode-with-pass-through-for-steam-experimental)):
only puckd opens the controller. Steam and games see a pass-through copy of it, a native
Steam Controller with gyro, trackpads and haptics.

- **Steam takes over.** When Steam starts using the copy, puckd steps aside and a banner says
  so: "Steam has the controller · Hold View + ≡ Menu for puckd". Steam Input then handles
  every button, including its own long-press and Steam-button features.
- **Games share.** A game that drives the Steam Controller itself (SDL3) gets the native
  controller in gamepad mode, but puckd keeps its buttons: QAM opens the overlay, holding ≡
  switches modes (a short press still reaches the game). In desktop mode or with the overlay
  open, the game sees the controller at rest. puckd's own virtual gamepad goes away while
  the game has the native one, so the game doesn't see two controllers. If Steam joins,
  Steam takes over.
- When Steam or the game lets go, puckd resets the controller's settings to the firmware
  defaults and applies its own.
- **Hold View + ≡ Menu for a second** to take the controller back: Steam sees it unplugged,
  and puckd's modes and overlay work again. Hold it again to let Steam have it back.
- These two buttons are held back for a moment when pressed alone, in case the other one
  follows; a quick tap still goes through.
- When Steam closes, puckd takes the controller back by itself.
- *Steam and games may take over* in the Controller tab (`passthrough = false`) turns the
  copy off: Steam and games then never see the controller. `handover_buttons` changes the
  chord.

## Troubleshooting

| Problem | Try |
|---|---|
| Nothing happens | `puckd list`: every slot should say "rw ok". If not, install Steam's udev rules (`steam-devices`) or puckd's rule, then replug the Puck. |
| Pointer speed feels inconsistent | Turn off your desktop's pointer acceleration for "puckd Mouse" (setup page for your desktop). |
| The overlay doesn't appear | Your compositor needs wlr-layer-shell (not available on GNOME). Run `puckd -v` and look for "overlay". |
| A game sees two controllers | Is Steam Input or another driver also active? puckd steps aside when Steam opens the controller. |
| Buttons are mixed up in one game | Make a preset for it, try another emulated controller, or remap buttons. |
| Check what the controller sends | Overlay → Controller → Input test, or `puckd dump` in a terminal. |
