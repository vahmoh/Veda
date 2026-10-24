// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Sockets: bidirectional byte streams.
//!
//! A socket has two endpoints. Bytes written to one are read from the other,
//! in order, through a bounded buffer per direction. Unlike channels, socket
//! handles can be duplicated, so several processes can share an endpoint as
//! they share a Unix pipe or terminal: the peer sees `PEER_CLOSED` only once
//! every handle to an endpoint is closed. An endpoint can also stop writing
//! ([`SocketEnd::shutdown`]); its peer then reads what is buffered and after
//! that the end of the stream, while the other direction keeps working.
//!
//! Bytes go straight between the buffer and where the caller has them: a
//! write is given the room it fills ([`SocketEnd::write_with`]), and a read
//! the bytes it copies out ([`SocketEnd::read_with`]), as up to two parts of
//! the buffer each (it wraps around), so that a system call copies them to
//! or from user memory without a buffer of its own.
//!
//! Handles can go with the bytes ([`SocketEnd::write_with`]), as Unix
//! domain sockets send descriptors (`SCM_RIGHTS`): they move with the first
//! byte written with them, and come out with the read that reaches it. A
//! read takes the handles of one write at most and ends before the next
//! write's; a read that does not ask for them closes them. Whoever holds an
//! endpoint reads them, as whoever holds it reads its bytes. As on
//! channels, no handle may go where it would keep itself alive
//! ([`super::would_cycle`]), and a direction holds at most
//! [`MAX_QUEUED_HANDLES`] of them.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::Error;
use abi::signals::{PEER_CLOSED, PEER_WRITE_DISABLED, READABLE, WRITABLE};

use crate::object::handle::Handle;
use crate::object::{KObject, Signals};
use crate::synchronization::SpinLock;

/// Bytes buffered per direction before writers get `ShouldWait`.
pub const CAPACITY: usize = abi::SOCKET_CAPACITY;
/// Writes of at most this many bytes are never split: they go in whole or
/// wait (POSIX `PIPE_BUF`). An endpoint is writable while this much fits.
pub const ATOMIC_WRITE: usize = abi::SOCKET_ATOMIC_WRITE;
/// A buffer that has emptied keeps its memory up to this size.
const KEPT_BUFFER: usize = 64 * 1024;
/// Handles waiting with the bytes of one direction at most: a write that
/// would queue more gets `LimitReached`.
pub const MAX_QUEUED_HANDLES: usize = 1024;

/// Makes room for `n` more bytes in `buf`, growing it by doubling but never
/// beyond [`CAPACITY`]; `NoMemory` (not a panic) if the kernel heap cannot.
fn grow(buf: &mut VecDeque<u8>, n: usize) -> Result<(), Error> {
    let need = buf.len() + n;
    if need <= buf.capacity() {
        return Ok(());
    }
    let target = need.max(buf.capacity() * 2).min(CAPACITY);
    buf.try_reserve_exact(target - buf.len()).map_err(|_| Error::NoMemory)
}

/// Gives `put` the first `n` bytes of `buf`, as the parts it keeps them in,
/// each with the place of its first byte among them.
fn copy_out(
    buf: &VecDeque<u8>,
    n: usize,
    put: &mut impl FnMut(usize, &[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let (front, back) = buf.as_slices();
    let from_front = n.min(front.len());
    put(0, &front[..from_front])?;
    if n > from_front {
        put(from_front, &back[..n - from_front])?;
    }
    Ok(())
}

/// Handles sent with a write's bytes.
struct Mark {
    /// The place of the write's first byte (counted as [`EndState::taken`]
    /// counts).
    at: u64,
    handles: Vec<Handle>,
}

struct EndState {
    /// Bytes written by the peer, waiting to be read here.
    rx: VecDeque<u8>,
    /// Bytes ever read here: the place of the first byte in `rx`.
    taken: u64,
    /// The handles sent with the bytes in `rx`, in order.
    marks: VecDeque<Mark>,
    /// How many handles `marks` holds.
    queued_handles: usize,
    /// Every handle to this endpoint is closed.
    closed: bool,
    /// This endpoint will write no more.
    write_disabled: bool,
}

impl EndState {
    /// How many of `n` bytes from the first waiting a read takes (it ends
    /// before the place of a second write's handles), and whether handles
    /// come with them.
    fn span(&self, n: usize) -> (usize, bool) {
        let end = self.taken + n as u64;
        let mut places = self.marks.iter().map(|m| m.at).take_while(|&at| at < end);
        match (places.next(), places.next()) {
            (None, _) => (n, false),
            (Some(_), None) => (n, true),
            // Marks are in the order of their places, each with a byte.
            (Some(_), Some(second)) => ((second - self.taken) as usize, true),
        }
    }
}

struct Pair {
    ends: SpinLock<[EndState; 2]>,
    signals: [Signals; 2],
    koids: [u64; 2],
}

/// One endpoint of a socket.
pub struct SocketEnd {
    pair: Arc<Pair>,
    side: usize,
}

/// Creates a connected pair of endpoints.
pub fn create() -> (Arc<SocketEnd>, Arc<SocketEnd>) {
    let empty = || EndState {
        rx: VecDeque::new(),
        taken: 0,
        marks: VecDeque::new(),
        queued_handles: 0,
        closed: false,
        write_disabled: false,
    };
    let pair = Arc::new(Pair {
        ends: SpinLock::new([empty(), empty()]),
        signals: [Signals::new(WRITABLE), Signals::new(WRITABLE)],
        koids: [crate::object::new_koid(), crate::object::new_koid()],
    });
    (Arc::new(SocketEnd { pair: pair.clone(), side: 0 }), Arc::new(SocketEnd { pair, side: 1 }))
}

impl SocketEnd {
    pub fn koid(&self) -> u64 {
        self.pair.koids[self.side]
    }

    pub fn signals(&self) -> &Signals {
        &self.pair.signals[self.side]
    }

    fn peer(&self) -> usize {
        1 - self.side
    }

    /// What identifies the socket's queues ([`super::would_cycle`]).
    pub fn queues(&self) -> *const () {
        Arc::as_ptr(&self.pair) as *const ()
    }

    /// The objects the handles waiting with the socket's bytes refer to,
    /// both ways.
    pub fn queued(&self) -> Vec<KObject> {
        let ends = self.pair.ends.lock();
        ends.iter().flat_map(|e| e.marks.iter()).flat_map(|m| m.handles.iter()).map(|h| h.object.clone()).collect()
    }

    /// How many bytes [`write`](Self::write) would accept now.
    pub fn writable_bytes(&self) -> usize {
        let ends = self.pair.ends.lock();
        let peer = &ends[self.peer()];
        if peer.closed || ends[self.side].write_disabled { 0 } else { CAPACITY - peer.rx.len() }
    }

    /// Bytes waiting to be read from this endpoint.
    pub fn readable_bytes(&self) -> usize {
        self.pair.ends.lock()[self.side].rx.len()
    }

    /// Appends as much of `len` bytes as fits to the peer's buffer and
    /// returns how much that was: `fill` writes them in the room made for
    /// them, given each part of it with the place of its first byte among
    /// them; if it fails, nothing is written. Writes of up to
    /// [`ATOMIC_WRITE`] bytes go in whole or not at all. `handles` go with
    /// the first byte (there must be one then): they are the socket's if a
    /// byte goes, and dropped otherwise. The caller has made sure that they
    /// do not reach the socket ([`super::would_cycle`]).
    pub fn write_with(
        &self,
        len: usize,
        handles: Vec<Handle>,
        mut fill: impl FnMut(usize, &mut [u8]) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        if !handles.is_empty() && len == 0 {
            return Err(Error::InvalidArgs);
        }
        let peer = self.peer();
        // On failure, the handles are dropped once the lock is let go
        // (dropping one may close an endpoint of this very socket).
        let mut handles = Some(handles);
        let (n, now_full) = {
            let mut ends = self.pair.ends.lock();
            if ends[self.side].write_disabled {
                return Err(Error::BadState);
            }
            let pe = &mut ends[peer];
            if pe.closed {
                return Err(Error::PeerClosed);
            }
            let room = CAPACITY - pe.rx.len();
            if room == 0 || (len <= ATOMIC_WRITE && room < len) {
                return Err(Error::ShouldWait);
            }
            let count = handles.as_ref().map_or(0, Vec::len);
            if pe.queued_handles + count > MAX_QUEUED_HANDLES {
                return Err(Error::LimitReached);
            }
            let n = len.min(room);
            grow(&mut pe.rx, n)?;
            if count > 0 {
                pe.marks.try_reserve(1).map_err(|_| Error::NoMemory)?;
            }
            // The room, which a `VecDeque` gives only zeroed (still cheaper
            // than bringing the bytes through a buffer), filled in place.
            let old = pe.rx.len();
            pe.rx.resize(old + n, 0);
            let (front, back) = pe.rx.as_mut_slices();
            let filled = if old >= front.len() {
                fill(0, &mut back[old - front.len()..])
            } else {
                let first = front.len() - old;
                fill(0, &mut front[old..]).and_then(|()| fill(first, back))
            };
            if let Err(e) = filled {
                pe.rx.truncate(old);
                return Err(e);
            }
            if count > 0 {
                let at = pe.taken + old as u64;
                pe.marks.push_back(Mark { at, handles: handles.take().unwrap_or_default() });
                pe.queued_handles += count;
            }
            (n, CAPACITY - pe.rx.len() < ATOMIC_WRITE)
        };
        if n > 0 {
            self.pair.signals[peer].update(0, READABLE);
        }
        if now_full {
            self.signals().update(WRITABLE, 0);
        }
        Ok(n)
    }

    /// Takes up to `max` buffered bytes, which `put` copies out (given each
    /// part of the buffer they are in, with the place of its first byte
    /// among them), and returns how many, and the handles that went with
    /// them: a write's whose first byte is among them, the read ending before
    /// the next write's (whose first byte comes next). If `put` fails,
    /// nothing is taken. At the end of the stream (the peer closed or stopped
    /// writing, and nothing is left) the result is `PeerClosed`.
    pub fn read_with(
        &self,
        max: usize,
        mut put: impl FnMut(usize, &[u8]) -> Result<(), Error>,
    ) -> Result<(usize, Vec<Handle>), Error> {
        let peer = self.peer();
        let (n, handles, now_empty, peer_writable) = {
            let mut ends = self.pair.ends.lock();
            let ended = ends[peer].closed || ends[peer].write_disabled;
            let me = &mut ends[self.side];
            if me.rx.is_empty() {
                return Err(if ended { Error::PeerClosed } else { Error::ShouldWait });
            }
            let (n, marked) = me.span(max.min(me.rx.len()));
            copy_out(&me.rx, n, &mut put)?;
            me.rx.drain(..n);
            me.taken += n as u64;
            let handles = match marked {
                true => me.marks.pop_front().map_or(Vec::new(), |m| m.handles),
                false => Vec::new(),
            };
            me.queued_handles -= handles.len();
            if me.rx.is_empty() && me.rx.capacity() > KEPT_BUFFER {
                // A reader that fell behind made it large; give it back.
                me.rx = VecDeque::new();
            }
            let (room, now_empty) = (CAPACITY - me.rx.len(), me.rx.is_empty());
            (n, handles, now_empty, room >= ATOMIC_WRITE && !ends[peer].write_disabled)
        };
        if now_empty {
            self.signals().update(READABLE, 0);
        }
        if peer_writable {
            self.pair.signals[peer].update(0, WRITABLE);
        }
        Ok((n, handles))
    }

    /// Gives `put` up to `max` buffered bytes, as
    /// [`read_with`](Self::read_with) would (up to the next write's handles),
    /// and leaves them to be read, without their handles. At the end of the
    /// stream the result is `PeerClosed`.
    pub fn peek_with(
        &self,
        max: usize,
        mut put: impl FnMut(usize, &[u8]) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let peer = self.peer();
        let ends = self.pair.ends.lock();
        let ended = ends[peer].closed || ends[peer].write_disabled;
        let me = &ends[self.side];
        if me.rx.is_empty() {
            return Err(if ended { Error::PeerClosed } else { Error::ShouldWait });
        }
        let (n, _) = me.span(max.min(me.rx.len()));
        copy_out(&me.rx, n, &mut put)?;
        Ok(n)
    }

    /// Stops writing from this endpoint: the peer reads what is buffered
    /// and then the end of the stream. Reading here is unaffected.
    pub fn shutdown(&self) {
        let peer = self.peer();
        let changed = {
            let mut ends = self.pair.ends.lock();
            !core::mem::replace(&mut ends[self.side].write_disabled, true)
        };
        if changed {
            self.signals().update(WRITABLE, 0);
            self.pair.signals[peer].update(0, PEER_WRITE_DISABLED);
        }
    }
}

impl Drop for SocketEnd {
    fn drop(&mut self) {
        let peer = self.peer();
        let unread = {
            let mut ends = self.pair.ends.lock();
            let me = &mut ends[self.side];
            me.closed = true;
            me.queued_handles = 0;
            (core::mem::take(&mut me.rx), core::mem::take(&mut me.marks))
        };
        // Dropping the handles that waited may close further objects (even
        // the other endpoint): not under the lock.
        drop(unread);
        self.pair.signals[peer].update(WRITABLE, PEER_CLOSED);
    }
}
