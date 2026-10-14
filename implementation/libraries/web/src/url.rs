// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! URLs for the web clients: `http`, `https`, `ws` and `wss`, and the
//! references a server redirects to (RFC 3986).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::WebError;

/// A parsed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `http`, `https`, `ws` or `wss` (lower case).
    pub scheme: String,
    /// Host name or address (IPv6 without brackets).
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    /// Parses `scheme://host[:port][/path][?query][#fragment]`. The fragment
    /// is dropped: it is never sent to a server. User information is not
    /// accepted (these clients have no use for it, and it hides the host
    /// from whoever reads the URL).
    pub fn parse(text: &str) -> Result<Url, WebError> {
        let (scheme, rest) = text.split_once("://").ok_or(WebError::BadUrl)?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            _ => return Err(WebError::BadUrl),
        };
        let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(WebError::BadUrl);
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6.split_once(']').ok_or(WebError::BadUrl)?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| WebError::BadUrl)?,
                None if after.is_empty() => default_port,
                None => return Err(WebError::BadUrl),
            };
            (h, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse().map_err(|_| WebError::BadUrl)?),
                None => (authority, default_port),
            }
        };
        if host.is_empty()
            || port == 0
            || host.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
        {
            return Err(WebError::BadUrl);
        }
        let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
        check_path(&path)?;
        Ok(Url { scheme, host: host.to_ascii_lowercase(), port, path })
    }

    /// The URL that `reference` (a redirect's `Location`, say) names
    /// relative to this one, resolved as RFC 3986 says (section 5.2): a URL
    /// of its own, `//host/path` (this scheme), `/path` (this server), a
    /// path relative to this one's directory, or `?query` (this path). Dot
    /// segments are removed; the fragment is dropped.
    pub fn join(&self, reference: &str) -> Result<Url, WebError> {
        let reference = reference.trim();
        let reference = reference.split_once('#').map_or(reference, |(before, _)| before);
        if has_scheme(reference) {
            return Url::parse(reference);
        }
        if let Some(network_path) = reference.strip_prefix("//") {
            return Url::parse(&format!("{}://{network_path}", self.scheme));
        }
        let (base_path, base_query) = match self.path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (self.path.as_str(), None),
        };
        let (path, query) = match reference.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (reference, None),
        };
        let (path, query) = if path.is_empty() {
            (base_path.to_string(), query.or(base_query))
        } else if path.starts_with('/') {
            (remove_dot_segments(path), query)
        } else {
            let directory = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
            (remove_dot_segments(&format!("{directory}{path}")), query)
        };
        let path = match query {
            Some(query) => format!("{path}?{query}"),
            None => path,
        };
        check_path(&path)?;
        Ok(Url { path, ..self.clone() })
    }

    /// True for `https` and `wss`.
    pub fn secure(&self) -> bool {
        matches!(self.scheme.as_str(), "https" | "wss")
    }

    /// The `Host` header value (the port only when it is not the default).
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.secure() { 443 } else { 80 };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }

    /// Appends `key=value` to the query string, percent-encoding both.
    pub fn with_query(mut self, key: &str, value: &str) -> Url {
        self.path.push(if self.path.contains('?') { '&' } else { '?' });
        self.path.push_str(&encode_component(key));
        self.path.push('=');
        self.path.push_str(&encode_component(value));
        self
    }
}

impl core::fmt::Display for Url {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}://{}{}", self.scheme, self.host_header(), self.path)
    }
}

/// A request target may not hold white space or control characters (they
/// would break the request line).
fn check_path(path: &str) -> Result<(), WebError> {
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) { Err(WebError::BadUrl) } else { Ok(()) }
}

/// Whether a reference starts with a scheme (`ALPHA *( ALPHA / DIGIT / "+"
/// / "-" / "." ) ":"`), which makes it a URL of its own.
fn has_scheme(reference: &str) -> bool {
    let scheme = reference.split_once(':').map_or("", |(scheme, _)| scheme);
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// An absolute path without its `.` and `..` segments (RFC 3986, section
/// 5.2.4); `..` above the root stays at the root.
fn remove_dot_segments(path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut segments = path.strip_prefix('/').unwrap_or(path).split('/').peekable();
    while let Some(segment) = segments.next() {
        let last = segments.peek().is_none();
        match segment {
            "." | ".." => {
                if segment == ".." {
                    kept.pop();
                }
                // A path ending in a dot segment names a directory.
                if last {
                    kept.push("");
                }
            }
            segment => kept.push(segment),
        }
    }
    format!("/{}", kept.join("/"))
}

/// What a URL component keeps as it is: RFC 3986's unreserved characters.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Percent-encodes everything except unreserved characters (RFC 3986).
pub fn encode_component(s: &str) -> String {
    utf8_percent_encode(s, UNRESERVED).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn parses_urls() {
        let u = Url::parse("wss://agent.deepgram.com/v1/agent/converse").unwrap();
        assert_eq!(
            (u.scheme.as_str(), u.host.as_str(), u.port, u.path.as_str()),
            ("wss", "agent.deepgram.com", 443, "/v1/agent/converse")
        );
        assert!(u.secure());
        assert_eq!(u.host_header(), "agent.deepgram.com");
        let u = Url::parse("http://10.0.2.2:8080").unwrap();
        assert_eq!((u.port, u.path.as_str(), u.host_header().as_str()), (8080, "/", "10.0.2.2:8080"));
        let u = Url::parse("HTTP://[::1]:81?x=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("::1", 81, "/?x=1"));
        assert_eq!(u.host_header(), "[::1]:81");
        assert_eq!(u.to_string(), "http://[::1]:81/?x=1");
        let u = Url::parse("http://[2001:db8::1]:8080/x").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.host_header().as_str()), ("2001:db8::1", 8080, "[2001:db8::1]:8080"));
        // The fragment is not part of what is fetched.
        assert_eq!(Url::parse("http://example.com:8080/a/b?q=1#frag").unwrap().path, "/a/b?q=1");
        assert_eq!(Url::parse("http://h#f").unwrap().path, "/");
        for bad in [
            "ftp://x",
            "http://",
            "http://a b/",
            "http://u@h/",
            "http://h:99999/",
            "http://h/a b",
            "nourl",
            "http://h:0/",
        ] {
            assert_eq!(Url::parse(bad), Err(WebError::BadUrl), "{bad}");
        }
    }

    #[test]
    fn resolves_references() {
        // RFC 3986, section 5.4: its examples whose results are HTTP URLs.
        let base = Url::parse("http://a/b/c/d;p?q").unwrap();
        for (reference, target) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q"),
            ("g#s", "http://a/b/c/g"),
            (";x", "http://a/b/c/;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("https://other.example:8443/x", "https://other.example:8443/x"),
        ] {
            assert_eq!(base.join(reference).unwrap().to_string(), target, "{reference}");
        }
        assert_eq!(base.join("ftp://a/"), Err(WebError::BadUrl));
        assert_eq!(base.join("g h"), Err(WebError::BadUrl));
        let secure = Url::parse("https://h:8443/a/b").unwrap();
        assert_eq!(secure.join("//other/x").unwrap().to_string(), "https://other/x");
        assert_eq!(secure.join("c").unwrap().to_string(), "https://h:8443/a/c");
    }

    #[test]
    fn encodes_query_components() {
        let u = Url::parse("https://api.deepgram.com/v1/speak")
            .unwrap()
            .with_query("model", "aura-2-thalia-en")
            .with_query("q", "a b&c/é~-._");
        assert_eq!(u.path, "/v1/speak?model=aura-2-thalia-en&q=a%20b%26c%2F%C3%A9~-._");
    }
}
