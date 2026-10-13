// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keyboard into a driver's controls: keys are on or off, pedals and
//! steering are not, so presses ramp the controls in and releases ramp
//! them out, steering quicker to centre than to turn, and gentler at
//! speed.

use crate::vehicle::Controls;

/// The keys held for driving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub handbrake: bool,
    /// The nitrous.
    pub boost: bool,
}

/// Ramps keys into controls.
#[derive(Clone, Copy, Debug, Default)]
pub struct Driver {
    controls: Controls,
}

impl Driver {
    /// The controls after `dt` seconds of `keys` at `speed` (m/s).
    pub fn update(&mut self, keys: Keys, speed: f32, dt: f32) -> Controls {
        let c = &mut self.controls;
        let toward = |v: f32, target: f32, rate: f32| v + (target - v).clamp(-rate * dt, rate * dt);
        c.throttle = toward(c.throttle, f32::from(u8::from(keys.up)), if keys.up { 4.0 } else { 8.0 });
        c.brake = toward(c.brake, f32::from(u8::from(keys.down)), if keys.down { 6.0 } else { 10.0 });
        let want = f32::from(u8::from(keys.left)) - f32::from(u8::from(keys.right));
        // Turning in takes longer at speed; letting go centres quickly.
        let turn_rate = 3.5 / (1.0 + speed.abs() * 0.025);
        let rate = if want == 0.0 || want.signum() != c.steer.signum() { 6.0 } else { turn_rate };
        c.steer = toward(c.steer, want, rate);
        c.handbrake = keys.handbrake;
        *c
    }

    /// Lets go of everything at once (the window lost the keyboard).
    pub fn release(&mut self) {
        self.controls = Controls::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ramp_in_and_out() {
        let mut d = Driver::default();
        let k = Keys { up: true, left: true, ..Keys::default() };
        let c = d.update(k, 0.0, 0.05);
        assert!(c.throttle > 0.1 && c.throttle < 0.5);
        assert!(c.steer > 0.0 && c.steer < 0.5);
        for _ in 0..20 {
            d.update(k, 0.0, 0.05);
        }
        let c = d.update(k, 0.0, 0.05);
        assert_eq!((c.throttle, c.steer), (1.0, 1.0));
        // Let go: it centres within a fifth of a second.
        let mut c = c;
        for _ in 0..4 {
            c = d.update(Keys::default(), 0.0, 0.05);
        }
        assert_eq!(c.steer, 0.0);
        assert_eq!(c.throttle, 0.0);
    }
}
