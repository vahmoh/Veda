// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the browser remembers of the user's choices. Its file has a line
//! a setting, `name=value`; what it does not know, or cannot read, is left
//! as it is by default.

use alloc::format;
use alloc::string::{String, ToString};

use crate::search;

/// What a new tab, and the home button, show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Home {
    /// The browser's own page: search, and the sites visited most.
    NewTab,
    Page(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The search engine's id ([`search::ENGINES`]).
    pub search: String,
    pub home: Home,
    /// The tabs open when the browser was closed open again as it starts.
    pub restore: bool,
    /// Cookies of sites other than the page's are refused.
    pub block_third_party_cookies: bool,
    /// Pages' zoom, in percent.
    pub zoom: u32,
    /// Pages are composited with OpenGL ES on the GPU, when there is one:
    /// smoother scrolling and animations, for less of the processor;
    /// otherwise the CPU paints each page as one picture. It takes effect at
    /// the next start.
    pub acceleration: bool,
}

/// The zooms pages can have, in percent.
pub const ZOOMS: [u32; 13] = [30, 50, 67, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

impl Default for Settings {
    fn default() -> Self {
        Settings {
            search: search::ENGINES[0].id.to_string(),
            home: Home::NewTab,
            restore: false,
            block_third_party_cookies: true,
            zoom: 100,
            acceleration: true,
        }
    }
}

impl Settings {
    pub fn parse(file: &str) -> Settings {
        let mut s = Settings::default();
        for line in file.lines() {
            let Some((name, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match name.trim() {
                "search" => s.search = search::engine(value).id.to_string(),
                "home" if value == "newtab" => s.home = Home::NewTab,
                "home" if !value.is_empty() => s.home = Home::Page(value.to_string()),
                "restore" => s.restore = value == "yes",
                "third-party-cookies" => s.block_third_party_cookies = value != "allow",
                "zoom" => s.zoom = value.parse().ok().filter(|z| ZOOMS.contains(z)).unwrap_or(100),
                "acceleration" => s.acceleration = value != "off",
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let home = match &self.home {
            Home::NewTab => "newtab",
            Home::Page(url) => url.as_str(),
        };
        format!(
            "search={}\nhome={}\nrestore={}\nthird-party-cookies={}\nzoom={}\nacceleration={}\n",
            self.search,
            home,
            if self.restore { "yes" } else { "no" },
            if self.block_third_party_cookies { "block" } else { "allow" },
            self.zoom,
            if self.acceleration { "on" } else { "off" }
        )
    }

    pub fn engine(&self) -> &'static search::Engine {
        search::engine(&self.search)
    }
}

/// The zoom after `zoom` (percent), up or down a step.
pub fn zoom_step(zoom: u32, up: bool) -> u32 {
    let i = ZOOMS.iter().position(|&z| z >= zoom).unwrap_or(ZOOMS.len() - 1);
    if up { ZOOMS[(if ZOOMS[i] > zoom { i } else { i + 1 }).min(ZOOMS.len() - 1)] } else { ZOOMS[i.saturating_sub(1)] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let mut s = Settings::default();
        assert_eq!(Settings::parse(&s.serialize()), s);
        s.search = "google".into();
        s.home = Home::Page("https://example.com/".into());
        s.restore = true;
        s.block_third_party_cookies = false;
        s.zoom = 125;
        s.acceleration = false;
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn what_cannot_be_read_stays_as_it_is() {
        let s = Settings::parse("search=nowhere\nzoom=1000\nnonsense\nhome=\nrestore=maybe\n");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(zoom_step(100, true), 110);
        assert_eq!(zoom_step(100, false), 90);
        assert_eq!(zoom_step(300, true), 300);
        assert_eq!(zoom_step(30, false), 30);
        assert_eq!(zoom_step(105, true), 110);
        assert_eq!(zoom_step(105, false), 100);
    }
}
