# Setup for KDE Plasma (Wayland)

Do the common steps first: [build, install and udev rule](../README.md#installation).

## Start puckd with your session

Install the autostart entry:

```sh
install -Dm644 dist/puckd.desktop ~/.config/autostart/puckd.desktop
```

Or use *System Settings → Autostart → Add… → Add Application*. If puckd isn't in the
session's `PATH` (e.g. `~/.cargo/bin` or `~/.local/bin`), put the full path in `Exec=`.

Instead of autostart you can also use the [systemd user service](../README.md#alternative-systemd-user-service).

## Turn off pointer acceleration for puckd's mouse

puckd shapes the trackpad pointer itself. KWin would add libinput's acceleration on top,
which makes fast and slow moves unpredictable. This setting affects only puckd's virtual
mouse, not your real mouse:

1. Start puckd (so the device exists).
2. *System Settings → Mouse & Touchpad → Mouse*, select **puckd Mouse** at the top.
3. Set *Pointer acceleration* to *None*. Leave *Pointer speed* in the middle.

Then tune speed and acceleration in puckd (overlay → Controller → Pointer speed /
acceleration).

## What works on KDE

| Feature | |
|---|---|
| Gamepad mode, desktop mode (mouse, keyboard) | yes |
| On-screen keyboard and settings overlay | yes (KWin has wlr-layer-shell) |
| Per-app presets (switch preset when an app gets focus) | **no**: KWin doesn't offer wlr-foreign-toplevel-management. Presets can still be switched by hand. |

Not tested by the author; reports welcome.
