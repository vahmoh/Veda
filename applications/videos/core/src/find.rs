// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Videos by their names, as people say them ("play the aurora video"):
//! words scored as Music scores songs, so the two apps understand names
//! alike.

use alloc::string::String;
use alloc::vec::Vec;

/// Words that name no video ("the", "video", "play").
const FILLER: [&str; 14] =
    ["the", "a", "an", "my", "video", "videos", "clip", "movie", "film", "play", "watch", "please", "one", "of"];

/// The lower-case words of `s`.
pub fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// The words of a request that name the video (all of them if every word
/// is a filler word, as in a video called "The Movie").
pub fn query_words(query: &str) -> Vec<String> {
    let all = words(query);
    let named: Vec<String> = all.iter().filter(|w| !FILLER.contains(&w.as_str())).cloned().collect();
    if named.is_empty() { all } else { named }
}

/// How well `query`'s words name `field`: 100 for the same words, less for
/// its first words, some words in a row or some words anywhere, 0 for too
/// little in common.
pub fn score(query: &[String], field: &str) -> u32 {
    let fw = words(field);
    if query.is_empty() || fw.is_empty() {
        return 0;
    }
    let (q, f) = (query.join(" "), fw.join(" "));
    if q == f {
        return 100;
    }
    if q.replace(' ', "") == f.replace(' ', "") {
        return 95;
    }
    if alloc::format!("{f} ").starts_with(&alloc::format!("{q} ")) {
        return 85;
    }
    if alloc::format!(" {f} ").contains(&alloc::format!(" {q} ")) {
        return 75;
    }
    // Words in common; a longer word may be the start of the other, as
    // speech recognition hears endings loosely.
    let same = |a: &str, b: &str| a == b || (a.len().min(b.len()) >= 4 && (a.starts_with(b) || b.starts_with(a)));
    let found = query.iter().filter(|w| fw.iter().any(|x| same(w, x))).count();
    if found * 2 < query.len() {
        return 0;
    }
    (found * 60 / query.len().max(fw.len())) as u32
}

/// The best of `names` for `query`: its index, if any matches well enough.
pub fn best(query: &str, names: &[&str]) -> Option<usize> {
    let q = query_words(query);
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (i, score(&q, n)))
        .filter(|&(_, s)| s >= 30)
        .max_by_key(|&(i, s)| (s, usize::MAX - i))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 3] = ["Aurora Timelapse", "Countdown", "Rising Bubbles"];

    #[test]
    fn names_as_people_say_them() {
        assert_eq!(best("Aurora Timelapse", &NAMES), Some(0));
        assert_eq!(best("the aurora video", &NAMES), Some(0));
        assert_eq!(best("count down", &NAMES), Some(1));
        assert_eq!(best("play the bubbles one", &NAMES), Some(2));
        assert_eq!(best("rising bubble", &NAMES), Some(2), "endings heard loosely");
        assert_eq!(best("casablanca", &NAMES), None);
        assert_eq!(best("", &NAMES), None);
    }
}
