//! The career: missions in order, each a pursuit in its own place, light
//! and weather against its own suspect; how a result is scored and the
//! stars it earns; and what is kept between sessions (the best results
//! and the settings), as a small text file.
//!
//! A mission is caught to be passed (one star); caught quickly, two or
//! three. Each opens the next.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::camera::View;
use crate::carmodel::Kind;
use crate::env::{TimeOfDay, Weather};
use crate::prelude::*;
use crate::pursuit::Wanted;
use crate::road::{Road, Section};
use crate::vehicle::Spec;

/// A mission.
#[derive(Clone, Copy, Debug)]
pub struct Mission {
    pub name: &'static str,
    /// What dispatch says of it, on the briefing.
    pub brief: &'static str,
    pub time: TimeOfDay,
    pub weather: Weather,
    /// Where it starts: this far past where `section` begins (m; before it
    /// if negative).
    pub section: Section,
    pub at: f32,
    pub wanted: Wanted,
    /// Where the suspect starts, ahead (m), and how fast both go (m/s).
    pub ahead: f32,
    pub speed: f32,
    pub traffic: bool,
    /// Police cars joining the chase.
    pub backup: usize,
    /// The time to catch it in (s), if limited.
    pub limit: Option<f32>,
    /// Caught within these times (s): three stars, two.
    pub stars: [f32; 2],
}

const RED: Vec3 = Vec3::new(0.55, 0.02, 0.02);
const SILVER: Vec3 = Vec3::new(0.6, 0.62, 0.65);
const YELLOW: Vec3 = Vec3::new(0.85, 0.55, 0.02);
const GREEN: Vec3 = Vec3::new(0.05, 0.3, 0.12);
const BLUE: Vec3 = Vec3::new(0.02, 0.18, 0.45);
const WHITE: Vec3 = Vec3::new(0.85, 0.85, 0.85);
const BLACK: Vec3 = Vec3::new(0.03, 0.03, 0.035);
const ORANGE: Vec3 = Vec3::new(0.9, 0.3, 0.02);

/// The career's missions, in order.
pub const MISSIONS: [Mission; 8] = [
    Mission {
        name: "First Shift",
        brief: "A stolen saloon is leaving downtown in a hurry. Catch up with it, then ram it off the road or box \
                it in and hold it until it stops.",
        time: TimeOfDay::Day,
        weather: Weather::Clear,
        section: Section::City,
        at: 900.0,
        wanted: Wanted { kind: Kind::Saloon, spec: Spec::saloon(), paint: SILVER, skill: 0.84, aggression: 0.05 },
        ahead: 60.0,
        speed: 14.0,
        traffic: true,
        backup: 0,
        limit: None,
        stars: [60.0, 110.0],
    },
    Mission {
        name: "Joyride",
        brief: "Kids in a borrowed coupe are racing through the hills. Stop them before somebody gets hurt.",
        time: TimeOfDay::Sunset,
        weather: Weather::Clear,
        section: Section::Hills,
        at: 100.0,
        wanted: Wanted { kind: Kind::Coupe, spec: Spec::sports(), paint: RED, skill: 0.9, aggression: 0.25 },
        ahead: 70.0,
        speed: 20.0,
        traffic: true,
        backup: 0,
        limit: None,
        stars: [70.0, 130.0],
    },
    Mission {
        name: "Downtown Dash",
        brief: "A muscle car is tearing through the city at night, weaving between the cars. Three minutes before \
                it reaches the highway.",
        time: TimeOfDay::Night,
        weather: Weather::Clear,
        section: Section::City,
        at: 200.0,
        wanted: Wanted { kind: Kind::Muscle, spec: Spec::sports(), paint: YELLOW, skill: 0.92, aggression: 0.5 },
        ahead: 70.0,
        speed: 18.0,
        traffic: true,
        backup: 0,
        limit: Some(180.0),
        stars: [80.0, 130.0],
    },
    Mission {
        name: "Coast Runner",
        brief: "A getaway car has a head start along the cliffs. Use the nitrous on the straights, and mind the \
                bends.",
        time: TimeOfDay::Day,
        weather: Weather::Clear,
        section: Section::Coast,
        at: 60.0,
        wanted: Wanted { kind: Kind::Coupe, spec: Spec::sports(), paint: GREEN, skill: 0.95, aggression: 0.4 },
        ahead: 160.0,
        speed: 26.0,
        traffic: true,
        backup: 0,
        limit: None,
        stars: [90.0, 150.0],
    },
    Mission {
        name: "Into the Dark",
        brief: "A driver who rams back is heading for the tunnel. Expect a fight in there.",
        time: TimeOfDay::Night,
        weather: Weather::Clear,
        section: Section::Tunnel,
        at: -450.0,
        wanted: Wanted { kind: Kind::Muscle, spec: Spec::sports(), paint: BLACK, skill: 0.94, aggression: 0.8 },
        ahead: 70.0,
        speed: 24.0,
        traffic: true,
        backup: 0,
        limit: None,
        stars: [80.0, 140.0],
    },
    Mission {
        name: "Bridge Burner",
        brief: "A supercar is making for the harbour bridge. It is faster than you on the straight, so catch it \
                in the bends. A unit joins you.",
        time: TimeOfDay::Sunset,
        weather: Weather::Clear,
        section: Section::Bridge,
        at: -700.0,
        wanted: Wanted { kind: Kind::Supercar, spec: Spec::supercar(), paint: ORANGE, skill: 0.95, aggression: 0.3 },
        ahead: 80.0,
        speed: 25.0,
        traffic: true,
        backup: 1,
        limit: Some(180.0),
        stars: [80.0, 130.0],
    },
    Mission {
        name: "Storm Chaser",
        brief: "Rain and a stolen sports car in the dark. The road is slippery: brake early.",
        time: TimeOfDay::Night,
        weather: Weather::Rain,
        section: Section::City,
        at: -300.0,
        wanted: Wanted { kind: Kind::Coupe, spec: Spec::sports(), paint: WHITE, skill: 0.95, aggression: 0.5 },
        ahead: 80.0,
        speed: 20.0,
        traffic: true,
        backup: 1,
        limit: None,
        stars: [90.0, 150.0],
    },
    Mission {
        name: "Most Wanted",
        brief: "The bay's most wanted driver, in the fastest car in the city. Every unit is on it: take it down.",
        time: TimeOfDay::Night,
        weather: Weather::Clear,
        section: Section::Coast,
        at: 300.0,
        wanted: Wanted { kind: Kind::Supercar, spec: Spec::supercar(), paint: BLUE, skill: 1.0, aggression: 0.8 },
        ahead: 120.0,
        speed: 30.0,
        traffic: true,
        backup: 2,
        limit: Some(240.0),
        stars: [100.0, 170.0],
    },
];

/// The suspects of quick chases and patrol calls.
pub const SUSPECTS: [(&str, Wanted); 4] = [
    ("Saloon", Wanted { kind: Kind::Saloon, spec: Spec::saloon(), paint: SILVER, skill: 0.88, aggression: 0.2 }),
    ("Coupe", Wanted { kind: Kind::Coupe, spec: Spec::sports(), paint: RED, skill: 0.92, aggression: 0.35 }),
    ("Muscle car", Wanted { kind: Kind::Muscle, spec: Spec::sports(), paint: YELLOW, skill: 0.95, aggression: 0.6 }),
    ("Supercar", Wanted { kind: Kind::Supercar, spec: Spec::supercar(), paint: BLUE, skill: 0.97, aggression: 0.4 }),
];

/// Where a section begins along `road` (its first sample's s).
pub fn section_start(road: &Road, section: Section) -> f32 {
    let samples = road.samples();
    let n = samples.len();
    (0..n)
        .find(|&i| samples[i].section == section && samples[(i + n - 1) % n].section != section)
        .map_or(0.0, |i| i as f32 * road.spacing())
}

/// How a pursuit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    TakenDown,
    Busted,
    Escaped,
    /// The mission's time ran out.
    OutOfTime,
}

impl Ending {
    pub fn caught(self) -> bool {
        matches!(self, Ending::TakenDown | Ending::Busted)
    }

    /// The results screen's headline.
    pub fn title(self) -> &'static str {
        match self {
            Ending::TakenDown => "TAKEN DOWN",
            Ending::Busted => "BUSTED",
            Ending::Escaped => "SUSPECT ESCAPED",
            Ending::OutOfTime => "OUT OF TIME",
        }
    }
}

/// A pursuit's result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    pub ending: Ending,
    /// How long it took (s).
    pub time: f32,
    /// Hits landed on the suspect.
    pub hits: u32,
    /// How battered the player's car came out (0..1).
    pub damage: f32,
    /// The player's top speed (m/s).
    pub top_speed: f32,
    pub score: u32,
    /// 0 (not caught) to 3.
    pub stars: u8,
}

impl Outcome {
    /// The result of a pursuit that ended so, after `time` seconds, with
    /// `hits` landed, the player's car `damage`d, at most `top_speed`;
    /// stars by `stars` (the times for three and two).
    pub fn new(ending: Ending, time: f32, hits: u32, damage: f32, top_speed: f32, stars: [f32; 2]) -> Outcome {
        let (score, stars) = if ending.caught() {
            let quick = (3000.0 - time * 20.0).max(0.0);
            let style = if ending == Ending::Busted { 1500.0 } else { 1000.0 };
            let care = (1.0 - damage.clamp(0.0, 1.0)) * 2000.0;
            let score = 5000.0 + quick + style + care + hits.min(30) as f32 * 100.0;
            (score as u32, 1 + u8::from(time <= stars[1]) + u8::from(time <= stars[0]))
        } else {
            (hits.min(30) * 100, 0)
        };
        Outcome { ending, time, hits, damage, top_speed, score, stars }
    }
}

/// A mission's best result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Best {
    pub stars: u8,
    pub time: f32,
    pub score: u32,
}

/// How fast the speedometer counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Kmh,
    Mph,
}

impl Units {
    /// `v` (m/s) in these units.
    pub fn of(self, v: f32) -> f32 {
        match self {
            Units::Kmh => v * 3.6,
            Units::Mph => v * 2.236_936,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Units::Kmh => "km/h",
            Units::Mph => "mph",
        }
    }
}

/// The player's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The sounds' and the music's volumes (0..1).
    pub effects: f32,
    pub music: f32,
    pub units: Units,
    pub view: View,
    /// The frame rate and the renderer's statistics.
    pub stats: bool,
    pub fullscreen: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { effects: 0.9, music: 0.6, units: Units::Kmh, view: View::Chase, stats: false, fullscreen: false }
    }
}

/// What is kept between sessions.
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    /// Each mission's best, in order.
    pub best: Vec<Option<Best>>,
    pub settings: Settings,
}

impl Default for Save {
    fn default() -> Save {
        Save { best: alloc::vec![None; MISSIONS.len()], settings: Settings::default() }
    }
}

fn view_name(v: View) -> &'static str {
    match v {
        View::Chase => "chase",
        View::Near => "near",
        View::Bonnet => "bonnet",
        View::Bumper => "bumper",
    }
}

impl Save {
    /// Whether mission `i` may be played: the first, and each after one
    /// passed.
    pub fn unlocked(&self, i: usize) -> bool {
        i == 0 || self.best.get(i - 1).is_some_and(|b| b.is_some_and(|b| b.stars > 0))
    }

    /// All the stars won.
    pub fn stars(&self) -> u32 {
        self.best.iter().flatten().map(|b| u32::from(b.stars)).sum()
    }

    /// Keeps `o` as mission `i`'s best if it is better (more stars, then a
    /// higher score); true if it is.
    pub fn record(&mut self, i: usize, o: &Outcome) -> bool {
        let Some(slot) = self.best.get_mut(i) else { return false };
        if o.stars == 0 {
            return false;
        }
        let better = slot.is_none_or(|b| (o.stars, o.score) > (b.stars, b.score));
        if better {
            *slot = Some(Best { stars: o.stars, time: o.time, score: o.score });
        }
        better
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let s = &self.settings;
        let mut t = String::from("# Interceptor: the career's best results and the settings\n");
        for (i, b) in self.best.iter().enumerate() {
            if let Some(b) = b {
                t.push_str(&format!("mission {} {} {:.2} {}\n", i + 1, b.stars, b.time, b.score));
            }
        }
        t.push_str(&format!("effects {:.2}\nmusic {:.2}\n", s.effects, s.music));
        t.push_str(&format!("units {}\n", if s.units == Units::Mph { "mph" } else { "kmh" }));
        t.push_str(&format!("camera {}\n", view_name(s.view)));
        t.push_str(&format!("stats {}\nfullscreen {}\n", on_off(s.stats), on_off(s.fullscreen)));
        t
    }

    /// Reads a file's text; what it lacks, or garbles, stays as by default.
    pub fn parse(text: &str) -> Save {
        let mut save = Save::default();
        let s = &mut save.settings;
        for line in text.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["mission", n, stars, time, score] => {
                    if let (Ok(n), Ok(stars), Ok(time), Ok(score)) =
                        (n.parse::<usize>(), stars.parse::<u8>(), time.parse::<f32>(), score.parse::<u32>())
                        && (1..=MISSIONS.len()).contains(&n)
                        && (1..=3).contains(&stars)
                        && time.is_finite()
                    {
                        save.best[n - 1] = Some(Best { stars, time, score });
                    }
                }
                ["effects", v] => s.effects = v.parse::<f32>().map_or(s.effects, |v| v.clamp(0.0, 1.0)),
                ["music", v] => s.music = v.parse::<f32>().map_or(s.music, |v| v.clamp(0.0, 1.0)),
                ["units", "mph"] => s.units = Units::Mph,
                ["units", "kmh"] => s.units = Units::Kmh,
                ["camera", v] => {
                    if let Some(view) =
                        [View::Chase, View::Near, View::Bonnet, View::Bumper].into_iter().find(|&w| view_name(w) == *v)
                    {
                        s.view = view;
                    }
                }
                ["stats", v] => s.stats = *v == "on",
                ["fullscreen", v] => s.fullscreen = *v == "on",
                _ => {}
            }
        }
        save
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// `secs` as minutes, seconds and tenths ("1:05.3").
pub fn clock(secs: f32) -> String {
    let t = (secs.max(0.0) * 10.0) as u32;
    format!("{}:{:02}.{}", t / 600, t / 10 % 60, t % 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;

    #[test]
    fn stars_follow_the_time() {
        let m = &MISSIONS[0];
        let fast = Outcome::new(Ending::Busted, 50.0, 3, 0.1, 50.0, m.stars);
        let slow = Outcome::new(Ending::TakenDown, 100.0, 8, 0.5, 50.0, m.stars);
        let later = Outcome::new(Ending::TakenDown, 300.0, 8, 0.5, 50.0, m.stars);
        let lost = Outcome::new(Ending::Escaped, 80.0, 2, 0.5, 50.0, m.stars);
        assert_eq!([fast.stars, slow.stars, later.stars, lost.stars], [3, 2, 1, 0]);
        assert!(fast.score > slow.score && slow.score > later.score && later.score > lost.score);
    }

    #[test]
    fn a_save_reads_back_and_opens_missions() {
        let mut save = Save::default();
        assert!(save.unlocked(0) && !save.unlocked(1));
        let o = Outcome::new(Ending::Busted, 70.0, 2, 0.2, 40.0, MISSIONS[0].stars);
        assert!(save.record(0, &o));
        // A worse result is not kept.
        assert!(!save.record(0, &Outcome::new(Ending::TakenDown, 200.0, 2, 0.2, 40.0, MISSIONS[0].stars)));
        save.settings.units = Units::Mph;
        save.settings.view = View::Bonnet;
        save.settings.music = 0.25;
        let back = Save::parse(&save.to_text());
        assert_eq!(back.best[0].map(|b| b.stars), Some(2));
        assert_eq!(back.settings, save.settings);
        assert!(back.unlocked(1) && !back.unlocked(2));
        // Garbage is ignored.
        let odd = Save::parse("mission 99 3 1 1\nmission 1 7 10 10\neffects loud\nunits furlongs\n");
        assert_eq!(odd, Save::default());
    }

    #[test]
    fn missions_start_on_their_roads() {
        let world = World::new();
        let road = &world.road;
        for section in [Section::City, Section::Hills, Section::Tunnel, Section::Coast, Section::Bridge, Section::Port]
        {
            let len = road.samples().iter().filter(|x| x.section == section).count() as f32 * road.spacing();
            std::println!("{section:?}: from {:.0} m, {len:.0} m long", section_start(road, section));
        }
        for m in &MISSIONS {
            let s = road.wrap(section_start(road, m.section) + m.at);
            std::println!("{}: starts at {s:.0} m ({:?})", m.name, road.at(s).section);
            assert!(m.stars[0] < m.stars[1]);
        }
        assert_eq!(clock(65.34), "1:05.3");
    }
}
