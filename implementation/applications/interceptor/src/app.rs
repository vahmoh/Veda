// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The game's screens and the moves between them: the title, the main
//! menu, the career's missions, a quick chase's and a patrol's choices,
//! the options, play itself, the pause menu and the results.
//!
//! Behind every menu runs the showcase (a chase the computer drives,
//! filmed as television would); choosing something to play replaces it
//! with that session, and leaving play brings it back. The keyboard and
//! the mouse both work the menus: arrows (or WASD) move and change
//! values, Enter chooses, Esc goes back; pointing selects, clicking
//! chooses.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use glam::Vec3;
use interceptor_game::audio::{Cue, Emitter};
use interceptor_game::camera::View;
use interceptor_game::career::{MISSIONS, Outcome, SUSPECTS, Save, Units};
use interceptor_game::env::{TimeOfDay, Weather};
use interceptor_game::session::{Mode, Phase, Quick, Session};
use interceptor_game::sound::Sound;
use interceptor_game::world::World;
use num_traits::Float;
use vgfx::Rect;
use vproto::input::keys;

use crate::speakers::{Speakers, Tune};
use crate::store::Store;

/// Where Esc leaves the options for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    Main,
    Pause,
}

/// A screen, and the item selected on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Title,
    Main(usize),
    /// The missions; the one selected.
    Career(usize),
    Quick(usize),
    Patrol(usize),
    Options(usize, Back),
    Playing,
    Paused(usize),
    Results(usize),
}

/// A menu item: its label, its value (for settings) and whether it can
/// be chosen.
pub struct Item {
    pub label: String,
    pub value: Option<String>,
    pub enabled: bool,
}

impl Item {
    fn button(label: &str) -> Item {
        Item { label: label.into(), value: None, enabled: true }
    }

    fn setting(label: &str, value: String) -> Item {
        Item { label: label.into(), value: Some(value), enabled: true }
    }
}

/// The last result, for the results screen.
#[derive(Clone, Copy, Debug)]
pub struct Result {
    pub outcome: Outcome,
    /// The mission, if a career mission.
    pub mission: Option<usize>,
    /// Better than the best before.
    pub best: bool,
}

/// What the main menu offers, and what each is.
pub const MAIN: [(&str, &str); 5] = [
    ("CAREER", "Eight pursuits across Veda Bay, from a stolen saloon to the bay's most wanted driver."),
    ("QUICK CHASE", "One pursuit: choose the suspect, the time of day and the weather."),
    ("PATROL", "Drive the bay and answer dispatch's calls, chase after chase."),
    ("OPTIONS", "Sound, music, speed units, the camera and the display."),
    ("QUIT", "Leave Interceptor."),
];

const TIMES: [(TimeOfDay, &str); 3] =
    [(TimeOfDay::Day, "Day"), (TimeOfDay::Sunset, "Sunset"), (TimeOfDay::Night, "Night")];
const WEATHERS: [(Weather, &str); 2] = [(Weather::Clear, "Clear"), (Weather::Rain, "Rain")];
const VIEWS: [(View, &str); 4] =
    [(View::Chase, "Chase"), (View::Near, "Near"), (View::Bonnet, "Bonnet"), (View::Bumper, "Bumper")];

fn time_name(t: TimeOfDay) -> &'static str {
    TIMES.iter().find(|x| x.0 == t).map_or("Night", |x| x.1)
}

fn weather_name(w: Weather) -> &'static str {
    WEATHERS.iter().find(|x| x.0 == w).map_or("Clear", |x| x.1)
}

fn on_off(b: bool) -> String {
    String::from(if b { "On" } else { "Off" })
}

/// The next of `list` after (or before, `dir` -1) the one `is`.
fn cycle<T: Copy>(list: &[(T, &str)], is: impl Fn(T) -> bool, dir: i32) -> T {
    let i = list.iter().position(|x| is(x.0)).unwrap_or(0) as i32;
    list[(i + dir).rem_euclid(list.len() as i32) as usize].0
}

/// The game.
pub struct App {
    pub world: Arc<World>,
    pub session: Session,
    pub screen: Screen,
    pub save: Save,
    store: Store,
    pub quick: Quick,
    pub patrol: (TimeOfDay, Weather),
    pub result: Option<Result>,
    /// Seconds on this screen (its animations), and since the start.
    pub screen_time: f32,
    pub clock: f32,
    /// Asks of the window and the renderer, for the main loop.
    pub quit: bool,
    pub relight: bool,
    pub fullscreen: Option<bool>,
    /// The menu items drawn last, where they are (for the mouse).
    pub targets: Vec<(Rect, usize)>,
    sound: Sound,
    speakers: Option<Speakers>,
    cues: Vec<Cue>,
}

impl App {
    /// The game in `world`, with what `store` kept (`save`); the window is
    /// already full screen if the save says so.
    pub fn new(world: Arc<World>, speakers: Option<Speakers>, store: Store, save: Save) -> App {
        let session = Session::new(world.clone(), Mode::Showcase);
        App {
            world,
            session,
            screen: Screen::Title,
            fullscreen: None,
            save,
            store,
            quick: Quick::default(),
            patrol: (TimeOfDay::Night, Weather::Clear),
            result: None,
            screen_time: 0.0,
            clock: 0.0,
            quit: false,
            relight: true,
            targets: Vec::new(),
            sound: Sound::new(),
            speakers,
            cues: Vec::new(),
        }
    }

    fn go(&mut self, screen: Screen) {
        if core::mem::discriminant(&screen) != core::mem::discriminant(&self.screen) {
            self.screen_time = 0.0;
        }
        self.screen = screen;
    }

    /// Plays `mode`.
    pub fn play(&mut self, mode: Mode) {
        self.session = Session::new(self.world.clone(), mode);
        self.session.game.follow.view = self.save.settings.view;
        self.relight = true;
        self.go(Screen::Playing);
    }

    /// Back to the menus, the showcase behind them.
    pub(crate) fn leave_play(&mut self, screen: Screen) {
        if !matches!(self.session.mode, Mode::Showcase) {
            self.session = Session::new(self.world.clone(), Mode::Showcase);
            self.relight = true;
        }
        self.go(screen);
    }

    /// Writes the save file (the best results and the settings).
    pub(crate) fn store_save(&mut self) {
        if !self.store.store(&self.save) {
            vrt::println!("cannot save the settings and results");
        }
    }

    /// Whether the session is a chase or patrol being played (or paused).
    pub fn in_play(&self) -> bool {
        matches!(self.screen, Screen::Playing | Screen::Paused(_) | Screen::Options(_, Back::Pause))
    }

    /// The items of the current screen.
    pub fn items(&self) -> Vec<Item> {
        let s = &self.save.settings;
        match self.screen {
            Screen::Title | Screen::Playing => Vec::new(),
            Screen::Main(_) => MAIN.iter().map(|m| Item::button(m.0)).collect(),
            Screen::Career(_) => MISSIONS
                .iter()
                .enumerate()
                .map(|(i, m)| Item { label: m.name.into(), value: None, enabled: self.save.unlocked(i) })
                .collect(),
            Screen::Quick(_) => {
                let q = &self.quick;
                let backup = match q.backup {
                    0 => String::from("None"),
                    1 => String::from("One unit"),
                    n => format!("{n} units"),
                };
                alloc::vec![
                    Item::setting("SUSPECT", SUSPECTS[q.suspect].0.into()),
                    Item::setting("TIME OF DAY", time_name(q.time).into()),
                    Item::setting("WEATHER", weather_name(q.weather).into()),
                    Item::setting("TRAFFIC", on_off(q.traffic)),
                    Item::setting("BACKUP", backup),
                    Item::button("START CHASE"),
                    Item::button("BACK"),
                ]
            }
            Screen::Patrol(_) => alloc::vec![
                Item::setting("TIME OF DAY", time_name(self.patrol.0).into()),
                Item::setting("WEATHER", weather_name(self.patrol.1).into()),
                Item::button("START PATROL"),
                Item::button("BACK"),
            ],
            Screen::Options(..) => alloc::vec![
                Item::setting("SOUND EFFECTS", format!("{}%", (s.effects * 100.0).round() as u32)),
                Item::setting("MUSIC", format!("{}%", (s.music * 100.0).round() as u32)),
                Item::setting("SPEED IN", s.units.name().into()),
                Item::setting("CAMERA", VIEWS.iter().find(|v| v.0 == s.view).map_or("Chase", |v| v.1).into()),
                Item::setting("STATISTICS", s.stats.name().into()),
                Item::setting("FULL SCREEN", on_off(s.fullscreen)),
                Item::button("BACK"),
            ],
            Screen::Paused(_) => {
                ["RESUME", "RESTART", "OPTIONS", "QUIT TO MENU"].iter().map(|l| Item::button(l)).collect()
            }
            Screen::Results(_) => self.result_buttons().iter().map(|l| Item::button(l)).collect(),
        }
    }

    /// The results screen's buttons.
    fn result_buttons(&self) -> Vec<&'static str> {
        let next =
            self.result.and_then(|r| r.mission).is_some_and(|m| m + 1 < MISSIONS.len() && self.save.unlocked(m + 1));
        if next { alloc::vec!["NEXT MISSION", "RETRY", "MENU"] } else { alloc::vec!["RETRY", "MENU"] }
    }

    /// The item selected.
    pub fn selected(&self) -> usize {
        match self.screen {
            Screen::Main(i)
            | Screen::Career(i)
            | Screen::Quick(i)
            | Screen::Patrol(i)
            | Screen::Options(i, _)
            | Screen::Paused(i)
            | Screen::Results(i) => i,
            Screen::Title | Screen::Playing => 0,
        }
    }

    fn select(&mut self, i: usize) {
        let n = self.items().len();
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        if i != self.selected() {
            self.cues.push(Cue::Move);
        }
        self.screen = match self.screen {
            Screen::Main(_) => Screen::Main(i),
            Screen::Career(_) => Screen::Career(i),
            Screen::Quick(_) => Screen::Quick(i),
            Screen::Patrol(_) => Screen::Patrol(i),
            Screen::Options(_, b) => Screen::Options(i, b),
            Screen::Paused(_) => Screen::Paused(i),
            Screen::Results(_) => Screen::Results(i),
            s => s,
        };
    }

    /// A key pressed (or released) on the menus or in play.
    pub fn key(&mut self, code: u16, pressed: bool, repeat: bool) {
        if self.screen == Screen::Playing {
            self.drive(code, pressed, repeat);
            return;
        }
        if !pressed {
            return;
        }
        let n = self.items().len().max(1);
        let sel = self.selected();
        match code {
            keys::UP | keys::W => self.select((sel + n - 1) % n),
            keys::DOWN | keys::S => self.select((sel + 1) % n),
            keys::LEFT | keys::A => self.adjust(sel, -1),
            keys::RIGHT | keys::D => self.adjust(sel, 1),
            keys::ENTER | keys::KPENTER | keys::SPACE if !repeat => self.choose(sel),
            keys::ESC | keys::BACKSPACE if !repeat => self.back(),
            keys::F3 if !repeat => self.cycle_stats(),
            keys::F11 if !repeat => self.toggle_fullscreen(),
            _ => {}
        }
    }

    /// The keys in play.
    fn drive(&mut self, code: u16, pressed: bool, repeat: bool) {
        let game = &mut self.session.game;
        let k = &mut game.keys;
        match code {
            keys::UP | keys::W => k.up = pressed,
            keys::DOWN | keys::S => k.down = pressed,
            keys::LEFT | keys::A => k.left = pressed,
            keys::RIGHT | keys::D => k.right = pressed,
            keys::SPACE => k.handbrake = pressed,
            keys::LEFTSHIFT | keys::RIGHTSHIFT => k.boost = pressed,
            _ => {}
        }
        if !pressed || repeat {
            return;
        }
        match code {
            keys::ESC | keys::P => self.pause(),
            keys::Q => game.toggle_siren(),
            keys::H => {
                let v = &game.player().vehicle;
                let emitter = Emitter { pos: v.pos + Vec3::Y, vel: Vec3::new(v.vel.x, v.vy, v.vel.y) };
                self.cues.push(Cue::Horn { emitter, secs: 0.45 });
            }
            keys::C => {
                game.follow.view = game.follow.view.next();
                self.save.settings.view = game.follow.view;
                self.store_save();
            }
            keys::R => game.reset_player(),
            keys::F3 => self.cycle_stats(),
            keys::F8 => {
                let on = !game.autopilot;
                let target = self.session.chase.filter(|c| c.on()).map(|c| c.suspect);
                self.session.game.set_autopilot(on, target);
                vrt::println!("autopilot {}", if on { "on" } else { "off" });
            }
            keys::F11 => self.toggle_fullscreen(),
            _ => {}
        }
    }

    /// F3: the frame rate and the GPU, the statistics as well, nothing.
    fn cycle_stats(&mut self) {
        self.save.settings.stats = self.save.settings.stats.step(1);
        self.store_save();
    }

    fn toggle_fullscreen(&mut self) {
        self.save.settings.fullscreen = !self.save.settings.fullscreen;
        self.fullscreen = Some(self.save.settings.fullscreen);
        self.store_save();
    }

    /// The window lost the keyboard: let go, and pause play.
    pub fn unfocused(&mut self) {
        self.session.game.release();
        if self.screen == Screen::Playing {
            self.pause();
        }
    }

    pub(crate) fn pause(&mut self) {
        self.session.game.release();
        self.session.game.paused = true;
        self.cues.push(Cue::Back);
        self.go(Screen::Paused(0));
    }

    pub(crate) fn resume(&mut self) {
        self.session.game.paused = matches!(self.session.phase, Phase::Countdown(_));
        self.go(Screen::Playing);
    }

    /// The pointer at (`x`, `y`): it selects the item under it.
    pub fn point(&mut self, x: i32, y: i32) {
        if let Some(&(_, i)) = self.targets.iter().find(|(r, _)| r.contains(x, y)) {
            self.select(i);
        }
    }

    /// A click at (`x`, `y`).
    pub fn click(&mut self, x: i32, y: i32) {
        match self.screen {
            Screen::Title => self.choose(0),
            Screen::Playing => {}
            _ => {
                if let Some(&(_, i)) = self.targets.iter().find(|(r, _)| r.contains(x, y)) {
                    self.select(i);
                    if self.items().get(i).is_some_and(|it| it.value.is_some()) {
                        self.adjust(i, 1);
                    } else {
                        self.choose(i);
                    }
                }
            }
        }
    }

    /// Changes setting `i` by `dir` (left, right); on the results, moves
    /// between the buttons.
    fn adjust(&mut self, i: usize, dir: i32) {
        if let Screen::Results(_) = self.screen {
            let n = self.items().len() as i32;
            self.select((i as i32 + dir).rem_euclid(n.max(1)) as usize);
            return;
        }
        let changed = match self.screen {
            Screen::Quick(_) => {
                let q = &mut self.quick;
                match i {
                    0 => q.suspect = (q.suspect as i32 + dir).rem_euclid(SUSPECTS.len() as i32) as usize,
                    1 => q.time = cycle(&TIMES, |t| t == q.time, dir),
                    2 => q.weather = cycle(&WEATHERS, |w| w == q.weather, dir),
                    3 => q.traffic = !q.traffic,
                    4 => q.backup = (q.backup as i32 + dir).rem_euclid(3) as usize,
                    _ => return,
                }
                true
            }
            Screen::Patrol(_) => {
                let p = &mut self.patrol;
                match i {
                    0 => p.0 = cycle(&TIMES, |t| t == p.0, dir),
                    1 => p.1 = cycle(&WEATHERS, |w| w == p.1, dir),
                    _ => return,
                }
                true
            }
            Screen::Options(..) => {
                let s = &mut self.save.settings;
                let step = |v: f32| ((v + dir as f32 * 0.1).clamp(0.0, 1.0) * 10.0).round() / 10.0;
                match i {
                    0 => s.effects = step(s.effects),
                    1 => s.music = step(s.music),
                    2 => s.units = if s.units == Units::Kmh { Units::Mph } else { Units::Kmh },
                    3 => {
                        s.view = cycle(&VIEWS, |v| v == s.view, dir);
                        self.session.game.follow.view = s.view;
                    }
                    4 => s.stats = s.stats.step(dir),
                    5 => {
                        s.fullscreen = !s.fullscreen;
                        self.fullscreen = Some(s.fullscreen);
                    }
                    _ => return,
                }
                self.store_save();
                true
            }
            _ => false,
        };
        if changed {
            self.cues.push(Cue::Move);
        }
    }

    /// Chooses item `i`.
    fn choose(&mut self, i: usize) {
        let Some(item) =
            self.items().into_iter().nth(i).or_else(|| (self.screen == Screen::Title).then(|| Item::button("")))
        else {
            return;
        };
        if !item.enabled {
            self.cues.push(Cue::Back);
            return;
        }
        if item.value.is_some() {
            self.adjust(i, 1);
            return;
        }
        self.cues.push(Cue::Select);
        match self.screen {
            Screen::Title => self.go(Screen::Main(0)),
            Screen::Main(_) => match i {
                0 => {
                    // The first mission not yet passed.
                    let next = (0..MISSIONS.len()).find(|&m| self.save.best[m].is_none()).unwrap_or(0);
                    self.go(Screen::Career(next));
                }
                1 => self.go(Screen::Quick(5)),
                2 => self.go(Screen::Patrol(2)),
                3 => self.go(Screen::Options(0, Back::Main)),
                _ => self.quit = true,
            },
            Screen::Career(m) => self.play(Mode::Career(m)),
            Screen::Quick(_) => match item.label.as_str() {
                "START CHASE" => self.play(Mode::Quick(self.quick)),
                _ => self.back(),
            },
            Screen::Patrol(_) => match item.label.as_str() {
                "START PATROL" => self.play(Mode::Patrol { time: self.patrol.0, weather: self.patrol.1 }),
                _ => self.back(),
            },
            Screen::Options(..) => self.back(),
            Screen::Paused(_) => match i {
                0 => self.resume(),
                1 => self.play(self.session.mode),
                2 => self.go(Screen::Options(0, Back::Pause)),
                _ => self.leave_play(Screen::Main(0)),
            },
            Screen::Results(_) => match item.label.as_str() {
                "NEXT MISSION" => {
                    if let Some(m) = self.result.and_then(|r| r.mission) {
                        self.play(Mode::Career(m + 1));
                    }
                }
                "RETRY" => self.play(self.session.mode),
                _ => {
                    let back = match self.session.mode {
                        Mode::Career(m) => Screen::Career(m),
                        Mode::Quick(_) => Screen::Quick(5),
                        _ => Screen::Main(0),
                    };
                    self.leave_play(back);
                }
            },
            Screen::Playing => {}
        }
    }

    /// Esc: back a screen.
    fn back(&mut self) {
        let to = match self.screen {
            Screen::Title => return,
            Screen::Main(_) => Screen::Title,
            Screen::Career(_) => Screen::Main(0),
            Screen::Quick(_) => Screen::Main(1),
            Screen::Patrol(_) => Screen::Main(2),
            Screen::Options(_, Back::Main) => Screen::Main(3),
            Screen::Options(_, Back::Pause) => Screen::Paused(2),
            Screen::Paused(_) => {
                self.cues.push(Cue::Back);
                self.resume();
                return;
            }
            Screen::Results(_) => {
                self.choose(self.items().len() - 1);
                return;
            }
            Screen::Playing => return,
        };
        self.cues.push(Cue::Back);
        self.go(to);
    }

    /// Advances `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        self.clock += dt;
        self.screen_time += dt;
        if matches!(self.screen, Screen::Paused(_) | Screen::Options(_, Back::Pause)) {
            return;
        }
        self.session.update(dt);
        // A chase played to its end: the results, after a moment.
        if self.screen == Screen::Playing
            && let (Phase::Over(t), Some(outcome)) = (self.session.phase, self.session.outcome)
            && t > 3.5
        {
            let mission = match self.session.mode {
                Mode::Career(m) => Some(m),
                _ => None,
            };
            let best = mission.is_some_and(|m| self.save.record(m, &outcome));
            if best {
                self.store_save();
            }
            vrt::println!(
                "result: {} in {}, {} hits, {} stars",
                outcome.ending.title().to_lowercase(),
                interceptor_game::career::clock(outcome.time),
                outcome.hits,
                outcome.stars
            );
            self.result = Some(Result { outcome, mission, best });
            self.session.game.release();
            self.go(Screen::Results(0));
        }
    }

    /// Hands the speakers what is heard now (`dt` since the last frame).
    pub fn sound(&mut self, dt: f32) {
        let Some(speakers) = &self.speakers else {
            self.cues.clear();
            let _ = self.session.take_cues();
            return;
        };
        let settings = &self.save.settings;
        // Behind the menus the showcase is quieter.
        let near = self.in_play() || matches!(self.screen, Screen::Results(_));
        let volume = settings.effects * if near { 1.0 } else { 0.35 };
        let (mut scene, mut cues) = self.sound.update(&self.session.game, volume, dt);
        scene.paused |= matches!(self.screen, Screen::Paused(_) | Screen::Options(_, Back::Pause) | Screen::Results(_));
        cues.extend(self.session.take_cues());
        cues.append(&mut self.cues);
        speakers.play(scene, cues);
        // The music: the chase's, hotter as the suspect is caught up with;
        // a patrol's while there is no chase; the theme in the menus.
        let chasing = self.session.chase.is_some_and(|c| c.on());
        let (tune, heat) = if !self.in_play() {
            (Tune::Theme, 0.0)
        } else if chasing && !matches!(self.session.phase, Phase::Countdown(_)) {
            let gap = self.session.gap().unwrap_or(200.0);
            (Tune::Pursuit, 1.0 - ((gap - 40.0) / 160.0).clamp(0.0, 1.0))
        } else if matches!(self.session.mode, Mode::Patrol { .. }) {
            (Tune::Patrol, 0.0)
        } else {
            (Tune::Pursuit, 0.0)
        };
        speakers.music(Some(tune), heat);
        let quieter = matches!(self.screen, Screen::Paused(_) | Screen::Options(_, Back::Pause));
        speakers.set_music_volume(settings.music * if quieter { 0.5 } else { 1.0 });
    }
}
