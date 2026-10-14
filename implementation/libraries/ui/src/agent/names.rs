// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Things by their names, as people say them ("play the aurora video",
//! "something by Miles Davis"): what speech recognition heard, scored
//! against names the same way in every application, so that they all
//! understand names alike.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The least [`score`] that counts as a match.
pub const MIN_SCORE: u32 = 30;

/// The lower-case words of `s`.
pub fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// The words of a request that name the thing: all but the `filler` words
/// ("the", "play", the kind of thing asked for), or all of them if every
/// word is a filler word, as in a song called "The Song".
pub fn query_words(query: &str, filler: &[&str]) -> Vec<String> {
    let all = words(query);
    let named: Vec<String> = all.iter().filter(|w| !filler.contains(&w.as_str())).cloned().collect();
    if named.is_empty() { all } else { named }
}

/// How well `query`'s words name `name`: 100 for the same words, less for
/// its first words, some words in a row or some words anywhere, 0 for too
/// little in common.
pub fn score(query: &[String], name: &str) -> u32 {
    let nw = words(name);
    if query.is_empty() || nw.is_empty() {
        return 0;
    }
    let (q, n) = (query.join(" "), nw.join(" "));
    if q == n {
        return 100;
    }
    if q.replace(' ', "") == n.replace(' ', "") {
        return 95;
    }
    if format!("{n} ").starts_with(&format!("{q} ")) {
        return 85;
    }
    if format!(" {n} ").contains(&format!(" {q} ")) {
        return 75;
    }
    // Words in common; a longer word may be the start of the other
    // ("window" and "windows"), as speech recognition hears endings loosely.
    let same = |a: &str, b: &str| a == b || (a.len().min(b.len()) >= 4 && (a.starts_with(b) || b.starts_with(a)));
    let found = query.iter().filter(|w| nw.iter().any(|x| same(w, x))).count();
    if found * 2 < query.len() {
        return 0;
    }
    (found * 60 / query.len().max(nw.len())) as u32
}

/// The best of `names` for `query`'s words: its index, if any scores
/// [`MIN_SCORE`] or more (the first of equals).
pub fn best(query: &[String], names: &[&str]) -> Option<usize> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (i, score(query, n)))
        .filter(|&(_, s)| s >= MIN_SCORE)
        .max_by_key(|&(i, s)| (s, usize::MAX - i))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 3] = ["Aurora Timelapse", "Countdown", "Rising Bubbles"];
    const FILLER: [&str; 5] = ["the", "a", "video", "play", "one"];

    fn find(query: &str) -> Option<usize> {
        best(&query_words(query, &FILLER), &NAMES)
    }

    #[test]
    fn names_as_people_say_them() {
        assert_eq!(find("Aurora Timelapse"), Some(0));
        assert_eq!(find("the aurora video"), Some(0));
        assert_eq!(find("count down"), Some(1));
        assert_eq!(find("play the bubbles one"), Some(2));
        assert_eq!(find("rising bubble"), Some(2), "endings heard loosely");
        assert_eq!(find("casablanca"), None);
        assert_eq!(find(""), None);
    }

    #[test]
    fn scores_from_the_same_words_down() {
        let q = |s: &str| query_words(s, &FILLER);
        assert_eq!(score(&q("so what"), "So What"), 100);
        assert_eq!(score(&q("blue in green"), "Blue-in-Green"), 100);
        assert_eq!(score(&q("count down"), "Countdown"), 95);
        assert_eq!(score(&q("kind of"), "Kind of Blue"), 85);
        assert_eq!(score(&q("of blue"), "Kind of Blue"), 75);
        assert_eq!(score(&q("blue kind"), "Kind of Blue"), 40);
        assert_eq!(score(&q("green dolphin"), "Kind of Blue"), 0);
        // Filler words name nothing, unless they are all there is.
        assert_eq!(query_words("play the video", &FILLER), ["play", "the", "video"]);
        assert_eq!(query_words("play the aurora video", &FILLER), ["aurora"]);
    }
}
