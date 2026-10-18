// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The pages kept, in the order they were kept. Its file has a line a
//! page: its address and its title.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::text;

/// The most bookmarks kept.
pub const MAX_BOOKMARKS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    list: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn parse(file: &str) -> Bookmarks {
        let mut list: Vec<Bookmark> = Vec::new();
        for f in text::records(file) {
            if let [url, title] = f.as_slice()
                && !url.is_empty()
                && !list.iter().any(|b| &b.url == url)
            {
                list.push(Bookmark { url: url.clone(), title: title.clone() });
            }
        }
        list.truncate(MAX_BOOKMARKS);
        Bookmarks { list }
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for b in &self.list {
            out.push_str(&text::line(&[&b.url, &b.title]));
            out.push('\n');
        }
        out
    }

    pub fn list(&self) -> &[Bookmark] {
        &self.list
    }

    pub fn contains(&self, url: &str) -> bool {
        self.list.iter().any(|b| b.url == url)
    }

    /// Keeps `url`, or forgets it if it was kept: whether it is now.
    pub fn toggle(&mut self, url: &str, title: &str) -> bool {
        if self.remove(url) {
            return false;
        }
        if self.list.len() < MAX_BOOKMARKS && !url.is_empty() {
            self.list.push(Bookmark { url: url.to_string(), title: title.to_string() });
        }
        self.contains(url)
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|b| b.url != url);
        self.list.len() != before
    }

    /// The bookmarks whose address or title contains `typed`.
    pub fn suggest(&self, typed: &str, limit: usize) -> Vec<&Bookmark> {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        self.list
            .iter()
            .filter(|b| b.url.to_lowercase().contains(&typed) || b.title.to_lowercase().contains(&typed))
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeping_and_forgetting() {
        let mut b = Bookmarks::default();
        assert!(b.toggle("https://a/", "A\ttab"));
        assert!(b.toggle("https://b/", "B"));
        assert!(!b.toggle("https://a/", "A"));
        assert!(!b.contains("https://a/") && b.contains("https://b/"));
        b.toggle("https://a/", "A\ttab");
        let again = Bookmarks::parse(&b.serialize());
        assert_eq!(again, b);
        assert_eq!(again.list()[1].title, "A\ttab");
        assert_eq!(b.suggest("b/", 5).len(), 1);
        assert_eq!(b.suggest("tab", 5).len(), 1, "titles too");
        // Twice in a file, once kept.
        assert_eq!(Bookmarks::parse("https://a/\tA\nhttps://a/\tA2\n").list().len(), 1);
    }
}
