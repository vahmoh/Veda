// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Trying again what may fail for a while: waiting longer after each
//! failure in a row ([`Backoff`]), so that what cannot work for good is not
//! tried every second for ever, and what comes back is used again soon;
//! serving each of the devices a driver drives on a thread of its own while
//! it is there ([`Served`]); and serving one through sessions with a
//! service of Veda's, one after another ([`sessions`]).

use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Waits between attempts: from the first, twice as long after each
/// failure in a row, up to the most; from the first again after a success.
#[derive(Debug, Clone)]
pub struct Backoff {
    first: Duration,
    most: Duration,
    next: Duration,
}

impl Backoff {
    /// Waits from `first` to `most`.
    pub fn new(first: Duration, most: Duration) -> Backoff {
        Backoff { first, most, next: first }
    }

    /// The wait before the next attempt (the one after it is longer).
    pub fn delay(&mut self) -> Duration {
        let wait = self.next;
        self.next = self.next.saturating_mul(2).min(self.most);
        wait
    }

    /// Waits before the next attempt.
    pub fn wait(&mut self) {
        std::thread::sleep(self.delay());
    }

    /// The last attempt went well: the next wait is the first again.
    pub fn reset(&mut self) {
        self.next = self.first;
    }

    /// The longest wait.
    pub fn most(&self) -> Duration {
        self.most
    }
}

/// The devices a driver drives, each served on a thread of its own while
/// it is there, by a key that no device that comes later has (a network
/// interface's index, a radio's name). One whose thread ended while it is
/// still there is served again, later after each end in a row (from the
/// driver's period to a minute; anew after a thread that lasted a minute);
/// one that is gone is forgotten.
pub struct Served<K> {
    devices: BTreeMap<K, Device>,
    period: Duration,
}

/// A device served: the thread serving it, since when; or, once that
/// ended, when it is served again.
struct Device {
    thread: Option<(JoinHandle<()>, Instant)>,
    again: Instant,
    backoff: Backoff,
}

impl<K: Ord + Clone> Served<K> {
    /// Devices looked for every `period`.
    pub fn new(period: Duration) -> Served<K> {
        Served { devices: BTreeMap::new(), period }
    }

    /// The devices there are now, `present` (each by its key), at `now`:
    /// those not served are served by `serve`, each on a thread of its own,
    /// when they are due.
    pub fn update<T: Send + 'static>(&mut self, present: Vec<(K, T)>, now: Instant, serve: fn(T)) {
        self.devices.retain(|key, _| present.iter().any(|(k, _)| k == key));
        for (key, device) in present {
            let d = self.devices.entry(key).or_insert_with(|| Device {
                thread: None,
                again: now,
                backoff: Backoff::new(self.period, Duration::from_secs(60)),
            });
            if let Some((thread, since)) = &d.thread {
                if !thread.is_finished() {
                    continue;
                }
                if now.saturating_duration_since(*since) >= d.backoff.most() {
                    d.backoff.reset();
                }
                d.again = now + d.backoff.delay();
                d.thread = None;
            }
            if now >= d.again {
                d.thread = Some((std::thread::spawn(move || serve(device)), now));
            }
        }
    }

    /// Whether a thread serves the device of `key`.
    pub fn serving(&self, key: &K) -> bool {
        self.devices.get(key).is_some_and(|d| d.thread.as_ref().is_some_and(|(t, _)| !t.is_finished()))
    }
}

/// How a session a driver served ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The service went away (or let go of what was attached): attach
    /// again.
    Left,
    /// What the driver serves is gone (an interface unplugged): no more
    /// sessions, for this reason.
    Gone(String),
}

/// Serves what a driver drives until it is gone: attaches it to a service
/// of Veda's (`attach`), serves the session (`serve`) until it ends, and
/// again. After a failure (to attach, or of a session) it waits, longer
/// after each in a row ([`Backoff`]: a second, up to half a minute); a
/// session that lasted that long, or that the service ended, starts the
/// waits anew. `what` names it in the log, where failures are said; the
/// closures say the rest. Returns why it is gone.
pub fn sessions<S>(
    what: &str,
    mut attach: impl FnMut() -> Result<S, String>,
    mut serve: impl FnMut(S) -> Result<Ended, String>,
) -> String {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    loop {
        let session = match attach() {
            Ok(session) => session,
            Err(e) => {
                println!("{what}: {e}");
                backoff.wait();
                continue;
            }
        };
        let started = Instant::now();
        match serve(session) {
            Ok(Ended::Left) => backoff.reset(),
            Ok(Ended::Gone(why)) => return why,
            Err(e) => {
                println!("{what}: {e}");
                if started.elapsed() >= backoff.most() {
                    backoff.reset();
                }
                backoff.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_grow_to_the_most_and_start_again_after_a_success() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        let waits: Vec<u64> = (0..5).map(|_| b.delay().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 5, 5]);
        b.reset();
        assert_eq!(b.delay(), Duration::from_secs(1));
    }

    #[test]
    fn a_device_is_served_again_after_its_thread_ends_later_each_time() {
        fn quick(_: ()) {}
        fn slow(_: ()) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let period = Duration::from_secs(1);
        let mut served: Served<u32> = Served::new(period);
        let start = Instant::now();
        served.update(vec![(1, ())], start, quick);
        // Ended at once: served again a period later, then two.
        std::thread::sleep(Duration::from_millis(50));
        served.update(vec![(1, ())], start, quick);
        assert!(!served.serving(&1));
        let mut again = start + period;
        served.update(vec![(1, ())], again, quick);
        std::thread::sleep(Duration::from_millis(50));
        served.update(vec![(1, ())], again, quick);
        served.update(vec![(1, ())], again + period, quick);
        assert!(!served.serving(&1), "not before two periods");
        again += 2 * period;
        served.update(vec![(1, ())], again, slow);
        assert!(served.serving(&1));
        // Gone: forgotten, and served at once when it comes again.
        served.update(vec![], again, slow);
        served.update(vec![(1, ())], again, slow);
        assert!(served.serving(&1));
    }

    #[test]
    fn sessions_go_on_until_what_is_served_is_gone() {
        let mut attached = 0;
        let mut served = 0;
        let why = sessions(
            "test",
            || {
                attached += 1;
                if attached == 1 { Err(String::from("not yet")) } else { Ok(attached) }
            },
            |session| {
                served += 1;
                Ok(if session < 4 { Ended::Left } else { Ended::Gone(String::from("unplugged")) })
            },
        );
        assert_eq!((why.as_str(), attached, served), ("unplugged", 4, 3));
    }
}
