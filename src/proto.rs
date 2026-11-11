//! Steam Controller (2026, "Triton") wire protocol, as used through the Puck dongle.
//!
//! Layouts follow SDL3 `SDL_hidapi_steam_triton.c` / `steam/controller_structs.h`.
//! All multi-byte fields are little endian; offsets below include the report ID byte.

pub const ID_STATE: u8 = 0x42;
pub const ID_BATTERY: u8 = 0x43;
pub const ID_STATE_BLE: u8 = 0x45;
pub const ID_WIRELESS_X: u8 = 0x46;
pub const ID_STATE_TS: u8 = 0x47;
pub const ID_WIRELESS: u8 = 0x79;

pub const WIRELESS_DISCONNECT: u8 = 1;
pub const WIRELESS_CONNECT: u8 = 2;

pub const FEATURE_REPORT_LEN: usize = 64;
pub const ID_SET_SETTINGS_VALUES: u8 = 0x87;
pub const ID_SET_DEFAULT_DIGITAL_MAPPINGS: u8 = 0x85;
pub const ID_LOAD_DEFAULT_SETTINGS: u8 = 0x8e;

pub const SETTING_LIZARD_MODE: u8 = 9;
pub const SETTING_IMU_MODE: u8 = 48;
pub const LIZARD_MODE_OFF: u16 = 0;
pub const LIZARD_MODE_ON: u16 = 1;
pub const IMU_MODE_RAW_ACCEL_GYRO: u16 = 0x08 | 0x10;

pub const OUT_HAPTIC_RUMBLE: u8 = 0x80;
pub const OUT_HAPTIC_PULSE: u8 = 0x81;
pub const OUT_HAPTIC_COMMAND: u8 = 0x82;

/// Haptic actuator selection for 0x81/0x82. Measured on the 2026 controller with
/// `puckd haptic-sides`: 0 = left, 1 = right, 2 = both, 3 = nothing (the 2015 controller
/// used 1/2/3, as SDL's headers note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left = 0,
    Right = 1,
    Both = 2,
}

/// Haptic command types for 0x82 (`haptic_type_t`).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum HapticCmd {
    Off = 0,
    Tick = 1,
    Click = 2,
}

/// Button bits of the `buttons` u32.
pub mod btn {
    pub const A: u32 = 0x0000_0001;
    pub const B: u32 = 0x0000_0002;
    pub const X: u32 = 0x0000_0004;
    pub const Y: u32 = 0x0000_0008;
    pub const QAM: u32 = 0x0000_0010;
    pub const R3: u32 = 0x0000_0020;
    /// Right center button (≡). SDL calls this bit VIEW and maps it to Start.
    pub const MENU: u32 = 0x0000_0040;
    pub const R4: u32 = 0x0000_0080;
    pub const R5: u32 = 0x0000_0100;
    pub const RB: u32 = 0x0000_0200;
    pub const DPAD_DOWN: u32 = 0x0000_0400;
    pub const DPAD_RIGHT: u32 = 0x0000_0800;
    pub const DPAD_LEFT: u32 = 0x0000_1000;
    pub const DPAD_UP: u32 = 0x0000_2000;
    /// Left center button (⧉). SDL calls this bit MENU and maps it to Back.
    pub const VIEW: u32 = 0x0000_4000;
    pub const L3: u32 = 0x0000_8000;
    pub const STEAM: u32 = 0x0001_0000;
    pub const L4: u32 = 0x0002_0000;
    pub const L5: u32 = 0x0004_0000;
    pub const LB: u32 = 0x0008_0000;
    pub const RSTICK_TOUCH: u32 = 0x0010_0000;
    pub const RPAD_TOUCH: u32 = 0x0020_0000;
    pub const RPAD_CLICK: u32 = 0x0040_0000;
    pub const RT_CLICK: u32 = 0x0080_0000;
    pub const LSTICK_TOUCH: u32 = 0x0100_0000;
    pub const LPAD_TOUCH: u32 = 0x0200_0000;
    pub const LPAD_CLICK: u32 = 0x0400_0000;
    pub const LT_CLICK: u32 = 0x0800_0000;
    pub const RGRIP_TOUCH: u32 = 0x1000_0000;
    pub const LGRIP_TOUCH: u32 = 0x2000_0000;

    /// (config name, bit). Names describe the physical buttons (menu = right ≡, view = left ⧉),
    /// which is the reverse of the VIEW/MENU names in SDL's Triton driver.
    pub const NAMES: &[(&str, u32)] = &[
        ("a", A),
        ("b", B),
        ("x", X),
        ("y", Y),
        ("qam", QAM),
        ("r3", R3),
        ("view", VIEW),
        ("r4", R4),
        ("r5", R5),
        ("rb", RB),
        ("dpad_down", DPAD_DOWN),
        ("dpad_right", DPAD_RIGHT),
        ("dpad_left", DPAD_LEFT),
        ("dpad_up", DPAD_UP),
        ("menu", MENU),
        ("l3", L3),
        ("steam", STEAM),
        ("l4", L4),
        ("l5", L5),
        ("lb", LB),
        ("rstick_touch", RSTICK_TOUCH),
        ("rpad_touch", RPAD_TOUCH),
        ("rpad_click", RPAD_CLICK),
        ("rt_click", RT_CLICK),
        ("lstick_touch", LSTICK_TOUCH),
        ("lpad_touch", LPAD_TOUCH),
        ("lpad_click", LPAD_CLICK),
        ("lt_click", LT_CLICK),
        ("rgrip_touch", RGRIP_TOUCH),
        ("lgrip_touch", LGRIP_TOUCH),
    ];

    pub fn from_name(name: &str) -> Option<u32> {
        let n = name.to_ascii_lowercase();
        NAMES.iter().find(|(k, _)| *k == n).map(|&(_, v)| v)
    }

    pub fn names(bits: u32) -> Vec<&'static str> {
        NAMES.iter().filter(|(_, v)| bits & v != 0).map(|(k, _)| *k).collect()
    }
}

/// Decoded controller state.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct State {
    pub report_id: u8,
    pub seq: u8,
    pub buttons: u32,
    /// 0..=32767
    pub lt: i16,
    pub rt: i16,
    /// Sticks: Y is up-positive (as on the wire).
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    /// Trackpads: -32768..32767, Y up-positive.
    pub lpad_x: i16,
    pub lpad_y: i16,
    pub lpad_pressure: u16,
    pub rpad_x: i16,
    pub rpad_y: i16,
    pub rpad_pressure: u16,
    /// Raw IMU tick (u32 µs for 0x42/0x45, u16 in 32 µs units for 0x47).
    pub imu_tick: u32,
    /// Microseconds per IMU tick unit and wrap modulus.
    pub imu_tick_us: u32,
    pub imu_tick_bits: u32,
    /// Accel ±2 g full scale, gyro ±2000 °/s full scale (raw axes).
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    State(State),
    Battery { charge_state: u8, level: u8 },
    Wireless(u8),
    Other(u8),
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn i16_at(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub fn parse(b: &[u8]) -> Option<Input> {
    let id = *b.first()?;
    Some(match id {
        ID_STATE | ID_STATE_BLE | ID_STATE_TS => {
            if b.len() < 46 {
                return None;
            }
            let mut s = State {
                report_id: id,
                seq: b[1],
                buttons: u32_at(b, 2),
                lt: i16_at(b, 6),
                rt: i16_at(b, 8),
                lx: i16_at(b, 10),
                ly: i16_at(b, 12),
                rx: i16_at(b, 14),
                ry: i16_at(b, 16),
                ..Default::default()
            };
            // 0x47 inserts a u16 trackpad timestamp at 18 and shrinks the IMU
            // timestamp to u16, so the IMU samples land at the same offsets.
            let p = if id == ID_STATE_TS { 20 } else { 18 };
            s.lpad_x = i16_at(b, p);
            s.lpad_y = i16_at(b, p + 2);
            s.lpad_pressure = u16_at(b, p + 4);
            s.rpad_x = i16_at(b, p + 6);
            s.rpad_y = i16_at(b, p + 8);
            s.rpad_pressure = u16_at(b, p + 10);
            if id == ID_STATE_TS {
                s.imu_tick = u16_at(b, 32) as u32;
                s.imu_tick_us = 32;
                s.imu_tick_bits = 16;
            } else {
                s.imu_tick = u32_at(b, 30);
                s.imu_tick_us = 1;
                s.imu_tick_bits = 32;
            }
            s.accel = [i16_at(b, 34), i16_at(b, 36), i16_at(b, 38)];
            s.gyro = [i16_at(b, 40), i16_at(b, 42), i16_at(b, 44)];
            Input::State(s)
        }
        ID_BATTERY if b.len() >= 3 => Input::Battery { charge_state: b[1], level: b[2] },
        ID_WIRELESS | ID_WIRELESS_X if b.len() >= 2 => Input::Wireless(b[1]),
        other => Input::Other(other),
    })
}

pub fn charge_state_name(s: u8) -> &'static str {
    match s {
        0 => "reset",
        1 => "discharging",
        2 => "charging",
        3 => "validating source",
        4 => "charged",
        _ => "unknown",
    }
}

/// Feature report 0x01 / SET_SETTINGS_VALUES with `{u8 num, u16 value}` entries.
pub fn settings_report(settings: &[(u8, u16)]) -> [u8; FEATURE_REPORT_LEN] {
    let mut b = [0u8; FEATURE_REPORT_LEN];
    b[0] = 1;
    b[1] = ID_SET_SETTINGS_VALUES;
    b[2] = (settings.len() * 3) as u8;
    for (i, &(num, val)) in settings.iter().take(20).enumerate() {
        let o = 3 + i * 3;
        b[o] = num;
        b[o + 1..o + 3].copy_from_slice(&val.to_le_bytes());
    }
    b
}

/// Feature report: load the firmware's default settings (undoes what Steam set).
pub fn load_default_settings_report() -> [u8; FEATURE_REPORT_LEN] {
    let mut b = [0u8; FEATURE_REPORT_LEN];
    b[..3].copy_from_slice(&[1, ID_LOAD_DEFAULT_SETTINGS, 0]);
    b
}

/// Feature report: restore lizard mode's default button mappings (Steam clears them).
pub fn default_digital_mappings_report() -> [u8; FEATURE_REPORT_LEN] {
    let mut b = [0u8; FEATURE_REPORT_LEN];
    b[..3].copy_from_slice(&[1, ID_SET_DEFAULT_DIGITAL_MAPPINGS, 0]);
    b
}

/// Feature report: turn the controller off (ID_TURN_OFF_CONTROLLER with the "off!" magic
/// Steam uses for its controllers).
pub fn power_off_report() -> [u8; FEATURE_REPORT_LEN] {
    let mut b = [0u8; FEATURE_REPORT_LEN];
    b[..7].copy_from_slice(&[1, 0x9f, 4, b'o', b'f', b'f', b'!']);
    b
}

/// Output report 0x80: rumble. `left` = low frequency / strong, `right` = high frequency / weak.
pub fn rumble_report(left: u16, right: u16) -> [u8; 10] {
    let mut b = [0u8; 10];
    b[0] = OUT_HAPTIC_RUMBLE;
    // b[1] type, b[2..4] intensity: 0
    b[4..6].copy_from_slice(&left.to_le_bytes());
    // b[6] left gain dB
    b[7..9].copy_from_slice(&right.to_le_bytes());
    // b[9] right gain dB
    b
}

/// Output report 0x82: a canned haptic effect (tick/click) on one or both pads.
pub fn haptic_command_report(side: Side, cmd: HapticCmd, gain_db: i8) -> [u8; 4] {
    [OUT_HAPTIC_COMMAND, side as u8, cmd as u8, gain_db as u8]
}

/// Output report 0x81: on/off pulse train.
pub fn haptic_pulse_report(side: Side, on_us: u16, off_us: u16, repeat: u16) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = OUT_HAPTIC_PULSE;
    b[1] = side as u8;
    b[2..4].copy_from_slice(&on_us.to_le_bytes());
    b[4..6].copy_from_slice(&off_us.to_le_bytes());
    b[6..8].copy_from_slice(&repeat.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lizard_off_bytes() {
        let r = settings_report(&[(SETTING_LIZARD_MODE, LIZARD_MODE_OFF)]);
        assert_eq!(&r[..6], &[1, 0x87, 3, 9, 0, 0]);
        assert!(r[6..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_real_report() {
        // Captured from a Puck slot (report 0x42, 54 bytes).
        let hex = concat!(
            "425200000000000000008f01080202fe",
            "14010000000000000000000000006228",
            "4e6ea4fdc326af341a01e8ffeaff4471",
            "df203711a72e"
        );
        let b: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
        assert_eq!(b.len(), 54);
        let Some(Input::State(s)) = parse(&b) else { panic!() };
        assert_eq!(s.seq, 0x52);
        assert_eq!(s.buttons, 0);
        assert_eq!((s.lx, s.ly), (0x018f, 0x0208));
        assert_eq!(s.imu_tick, 0x6e4e_2862);
        assert_eq!(s.accel[0], 0xfda4u16 as i16);
    }

    #[test]
    fn haptic_sides() {
        assert_eq!(haptic_command_report(Side::Left, HapticCmd::Click, 0), [0x82, 0, 2, 0]);
        assert_eq!(haptic_command_report(Side::Right, HapticCmd::Tick, -6), [0x82, 1, 1, 0xfa]);
        assert_eq!(haptic_command_report(Side::Both, HapticCmd::Click, 0)[1], 2);
    }

    #[test]
    fn rumble_layout() {
        let r = rumble_report(0x1234, 0xabcd);
        assert_eq!(r, [0x80, 0, 0, 0, 0x34, 0x12, 0, 0xcd, 0xab, 0]);
    }
}
