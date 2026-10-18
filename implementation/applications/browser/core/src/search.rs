// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The search engines the address bar searches with.

use alloc::string::String;

use crate::url::encode_query;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    /// What settings call it.
    pub id: &'static str,
    pub name: &'static str,
    /// The results' address, `{}` where the words go.
    template: &'static str,
}

impl Engine {
    /// The address of the results for `words`.
    pub fn results(&self, words: &str) -> String {
        self.template.replacen("{}", &encode_query(words.trim()), 1)
    }
}

/// The engines, the default first: one that keeps no record of searches.
pub const ENGINES: [Engine; 7] = [
    Engine { id: "duckduckgo", name: "DuckDuckGo", template: "https://duckduckgo.com/?q={}" },
    Engine { id: "startpage", name: "Startpage", template: "https://www.startpage.com/sp/search?query={}" },
    Engine { id: "brave", name: "Brave Search", template: "https://search.brave.com/search?q={}" },
    Engine { id: "google", name: "Google", template: "https://www.google.com/search?q={}" },
    Engine { id: "bing", name: "Bing", template: "https://www.bing.com/search?q={}" },
    Engine { id: "ecosia", name: "Ecosia", template: "https://www.ecosia.org/search?q={}" },
    Engine { id: "wikipedia", name: "Wikipedia", template: "https://en.wikipedia.org/w/index.php?search={}" },
];

/// The engine `id` names, or the default.
pub fn engine(id: &str) -> &'static Engine {
    ENGINES.iter().find(|e| e.id == id).unwrap_or(&ENGINES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_of_words() {
        assert_eq!(engine("duckduckgo").results(" veda os "), "https://duckduckgo.com/?q=veda+os");
        assert_eq!(engine("nonsense").id, "duckduckgo");
        assert_eq!(engine("wikipedia").results("C++"), "https://en.wikipedia.org/w/index.php?search=C%2B%2B");
        // Ids are unique.
        for (i, e) in ENGINES.iter().enumerate() {
            assert!(ENGINES[i + 1..].iter().all(|o| o.id != e.id));
            assert!(e.template.starts_with("https://") && e.template.contains("{}"));
        }
    }
}
