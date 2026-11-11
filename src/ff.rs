//! Force-feedback effect bookkeeping for a uinput gamepad, mixed down to two rumble motors.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::uinput::{FfEffect, ev::*};

#[derive(Debug, Clone, Copy)]
enum Kind {
    /// (strong / low frequency, weak / high frequency)
    Rumble(u16, u16),
    /// Periodic/constant effects: one magnitude on both motors.
    Level(u16),
}

#[derive(Debug, Clone, Copy)]
struct Play {
    start: Instant,
    end: Option<Instant>,
}

#[derive(Debug, Clone)]
struct Stored {
    kind: Kind,
    length: Duration,
    delay: Duration,
    play: Option<Play>,
}

#[derive(Debug)]
pub struct FfEngine {
    effects: HashMap<i16, Stored>,
    gain: u32,
}

impl Default for FfEngine {
    fn default() -> Self {
        Self { effects: HashMap::new(), gain: 0xffff }
    }
}

pub const SUPPORTED: &[u16] =
    &[FF_RUMBLE, FF_PERIODIC, FF_CONSTANT, FF_SINE, FF_SQUARE, FF_TRIANGLE, FF_SAW_UP, FF_SAW_DOWN, FF_GAIN];
pub const MAX_EFFECTS: u32 = 16;

fn scale_i16(m: i16) -> u16 {
    ((m as i32).unsigned_abs() * 2).min(0xffff) as u16
}

impl FfEngine {
    /// Store an uploaded effect. Returns a negative errno for unsupported effects.
    pub fn upload(&mut self, e: &FfEffect, now: Instant) -> i32 {
        let kind = match e.kind {
            FF_RUMBLE => {
                let (s, w) = e.rumble();
                Kind::Rumble(s, w)
            }
            FF_PERIODIC => Kind::Level(scale_i16(e.periodic().2)),
            FF_CONSTANT => Kind::Level(scale_i16(e.constant())),
            _ => return -libc::EINVAL,
        };
        let length = Duration::from_millis(e.replay_length as u64);
        let delay = Duration::from_millis(e.replay_delay as u64);
        let prev_play = self.effects.get(&e.id).and_then(|s| s.play);
        // Re-uploading a playing effect updates it in place (SDL does this to change magnitudes).
        let play = prev_play.map(|p| Play {
            start: p.start,
            end: if length.is_zero() { None } else { Some(p.start.max(now) + length) },
        });
        self.effects.insert(e.id, Stored { kind, length, delay, play });
        0
    }

    pub fn erase(&mut self, id: i16) {
        self.effects.remove(&id);
    }

    pub fn play(&mut self, id: i16, count: i32, now: Instant) {
        let Some(s) = self.effects.get_mut(&id) else { return };
        if count <= 0 {
            s.play = None;
            return;
        }
        let start = now + s.delay;
        let end = if s.length.is_zero() { None } else { Some(start + s.length * count.min(1000) as u32) };
        s.play = Some(Play { start, end });
    }

    pub fn set_gain(&mut self, gain: i32) {
        self.gain = gain.clamp(0, 0xffff) as u32;
    }

    /// Current (strong, weak) motor levels.
    pub fn mix(&mut self, now: Instant) -> (u16, u16) {
        let (mut strong, mut weak) = (0u32, 0u32);
        for s in self.effects.values_mut() {
            let Some(p) = s.play else { continue };
            if let Some(end) = p.end
                && now >= end
            {
                s.play = None;
                continue;
            }
            if now < p.start {
                continue;
            }
            match s.kind {
                Kind::Rumble(a, b) => {
                    strong += a as u32;
                    weak += b as u32;
                }
                Kind::Level(l) => {
                    strong += l as u32;
                    weak += l as u32;
                }
            }
        }
        let g = |v: u32| (v.min(0xffff) * self.gain / 0xffff) as u16;
        (g(strong), g(weak))
    }

    /// Next moment the mix changes on its own (effect start or end).
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        self.effects
            .values()
            .filter_map(|s| s.play)
            .flat_map(|p| [Some(p.start), p.end])
            .flatten()
            .filter(|&t| t > now)
            .min()
    }

    /// Feed one EV_FF event (play/stop or gain) from the client.
    pub fn handle_ff_event(&mut self, code: u16, value: i32, now: Instant) {
        if code == FF_GAIN {
            self.set_gain(value);
        } else {
            self.play(code as i16, value, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rumble(id: i16, s: u16, w: u16, len: u16) -> FfEffect {
        let mut e = FfEffect { kind: FF_RUMBLE, id, replay_length: len, ..Default::default() };
        e.u[0] = (s as u64) | ((w as u64) << 16);
        e
    }

    #[test]
    fn play_and_expire() {
        let t0 = Instant::now();
        let mut ff = FfEngine::default();
        assert_eq!(ff.upload(&rumble(0, 1000, 2000, 100), t0), 0);
        assert_eq!(ff.mix(t0), (0, 0));
        ff.play(0, 1, t0);
        assert_eq!(ff.mix(t0), (1000, 2000));
        assert_eq!(ff.next_change(t0), Some(t0 + Duration::from_millis(100)));
        assert_eq!(ff.mix(t0 + Duration::from_millis(101)), (0, 0));
    }

    #[test]
    fn gain_and_sum() {
        let t0 = Instant::now();
        let mut ff = FfEngine::default();
        ff.upload(&rumble(0, 0xffff, 0, 0), t0);
        ff.upload(&rumble(1, 0xffff, 0x100, 0), t0);
        ff.play(0, 1, t0);
        ff.play(1, 1, t0);
        assert_eq!(ff.mix(t0), (0xffff, 0x100));
        ff.set_gain(0x7fff);
        assert_eq!(ff.mix(t0).0, 0x7fff);
        ff.erase(0);
        ff.erase(1);
        assert_eq!(ff.mix(t0), (0, 0));
    }
}
