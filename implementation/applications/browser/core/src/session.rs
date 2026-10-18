// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The tabs open as the browser closed, to open again as it starts (no
//! private one: those leave nothing behind). Its file has a line a tab, its
//! address and title, and first which of them was shown.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::text;

/// The most tabs a session keeps.
pub const MAX_TABS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub shown: usize,
}

impl Session {
    pub fn parse(file: &str) -> Session {
        let mut lines = text::records(file);
        let shown = lines.next().and_then(|f| f.first()?.parse().ok()).unwrap_or(0);
        let tabs: Vec<SavedTab> = lines
            .filter_map(|f| match f.as_slice() {
                [url, title] if !url.is_empty() => Some(SavedTab { url: url.clone(), title: title.clone() }),
                _ => None,
            })
            .take(MAX_TABS)
            .collect();
        let shown = if shown < tabs.len() { shown } else { 0 };
        Session { tabs, shown }
    }

    pub fn serialize(&self) -> String {
        let mut out = self.shown.to_string();
        out.push('\n');
        for t in self.tabs.iter().take(MAX_TABS) {
            out.push_str(&text::line(&[&t.url, &t.title]));
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip() {
        let s = Session {
            tabs: alloc::vec![
                SavedTab { url: "https://a/".into(), title: "A".into() },
                SavedTab { url: "https://b/".into(), title: "B\nline".into() },
            ],
            shown: 1,
        };
        assert_eq!(Session::parse(&s.serialize()), s);
        assert_eq!(Session::parse("7\nhttps://a/\tA\n").shown, 0, "no such tab");
        assert_eq!(Session::parse(""), Session::default());
    }
}
