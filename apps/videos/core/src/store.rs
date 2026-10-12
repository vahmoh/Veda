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

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Positions closer than this to the start are not worth resuming (ms).
pub const RESUME_FROM_MS: u64 = 10_000;
/// ... nor closer than this to the end: the video was watched.
pub const RESUME_UNTIL_END_MS: u64 = 15_000;
/// Videos remembered at most (the least recent go first).
pub const MAX_RESUME: usize = 200;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Percent.
    pub volume: u32,
    pub muted: bool,
    pub resume: Vec<Resume>,
    pub watched: Vec<String>,
}

impl Default for State {
    fn default() -> State {
        State { volume: 100, muted: false, resume: Vec::new(), watched: Vec::new() }
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
                        state.resume.push(Resume {
                            path: path.to_string(),
                            position_ms,
                            duration_ms,
                            size,
                            modified,
                            at_s,
                        });
                    }
                }
            } else if first == "watched"
                && let Some(path) = fields.next()
                && storable(path)
            {
                state.watched.push(path.to_string());
            }
        }
        state.resume.truncate(MAX_RESUME);
        state.watched.truncate(MAX_WATCHED);
        state
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = format!("videos 1\nvolume {}\nmuted {}\n", self.volume.min(100), self.muted as u32);
        for r in &self.resume {
            out += &format!(
                "resume\t{}\t{}\t{}\t{}\t{}\t{}\n",
                r.position_ms, r.duration_ms, r.size, r.modified, r.at_s, r.path
            );
        }
        for w in &self.watched {
            out += &format!("watched\t{w}\n");
        }
        out
    }

    /// Where to resume `path` (ms), if it was left part way and is the
    /// same file.
    pub fn resume_at(&self, path: &str, size: u64, modified: u64) -> Option<u64> {
        self.resume.iter().find(|r| r.path == path && r.size == size && r.modified == modified).map(|r| r.position_ms)
    }

    /// The entry for `path`, if any (whatever the file).
    pub fn resume_of(&self, path: &str) -> Option<&Resume> {
        self.resume.iter().find(|r| r.path == path)
    }

    pub fn is_watched(&self, path: &str) -> bool {
        self.watched.iter().any(|w| w == path)
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
        let before = self.clone();
        self.resume.retain(|r| r.path != path);
        if duration_ms > 0 && position_ms + RESUME_UNTIL_END_MS >= duration_ms {
            if !self.is_watched(path) {
                self.watched.insert(0, path.to_string());
                self.watched.truncate(MAX_WATCHED);
            }
        } else if position_ms >= RESUME_FROM_MS {
            self.resume
                .insert(0, Resume { path: path.to_string(), position_ms, duration_ms, size, modified, at_s: now_s });
            self.resume.truncate(MAX_RESUME);
        }
        *self != before
    }

    /// Forgets where `path` was left (started over).
    pub fn forget(&mut self, path: &str) -> bool {
        let n = self.resume.len();
        self.resume.retain(|r| r.path != path);
        n != self.resume.len()
    }

    /// The videos left part way, most recent first.
    pub fn recent(&self) -> Vec<&Resume> {
        let mut v: Vec<&Resume> = self.resume.iter().collect();
        v.sort_by_key(|r| core::cmp::Reverse(r.at_s));
        v
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
        let text = s.to_text();
        assert_eq!(State::parse(&text), s);
        assert_eq!(s.resume_at("/home/user/Videos/A b.mkv", 1234, 99), Some(65_000));
        assert_eq!(s.resume_at("/home/user/Videos/A b.mkv", 1235, 99), None, "another file of that name");
        assert!(s.is_watched("/home/user/Videos/End.mp4"));
    }

    #[test]
    fn what_is_worth_remembering() {
        let mut s = State::default();
        assert!(!s.remember("/v.mkv", 5_000, 600_000, 0, 0, 1), "barely begun");
        assert!(s.remember("/v.mkv", 30_000, 600_000, 0, 0, 1));
        assert!(s.remember("/v.mkv", 590_000, 600_000, 0, 0, 2), "near the end: watched");
        assert!(s.resume.is_empty() && s.is_watched("/v.mkv"));
        assert!(!s.remember("bad\tpath", 30_000, 600_000, 0, 0, 1));
        assert!(s.remember("/w.mkv", 30_000, 0, 0, 0, 5), "no duration known: remembered");
        assert!(s.forget("/w.mkv") && !s.forget("/w.mkv"));
    }

    #[test]
    fn damaged_files_and_limits() {
        let s = State::parse("volume 250\nmuted x\nresume\t1\t2\nresume\tx\t2\t3\t4\t5\t/p\nwatched\nnoise\n");
        assert_eq!(s, State { volume: 100, ..State::default() });
        let mut s = State::default();
        for i in 0..MAX_RESUME as u64 + 10 {
            s.remember(&format!("/v{i}.mkv"), 20_000, 600_000, 0, 0, i);
        }
        assert_eq!(s.resume.len(), MAX_RESUME);
        assert_eq!(s.recent()[0].path, format!("/v{}.mkv", MAX_RESUME + 9));
    }
}
