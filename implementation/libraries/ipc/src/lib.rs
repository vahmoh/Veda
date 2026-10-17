// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `ipc` — typed inter-process communication for Veda.
//!
//! * [`codec`]: the binary encoding (`Encode`/`Decode`) plus the `message!`,
//!   `enumeration!` and `union!` macros for declaring wire types.
//! * [`protocol!`]: declares an RPC protocol and generates, in a module, a
//!   typed `Client`, a `Server` trait and a `dispatch` function.
//! * [`WaitSet`]: waits on many handles at once (the core of every service
//!   loop): a set kept from one wait to the next keeps its handles
//!   registered with a port, so that a wait costs what is ready, not how
//!   many handles the set holds.
//!
//! Every message starts with a 12-byte [`Header`]: method ordinal,
//! transaction id and flags. Requests and their responses share a
//! transaction id; events (server → client notifications) use a separate
//! channel in Veda protocols, which keeps clients simple.
//!
//! Every request is answered. A server that cannot serve one (it cannot be
//! decoded, it names no method of the protocol, or its result is larger
//! than a message carries) answers with an error response
//! ([`FLAG_ERROR`]), which fails the caller's call with
//! [`IpcError::Refused`]: no caller waits for an answer that never comes.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod codec;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

pub use abi::DEADLINE_INFINITE;
use abi::{Error, RawHandle, WaitItem};
pub use codec::{Bytes, Decode, DecodeError, Decoder, Encode, Encoder};
pub use rt::object::{Channel, Handle, Message, Port};
pub use rt::time::now_ns;

/// Size of the message header.
pub const HEADER_LEN: usize = 12;
pub const FLAG_REQUEST: u32 = 1;
pub const FLAG_RESPONSE: u32 = 2;
pub const FLAG_EVENT: u32 = 4;
/// With [`FLAG_RESPONSE`]: the server could not serve the request; the body
/// is a [`Refusal`]'s code.
pub const FLAG_ERROR: u32 = 8;

/// Re-exports used by macro-generated code.
#[doc(hidden)]
pub mod prelude {
    pub use alloc::boxed::Box;
    pub use alloc::string::String;
    pub use alloc::vec::Vec;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub ordinal: u32,
    pub txid: u32,
    pub flags: u32,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Header, IpcError> {
        if bytes.len() < HEADER_LEN {
            return Err(IpcError::Decode(DecodeError::Truncated));
        }
        let w = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        Ok(Header { ordinal: w(0), txid: w(4), flags: w(8) })
    }
}

/// IPC failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// The other side closed the channel.
    PeerClosed,
    /// A kernel error while sending or receiving.
    Kernel(Error),
    /// The message was malformed.
    Decode(DecodeError),
    /// The server does not implement the requested method.
    UnknownMethod(u32),
    /// The message is larger than a channel carries
    /// (`abi::CHANNEL_MAX_BYTES`, `abi::CHANNEL_MAX_HANDLES`), so it was
    /// not sent.
    TooLarge,
    /// The server answered that it could not serve the request.
    Refused(Refusal),
}

/// Why a server answered a request with an error response instead of its
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request could not be decoded.
    Malformed,
    /// The request names no method of the server's protocol.
    UnknownMethod,
    /// The result is larger than a message carries.
    TooLarge,
}

impl Refusal {
    fn code(self) -> u32 {
        match self {
            Refusal::Malformed => 1,
            Refusal::UnknownMethod => 2,
            Refusal::TooLarge => 3,
        }
    }

    fn from_code(code: u32) -> Refusal {
        match code {
            2 => Refusal::UnknownMethod,
            3 => Refusal::TooLarge,
            _ => Refusal::Malformed,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::Malformed => "the request was malformed",
            Refusal::UnknownMethod => "the server does not know the request",
            Refusal::TooLarge => "the answer is too large to send",
        })
    }
}

impl From<DecodeError> for IpcError {
    fn from(e: DecodeError) -> Self {
        IpcError::Decode(e)
    }
}

impl From<Error> for IpcError {
    fn from(e: Error) -> Self {
        if e == Error::PeerClosed { IpcError::PeerClosed } else { IpcError::Kernel(e) }
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::PeerClosed => f.write_str("peer closed the channel"),
            IpcError::Kernel(e) => write!(f, "kernel error: {e}"),
            IpcError::Decode(e) => write!(f, "bad message: {e}"),
            IpcError::UnknownMethod(o) => write!(f, "unknown method {o}"),
            IpcError::TooLarge => f.write_str("the message is larger than a channel carries"),
            IpcError::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

impl Encoder {
    /// Starts a message with the given header.
    pub fn with_header(ordinal: u32, txid: u32, flags: u32) -> Encoder {
        let mut e = Encoder::new();
        e.put_u32(ordinal);
        e.put_u32(txid);
        e.put_u32(flags);
        e
    }

    /// Writes the message to `ch`. A message larger than a channel carries
    /// is not sent (`TooLarge`, its handles closed); when it is a response,
    /// the requester is sent an error response in its place, so that its
    /// call fails rather than waits for ever.
    pub fn send(self, ch: &Channel) -> Result<(), IpcError> {
        if self.bytes.len() > abi::CHANNEL_MAX_BYTES || self.handles.len() > abi::CHANNEL_MAX_HANDLES {
            if let Ok(h) = Header::parse(&self.bytes)
                && h.flags & FLAG_RESPONSE != 0
            {
                let _ = refuse(ch, h, Refusal::TooLarge);
            }
            return Err(IpcError::TooLarge);
        }
        ch.write(&self.bytes, self.handles).map_err(IpcError::from)
    }
}

/// Answers the request with header `request` with an error response: the
/// requester's call fails with [`IpcError::Refused`]`(why)`.
pub fn refuse(ch: &Channel, request: Header, why: Refusal) -> Result<(), IpcError> {
    refusal(request, why).send(ch)
}

/// The error response that refuses the request with header `request`.
#[doc(hidden)]
pub fn refusal(request: Header, why: Refusal) -> Encoder {
    let mut e = Encoder::with_header(request.ordinal, request.txid, FLAG_RESPONSE | FLAG_ERROR);
    e.put_u32(why.code());
    e
}

/// Splits a received message into its header and a decoder for the body.
pub fn open(msg: &mut Message) -> Result<(Header, Decoder<'_>), IpcError> {
    let header = Header::parse(&msg.bytes)?;
    let handles = core::mem::take(&mut msg.handles);
    Ok((header, Decoder::new(&msg.bytes[HEADER_LEN..], handles)))
}

/// Sends a request and waits for the response with the same transaction id.
pub fn call(ch: &Channel, request: Encoder, txid: u32) -> Result<Message, IpcError> {
    call_until(ch, request, txid, abi::DEADLINE_INFINITE)
}

/// Like [`call`], but gives up at `deadline` (kernel time in nanoseconds)
/// with `IpcError::Kernel(Error::TimedOut)`. A reply that arrives later is
/// dropped by the next call on the channel.
pub fn call_until(ch: &Channel, request: Encoder, txid: u32, deadline: u64) -> Result<Message, IpcError> {
    request.send(ch)?;
    loop {
        let msg = ch.read_blocking(deadline)?;
        let h = Header::parse(&msg.bytes)?;
        if h.flags & FLAG_RESPONSE != 0 && h.txid == txid {
            if h.flags & FLAG_ERROR != 0 {
                let code = msg
                    .bytes
                    .get(HEADER_LEN..HEADER_LEN + 4)
                    .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap_or_default()));
                return Err(IpcError::Refused(Refusal::from_code(code)));
            }
            return Ok(msg);
        }
        // Anything else (e.g. a late reply to an abandoned call) is dropped.
    }
}

/// Sends a one-way event message with the given ordinal and payload.
pub fn send_event<T: Encode>(ch: &Channel, ordinal: u32, payload: T) -> Result<(), IpcError> {
    let mut e = Encoder::with_header(ordinal, 0, FLAG_EVENT);
    payload.encode(&mut e);
    e.send(ch)
}

/// Decodes the payload of an event message.
pub fn decode_event<T: Decode>(mut msg: Message) -> Result<(u32, T), IpcError> {
    let (h, mut d) = open(&mut msg)?;
    let v = T::decode(&mut d)?;
    Ok((h.ordinal, v))
}

/// Declares an RPC protocol.
///
/// ```ignore
/// ipc::protocol! {
///     /// File system service.
///     pub mod vfs = "vfs" {
///         1 => fn stat(path: String) -> Result<FileInfo, FsError>;
///         2 => fn read(fd: u32, offset: u64, len: u32) -> Result<Bytes, FsError>;
///     }
/// }
/// ```
///
/// generates `vfs::NAME`, `vfs::Client` (one method per call, returning
/// `Result<ReturnType, IpcError>`), the `vfs::Server` trait and
/// `vfs::dispatch(&mut impl Server, Message) -> Result<Encoder, IpcError>`,
/// which gives the answer to a request: its response, or the error response
/// that refuses a request that cannot be decoded or names no method.
#[macro_export]
macro_rules! protocol {
    (
        $(#[$meta:meta])*
        $vis:vis mod $module:ident = $service:literal {
            $(
                $(#[$mmeta:meta])*
                $ord:literal => fn $method:ident ( $($arg:ident : $aty:ty),* $(,)? ) -> $ret:ty ;
            )*
        }
    ) => {
        $(#[$meta])*
        // A program uses one side of a protocol, the client or the server.
        #[allow(clippy::too_many_arguments, unused_imports, dead_code)]
        $vis mod $module {
            use super::*;
            use $crate::prelude::*;

            /// Name under which the service registers.
            pub const NAME: &str = $service;

            /// Typed client stub. Not thread-safe: wrap it in a mutex to
            /// share it.
            pub struct Client {
                channel: $crate::Channel,
                txid: core::cell::Cell<u32>,
                /// Nanoseconds to wait for each reply (0: forever).
                timeout_ns: core::cell::Cell<u64>,
            }

            impl Client {
                pub fn new(channel: $crate::Channel) -> Client {
                    Client { channel, txid: core::cell::Cell::new(1), timeout_ns: core::cell::Cell::new(0) }
                }

                /// Makes every call give up after `ns` nanoseconds without a
                /// reply (0: wait forever, the default). Use it towards peers
                /// that may hang, such as drivers.
                pub fn set_timeout(&self, ns: u64) {
                    self.timeout_ns.set(ns);
                }

                fn deadline(&self) -> u64 {
                    match self.timeout_ns.get() {
                        0 => $crate::DEADLINE_INFINITE,
                        t => $crate::now_ns().saturating_add(t),
                    }
                }

                pub fn channel(&self) -> &$crate::Channel {
                    &self.channel
                }

                pub fn into_channel(self) -> $crate::Channel {
                    self.channel
                }

                fn next_txid(&self) -> u32 {
                    let t = self.txid.get();
                    self.txid.set(t.wrapping_add(1).max(1));
                    t
                }

                $(
                    $(#[$mmeta])*
                    pub fn $method(&self, $($arg: $aty),*) -> Result<$ret, $crate::IpcError> {
                        let txid = self.next_txid();
                        #[allow(unused_mut)]
                        let mut e = $crate::Encoder::with_header($ord, txid, $crate::FLAG_REQUEST);
                        $( $crate::Encode::encode($arg, &mut e); )*
                        let mut reply = $crate::call_until(&self.channel, e, txid, self.deadline())?;
                        let (_, mut d) = $crate::open(&mut reply)?;
                        Ok(<$ret as $crate::Decode>::decode(&mut d)?)
                    }
                )*
            }

            /// Implemented by the service.
            pub trait Server {
                $(
                    $(#[$mmeta])*
                    fn $method(&mut self, $($arg: $aty),*) -> $ret;
                )*
            }

            /// Decodes one request, invokes the server and encodes the
            /// answer to send back on the same channel: the response, or
            /// for a request that cannot be decoded or names no method of
            /// the protocol, the error response that refuses it (the
            /// caller's call fails with `IpcError::Refused`). Fails only
            /// for a message too short to have a header.
            pub fn dispatch<S: Server + ?Sized>(server: &mut S, mut msg: $crate::Message) -> Result<$crate::Encoder, $crate::IpcError> {
                let (header, mut d) = $crate::open(&mut msg)?;
                let answer = (|| -> Result<$crate::Encoder, $crate::IpcError> {
                    match header.ordinal {
                        $(
                            $ord => {
                                $( let $arg = <$aty as $crate::Decode>::decode(&mut d)?; )*
                                d.finish()?;
                                let r = server.$method($($arg),*);
                                let mut e = $crate::Encoder::with_header($ord, header.txid, $crate::FLAG_RESPONSE);
                                $crate::Encode::encode(r, &mut e);
                                Ok(e)
                            }
                        )*
                        other => Err($crate::IpcError::UnknownMethod(other)),
                    }
                })();
                Ok(answer.unwrap_or_else(|e| match e {
                    $crate::IpcError::UnknownMethod(_) => $crate::refusal(header, $crate::Refusal::UnknownMethod),
                    _ => $crate::refusal(header, $crate::Refusal::Malformed),
                }))
            }
        }
    };
}

/// A set of handles to wait on together, each tagged with a key.
///
/// A set waited on once waits as `object_wait_many` does: one system call,
/// every handle registered for it and unregistered after. A set waited on
/// again (a service's loop keeps one, [`WaitSet::clear`]ing and filling it
/// each time round), or one of more handles than a call takes
/// (`abi::WAIT_MANY_MAX`), keeps its handles registered with a port
/// instead (`object_wait_async`): a wait then costs what changed since the
/// last one and what is ready, however many handles the set holds. Where
/// there are no ports (the driver VM's guest, whose calls the monitor's
/// bridge carries), every wait is one call, and a set of more handles than
/// a call takes is polled.
#[derive(Default)]
pub struct WaitSet {
    items: Vec<WaitItem>,
    keys: Vec<u64>,
    /// It waited before: the next wait is worth registering for.
    waited: bool,
    port: Option<(Port, Registry)>,
    /// The kernel has no ports for it.
    no_port: bool,
}

impl WaitSet {
    pub fn new() -> WaitSet {
        WaitSet::default()
    }

    pub fn add(&mut self, handle: RawHandle, signals: u32, key: u64) {
        self.items.push(WaitItem { handle, signals, observed: 0, _reserved: 0 });
        self.keys.push(key);
    }

    pub fn remove(&mut self, key: u64) {
        if let Some(i) = self.keys.iter().position(|&k| k == key) {
            self.items.swap_remove(i);
            self.keys.swap_remove(i);
        }
    }

    /// Empties the set, to be filled again for the next wait: what it held
    /// and holds again stays registered.
    pub fn clear(&mut self) {
        self.items.clear();
        self.keys.clear();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Waits until at least one handle is ready or `deadline` passes.
    /// Returns `(key, observed signals)` for every ready handle; an empty
    /// result means the deadline passed.
    pub fn wait(&mut self, deadline: u64) -> Result<Vec<(u64, u32)>, Error> {
        let again = core::mem::replace(&mut self.waited, true);
        if self.port.is_none() && !self.no_port && (again || self.items.len() > abi::WAIT_MANY_MAX) {
            match Port::create() {
                Ok(port) => self.port = Some((port, Registry::default())),
                Err(_) => self.no_port = true,
            }
        }
        let Some((port, registry)) = &mut self.port else { return self.wait_many(deadline) };
        let wanted: Vec<Wanted> = self
            .items
            .iter()
            .zip(&self.keys)
            .map(|(it, &key)| Wanted { handle: it.handle, signals: it.signals, key })
            .collect();
        let plan = registry.plan(&wanted);
        for (handle, kernel_key) in plan.cancel {
            // A handle closed took its registrations with it.
            let _ = rt::object::wait_async_cancel(handle, port, kernel_key);
        }
        for (w, kernel_key) in plan.register {
            rt::object::wait_async(w.handle, port, kernel_key, w.signals)?;
            registry.armed(kernel_key);
        }
        let mut packets = [abi::PortPacket::default(); abi::PORT_WAIT_MAX];
        loop {
            let n = match port.wait_packets(&mut packets, deadline) {
                Ok(n) => n,
                Err(Error::TimedOut) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
            let ready: Vec<(u64, u32)> =
                packets[..n].iter().filter_map(|p| registry.fired(p.key).map(|key| (key, p.observed))).collect();
            // Packets of registrations undone since say nothing: wait on.
            if !ready.is_empty() {
                return Ok(ready);
            }
        }
    }

    /// Waits in one call (or, for more handles than a call takes, polls
    /// them in calls of as many as it takes).
    fn wait_many(&mut self, deadline: u64) -> Result<Vec<(u64, u32)>, Error> {
        if self.items.is_empty() {
            rt::time::sleep_until(deadline);
            return Ok(Vec::new());
        }
        let max = abi::WAIT_MANY_MAX;
        if self.items.len() <= max {
            match rt::object::wait_many(&mut self.items, deadline) {
                Ok(_) | Err(Error::TimedOut) => {}
                Err(e) => return Err(e),
            }
            return Ok(self.ready(self.items.len()));
        }
        for chunk in self.items.chunks_mut(max) {
            let _ = rt::object::wait_many(chunk, 0);
        }
        let ready = self.ready(self.items.len());
        if !ready.is_empty() {
            return Ok(ready);
        }
        // Nothing ready among many handles: block on a short timeout.
        let short = rt::time::now_ns().saturating_add(2_000_000).min(deadline);
        let _ = rt::object::wait_many(&mut self.items[..max], short);
        Ok(self.ready(max))
    }

    /// The first `n` items' that are ready, with their keys.
    fn ready(&self, n: usize) -> Vec<(u64, u32)> {
        self.items[..n]
            .iter()
            .zip(&self.keys)
            .filter(|(it, _)| it.observed & it.signals != 0)
            .map(|(it, &k)| (k, it.observed))
            .collect()
    }
}

/// A handle a set waits on: its signals of interest, and the set's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wanted {
    handle: RawHandle,
    signals: u32,
    key: u64,
}

/// What a set has registered with its port, by the key each registration
/// has with the kernel. A kernel key is never used for another handle, so
/// that a packet of a registration since undone is known for stale. A
/// registration is known by its handle's number and the set's key: the
/// kernel gives a closed handle's number to another object only after its
/// slot has been used 2048 times more, far more than a service opens and
/// closes in one slot between two waits.
#[derive(Default)]
struct Registry {
    /// Registrations by kernel key: what they are for, and whether one is
    /// waiting (not yet fired) in the kernel.
    by_kernel_key: BTreeMap<u64, (Wanted, bool)>,
    /// Kernel keys by handle and set key.
    by_handle: BTreeMap<(RawHandle, u64), u64>,
    next: u64,
}

/// What to do before a wait: registrations to undo (handle, kernel key),
/// and ones to make (again, for those that fired).
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    cancel: Vec<(RawHandle, u64)>,
    register: Vec<(Wanted, u64)>,
}

impl Registry {
    /// What makes the registrations those of `wanted`: undoing those no
    /// longer wanted (or wanted for other signals), registering the new
    /// ones and again those that fired.
    fn plan(&mut self, wanted: &[Wanted]) -> Plan {
        let mut plan = Plan::default();
        let wanted_by_handle: BTreeMap<(RawHandle, u64), &Wanted> =
            wanted.iter().map(|w| ((w.handle, w.key), w)).collect();
        let stale: Vec<(RawHandle, u64)> = self
            .by_handle
            .iter()
            .filter(|(id, kernel_key)| {
                let (had, _) = &self.by_kernel_key[kernel_key];
                wanted_by_handle.get(id).is_none_or(|w| w.signals != had.signals)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(kernel_key) = self.by_handle.remove(&id) {
                self.by_kernel_key.remove(&kernel_key);
                plan.cancel.push((id.0, kernel_key));
            }
        }
        for (id, w) in wanted_by_handle {
            match self.by_handle.get(&id) {
                Some(&kernel_key) => {
                    if !self.by_kernel_key[&kernel_key].1 {
                        plan.register.push((*w, kernel_key));
                    }
                }
                None => {
                    self.next += 1;
                    let kernel_key = self.next;
                    self.by_handle.insert(id, kernel_key);
                    self.by_kernel_key.insert(kernel_key, (*w, false));
                    plan.register.push((*w, kernel_key));
                }
            }
        }
        plan
    }

    /// The registration of `kernel_key` is waiting in the kernel.
    fn armed(&mut self, kernel_key: u64) {
        if let Some((_, waiting)) = self.by_kernel_key.get_mut(&kernel_key) {
            *waiting = true;
        }
    }

    /// The registration of `kernel_key` fired: the set's key, unless it
    /// was undone since.
    fn fired(&mut self, kernel_key: u64) -> Option<u64> {
        let (w, waiting) = self.by_kernel_key.get_mut(&kernel_key)?;
        *waiting = false;
        Some(w.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    protocol! {
        mod echo = "echo" {
            1 => fn echo(text: String) -> String;
        }
    }

    struct Echo;

    impl echo::Server for Echo {
        fn echo(&mut self, text: String) -> String {
            text
        }
    }

    fn request(ordinal: u32, body: &[u8]) -> Message {
        let mut e = Encoder::with_header(ordinal, 7, FLAG_REQUEST);
        e.bytes.extend_from_slice(body);
        Message { bytes: e.bytes, handles: vec![] }
    }

    fn answer(msg: Message) -> (Header, Vec<u8>) {
        let reply = echo::dispatch(&mut Echo, msg).expect("a request is answered");
        (Header::parse(&reply.bytes).unwrap(), reply.bytes[HEADER_LEN..].to_vec())
    }

    #[test]
    fn a_request_is_answered_with_its_response() {
        let mut body = Encoder::new();
        String::from("hi").encode(&mut body);
        let (h, rest) = answer(request(1, &body.bytes));
        assert_eq!((h.ordinal, h.txid, h.flags), (1, 7, FLAG_RESPONSE));
        let mut d = Decoder::new(&rest, vec![]);
        assert_eq!(String::decode(&mut d).unwrap(), "hi");
    }

    #[test]
    fn a_request_that_cannot_be_served_is_refused() {
        // An unknown method, then a body that does not decode.
        for (ordinal, body, why) in [(9, &[][..], Refusal::UnknownMethod), (1, &[0xFF, 0xFF][..], Refusal::Malformed)] {
            let (h, rest) = answer(request(ordinal, body));
            assert_eq!((h.ordinal, h.txid, h.flags), (ordinal, 7, FLAG_RESPONSE | FLAG_ERROR));
            assert_eq!(Refusal::from_code(u32::from_le_bytes(rest[..4].try_into().unwrap())), why);
        }
    }

    fn wanted(handle: RawHandle, signals: u32, key: u64) -> Wanted {
        Wanted { handle, signals, key }
    }

    #[test]
    fn a_set_kept_registers_only_what_changed_and_what_fired() {
        let mut r = Registry::default();
        let (a, b, c) = (wanted(10, 1, 100), wanted(11, 1, 101), wanted(12, 5, 102));
        let first = r.plan(&[a, b, c]);
        assert!(first.cancel.is_empty());
        assert_eq!(first.register.iter().map(|(w, _)| *w).collect::<Vec<_>>(), vec![a, b, c]);
        for (_, k) in &first.register {
            r.armed(*k);
        }
        // Nothing changed, nothing fired: nothing to do.
        assert_eq!(r.plan(&[a, b, c]), Plan::default());
        // `b` fired: registered again, under the same kernel key.
        let kb = first.register[1].1;
        assert_eq!(r.fired(kb), Some(101));
        let again = r.plan(&[a, b, c]);
        assert_eq!(again, Plan { cancel: vec![], register: vec![(b, kb)] });
        r.armed(kb);
        // `c` is wanted for other signals, `a` no longer, `d` is new.
        let (c2, d) = (wanted(12, 4, 102), wanted(13, 1, 103));
        let changed = r.plan(&[b, c2, d]);
        let (ka, kc) = (first.register[0].1, first.register[2].1);
        assert_eq!(changed.cancel, vec![(10, ka), (12, kc)]);
        let made: Vec<Wanted> = changed.register.iter().map(|(w, _)| *w).collect();
        assert_eq!(made, vec![c2, d]);
        // New registrations never reuse a kernel key: a packet of one undone
        // is stale.
        assert!(changed.register.iter().all(|(_, k)| *k > kc));
        assert_eq!(r.fired(ka), None);
    }

    #[test]
    fn a_handle_waited_on_for_two_keys_is_two_registrations() {
        let mut r = Registry::default();
        let plan = r.plan(&[wanted(10, 1, 1), wanted(10, 2, 2)]);
        assert_eq!(plan.register.len(), 2);
        assert_ne!(plan.register[0].1, plan.register[1].1);
    }

    #[test]
    fn a_message_without_a_header_is_an_error() {
        let msg = Message { bytes: vec![1, 2, 3], handles: vec![] };
        assert!(echo::dispatch(&mut Echo, msg).is_err());
    }
}
