//! How cars drive: a rigid body on four tyres.
//!
//! Each tyre's grip follows a Pacejka "magic formula" of its slip angle,
//! limited with its drive or braking force by a friction circle; the
//! loads on the tyres shift with braking, accelerating and cornering. The
//! engine has a torque curve and an automatic gearbox; drag grows with
//! the square of the speed; slopes pull; over a crest at speed a car
//! flies. A few assists keep it easy to drive without hiding the physics,
//! as a modern car's do: steering that lightens with speed and helps catch
//! a slide, anti-lock brakes, traction control, and stability control,
//! which reins in a car turning faster than its steering asks (off while
//! the handbrake is held, so that it can still be thrown into a slide).
//!
//! Positions are world metres (x, z on the ground, y up); a car's heading
//! `yaw` points it along `(sin yaw, cos yaw)` in x and z, and a positive
//! yaw rate turns it left. Car space has +z forward and +x to the left.

use crate::prelude::*;

/// Gravity (m/s²).
pub const G: f32 = 9.81;

/// Which wheels the engine drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    Rear,
    Front,
    All,
}

/// A car's mechanical make-up.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub mass: f32,
    /// Yaw inertia (kg m²).
    pub inertia: f32,
    pub wheelbase: f32,
    /// The centre of mass's distance behind the front axle.
    pub cg_front: f32,
    pub cg_height: f32,
    pub track: f32,
    pub wheel_radius: f32,
    /// The tyres' friction coefficient.
    pub grip: f32,
    pub drive: Drive,
    /// The engine's peak torque (N m) and where it peaks, idles and
    /// stops (rpm).
    pub torque: f32,
    pub rpm_peak: f32,
    pub rpm_idle: f32,
    pub rpm_red: f32,
    /// Gear ratios (first to sixth) and the final drive.
    pub gears: [f32; 6],
    pub reverse: f32,
    pub final_drive: f32,
    /// Half the air's density times the drag area (N per (m/s)²).
    pub drag: f32,
    /// Downforce (N per (m/s)²).
    pub downforce: f32,
    /// The brakes' strongest force (N) and its share at the front.
    pub brake: f32,
    pub brake_front: f32,
    /// The front wheels' greatest steering angle (radians).
    pub steer_max: f32,
    /// Half the body's length and width (for collisions).
    pub half_length: f32,
    pub half_width: f32,
}

impl Spec {
    /// The police's interceptor: a V8 saloon, all-wheel drive.
    pub const fn interceptor() -> Spec {
        Spec {
            mass: 1850.0,
            inertia: 3400.0,
            wheelbase: 2.95,
            cg_front: 1.40,
            cg_height: 0.55,
            track: 1.62,
            wheel_radius: 0.36,
            grip: 1.12,
            drive: Drive::All,
            torque: 600.0,
            rpm_peak: 4500.0,
            rpm_idle: 800.0,
            rpm_red: 6800.0,
            gears: [3.6, 2.3, 1.62, 1.22, 0.98, 0.80],
            reverse: 3.2,
            final_drive: 3.36,
            drag: 0.48,
            downforce: 0.05,
            brake: 21000.0,
            brake_front: 0.62,
            steer_max: 0.62,
            half_length: 2.52,
            half_width: 0.96,
        }
    }

    /// A family saloon.
    pub const fn saloon() -> Spec {
        Spec {
            mass: 1500.0,
            inertia: 2600.0,
            torque: 280.0,
            rpm_peak: 4200.0,
            rpm_red: 6200.0,
            grip: 1.0,
            drive: Drive::Front,
            drag: 0.40,
            downforce: 0.0,
            brake: 16000.0,
            ..Spec::interceptor()
        }
    }

    /// A fast coupe.
    pub const fn sports() -> Spec {
        Spec {
            mass: 1450.0,
            inertia: 2300.0,
            wheelbase: 2.70,
            cg_front: 1.28,
            cg_height: 0.46,
            track: 1.64,
            wheel_radius: 0.35,
            grip: 1.2,
            drive: Drive::Rear,
            torque: 480.0,
            rpm_peak: 5500.0,
            rpm_red: 7600.0,
            gears: [3.4, 2.2, 1.6, 1.25, 1.0, 0.83],
            final_drive: 3.5,
            drag: 0.36,
            downforce: 0.12,
            brake: 19000.0,
            half_length: 2.28,
            half_width: 0.97,
            ..Spec::interceptor()
        }
    }

    /// A supercar.
    pub const fn supercar() -> Spec {
        Spec {
            mass: 1500.0,
            inertia: 2300.0,
            wheelbase: 2.70,
            cg_front: 1.45,
            cg_height: 0.42,
            track: 1.70,
            wheel_radius: 0.35,
            grip: 1.3,
            drive: Drive::All,
            torque: 700.0,
            rpm_peak: 6200.0,
            rpm_red: 8500.0,
            gears: [3.2, 2.2, 1.65, 1.3, 1.06, 0.88],
            final_drive: 3.4,
            drag: 0.34,
            downforce: 0.25,
            brake: 22000.0,
            half_length: 2.30,
            half_width: 1.01,
            ..Spec::interceptor()
        }
    }

    /// A heavy van.
    pub const fn van() -> Spec {
        Spec {
            mass: 2600.0,
            inertia: 6000.0,
            wheelbase: 3.30,
            cg_front: 1.55,
            cg_height: 0.85,
            track: 1.70,
            wheel_radius: 0.37,
            grip: 0.95,
            drive: Drive::Rear,
            torque: 420.0,
            rpm_peak: 3000.0,
            rpm_red: 5000.0,
            final_drive: 3.9,
            drag: 0.55,
            downforce: 0.0,
            brake: 22000.0,
            half_length: 2.65,
            half_width: 1.0,
            ..Spec::interceptor()
        }
    }

    /// Weight on the front and rear axles at rest (N).
    fn static_loads(&self) -> (f32, f32) {
        let b = self.wheelbase - self.cg_front;
        let w = self.mass * G;
        (w * b / self.wheelbase, w * self.cg_front / self.wheelbase)
    }

    /// The engine's torque at `rpm` with the throttle wide open (N m).
    pub fn engine_torque(&self, rpm: f32) -> f32 {
        let x = (rpm - self.rpm_peak) / (self.rpm_red - self.rpm_idle);
        let shape = (1.0 - 1.6 * x * x).clamp(0.35, 1.0);
        if rpm > self.rpm_red { 0.0 } else { self.torque * shape }
    }
}

/// What a driver does (each in -1..1 or 0..1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Controls {
    pub throttle: f32,
    pub brake: f32,
    /// -1 hard right .. 1 hard left.
    pub steer: f32,
    pub handbrake: bool,
}

/// What the ground under a point is like.
#[derive(Clone, Copy, Debug)]
pub struct Ground {
    pub height: f32,
    pub normal: Vec3,
    /// How well tyres grip it (1 tarmac).
    pub grip: f32,
}

/// The tyre's lateral grip as a fraction of the most it gives, by slip
/// angle (radians): Pacejka's formula, peaking near 7 degrees.
pub fn lateral_grip(alpha: f32) -> f32 {
    let (b, c, e) = (11.0, 1.55, 0.95);
    let ba = b * alpha;
    (c * (ba - e * (ba - ba.atan())).atan()).sin()
}

/// A car's state.
#[derive(Clone, Debug)]
pub struct Vehicle {
    pub spec: Spec,
    /// The ground point under the centre of mass.
    pub pos: Vec3,
    pub yaw: f32,
    /// Velocity on the ground (x, z), and up.
    pub vel: Vec2,
    pub vy: f32,
    pub yaw_rate: f32,
    pub airborne: bool,
    /// The front wheels' angle (radians, left positive).
    pub steer: f32,
    pub rpm: f32,
    /// 1..=6 forward, -1 reverse.
    pub gear: i32,
    shift: f32,
    /// What the driver's pedals do after the assists (for the lights and
    /// the sounds).
    pub throttle: f32,
    pub braking: f32,
    /// Each wheel's turn (radians) and how much it slides (m/s): front
    /// left, front right, rear left, rear right.
    pub spin: f32,
    pub slip: [f32; 4],
    /// Accelerations along and across the car, smoothed (m/s²).
    pub accel: Vec2,
    /// The body's pitch, roll and heave on its springs (for the looks).
    pub pitch: f32,
    pub roll: f32,
    pitch_rate: f32,
    roll_rate: f32,
    /// The ground's normal, smoothed.
    pub up: Vec3,
    /// Stability control (on in every car unless switched off).
    pub esc: bool,
    /// The engine's output as a share of its make's (a nitrous boost
    /// above 1, a tired or held-back engine below).
    pub power: f32,
}

impl Vehicle {
    /// A car of `spec` standing at `pos`, heading `yaw`.
    pub fn new(spec: Spec, pos: Vec3, yaw: f32) -> Vehicle {
        Vehicle {
            spec,
            pos,
            yaw,
            vel: Vec2::ZERO,
            vy: 0.0,
            yaw_rate: 0.0,
            airborne: false,
            steer: 0.0,
            rpm: spec.rpm_idle,
            gear: 1,
            shift: 0.0,
            throttle: 0.0,
            braking: 0.0,
            spin: 0.0,
            slip: [0.0; 4],
            accel: Vec2::ZERO,
            pitch: 0.0,
            roll: 0.0,
            pitch_rate: 0.0,
            roll_rate: 0.0,
            up: Vec3::Y,
            esc: true,
            power: 1.0,
        }
    }

    pub fn forward(&self) -> Vec2 {
        Vec2::new(self.yaw.sin(), self.yaw.cos())
    }

    pub fn left(&self) -> Vec2 {
        Vec2::new(self.yaw.cos(), -self.yaw.sin())
    }

    /// Changing gear (the engine's drive is cut for a moment).
    pub fn shifting(&self) -> bool {
        self.shift > 0.0
    }

    /// Speed along the heading (m/s, negative backwards).
    pub fn speed(&self) -> f32 {
        self.vel.dot(self.forward())
    }

    /// How fast it slides sideways (m/s, left positive).
    pub fn sideways(&self) -> f32 {
        self.vel.dot(self.left())
    }

    /// Sets it moving at `speed` along its heading.
    pub fn set_speed(&mut self, speed: f32) {
        self.vel = self.forward() * speed;
        self.match_gear();
    }

    /// The gear and engine speed that suit the speed (after a jump of
    /// speed, such as a start at speed).
    pub fn match_gear(&mut self) {
        let v = self.speed().abs();
        self.gear = 1;
        while self.gear < 6 && self.rpm_for(v, self.gear) > self.spec.rpm_red * 0.82 {
            self.gear += 1;
        }
        self.rpm = self.rpm_for(v, self.gear).max(self.spec.rpm_idle);
    }

    /// The engine's speed at `v` (m/s) in `gear`.
    fn rpm_for(&self, v: f32, gear: i32) -> f32 {
        let ratio = self.ratio(gear);
        v / self.spec.wheel_radius * ratio * 60.0 / core::f32::consts::TAU
    }

    fn ratio(&self, gear: i32) -> f32 {
        let g = if gear < 0 { self.spec.reverse } else { self.spec.gears[(gear.max(1) - 1) as usize] };
        g * self.spec.final_drive
    }

    /// The wheels' places from the centre of mass, in car space (x left,
    /// z forward): front left, front right, rear left, rear right.
    fn wheel_offsets(&self) -> [Vec2; 4] {
        let s = &self.spec;
        let (a, b, t) = (s.cg_front, s.wheelbase - s.cg_front, s.track * 0.5);
        [Vec2::new(t, a), Vec2::new(-t, a), Vec2::new(t, -b), Vec2::new(-t, -b)]
    }

    /// Advances `dt` seconds (keep it at 1/120 s or less) with `c`, on
    /// `ground` (the ground under any point).
    pub fn step(&mut self, c: &Controls, dt: f32, ground: &dyn Fn(Vec3) -> Ground) {
        let s = self.spec;
        let f = self.forward();
        let l = self.left();
        let v_long = self.vel.dot(f);
        let v_lat = self.vel.dot(l);
        let speed = self.vel.length();
        let here = ground(self.pos);

        // Steering: less lock as the speed rises, and a hand catching a
        // slide (towards where the car is going).
        let lock = s.steer_max / (1.0 + speed * 0.055);
        let slide = if v_long > 3.0 { (v_lat / v_long.max(1.0)).atan() } else { 0.0 };
        let catch = if c.handbrake { 0.0 } else { slide * 0.55 };
        let target = (c.steer * lock + catch).clamp(-s.steer_max, s.steer_max);
        let rate = 3.2 + 3.0 / (1.0 + speed * 0.1);
        self.steer += (target - self.steer).clamp(-rate * dt, rate * dt);

        // Pedals: braking while rolling forward, reverse once stopped.
        let mut throttle = c.throttle.clamp(0.0, 1.0);
        let mut brake = c.brake.clamp(0.0, 1.0);
        if self.gear < 0 {
            // In reverse the brake pedal drives backwards, the throttle
            // brakes, until it changes direction.
            if c.throttle > 0.1 && v_long > -0.6 {
                self.gear = 1;
            } else {
                throttle = c.brake;
                brake = c.throttle;
            }
        } else if c.brake > 0.1 && c.throttle < 0.1 && v_long < 0.6 {
            self.gear = -1;
            throttle = c.brake;
            brake = 0.0;
        }
        self.throttle = throttle;
        self.braking = brake;

        // The gearbox.
        self.shift = (self.shift - dt).max(0.0);
        if self.gear > 0 && self.shift <= 0.0 {
            let rpm = self.rpm_for(v_long.max(0.0), self.gear);
            if rpm > s.rpm_red * 0.94 && self.gear < 6 {
                self.gear += 1;
                self.shift = 0.16;
            } else if self.gear > 1
                && self.rpm_for(v_long.max(0.0), self.gear - 1) < s.rpm_red * 0.72
                && rpm < s.rpm_peak * 0.62
            {
                self.gear -= 1;
                self.shift = 0.12;
            }
        }
        let ratio = self.ratio(self.gear);
        let wheel_rpm = self.rpm_for(v_long.abs(), self.gear);
        // The clutch slips below idle; the throttle revs a free engine.
        let free = s.rpm_idle + throttle * (s.rpm_peak - s.rpm_idle) * 0.7;
        let target_rpm =
            if wheel_rpm < free && v_long.abs() < 8.0 { free.max(wheel_rpm) } else { wheel_rpm.max(s.rpm_idle) };
        self.rpm += (target_rpm - self.rpm) * (1.0 - (-dt * 18.0).exp());
        let limiter = self.rpm >= s.rpm_red;
        let torque =
            if self.shift > 0.0 || limiter || self.airborne { 0.0 } else { s.engine_torque(self.rpm) * throttle };
        let drive_dir = if self.gear < 0 { -1.0 } else { 1.0 };
        let mut drive = torque * ratio * 0.88 / s.wheel_radius * drive_dir;
        // Engine braking off the throttle.
        if throttle < 0.05 && self.gear > 0 && v_long > 1.0 {
            drive -= (self.rpm / s.rpm_red) * 900.0 * ratio / 10.0;
        }

        // Loads on the tyres: weight, downforce, and the shift of weight
        // with the accelerations.
        let (front, rear) = s.static_loads();
        let down = s.downforce * speed * speed;
        let shift_long = s.mass * self.accel.y * s.cg_height / s.wheelbase;
        let shift_lat = s.mass * self.accel.x * s.cg_height / s.track;
        let lf = (front + down * 0.45 - shift_long).max(0.0);
        let lr = (rear + down * 0.55 + shift_long).max(0.0);
        let fb = s.cg_front / s.wheelbase;
        // A positive lateral acceleration (to the left) loads the right.
        let loads = [
            (lf * 0.5 - shift_lat * (1.0 - fb)).max(lf * 0.05),
            (lf * 0.5 + shift_lat * (1.0 - fb)).max(lf * 0.05),
            (lr * 0.5 - shift_lat * fb).max(lr * 0.05),
            (lr * 0.5 + shift_lat * fb).max(lr * 0.05),
        ];
        let driven = |w: usize| match s.drive {
            Drive::Rear => w >= 2,
            Drive::Front => w < 2,
            Drive::All => true,
        };
        let driven_share = |w: usize| match s.drive {
            Drive::All => {
                if w < 2 {
                    0.2
                } else {
                    0.3
                }
            }
            _ => 0.5,
        };

        let mut force = Vec2::ZERO;
        let mut torque_yaw = 0.0;
        let offsets = self.wheel_offsets();
        for w in 0..4 {
            if self.airborne {
                self.slip[w] = 0.0;
                continue;
            }
            let d_car = offsets[w];
            // The offset in the world.
            let d = l * d_car.x + f * d_car.y;
            // The wheel's velocity: the body's, plus the turn's.
            let vw = self.vel + Vec2::new(self.yaw_rate * d.y, -self.yaw_rate * d.x);
            let delta = if w < 2 { self.steer } else { 0.0 };
            let (sd, cd) = delta.sin_cos();
            let wf = f * cd + l * sd;
            let wl = l * cd - f * sd;
            let vl = vw.dot(wf);
            let vt = vw.dot(wl);
            let mu = s.grip * here.grip;
            let most = mu * loads[w];
            let locked = c.handbrake && w >= 2;
            let wforce = if locked {
                // A locked wheel slides: friction against its motion,
                // weaker than rolling grip.
                let v = vw.length();
                if v > 0.3 { -vw / v * most * 0.75 } else { -vw * most * 0.5 }
            } else {
                let alpha = vt.atan2(vl.abs().max(1.5));
                let mut fy = -most * lateral_grip(alpha);
                // At a crawl, damp the sideways motion instead.
                if vw.length() < 1.5 {
                    fy = -vt * s.mass * 0.5;
                }
                let mut fx = 0.0;
                if driven(w) {
                    fx += drive * driven_share(w);
                }
                if brake > 0.0 && vl.abs() > 0.05 {
                    let share = if w < 2 { s.brake_front } else { 1.0 - s.brake_front } * 0.5;
                    // Anti-lock: never more than the tyre holds.
                    fx -= vl.signum() * (s.brake * share * brake).min(most * 0.98);
                }
                // Rolling resistance: about 1.2% of the load.
                if vl.abs() > 0.1 {
                    fx -= vl.signum() * 0.012 * loads[w];
                }
                // The friction circle: drive or brake and corner share it;
                // traction control keeps the drive within it.
                let total = (fx * fx + fy * fy).sqrt();
                if total > most && total > 0.0 {
                    let k = most / total;
                    fx *= k;
                    fy *= k;
                }
                wf * fx + wl * fy
            };
            self.slip[w] = if locked { vw.length() } else { vt.abs() };
            force += wforce;
            // The yaw torque: d x F about +y.
            torque_yaw += d.y * wforce.x - d.x * wforce.y;
        }
        // Stability control: the yaw rate the steering asks for at this
        // speed (a bicycle's, short of what the tyres hold); turning faster
        // than that is the tail stepping out, which braking one wheel
        // reins in, and the engine is cut back.
        if self.esc && !c.handbrake && !self.airborne && v_long > 5.0 {
            // A mildly understeering car's: an understeer gradient of 0.02
            // radians per g.
            let asked = v_long * self.steer.tan() / (s.wheelbase + 0.02 * v_long * v_long / G);
            let most = s.grip * G / v_long;
            let asked = asked.clamp(-most, most);
            let excess = self.yaw_rate - asked;
            // Beyond a margin, and only the tail stepping out (turning more
            // than asked), never a car turning less.
            if excess.abs() > 0.1 + asked.abs() * 0.15 && excess.signum() == self.yaw_rate.signum() {
                let brake_torque = 0.5 * s.track * s.grip * s.mass * G * 0.25;
                torque_yaw -= excess.signum() * (excess.abs() * s.inertia * 6.0).min(brake_torque);
                force -= f * (excess.abs().min(0.5) * s.mass * 2.0);
            }
        }
        // Drag, and gravity down slopes.
        force -= self.vel * (s.drag * speed);
        let n = self.up;
        if !self.airborne {
            force += Vec2::new(n.x, n.z) * (s.mass * G);
        }

        // Integrate.
        let accel = force / s.mass;
        self.vel += accel * dt;
        self.yaw_rate += torque_yaw / s.inertia * dt;
        if self.airborne {
            // In the air the body keeps turning, slowly damped.
            self.yaw_rate *= 1.0 - dt * 0.5;
        }
        // A car standing still stays still.
        if !self.airborne && self.vel.length() < 0.05 && throttle < 0.05 {
            self.vel = Vec2::ZERO;
            self.yaw_rate *= 0.5;
        }
        self.yaw += self.yaw_rate * dt;
        self.pos.x += self.vel.x * dt;
        self.pos.z += self.vel.y * dt;
        let k = 1.0 - (-dt / 0.08).exp();
        self.accel += (Vec2::new(accel.dot(self.left()), accel.dot(self.forward())) - self.accel) * k;
        self.spin += v_long * dt / s.wheel_radius;

        // Up and down: follow the ground, or fly off a crest.
        let g = ground(self.pos);
        if self.airborne {
            self.vy -= G * dt;
            self.pos.y += self.vy * dt;
            if self.pos.y <= g.height {
                // Landing: the springs take it.
                self.pitch_rate -= self.vy.min(0.0) * 0.06;
                self.pos.y = g.height;
                self.vy = 0.0;
                self.airborne = false;
            }
        } else {
            let rise = (g.height - self.pos.y) / dt;
            // The ground falls away faster than gravity can follow, and
            // the car was going up: it leaves the ground.
            if rise < self.vy - G * dt * 1.5 && self.vy > 1.0 && g.height < self.pos.y - 0.05 {
                self.airborne = true;
                self.vy -= G * dt;
                self.pos.y += self.vy * dt;
            } else {
                self.vy = rise.clamp(-30.0, 30.0);
                self.pos.y = g.height;
            }
        }
        if !self.airborne {
            self.up = (self.up + (g.normal - self.up) * (1.0 - (-dt * 12.0).exp())).normalize_or(Vec3::Y);
        }

        // The body on its springs: it dives under braking, squats under
        // power, leans out of corners.
        let target_pitch = (self.accel.y * 0.006).clamp(-0.06, 0.06);
        let target_roll = (-self.accel.x * 0.007).clamp(-0.07, 0.07);
        let (kp, kd) = (90.0, 11.0);
        self.pitch_rate += ((target_pitch - self.pitch) * kp - self.pitch_rate * kd) * dt;
        self.roll_rate += ((target_roll - self.roll) * kp - self.roll_rate * kd) * dt;
        self.pitch += self.pitch_rate * dt;
        self.roll += self.roll_rate * dt;
    }

    /// Car space to the world, with the body's pitch and roll.
    pub fn transform(&self) -> Mat4 {
        let fwd = Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos());
        let up = self.up;
        let f = (fwd - up * fwd.dot(up)).normalize_or(fwd);
        let left = up.cross(f).normalize_or(Vec3::X);
        let basis = Mat4::from_cols(left.extend(0.0), up.extend(0.0), f.extend(0.0), self.pos.extend(1.0));
        // Pitch about the left axis (nose down positive), roll about the
        // forward one.
        basis * Mat4::from_quat(Quat::from_rotation_z(self.roll) * Quat::from_rotation_x(self.pitch))
    }

    /// The body's corners on the ground (x, z): front left, front right,
    /// rear right, rear left.
    pub fn corners(&self) -> [Vec2; 4] {
        let (f, l) = (self.forward(), self.left());
        let (hl, hw) = (self.spec.half_length, self.spec.half_width);
        // The box's middle is the body's, a little behind the centre of
        // mass on most cars; close enough to share it.
        let c = self.pos.xz();
        [c + f * hl + l * hw, c + f * hl - l * hw, c - f * hl - l * hw, c - f * hl + l * hw]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(_: Vec3) -> Ground {
        Ground { height: 0.0, normal: Vec3::Y, grip: 1.0 }
    }

    /// Drives with `c` for `secs`, calling `each` every step.
    fn drive(v: &mut Vehicle, c: Controls, secs: f32, mut each: impl FnMut(&Vehicle, f32)) {
        let dt = 1.0 / 120.0;
        let mut t = 0.0;
        while t < secs {
            v.step(&c, dt, &flat);
            t += dt;
            each(v, t);
            assert!(v.vel.x.is_finite() && v.vel.y.is_finite() && v.yaw.is_finite(), "the state went bad");
        }
    }

    #[test]
    fn the_interceptor_accelerates_brakes_and_tops_out_plausibly() {
        let mut v = Vehicle::new(Spec::interceptor(), Vec3::ZERO, 0.0);
        let mut to_100 = None;
        let full = Controls { throttle: 1.0, ..Controls::default() };
        drive(&mut v, full, 60.0, |v, t| {
            if to_100.is_none() && v.speed() >= 100.0 / 3.6 {
                to_100 = Some(t);
            }
        });
        let to_100 = to_100.expect("never reached 100 km/h");
        let top = v.speed() * 3.6;
        std::println!("0-100 km/h in {to_100:.1} s, top speed {top:.0} km/h, gear {}", v.gear);
        assert!((3.0..6.0).contains(&to_100), "0-100 in {to_100} s");
        assert!((220.0..300.0).contains(&top), "top speed {top} km/h");
        // From 100 km/h to a stop.
        v.set_speed(100.0 / 3.6);
        let start = v.pos;
        let mut stopped = None;
        drive(&mut v, Controls { brake: 1.0, ..Controls::default() }, 6.0, |v, t| {
            if stopped.is_none() && v.speed() < 0.1 {
                stopped = Some((t, v.pos.distance(start)));
            }
        });
        let (t, d) = stopped.expect("never stopped");
        std::println!("100-0 km/h in {t:.1} s, {d:.0} m");
        assert!((25.0..50.0).contains(&d), "stopped in {d} m");
    }

    #[test]
    fn it_turns_and_holds_a_corner() {
        let mut v = Vehicle::new(Spec::interceptor(), Vec3::ZERO, 0.0);
        v.set_speed(15.0);
        // A steady left turn at 15 m/s: the yaw rate is positive, the car
        // keeps its speed roughly and does not spin.
        let c = Controls { throttle: 0.35, steer: 0.6, ..Controls::default() };
        drive(&mut v, c, 6.0, |_, _| {});
        assert!(v.yaw_rate > 0.2, "yaw rate {}", v.yaw_rate);
        assert!(v.sideways().abs() < 3.0, "sliding at {} m/s", v.sideways());
        // Lateral acceleration within the tyres' grip.
        let lat = v.speed() * v.yaw_rate;
        assert!(lat < 1.3 * G, "{lat} m/s² sideways");
    }

    #[test]
    fn the_handbrake_swings_the_tail() {
        let mut v = Vehicle::new(Spec::interceptor(), Vec3::ZERO, 0.0);
        v.set_speed(20.0);
        let c = Controls { steer: 1.0, handbrake: true, ..Controls::default() };
        let mut most = 0.0f32;
        drive(&mut v, c, 1.2, |v, _| most = most.max(v.sideways().abs()));
        assert!(most > 3.0, "only {most} m/s sideways");
        // Let go, straighten up: it recovers.
        drive(&mut v, Controls { throttle: 0.5, ..Controls::default() }, 4.0, |_, _| {});
        assert!(v.sideways().abs() < 1.5, "still sliding at {}", v.sideways());
    }

    #[test]
    fn straight_at_speed_it_stays_straight_and_reverse_works() {
        let mut v = Vehicle::new(Spec::sports(), Vec3::ZERO, 0.3);
        v.set_speed(60.0);
        drive(&mut v, Controls { throttle: 1.0, ..Controls::default() }, 5.0, |_, _| {});
        assert!((v.yaw - 0.3).abs() < 0.01, "drifted to {}", v.yaw);
        // Stop, then back up.
        let mut w = Vehicle::new(Spec::saloon(), Vec3::ZERO, 0.0);
        drive(&mut w, Controls { brake: 1.0, ..Controls::default() }, 3.0, |_, _| {});
        assert!(w.speed() < -1.0, "reversing at {}", w.speed());
        assert_eq!(w.gear, -1);
        drive(&mut w, Controls { throttle: 1.0, ..Controls::default() }, 3.0, |_, _| {});
        assert!(w.speed() > 1.0 && w.gear >= 1);
    }

    #[test]
    fn over_a_crest_it_flies_and_lands() {
        let mut v = Vehicle::new(Spec::interceptor(), Vec3::ZERO, 0.0);
        v.set_speed(40.0);
        // A ramp up then a drop.
        let ramp = |p: Vec3| {
            let z = p.z;
            let h = if z < 20.0 { z * 0.15 } else { (3.0 - (z - 20.0) * 0.3).max(0.0) };
            let slope = if z < 20.0 {
                -0.15
            } else if h > 0.0 {
                0.3
            } else {
                0.0
            };
            Ground { height: h, normal: Vec3::new(0.0, 1.0, slope).normalize(), grip: 1.0 }
        };
        let mut flew = false;
        let dt = 1.0 / 120.0;
        for _ in 0..360 {
            v.step(&Controls { throttle: 0.5, ..Controls::default() }, dt, &ramp);
            flew |= v.airborne;
        }
        assert!(flew, "never left the ground");
        assert!(!v.airborne && v.pos.y.abs() < 0.01, "still in the air");
    }
}
