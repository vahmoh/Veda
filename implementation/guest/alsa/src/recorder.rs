// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The card's capture device, recording into the audio service's input
//! ring while the service wants audio (the ring's `CAPTURE` flag), on a
//! thread of its own: what it read, stamped with when it was recorded,
//! made stereo if the device records otherwise.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vabi::{Error, WaitItem, signals};
use vproto::audio::{Ring, ring::flags};
use vrt::object::Event;
use vrt::time::now_ns;

use crate::pcm::Pcm;
use crate::{CHANNELS, RATE};

/// The card's capture device, recording into the input ring.
pub struct Recorder {
    pub pcm: Pcm,
    running: bool,
    overruns: u32,
    /// What it read, in its channels; made stereo, if it records otherwise.
    scratch: Vec<i16>,
    stereo: Vec<i16>,
}

/// The input link: the ring the recorder fills, the event that tells the
/// service, and the one by which it says it wants audio.
pub struct Input {
    pub ring: Ring,
    pub data_event: Event,
    pub wake_event: Event,
}

impl Recorder {
    pub fn new(pcm: Pcm) -> Recorder {
        let samples = (pcm.period * pcm.channels) as usize;
        Recorder { pcm, running: false, overruns: 0, scratch: vec![0; samples], stereo: Vec::new() }
    }

    /// Records while the service wants audio.
    fn follow(&mut self, ring: &Ring) {
        let wanted = ring.consumer_flags() & flags::CAPTURE != 0;
        if wanted == self.running {
            return;
        }
        if wanted {
            match self.pcm.prepare().and_then(|_| self.pcm.start()) {
                Ok(()) => {
                    self.running = true;
                    println!("alsa: recording started");
                }
                Err(e) => println!("alsa: the card does not record: {e}"),
            }
        } else {
            self.stop();
            println!("alsa: recording stopped");
        }
    }

    fn stop(&mut self) {
        let _ = self.pcm.stop();
        self.running = false;
    }

    /// Takes what the card recorded into the ring. Returns true if frames
    /// were delivered.
    fn reap(&mut self, ring: &Ring) -> bool {
        let mut delivered = false;
        while self.running {
            let n = match self.pcm.read(&mut self.scratch) {
                Ok(0) => break,
                Ok(n) => n,
                // EPIPE: it ran over (this program was too slow), and
                // stopped: start again.
                Err(e) if e.raw_os_error() == Some(32) => {
                    self.overruns += 1;
                    if self.overruns <= 5 {
                        println!("alsa: recording overrun; restarting the input");
                    }
                    if let Err(e) = self.pcm.prepare().and_then(|_| self.pcm.start()) {
                        println!("alsa: the card does not record: {e}");
                        self.running = false;
                    }
                    break;
                }
                Err(e) => {
                    println!("alsa: the card's recording failed: {e}");
                    self.stop();
                    break;
                }
            };
            let read = &self.scratch[..n * self.pcm.channels as usize];
            let frames = if self.pcm.channels == CHANNELS {
                read
            } else {
                stereo(read, self.pcm.channels as usize, &mut self.stereo);
                &self.stereo
            };
            let written = ring.write(frames);
            if written < n {
                ring.set_overruns(ring.overruns().saturating_add((n - written) as u32));
            }
            // What was read is all the card had: its last frame is now's.
            ring.set_capture_clock(ring.write_pos(), now_ns());
            delivered = true;
        }
        delivered
    }

    /// Records until `stop` is set (and the wake event signaled).
    pub fn serve(&mut self, input: &Input, stop: &AtomicBool) {
        let period_ms = (self.pcm.period * 1000 / RATE) as i32;
        while !stop.load(Ordering::Acquire) {
            if self.running {
                // The card's period interrupt (or two periods' time).
                let _ = self.pcm.wait(2 * period_ms + 5);
            } else {
                let mut items =
                    [WaitItem { handle: input.wake_event.raw(), signals: signals::SIGNALED, ..Default::default() }];
                match vrt::object::wait_many(&mut items, now_ns() + 500_000_000) {
                    Ok(_) | Err(Error::TimedOut) => {}
                    // The wait was refused (the monitor at its limits):
                    // not at once again.
                    Err(_) => std::thread::sleep(Duration::from_millis(500)),
                }
                let _ = input.wake_event.clear();
            }
            self.follow(&input.ring);
            if self.reap(&input.ring) {
                let _ = input.data_event.signal();
            }
        }
        self.stop();
    }
}

/// Frames of `channels` interleaved samples made stereo, into `out`: one
/// channel on both sides; more, the mean of the even ones on the left and
/// of the odd ones on the right (an array's microphones in pairs, the left
/// one first, as digital microphones are wired).
fn stereo(samples: &[i16], channels: usize, out: &mut Vec<i16>) {
    out.clear();
    for frame in samples.chunks_exact(channels) {
        let mean = |side: usize| {
            let (sum, n) =
                frame.iter().skip(side).step_by(2).fold((0i32, 0i32), |(sum, n), &s| (sum + i32::from(s), n + 1));
            if n == 0 { i32::from(frame[0]) } else { sum / n }
        };
        out.extend([mean(0) as i16, mean(1) as i16]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_made_stereo() {
        let mut out = Vec::new();
        // Four microphones: the even ones' mean on the left, the odd ones'.
        stereo(&[100, -100, 300, -300, 0, 8, 4, 0], 4, &mut out);
        assert_eq!(out, [200, -200, 2, 4]);
        // One: on both sides.
        stereo(&[7, -9], 1, &mut out);
        assert_eq!(out, [7, 7, -9, -9]);
        // Three: the first and third left, the second right.
        stereo(&[i16::MAX, 5, i16::MAX], 3, &mut out);
        assert_eq!(out, [i16::MAX, 5]);
    }
}
