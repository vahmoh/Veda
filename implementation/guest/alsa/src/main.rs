// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `alsa` — Veda's sound driver for Linux: the guest's sound card (the PC's
//! own: its first playback device that is not a display's, and its card's
//! microphones, through ALSA's kernel interface: `cards`) as Veda's output
//! and input devices.
//!
//! It attaches to Veda's audio service (`audiodev`) over the bridge, as
//! Veda's own sound drivers do, and plays the service's mixed output from
//! the shared ring (`player`): a period (10 ms) at a time, a few periods
//! ahead of what is heard. Running short while streams play, it pads with
//! silence (an underrun, which deepens the queue, until half a minute
//! without one makes it a period shallower again); a stream that ends, and
//! says so, is no underrun, nor is the card running out of padding with
//! nothing to play after it. How much has played comes from the card's own
//! count (ALSA's delay), stamped with Veda's clock, which the guest reads
//! itself. The card's period interrupts pace this program while it plays:
//! it wakes when the card's buffer holds a period less than the queue's
//! depth, and tops it up; after a while without audio it stops, and waits
//! for the service to have some. The card's own mixer stays at unity gain
//! (`ctl`): the volume is Veda's.
//!
//! The card records (`recorder`), on a thread of its own, while the audio
//! service wants audio (the input ring's `CAPTURE` flag): what it read goes
//! into the ring, with the moment it was recorded, and the service hears
//! of it. Its microphones are the built-in ones: an audio DSP's digital
//! microphones (SOF's `DMIC` device), if the card has them, beside its
//! codec's input; else its first capture device. A device that records
//! other than stereo (an array of four microphones) is made stereo.
//!
//! The card is attached again whenever the service comes back, and after a
//! failure, later after each in a row. A card that cannot play waits for
//! Linux's cards to change (one that comes later, a USB card), rather than
//! the program ending to be started again.

mod cards;
mod ctl;
mod pcm;
mod player;
mod recorder;

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use guest_sys::retry::{self, Ended};
use rustix::event::{PollFd, PollFlags, poll};
use vabi::signals;
use vproto::audio::{DeviceFormat, Ring, Role, audiodev};
use vrt::guest::Watch;
use vrt::object::{Channel, Event};

use cards::Card;
use pcm::{Pcm, Stream};
use player::{MAX_DEPTH, Player};
use recorder::{Input, Recorder};

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
/// The most channels a capture device may record.
const MAX_CAPTURE_CHANNELS: u32 = 8;
/// A period: 10 ms.
const PERIOD: u32 = RATE / 100;
/// The card's buffer, in periods.
const PERIODS: u32 = 16;
/// How often Linux's cards are looked at, while the card cannot play.
const CARDS_CHECK: Duration = Duration::from_secs(2);

/// The output link: the ring the player plays, and its events.
struct Output {
    ring: Ring,
    data_event: Event,
    space_event: Event,
}

/// Connects to the audio service and attaches the card, its periods
/// `period` frames long (and its input, with periods of `capture` frames).
fn attach(name: &str, period: u32, capture: Option<u32>) -> Result<(Channel, Output, Option<Input>), String> {
    let ch = vproto::connect(audiodev::NAME).map_err(|e| format!("no audio service: {e:?}"))?;
    let client = audiodev::Client::new(ch);
    let format = |period_frames| DeviceFormat {
        name: format!("{name} (Linux)"),
        rate: RATE,
        channels: CHANNELS,
        period_frames,
        max_periods: MAX_DEPTH,
    };
    let link = client
        .attach(format(period))
        .map_err(|e| format!("the audio service went away: {e}"))?
        .map_err(|e| format!("the audio service refused the card: {e}"))?;
    let ring = Ring::map(link.ring, Role::Consumer).map_err(|e| format!("bad ring: {e:?}"))?;
    if ring.rate() != RATE || ring.channels() != CHANNELS {
        return Err("the ring's format is not the card's".into());
    }
    let output = Output { ring, data_event: link.data_event, space_event: link.space_event };
    let input = capture.and_then(|period| match client.attach_input(format(period)) {
        Ok(Ok(l)) => match Ring::map(l.ring, Role::Producer) {
            Ok(ring) => Some(Input { ring, data_event: l.data_event, wake_event: l.wake_event }),
            Err(e) => {
                println!("alsa: bad input ring: {e:?}");
                None
            }
        },
        Ok(Err(e)) => {
            println!("alsa: the audio service refused the input: {e}");
            None
        }
        Err(_) => None,
    });
    Ok((client.into_channel(), output, input))
}

/// Plays until the audio service goes away. While the card plays, its
/// period interrupts wake this program when it needs more (as
/// [`Player::pace`] has them); until the card has all it is to hold of the
/// ring's audio, so does more audio in the ring; and so does the link
/// closing. An error ends the session (the card refused audio, a watch
/// failed).
fn serve(player: &mut Player, link: &Channel, out: &Output) -> Result<Ended, String> {
    let (ring, data_event, space_event) = (&out.ring, &out.data_event, &out.space_event);
    let watch = |handle, signals| Watch::new(handle, signals).map_err(|e| format!("cannot watch the link: {e}"));
    let data = watch(data_event.raw(), signals::SIGNALED)?;
    let closed = watch(link.raw(), signals::PEER_CLOSED)?;
    let failed = |e| format!("a watch failed ({e}); attaching again");
    let period_ms = (player.period() * 1000 / RATE) as i32;
    // The periods the card's wake-ups are set for.
    let mut paced = None;
    loop {
        let target = player.running.then(|| player.target(ring));
        if let Some(t) = target
            && paced != target
        {
            player.pace(t).map_err(|e| format!("cannot play: {e}"))?;
            paced = target;
        }
        let audio = target.is_none_or(|t| t < player.depth.periods);
        // The link closing; audio coming, while there is room for it; room
        // on the card, while it plays.
        let mut fds = vec![PollFd::new(&closed, PollFlags::IN)];
        let data_at = audio.then(|| {
            fds.push(PollFd::new(&data, PollFlags::IN));
            fds.len() - 1
        });
        if player.running {
            fds.push(PollFd::new(player.pcm.file(), PollFlags::OUT));
        }
        // Two periods' time at most while the card plays, in case.
        let timeout = guest_sys::poll_timeout(if player.running { 2 * period_ms + 5 } else { 500 });
        if let Err(e) = poll(&mut fds, timeout.as_ref())
            && e != rustix::io::Errno::INTR
        {
            return Err(format!("cannot wait: {e}"));
        }
        let closing = !fds[0].revents().is_empty();
        let data_came = data_at.is_some_and(|i| !fds[i].revents().is_empty());
        drop(fds);
        if closed.take(closing).map_err(failed)?.is_some() {
            println!("alsa: the audio service went away");
            return Ok(Ended::Left);
        }
        if data.take(data_came).map_err(failed)?.is_some() {
            let _ = data_event.clear();
        }
        let mut changed = player.reap();
        changed |= player.pump(ring);
        player.publish(ring);
        if changed {
            let _ = space_event.signal();
        }
    }
}

/// Plays `card` (and records it) for the audio service, as long as it can:
/// why it cannot.
fn play(card: &Card) -> String {
    let name = &card.name;
    match ctl::open_outputs(&card.number) {
        Ok(set) => println!("alsa: outputs on, at unity gain: {}", set.join(", ")),
        Err(e) => println!("alsa: the mixer of card {}: {e}", card.number),
    }
    let pcm = match Pcm::open(&card.playback, Stream::Playback, RATE, CHANNELS..=CHANNELS, PERIOD, PERIODS) {
        Ok(p) => p,
        Err(e) => return format!("{} cannot play {RATE} Hz, 16-bit stereo: {e}", card.playback),
    };
    println!(
        "alsa: {name} ({}): {} Hz, periods of {} frames, a buffer of {}",
        card.playback, pcm.rate, pcm.period, pcm.buffer
    );
    match ctl::open_inputs(&card.number) {
        Ok(set) if !set.is_empty() => println!("alsa: inputs on, at unity gain: {}", set.join(", ")),
        Ok(_) => {}
        Err(e) => println!("alsa: the mixer of card {}: {e}", card.number),
    }
    let recorder = card.capture.as_ref().and_then(|path| {
        // Stereo, or as few channels as the device records.
        let open = |channels| Pcm::open(path, Stream::Capture, RATE, channels, PERIOD, PERIODS);
        match open(CHANNELS..=CHANNELS).or_else(|_| open(1..=MAX_CAPTURE_CHANNELS)) {
            Ok(p) => {
                println!(
                    "alsa: {name} records ({path}, {} channels): periods of {} frames, a buffer of {}",
                    p.channels, p.period, p.buffer
                );
                Some(Recorder::new(p))
            }
            Err(e) => {
                println!("alsa: {path} cannot record {RATE} Hz, 16-bit: {e}");
                None
            }
        }
    });
    let mut player = Player::new(pcm);
    let period = player.period();
    // The recorder, while no session's thread has it.
    let recorder = RefCell::new(recorder);
    let attached = || {
        let capture = recorder.borrow().as_ref().map(|r| r.pcm.period);
        let session = attach(name, period, capture).map_err(|e| format!("cannot attach: {e}"))?;
        println!("alsa: attached to the audio service{}", if session.2.is_some() { " (with recording)" } else { "" });
        Ok(session)
    };
    retry::sessions("alsa", attached, |(link, out, input): (Channel, Output, Option<Input>)| {
        player.played = out.ring.read_pos();
        player.played_ns = 0;
        // The recorder records on a thread of its own while the link lasts.
        let stop = Arc::new(AtomicBool::new(false));
        let taken = recorder.borrow_mut().take();
        let recording = match (input, taken) {
            (Some(input), Some(mut rec)) => {
                let stop = stop.clone();
                let wake = input.wake_event.0.duplicate(None).ok().map(Event::from_handle);
                let thread = std::thread::spawn(move || {
                    rec.serve(&input, &stop);
                    rec
                });
                Some((thread, wake))
            }
            (_, rec) => {
                *recorder.borrow_mut() = rec;
                None
            }
        };
        let served = serve(&mut player, &link, &out);
        player.stop();
        if let Some((thread, wake)) = recording {
            stop.store(true, Ordering::Release);
            if let Some(w) = wake {
                let _ = w.signal();
            }
            *recorder.borrow_mut() = thread.join().ok();
        }
        served
    })
}

fn main() {
    loop {
        let card = cards::find_card();
        let listed = cards::listed();
        let why = play(&card);
        println!("alsa: {why}; waiting for Linux's sound cards to change");
        while cards::listed() == listed {
            std::thread::sleep(CARDS_CHECK);
        }
    }
}
