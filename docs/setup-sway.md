# Setup for Sway

Do the common steps first: [build, install and udev rule](../README.md#installation).

## Start puckd with your session

In `~/.config/sway/config`:

```
exec puckd
```

Use the full path to puckd if it isn't in Sway's `PATH`.
Instead you can also use the [systemd user service](../README.md#alternative-systemd-user-service).

## Turn off pointer acceleration for puckd's mouse

puckd shapes the trackpad pointer itself; libinput's acceleration on top makes fast and
slow moves unpredictable. This affects only puckd's virtual mouse
(`swaymsg -t get_inputs` lists it as `4617:23489:puckd_Mouse`):

```
input "4617:23489:puckd_Mouse" {
    accel_profile flat
    pointer_accel 0
}
```

Reload with `swaymsg reload`. Then tune speed and acceleration in puckd (overlay →
Controller → Pointer speed / acceleration).

## What works on Sway

| Feature | |
|---|---|
| Gamepad mode, desktop mode (mouse, keyboard) | yes |
| On-screen keyboard and settings overlay | yes (wlr-layer-shell) |
| Per-app presets | yes (wlr-foreign-toplevel-management) |

Not tested by the author; reports welcome.
