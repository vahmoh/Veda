// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Interceptor for the voice agent: what the game shows (the menus, the
//! career and its stars, a chase's suspect, distance, time and damage, a
//! patrol's tally, the results, the settings) and what it does (starting a
//! mission, a quick chase or a patrol; pausing, resuming, restarting and
//! leaving play; changing the settings), through the same code paths as
//! the keyboard and the menus. The user drives: the agent has no controls
//! for the car.
//!
//! [`serve`] answers the agent between frames; [`Link`] registers the game
//! with the agent service.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use interceptor_game::camera::View as CameraView;
use interceptor_game::career::{self, MISSIONS, SUSPECTS, Stats, Units};
use interceptor_game::env::{TimeOfDay, Weather};
use interceptor_game::pursuit::State;
use interceptor_game::session::{self, Mode, Phase, Quick};
use num_traits::Float;
use vui::agent::{self, Action, AgentServer, AppAgentInfo, Value, arg_bool, arg_opt_int, arg_opt_str, object};

pub use vui::agent::Registration as Link;

use crate::app::{App, Back, Screen};

/// The keys, for questions such as "how do I use the nitrous?".
const CONTROLS: &str = "Arrow keys or WASD drive (down brakes, then reverses), Space is the handbrake, Shift fires \
                        the nitrous, Q switches the siren, H sounds the horn, C changes the camera, R puts the car \
                        back on the road, Esc pauses";

const TIMES: [(&str, TimeOfDay); 3] =
    [("day", TimeOfDay::Day), ("sunset", TimeOfDay::Sunset), ("night", TimeOfDay::Night)];
const WEATHERS: [(&str, Weather); 2] = [("clear", Weather::Clear), ("rain", Weather::Rain)];
const VIEWS: [(&str, CameraView); 4] = [
    ("chase", CameraView::Chase),
    ("near", CameraView::Near),
    ("bonnet", CameraView::Bonnet),
    ("bumper", CameraView::Bumper),
];

fn mission_names() -> Vec<&'static str> {
    MISSIONS.iter().map(|m| m.name).collect()
}

fn suspect_names() -> Vec<&'static str> {
    SUSPECTS.iter().map(|s| s.0).collect()
}

/// What the game offers the agent.
pub fn info() -> AppAgentInfo {
    let missions = mission_names();
    let suspects = suspect_names();
    let times: Vec<&str> = TIMES.iter().map(|t| t.0).collect();
    let weathers: Vec<&str> = WEATHERS.iter().map(|w| w.0).collect();
    let views: Vec<&str> = VIEWS.iter().map(|v| v.0).collect();
    agent::info(
        "Interceptor, a 3D police pursuit game in Veda Bay: it starts career missions, quick chases and patrols, \
         pauses, resumes and restarts them, and changes the settings, while the user drives the police car with \
         the keyboard.",
        vec![
            Action::new(
                "start_mission",
                "Starts one of the career's missions (one already opened: each opens when the one before is passed). \
                 Does not abandon a chase in progress (restart or quit_to_menu do)",
            )
            .choice("mission", "The mission", true, &missions)
            .build(),
            Action::new("quick_chase", "Starts a single pursuit with the choices given (the last ones otherwise)")
                .choice("suspect", "The suspect's car", false, &suspects)
                .choice("time_of_day", "When", false, &times)
                .choice("weather", "The weather", false, &weathers)
                .param("traffic", "boolean", "Traffic on the road", false)
                .param("backup", "integer", "Police cars joining the chase, 0 to 2", false)
                .build(),
            Action::new("start_patrol", "Drives about Veda Bay answering dispatch's calls, chase after chase")
                .choice("time_of_day", "When", false, &times)
                .choice("weather", "The weather", false, &weathers)
                .build(),
            Action::new("pause", "Pauses the chase or patrol in progress").build(),
            Action::new("resume", "Resumes the paused chase or patrol; Interceptor's window must be in front").build(),
            Action::new("restart", "Starts the chase or patrol in progress (or the last one played) again").build(),
            Action::new("quit_to_menu", "Goes back to the main menu, abandoning any chase or patrol in progress")
                .build(),
            Action::new("change_settings", "Changes the settings (kept for next time)")
                .param("sound_effects", "integer", "The sound effects' volume, 0 to 100", false)
                .param("music", "integer", "The music's volume, 0 to 100", false)
                .choice("speed_units", "How the speedometer counts", false, &["km/h", "mph"])
                .choice("camera", "The camera in play", false, &views)
                .choice(
                    "statistics",
                    "What is shown of how the game runs: off, fps (the frame rate and the GPU rendering) or \
                     detailed (the renderer's statistics as well)",
                    false,
                    &["off", "fps", "detailed"],
                )
                .param("full_screen", "boolean", "Fill the screen", false)
                .build(),
        ],
    )
}

/// `secs` as "1:05".
fn clock(secs: f32) -> String {
    let t = secs.max(0.0) as u32;
    format!("{}:{:02}", t / 60, t % 60)
}

fn time_name(t: TimeOfDay) -> &'static str {
    TIMES.iter().find(|x| x.1 == t).map_or("night", |x| x.0)
}

fn weather_name(w: Weather) -> &'static str {
    WEATHERS.iter().find(|x| x.1 == w).map_or("clear", |x| x.0)
}

/// What the game shows.
pub fn state(app: &App, in_front: bool) -> Value {
    let screen = match app.screen {
        Screen::Title => "title",
        Screen::Main(_) => "main_menu",
        Screen::Career(_) => "career",
        Screen::Quick(_) => "quick_chase_choices",
        Screen::Patrol(_) => "patrol_choices",
        Screen::Options(_, Back::Main) => "options",
        Screen::Options(_, Back::Pause) => "options_while_paused",
        Screen::Playing => "playing",
        Screen::Paused(_) => "paused",
        Screen::Results(_) => "results",
    };
    let mut v = object! { "screen" => screen };
    if app.in_play() {
        v.set("play", play(app));
    }
    if let (Screen::Results(_), Some(r)) = (app.screen, app.result) {
        let o = r.outcome;
        let mut res = object! {
            "ending" => o.ending.title().to_lowercase(),
            "caught" => o.ending.caught(),
            "time" => career::clock(o.time),
            "hits_landed" => o.hits,
            "damage_taken_percent" => (o.damage * 100.0).round() as u32,
            "score" => o.score,
            "stars" => o.stars,
            "new_best" => r.best,
        };
        if let Some(m) = r.mission {
            res.set("mission", MISSIONS[m].name);
        }
        v.set("results", res);
    }
    if matches!(app.screen, Screen::Career(_) | Screen::Main(_) | Screen::Results(_)) {
        let missions: Vec<Value> = MISSIONS
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let mut e = object! { "mission" => m.name, "open" => app.save.unlocked(i) };
                if let Some(b) = app.save.best[i] {
                    e.set("stars", b.stars);
                    e.set("best_time", career::clock(b.time));
                }
                e
            })
            .collect();
        v.set("career", object! { "stars" => app.save.stars(), "of" => MISSIONS.len() * 3, "missions" => missions });
    }
    let s = &app.save.settings;
    v.set(
        "settings",
        object! {
            "sound_effects" => (s.effects * 100.0).round() as u32,
            "music" => (s.music * 100.0).round() as u32,
            "speed_units" => s.units.name(),
            "camera" => VIEWS.iter().find(|x| x.1 == s.view).map_or("chase", |x| x.0),
            "statistics" => s.stats.key(),
            "full_screen" => s.fullscreen,
        },
    );
    v.set("window_in_front", in_front);
    v.set("controls", CONTROLS);
    v
}

/// The chase or patrol being played, as the screen shows it.
fn play(app: &App) -> Value {
    let s = &app.session;
    let mode = match s.mode {
        Mode::Career(m) => format!("mission {}: {}", m + 1, MISSIONS[m].name),
        Mode::Quick(_) => String::from("quick chase"),
        Mode::Patrol { .. } => String::from("patrol"),
        Mode::Showcase => String::from("showcase"),
    };
    let units = app.save.settings.units;
    let mut v = object! {
        "mode" => mode,
        "time_of_day" => time_name(s.env().time),
        "weather" => weather_name(s.env().weather),
        "speed" => format!("{} {}", units.of(s.game.player().vehicle.speed().abs()).round() as u32, units.name()),
        "siren" => s.game.player().siren,
        "nitrous_percent" => (s.game.boost.level * 100.0) as u32,
    };
    match s.phase {
        Phase::Countdown(t) => {
            v.set("starts_in_seconds", t.ceil() as u32);
        }
        Phase::Driving => {}
        Phase::Over(_) => {
            v.set("chase_over", true);
        }
    }
    if let Some(c) = s.chase {
        let mut chase = object! {
            "time" => clock(c.time),
            "suspect_distance_m" => c.gap.max(0.0).round() as i32,
            "suspect_car_left_percent" => (c.integrity(&s.game) * 100.0).max(0.0).round() as u32,
            "hits_landed" => c.hits,
        };
        if let Some(left) = s.time_left() {
            chase.set("time_left", clock(left));
        }
        match c.state {
            State::Chasing { escaping, busting } => {
                if escaping > 0.0 {
                    chase.set("suspect_getting_away", true);
                }
                if busting > 0.0 {
                    chase.set("busting_percent", (busting / interceptor_game::pursuit::BUST_TIME * 100.0) as u32);
                }
            }
            State::TakenDown => {
                chase.set("ended", "taken down");
            }
            State::Busted => {
                chase.set("ended", "busted");
            }
            State::Escaped => {
                chase.set("ended", "escaped");
            }
        }
        v.set("chase", chase);
    }
    if let Mode::Patrol { .. } = s.mode {
        v.set("caught", s.arrests);
        v.set("lost", s.escapes);
    }
    if let Some((words, age)) = &s.radio
        && *age < session::RADIO_SECS
    {
        v.set("radio", words.as_str());
    }
    v
}

/// The choice named `name` (case aside, or a part of it) among `options`.
fn pick<T: Copy>(what: &str, name: &str, options: &[(&str, T)]) -> Result<T, String> {
    let want = name.trim().to_lowercase();
    options
        .iter()
        .find(|o| o.0.to_lowercase() == want)
        .or_else(|| options.iter().find(|o| !want.is_empty() && o.0.to_lowercase().contains(&want)))
        .map(|o| o.1)
        .ok_or_else(|| {
            let names: Vec<&str> = options.iter().map(|o| o.0).collect();
            format!("Interceptor has no {what} called {name}; it has {}", names.join(", "))
        })
}

impl App {
    /// Where the game is, for actions that do not apply there.
    fn where_it_is(&self) -> &'static str {
        match self.screen {
            Screen::Title => "on the title screen",
            Screen::Main(_) => "on the main menu",
            Screen::Career(_) => "on the career's list of missions",
            Screen::Quick(_) | Screen::Patrol(_) | Screen::Options(..) => "on a menu",
            Screen::Playing | Screen::Paused(_) => "playing",
            Screen::Results(_) => "showing the results",
        }
    }

    /// A chase or patrol is in progress (not over).
    fn busy(&self) -> bool {
        self.in_play() && !matches!(self.session.phase, Phase::Over(_))
    }

    /// Starts `mode` as the menus do; starts it paused if the window is not
    /// in front (play pauses whenever it is not).
    fn agent_play(&mut self, mode: Mode, in_front: bool) -> Value {
        self.play(mode);
        let mut v = object! { "started" => true };
        if !in_front {
            self.pause();
            v.set("note", "Interceptor's window is not in front, so play waits, paused, until it is");
        }
        v
    }

    fn agent_invoke(&mut self, action: &str, args: &Value, in_front: bool) -> Result<Value, String> {
        let busy = self.busy();
        let in_progress = || "A chase or patrol is in progress: restart starts it again, quit_to_menu abandons it, resume continues it";
        match action {
            "start_mission" => {
                let name = arg_opt_str(args, "mission").ok_or("Say which mission")?;
                let names: Vec<(&str, usize)> = MISSIONS.iter().enumerate().map(|(i, m)| (m.name, i)).collect();
                let m = match name.trim().parse::<usize>() {
                    Ok(n) if (1..=MISSIONS.len()).contains(&n) => n - 1,
                    _ => pick("mission", name, &names)?,
                };
                if busy {
                    return Err(in_progress().into());
                }
                if !self.save.unlocked(m) {
                    return Err(format!(
                        "{} is not open yet: catch the suspect of {} first",
                        MISSIONS[m].name,
                        MISSIONS[m - 1].name
                    ));
                }
                let mut v = self.agent_play(Mode::Career(m), in_front);
                v.set("mission", MISSIONS[m].name);
                v.set("brief", MISSIONS[m].brief);
                Ok(v)
            }
            "quick_chase" => {
                if busy {
                    return Err(in_progress().into());
                }
                let mut q: Quick = self.quick;
                if let Some(s) = arg_opt_str(args, "suspect") {
                    let names: Vec<(&str, usize)> = SUSPECTS.iter().enumerate().map(|(i, s)| (s.0, i)).collect();
                    q.suspect = pick("suspect", s, &names)?;
                }
                if let Some(t) = arg_opt_str(args, "time_of_day") {
                    q.time = pick("time of day", t, &TIMES)?;
                }
                if let Some(w) = arg_opt_str(args, "weather") {
                    q.weather = pick("weather", w, &WEATHERS)?;
                }
                if let Some(t) = arg_bool(args, "traffic") {
                    q.traffic = t;
                }
                if let Some(b) = arg_opt_int(args, "backup")? {
                    if !(0..=2).contains(&b) {
                        return Err("Backup is 0 to 2 police cars".into());
                    }
                    q.backup = b as usize;
                }
                self.quick = q;
                Ok(self.agent_play(Mode::Quick(q), in_front))
            }
            "start_patrol" => {
                if busy {
                    return Err(in_progress().into());
                }
                if let Some(t) = arg_opt_str(args, "time_of_day") {
                    self.patrol.0 = pick("time of day", t, &TIMES)?;
                }
                if let Some(w) = arg_opt_str(args, "weather") {
                    self.patrol.1 = pick("weather", w, &WEATHERS)?;
                }
                let (time, weather) = self.patrol;
                Ok(self.agent_play(Mode::Patrol { time, weather }, in_front))
            }
            "pause" => {
                if !self.in_play() {
                    return Err(format!("There is nothing to pause: Interceptor is {}", self.where_it_is()));
                }
                if self.screen == Screen::Playing {
                    self.pause();
                    Ok(object! { "paused" => true })
                } else {
                    Ok(object! { "paused" => true, "note" => "it was already paused" })
                }
            }
            "resume" => {
                if !self.in_play() {
                    return Err(format!("There is nothing to resume: Interceptor is {}", self.where_it_is()));
                }
                if self.screen == Screen::Playing {
                    return Ok(object! { "paused" => false, "note" => "it was not paused" });
                }
                if !in_front {
                    return Err("Interceptor's window is not in front, and play pauses whenever it is not: bring \
                                the window to the front first"
                        .into());
                }
                self.resume();
                Ok(object! { "paused" => false })
            }
            "restart" => {
                let mode = match self.session.mode {
                    Mode::Showcase => {
                        return Err("Nothing has been played yet: start a mission, a quick chase or a patrol".into());
                    }
                    m => m,
                };
                Ok(self.agent_play(mode, in_front))
            }
            "quit_to_menu" => {
                let abandoned = busy;
                self.leave_play(Screen::Main(0));
                Ok(object! { "screen" => "main_menu", "abandoned" => abandoned })
            }
            "change_settings" => {
                let s = &mut self.save.settings;
                let volume = |n: Option<i64>| -> Result<Option<f32>, String> {
                    match n {
                        Some(n) if !(0..=100).contains(&n) => Err("A volume is 0 to 100".into()),
                        n => Ok(n.map(|n| n as f32 / 100.0)),
                    }
                };
                let mut changed = false;
                if let Some(v) = volume(arg_opt_int(args, "sound_effects")?)? {
                    s.effects = v;
                    changed = true;
                }
                if let Some(v) = volume(arg_opt_int(args, "music")?)? {
                    s.music = v;
                    changed = true;
                }
                if let Some(u) = arg_opt_str(args, "speed_units") {
                    s.units = pick("speed unit", u, &[("km/h", Units::Kmh), ("kmh", Units::Kmh), ("mph", Units::Mph)])?;
                    changed = true;
                }
                if let Some(c) = arg_opt_str(args, "camera") {
                    s.view = pick("camera", c, &VIEWS)?;
                    self.session.game.follow.view = s.view;
                    changed = true;
                }
                if let Some(v) = arg_opt_str(args, "statistics") {
                    let options: Vec<(&str, Stats)> = Stats::ALL.iter().map(|&x| (x.key(), x)).collect();
                    s.stats = pick("statistics setting", v, &options)?;
                    changed = true;
                }
                if let Some(b) = arg_bool(args, "full_screen") {
                    s.fullscreen = b;
                    self.fullscreen = Some(b);
                    changed = true;
                }
                if !changed {
                    return Err("Say what to change: sound_effects, music, speed_units, camera, statistics or \
                                full_screen"
                        .into());
                }
                self.store_save();
                Ok(object! { "changed" => true })
            }
            other => Err(format!("Interceptor has no action called {other}")),
        }
    }
}

/// The game and its window during one frame, as the agent sees them.
struct View<'a> {
    app: &'a mut App,
    /// The window has the keyboard (play pauses when it loses it).
    in_front: bool,
}

impl AgentServer for View<'_> {
    fn agent_info(&self) -> AppAgentInfo {
        info()
    }

    fn agent_state(&self) -> Value {
        state(self.app, self.in_front)
    }

    fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        self.app.agent_invoke(action, args, self.in_front)
    }
}

/// Answers the agent's requests, between frames (never waits).
pub fn serve(link: &mut Link, app: &mut App, in_front: bool) {
    link.serve(&mut View { app, in_front });
}
