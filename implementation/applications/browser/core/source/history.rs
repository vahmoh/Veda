// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pages visited, the latest first, and what the address bar suggests
//! of them: the ones whose address or title begins with what is typed (a
//! word of the title), then those that contain it, each by how often and
//! how lately they were visited.
//!
//! Its file has a line a page: when it was visited last (seconds since
//! 1970), how often, its address and its title.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::text;
use crate::url;

/// The most pages kept: older ones are forgotten.
pub const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    pub visits: u32,
    /// Seconds since 1970.
    pub last: u64,
}

impl Entry {
    /// How much it is visited, as of `now`: visits, worth less as they age
    /// (half in a week, a tenth in two months).
    fn frecency(&self, now: u64) -> u64 {
        let days = now.saturating_sub(self.last) / 86_400;
        u64::from(self.visits.min(1000)) * 1000 / (1 + days / 7)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// The latest first.
    entries: Vec<Entry>,
}

/// Whether a page at `url` is kept: the web's and files, not the browser's
/// own pages nor data.
fn kept(url: &str) -> bool {
    url::is_web(url) || url.starts_with("file:")
}

impl History {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The history a file has.
    pub fn parse(file: &str) -> History {
        let mut entries: Vec<Entry> = text::records(file)
            .filter_map(|f| {
                let [last, visits, url, title] = <[String; 4]>::try_from(f).ok()?;
                Some(Entry { url, title, visits: visits.parse().ok()?, last: last.parse().ok()? })
            })
            .filter(|e| kept(&e.url))
            .collect();
        entries.sort_by_key(|e| core::cmp::Reverse(e.last));
        entries.dedup_by(|b, a| a.url == b.url);
        entries.truncate(MAX_ENTRIES);
        History { entries }
    }

    /// Its file.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&text::line(&[&e.last.to_string(), &e.visits.to_string(), &e.url, &e.title]));
            out.push('\n');
        }
        out
    }

    /// `url` was visited at `now`, titled `title` if it is known.
    pub fn visit(&mut self, url: &str, title: &str, now: u64) {
        if !kept(url) {
            return;
        }
        let mut entry = match self.entries.iter().position(|e| e.url == url) {
            Some(i) => self.entries.remove(i),
            None => Entry { url: url.to_string(), title: String::new(), visits: 0, last: now },
        };
        entry.visits = entry.visits.saturating_add(1);
        entry.last = now;
        if !title.is_empty() {
            entry.title = title.to_string();
        }
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
    }

    /// The page at `url` is titled `title`.
    pub fn retitle(&mut self, url: &str, title: &str) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.url == url) {
            e.title = title.to_string();
        }
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.url != url);
        self.entries.len() != before
    }

    /// Forgets the pages visited since `since` (0: all of them).
    pub fn clear_since(&mut self, since: u64) {
        self.entries.retain(|e| e.last < since);
    }

    /// What the address bar suggests for `typed`, `limit` at most.
    pub fn suggest(&self, typed: &str, now: u64, limit: usize) -> Vec<&Entry> {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(u32, u64, &Entry)> =
            self.entries.iter().filter_map(|e| Some((matches(e, &typed)?, e.frecency(now), e))).collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        found.into_iter().take(limit).map(|(_, _, e)| e).collect()
    }

    /// The sites visited most, as of `now`, each by its page visited
    /// most: what the new tab offers.
    pub fn top(&self, now: u64, limit: usize) -> Vec<&Entry> {
        // Each site's visits (all its pages'), and its page visited most.
        let mut sites: Vec<(Option<&str>, u64, &Entry)> = Vec::new();
        for e in &self.entries {
            let (host, f) = (url::host(&e.url), e.frecency(now));
            match sites.iter_mut().find(|s| s.0 == host) {
                Some(site) => {
                    site.1 += f;
                    if f > site.2.frecency(now) {
                        site.2 = e;
                    }
                }
                None => sites.push((host, f, e)),
            }
        }
        sites.sort_by_key(|s| core::cmp::Reverse(s.1));
        sites.into_iter().take(limit).map(|s| s.2).collect()
    }
}

/// How well `e` matches `typed` (lower case): 3, its address begins with
/// it; 2, a word of its title does; 1, either contains it.
fn matches(e: &Entry, typed: &str) -> Option<u32> {
    let address = e.url.to_lowercase();
    let bare = address.split_once("://").map_or(address.as_str(), |(_, rest)| rest);
    let bare = bare.strip_prefix("www.").unwrap_or(bare);
    let title = e.title.to_lowercase();
    if bare.starts_with(typed) || address.starts_with(typed) {
        Some(3)
    } else if title.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(typed)) {
        Some(2)
    } else if address.contains(typed) || title.contains(typed) {
        Some(1)
    } else {
        None
    }
}

/// How long ago `then` was, as of `now`, as people say it.
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn visits_are_kept_latest_first() {
        let mut h = History::default();
        h.visit("https://a.example/", "A", 100);
        h.visit("https://b.example/", "B", 200);
        h.visit("https://a.example/", "", 300);
        h.visit("about:blank", "", 400);
        h.visit("data:text/plain,x", "", 400);
        let e = h.entries();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].url.as_str(), e[0].title.as_str(), e[0].visits), ("https://a.example/", "A", 2));
        h.retitle("https://b.example/", "Bee");
        assert_eq!(History::parse(&h.serialize()), h);
        assert!(h.remove("https://b.example/") && !h.remove("https://b.example/"));
        h.clear_since(0);
        assert!(h.entries().is_empty());
    }

    #[test]
    fn a_damaged_file_loses_its_bad_lines() {
        let file = "100\t1\thttps://a/\tA\nnonsense\n200\tx\thttps://b/\tB\n300\t2\thttps://c/\tC\n";
        let h = History::parse(file);
        assert_eq!(h.entries().iter().map(|e| e.url.as_str()).collect::<Vec<_>>(), ["https://c/", "https://a/"]);
    }

    #[test]
    fn suggestions_begin_with_what_is_typed() {
        let mut h = History::default();
        let now = 100 * DAY;
        h.visit("https://www.wikipedia.org/", "Wikipedia", now - 50 * DAY);
        h.visit("https://example.com/wiki", "Example", now);
        h.visit("https://news.example/", "Daily Wiki digest", now);
        h.visit("https://www.wikipedia.org/", "Wikipedia", now - 50 * DAY);
        let got: Vec<&str> = h.suggest("wiki", now, 5).iter().map(|e| e.url.as_str()).collect();
        assert_eq!(got, ["https://www.wikipedia.org/", "https://news.example/", "https://example.com/wiki"]);
        assert!(h.suggest("  ", now, 5).is_empty());
        assert_eq!(h.suggest("wiki", now, 1).len(), 1);
    }

    #[test]
    fn the_most_visited_sites() {
        let mut h = History::default();
        for i in 0..5 {
            h.visit(&format!("https://a.example/{i}"), "", 10);
        }
        h.visit("https://b.example/", "", 10);
        let top: Vec<&str> = h.top(10, 4).iter().map(|e| url::host(&e.url).unwrap()).collect();
        assert_eq!(top, ["a.example", "b.example"], "one a site");
        assert_eq!(ago(0, 30), "just now");
        assert_eq!(ago(0, 7200), "2 h ago");
    }
}
