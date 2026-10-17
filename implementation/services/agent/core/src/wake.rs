// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Recognising that someone is talking to the agent.
//!
//! While asleep the agent hears transcripts of what is said near the
//! computer. It wakes when it is addressed by name — "Veda, play some
//! music", "Hey Veda", "what time is it, Veda?" — but not when it is only
//! mentioned ("I was telling Anna about Veda yesterday").
//!
//! A call starts or ends a sentence (or a clause: the recogniser
//! punctuates), so a transcript that also holds other things — a song's
//! words, someone else talking — still wakes it when the name starts or
//! ends one of its sentences. The recogniser does not always spell the name
//! the same way: one letter off counts too ("Vida"), except for common
//! words ("very" does not call an agent named Vera).
//!
//! Hearing costs: a [`Budget`] caps the speech recognition that listening
//! for the name may use each hour. And a sound that held no words (music,
//! a television) is a [`Background`]: while it goes on, only a sound that
//! [stands out](stands_out) from it is listened to, so that it does not
//! keep the recogniser busy.

use alloc::string::String;
use alloc::vec::Vec;

/// Words people put before a name when calling someone.
const OPENERS: &[&str] =
    &["hey", "hi", "hello", "ok", "okay", "yo", "oh", "so", "um", "uh", "well", "and", "excuse", "me", "dear"];

/// Common words never taken for a name one letter off ("very" is not
/// "Vera", "now" is not "Nova").
const COMMON: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "an", "and", "any", "are", "area", "as", "ask", "at", "away",
    "back", "bad", "be", "bear", "been", "best", "big", "both", "but", "by", "call", "came", "can", "car", "care",
    "come", "could", "day", "dear", "did", "do", "does", "done", "down", "each", "else", "era", "even", "ever",
    "every", "eye", "far", "fear", "feel", "few", "fine", "for", "from", "game", "get", "give", "go", "good", "got",
    "had", "has", "have", "he", "hear", "her", "here", "hey", "him", "his", "home", "how", "if", "in", "into", "is",
    "it", "its", "just", "keep", "kind", "know", "last", "let", "like", "line", "look", "lot", "made", "make", "man",
    "many", "may", "me", "mean", "more", "most", "move", "much", "must", "my", "name", "near", "need", "never", "new",
    "next", "nice", "no", "nor", "not", "now", "of", "off", "oh", "ok", "old", "on", "once", "one", "only", "open",
    "or", "our", "out", "over", "own", "part", "play", "put", "real", "right", "said", "same", "say", "see", "seem",
    "she", "show", "so", "some", "song", "stop", "such", "sure", "take", "tell", "than", "that", "the", "them", "then",
    "there", "these", "they", "this", "time", "to", "too", "turn", "two", "up", "us", "use", "vary", "verb", "very",
    "want", "was", "way", "we", "wear", "well", "went", "were", "what", "when", "where", "who", "why", "will", "with",
    "work", "would", "year", "yes", "yet", "you", "your",
];

/// Lower-case words without punctuation.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || is_break(c) || c == '"')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Punctuation that ends a sentence or a clause.
fn is_break(c: char) -> bool {
    matches!(c, ',' | '.' | '!' | '?' | ';' | ':')
}

/// Edit distance, for names the recogniser spells slightly differently.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = alloc::vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Whether `word` is the name: exactly, with a possessive "'s", or one
/// letter off for names of four letters or more (unless it is a common
/// word).
fn is_name(word: &str, name: &str) -> bool {
    let word = word.strip_suffix("'s").unwrap_or(word);
    word == name || (name.chars().count() >= 4 && !COMMON.contains(&word) && distance(word, name) <= 1)
}

/// Whether `word` sounds a little like the name (two letters off at most):
/// for the log, when a transcript came close but did not wake the agent.
pub fn resembles(transcript: &str, name: &str) -> bool {
    let Some(name) = words(name).into_iter().next() else { return false };
    words(transcript).iter().any(|w| distance(w, &name) <= 2 && !COMMON.contains(&w.as_str()))
}

/// If `transcript` addresses the agent called `name`, what was asked
/// (possibly nothing: "Hey Veda" just wakes it).
pub fn addressed(transcript: &str, name: &str) -> Option<String> {
    find_call(transcript, name).map(|(_, request)| request)
}

/// If `transcript` addresses the agent called `name`, the transcript from
/// the call on: what comes before it (a song's words, someone else) is
/// left out ("... moonlight. Veda, turn it down." is "Veda, turn it down.").
pub fn call(transcript: &str, name: &str) -> Option<String> {
    find_call(transcript, name).map(|(at, _)| String::from(transcript[at..].trim()))
}

/// Where the call starts in `transcript` (a byte offset) and what was
/// asked.
fn find_call(transcript: &str, name: &str) -> Option<(usize, String)> {
    let name_words = words(name);
    let name = name_words.first()?;
    // Words before each clause, over the whole transcript, and the clause
    // before if a comma joined it to this one (the same sentence).
    let mut before = 0;
    let mut at = 0;
    let mut joined: Option<(usize, Vec<String>)> = None;
    for piece in transcript.split_inclusive(is_break) {
        let w = words(piece);
        // Called at the start, after an opener or two: "Hey Veda, ..."
        let start = w.iter().take_while(|x| OPENERS.contains(&x.as_str())).count();
        if start <= 2 && w.get(start).is_some_and(|x| is_name(x, name)) {
            let rest = after_words(transcript, before + start + name_words.len());
            if !rest.chars().any(char::is_alphanumeric)
                && let Some((joined_at, joined)) = joined
            {
                // Only the name, after the request in the same sentence:
                // "What time is it, Veda?"
                return Some((joined_at, request_before(&joined)));
            }
            if !rest.chars().any(char::is_alphanumeric) {
                return Some((at, String::new()));
            }
            return Some((at, rest));
        }
        // Called at the end: "... what time is it Veda"
        if w.len() >= 2 && w.last().is_some_and(|x| is_name(x, name)) {
            return Some((at, request_before(&w[..w.len() - 1])));
        }
        before += w.len();
        joined = piece.trim_end().ends_with(',').then_some((at, w));
        at += piece.len();
    }
    None
}

/// What was asked before the name: the words, without openers at the end
/// ("play some music, hey" is "play some music").
fn request_before(w: &[String]) -> String {
    let mut end = w.len();
    while end > 0 && OPENERS.contains(&w[end - 1].as_str()) {
        end -= 1;
    }
    w[..end].join(" ")
}

/// How much speech recognition listening for the name may use: so much in
/// each period (ten minutes an hour, say), so that a television or music
/// playing all day cannot run up the bill. It belongs to the agent rather
/// than to one stretch of listening, which starts anew at every wake, after
/// every conversation and on every change of settings: what a period used
/// counts until the period is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    allowed_ns: u64,
    period_ns: u64,
    /// When the current period began, and how much it has used.
    start_ns: u64,
    used_ns: u64,
    /// Running out was reported in this period.
    reported: bool,
}

impl Budget {
    /// `allowed_ns` of recognition in every `period_ns`, the first period
    /// beginning at `now`.
    pub fn new(allowed_ns: u64, period_ns: u64, now: u64) -> Budget {
        Budget { allowed_ns, period_ns: period_ns.max(1), start_ns: now, used_ns: 0, reported: false }
    }

    /// Whether recognition may start at `now`.
    pub fn allows(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.start_ns) >= self.period_ns {
            self.start_ns = now;
            self.used_ns = 0;
            self.reported = false;
        }
        self.used_ns < self.allowed_ns
    }

    /// Records `ns` of recognition.
    pub fn spend(&mut self, ns: u64) {
        self.used_ns = self.used_ns.saturating_add(ns);
    }

    /// True the first time in a period that it is asked after running out
    /// (to say so once).
    pub fn report_exhausted(&mut self) -> bool {
        !core::mem::replace(&mut self.reported, true)
    }
}

/// Over a [`Background`], a sound must be this much louder than the
/// background's usual peaks to be listened to ...
pub const STAND_OUT_DB: f32 = 10.0;
/// ... than the loudest tenth of the levels of the last seconds (music
/// builds up).
const PEAKS: f32 = 0.9;
/// A background has stopped once it has been this much quieter than its
/// usual level ...
pub const BACKGROUND_GONE_DB: f32 = 15.0;
/// ... for this long: longer than the pause between two songs.
pub const BACKGROUND_GONE_NS: u64 = 5_000_000_000;

/// The level below which `share` of `levels` lie (`None` for none).
fn quantile(levels: impl Iterator<Item = f32>, share: f32) -> Option<f32> {
    let mut sorted: Vec<f32> = levels.collect();
    sorted.sort_by(f32::total_cmp);
    let i = ((sorted.len() as f32 * share) as usize).min(sorted.len().checked_sub(1)?);
    Some(sorted[i])
}

/// Whether the newest of `recent` levels (dBFS) are clearly louder than
/// the usual peaks of `window`, the levels of the last seconds that hold
/// them: the loudest of `recent` is [`STAND_OUT_DB`] above the loudest tenth
/// of `window`. A call over music does; the music's usual loud moments do
/// not (once maybe, as it grows much louder than it has been: a quiet
/// start).
pub fn stands_out(window: &[f32], recent: &[f32]) -> bool {
    let peaks = quantile(window.iter().copied(), PEAKS).unwrap_or(f32::NEG_INFINITY);
    let now = recent.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    now > peaks + STAND_OUT_DB
}

/// A sound going on that held no words — music, a television — as the
/// listener found it: of its usual level until it stops. It stops once the
/// sound has been [`BACKGROUND_GONE_DB`] below that level for
/// [`BACKGROUND_GONE_NS`], whatever a voice detector makes of it: quiet
/// passages of music sound like nobody speaking, but are not its end, and a
/// detector that took them for it would have the next loud passage listened
/// to as speech again.
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /// Its usual level (dBFS): the median of the sound when it was found.
    usual_db: f32,
    /// When it was last heard near that level.
    heard_ns: u64,
}

impl Background {
    /// The sound of `levels` (dBFS, of the last seconds), found at `now` to
    /// hold no words; `None` without levels.
    pub fn new(levels: impl Iterator<Item = f32>, now: u64) -> Option<Background> {
        Some(Background { usual_db: quantile(levels, 0.5)?, heard_ns: now })
    }

    /// Its usual level (dBFS).
    pub fn usual_db(&self) -> f32 {
        self.usual_db
    }

    /// Takes the level of the sound at `now`: whether the background goes
    /// on.
    pub fn hear(&mut self, level_db: f32, now: u64) -> bool {
        if level_db >= self.usual_db - BACKGROUND_GONE_DB {
            self.heard_ns = now;
        }
        now.saturating_sub(self.heard_ns) <= BACKGROUND_GONE_NS
    }
}

/// The transcript after its first `skip` words, keeping its punctuation.
fn after_words(transcript: &str, skip: usize) -> String {
    let mut seen = 0;
    let mut rest = transcript;
    while seen < skip {
        let t = rest.trim_start_matches(|c: char| !c.is_alphanumeric());
        let end = t.find(|c: char| c.is_whitespace() || is_break(c) || c == '"').unwrap_or(t.len());
        rest = &t[end..];
        seen += 1;
    }
    String::from(rest.trim_start_matches(|c: char| c.is_whitespace() || is_break(c)).trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_when_called() {
        assert_eq!(addressed("Veda, play some music.", "Veda").as_deref(), Some("play some music."));
        assert_eq!(addressed("Hey Veda!", "Veda").as_deref(), Some(""));
        assert_eq!(addressed("hey veda what's the weather like", "Veda").as_deref(), Some("what's the weather like"));
        assert_eq!(addressed("Okay so um Veda open my notes", "Veda"), None, "three openers is not calling");
        assert_eq!(addressed("OK Veda, open my notes", "Veda").as_deref(), Some("open my notes"));
        assert_eq!(addressed("What time is it, Veda?", "Veda").as_deref(), Some("what time is it"));
        assert_eq!(addressed("Hey Samantha, hi", "Samantha").as_deref(), Some("hi"));
        // The recogniser may be one letter off.
        assert_eq!(addressed("Hey Samanta, lights", "Samantha").as_deref(), Some("lights"));
        assert_eq!(addressed("Vida, stop the music.", "Veda").as_deref(), Some("stop the music."));
        assert_eq!(addressed("Hey, Beda.", "Veda").as_deref(), Some(""));
    }

    #[test]
    fn wakes_amid_other_words() {
        // A song or someone else before the call, in the same transcript.
        assert_eq!(
            addressed("Dancing in the moonlight. Veda, turn it down.", "Veda").as_deref(),
            Some("turn it down.")
        );
        assert_eq!(addressed("and then we went home. Hey Veda.", "Veda").as_deref(), Some(""));
        assert_eq!(addressed("Play some music, hey Veda?", "Veda").as_deref(), Some("play some music"));
        assert_eq!(addressed("I mean it. What's the time Veda", "Veda").as_deref(), Some("what's the time"));
    }

    #[test]
    fn the_call_without_what_came_before() {
        let call = |t| super::call(t, "Veda");
        assert_eq!(call("Dancing in the moonlight. Veda, turn it down.").as_deref(), Some("Veda, turn it down."));
        assert_eq!(call("Hey Veda, what time is it?").as_deref(), Some("Hey Veda, what time is it?"));
        assert_eq!(call("Oh well. What time is it, Veda?").as_deref(), Some("What time is it, Veda?"));
        assert_eq!(call("I mean it. What's the time Veda").as_deref(), Some("What's the time Veda"));
        assert_eq!(call("I was telling Anna about Veda yesterday"), None);
    }

    #[test]
    fn ignores_mentions() {
        assert_eq!(addressed("I was telling Anna about Veda yesterday", "Veda"), None);
        assert_eq!(addressed("Veda's voice is nice, isn't it", "Veda").as_deref(), Some("voice is nice, isn't it"));
        assert_eq!(addressed("", "Veda"), None);
        assert_eq!(addressed("very nice", "Vera"), None, "a common word is not the name");
        assert_eq!(addressed("Very good. Very nice.", "Vera"), None);
        assert_eq!(addressed("Now play it", "Nova"), None);
        assert_eq!(addressed("Veda", ""), None);
    }

    #[test]
    fn the_budget_lasts_its_period() {
        const MIN: u64 = 60_000_000_000;
        let mut b = Budget::new(10 * MIN, 60 * MIN, 0);
        assert!(b.allows(0));
        // Ten stretches of a minute, whoever listened: the period's budget.
        for i in 0..10 {
            assert!(b.allows(i * MIN));
            b.spend(MIN);
        }
        assert!(!b.allows(20 * MIN));
        assert!(b.report_exhausted() && !b.report_exhausted());
        assert!(!b.allows(59 * MIN));
        // A new period starts afresh.
        assert!(b.allows(61 * MIN));
        assert!(b.report_exhausted());
    }

    /// 20 ms frames of `ms` milliseconds, from `start` (ns), each at the
    /// level `level(i)` for frame `i`.
    fn frames(start: u64, ms: u64, level: impl Fn(u64) -> f32) -> impl Iterator<Item = (u64, f32)> {
        (0..ms / 20).map(move |i| (start + i * 20_000_000, level(i)))
    }

    /// Music: beats between -14 and -26 dBFS.
    fn music(i: u64) -> f32 {
        if i % 15 < 5 { -14.0 } else { -26.0 }
    }

    /// A background found over ten seconds of music, at `at`.
    fn over_music(at: u64) -> Background {
        let levels: Vec<f32> = frames(0, 10_000, music).map(|(_, l)| l).collect();
        Background::new(levels.into_iter(), at).unwrap()
    }

    #[test]
    fn music_goes_on_through_quiet_passages_and_pauses() {
        const S: u64 = 1_000_000_000;
        let mut b = over_music(10 * S);
        assert_eq!(b.usual_db(), -26.0);
        // A quiet bridge (10 dB under the music) for half a minute, then the
        // pause between two songs, then the next song.
        let quiet = frames(10 * S, 30_000, |i| if i % 50 == 0 { -30.0 } else { -36.0 });
        let pause = frames(40 * S, 2_000, |_| -96.0);
        let next = frames(42 * S, 10_000, music);
        assert!(quiet.chain(pause).chain(next).all(|(t, l)| b.hear(l, t)), "the music went on");
    }

    #[test]
    fn a_background_stops_when_the_sound_does() {
        const S: u64 = 1_000_000_000;
        let mut b = over_music(10 * S);
        // The music is stopped: the room's quiet, which ends it after five
        // seconds, not before.
        let silence: Vec<(u64, f32)> = frames(10 * S, 8_000, |_| -70.0).collect();
        let going_on: Vec<bool> = silence.iter().map(|&(t, l)| b.hear(l, t)).collect();
        let stopped = going_on.iter().position(|on| !on).expect("it stopped");
        let after = silence[stopped].0 - 10 * S;
        assert!((BACKGROUND_GONE_NS..BACKGROUND_GONE_NS + 100_000_000).contains(&after), "after {after} ns");
        // Turned down, it goes on; turned far down, it is as good as
        // stopped (a new background is found if it goes on without words).
        let mut b = over_music(10 * S);
        assert!(frames(10 * S, 6_000, |i| music(i) - 20.0).all(|(t, l)| b.hear(l, t)));
        let mut b = over_music(10 * S);
        assert!(!frames(10 * S, 6_000, |i| music(i) - 35.0).all(|(t, l)| b.hear(l, t)));
    }

    #[test]
    fn a_call_stands_out_from_music() {
        let window: Vec<f32> = frames(0, 10_000, music).map(|(_, l)| l).collect();
        // The music's own loud moments do not; someone calling over it does.
        assert!(!stands_out(&window, &[-14.0, -15.0]));
        assert!(!stands_out(&window, &[-5.0]));
        assert!(stands_out(&window, &[-20.0, -3.0]));
        // Over quiet, anything louder than the quiet's peaks by enough does.
        let quiet = [-80.0f32; 500];
        assert!(stands_out(&quiet, &[-60.0]) && !stands_out(&quiet, &[-75.0]));
        assert!(stands_out(&[], &[-90.0]), "no window: no background to stand out from");
    }

    #[test]
    fn notices_near_misses() {
        assert!(resembles("Hi Vita, how are you", "Veda"));
        assert!(!resembles("very nice indeed", "Vera"));
        assert!(!resembles("play some music", "Veda"));
    }
}
