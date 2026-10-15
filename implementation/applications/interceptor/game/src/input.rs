// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The keyboard into a driver's controls: keys are on or off, pedals and
//! steering are not, so presses ramp the controls in and releases ramp
//! them out, steering quicker to centre than to turn, and gentler at
//! speed. Down brakes, and once the car has stopped backs it up (up then
//! braking it, until it has stopped and drives it forward again).

use crate::vehicle::{Controls, STOPPED};

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
    /// The controls after `dt` seconds of `keys` at `speed` (m/s, negative
    /// backwards).
    pub fn update(&mut self, keys: Keys, speed: f32, dt: f32) -> Controls {
        let c = &mut self.controls;
        // Which way it goes changes once it has stopped, the key the other
        // way alone held.
        if c.reverse {
            if keys.up && !keys.down && speed > -STOPPED {
                c.reverse = false;
            }
        } else if keys.down && !keys.up && speed < STOPPED {
            c.reverse = true;
        }
        let (go, stop) = if c.reverse { (keys.down, keys.up) } else { (keys.up, keys.down) };
        let toward = |v: f32, target: f32, rate: f32| v + (target - v).clamp(-rate * dt, rate * dt);
        c.throttle = toward(c.throttle, f32::from(u8::from(go)), if go { 4.0 } else { 8.0 });
        c.brake = toward(c.brake, f32::from(u8::from(stop)), if stop { 6.0 } else { 10.0 });
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

    #[test]
    fn down_brakes_then_backs_up_and_up_goes_forward_again() {
        let mut d = Driver::default();
        let down = Keys { down: true, ..Keys::default() };
        // Rolling forward: the brakes.
        let c = (0..10).fold(Controls::default(), |_, _| d.update(down, 10.0, 0.05));
        assert!(!c.reverse && c.brake == 1.0 && c.throttle == 0.0, "{c:?}");
        // Stopped: backing up.
        let c = (0..10).fold(c, |_, _| d.update(down, 0.0, 0.05));
        assert!(c.reverse && c.throttle == 1.0 && c.brake == 0.0, "{c:?}");
        // Up while still rolling back brakes; stopped, it goes forward.
        let up = Keys { up: true, ..Keys::default() };
        let c = (0..10).fold(c, |_, _| d.update(up, -3.0, 0.05));
        assert!(c.reverse && c.brake == 1.0 && c.throttle == 0.0, "{c:?}");
        let c = (0..10).fold(c, |_, _| d.update(up, 0.0, 0.05));
        assert!(!c.reverse && c.throttle == 1.0, "{c:?}");
    }

    #[test]
    fn the_keyboard_backs_a_car_up() {
        use crate::prelude::*;
        use crate::vehicle::{Ground, Vehicle};
        let flat = |_: Vec3| Ground { height: 0.0, normal: Vec3::Y, grip: 1.0 };
        let mut v = Vehicle::new(crate::carmodel::Kind::Police, Vec3::ZERO, 0.0);
        v.set_speed(10.0);
        let mut d = Driver::default();
        let down = Keys { down: true, ..Keys::default() };
        for _ in 0..(5.0 * 120.0) as usize {
            let c = d.update(down, v.speed(), 1.0 / 120.0);
            v.step(&c, 1.0 / 120.0, &flat);
        }
        assert!(v.speed() < -2.0 && v.gear == -1, "going {} m/s in gear {}", v.speed(), v.gear);
    }
}
