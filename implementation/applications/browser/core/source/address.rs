// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the address bar's text means: an address to go to, or words to
//! search for.
//!
//! * An address with its scheme (`https://`, `file:`, `about:`, `data:`)
//!   goes as typed, spaces in its path or a data URL's content too (the
//!   engine escapes them); `javascript:` addresses are searched for
//!   instead, as a page's script typed in the address bar is a trick to run
//!   it there.
//! * A host, with or without a port and a path (`example.com/a`,
//!   `localhost:8080`, `192.168.1.1`, `[::1]`), is a web address: over
//!   HTTPS, but for this machine, a network's own names (no dot) and bare
//!   addresses, which are HTTP's (local devices seldom have certificates).
//! * Anything else (words, a question) is searched for.

use alloc::format;
use alloc::string::{String, ToString};

use crate::search::Engine;
use crate::url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meaning {
    /// An address.
    Go(String),
    /// Words.
    Search(String),
}

/// Schemes an address may be typed with.
const SCHEMES: [&str; 6] = ["http", "https", "file", "about", "data", "view-source"];

/// What `text` means; `None` for blank text.
pub fn interpret(text: &str) -> Option<Meaning> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(scheme) = url::scheme(text)
        && SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme))
        && whitespace_fits(scheme, &text[scheme.len() + 1..])
    {
        return Some(Meaning::Go(text.to_string()));
    }
    if text.contains(char::is_whitespace) {
        return Some(Meaning::Search(text.to_string()));
    }
    match host_kind(text) {
        Some(Host::Local) => Some(Meaning::Go(format!("http://{text}"))),
        Some(Host::Public) => Some(Meaning::Go(format!("https://{text}"))),
        None => Some(Meaning::Search(text.to_string())),
    }
}

/// The address `text` goes to: its own, or `engine`'s results for it.
pub fn resolve(text: &str, engine: &Engine) -> Option<String> {
    Some(match interpret(text)? {
        Meaning::Go(address) => address,
        Meaning::Search(words) => engine.results(&words),
    })
}

/// Whether an address of `scheme` (`rest` following its colon) is one to
/// go to with the whitespace it has, if any: a data URL's content may have
/// it, and so may a web or file address's path and query, its host whole;
/// words after `about:` or `http:` are a search.
fn whitespace_fits(scheme: &str, rest: &str) -> bool {
    if !rest.contains(char::is_whitespace) {
        return true;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "data" => true,
        "about" => false,
        "view-source" => url::scheme(rest).is_some_and(|inner| {
            !inner.eq_ignore_ascii_case("view-source") && whitespace_fits(inner, &rest[inner.len() + 1..])
        }),
        // http, https and file: a host without any.
        _ => rest.strip_prefix("//").is_some_and(|after| {
            let authority = &after[..after.find(['/', '?', '#']).unwrap_or(after.len())];
            !authority.contains(char::is_whitespace)
        }),
    }
}

enum Host {
    /// This machine, a network's own name, a bare address.
    Local,
    Public,
}

/// What kind of host `text` (`host[:port][/path...]`) starts with, if it
/// starts with one.
fn host_kind(text: &str) -> Option<Host> {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = &text[..end];
    if let Some(rest) = authority.strip_prefix('[') {
        // An IPv6 address, with its port perhaps.
        let (address, after) = rest.split_once(']')?;
        let port_ok = after.is_empty() || after.strip_prefix(':').is_some_and(is_port);
        return (port_ok && address.contains(':') && address.chars().all(|c| c.is_ascii_hexdigit() || c == ':'))
            .then_some(Host::Local);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if port.is_some_and(|p| !is_port(p)) || host.is_empty() {
        return None;
    }
    if host.eq_ignore_ascii_case("localhost") || is_ipv4(host) {
        return Some(Host::Local);
    }
    let labels: alloc::vec::Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    if !labels.iter().all(label_ok) {
        return None;
    }
    match labels.len() {
        // A name of the network's own, with a port to say it is a host.
        1 => port.map(|_| Host::Local),
        // A top-level domain is letters (or an internationalized one).
        _ => {
            let top = labels[labels.len() - 1];
            let letters = top.chars().all(|c| c.is_alphabetic()) && top.chars().count() >= 2;
            (letters || top.starts_with("xn--")).then_some(Host::Public)
        }
    }
}

fn is_port(p: &str) -> bool {
    !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| n <= 65535)
}

fn is_ipv4(host: &str) -> bool {
    let parts: alloc::vec::Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::engine;

    fn go(s: &str) -> Option<Meaning> {
        Some(Meaning::Go(s.to_string()))
    }

    fn search(s: &str) -> Option<Meaning> {
        Some(Meaning::Search(s.to_string()))
    }

    #[test]
    fn addresses_with_schemes_go_as_typed() {
        assert_eq!(interpret("https://example.com/a?b=c"), go("https://example.com/a?b=c"));
        assert_eq!(interpret("about:blank"), go("about:blank"));
        assert_eq!(interpret("file:///home/user/a.html"), go("file:///home/user/a.html"));
        assert_eq!(interpret("data:text/html,<p>hi"), go("data:text/html,<p>hi"));
        // Spaces in a path, a query or a data URL's content, which the
        // engine escapes.
        assert_eq!(interpret("  https://example.com/a b?q=c d "), go("https://example.com/a b?q=c d"));
        assert_eq!(interpret("file:///home/user/My Pages/a.html"), go("file:///home/user/My Pages/a.html"));
        assert_eq!(interpret("data:text/html,<p>hello world"), go("data:text/html,<p>hello world"));
        assert_eq!(interpret("view-source:https://example.com/a b"), go("view-source:https://example.com/a b"));
        // Never a script, typed.
        assert_eq!(interpret("javascript:alert(1)"), search("javascript:alert(1)"));
    }

    #[test]
    fn hosts_are_web_addresses() {
        assert_eq!(interpret("example.com"), go("https://example.com"));
        assert_eq!(interpret("en.wikipedia.org/wiki/Veda"), go("https://en.wikipedia.org/wiki/Veda"));
        assert_eq!(interpret("example.com:8443/x?y#z"), go("https://example.com:8443/x?y#z"));
        assert_eq!(interpret("bücher.de"), go("https://bücher.de"));
        assert_eq!(interpret("localhost:8080"), go("http://localhost:8080"));
        assert_eq!(interpret("localhost"), go("http://localhost"));
        assert_eq!(interpret("192.168.1.1/admin"), go("http://192.168.1.1/admin"));
        assert_eq!(interpret("10.0.2.2:8080/second"), go("http://10.0.2.2:8080/second"));
        assert_eq!(interpret("[::1]:3000"), go("http://[::1]:3000"));
        assert_eq!(interpret("router:80"), go("http://router:80"));
    }

    #[test]
    fn the_rest_is_searched_for() {
        assert_eq!(interpret("   "), None);
        assert_eq!(interpret("veda os"), search("veda os"));
        assert_eq!(interpret("hello"), search("hello"));
        assert_eq!(interpret("1.5"), search("1.5"));
        assert_eq!(interpret("3.14159"), search("3.14159"));
        assert_eq!(interpret("example.com:99999"), search("example.com:99999"));
        assert_eq!(interpret("-bad-.com"), search("-bad-.com"));
        assert_eq!(interpret("what is 2+2?"), search("what is 2+2?"));
        // Words after a scheme, or a host with a space in it.
        assert_eq!(interpret("http: the protocol"), search("http: the protocol"));
        assert_eq!(interpret("about: this"), search("about: this"));
        assert_eq!(interpret("https://exa mple.com/"), search("https://exa mple.com/"));
        assert_eq!(interpret("view-source:view-source:a b"), search("view-source:view-source:a b"));
        assert_eq!(resolve("veda os", engine("duckduckgo")).unwrap(), "https://duckduckgo.com/?q=veda+os");
        assert_eq!(resolve("example.org", engine("google")).unwrap(), "https://example.org");
    }
}
