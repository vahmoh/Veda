// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Addresses as people read them, and what they say of a page. The engine
//! parses addresses as the web does; this only looks at their parts.

use alloc::string::String;

/// The browser's own pages: the new tab's.
pub const NEW_TAB: &str = "about:newtab";

/// `url`'s scheme, as written (`https` of `https://example.com`), if it
/// starts with one.
pub fn scheme(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

/// The host of a hierarchical address (`scheme://user@host:port/path`):
/// `host`, without its port, or an IPv6 literal with its brackets.
pub fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if authority.starts_with('[') {
        &authority[..authority.find(']').map_or(authority.len(), |i| i + 1)]
    } else {
        authority.split_once(':').map_or(authority, |(h, _)| h)
    };
    (!host.is_empty()).then_some(host)
}

/// Whether `url` is a page of the network's (HTTP, with or without TLS).
pub fn is_web(url: &str) -> bool {
    scheme(url).is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
}

/// What names a page that has no title: its host without `www.`, or else
/// the address.
pub fn short(url: &str) -> String {
    if url == NEW_TAB || url.is_empty() {
        return String::from("New tab");
    }
    match host(url) {
        Some(h) => String::from(h.strip_prefix("www.").unwrap_or(h)),
        None => String::from(url),
    }
}

/// `url` as the address bar shows it while it is not being edited: without
/// `https://`, which the padlock says, nor a lone `/` after the host.
pub fn display(url: &str) -> String {
    if url == NEW_TAB {
        return String::new();
    }
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let trimmed = match rest.find('/') {
        Some(i) if i + 1 == rest.len() && !rest[..i].contains(['?', '#']) => &rest[..i],
        _ => rest,
    };
    String::from(trimmed)
}

/// `text` for a URL's query, as a form sends it: spaces as `+`, and every
/// byte but letters, digits and `-._~` as `%XX`.
pub fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            b => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 15) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_of_addresses() {
        assert_eq!(scheme("https://example.com"), Some("https"));
        assert_eq!(scheme("about:blank"), Some("about"));
        assert_eq!(scheme("example.com:8080"), Some("example.com"));
        assert_eq!(scheme("1http://x"), None);
        assert_eq!(scheme("no scheme"), None);
        assert_eq!(host("https://user:pw@www.Example.com:8443/a?b#c"), Some("www.Example.com"));
        assert_eq!(host("http://[::1]:8080/"), Some("[::1]"));
        assert_eq!(host("http://localhost?x"), Some("localhost"));
        assert_eq!(host("about:blank"), None);
        assert!(is_web("HTTPS://a.b") && is_web("http://a") && !is_web("file:///a") && !is_web("about:blank"));
    }

    #[test]
    fn what_people_read() {
        assert_eq!(short("https://www.example.com/page"), "example.com");
        assert_eq!(short("about:blank"), "about:blank");
        assert_eq!(short(NEW_TAB), "New tab");
        assert_eq!(display("https://example.com/"), "example.com");
        assert_eq!(display("https://example.com/a/"), "example.com/a/");
        assert_eq!(display("http://example.com/"), "http://example.com/");
        assert_eq!(display(NEW_TAB), "");
        assert_eq!(encode_query("a b&c=d/é"), "a+b%26c%3Dd%2F%C3%A9");
    }
}
