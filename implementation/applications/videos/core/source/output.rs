// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! An output stream of the audio service that outlives the service.
//!
//! The audio service is started again by `init` when it fails, and a stream
//! of the one that went is a ring nobody reads any more. [`Output`] notices
//! (a call that fails, its connection closing), and once the service is
//! back opens a new stream with what the player asked of the old one: its
//! rate, volume and whether it is paused. [`Output::keep`] says when a new
//! stream starts, whose positions count from 0 again: the player starts
//! what it writes over from there.
//!
//! Without the service, it does not wait: it asks the registry whether the
//! service is there (one quick call), and tries again later, less often
//! the longer it is away ([`Retry`]).
//!
//! Videos and Music both play through it. It belongs beside
//! `proto::audio::OutputStream`, where every player could use it.

use alloc::string::String;
use alloc::vec::Vec;

use abi::{WaitItem, signals};
use proto::audio::{OutputStream, StreamSpec, audio};

/// How long a call to the audio service may take.
const CALL_TIMEOUT_NS: u64 = 2_000_000_000;

/// When to try again to reach a service that is away: soon after it went,
/// then less and less often (doubling up to a limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    at: u64,
    wait: u64,
}

impl Retry {
    /// The first wait after a failed try (ns).
    pub const FIRST_NS: u64 = 250_000_000;
    /// The longest.
    pub const MAX_NS: u64 = 8_000_000_000;

    /// Due at once.
    pub fn new() -> Retry {
        Retry { at: 0, wait: Self::FIRST_NS }
    }

    /// Whether to try now.
    pub fn due(&self, now: u64) -> bool {
        now >= self.at
    }

    /// When the next try is due.
    pub fn at(&self) -> u64 {
        self.at
    }

    /// A try at `now` failed: the next waits longer.
    pub fn failed(&mut self, now: u64) {
        self.at = now.saturating_add(self.wait);
        self.wait = self.wait.saturating_mul(2).min(Self::MAX_NS);
    }

    /// A try succeeded (or the service went just now): the next one, when
    /// it is needed, is at once.
    pub fn reset(&mut self) {
        *self = Retry::new();
    }
}

impl Default for Retry {
    fn default() -> Retry {
        Retry::new()
    }
}

/// What [`Output::keep`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// The stream is there, as before.
    Same,
    /// A new stream was opened (positions count from 0 again).
    Opened,
    /// There is none (the service is away).
    None,
}

/// A stream of the audio service, opened again whenever the service comes
/// back.
pub struct Output {
    name: String,
    rate: u32,
    buffer_ms: u32,
    volume: f32,
    paused: bool,
    client: Option<audio::Client>,
    stream: Option<OutputStream>,
    /// The output device and its rate, as the service last said.
    device: String,
    device_rate: u32,
    retry: Retry,
    /// Streams opened so far, and lost with the service.
    opened: u32,
    losses: u32,
}

/// Whether service `name` is registered now (asking the registry, which
/// would otherwise queue a connection to a service not there yet).
pub fn registered(name: &str) -> bool {
    proto::with_registry(|r| r.list().map(|l| l.iter().any(|s| s == name)).unwrap_or(false)).unwrap_or(false)
}

impl Output {
    /// An output named `name` (for the service's diagnostics) of stereo
    /// sound at `rate` with about `buffer_ms` of buffering, at linear
    /// `volume`, starting `paused`. It opens its stream when first kept.
    pub fn new(name: &str, rate: u32, buffer_ms: u32, volume: f32, paused: bool) -> Output {
        Output {
            name: name.into(),
            rate,
            buffer_ms,
            volume,
            paused,
            client: None,
            stream: None,
            device: String::new(),
            device_rate: 0,
            retry: Retry::new(),
            opened: 0,
            losses: 0,
        }
    }

    /// Makes sure there is a stream, opening one if it is time to try.
    pub fn keep(&mut self, now: u64) -> Kept {
        if self.stream.is_some() {
            return Kept::Same;
        }
        if !self.retry.due(now) {
            return Kept::None;
        }
        if self.open() {
            self.retry.reset();
            self.opened = self.opened.wrapping_add(1);
            Kept::Opened
        } else {
            self.client = None;
            self.retry.failed(now);
            Kept::None
        }
    }

    fn open(&mut self) -> bool {
        if self.client.is_none() {
            if !registered(audio::NAME) {
                return false;
            }
            let Ok(ch) = proto::connect(audio::NAME) else { return false };
            let client = audio::Client::new(ch);
            client.set_timeout(CALL_TIMEOUT_NS);
            self.client = Some(client);
        }
        let Some(client) = &self.client else { return false };
        match client.status() {
            Ok(st) => {
                self.device = st.device;
                self.device_rate = st.rate;
            }
            Err(_) => return false,
        }
        let mut spec = StreamSpec::stereo(self.rate, self.buffer_ms, &self.name);
        spec.paused = self.paused;
        spec.volume = self.volume;
        match OutputStream::open(client, spec) {
            Ok(s) => {
                self.stream = Some(s);
                true
            }
            Err(e) => {
                runtime::println!("cannot open an audio stream ({e:?})");
                false
            }
        }
    }

    /// The stream went with its service: a new one is tried at once.
    pub fn lost(&mut self) {
        if self.stream.is_some() || self.client.is_some() {
            runtime::println!("the audio service went away");
            self.losses = self.losses.wrapping_add(1);
        }
        self.stream = None;
        self.client = None;
        self.retry.reset();
    }

    /// Notices a service that went away (its connection closed), after
    /// waiting on [`Output::wait_items`].
    pub fn observe(&mut self) {
        let closed = self.client.as_ref().is_some_and(|c| c.channel().wait(signals::PEER_CLOSED, 0).is_ok());
        if closed {
            self.lost();
        }
    }

    /// The stream, while there is one.
    pub fn stream(&self) -> Option<&OutputStream> {
        self.stream.as_ref()
    }

    /// The rate the stream plays at.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The output device's rate (0 until the service said): sound at it is
    /// not resampled.
    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// The output device's name (empty until the service said).
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Streams opened so far.
    pub fn opened(&self) -> u32 {
        self.opened
    }

    /// Streams lost with the service so far.
    pub fn losses(&self) -> u32 {
        self.losses
    }

    /// When to come back to open a stream, while there is none.
    pub fn retry_at(&self) -> Option<u64> {
        self.stream.is_none().then_some(self.retry.at())
    }

    /// The kernel objects to wait on: the stream's event (room in it), and
    /// the connection (the service going away).
    pub fn wait_items(&self, out: &mut Vec<WaitItem>) {
        if let Some(s) = &self.stream {
            out.push(WaitItem { handle: s.event().raw(), signals: signals::SIGNALED, ..Default::default() });
        }
        if let Some(c) = &self.client {
            out.push(WaitItem { handle: c.channel().raw(), signals: signals::PEER_CLOSED, ..Default::default() });
        }
    }

    /// Plays at `rate` from now on: a stream at another rate is closed, and
    /// one at the new rate opened when next kept.
    pub fn set_rate(&mut self, rate: u32) {
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        if let (Some(c), Some(s)) = (&self.client, self.stream.take()) {
            let _ = c.close(s.id);
        }
        self.retry.reset();
    }

    /// Pauses or resumes (a stream opened later starts so too).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.call(|c, id| c.set_paused(id, paused));
    }

    /// The stream's own volume, a linear gain.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        self.call(|c, id| c.set_volume(id, volume));
    }

    /// Drops what is queued; the stream's new position (frames written so
    /// far), unless there is no stream.
    pub fn flush(&mut self) -> Option<u64> {
        let at = self.call(|c, id| c.flush(id))?;
        if let Some(s) = &self.stream {
            s.ring().set_producer_flags(0);
        }
        Some(at)
    }

    /// Calls the service about the stream: a call that does not get through
    /// is a service gone.
    fn call<T>(
        &mut self,
        f: impl FnOnce(&audio::Client, u32) -> Result<Result<T, proto::audio::AudioError>, ipc::IpcError>,
    ) -> Option<T> {
        let (Some(c), Some(s)) = (&self.client, &self.stream) else { return None };
        match f(c, s.id) {
            Ok(r) => r.ok(),
            Err(_) => {
                self.lost();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn tries_come_less_often_the_longer_the_service_is_away() {
        let mut r = Retry::new();
        assert!(r.due(0), "the first try is at once");
        r.failed(10 * S);
        assert!(!r.due(10 * S) && r.due(10 * S + Retry::FIRST_NS));
        let mut at = 10 * S;
        let mut waits = Vec::new();
        for _ in 0..8 {
            r.failed(at);
            waits.push(r.at() - at);
            at = r.at();
        }
        assert_eq!(waits[0], 2 * Retry::FIRST_NS);
        assert!(waits.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(*waits.last().unwrap(), Retry::MAX_NS, "no longer than the limit");
        // The service went (again): at once.
        r.reset();
        assert!(r.due(at));
        r.failed(u64::MAX - 1);
        assert_eq!(r.at(), u64::MAX);
    }
}
