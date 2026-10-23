// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the browser keeps in files: its settings (`~/.config/browser`),
//! the pages visited, the bookmarks and the tabs to open again
//! (`~/.local/share/browser`), each written whole, in one step
//! (`apps_common::store`), a moment after it changes and as the browser
//! closes. The engine keeps cookies, storage and the like beside them
//! (`engine/`), its cache in `~/.cache/browser`, and its temporary files
//! in `/tmp/browser`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use apps_common::store;
use browser_core::bookmarks::Bookmarks;
use browser_core::history::History;
use browser_core::session::Session;
use browser_core::settings::Settings;
use proto::vfs;

/// The largest file read back (bytes): larger is no file of the browser's.
const MAX_FILE: usize = 16 << 20;
/// How long after a change the file is written (ns).
const SAVE_AFTER_NS: u64 = 2_000_000_000;

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/home/user"), PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    home().join(".config/browser")
}

pub fn data_dir() -> PathBuf {
    home().join(".local/share/browser")
}

/// The engine's: cookies, storage, icons.
pub fn engine_data_dir() -> PathBuf {
    data_dir().join("engine")
}

pub fn cache_dir() -> PathBuf {
    home().join(".cache/browser")
}

/// Where downloads go.
pub fn downloads_dir() -> PathBuf {
    home().join("Downloads")
}

/// The engine's temporary files, which go as the browser starts and
/// closes: the network process's own (its `TMPDIR`), and the files the user
/// chooses for pages to upload, copied there for it ([`upload_copy`]).
pub fn temporary_dir() -> PathBuf {
    PathBuf::from("/tmp/browser")
}

/// Empties the engine's temporary files (as a browser that stopped
/// without closing left them), and makes their folder again if `make`.
pub fn clear_temporary(make: bool) {
    let dir = temporary_dir();
    let _ = std::fs::remove_dir_all(&dir);
    if make && let Err(e) = std::fs::create_dir_all(&dir) {
        println!("browser: {}: {e}", dir.display());
    }
}

/// The file at `path`, which the user chose for a page to upload, copied
/// where the network process, which reads it, reaches it: sandboxed, it
/// reaches none of the user's files but the engine's own. The copy has
/// the file's name, in a folder of its own in the temporary files, and
/// the VFS makes it in one step, sharing the contents until either
/// changes, so that it takes neither time nor memory, however large.
pub fn upload_copy(path: &str) -> Result<String, String> {
    static COPIES: AtomicU64 = AtomicU64::new(0);
    let name = Path::new(path).file_name().ok_or("it is not a file")?;
    let dir = temporary_dir().join("uploads").join(COPIES.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let copy = dir.join(name).to_string_lossy().into_owned();
    let vfs = proto::connect(vfs::NAME).map(vfs::Client::new).map_err(|_| "the file system is not there")?;
    match vfs.copy_file(path.into(), copy.clone()) {
        Ok(Ok(())) => Ok(copy),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the file system did not answer".into()),
    }
}

fn read(path: &Path) -> String {
    store::read(path, MAX_FILE).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn write(path: &Path, text: &str) {
    if let Err(e) = store::write(path, text.as_bytes()) {
        println!("browser: {}: {e}", path.display());
    }
}

/// What changed, to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    Settings,
    History,
    Bookmarks,
}

pub struct Store {
    pub settings: Settings,
    pub history: History,
    pub bookmarks: Bookmarks,
    /// What changed since it was written, and when it is to be.
    dirty: [bool; 3],
    due: Option<u64>,
}

impl Store {
    pub fn load() -> Store {
        Store {
            settings: Settings::parse(&read(&config_dir().join("settings"))),
            history: History::parse(&read(&data_dir().join("history"))),
            bookmarks: Bookmarks::parse(&read(&data_dir().join("bookmarks"))),
            dirty: [false; 3],
            due: None,
        }
    }

    /// `what` changed at `now`: it is written soon.
    pub fn changed(&mut self, what: Kept, now: u64) {
        self.dirty[what as usize] = true;
        self.due.get_or_insert(now + SAVE_AFTER_NS);
    }

    /// When the next write is due.
    pub fn due(&self) -> Option<u64> {
        self.due
    }

    /// Writes what changed if it is time (`now`), or whatever `now` is
    /// if `all`.
    pub fn save(&mut self, now: u64, all: bool) {
        if !all && self.due.is_none_or(|d| now < d) {
            return;
        }
        self.due = None;
        if std::mem::take(&mut self.dirty[Kept::Settings as usize]) {
            write(&config_dir().join("settings"), &self.settings.serialize());
        }
        if std::mem::take(&mut self.dirty[Kept::History as usize]) {
            write(&data_dir().join("history"), &self.history.serialize());
        }
        if std::mem::take(&mut self.dirty[Kept::Bookmarks as usize]) {
            write(&data_dir().join("bookmarks"), &self.bookmarks.serialize());
        }
    }

    pub fn session() -> Session {
        Session::parse(&read(&data_dir().join("session")))
    }

    pub fn save_session(session: &Session) {
        write(&data_dir().join("session"), &session.serialize());
    }
}
