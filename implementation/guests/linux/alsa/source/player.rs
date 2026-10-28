// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The card, playing the audio service's mixed output from the shared
//! ring: a period at a time, a few periods ahead of what is heard, padded
//! with silence when it runs short while streams play (an underrun, which
//! deepens the queue: [`Depth`]); how much has played, from the card's own
//! count; the card's period interrupts pacing the program while it plays.

use std::collections::VecDeque;
use std::io;

use proto::audio::{Ring, ring::flags};
use runtime::time::now_ns;

use crate::pcm::{self, Pcm};
use crate::{CHANNELS, RATE};

/// Periods kept queued normally (60 ms), and at most after underruns.
const DEPTH: u32 = 6;
pub const MAX_DEPTH: u32 = 12;
/// After this long without an underrun the queue gets a period shorter,
/// back to [`DEPTH`] in the end.
const SHALLOWER_NS: u64 = 30_000_000_000;
/// Below this many queued periods the driver pads with silence.
const MIN_QUEUED: u32 = 2;
/// Periods of audio needed before (re)starting playback.
const PREFILL: u32 = 2;
/// Stop the card after this long without audio.
const IDLE_STOP_NS: u64 = 1_500_000_000;

/// How many periods the card is kept ahead: [`DEPTH`], a period more for
/// each underrun (up to [`MAX_DEPTH`]), and a period less again for each
/// [`SHALLOWER_NS`] without one.
pub struct Depth {
    pub periods: u32,
    /// When it last changed.
    since_ns: u64,
}

impl Depth {
    fn new() -> Depth {
        Depth { periods: DEPTH, since_ns: 0 }
    }

    /// An underrun at `now`: a period deeper.
    fn deepen(&mut self, now: u64) {
        self.periods = (self.periods + 1).min(MAX_DEPTH);
        self.since_ns = now;
    }

    /// A period shallower, if it has been deeper than [`DEPTH`] for
    /// [`SHALLOWER_NS`] without an underrun by `now`; true if it got so.
    fn recover(&mut self, now: u64) -> bool {
        if self.periods > DEPTH && now.saturating_sub(self.since_ns) >= SHALLOWER_NS {
            self.periods -= 1;
            self.since_ns = now;
            return true;
        }
        false
    }
}

/// The card, playing the ring.
pub struct Player {
    pub pcm: Pcm,
    pub running: bool,
    /// Periods kept queued.
    pub depth: Depth,
    /// Frames written to the card since it started, and the chunks written
    /// (the card's position at a chunk's end, and how many of its frames
    /// came from the ring: the rest is silence).
    written: u64,
    chunks: VecDeque<(u64, u32)>,
    /// Frames written that are not heard yet, and those of them still in
    /// the card's buffer (the rest is the card's own delay).
    queued: u64,
    filled: u64,
    /// Ring frames heard, and when that count last grew.
    pub played: u64,
    pub played_ns: u64,
    underruns: u32,
    starving: bool,
    last_audio_ns: u64,
    scratch: Vec<i16>,
}

impl Player {
    pub fn new(pcm: Pcm) -> Player {
        let samples = (pcm.period * CHANNELS) as usize;
        Player {
            pcm,
            running: false,
            depth: Depth::new(),
            written: 0,
            chunks: VecDeque::new(),
            queued: 0,
            filled: 0,
            played: 0,
            played_ns: 0,
            underruns: 0,
            starving: false,
            last_audio_ns: 0,
            scratch: vec![0; samples],
        }
    }

    pub fn period(&self) -> u32 {
        self.pcm.period
    }

    /// Periods in the card's buffer.
    fn filled_periods(&self) -> u32 {
        self.filled.div_ceil(self.period() as u64) as u32
    }

    /// The periods the card is to hold until [`pump`](Self::pump) tops it
    /// up again: the queue's depth while the ring has audio for it; else
    /// the silence it is padded with ([`MIN_QUEUED`]), and none when it is
    /// left to run out before it stops.
    pub fn target(&self, ring: &Ring) -> u32 {
        if ring.filled() >= self.period() {
            self.depth.periods
        } else if self.idle(ring) {
            0
        } else {
            MIN_QUEUED
        }
    }

    /// Has the card's period interrupts wake [`serve`] once its buffer
    /// holds a period less than `target` (or nothing, for none). ALSA
    /// wakes a writer as soon as a period of the buffer is free, which,
    /// with the queue shorter than the buffer, it always is.
    pub fn pace(&self, target: u32) -> io::Result<()> {
        let period = self.period();
        self.pcm.wake_at(self.pcm.buffer.saturating_sub(target.saturating_sub(1) * period).max(period))
    }

    /// Nothing more to play: no stream is active, the ring is empty, and
    /// the last audio is a while ago.
    fn idle(&self, ring: &Ring) -> bool {
        ring.producer_flags() & flags::ACTIVE == 0
            && ring.filled() == 0
            && now_ns().saturating_sub(self.last_audio_ns) > IDLE_STOP_NS
    }

    /// Counts the chunks the card has played into the ring's played
    /// position. Returns true if any did.
    pub fn reap(&mut self) -> bool {
        if !self.running {
            return false;
        }
        // Ran dry (or cannot tell): all of it was heard.
        let status = self.pcm.status().unwrap_or(pcm::Status { free: self.pcm.buffer as u64, delay: 0 });
        let delay = status.delay.min(self.written);
        let heard = self.written - delay;
        self.queued = delay;
        self.filled = (self.pcm.buffer as u64).saturating_sub(status.free).min(delay);
        let now = now_ns();
        let mut any = false;
        while let Some(&(end, frames)) = self.chunks.front() {
            if end > heard {
                break;
            }
            self.chunks.pop_front();
            if frames > 0 {
                self.played += frames as u64;
                // That many frames ago, at the card's rate.
                self.played_ns = now.saturating_sub((heard - end) * 1_000_000_000 / RATE as u64);
            }
            any = true;
        }
        any
    }

    /// Writes a period: up to `frames` frames of the ring, then silence.
    /// Returns the ring frames the card took (`None`: it took nothing, it
    /// is full).
    fn queue(&mut self, ring: &Ring, frames: u32) -> io::Result<Option<u32>> {
        let (period, ch) = (self.period() as usize, CHANNELS as usize);
        let n = ring.peek(&mut self.scratch[..(frames as usize).min(period) * ch]);
        self.scratch[n * ch..].fill(0);
        let taken = self.pcm.write(&self.scratch)?;
        let from_ring = n.min(taken);
        ring.consume(from_ring);
        if taken > 0 {
            self.written += taken as u64;
            self.queued += taken as u64;
            self.filled += taken as u64;
            self.chunks.push_back((self.written, from_ring as u32));
        }
        if from_ring > 0 {
            self.last_audio_ns = now_ns();
        }
        Ok((taken > 0).then_some(from_ring as u32))
    }

    /// Starts the card on what the ring holds.
    fn start(&mut self, ring: &Ring) -> io::Result<()> {
        self.pcm.prepare()?;
        (self.written, self.queued, self.filled) = (0, 0, 0);
        self.chunks.clear();
        // The card starts with the first period written.
        while self.filled_periods() < self.depth.periods && ring.filled() > 0 {
            let n = ring.filled().min(self.period());
            if self.queue(ring, n)?.is_none() {
                break;
            }
        }
        self.running = true;
        self.starving = false;
        self.last_audio_ns = now_ns();
        Ok(())
    }

    /// An underrun: audio that should have been playing was not there in
    /// time. The queue gets a period longer.
    fn underrun(&mut self) {
        self.underruns += 1;
        self.depth.deepen(now_ns());
    }

    /// The card ran dry and stopped: it was not refilled in time. That is
    /// an underrun if audio was waiting, in the ring or on its way (streams
    /// play); with nothing but the silence it was padded with, the card
    /// stopped as it would have.
    fn ran_dry(&mut self, ring: &Ring) {
        if ring.filled() > 0 || ring.producer_flags() & flags::ACTIVE != 0 {
            self.underrun();
            if self.underruns <= 10 || self.underruns.is_multiple_of(100) {
                println!(
                    "alsa: the card ran dry: underrun {} (queue depth now {})",
                    self.underruns, self.depth.periods
                );
            }
        }
        self.stop();
    }

    pub fn stop(&mut self) {
        let _ = self.pcm.stop();
        self.running = false;
        (self.queued, self.filled) = (0, 0);
        self.chunks.clear();
    }

    /// Starts, refills or stops the card. Returns true if ring data was
    /// consumed or the state changed.
    pub fn pump(&mut self, ring: &Ring) -> bool {
        if self.depth.recover(now_ns()) {
            let secs = SHALLOWER_NS / 1_000_000_000;
            println!("alsa: no underrun for {secs} s (queue depth now {})", self.depth.periods);
        }
        let active = ring.producer_flags() & flags::ACTIVE != 0;
        if !self.running {
            let filled = ring.filled();
            if filled == 0 || (active && filled < self.period() * PREFILL) {
                return false;
            }
            if let Err(e) = self.start(ring) {
                println!("alsa: the card does not start: {e}");
                self.stop();
            }
            return true;
        }
        if self.idle(ring) {
            // Nothing more: what is queued plays, then the card stops.
            if self.queued == 0 {
                self.stop();
                return true;
            }
            return false;
        }
        let mut changed = false;
        while self.filled_periods() < self.depth.periods {
            let filled = ring.filled();
            let result = if filled >= self.period() {
                self.starving = false;
                self.queue(ring, self.period())
            } else if self.filled_periods() >= MIN_QUEUED {
                break;
            } else {
                // About to run dry: queue what there is, and silence.
                if active && !self.starving {
                    self.starving = true;
                    self.underrun();
                    if self.underruns <= 10 || self.underruns.is_multiple_of(100) {
                        println!("alsa: underrun {} (queue depth now {})", self.underruns, self.depth.periods);
                    }
                }
                self.queue(ring, filled)
            };
            match result {
                Ok(Some(n)) => changed |= n > 0,
                Ok(None) => break,
                // EPIPE: it ran dry before this was written.
                Err(e) if e.raw_os_error() == Some(32) => {
                    self.ran_dry(ring);
                    return true;
                }
                Err(e) => {
                    println!("alsa: the card refused audio: {e}");
                    self.stop();
                    return true;
                }
            }
        }
        changed
    }

    pub fn publish(&self, ring: &Ring) {
        ring.set_played(self.played, self.played_ns);
        ring.set_latency(self.queued.min(u32::MAX as u64) as u32);
        ring.set_underruns(self.underruns);
        ring.set_consumer_flags(if self.running { flags::RUNNING } else { 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_deepens_with_underruns_and_recovers_without() {
        let mut depth = Depth::new();
        assert!(!depth.recover(10 * SHALLOWER_NS));
        // Underruns at 1 s, 2 s, ...: a period each, up to the most.
        for s in 1..=10 {
            depth.deepen(s * 1_000_000_000);
        }
        assert_eq!(depth.periods, MAX_DEPTH);
        // Half a minute after the last one, a period less, and then a
        // period less for each half minute more, down to the usual depth.
        let last = 10 * 1_000_000_000;
        assert!(!depth.recover(last + SHALLOWER_NS - 1));
        assert!(depth.recover(last + SHALLOWER_NS));
        assert!(!depth.recover(last + SHALLOWER_NS + 1));
        let mut now = last + SHALLOWER_NS;
        while depth.recover(now + SHALLOWER_NS) {
            now += SHALLOWER_NS;
        }
        assert_eq!(depth.periods, DEPTH);
        assert_eq!(now, last + (MAX_DEPTH - DEPTH) as u64 * SHALLOWER_NS);
        // An underrun on the way back starts the wait again.
        depth.deepen(now);
        assert!(!depth.recover(now + SHALLOWER_NS / 2));
        assert_eq!(depth.periods, DEPTH + 1);
    }
}
