// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Videos for the voice agent: what it shows (the library, the video open
//! and where it is) and what it does (playing a video by name, pausing,
//! seeking, the player's volume, full screen, back to the library), through
//! the same operations as the buttons and keys.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use vfiles::format::duration as time;
use videos_core::control::{self, SeekError};
use videos_core::playback::Phase;
use videos_core::text;
use vui::agent::names::{self, MIN_SCORE, query_words, score};
use vui::agent::{self, Action, AppAgentInfo, Value, arg_bool, arg_f64, arg_opt_int, arg_opt_str, json, show_path};

use crate::player::Player;
use crate::{OpenAt, Videos};

/// The most videos the agent gets in one list.
const MAX_VIDEOS: usize = 60;
/// Words of a request that name no video ("the", "video", "play").
const FILLER: &[&str] =
    &["the", "a", "an", "my", "video", "videos", "clip", "movie", "film", "play", "watch", "please", "one", "of"];

pub fn info() -> AppAgentInfo {
    agent::info(
        "A video player for the videos in ~/Videos: it plays a video by name (from where it was left or from the \
         start), pauses, seeks, has its own volume and shows videos full screen.",
        vec![
            Action::new(
                "play",
                "Plays a video: the best match for a title when one is given, otherwise resumes the video open (or \
                 the one watched last)",
            )
            .param("video", "string", "The video's title as the user said it (leave out to resume)", false)
            .choice(
                "from",
                "Where to start: where it was left (resume, the default) or the beginning (start)",
                false,
                &["resume", "start"],
            )
            .build(),
            Action::new("pause", "Pauses the video (play resumes it at the same place)").build(),
            Action::new("seek", "Jumps to a position in the video open")
                .param(
                    "seconds",
                    "number",
                    "Seconds from the start of the video; with relative, how far to move (negative goes back)",
                    true,
                )
                .param("relative", "boolean", "Move from the current position (false by default)", false)
                .build(),
            Action::new("set_volume", "Changes the player's own volume (the system volume is separate)")
                .param("level", "integer", "Volume in percent, 0 to 100", false)
                .param("change", "integer", "Change by this many percentage points, such as 10 or -10", false)
                .param("mute", "boolean", "Mute (true) or unmute (false) the player", false)
                .build(),
            Action::new("full_screen", "Shows the video open on the whole screen, or in its window again")
                .param("on", "boolean", "true for full screen, false for the window", true)
                .build(),
            Action::new("list_videos", "Lists the videos in the library, or those whose title matches")
                .param("query", "string", "What to look for (every video if not given)", false)
                .build(),
            Action::new("show_library", "Closes the video open and shows the library").build(),
        ],
    )
}

/// "playing", "paused", ... as the agent says it.
fn playback(p: &Player) -> String {
    match p.phase() {
        Phase::Opening => "opening".into(),
        Phase::Seeking if p.playing() => "playing (seeking)".into(),
        Phase::Seeking => "paused (seeking)".into(),
        Phase::Playing => "playing".into(),
        Phase::Paused => "paused".into(),
        Phase::Ended => "finished".into(),
        Phase::Failed(why) => format!("cannot play: {why}"),
        Phase::Restarting => "restarting the decoder".into(),
    }
}

/// The video open as the agent sees it.
fn now_playing(p: &Player) -> Value {
    let now = vrt::time::now_ns();
    let at = p.position_us(now).max(0) as u64 / 1000;
    let mut v = json!({
        "title": p.title(),
        "playback": playback(p),
        "position": time(at),
        "position_seconds": at / 1000,
        "file": show_path(&p.path),
    });
    if let Some(info) = p.info() {
        if let Some(d) = info.duration_us {
            let ms = d.max(0) as u64 / 1000;
            v["duration"] = json!(time(ms));
            v["duration_seconds"] = json!(ms / 1000);
        }
        if !info.seekable {
            v["can_seek"] = json!(false);
        }
        v["picture"] = json!(info.video_stream().map_or(String::from("none"), text::picture));
        v["sound"] = json!(info.audio_stream().map_or(String::from("none"), text::sound));
    }
    v
}

pub fn state(v: &Videos) -> Value {
    let mut s = json!({ "view": if v.player.is_some() { "player" } else { "library" } });
    s["now_playing"] = json!(v.player.as_ref().map_or(Value::Null, now_playing));
    s["full_screen"] = json!(v.full_screen);
    s["volume"] = json!((v.volume * 100.0 + 0.5) as u32);
    s["muted"] = json!(v.muted);
    if let Some(why) = &v.unavailable {
        s["playback_available"] = json!(false);
        s["why_unavailable"] = json!(why.as_str());
    }
    let total: u64 = v.entries.iter().filter_map(|e| e.duration_ms()).sum();
    s["library"] = json!({
        "folder": show_path(&v.dir),
        "videos": v.entries.len(),
        "total_time": time(total),
    });
    s["videos"] = json!(v.entries.iter().take(MAX_VIDEOS).map(|e| v.video(e)).collect::<Vec<_>>());
    if v.entries.len() > MAX_VIDEOS {
        s["videos_cut_short"] = json!(true);
    }
    let cont: Vec<Value> = v.continuing().iter().map(|&i| v.video(&v.entries[i])).collect();
    s["continue_watching"] = cont.into();
    s
}

impl Videos {
    /// One video of the library as the agent sees it.
    fn video(&self, e: &crate::library::Entry) -> Value {
        let mut v = json!({ "title": e.title.as_str() });
        if let Some(d) = e.duration_ms() {
            v["duration"] = json!(time(d));
        }
        let what = e.description();
        if !what.is_empty() {
            v[if matches!(e.info, Some(Err(_))) { "problem" } else { "picture" }] = what.into();
        }
        if let Some(r) = self.state.resume_of(&e.path).filter(|r| r.size == e.size && r.modified == e.modified) {
            v["left_at"] = json!(time(r.position_ms));
        } else if self.state.is_watched(&e.path) {
            v["watched"] = json!(true);
        }
        v
    }

    /// What the engine said of the video open since the last frame (it
    /// may have opened, or failed).
    fn agent_sync(&mut self) -> Result<(), String> {
        let now = vrt::time::now_ns();
        let Some(p) = &mut self.player else { return Ok(()) };
        p.sync(now);
        match p.phase() {
            Phase::Failed(why) => Err(format!("{} cannot be played: {why}", p.title())),
            _ => Ok(()),
        }
    }

    /// The video `query` names.
    fn find_video(&self, query: &str) -> Result<usize, String> {
        let q = query_words(query, FILLER);
        let titles: Vec<&str> = self.entries.iter().map(|e| e.title.as_str()).collect();
        if let Some(i) = names::best(&q, &titles) {
            return Ok(i);
        }
        let files: Vec<&str> = self.entries.iter().map(|e| vfiles::path::file_stem(&e.name)).collect();
        names::best(&q, &files).ok_or_else(|| {
            let some: Vec<&str> = titles.iter().take(6).copied().collect();
            format!(
                "nothing in the library matches \u{201c}{query}\u{201d}; it has {} videos, such as {}",
                titles.len(),
                some.join(", ")
            )
        })
    }

    fn empty_library(&self) -> String {
        match &self.list_error {
            Some(e) => e.clone(),
            None => format!("the library is empty: there are no videos in {}", show_path(&self.dir)),
        }
    }

    /// Runs one of the agent's actions. None waits: an action that opens
    /// a video answers at once that it is opening (the state says when it
    /// plays), as the window must go on drawing meanwhile.
    pub(crate) fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        if !self.listed {
            self.reload();
        }
        let now = vrt::time::now_ns();
        match action {
            "play" => {
                if let Some(why) = &self.unavailable {
                    return Err(format!("videos cannot be played here: {why}"));
                }
                let from = match arg_opt_str(args, "from") {
                    None | Some("resume") => OpenAt::Resume,
                    Some("start") => OpenAt::Start,
                    Some(other) => return Err(format!("'from' cannot be {other}: it is resume or start")),
                };
                match arg_opt_str(args, "video") {
                    Some(query) => {
                        if self.entries.is_empty() {
                            return Err(self.empty_library());
                        }
                        let i = self.find_video(query)?;
                        self.open_entry(i, from);
                    }
                    None => match &mut self.player {
                        Some(p) => {
                            if from == OpenAt::Start {
                                p.start_over(now);
                            } else {
                                p.play(now);
                            }
                        }
                        None => {
                            let Some(&i) = self.continuing().first() else {
                                return Err("no video is open: say which one to play".into());
                            };
                            self.open_entry(i, from);
                        }
                    },
                }
                self.agent_sync()?;
                Ok(self.player.as_ref().map_or(Value::Null, now_playing))
            }
            "pause" => {
                let p = self.player.as_mut().ok_or("no video is open")?;
                match p.phase() {
                    Phase::Failed(_) => Err(playback(p)),
                    Phase::Ended => Ok(now_playing(p)),
                    _ => {
                        p.pause(now);
                        Ok(now_playing(p))
                    }
                }
            }
            "seek" => {
                if self.player.is_none() {
                    return Err("no video is open: play one first".into());
                }
                self.agent_sync()?;
                let p = self.player.as_mut().ok_or("no video is open")?;
                if p.info().is_none() {
                    return Err("the video is still opening: ask again in a moment".into());
                }
                if !p.seekable() {
                    return Err("this video cannot be sought".into());
                }
                let duration = p.duration_us().ok_or("this video cannot be sought (its length is not known)")?;
                let duration_ms = duration.max(0) as u64 / 1000;
                let secs = arg_f64(args, "seconds")?;
                let relative = arg_bool(args, "relative").unwrap_or(false);
                let position_ms = p.position_us(now).max(0) as u64 / 1000;
                let ms = match control::seek_target(position_ms, duration_ms, secs, relative, 0) {
                    Ok(ms) => ms,
                    Err(SeekError::BeforeStart) => {
                        return Err("'seconds' counts from the start of the video and cannot be negative".into());
                    }
                    Err(SeekError::PastEnd) => return Err(format!("{} is only {} long", p.title(), time(duration_ms))),
                    Err(SeekError::NotANumber) => return Err("'seconds' is not a number of seconds".into()),
                };
                p.seek_to(ms as i64 * 1000, vproto::media::SeekMode::Exact, now);
                Ok(json!({
                    "title": p.title(),
                    "position": time(ms),
                    "duration": time(duration_ms),
                }))
            }
            "set_volume" => {
                let current = (self.volume * 100.0 + 0.5) as u32;
                let level = control::volume(current, arg_opt_int(args, "level")?, arg_opt_int(args, "change")?);
                if let Some(level) = level {
                    self.set_volume(level as f32 / 100.0);
                }
                if let Some(mute) = arg_bool(args, "mute")
                    && mute != self.muted
                {
                    self.set_muted(mute);
                }
                Ok(json!({ "volume": (self.volume * 100.0 + 0.5) as u32, "muted": self.muted }))
            }
            "full_screen" => {
                let on = arg_bool(args, "on").ok_or("say whether full screen should be on (true) or off (false)")?;
                if self.player.is_none() && on {
                    return Err("full screen is for a video: play one first".into());
                }
                self.want_full_screen = Some(on);
                Ok(json!({ "full_screen": on }))
            }
            "list_videos" => {
                let query = arg_opt_str(args, "query");
                if self.entries.is_empty() {
                    return Err(self.empty_library());
                }
                let q = query.map(|q| query_words(q, FILLER)).unwrap_or_default();
                let found: Vec<usize> = (0..self.entries.len())
                    .filter(|&i| {
                        query.is_none()
                            || score(&q, &self.entries[i].title) >= MIN_SCORE
                            || score(&q, vfiles::path::file_stem(&self.entries[i].name)) >= MIN_SCORE
                    })
                    .collect();
                if found.is_empty() {
                    return Err(format!(
                        "no video in the library matches \u{201c}{}\u{201d}",
                        query.unwrap_or_default()
                    ));
                }
                let videos: Vec<Value> = found.iter().take(MAX_VIDEOS).map(|&i| self.video(&self.entries[i])).collect();
                Ok(json!({ "videos": videos, "total": found.len(), "cut_short": found.len() > MAX_VIDEOS }))
            }
            "show_library" => {
                self.show_library();
                Ok(json!({ "view": "library", "videos": self.entries.len() }))
            }
            other => Err(format!("Videos has no action called {other}")),
        }
    }
}
