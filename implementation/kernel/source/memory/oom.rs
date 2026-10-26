// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! When memory runs out: which process the system ends to make room.
//!
//! Memory is committed as it is touched, so it runs out at a page fault,
//! which no call can return an error from. Ending the process that touched
//! the page would end whichever was unlucky: the window system, or the
//! browser while one of its pages took all the memory. The kernel ends the
//! process whose loss costs least instead ([`abi::memory_class`]): of the
//! processes that are not the system's own, one of the most expendable
//! class, and of those the one that holds the most memory, as that is
//! most likely what took it. The thread whose access found no page waits
//! until that process is gone, then makes its access again; if there is
//! still no page, the next process goes. Only when the process that
//! touched the page is the one to go, or no other may, does it end itself.
//!
//! One process is ended for memory at a time: a fault while one is still
//! going waits for it rather than end another, as its memory may be all
//! that was missing.

use alloc::sync::Arc;

use crate::scheduler::process::{self, Process};
use crate::scheduler::{self, Thread, WakeReason};

/// How long a thread waits for a process ended for memory to be gone, at
/// most: processes end within milliseconds (their threads are woken and
/// interrupted), so one that has not after this will not, and the faulting
/// process ends instead of waiting for ever.
const PATIENCE_NS: u64 = 10_000_000_000;

/// Makes room for an access of `cur`'s that found no page, by ending
/// another process. `true` once that process is gone, and the access can
/// be made again; `false` if `cur`'s process is to end itself: it is the
/// one that should go, or it is going already.
pub fn make_room(cur: &Arc<Thread>) -> bool {
    let Some(me) = cur.process.clone() else { return false };
    if me.ending_with().is_some() {
        return false;
    }
    let victim = match going() {
        Some(p) => p,
        None => match choose(&me) {
            Some((p, bytes)) if p.koid != me.koid => {
                let (total, free) = super::phys::stats();
                crate::kwarn!(
                    "out of memory: ending {} (process {}, {}, {} MiB) to make room for {} (thread {}); \
                     {} MiB of {} MiB free",
                    p.name.as_str(),
                    p.koid,
                    class_name(p.memory_class()),
                    bytes >> 20,
                    cur.process_name(),
                    cur.koid,
                    free >> 20,
                    total >> 20
                );
                p.kill(abi::EXIT_CODE_OUT_OF_MEMORY);
                p
            }
            _ => return false,
        },
    };
    wait_gone(cur, &victim)
}

/// A process ended for memory whose threads have not all ended yet.
fn going() -> Option<Arc<Process>> {
    process::all().into_iter().find(|p| p.ending_with() == Some(abi::EXIT_CODE_OUT_OF_MEMORY) && !p.is_terminated())
}

/// The process to end, with the memory it holds: of `me` and the processes
/// above the system's class not ending already, the one of the most
/// expendable class, and of that class the one holding the most. `me` is
/// there whatever its class, so that it is the one to go if nothing else
/// may.
fn choose(me: &Arc<Process>) -> Option<(Arc<Process>, u64)> {
    process::all()
        .into_iter()
        .filter(|p| !p.is_terminated() && p.ending_with().is_none())
        .filter(|p| p.koid == me.koid || usize::from(p.memory_class()) > abi::memory_class::SYSTEM)
        .map(|p| {
            let bytes = p.aspace().map_or(0, |a| a.committed_bytes());
            (p, bytes)
        })
        .max_by_key(|(p, bytes)| (p.memory_class(), *bytes))
}

/// Waits until `victim` is gone (its `TERMINATED` signal), for
/// [`PATIENCE_NS`] at most: `true` if it is, `false` if it is not by then
/// or `cur` is ended meanwhile.
fn wait_gone(cur: &Arc<Thread>, victim: &Arc<Process>) -> bool {
    let id = crate::capability::new_koid();
    let deadline = crate::time::now_ns().saturating_add(PATIENCE_NS);
    while !victim.is_terminated() {
        victim.signals.add_waiter(id, cur.clone(), abi::signals::TERMINATED);
        // It may have ended between the check and the registration.
        let reason = if victim.is_terminated() { WakeReason::Signaled } else { scheduler::block(Some(deadline)) };
        victim.signals.remove_waiter(id);
        match reason {
            WakeReason::Killed => return false,
            WakeReason::TimedOut => {
                if victim.is_terminated() {
                    break;
                }
                crate::kerror!(
                    "out of memory: {} (process {}) did not end in {} s; {} (thread {}) ends instead",
                    victim.name.as_str(),
                    victim.koid,
                    PATIENCE_NS / 1_000_000_000,
                    cur.process_name(),
                    cur.koid
                );
                return false;
            }
            _ => {}
        }
    }
    true
}

/// How the log names a memory class.
fn class_name(class: u8) -> &'static str {
    match usize::from(class) {
        abi::memory_class::SYSTEM => "the system's",
        abi::memory_class::APPLICATION => "an application",
        _ => "expendable",
    }
}
