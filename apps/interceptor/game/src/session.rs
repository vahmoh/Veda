//! A session of play: a career mission, a quick chase, a patrol, or the
//! menus' showcase, from the countdown to the result.
//!
//! A **mission** or a **quick chase** puts the player behind its suspect
//! (with any backup), counts down from three, and ends when the suspect is
//! caught or gets away, or its time runs out: then it is scored. On
//! **patrol** the player drives about in the traffic until dispatch calls
//! in a suspect nearby, chase after chase, keeping a tally. The
//! **showcase** is a chase the computer drives, filmed by the cinematic
//! camera, behind the menus.
//!
//! Along the way the radio speaks (the start, the first hit, a suspect
//! getting away, the end) and the countdown beeps: the session's cues.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use vmath::Rng;

use crate::audio::Cue;
use crate::camera::Cinematic;
use crate::career::{self, Ending, MISSIONS, Outcome, SUSPECTS};
use crate::carmodel::Kind;
use crate::driver::{Police, Role};
use crate::env::{Environment, TimeOfDay, Weather};
use crate::game::{Car, Game};
use crate::prelude::*;
use crate::pursuit::{self, Pursuit, State, Wanted};
use crate::render::{CarView, Light};
use crate::road::{Road, Section};
use crate::vehicle::Spec;
use crate::world::World;

/// A quick chase's choices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quick {
    /// Which of [`SUSPECTS`].
    pub suspect: usize,
    pub time: TimeOfDay,
    pub weather: Weather,
    pub traffic: bool,
    /// Police cars joining the chase.
    pub backup: usize,
}

impl Default for Quick {
    fn default() -> Quick {
        Quick { suspect: 1, time: TimeOfDay::Night, weather: Weather::Clear, traffic: true, backup: 0 }
    }
}

/// What is played.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Career mission `n` (of [`MISSIONS`]).
    Career(usize),
    Quick(Quick),
    /// Driving about at `time` in `weather`, called to chase after chase.
    Patrol {
        time: TimeOfDay,
        weather: Weather,
    },
    /// The chase behind the menus.
    Showcase,
}

/// Where a session is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Counting down to the start (s left); the cars wait.
    Countdown(f32),
    Driving,
    /// Its chase is over (s since).
    Over(f32),
}

/// The countdown (s).
pub const COUNTDOWN: f32 = 3.0;
/// How long the radio's words stay up (s).
pub const RADIO_SECS: f32 = 5.0;
/// How far the showcase's police car follows its suspect (m): close enough
/// to film both, never catching it.
const SHOWCASE_GAP: f32 = 26.0;

/// A session of play.
pub struct Session {
    pub mode: Mode,
    pub game: Game,
    /// The chase on (or just over).
    pub chase: Option<Pursuit>,
    pub phase: Phase,
    /// What the radio said last, and how long ago (s).
    pub radio: Option<(String, f32)>,
    /// How the chase ended (missions and quick chases).
    pub outcome: Option<Outcome>,
    /// The time to catch the suspect in (s), if limited.
    pub limit: Option<f32>,
    /// The player's top speed (m/s).
    pub top_speed: f32,
    /// On patrol: suspects caught and lost.
    pub arrests: u32,
    pub escapes: u32,
    stars: [f32; 2],
    /// On patrol, the time to the next call (s); in the showcase, to the
    /// next suspect.
    next: f32,
    calls: u32,
    rng: Rng,
    cinema: Option<Cinematic>,
    cues: Vec<Cue>,
    /// What the radio has said of this chase already.
    said: Said,
}

/// The radio's lines said once a chase.
#[derive(Clone, Copy, Debug, Default)]
struct Said {
    contact: bool,
    smoking: bool,
    getting_away: bool,
    half_time: bool,
    last_seconds: bool,
}

/// A suspect's car in words ("a red coupe").
pub fn describe(w: &Wanted) -> String {
    let colors = [
        ("red", Vec3::new(0.55, 0.02, 0.02)),
        ("silver", Vec3::new(0.6, 0.62, 0.65)),
        ("yellow", Vec3::new(0.85, 0.55, 0.02)),
        ("green", Vec3::new(0.05, 0.3, 0.12)),
        ("blue", Vec3::new(0.02, 0.18, 0.45)),
        ("white", Vec3::new(0.85, 0.85, 0.85)),
        ("black", Vec3::new(0.03, 0.03, 0.035)),
        ("orange", Vec3::new(0.9, 0.3, 0.02)),
    ];
    let color = colors
        .iter()
        .min_by(|a, b| a.1.distance_squared(w.paint).total_cmp(&b.1.distance_squared(w.paint)))
        .map_or("dark", |c| c.0);
    let kind = match w.kind {
        Kind::Coupe => "coupe",
        Kind::Muscle => "muscle car",
        Kind::Supercar => "supercar",
        Kind::Van => "van",
        Kind::Suv => "SUV",
        Kind::Hatchback => "hatchback",
        Kind::Estate => "estate",
        Kind::Taxi => "taxi",
        Kind::Saloon | Kind::Police => "saloon",
    };
    format!("{} {color} {kind}", if color == "orange" { "an" } else { "a" })
}

/// Where along the loop a section is, in words.
fn road_name(s: Section) -> &'static str {
    match s {
        Section::City => "downtown",
        Section::Hills => "in the hills",
        Section::Tunnel => "in the tunnel",
        Section::Coast => "on the coast road",
        Section::Bridge => "on the harbour bridge",
        Section::Port => "by the port",
    }
}

impl Session {
    /// Starts `mode` in `world`.
    pub fn new(world: Arc<World>, mode: Mode) -> Session {
        let mut rng = Rng::new(crate::world::SEED ^ 0x5E55);
        let road_start = |world: &World, section: Section, at: f32| {
            world.road.wrap(career::section_start(&world.road, section) + at)
        };
        // The light, the start, and the chase.
        let (env, start, chase): (Environment, f32, Option<(Wanted, f32, f32)>) = match mode {
            Mode::Career(i) => {
                let m = &MISSIONS[i.min(MISSIONS.len() - 1)];
                let s = road_start(&world, m.section, m.at);
                (Environment::new(m.time, m.weather), s, Some((m.wanted, m.ahead, m.speed)))
            }
            Mode::Quick(q) => {
                let s = rng.range_f32(0.0, world.road.length());
                let w = SUSPECTS[q.suspect.min(SUSPECTS.len() - 1)].1;
                (Environment::new(q.time, q.weather), s, Some((w, 70.0, 20.0)))
            }
            Mode::Patrol { time, weather } => {
                (Environment::new(time, weather), road_start(&world, Section::City, 300.0), None)
            }
            Mode::Showcase => {
                let s = road_start(&world, Section::City, 250.0);
                (Environment::new(TimeOfDay::Night, Weather::Clear), s, Some((SUSPECTS[2].1, 45.0, 22.0)))
            }
        };
        let (traffic, backup, limit, stars) = match mode {
            Mode::Career(i) => {
                let m = &MISSIONS[i.min(MISSIONS.len() - 1)];
                (m.traffic, m.backup, m.limit, m.stars)
            }
            Mode::Quick(q) => (q.traffic, q.backup, None, [80.0, 140.0]),
            Mode::Patrol { .. } | Mode::Showcase => (true, 0, None, [80.0, 140.0]),
        };
        let mut game = Game::new(world, env);
        let speed = chase.map_or(0.0, |c| c.2);
        game.place_player(start, speed);
        game.cars[game.player].siren = chase.is_some();
        if traffic {
            game.start_traffic();
        }
        let mut s = Session {
            mode,
            game,
            chase: None,
            phase: Phase::Driving,
            radio: None,
            outcome: None,
            limit,
            top_speed: 0.0,
            arrests: 0,
            escapes: 0,
            stars,
            next: 0.0,
            calls: 0,
            rng,
            cinema: None,
            cues: Vec::new(),
            said: Said::default(),
        };
        if let Some((wanted, ahead, speed)) = chase {
            s.begin_chase(&wanted, ahead, speed, backup);
        }
        match mode {
            Mode::Career(_) | Mode::Quick(_) => {
                s.phase = Phase::Countdown(COUNTDOWN);
                s.game.paused = true;
                s.cues.push(Cue::Count { last: false });
            }
            Mode::Patrol { .. } => {
                s.next = 12.0;
                s.say(String::from("Dispatch to Interceptor one: patrol Veda Bay. Stand by for calls."));
            }
            Mode::Showcase => {
                let target = s.chase.map(|c| c.suspect);
                s.game.set_autopilot(true, target);
                s.game.autopilot_follows(SHOWCASE_GAP);
                s.cinema = Some(Cinematic::new());
            }
        }
        s
    }

    /// Puts a suspect `ahead` of the player at `speed`, with `backup`
    /// police cars behind, and starts the chase.
    fn begin_chase(&mut self, wanted: &Wanted, ahead: f32, speed: f32, backup: usize) {
        let chase = Pursuit::start(&mut self.game, wanted, ahead, speed);
        let p = self.game.player;
        let (s, _) = self.game.cars[p].place(&self.game.world);
        // The backup behind, in the outer lane and then the inner.
        for k in 0..backup {
            let lane = Road::lane_center(1 - k % 2, 1.0);
            let at = self.game.world.road.wrap(s - 22.0 - 16.0 * k as f32);
            let speed = self.game.cars[p].vehicle.speed();
            let mut car = Car::on_road(&self.game.world, Kind::Police, Spec::interceptor(), at, lane, false, speed);
            car.paint = Vec3::new(0.02, 0.02, 0.02);
            car.siren = true;
            car.role = Role::Police(Police::new(lane, chase.suspect, 1.0));
            self.game.cars.push(car);
        }
        self.game.cars[p].siren = true;
        self.chase = Some(chase);
        self.said = Said::default();
        if self.mode != Mode::Showcase {
            let units = match backup {
                0 => String::new(),
                1 => String::from(" One unit is with you."),
                n => format!(" {n} units are with you."),
            };
            self.say(format!("Suspect vehicle: {}.{units}", describe(wanted)));
        }
    }

    /// What the radio says now.
    fn say(&mut self, text: String) {
        self.radio = Some((text, 0.0));
        self.cues.push(Cue::Radio);
    }

    /// The cues since the last call (for the speakers).
    pub fn take_cues(&mut self) -> Vec<Cue> {
        core::mem::take(&mut self.cues)
    }

    /// The time left to catch the suspect (s), if limited.
    pub fn time_left(&self) -> Option<f32> {
        let chase = self.chase.as_ref()?;
        self.limit.map(|l| (l - chase.time).max(0.0))
    }

    /// Advances `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        if let Some(r) = &mut self.radio {
            r.1 += dt;
        }
        if let Phase::Countdown(t) = self.phase {
            let left = t - dt;
            if left <= 0.0 {
                self.phase = Phase::Driving;
                self.game.paused = false;
                self.cues.push(Cue::Count { last: true });
            } else {
                if left.ceil() < t.ceil() {
                    self.cues.push(Cue::Count { last: false });
                }
                self.phase = Phase::Countdown(left);
            }
            return;
        }
        self.game.update(dt);
        self.top_speed = self.top_speed.max(self.game.player().vehicle.speed().abs());
        if let Phase::Over(t) = &mut self.phase {
            *t += dt;
        }
        // The chase, while it is on.
        let mut ended = None;
        if let Some(chase) = &mut self.chase
            && self.phase == Phase::Driving
        {
            chase.update(&mut self.game, dt);
            ended = match chase.state {
                State::TakenDown => Some(Ending::TakenDown),
                State::Busted => Some(Ending::Busted),
                State::Escaped => Some(Ending::Escaped),
                State::Chasing { .. } if self.limit.is_some_and(|l| chase.time >= l) => Some(Ending::OutOfTime),
                State::Chasing { .. } => None,
            };
        }
        match ended {
            Some(e) => self.end(e),
            None => self.chatter(),
        }
        match self.mode {
            Mode::Patrol { .. } => self.patrol(dt),
            Mode::Showcase => self.showcase(dt),
            _ => {}
        }
    }

    /// The radio's words during a chase.
    fn chatter(&mut self) {
        let Some(chase) = self.chase.filter(|c| c.on() && self.phase == Phase::Driving) else { return };
        let integrity = chase.integrity(&self.game);
        let State::Chasing { escaping, .. } = chase.state else { return };
        let said = self.said;
        if self.mode == Mode::Showcase {
            return;
        }
        if chase.hits > 0 && !said.contact {
            self.said.contact = true;
            self.say(String::from("Contact! Keep the pressure on."));
        } else if integrity < 0.45 && !said.smoking {
            self.said.smoking = true;
            self.say(String::from("It's smoking: one more good hit."));
        } else if escaping > 3.0 && !said.getting_away {
            self.said.getting_away = true;
            self.say(String::from("It's getting away! Use your nitrous."));
        } else if let Some(left) = self.time_left() {
            if left < self.limit.unwrap_or(0.0) * 0.5 && !said.half_time {
                self.said.half_time = true;
                self.say(format!("{} left to stop it.", career::clock(left).split('.').next().unwrap_or("")));
            } else if left < 20.0 && !said.last_seconds {
                self.said.last_seconds = true;
                self.say(String::from("Twenty seconds! Stop it now."));
            }
        }
        if escaping == 0.0 {
            self.said.getting_away = false;
        }
    }

    /// The chase ended so.
    fn end(&mut self, ending: Ending) {
        let Some(chase) = self.chase else { return };
        let p = self.game.player;
        let outcome =
            Outcome::new(ending, chase.time, chase.hits, self.game.cars[p].damage, self.top_speed, self.stars);
        self.phase = Phase::Over(0.0);
        if self.mode == Mode::Showcase {
            return;
        }
        self.cues.push(if ending.caught() { Cue::Win } else { Cue::Lose });
        let line = match ending {
            Ending::Busted => "Suspect in custody. Nice work.",
            Ending::TakenDown => "Suspect vehicle disabled. Good takedown.",
            Ending::Escaped => "We've lost the suspect. Pursuit terminated.",
            Ending::OutOfTime => "Out of time: pursuit called off.",
        };
        self.say(String::from(line));
        match self.mode {
            Mode::Patrol { .. } => {
                if ending.caught() {
                    self.arrests += 1;
                } else {
                    self.escapes += 1;
                }
            }
            _ => self.outcome = Some(outcome),
        }
    }

    /// On patrol: the suspect of a chase over is taken away, and dispatch
    /// calls in the next.
    fn patrol(&mut self, dt: f32) {
        if let Phase::Over(t) = self.phase
            && t > 6.0
        {
            if let Some(c) = self.chase.take() {
                self.game.remove_car(c.suspect);
                self.remove_backup();
            }
            self.phase = Phase::Driving;
            self.next = self.rng.range_f32(15.0, 30.0);
            let p = self.game.player;
            self.game.cars[p].siren = false;
        }
        if self.chase.is_some() {
            return;
        }
        self.next -= dt;
        if self.next <= 0.0 {
            self.calls += 1;
            let wanted = SUSPECTS[self.rng.below(SUSPECTS.len() as u32) as usize].1;
            let ahead = self.rng.range_f32(160.0, 260.0);
            let speed = self.game.player().vehicle.speed().max(16.0);
            let (s, _) = self.game.cars[self.game.player].place(&self.game.world);
            let place = road_name(self.game.world.road.at(s + ahead).section);
            let chase = Pursuit::start(&mut self.game, &wanted, ahead, speed);
            self.chase = Some(chase);
            self.said = Said::default();
            self.top_speed = 0.0;
            let p = self.game.player;
            self.game.cars[p].siren = true;
            self.say(format!("All units: {} speeding {place}, just ahead of you. Pursue.", describe(&wanted)));
        }
    }

    /// The police cars that joined a chase leave with its suspect.
    fn remove_backup(&mut self) {
        let p = self.game.player;
        while let Some(i) =
            (0..self.game.cars.len()).rev().find(|&i| i != p && matches!(self.game.cars[i].role, Role::Police(_)))
        {
            self.game.remove_car(i);
        }
    }

    /// In the showcase: a new suspect after each chase.
    fn showcase(&mut self, _dt: f32) {
        if let Phase::Over(t) = self.phase
            && t > 2.5
        {
            if let Some(c) = self.chase.take() {
                self.game.remove_car(c.suspect);
            }
            self.calls += 1;
            let wanted = SUSPECTS[(self.calls as usize + 2) % SUSPECTS.len()].1;
            let speed = self.game.player().vehicle.speed().max(18.0);
            self.begin_chase(&wanted, 45.0, speed, 0);
            let target = self.chase.map(|c| c.suspect);
            self.game.set_autopilot(true, target);
            self.game.autopilot_follows(SHOWCASE_GAP);
            self.phase = Phase::Driving;
        }
    }

    /// What to draw now, filmed by the session's camera (`dt` since the
    /// last frame).
    pub fn frame(&mut self, dt: f32) -> (Vec<CarView>, Vec<Light>) {
        let (cars, lights) = self.game.frame(dt);
        if let Some(cinema) = &mut self.cinema {
            let g = &self.game;
            let hero = &g.cars[g.player];
            let chased = self.chase.filter(|c| c.on()).map(|c| &g.cars[c.suspect].vehicle);
            self.game.camera = cinema.update(&g.world, &hero.vehicle, hero.hint, chased, dt);
        }
        (cars, lights)
    }

    /// The light of the session.
    pub fn env(&self) -> Environment {
        self.game.env
    }

    /// The suspect's distance ahead (m), while a chase is on.
    pub fn gap(&self) -> Option<f32> {
        self.chase.filter(|c| c.on()).map(|c| c.gap)
    }

    /// How close the suspect is to escaping (0..1) and to being busted.
    pub fn danger(&self) -> (f32, f32) {
        match self.chase.map(|c| c.state) {
            Some(State::Chasing { escaping, busting }) => {
                (escaping / pursuit::ESCAPE_TIME, busting / pursuit::BUST_TIME)
            }
            _ => (0.0, 0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::STEP;

    fn world() -> Arc<World> {
        Arc::new(World::new())
    }

    #[test]
    fn a_mission_counts_down_then_chases_to_a_result() {
        let mut s = Session::new(world(), Mode::Career(0));
        assert!(matches!(s.phase, Phase::Countdown(_)));
        // The cars wait through the countdown, beeping each second.
        let start = s.game.player().vehicle.pos;
        for _ in 0..(COUNTDOWN / STEP) as usize + 2 {
            s.update(STEP);
        }
        assert_eq!(s.phase, Phase::Driving);
        let cues = s.take_cues();
        let beeps = cues.iter().filter(|c| matches!(c, Cue::Count { .. })).count();
        assert_eq!(beeps, 4, "{cues:?}");
        assert!(s.game.player().vehicle.pos.distance(start) < 0.5);
        // The computer drives the player's car after the suspect.
        let target = s.chase.map(|c| c.suspect);
        s.game.set_autopilot(true, target);
        let mut t = 0.0;
        while s.outcome.is_none() && t < 400.0 {
            s.update(1.0 / 30.0);
            t += 1.0 / 30.0;
        }
        let o = s.outcome.expect("the chase never ended");
        std::println!("{o:?}");
        assert!(o.ending.caught(), "{o:?}");
        assert!(matches!(s.phase, Phase::Over(_)));
    }

    #[test]
    fn backup_joins_the_chase() {
        // "Most Wanted": two units behind the player, in its lanes.
        let s = Session::new(world(), Mode::Career(7));
        let suspect = s.chase.unwrap().suspect;
        let units: Vec<&Car> =
            s.game.cars.iter().filter(|c| matches!(c.role, Role::Police(p) if p.target == suspect)).collect();
        assert_eq!(units.len(), 2);
        let player = s.game.player().place(&s.game.world).0;
        for u in &units {
            let (at, lateral) = u.place(&s.game.world);
            assert!(u.siren && s.game.world.road.delta(at, player) > 15.0 && lateral > 1.0, "{at} {lateral}");
        }
        assert!((units[0].place(&s.game.world).1 - units[1].place(&s.game.world).1).abs() > 2.0);
    }

    #[test]
    fn dispatch_calls_on_patrol() {
        let mut s = Session::new(world(), Mode::Patrol { time: TimeOfDay::Day, weather: Weather::Clear });
        assert!(s.chase.is_none());
        for _ in 0..(14.0 * 30.0) as usize {
            s.update(1.0 / 30.0);
        }
        let chase = s.chase.expect("no call came");
        assert!(chase.on());
        let (text, _) = s.radio.clone().unwrap();
        assert!(text.starts_with("All units"), "{text}");
        assert!(s.game.cars[chase.suspect].vehicle.pos.distance(s.game.player().vehicle.pos) > 100.0);
    }

    #[test]
    fn the_showcase_films_a_chase_and_starts_another() {
        let mut s = Session::new(world(), Mode::Showcase);
        let first = s.chase.unwrap().suspect;
        let mut eyes = Vec::new();
        for _ in 0..(40.0 * 30.0) as usize {
            s.update(1.0 / 30.0);
            s.frame(1.0 / 30.0);
            eyes.push(s.game.camera.eye);
            assert!(s.game.camera.eye.is_finite());
        }
        // The camera cut between shots.
        let cuts = eyes.windows(2).filter(|w| w[0].distance(w[1]) > 8.0).count();
        assert!(cuts >= 3, "{cuts} cuts");
        assert!(s.chase.is_some() && s.radio.is_none());
        let _ = first;
    }
}
