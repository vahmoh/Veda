// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `web` — web clients for Veda applications.
//!
//! * [`Url`]: `http`, `https`, `ws` and `wss` URLs, the references
//!   redirects give ([`Url::join`]), and percent-encoding for query strings
//!   ([`url::encode_component`]).
//! * [`http`]: HTTP/1.1 requests and responses over any [`Connection`]
//!   (`Content-Length`, chunked and read-to-close bodies), [`http::fetch`]
//!   which connects, sends and reads in one call, and [`http::get`] which
//!   follows redirects, each within one time limit as a whole.
//! * [`ws`]: a WebSocket client (RFC 6455): the opening handshake, masked
//!   frames, fragmented messages, pings answered automatically, the
//!   closing handshake, and a non-blocking [`ws::WebSocket::poll`] for
//!   event loops.
//! * [`Connection`]: the byte stream underneath — a plain
//!   [`net::TcpStream`] or a TLS stream (`tls`, certificates checked
//!   against the Mozilla roots); [`connect`] picks by scheme.
//!
//! Nothing here panics on what a server sends: malformed responses and
//! frames become [`WebError`]s.

#![no_std]

extern crate alloc;

pub mod http;
pub mod url;
pub mod ws;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use core::fmt;

use abi::RawHandle;
use net::{NetError, TcpStream};
use runtime::sync::Mutex;
use runtime::time::Duration;
use tls::{ClientConfig, TlsError, TlsStream};

pub use url::Url;

/// What went wrong talking to a web server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebError {
    /// The network failed (connection refused, reset, no route, ...).
    Net(NetError),
    /// The secure connection failed (certificate, handshake, ...).
    Tls(String),
    /// The URL is malformed or uses an unsupported scheme.
    BadUrl,
    /// The server broke the protocol.
    Protocol(String),
    /// The server answered with an unexpected HTTP status.
    Status {
        code: u16,
        body: String,
    },
    /// A response or message exceeded the size limit.
    TooLarge,
    /// The server redirected more times than a client follows.
    TooManyRedirects,
    /// The connection was closed.
    Closed,
    /// A secure connection ended without the server closing it: what came
    /// before is genuine, but may have been cut short.
    Truncated,
    TimedOut,
}

impl From<NetError> for WebError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::TimedOut => WebError::TimedOut,
            e => WebError::Net(e),
        }
    }
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebError::Net(e) => write!(f, "{e}"),
            WebError::Tls(e) => f.write_str(e),
            WebError::BadUrl => f.write_str("invalid web address"),
            WebError::Protocol(what) => write!(f, "the server sent an invalid response ({what})"),
            WebError::Status { code, body } => {
                write!(f, "the server answered {code}")?;
                if !body.is_empty() {
                    write!(f, ": {}", body.chars().take(200).collect::<String>())?;
                }
                Ok(())
            }
            WebError::TooLarge => f.write_str("the response is too large"),
            WebError::TooManyRedirects => f.write_str("the server redirected too many times"),
            WebError::Closed => f.write_str("the connection was closed"),
            WebError::Truncated => f.write_str("the secure connection ended early: the response may be incomplete"),
            WebError::TimedOut => f.write_str("the server did not answer in time"),
        }
    }
}

/// A byte stream to a server.
pub trait Connection {
    /// Blocking read (up to the read timeout); `Ok(0)` at the end of the
    /// stream (over TLS: once the server closed it, else
    /// [`WebError::Truncated`]).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError>;
    /// Non-blocking read: `Ok(None)` if nothing is available yet.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError>;
    /// Blocking write (up to the write timeout).
    fn write_all(&mut self, data: &[u8]) -> Result<(), WebError>;
    /// Non-blocking write: how much of `data` went out now (over TLS, all
    /// of it: what the connection does not take now waits, in order, for
    /// the next write).
    fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError>;
    /// How long a blocking read may wait (`None`: forever).
    fn set_read_timeout(&mut self, timeout: Option<Duration>);
    /// How long a blocking write may wait (`None`: forever).
    fn set_write_timeout(&mut self, timeout: Option<Duration>);
    /// The kernel object that becomes readable when data arrives (for
    /// event loops).
    fn handle(&self) -> RawHandle;
}

impl Connection for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError> {
        TcpStream::read(self, buf).map_err(WebError::from)
    }

    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError> {
        TcpStream::try_read(self, buf).map_err(WebError::from)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), WebError> {
        TcpStream::write_all(self, data).map_err(WebError::from)
    }

    fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError> {
        TcpStream::try_write(self, data).map_err(WebError::from)
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        TcpStream::set_read_timeout(self, timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        TcpStream::set_write_timeout(self, timeout);
    }

    fn handle(&self) -> RawHandle {
        TcpStream::handle(self)
    }
}

fn tls_error(e: TlsError) -> WebError {
    match e {
        TlsError::TimedOut => WebError::TimedOut,
        TlsError::Closed => WebError::Closed,
        TlsError::Truncated => WebError::Truncated,
        e => WebError::Tls(e.to_string()),
    }
}

impl Connection for TlsStream<TcpStream> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError> {
        TlsStream::read(self, buf).map_err(tls_error)
    }

    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError> {
        TlsStream::try_read(self, buf).map_err(tls_error)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), WebError> {
        TlsStream::write_all(self, data).map_err(tls_error)
    }

    fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError> {
        TlsStream::try_write(self, data).map(|_| data.len()).map_err(tls_error)
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.transport_mut().set_read_timeout(timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.transport_mut().set_write_timeout(timeout);
    }

    fn handle(&self) -> RawHandle {
        self.transport().handle()
    }
}

/// A connection to a web server, plain or encrypted.
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Conn {
    fn stream(&mut self) -> &mut dyn Connection {
        match self {
            Conn::Plain(s) => s,
            Conn::Tls(s) => &mut **s,
        }
    }
}

impl Connection for Conn {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError> {
        self.stream().read(buf)
    }

    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError> {
        self.stream().try_read(buf)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), WebError> {
        self.stream().write_all(data)
    }

    fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError> {
        self.stream().try_write(data)
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.stream().set_read_timeout(timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.stream().set_write_timeout(timeout);
    }

    fn handle(&self) -> RawHandle {
        match self {
            Conn::Plain(s) => Connection::handle(s),
            Conn::Tls(s) => Connection::handle(&**s),
        }
    }
}

/// What HTTPS and secure WebSocket connections trust and offer, built once.
static TLS_CONFIG: Mutex<Option<ClientConfig>> = Mutex::new(None);

fn tls_config() -> ClientConfig {
    TLS_CONFIG.lock().get_or_insert_with(|| ClientConfig::with_alpn(&[b"http/1.1"])).clone()
}

/// Connects to the server of `url` (TCP, then TLS for `https`/`wss`), all
/// within about `timeout`.
pub fn connect(url: &Url, timeout: Duration) -> Result<Conn, WebError> {
    let start = runtime::time::Instant::now();
    let mut tcp = TcpStream::connect_host(&url.host, url.port, timeout)?;
    // Requests and WebSocket messages are small and latency matters.
    let _ = tcp.set_nodelay(true);
    if !url.secure() {
        return Ok(Conn::Plain(tcp));
    }
    // The handshake gets what is left of the time (at least a second).
    let left = timeout.saturating_sub(start.elapsed()).max(Duration::from_secs(1));
    let tls = tls::connect_over(tcp, &url.host, left, &tls_config()).map_err(tls_error)?;
    Ok(Conn::Tls(Box::new(tls)))
}

/// Random bytes for WebSocket keys and masks.
pub(crate) fn random_bytes(buf: &mut [u8]) {
    #[cfg(not(test))]
    runtime::object::random_bytes(buf);
    #[cfg(test)]
    {
        use core::sync::atomic::{AtomicU64, Ordering};
        static STATE: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);
        for b in buf {
            let mut x = STATE.load(Ordering::Relaxed);
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            STATE.store(x, Ordering::Relaxed);
            *b = x as u8;
        }
    }
}

#[cfg(test)]
extern crate std;

/// An in-memory connection for tests: reads come from `input`, writes go
/// to `output`.
#[cfg(test)]
pub(crate) mod testconn {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    #[derive(Default)]
    pub struct TestConn {
        pub input: VecDeque<u8>,
        pub output: Vec<u8>,
        /// Hand out at most this many bytes per read (exercises partial
        /// reads).
        pub chunk: usize,
        pub eof: bool,
        /// Bytes the server still takes before it stops reading (`None`:
        /// no limit); a blocking write beyond it times out.
        pub write_room: Option<usize>,
        /// Read timeouts set, last first.
        pub read_timeouts: Vec<Option<Duration>>,
    }

    impl TestConn {
        pub fn new(input: &[u8], chunk: usize) -> TestConn {
            TestConn {
                input: input.iter().copied().collect(),
                output: Vec::new(),
                chunk: chunk.max(1),
                eof: true,
                write_room: None,
                read_timeouts: Vec::new(),
            }
        }
    }

    impl Connection for TestConn {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError> {
            match self.try_read(buf)? {
                Some(n) => Ok(n),
                None if self.eof => Ok(0),
                None => Err(WebError::TimedOut),
            }
        }

        fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError> {
            if self.input.is_empty() {
                return Ok(if self.eof { Some(0) } else { None });
            }
            let n = buf.len().min(self.chunk).min(self.input.len());
            for b in buf.iter_mut().take(n) {
                *b = self.input.pop_front().unwrap_or(0);
            }
            Ok(Some(n))
        }

        fn write_all(&mut self, data: &[u8]) -> Result<(), WebError> {
            if self.try_write(data)? < data.len() {
                return Err(WebError::TimedOut);
            }
            Ok(())
        }

        fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError> {
            let n = data.len().min(self.write_room.unwrap_or(usize::MAX));
            if let Some(room) = &mut self.write_room {
                *room -= n;
            }
            self.output.extend_from_slice(&data[..n]);
            Ok(n)
        }

        fn set_read_timeout(&mut self, timeout: Option<Duration>) {
            self.read_timeouts.insert(0, timeout);
        }

        fn set_write_timeout(&mut self, _timeout: Option<Duration>) {}

        fn handle(&self) -> RawHandle {
            0
        }
    }
}
