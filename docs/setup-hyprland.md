# Setup for Hyprland

Do the common steps first: [build, install and udev rule](../README.md#installation).
This is the setup puckd is developed and tested on (Hyprland 0.57).

## Start puckd with your session

Hyprland 0.57+ (Lua config), in `~/.config/hypr/hyprland.lua`:

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("puckd")
end)
```

Older Hyprland (`hyprland.conf`):

```ini
exec-once = puckd
```

Use the full path to puckd if it isn't in Hyprland's `PATH`.
Instead you can also use the [systemd user service](../README.md#alternative-systemd-user-service).

## Turn off pointer acceleration for puckd's mouse

puckd shapes the trackpad pointer itself; libinput's acceleration on top makes fast and
slow moves unpredictable. This affects only puckd's virtual mouse (`hyprctl devices` lists
it as `puckd-mouse`).

Hyprland 0.57+ (`hyprland.lua`):

```lua
hl.device({
    name          = "puckd-mouse",
    accel_profile = "flat",
    sensitivity   = 0,
})
```

Older Hyprland (`hyprland.conf`):

```ini
device {
    name = puckd-mouse
    accel_profile = flat
    sensitivity = 0
}
```

`hyprctl reload` applies it. Then tune speed and acceleration in puckd (overlay →
Controller → Pointer speed / acceleration).

## What works on Hyprland

| Feature | |
|---|---|
| Gamepad mode, desktop mode (mouse, keyboard) | yes |
| On-screen keyboard and settings overlay | yes (wlr-layer-shell) |
| Per-app presets | yes (wlr-foreign-toplevel-management) |
| `puckd mouse-study` cursor recording | yes (uses `hyprctl cursorpos`) |

Note: Steam's own "desktop mode" for the controller doesn't reach native Wayland windows on
Hyprland (it uses XTest through XWayland). puckd's devices are real input devices, so they
work everywhere.
