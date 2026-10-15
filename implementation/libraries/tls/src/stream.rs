// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The TLS stream: TLS records over a [`Transport`] (usually a TCP
//! connection), driven through rustls' unbuffered client API.
//!
//! A [`TlsStream`] keeps three buffers: TLS bytes received but not processed
//! yet (records arrive in pieces or several at a time, and a handshake
//! message may span records), decrypted data not read yet, and records
//! waiting to be sent. Every operation first lets rustls process what was
//! received, queues whatever it produces (handshake messages, responses to
//! key updates, alerts) and sends it before anything later, and reads from
//! the transport only when rustls needs more. The blocking operations wait
//! for the transport to take what is queued; the non-blocking ones
//! ([`TlsStream::try_read`], [`TlsStream::try_write`]) send what it takes
//! at once and leave the rest queued.
//!
//! The end of the stream is the server's close_notify. A transport that
//! ends without it may have been cut short by anyone on the path: reading
//! then fails with [`TlsError::Truncated`] once the data before it is read.

use alloc::string::ToString;
use alloc::vec::Vec;
use core::fmt;

use rustls::client::{ClientConnectionData, UnbufferedClientConnection};
use rustls::pki_types::ServerName;
use rustls::unbuffered::{
    ConnectionState, EncodeError, EncodeTlsData, EncryptError, InsufficientSizeError, UnbufferedStatus, WriteTraffic,
};
use rustls::{Error, ProtocolVersion};
use zeroize::Zeroize;

use crate::config::ClientConfig;
use crate::error::TlsError;

/// The byte stream under TLS.
///
/// Implemented for `vnet::TcpStream`; tests implement it for in-memory
/// pipes.
pub trait Transport {
    /// Blocking read (up to the transport's own read timeout, if it has
    /// one): `Ok(0)` at the end of the stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError>;

    /// Non-blocking read: `Ok(None)` if nothing is available yet,
    /// `Ok(Some(0))` at the end of the stream.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TlsError>;

    /// Blocking write of all of `data`.
    fn write_all(&mut self, data: &[u8]) -> Result<(), TlsError>;

    /// Non-blocking write: how much of `data` the transport took now
    /// (possibly nothing).
    fn try_write(&mut self, data: &[u8]) -> Result<usize, TlsError>;
}

/// A borrowed transport, so that the caller keeps it after the TLS stream.
impl<T: Transport + ?Sized> Transport for &mut T {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        (**self).read(buf)
    }

    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TlsError> {
        (**self).try_read(buf)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), TlsError> {
        (**self).write_all(data)
    }

    fn try_write(&mut self, data: &[u8]) -> Result<usize, TlsError> {
        (**self).try_write(data)
    }
}

impl Transport for vnet::TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        vnet::TcpStream::read(self, buf).map_err(TlsError::from)
    }

    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TlsError> {
        vnet::TcpStream::try_read(self, buf).map_err(TlsError::from)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), TlsError> {
        vnet::TcpStream::write_all(self, data).map_err(TlsError::from)
    }

    fn try_write(&mut self, data: &[u8]) -> Result<usize, TlsError> {
        vnet::TcpStream::try_write(self, data).map_err(TlsError::from)
    }
}

/// Bytes asked of the transport per read: a whole record of the largest
/// size (16 KiB of data plus header and protection overhead).
const READ_CHUNK: usize = 18 * 1024;
/// Most TLS bytes held without completing a record or handshake message
/// (rustls' own limits are lower).
const MAX_INCOMING: usize = 256 * 1024;
/// Application data encrypted per step of [`TlsStream::write_all`].
const WRITE_CHUNK: usize = 16 * 1024;

/// What to do once the connection may send application data.
#[derive(Clone, Copy)]
enum Job<'a> {
    Nothing,
    Send(&'a [u8]),
    Close,
}

/// Why [`TlsStream::drive`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The handshake needs more data from the server.
    NeedInput,
    /// Everything received has been processed, and the connection is open
    /// for sending (for a [`Job::Nothing`]).
    Idle,
    /// The job was done.
    Done,
    /// Both sides have closed the connection.
    Closed,
}

/// A failure inside [`TlsStream::drive`].
enum Failure {
    /// A protocol error (rustls has queued an alert for the server).
    Tls(Error),
    /// The transport failed.
    Io(TlsError),
}

/// A TLS client connection over a transport.
pub struct TlsStream<T: Transport> {
    transport: T,
    conn: UnbufferedClientConnection,
    /// Received TLS bytes not processed yet.
    incoming: Vec<u8>,
    /// Decrypted application data; `plaintext[plaintext_pos..]` is unread.
    plaintext: Vec<u8>,
    plaintext_pos: usize,
    /// Records to send, in order: nothing is sent before them.
    outgoing: Vec<u8>,
    /// The server sent close_notify (no more data will come).
    peer_closed: bool,
    /// The transport reached the end of its stream (without close_notify
    /// unless `peer_closed`).
    eof: bool,
    /// `close` was called.
    closed: bool,
    /// The error that broke the connection, returned by every later call.
    failure: Option<TlsError>,
}

impl<T: Transport> fmt::Debug for TlsStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsStream")
            .field("version", &self.protocol_version())
            .field("cipher_suite", &self.cipher_suite())
            .field("closed", &self.closed)
            .field("peer_closed", &self.peer_closed)
            .finish()
    }
}

impl<T: Transport> Drop for TlsStream<T> {
    fn drop(&mut self) {
        self.plaintext.zeroize();
    }
}

impl<T: Transport> TlsStream<T> {
    /// Runs the handshake over `transport` (blocking) and verifies that the
    /// server's certificate is valid for `server_name` (a DNS name, sent as
    /// SNI, or an IP address).
    pub fn connect(transport: T, server_name: &str, config: &ClientConfig) -> Result<TlsStream<T>, TlsError> {
        let name = ServerName::try_from(server_name)
            .map_err(|_| TlsError::InvalidServerName(server_name.to_string()))?
            .to_owned();
        let conn = UnbufferedClientConnection::new(config.rustls().clone(), name)?;
        let mut stream = TlsStream {
            transport,
            conn,
            incoming: Vec::new(),
            plaintext: Vec::new(),
            plaintext_pos: 0,
            outgoing: Vec::new(),
            peer_closed: false,
            eof: false,
            closed: false,
            failure: None,
        };
        stream.handshake()?;
        Ok(stream)
    }

    fn handshake(&mut self) -> Result<(), TlsError> {
        loop {
            match self.drive(Job::Nothing)? {
                Stop::Idle => return self.flush(),
                Stop::NeedInput => {
                    self.flush()?;
                    if self.receive(true)? == Some(0) {
                        return Err(self.fail(Failure::Io(TlsError::Tls(
                            "the server closed the connection during the handshake".to_string(),
                        ))));
                    }
                }
                Stop::Done | Stop::Closed => {
                    return Err(self.fail(Failure::Io(TlsError::Tls(
                        "the server closed the connection during the handshake".to_string(),
                    ))));
                }
            }
        }
    }

    /// Reads decrypted data, waiting for it (up to the transport's read
    /// timeout). Returns `Ok(0)` once the server has closed the connection
    /// (close_notify); fails with [`TlsError::Truncated`] if the transport
    /// ended without it.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(n) = self.ready(buf)? {
                return Ok(n);
            }
            self.flush()?;
            self.receive(true)?;
        }
    }

    /// Reads decrypted data without waiting: `Ok(None)` if none is ready,
    /// `Ok(Some(0))` at the end of the stream.
    ///
    /// For event loops: when the transport is readable (for a
    /// `vnet::TcpStream`, its `handle()`), call this until it returns
    /// `Ok(None)`. It takes everything the transport has, so the transport
    /// signals readable again only when new data arrives. It may send small
    /// protocol messages (such as a key update response), as far as the
    /// transport takes them at once: it never waits.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TlsError> {
        if buf.is_empty() {
            return Ok(Some(0));
        }
        loop {
            if let Some(n) = self.ready(buf)? {
                self.try_flush()?;
                return Ok(Some(n));
            }
            self.try_flush()?;
            if self.receive(false)?.is_none() {
                return Ok(None);
            }
        }
    }

    /// Decrypted data that is ready: `Some(0)` at the end of the stream,
    /// `None` if more must be received first.
    fn ready(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TlsError> {
        if self.plaintext_pos == self.plaintext.len() && !self.peer_closed && !self.eof {
            self.drive(Job::Nothing)?;
        }
        if self.plaintext_pos < self.plaintext.len() {
            let available = &self.plaintext[self.plaintext_pos..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            self.plaintext_pos += n;
            if self.plaintext_pos == self.plaintext.len() {
                // Read data does not linger in memory.
                self.plaintext.zeroize();
                self.plaintext_pos = 0;
            }
            return Ok(Some(n));
        }
        if self.peer_closed {
            return Ok(Some(0));
        }
        if self.eof {
            return Err(TlsError::Truncated);
        }
        Ok(None)
    }

    /// Encrypts and sends all of `data`, waiting for the transport to take
    /// it.
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), TlsError> {
        for chunk in data.chunks(WRITE_CHUNK) {
            self.queue(chunk)?;
            self.flush()?;
        }
        Ok(())
    }

    /// Encrypts `data` and sends what the transport takes without waiting;
    /// the rest is queued, and goes before anything sent later (so that it
    /// suits small messages, such as a protocol's control messages, which
    /// must not wait for a peer that stopped reading). Returns whether
    /// everything went out.
    pub fn try_write(&mut self, data: &[u8]) -> Result<bool, TlsError> {
        for chunk in data.chunks(WRITE_CHUNK) {
            self.queue(chunk)?;
        }
        self.try_flush()
    }

    /// Encrypts `data` into the records to send.
    fn queue(&mut self, data: &[u8]) -> Result<(), TlsError> {
        if self.closed {
            return Err(TlsError::Closed);
        }
        loop {
            match self.drive(Job::Send(data))? {
                Stop::Done => return Ok(()),
                Stop::NeedInput => {
                    self.flush()?;
                    if self.receive(true)? == Some(0) {
                        return Err(TlsError::Closed);
                    }
                }
                Stop::Idle | Stop::Closed => return Err(TlsError::Closed),
            }
        }
    }

    /// Sends the queued records, waiting for the transport to take them.
    /// A failure leaves an unknown part sent: the connection is broken.
    fn flush(&mut self) -> Result<(), TlsError> {
        if self.outgoing.is_empty() {
            return Ok(());
        }
        match self.transport.write_all(&self.outgoing) {
            Ok(()) => {
                self.outgoing.clear();
                Ok(())
            }
            Err(e) => Err(self.fail(Failure::Io(e))),
        }
    }

    /// Sends as many of the queued records as the transport takes now.
    /// Returns whether none is left.
    fn try_flush(&mut self) -> Result<bool, TlsError> {
        while !self.outgoing.is_empty() {
            match self.transport.try_write(&self.outgoing) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.outgoing.drain(..n.min(self.outgoing.len()));
                }
                Err(e) => return Err(self.fail(Failure::Io(e))),
            }
        }
        Ok(true)
    }

    /// Tells the server that no more data will come (close_notify), best
    /// effort. Reading can continue until the server closes its side;
    /// writing fails from now on. Dropping the stream without closing it
    /// just ends the transport (which the server should take as a
    /// truncation).
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if self.failure.is_none() && self.drive(Job::Close).is_ok() {
            let _ = self.flush();
        }
    }

    /// The transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The transport, for example to change its timeouts. Reading or
    /// writing it directly would corrupt the TLS stream.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// The application protocol the server chose (ALPN), if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn_protocol()
    }

    /// `"TLS 1.3"` or `"TLS 1.2"`.
    pub fn protocol_version(&self) -> &'static str {
        match self.conn.protocol_version() {
            Some(ProtocolVersion::TLSv1_3) => "TLS 1.3",
            Some(ProtocolVersion::TLSv1_2) => "TLS 1.2",
            _ => "unknown",
        }
    }

    /// The cipher suite, such as `"TLS13_AES_128_GCM_SHA256"`.
    pub fn cipher_suite(&self) -> &'static str {
        self.conn.negotiated_cipher_suite().and_then(|s| s.suite().as_str()).unwrap_or("unknown")
    }

    /// The key exchange group, such as `"X25519"`.
    pub fn key_exchange_group(&self) -> &'static str {
        self.conn.negotiated_key_exchange_group().and_then(|g| g.name().as_str()).unwrap_or("unknown")
    }

    /// Runs rustls on the received data until it needs more, has nothing
    /// left to do, or has done `job`: decrypted data goes to `plaintext`,
    /// whatever rustls produces is queued to be sent.
    fn drive(&mut self, job: Job<'_>) -> Result<Stop, TlsError> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        loop {
            let UnbufferedStatus { discard, state } = self.conn.process_tls_records(&mut self.incoming);
            let step = match state {
                Err(e) => Err(Failure::Tls(e)),
                Ok(ConnectionState::ReadTraffic(mut traffic)) => {
                    let mut step = Ok(None);
                    while let Some(record) = traffic.next_record() {
                        match record {
                            Ok(record) => self.plaintext.extend_from_slice(record.payload),
                            Err(e) => {
                                step = Err(Failure::Tls(e));
                                break;
                            }
                        }
                    }
                    step
                }
                Ok(ConnectionState::EncodeTlsData(mut data)) => encode(&mut data, &mut self.outgoing).map(|()| None),
                // The records are queued, ahead of anything sent later:
                // as good as transmitted, in order.
                Ok(ConnectionState::TransmitTlsData(data)) => {
                    data.done();
                    Ok(None)
                }
                Ok(ConnectionState::BlockedHandshake) => Ok(Some(Stop::NeedInput)),
                Ok(ConnectionState::WriteTraffic(mut traffic)) => match job {
                    Job::Nothing => Ok(Some(Stop::Idle)),
                    Job::Send(data) => encrypt(&mut traffic, data, &mut self.outgoing).map(|()| Some(Stop::Done)),
                    Job::Close => close_notify(&mut traffic, &mut self.outgoing).map(|()| Some(Stop::Done)),
                },
                Ok(ConnectionState::PeerClosed) => {
                    self.peer_closed = true;
                    Ok(None)
                }
                Ok(ConnectionState::Closed) => {
                    self.peer_closed = true;
                    Ok(Some(Stop::Closed))
                }
                Ok(_) => Err(Failure::Tls(Error::General("unexpected TLS connection state".to_string()))),
            };
            self.incoming.drain(..discard.min(self.incoming.len()));
            match step {
                Ok(None) => {}
                Ok(Some(stop)) => return Ok(stop),
                Err(failure) => return Err(self.fail(failure)),
            }
        }
    }

    /// Reads more TLS data from the transport (waiting for it if `wait`):
    /// `Ok(None)` if there is none yet, `Ok(Some(0))` at the end of the
    /// stream.
    fn receive(&mut self, wait: bool) -> Result<Option<usize>, TlsError> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let start = self.incoming.len();
        if start >= MAX_INCOMING {
            let e = TlsError::Tls("the server sent an oversized message".to_string());
            return Err(self.fail(Failure::Io(e)));
        }
        self.incoming.resize(start + READ_CHUNK, 0);
        let buf = &mut self.incoming[start..];
        let result = if wait { self.transport.read(buf).map(Some) } else { self.transport.try_read(buf) };
        let got = match result {
            Ok(Some(n)) => n.min(READ_CHUNK),
            _ => 0,
        };
        self.incoming.truncate(start + got);
        match result {
            Ok(Some(0)) => {
                self.eof = true;
                Ok(Some(0))
            }
            Ok(r) => Ok(r),
            // Nothing arrived in time; the connection is still usable.
            Err(TlsError::TimedOut) => Err(TlsError::TimedOut),
            Err(e) => Err(self.fail(Failure::Io(e))),
        }
    }

    /// Records a failure that breaks the connection. After a protocol error
    /// the alert rustls queued (telling the server why) is sent first.
    fn fail(&mut self, failure: Failure) -> TlsError {
        let error = match failure {
            Failure::Tls(e) => {
                self.send_alert();
                TlsError::from(e)
            }
            Failure::Io(e) => e,
        };
        self.failure = Some(error.clone());
        error
    }

    /// Sends the alert rustls queued after an error (best effort: as far
    /// as the transport takes it at once).
    ///
    /// rustls hands out a queued record before it looks at received data
    /// again, so one call yields the alert without processing the data that
    /// caused the error a second time. The buffer must be the usual one:
    /// rustls keeps offsets into it.
    fn send_alert(&mut self) {
        let UnbufferedStatus { discard, state } = self.conn.process_tls_records(&mut self.incoming);
        if let Ok(ConnectionState::EncodeTlsData(mut data)) = state {
            let _ = encode(&mut data, &mut self.outgoing);
        }
        self.incoming.drain(..discard.min(self.incoming.len()));
        while !self.outgoing.is_empty() {
            match self.transport.try_write(&self.outgoing) {
                Ok(n) if n > 0 => {
                    self.outgoing.drain(..n.min(self.outgoing.len()));
                }
                _ => break,
            }
        }
    }
}

/// Grows `out` and lets `write` fill the new space, enlarging it if
/// rustls asks for more.
fn produce(
    out: &mut Vec<u8>,
    mut room: usize,
    mut write: impl FnMut(&mut [u8]) -> Result<usize, Option<usize>>,
) -> Result<(), Failure> {
    let start = out.len();
    loop {
        out.resize(start + room, 0);
        match write(&mut out[start..]) {
            Ok(n) => {
                out.truncate(start + n.min(room));
                return Ok(());
            }
            Err(Some(required)) if required > room => room = required,
            Err(_) => {
                out.truncate(start);
                return Err(Failure::Tls(Error::EncryptError));
            }
        }
    }
}

/// Appends a handshake record (or alert) rustls wants to send.
fn encode(data: &mut EncodeTlsData<'_, ClientConnectionData>, out: &mut Vec<u8>) -> Result<(), Failure> {
    produce(out, 4096, |buf| {
        data.encode(buf).map_err(|e| match e {
            EncodeError::InsufficientSize(InsufficientSizeError { required_size }) => Some(required_size),
            _ => None,
        })
    })
}

/// Appends the records carrying `data`.
fn encrypt(
    traffic: &mut WriteTraffic<'_, ClientConnectionData>,
    data: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), Failure> {
    // Room for the records' headers and protection (a few dozen bytes each).
    let room = data.len() + 64 * (data.len() / 16384 + 1);
    produce(out, room, |buf| traffic.encrypt(data, buf).map_err(encrypt_error))
}

/// Appends a close_notify alert.
fn close_notify(traffic: &mut WriteTraffic<'_, ClientConnectionData>, out: &mut Vec<u8>) -> Result<(), Failure> {
    produce(out, 64, |buf| traffic.queue_close_notify(buf).map_err(encrypt_error))
}

fn encrypt_error(e: EncryptError) -> Option<usize> {
    match e {
        EncryptError::InsufficientSize(InsufficientSizeError { required_size }) => Some(required_size),
        _ => None,
    }
}
