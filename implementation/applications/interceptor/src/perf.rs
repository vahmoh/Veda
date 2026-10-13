// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! How the game runs, frame by frame: the frame rate, the frames' lengths
//! and where their time goes, and what renders. The readout over the game
//! shows it ([`crate::hud::performance`]); the log has the longest frame
//! every few seconds, and what it went to.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;

use interceptor_game::render::{Quality, device_name};

/// Where a frame's time went (ms): moving the game on (with the events,
/// the voice agent and the sounds), rendering (handing the GPU the
/// frame's commands), waiting for the window to take a frame, showing
/// (the picture read back into the window), and drawing the dashboard and
/// the menus over it and handing the frame over.
#[derive(Clone, Copy, Debug, Default)]
pub struct Phases {
    pub update: f32,
    pub render: f32,
    pub wait: f32,
    pub show: f32,
    pub draw: f32,
}

/// Frames longer than this are slow (ms): long enough for a stutter.
const SLOW_MS: f32 = 50.0;
/// Frames whose lengths the graph shows.
pub const HISTORY: usize = 150;

pub struct Perf {
    /// The renderer's name (`GL_RENDERER`), what renders in words, and
    /// whether that is the processor (no GPU).
    pub renderer: String,
    pub device: String,
    pub cpu: bool,
    /// Frames a second over the last second, and their mean length (ms).
    pub fps: f32,
    pub frame_ms: f32,
    /// How long rendering and showing take (ms, moving means).
    pub render_ms: f32,
    pub show_ms: f32,
    /// The last frames' lengths (ms), oldest first.
    pub history: VecDeque<f32>,
    /// Frames since `since` (ns).
    frames: u32,
    since: u64,
    /// The longest frame since the last report and what its time went to,
    /// and how many were slow.
    longest: (f32, Phases),
    slow: u32,
}

impl Perf {
    pub fn new(renderer: String, now: u64) -> Perf {
        Perf {
            device: device_name(&renderer),
            cpu: Quality::for_renderer(&renderer) == Quality::Low,
            renderer,
            fps: 0.0,
            frame_ms: 0.0,
            render_ms: 0.0,
            show_ms: 0.0,
            history: VecDeque::with_capacity(HISTORY),
            frames: 0,
            since: now,
            longest: (0.0, Phases::default()),
            slow: 0,
        }
    }

    /// A frame ended at `now` (ns), `ms` after the one before, its time
    /// having gone to `spent`. True when a second has passed, and the frame
    /// rate is new.
    pub fn frame(&mut self, now: u64, ms: f32, spent: Phases) -> bool {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(ms);
        self.render_ms = self.render_ms * 0.9 + spent.render * 0.1;
        self.show_ms = self.show_ms * 0.9 + spent.show * 0.1;
        if ms > self.longest.0 {
            self.longest = (ms, spent);
        }
        self.slow += u32::from(ms > SLOW_MS);
        self.frames += 1;
        let secs = now.saturating_sub(self.since) as f32 / 1e9;
        if secs < 1.0 {
            return false;
        }
        self.fps = self.frames as f32 / secs;
        self.frame_ms = secs * 1000.0 / self.frames as f32;
        (self.frames, self.since) = (0, now);
        true
    }

    /// The longest frame since the last report and what it went to, and
    /// the slow frames, for the log; counting starts again.
    pub fn report(&mut self) -> String {
        let (ms, p) = core::mem::take(&mut self.longest);
        let slow = match core::mem::take(&mut self.slow) {
            0 => String::from("none"),
            n => format!("{n}"),
        };
        let went = format!(
            "update {:.0}, render {:.0}, wait {:.0}, show {:.0}, draw {:.0}",
            p.update, p.render, p.wait, p.show, p.draw
        );
        format!("longest frame {ms:.0} ms ({went}), {slow} over {SLOW_MS:.0} ms")
    }

    /// The longest of the last frames (ms).
    pub fn longest_shown(&self) -> f32 {
        self.history.iter().fold(0.0, |m, &x| m.max(x))
    }
}
