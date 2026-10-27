// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The repositories a system uses, in two layers:
//!
//! * **The system's**, one file each in the system image
//!   (`/system/etc/packages/repositories/NAME.repo`), read at every start:
//!   what the image says, and nothing kept of an image the system started
//!   from before. A newer image may add, change or take away its
//!   repositories, and its own index's key, and every system that starts
//!   from it uses them as they are.
//!
//!   ```text
//!   # The packages of the system image.
//!   title=Veda system image
//!   url=file:///system/packages
//!   key=ed25519:6XZ...=
//!   ```
//!
//!   `key` may be given more than once (while a repository moves to a new
//!   key, its index is signed by both); `enabled=false` leaves the
//!   repository out until the user enables it.
//! * **The user's** ([`UserConfig`]), which the package manager keeps:
//!   the repositories the user added, and which of the system's they
//!   disabled or enabled, by name. Nothing of a system repository but that
//!   is kept, so that the image stays the one source of what its
//!   repositories are.
//!
//! [`merge`] puts the two together: the system's repositories in the order
//! of their names, then the user's in the order they were added. A user's
//! repository whose name a newer image has given one of its own is left
//! out, and said to be: the image's name is the one packages installed from
//! it were recorded by.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::keys::PublicKey;
use crate::name::is_repository_name;
use crate::{limits, printable};

/// The format of the user's configuration this code reads and writes.
pub const USER_FORMAT: u32 = 1;
/// The longest title of a repository.
pub const TITLE: usize = 80;

/// Where a repository's files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Url {
    /// A directory of this system's (`file:///system/packages`).
    File(String),
    /// A web server's (`https://packages.example.org/veda/amd64`), over
    /// TLS or (`http`) not.
    Web { secure: bool, url: String },
}

/// Why a repository's configuration is not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadConfig(pub String);

impl fmt::Display for BadConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where the system image is: a repository there is part of it, read in
/// place.
const SYSTEM_IMAGE: &str = "/system";
/// The services' private directories, which no repository may be in: the
/// package manager would read them for whoever added it.
const PRIVATE: &str = "/home/.private";

impl Url {
    /// Reads a repository's address: `file://` and an absolute path, or
    /// `https://` (or `http://`), a host, perhaps a port, and a path; no
    /// user name, password, query or fragment.
    pub fn parse(text: &str) -> Result<Url, BadConfig> {
        let bad = |why: &str| {
            BadConfig(alloc::format!(
                "'{}' is not a repository's address: {why}",
                text.chars().take(120).collect::<String>()
            ))
        };
        if text.len() > limits::URL || !printable(text) || text.contains(char::is_whitespace) {
            return Err(bad("it is too long, or has spaces or control characters"));
        }
        let text = text.strip_suffix('/').unwrap_or(text);
        if let Some(path) = text.strip_prefix("file://") {
            let plain = path.starts_with('/')
                && path[1..].split('/').all(|c| !c.is_empty() && c != "." && c != "..")
                && !path.contains(['?', '#', '\\']);
            if !plain || path == "/" {
                return Err(bad(
                    "file:// is followed by an absolute path to a directory, such as file:///home/user/repo",
                ));
            }
            if path == PRIVATE || path.starts_with("/home/.private/") {
                return Err(bad("a repository cannot be in a service's private directory"));
            }
            return Ok(Url::File(path.to_string()));
        }
        let (secure, rest) = match (text.strip_prefix("https://"), text.strip_prefix("http://")) {
            (Some(rest), _) => (true, rest),
            (None, Some(rest)) => (false, rest),
            _ => return Err(bad("it begins with https://, http:// or file://")),
        };
        if rest.contains(['@', '?', '#', '\\']) {
            return Err(bad("it has no user name, password, query or fragment"));
        }
        let host = rest.split('/').next().unwrap_or_default();
        let bad_host = || bad("its host is not a name or an address (with a port, perhaps)");
        // An IPv6 address is in brackets (`[::1]:8080`), as its colons are
        // not a port's.
        let (name_ok, port) = match host.strip_prefix('[') {
            Some(after) => {
                let (literal, tail) = after.split_once(']').ok_or_else(bad_host)?;
                let port = if tail.is_empty() { None } else { Some(tail.strip_prefix(':').ok_or_else(bad_host)?) };
                let ok =
                    !literal.is_empty() && literal.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.');
                (ok, port)
            }
            None => {
                let (name, port) = match host.split_once(':') {
                    Some((name, port)) => (name, Some(port)),
                    None => (host, None),
                };
                (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'), port)
            }
        };
        if !name_ok || !port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p > 0)) {
            return Err(bad_host());
        }
        Ok(Url::Web { secure, url: text.to_string() })
    }

    /// Where `file` (a path in the repository) is.
    pub fn join(&self, file: &str) -> String {
        match self {
            Url::File(path) => alloc::format!("{path}/{file}"),
            Url::Web { url, .. } => alloc::format!("{url}/{file}"),
        }
    }

    /// Whether the repository is part of the system image, which the system
    /// trusts as it trusts its own programs: its index need not expire, and
    /// the image it came with is the only one it is checked against.
    pub fn in_system_image(&self) -> bool {
        matches!(self, Url::File(path) if path == SYSTEM_IMAGE || path.starts_with("/system/"))
    }

    /// Whether the repository is fetched over the network.
    pub fn is_web(&self) -> bool {
        matches!(self, Url::Web { .. })
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Url::File(path) => write!(f, "file://{path}"),
            Url::Web { url, .. } => f.write_str(url),
        }
    }
}

impl Serialize for Url {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Url {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Url, D::Error> {
        let text: &str = Deserialize::deserialize(d)?;
        Url::parse(text).map_err(serde::de::Error::custom)
    }
}

/// Which layer a repository comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    System,
    User,
}

/// A repository, as a system uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Its name, which its index must give.
    pub name: String,
    /// What people call it.
    pub title: String,
    pub url: Url,
    /// The keys it is trusted with.
    pub keys: Vec<PublicKey>,
    pub enabled: bool,
    pub origin: Origin,
}

/// Checks a repository's name, title and keys.
fn check(name: &str, title: &str, keys: &[PublicKey]) -> Result<(), BadConfig> {
    if !is_repository_name(name) {
        return Err(BadConfig(alloc::format!(
            "'{name}' is not a repository's name: it takes 1 to 32 lower-case letters, digits and '-'"
        )));
    }
    if title.is_empty() || title.len() > TITLE || !printable(title) {
        return Err(BadConfig(alloc::format!("{name}: its title is one line of 1 to {TITLE} bytes")));
    }
    if keys.is_empty() || keys.len() > limits::KEYS {
        return Err(BadConfig(alloc::format!("{name}: a repository is trusted with 1 to {} keys", limits::KEYS)));
    }
    Ok(())
}

/// Reads the system's repository `name` from its file (`NAME.repo`).
pub fn system_repository(name: &str, text: &str) -> Result<Repository, BadConfig> {
    let bad = |line: usize, why: String| Err(BadConfig(alloc::format!("{name}.repo, line {line}: {why}")));
    let (mut title, mut url, mut keys, mut enabled) = (None, None, Vec::new(), true);
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return bad(i + 1, String::from("each line is KEY=VALUE"));
        };
        let value = value.trim();
        match key.trim() {
            "title" if title.is_none() => title = Some(value.to_string()),
            "url" if url.is_none() => match Url::parse(value) {
                Ok(u) => url = Some(u),
                Err(e) => return bad(i + 1, e.to_string()),
            },
            "key" => match PublicKey::parse(value) {
                Ok(k) if !keys.contains(&k) => keys.push(k),
                Ok(_) => return bad(i + 1, String::from("the key is given twice")),
                Err(e) => return bad(i + 1, e.to_string()),
            },
            "enabled" => match value {
                "true" => enabled = true,
                "false" => enabled = false,
                _ => return bad(i + 1, String::from("enabled is true or false")),
            },
            "title" | "url" => return bad(i + 1, alloc::format!("{} is given twice", key.trim())),
            other => return bad(i + 1, alloc::format!("a repository has no {other}")),
        }
    }
    let url = url.ok_or_else(|| BadConfig(alloc::format!("{name}.repo gives no url")))?;
    let title = title.unwrap_or_else(|| name.to_string());
    check(name, &title, &keys)?;
    Ok(Repository { name: name.to_string(), title, url, keys, enabled, origin: Origin::System })
}

/// A repository the user added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserRepository {
    pub name: String,
    pub title: String,
    pub url: Url,
    pub keys: Vec<PublicKey>,
    pub enabled: bool,
}

/// What the user changed of the repositories.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub format: u32,
    /// The repositories the user added, in the order they did.
    pub added: Vec<UserRepository>,
    /// The system's repositories the user disabled, and those disabled in
    /// the image that the user enabled.
    pub disabled: Vec<String>,
    pub enabled: Vec<String>,
}

/// Why a change to the repositories is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// No repository has that name.
    NotFound(String),
    /// A repository has that name already.
    Exists(String),
    /// The system's repositories are the image's: they are disabled, not
    /// removed.
    SystemRepository(String),
    /// As many repositories as a system uses are there already.
    TooMany,
    /// The repository is not a valid one.
    Bad(BadConfig),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::NotFound(name) => write!(f, "there is no repository called {name}"),
            ConfigError::Exists(name) => write!(f, "a repository called {name} is there already"),
            ConfigError::SystemRepository(name) => {
                write!(f, "{name} is the system image's repository: it can be disabled, not removed")
            }
            ConfigError::TooMany => write!(f, "a system uses at most {} repositories", limits::REPOSITORIES),
            ConfigError::Bad(e) => e.fmt(f),
        }
    }
}

impl UserConfig {
    /// Nothing changed yet.
    pub fn new() -> UserConfig {
        UserConfig { format: USER_FORMAT, ..UserConfig::default() }
    }

    /// Reads the user's configuration as the service keeps it.
    pub fn parse(bytes: &[u8]) -> Result<UserConfig, BadConfig> {
        let config: UserConfig = serde_json::from_slice(bytes).map_err(|e| BadConfig(alloc::format!("{e}")))?;
        if config.format != USER_FORMAT {
            return Err(BadConfig(alloc::format!("the configuration is of format {}", config.format)));
        }
        if config.added.len() > limits::REPOSITORIES
            || config.disabled.len() > limits::REPOSITORIES
            || config.enabled.len() > limits::REPOSITORIES
        {
            return Err(BadConfig(String::from("the configuration names more repositories than a system uses")));
        }
        for (i, r) in config.added.iter().enumerate() {
            check(&r.name, &r.title, &r.keys)?;
            if config.added[..i].iter().any(|o| o.name == r.name) {
                return Err(BadConfig(alloc::format!("{} is added twice", r.name)));
            }
        }
        Ok(config)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        crate::to_json(self)
    }

    /// Adds `repository`, unless a repository of the system's or the
    /// user's has its name.
    pub fn add(&mut self, repository: UserRepository, system: &[Repository]) -> Result<(), ConfigError> {
        check(&repository.name, &repository.title, &repository.keys).map_err(ConfigError::Bad)?;
        if system.iter().any(|r| r.name == repository.name) || self.added.iter().any(|r| r.name == repository.name) {
            return Err(ConfigError::Exists(repository.name));
        }
        if system.len() + self.added.len() >= limits::REPOSITORIES {
            return Err(ConfigError::TooMany);
        }
        self.added.push(repository);
        Ok(())
    }

    /// Removes the repository `name` the user added.
    pub fn remove(&mut self, name: &str, system: &[Repository]) -> Result<(), ConfigError> {
        if system.iter().any(|r| r.name == name) {
            return Err(ConfigError::SystemRepository(name.to_string()));
        }
        let at =
            self.added.iter().position(|r| r.name == name).ok_or_else(|| ConfigError::NotFound(name.to_string()))?;
        self.added.remove(at);
        Ok(())
    }

    /// Enables or disables the repository `name`, the system's or the
    /// user's.
    pub fn set_enabled(&mut self, name: &str, enabled: bool, system: &[Repository]) -> Result<(), ConfigError> {
        if let Some(repository) = system.iter().find(|r| r.name == name) {
            self.disabled.retain(|n| n != name);
            self.enabled.retain(|n| n != name);
            // Only what differs from the image is kept.
            if enabled != repository.enabled {
                if enabled { self.enabled.push(name.to_string()) } else { self.disabled.push(name.to_string()) }
            }
            return Ok(());
        }
        let r =
            self.added.iter_mut().find(|r| r.name == name).ok_or_else(|| ConfigError::NotFound(name.to_string()))?;
        r.enabled = enabled;
        Ok(())
    }
}

/// The repositories in use, as the module's documentation says; and the
/// names of the user's that a system repository of the same name leaves
/// out.
pub fn merge(mut system: Vec<Repository>, user: &UserConfig) -> (Vec<Repository>, Vec<String>) {
    system.sort_by(|a, b| a.name.cmp(&b.name));
    for r in &mut system {
        if user.disabled.contains(&r.name) {
            r.enabled = false;
        } else if user.enabled.contains(&r.name) {
            r.enabled = true;
        }
    }
    let mut shadowed = Vec::new();
    for r in &user.added {
        if system.iter().any(|s| s.name == r.name) {
            shadowed.push(r.name.clone());
            continue;
        }
        system.push(Repository {
            name: r.name.clone(),
            title: r.title.clone(),
            url: r.url.clone(),
            keys: r.keys.clone(),
            enabled: r.enabled,
            origin: Origin::User,
        });
    }
    (system, shadowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::SecretKey;

    fn key(n: u8) -> PublicKey {
        SecretKey::from_seed(&[n; 32]).public()
    }

    fn system(name: &str) -> Repository {
        let text =
            alloc::format!("# {name}\ntitle=The {name} repository\nurl=file:///system/packages\nkey={}\n", key(1));
        system_repository(name, &text).unwrap()
    }

    fn user(name: &str) -> UserRepository {
        UserRepository {
            name: name.to_string(),
            title: name.to_string(),
            url: Url::parse("https://packages.example.org/veda").unwrap(),
            keys: alloc::vec![key(2)],
            enabled: true,
        }
    }

    #[test]
    fn addresses_are_local_directories_or_web_servers() {
        for (text, shown) in [
            ("file:///system/packages", "file:///system/packages"),
            ("file:///home/user/repo/", "file:///home/user/repo"),
            ("https://packages.vedaos.org/stable/amd64", "https://packages.vedaos.org/stable/amd64"),
            ("http://10.0.2.2:8080/packages", "http://10.0.2.2:8080/packages"),
            ("https://[::1]:8443/x", "https://[::1]:8443/x"),
            ("https://[fe80::1]", "https://[fe80::1]"),
        ] {
            assert_eq!(Url::parse(text).unwrap().to_string(), shown, "{text}");
        }
        for bad in [
            "",
            "ftp://x",
            "file://",
            "file:///",
            "file://relative",
            "file:///a/../b",
            "file:///a//b",
            "file:///home/.private/agent",
            "file:///home/.private",
            "https://",
            "https://user:pw@host/x",
            "https://host/x?y",
            "https://host/#x",
            "https://bad_host/x",
            "https://host:0/x",
            "https://host:99999/x",
            "https://host/a b",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad:?}");
        }
        let image = Url::parse("file:///system/packages").unwrap();
        assert!(image.in_system_image() && !image.is_web());
        assert_eq!(image.join("index.vidx"), "/system/packages/index.vidx");
        assert!(!Url::parse("file:///systemd/x").unwrap().in_system_image());
        let web = Url::parse("https://example.org/r").unwrap();
        assert!(web.is_web() && !web.in_system_image());
        assert_eq!(web.join("index.vidx"), "https://example.org/r/index.vidx");
    }

    #[test]
    fn system_repositories_are_files_of_lines() {
        let r = system("main");
        assert_eq!((r.title.as_str(), r.enabled, r.origin), ("The main repository", true, Origin::System));
        let text = alloc::format!("url=file:///system/packages\nkey={}\nkey={}\nenabled=false\n", key(1), key(3));
        let r = system_repository("two", &text).unwrap();
        assert_eq!((r.title.as_str(), r.keys.len(), r.enabled), ("two", 2, false));
        for (text, why) in [
            (String::from("url=file:///x\n"), "1 to 8 keys"),
            (alloc::format!("key={}\n", key(1)), "gives no url"),
            (alloc::format!("url=file:///x\nurl=file:///y\nkey={}\n", key(1)), "given twice"),
            (alloc::format!("url=file:///x\nkey={}\nkey={}\n", key(1), key(1)), "given twice"),
            (alloc::format!("url=file:///x\nkey={}\ncolour=red\n", key(1)), "has no colour"),
            (String::from("url=file:///x\nkey=ed25519:AAAA\n"), "not a public key"),
            (alloc::format!("url=file:///x\nkey={}\nenabled=yes\n", key(1)), "true or false"),
        ] {
            let e = system_repository("main", &text).unwrap_err();
            assert!(e.to_string().contains(why), "{text}: {e}");
        }
        assert!(system_repository("Main", &alloc::format!("url=file:///x\nkey={}\n", key(1))).is_err());
    }

    #[test]
    fn the_user_adds_to_what_the_image_has() {
        let image = alloc::vec![system("system")];
        let mut config = UserConfig::new();
        config.add(user("extra"), &image).unwrap();
        assert_eq!(config.add(user("extra"), &image), Err(ConfigError::Exists(String::from("extra"))));
        assert_eq!(config.add(user("system"), &image), Err(ConfigError::Exists(String::from("system"))));
        config.set_enabled("system", false, &image).unwrap();
        let (merged, shadowed) = merge(image.clone(), &config);
        let names: Vec<(&str, bool, Origin)> = merged.iter().map(|r| (r.name.as_str(), r.enabled, r.origin)).collect();
        assert_eq!(names, [("system", false, Origin::System), ("extra", true, Origin::User)]);
        assert!(shadowed.is_empty());
        // Enabling it again keeps nothing: the image's word stands.
        config.set_enabled("system", true, &image).unwrap();
        assert!(config.disabled.is_empty() && config.enabled.is_empty());
        assert_eq!(config.remove("system", &image), Err(ConfigError::SystemRepository(String::from("system"))));
        config.remove("extra", &image).unwrap();
        assert_eq!(config.remove("extra", &image), Err(ConfigError::NotFound(String::from("extra"))));
        assert_eq!(config.set_enabled("nothing", true, &image), Err(ConfigError::NotFound(String::from("nothing"))));
    }

    #[test]
    fn a_newer_image_keeps_its_names() {
        // The user added "extra"; a newer image has a repository of that
        // name: the image's is used, the user's left out and said to be.
        let mut config = UserConfig::new();
        config.add(user("extra"), &[]).unwrap();
        config.set_enabled("gone", false, &[system("gone")]).unwrap();
        let (merged, shadowed) = merge(alloc::vec![system("extra"), system("system")], &config);
        let names: Vec<(&str, Origin)> = merged.iter().map(|r| (r.name.as_str(), r.origin)).collect();
        assert_eq!(names, [("extra", Origin::System), ("system", Origin::System)]);
        assert_eq!(shadowed, ["extra"]);
        // What the user changed of a repository the image no longer has
        // changes nothing.
        assert!(merged.iter().all(|r| r.enabled));
    }

    #[test]
    fn the_users_configuration_is_checked_when_read_back() {
        let mut config = UserConfig::new();
        config.add(user("extra"), &[]).unwrap();
        let bytes = config.to_bytes();
        assert_eq!(UserConfig::parse(&bytes).unwrap(), config);
        let text = String::from_utf8(bytes).unwrap();
        for bad in [
            text.replace("\"format\":1", "\"format\":2"),
            text.replace("\"name\":\"extra\"", "\"name\":\"Extra\""),
            text.replace("https://packages.example.org/veda", "ftp://x"),
            text.replace("\"enabled\":[]", "\"enabled\":[],\"mirrors\":[]"),
        ] {
            assert!(UserConfig::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        let mut twice = config.clone();
        twice.added.push(user("extra"));
        assert!(UserConfig::parse(&twice.to_bytes()).is_err());
    }
}
