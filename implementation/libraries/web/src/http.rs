// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP/1.1 requests over a [`Connection`].
//!
//! One request per connection (`Connection: close`), which is all the
//! system's clients need (API calls and downloads). Response heads and the
//! sizes of chunks are parsed by `httparse`; bodies may be
//! `Content-Length`, chunked or read-to-close (over TLS only if the server
//! closes the secure session: anything else may be cut short), and are
//! limited to a caller-chosen size. [`fetch`] makes one request; [`get`]
//! follows redirects; each finishes within its time limit as a whole, a
//! server that trickles its answer included.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use rt::time::Duration;

use crate::{Connection, Url, WebError};

/// Largest response head.
const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;
/// Longest line that gives a chunk's size (with its extensions).
const MAX_CHUNK_LINE: usize = 1024;
/// Redirects [`get`] follows.
const MAX_REDIRECTS: usize = 5;

/// A response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// The status's reason phrase (`OK`, `Not Found`, ...), which may be
    /// empty.
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header called `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The body as text (invalid UTF-8 replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `Ok(self)` for a 2xx status, otherwise [`WebError::Status`].
    pub fn ok(self) -> Result<Response, WebError> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            Err(WebError::Status { code: self.status, body: self.text() })
        }
    }
}

/// When a request must be done, on a clock (the system's, or a test's).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadline {
    at: u64,
    clock: fn() -> u64,
}

impl Deadline {
    /// `timeout` from now on the system's clock.
    fn after(timeout: Duration) -> Deadline {
        Deadline { at: rt::time::deadline_after(timeout), clock: rt::time::now_ns }
    }

    /// The time left, or `TimedOut`.
    fn left(&self) -> Result<Duration, WebError> {
        let now = (self.clock)();
        if now >= self.at { Err(WebError::TimedOut) } else { Ok(Duration::from_nanos(self.at - now)) }
    }
}

/// Writes a request. `headers` must not contain line breaks (they are
/// rejected); `Host`, `Connection` and `Content-Length` are added, and
/// `User-Agent` and `Accept-Encoding: identity` unless `headers` has them
/// (nothing here decompresses a body).
pub fn write_request<C: Connection>(
    conn: &mut C,
    method: &str,
    url: &Url,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), WebError> {
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {}\r\n", url.path, url.host_header());
    let (mut has_agent, mut has_encoding) = (false, false);
    for (k, v) in headers {
        if k.is_empty() || [k, v].iter().any(|s| s.contains(['\r', '\n'])) {
            return Err(WebError::Protocol("invalid request header".into()));
        }
        has_agent |= k.eq_ignore_ascii_case("user-agent");
        has_encoding |= k.eq_ignore_ascii_case("accept-encoding");
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_agent {
        head.push_str(concat!("User-Agent: Veda/", env!("CARGO_PKG_VERSION"), "\r\n"));
    }
    if !has_encoding {
        head.push_str("Accept-Encoding: identity\r\n");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    conn.write_all(head.as_bytes())?;
    if !body.is_empty() {
        conn.write_all(body)?;
    }
    Ok(())
}

/// Reads through a connection with a buffer of bytes that arrived early.
pub(crate) struct Reader<'a, C: Connection> {
    pub conn: &'a mut C,
    pub buf: Vec<u8>,
    /// When all reading must be done, if it must.
    pub deadline: Option<Deadline>,
}

impl<C: Connection> Reader<'_, C> {
    /// Reads more into the buffer; `Ok(false)` at the end of the stream.
    pub fn fill(&mut self) -> Result<bool, WebError> {
        if let Some(d) = self.deadline {
            self.conn.set_read_timeout(Some(d.left()?));
        }
        let mut chunk = [0u8; 4096];
        let n = self.conn.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(n > 0)
    }

    /// Reads until the buffer holds a complete head; returns its length.
    pub fn read_head(&mut self) -> Result<usize, WebError> {
        loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(i + 4);
            }
            if self.buf.len() > MAX_HEAD {
                return Err(WebError::TooLarge);
            }
            if !self.fill()? {
                return Err(if self.buf.is_empty() {
                    WebError::Closed
                } else {
                    WebError::Protocol("connection closed in the response head".into())
                });
            }
        }
    }
}

/// A parsed response head.
pub(crate) struct Head {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
}

/// Parses a response head.
pub(crate) fn parse_head(bytes: &[u8]) -> Result<Head, WebError> {
    let mut raw = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut resp = httparse::Response::new(&mut raw);
    match resp.parse(bytes) {
        Ok(httparse::Status::Complete(_)) => {
            let status = resp.code.ok_or_else(|| WebError::Protocol("no status".into()))?;
            let reason = resp.reason.unwrap_or_default().to_string();
            let headers = resp
                .headers
                .iter()
                .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).trim().to_string()))
                .collect();
            Ok(Head { status, reason, headers })
        }
        Ok(httparse::Status::Partial) => Err(WebError::Protocol("incomplete head".into())),
        Err(e) => Err(WebError::Protocol(format!("{e}"))),
    }
}

/// Reads a response to a `method` request (bodies at most `max_body`
/// bytes), each read within the connection's read timeout.
pub fn read_response<C: Connection>(conn: &mut C, method: &str, max_body: usize) -> Result<Response, WebError> {
    read_response_until(conn, method, max_body, None)
}

/// Reads a response, all of it by `deadline` if there is one.
pub(crate) fn read_response_until<C: Connection>(
    conn: &mut C,
    method: &str,
    max_body: usize,
    deadline: Option<Deadline>,
) -> Result<Response, WebError> {
    let mut r = Reader { conn, buf: Vec::new(), deadline };
    loop {
        let head_len = r.read_head()?;
        let Head { status, reason, headers } = parse_head(&r.buf[..head_len])?;
        r.buf.drain(..head_len);
        // Interim responses (100 Continue) precede the real one.
        if (100..200).contains(&status) {
            continue;
        }
        let find = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
        let no_body = method == "HEAD" || status == 204 || status == 304;
        let body = if no_body {
            Vec::new()
        } else if find("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
            read_chunked(&mut r, max_body)?
        } else if let Some(len) = find("content-length") {
            let len: usize = len.parse().map_err(|_| WebError::Protocol("bad Content-Length".into()))?;
            if len > max_body {
                return Err(WebError::TooLarge);
            }
            while r.buf.len() < len {
                if !r.fill()? {
                    return Err(WebError::Protocol("connection closed in the body".into()));
                }
            }
            r.buf.truncate(len);
            core::mem::take(&mut r.buf)
        } else {
            while r.fill()? {
                if r.buf.len() > max_body {
                    return Err(WebError::TooLarge);
                }
            }
            core::mem::take(&mut r.buf)
        };
        return Ok(Response { status, reason, headers, body });
    }
}

/// Reads a chunked body (RFC 9112, section 7.1). `httparse` parses each
/// chunk's size (16 hexadecimal digits at most, and at least one: it would
/// take a line without digits for 0, the end); the sizes add up without
/// overflowing, past `max_body` being refused, and each chunk must end its
/// line.
fn read_chunked<C: Connection>(r: &mut Reader<'_, C>, max_body: usize) -> Result<Vec<u8>, WebError> {
    let bad = |what: &str| WebError::Protocol(what.into());
    let mut body = Vec::new();
    loop {
        let (line_len, size) = loop {
            match httparse::parse_chunk_size(&r.buf) {
                Ok(httparse::Status::Complete(found)) if r.buf[0].is_ascii_hexdigit() => break found,
                Ok(httparse::Status::Partial) if r.buf.len() <= MAX_CHUNK_LINE => {
                    if !r.fill()? {
                        return Err(bad("connection closed in a chunk size"));
                    }
                }
                _ => return Err(bad("bad chunk size")),
            }
        };
        r.buf.drain(..line_len);
        if size == 0 {
            // Trailers and an empty line may follow; the connection closes
            // after them.
            return Ok(body);
        }
        // The chunk, then the line break that ends it.
        let end = usize::try_from(size)
            .ok()
            .filter(|&size| body.len().saturating_add(size) <= max_body)
            .and_then(|size| size.checked_add(2))
            .ok_or(WebError::TooLarge)?;
        while r.buf.len() < end {
            if !r.fill()? {
                return Err(bad("connection closed in a chunk"));
            }
        }
        if &r.buf[end - 2..end] != b"\r\n" {
            return Err(bad("chunk not terminated"));
        }
        body.extend_from_slice(&r.buf[..end - 2]);
        r.buf.drain(..end);
    }
}

/// Connects to the server of `url`, sends one request and reads the
/// response, all within `timeout`.
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
    max_body: usize,
) -> Result<Response, WebError> {
    request(method, &Url::parse(url)?, headers, body, Deadline::after(timeout), max_body)
}

/// Fetches `url` with `GET`, following up to five redirects, to `http` and
/// `https` URLs (relative ones resolved against the URL that redirected).
/// `timeout` bounds the whole of it, redirects included, and `max_body`
/// each body.
pub fn get(url: &str, timeout: Duration, max_body: usize) -> Result<Response, WebError> {
    let mut url = Url::parse(url)?;
    let deadline = Deadline::after(timeout);
    for _ in 0..=MAX_REDIRECTS {
        if !matches!(url.scheme.as_str(), "http" | "https") {
            return Err(WebError::BadUrl);
        }
        let response = request("GET", &url, &[("Accept", "*/*")], &[], deadline, max_body)?;
        match response.header("location") {
            Some(to) if matches!(response.status, 301 | 302 | 303 | 307 | 308) => url = url.join(to)?,
            _ => return Ok(response),
        }
    }
    Err(WebError::TooManyRedirects)
}

/// Connects to the server of `url`, sends one request and reads the
/// response, all by `deadline`.
fn request(
    method: &str,
    url: &Url,
    headers: &[(&str, &str)],
    body: &[u8],
    deadline: Deadline,
    max_body: usize,
) -> Result<Response, WebError> {
    let mut conn = crate::connect(url, deadline.left()?)?;
    conn.set_write_timeout(Some(deadline.left()?));
    write_request(&mut conn, method, url, headers, body)?;
    read_response_until(&mut conn, method, max_body, Some(deadline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testconn::TestConn;

    #[test]
    fn writes_requests() {
        let url = Url::parse("https://api.deepgram.com/v1/models?x=1").unwrap();
        let mut c = TestConn::new(b"", 1);
        write_request(
            &mut c,
            "POST",
            &url,
            &[("Authorization", "Token abc"), ("Content-Type", "application/json")],
            b"{}",
        )
        .unwrap();
        let text = String::from_utf8(c.output).unwrap();
        assert!(text.starts_with("POST /v1/models?x=1 HTTP/1.1\r\nHost: api.deepgram.com\r\n"));
        assert!(text.contains("Authorization: Token abc\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains(concat!("User-Agent: Veda/", env!("CARGO_PKG_VERSION"), "\r\n")));
        assert!(text.contains("Accept-Encoding: identity\r\n"));
        assert!(text.ends_with("Connection: close\r\n\r\n{}"));
        // The caller's own choices stand.
        let mut c = TestConn::new(b"", 1);
        write_request(&mut c, "GET", &url, &[("user-agent", "Test"), ("Accept-Encoding", "gzip")], b"").unwrap();
        let text = String::from_utf8(c.output).unwrap();
        assert!(!text.contains("Veda/") && !text.contains("identity"));
        let mut c = TestConn::new(b"", 1);
        assert!(write_request(&mut c, "GET", &url, &[("X", "a\r\nInjected: 1")], b"").is_err());
    }

    #[test]
    fn reads_bodies_of_every_kind() {
        for chunk in [1, 3, 64] {
            let mut c = TestConn::new(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b \r\n\r\nhelloEXTRA", chunk);
            let r = read_response(&mut c, "GET", 100).unwrap();
            assert_eq!((r.status, r.body.as_slice(), r.header("x-a")), (200, b"hello".as_slice(), Some("b")));
            assert_eq!(r.reason, "OK");

            let mut c = TestConn::new(
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n4;ext\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n",
                chunk,
            );
            let r = read_response(&mut c, "POST", 100).unwrap();
            assert_eq!((r.status, r.body.as_slice()), (201, b"Wikipedia".as_slice()));

            let mut c = TestConn::new(b"HTTP/1.0 401 Unauthorized\r\n\r\n{\"err\":\"bad key\"}", chunk);
            let r = read_response(&mut c, "GET", 100).unwrap();
            assert_eq!(r.status, 401);
            assert!(matches!(r.ok(), Err(WebError::Status { code: 401, .. })));
        }
    }

    #[test]
    fn rejects_bad_responses() {
        let mut c = TestConn::new(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\nshort", 7);
        assert!(matches!(read_response(&mut c, "GET", 1000), Err(WebError::Protocol(_))));
        let mut c = TestConn::new(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n", 7);
        assert_eq!(read_response(&mut c, "GET", 100), Err(WebError::TooLarge));
        let mut c = TestConn::new(b"garbage\r\n\r\n", 7);
        assert!(matches!(read_response(&mut c, "GET", 100), Err(WebError::Protocol(_))));
        let mut c = TestConn::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 7);
        assert!(matches!(read_response(&mut c, "GET", 100), Err(WebError::Protocol(_))));
        let mut c = TestConn::new(b"", 7);
        assert_eq!(read_response(&mut c, "GET", 100), Err(WebError::Closed));
    }

    #[test]
    fn a_trickling_server_runs_out_of_time() {
        use core::sync::atomic::{AtomicU64, Ordering};
        // A clock that moves a second each time it is read.
        static NOW: AtomicU64 = AtomicU64::new(0);
        fn clock() -> u64 {
            NOW.fetch_add(1_000_000_000, Ordering::Relaxed)
        }
        let deadline = Deadline { at: 10_000_000_000, clock };
        // A byte per read: each read is quick, the whole is not.
        let mut c = TestConn::new(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nslow body...", 1);
        assert_eq!(read_response_until(&mut c, "GET", 1000, Some(deadline)), Err(WebError::TimedOut));
        // Each read gets the time that is left, less and less.
        assert!(c.read_timeouts.len() >= 2);
        assert!(c.read_timeouts.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn read_to_close_bodies_need_a_proper_end() {
        // Over TLS, a connection that just ends may have been cut short.
        struct Cut(TestConn);
        impl Connection for Cut {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, WebError> {
                match self.0.read(buf)? {
                    0 => Err(WebError::Truncated),
                    n => Ok(n),
                }
            }
            fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, WebError> {
                self.0.try_read(buf)
            }
            fn write_all(&mut self, data: &[u8]) -> Result<(), WebError> {
                self.0.write_all(data)
            }
            fn try_write(&mut self, data: &[u8]) -> Result<usize, WebError> {
                self.0.try_write(data)
            }
            fn set_read_timeout(&mut self, _: Option<Duration>) {}
            fn set_write_timeout(&mut self, _: Option<Duration>) {}
            fn handle(&self) -> abi::RawHandle {
                0
            }
        }
        let mut c = Cut(TestConn::new(b"HTTP/1.1 200 OK\r\n\r\nall of it?", 4));
        assert_eq!(read_response(&mut c, "GET", 100), Err(WebError::Truncated));
        // A body of a known length is complete before the end.
        let mut c = Cut(TestConn::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", 4));
        assert_eq!(read_response(&mut c, "GET", 100).unwrap().body, b"ok");
    }

    #[test]
    fn rejects_hostile_chunks() {
        let chunked = |chunks: &[u8]| {
            let mut input = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            input.extend_from_slice(chunks);
            input
        };
        for chunk in [1, 5, 4096] {
            let read =
                |chunks: &[u8], max_body| read_response(&mut TestConn::new(&chunked(chunks), chunk), "GET", max_body);
            // Sizes that would overflow a sum or the buffer's index.
            assert_eq!(read(b"1\r\na\r\nffffffffffffffff\r\nb\r\n0\r\n\r\n", 100), Err(WebError::TooLarge));
            assert_eq!(read(b"fffffffffffffffe\r\n", usize::MAX), Err(WebError::TooLarge));
            assert_eq!(read(b"5\r\nabcde\r\n6\r\nfghijk\r\n0\r\n\r\n", 10), Err(WebError::TooLarge));
            // More than 16 digits, a sign, no digits, a chunk without its
            // line break, a size line that never ends.
            for bad in [
                &b"10000000000000000\r\n"[..],
                b"+5\r\nabcde\r\n0\r\n\r\n",
                b"\r\n",
                b";ext\r\n",
                b" 5\r\nabcde\r\n0\r\n\r\n",
                b"3\r\nabcXY0\r\n\r\n",
            ] {
                assert!(matches!(read(bad, 100), Err(WebError::Protocol(_))), "{}", String::from_utf8_lossy(bad));
            }
            assert!(matches!(read(&[b'1'; 2000], 100), Err(WebError::Protocol(_))));
            // Extensions and a trailer are allowed; the body is exact.
            let r = read(b"3;name=value\r\nabc\r\n2 \r\nde\r\n0\r\nX-Trailer: 1\r\n\r\n", 5).unwrap();
            assert_eq!(r.body, b"abcde");
        }
    }
}
