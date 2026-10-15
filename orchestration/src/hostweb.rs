// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The host's end of `nettest local`: a web server on this computer that
//! answers `GET /hello` with "hello from the host", so that the scripts
//! of the network cards check DHCP, ping and TCP through QEMU's NAT
//! without the Internet (`nettest local` in a script). nettest reaches it
//! at its network's gateway, which QEMU's NAT makes this computer.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::util::Result;

/// What the server answers, and what nettest looks for.
const HELLO: &str = "hello from the host\n";

/// The server, which stops when this is dropped.
pub struct HostWeb {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl HostWeb {
    /// Listens on a free local port and answers, a thread for each
    /// connection.
    pub fn start() -> Result<HostWeb> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("the host's web server: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("hostweb".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Ok(stream) = stream {
                        std::thread::spawn(move || answer(stream));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(HostWeb { port, stop })
    }

    /// The kernel command line's argument that has nettest check the
    /// network against the server.
    pub fn boot_arg(&self) -> String {
        format!("run=nettest:local,gateway,{}", self.port)
    }
}

impl Drop for HostWeb {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accepting thread.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// Answers one request: `GET /hello`, or else nothing found.
fn answer(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let (status, body) = if head.starts_with(b"GET /hello ") { ("200 OK", HELLO) } else { ("404 Not Found", "") };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 10.0.2.2\r\nUser-Agent: Veda\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn nettest_finds_the_host_saying_hello() {
        let server = HostWeb::start().unwrap();
        let port: u16 = server.boot_arg().rsplit(',').next().unwrap().parse().unwrap();
        assert!(server.boot_arg().starts_with("run=nettest:local,gateway,"));
        let hello = get(port, "/hello");
        assert!(hello.starts_with("HTTP/1.1 200 OK\r\n"), "{hello}");
        assert!(hello.ends_with("\r\n\r\nhello from the host\n"), "{hello}");
        assert!(hello.contains(&format!("Content-Length: {}\r\n", HELLO.len())));
        assert!(get(port, "/").starts_with("HTTP/1.1 404 Not Found\r\n"));
    }
}
