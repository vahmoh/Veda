// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Videos remembers between runs, in `~/.config/videos/state`: its
//! volume, where each video was left, and which were watched to the end.
//!
//! The file is text, one setting a line:
//!
//! ```text
//! videos 1
//! volume 85
//! muted 0
//! resume<TAB>position ms<TAB>duration ms<TAB>size<TAB>modified<TAB>when (s)<TAB>path
//! watched<TAB>path
//! ```
//!
//! A video is known by its path, size and modification time: replaced by
//! another file of the same name, it starts from the beginning. Every write
//! of the home folder is saved whole, so the player writes this rarely.
//!
//! The library asks for every card it draws where its video was left: the
//! videos are kept by path, and [`State::version`] says when what was left
//! changed (the library keeps what it worked out until then).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Positions closer than this to the start are not worth resuming (ms).
pub const RESUME_FROM_MS: u64 = 10_000;
/// ... nor closer than this to the end: the video was watched.
pub const RESUME_UNTIL_END_MS: u64 = 15_000;
/// Videos remembered at most (the least recent go first).
pub const MAX_RESUME: usize = 200;
/// Videos remembered as watched at most (the least recent go first).
pub const MAX_WATCHED: usize = 500;

/// Where a video was left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    pub path: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub size: u64,
    pub modified: u64,
    /// When it was left (Unix seconds).
    pub at_s: u64,
}

/// What Videos remembers.
#[derive(Debug, Clone)]
pub struct State {
    /// Percent.
    pub volume: u32,
    pub muted: bool,
    /// Where videos were left, by path.
    resume: BTreeMap<String, Resume>,
    /// The videos watched to the end, by path, each with its place in the
    /// order they were watched in (the later, the larger).
    watched: BTreeMap<String, u64>,
    /// The next place in that order.
    next_watched: u64,
    /// Counted up whenever where videos were left, or which were watched,
    /// changes.
    version: u64,
}

impl Default for State {
    fn default() -> State {
        State {
            volume: 100,
            muted: false,
            resume: BTreeMap::new(),
            watched: BTreeMap::new(),
            next_watched: 0,
            version: 0,
        }
    }
}

impl PartialEq for State {
    /// The same settings and videos (whatever their versions).
    fn eq(&self, other: &State) -> bool {
        self.volume == other.volume
            && self.muted == other.muted
            && self.resume == other.resume
            && self.watched_in_order() == other.watched_in_order()
    }
}

/// Whether a path can be written on one line.
fn storable(path: &str) -> bool {
    !path.is_empty() && !path.contains(['\t', '\n', '\r'])
}

impl State {
    /// Reads the file's text; what it does not understand is left out.
    pub fn parse(text: &str) -> State {
        let mut state = State::default();
        let mut watched = Vec::new();
        for line in text.lines() {
            let mut fields = line.split('\t');
            let first = fields.next().unwrap_or("");
            if let Some(v) = first.strip_prefix("volume ") {
                state.volume = v.trim().parse::<u32>().map_or(state.volume, |v| v.min(100));
            } else if let Some(v) = first.strip_prefix("muted ") {
                state.muted = v.trim() == "1";
            } else if first == "resume" {
                let f: Vec<&str> = fields.collect();
                if let [pos, dur, size, modified, at, path] = f[..]
                    && storable(path)
                {
                    let n = |s: &str| s.parse::<u64>().ok();
                    if let (Some(position_ms), Some(duration_ms), Some(size), Some(modified), Some(at_s)) =
                        (n(pos), n(dur), n(size), n(modified), n(at))
                    {
                        let r = Resume { path: path.to_string(), position_ms, duration_ms, size, modified, at_s };
                        state.resume.insert(r.path.clone(), r);
                    }
                }
            } else if first == "watched"
                && let Some(path) = fields.next()
                && storable(path)
                && !watched.contains(&path)
            {
                watched.push(path);
            }
        }
        // The file lists the most recent first.
        watched.truncate(MAX_WATCHED);
        for path in watched.into_iter().rev() {
            state.watch(path);
        }
        while state.resume.len() > MAX_RESUME {
            state.forget_oldest();
        }
        state
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = format!("videos 1\nvolume {}\nmuted {}\n", self.volume.min(100), self.muted as u32);
        let mut resume: Vec<&Resume> = self.resume.values().collect();
        resume.sort_by_key(|r| core::cmp::Reverse(r.at_s));
        for r in resume {
            out += &format!(
                "resume\t{}\t{}\t{}\t{}\t{}\t{}\n",
                r.position_ms, r.duration_ms, r.size, r.modified, r.at_s, r.path
            );
        }
        for w in self.watched_in_order() {
            out += &format!("watched\t{w}\n");
        }
        out
    }

    /// The videos watched, the most recent first.
    fn watched_in_order(&self) -> Vec<&str> {
        let mut v: Vec<(&String, &u64)> = self.watched.iter().collect();
        v.sort_by_key(|(_, order)| core::cmp::Reverse(**order));
        v.into_iter().map(|(p, _)| p.as_str()).collect()
    }

    /// Counted up whenever where videos were left, or which were watched,
    /// changes.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Videos remembered part way.
    pub fn resume_count(&self) -> usize {
        self.resume.len()
    }

    /// Where to resume `path` (ms), if it was left part way and is the
    /// same file.
    pub fn resume_at(&self, path: &str, size: u64, modified: u64) -> Option<u64> {
        self.resume.get(path).filter(|r| r.size == size && r.modified == modified).map(|r| r.position_ms)
    }

    /// The entry for `path`, if any (whatever the file).
    pub fn resume_of(&self, path: &str) -> Option<&Resume> {
        self.resume.get(path)
    }

    /// Whether `path` was watched to the end.
    pub fn is_watched(&self, path: &str) -> bool {
        self.watched.contains_key(path)
    }

    /// Notes `path` as watched (the most recent), the least recent going
    /// beyond [`MAX_WATCHED`].
    fn watch(&mut self, path: &str) {
        self.watched.insert(path.to_string(), self.next_watched);
        self.next_watched += 1;
        if self.watched.len() > MAX_WATCHED
            && let Some(oldest) = self.watched.iter().min_by_key(|(_, order)| **order).map(|(p, _)| p.clone())
        {
            self.watched.remove(&oldest);
        }
    }

    /// Forgets the video left the longest ago.
    fn forget_oldest(&mut self) {
        if let Some(oldest) = self.resume.values().min_by_key(|r| r.at_s).map(|r| r.path.clone()) {
            self.resume.remove(&oldest);
        }
    }

    /// Notes that `path` was left at `position_ms` of `duration_ms` (at
    /// `now_s`): remembered part way through; watched at the end; forgotten
    /// barely begun. Returns whether anything changed.
    pub fn remember(
        &mut self,
        path: &str,
        position_ms: u64,
        duration_ms: u64,
        size: u64,
        modified: u64,
        now_s: u64,
    ) -> bool {
        if !storable(path) {
            return false;
        }
        let mut changed = false;
        let left = Resume { path: path.to_string(), position_ms, duration_ms, size, modified, at_s: now_s };
        if duration_ms > 0 && position_ms.saturating_add(RESUME_UNTIL_END_MS) >= duration_ms {
            changed |= self.resume.remove(path).is_some();
            if !self.is_watched(path) {
                self.watch(path);
                changed = true;
            }
        } else if position_ms >= RESUME_FROM_MS {
            changed |= self.resume.get(path) != Some(&left);
            self.resume.insert(left.path.clone(), left);
            if self.resume.len() > MAX_RESUME {
                self.forget_oldest();
            }
        } else {
            changed |= self.resume.remove(path).is_some();
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Forgets where `path` was left (started over).
    pub fn forget(&mut self, path: &str) -> bool {
        let forgotten = self.resume.remove(path).is_some();
        if forgotten {
            self.version += 1;
        }
        forgotten
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_round_trips() {
        let mut s = State { volume: 85, muted: true, ..State::default() };
        assert!(s.remember("/home/user/Videos/A b.mkv", 65_000, 600_000, 1234, 99, 1_700_000_000));
        assert!(s.remember("/home/user/Videos/End.mp4", 595_000, 600_000, 1, 2, 1_700_000_100));
        assert!(s.remember("/home/user/Videos/Later.mp4", 595_000, 600_000, 1, 2, 1_700_000_200));
        let text = s.to_text();
        assert_eq!(State::parse(&text), s);
        assert_eq!(s.resume_at("/home/user/Videos/A b.mkv", 1234, 99), Some(65_000));
        assert_eq!(s.resume_at("/home/user/Videos/A b.mkv", 1235, 99), None, "another file of that name");
        assert!(s.is_watched("/home/user/Videos/End.mp4"));
        assert!(text.ends_with("watched\t/home/user/Videos/Later.mp4\nwatched\t/home/user/Videos/End.mp4\n"));
    }

    #[test]
    fn what_is_worth_remembering() {
        let mut s = State::default();
        assert!(!s.remember("/v.mkv", 5_000, 600_000, 0, 0, 1), "barely begun");
        assert!(s.remember("/v.mkv", 30_000, 600_000, 0, 0, 1));
        assert!(!s.remember("/v.mkv", 30_000, 600_000, 0, 0, 1), "the same again");
        assert!(s.remember("/v.mkv", 590_000, 600_000, 0, 0, 2), "near the end: watched");
        assert!(s.resume_count() == 0 && s.is_watched("/v.mkv"));
        assert!(!s.remember("bad\tpath", 30_000, 600_000, 0, 0, 1));
        assert!(s.remember("/w.mkv", 30_000, 0, 0, 0, 5), "no duration known: remembered");
        let v = s.version();
        assert!(s.forget("/w.mkv") && !s.forget("/w.mkv"));
        assert_eq!(s.version(), v + 1, "a change counts once");
        assert!(!s.remember("/x.mkv", u64::MAX, 600_000, 0, 0, 1) || s.is_watched("/x.mkv"));
    }

    #[test]
    fn damaged_files_and_limits() {
        let s = State::parse("volume 250\nmuted x\nresume\t1\t2\nresume\tx\t2\t3\t4\t5\t/p\nwatched\nnoise\n");
        assert_eq!(s, State { volume: 100, ..State::default() });
        let mut s = State::default();
        for i in 0..MAX_RESUME as u64 + 10 {
            s.remember(&format!("/v{i}.mkv"), 20_000, 600_000, 0, 0, i);
        }
        assert_eq!(s.resume_count(), MAX_RESUME);
        assert!(s.resume_of("/v0.mkv").is_none(), "the least recent went");
        assert!(s.resume_of(&format!("/v{}.mkv", MAX_RESUME + 9)).is_some());
        for i in 0..MAX_WATCHED as u64 + 3 {
            s.remember(&format!("/w{i}.mkv"), 600_000, 600_000, 0, 0, i);
        }
        assert!(!s.is_watched("/w0.mkv") && s.is_watched(&format!("/w{}.mkv", MAX_WATCHED + 2)));
        assert_eq!(State::parse(&s.to_text()), s);
    }
}
