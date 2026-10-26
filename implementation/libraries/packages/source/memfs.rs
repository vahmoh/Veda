// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A file system in memory for the tests, as the VFS's behaves (a file
//! moved in one step, a directory removed only when empty), which can be
//! made to lose its power after a given number of changes: that change and
//! everything after it fails, and what the file system holds then is what
//! a computer that lost its power would find at its next start (the VFS
//! saves `/home` whole, as it was after some change). It can also run out
//! of room.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::transaction::{Failure, Files, FilesError, Kind, Sink};

/// What is at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    File(Vec<u8>),
    Dir,
}

#[derive(Debug, Default)]
struct State {
    /// By absolute path; the root is always there.
    nodes: BTreeMap<String, Node>,
    /// Changes left before the power goes, if it is to.
    left: Option<usize>,
    /// The power went.
    off: bool,
    /// Changes made.
    changes: usize,
    /// Bytes the files may hold together, if they are bounded.
    room: Option<usize>,
}

/// The file system; clones share it.
#[derive(Debug, Clone, Default)]
pub struct MemFiles(Rc<RefCell<State>>);

fn error(failure: Failure, detail: &str) -> FilesError {
    FilesError { failure, detail: detail.to_string() }
}

fn parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) => parent,
        None => "/",
    }
}

impl State {
    fn kind(&self, path: &str) -> Option<Kind> {
        if path == "/" {
            return Some(Kind::Dir);
        }
        self.nodes.get(path).map(|n| match n {
            Node::File(_) => Kind::File,
            Node::Dir => Kind::Dir,
        })
    }

    /// Counts a change, unless the power is gone or goes with it.
    fn change(&mut self) -> Result<(), FilesError> {
        if self.off {
            return Err(error(Failure::Other, "the power is off"));
        }
        if let Some(left) = &mut self.left {
            if *left == 0 {
                self.off = true;
                return Err(error(Failure::Other, "the power went"));
            }
            *left -= 1;
        }
        self.changes += 1;
        Ok(())
    }

    fn look(&self) -> Result<(), FilesError> {
        if self.off { Err(error(Failure::Other, "the power is off")) } else { Ok(()) }
    }

    fn used(&self) -> usize {
        self.nodes.values().map(|n| if let Node::File(d) = n { d.len() } else { 0 }).sum()
    }

    fn children(&self, dir: &str) -> Vec<String> {
        let prefix = if dir == "/" { String::from("/") } else { alloc::format!("{dir}/") };
        self.nodes
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(String::from)
            .collect()
    }
}

impl MemFiles {
    /// An empty file system: the root alone.
    pub fn new() -> MemFiles {
        MemFiles::default()
    }

    /// A file system holding `nodes`, with its power on.
    pub fn from(nodes: BTreeMap<String, Node>) -> MemFiles {
        let fs = MemFiles::new();
        fs.0.borrow_mut().nodes = nodes;
        fs
    }

    /// What it holds.
    pub fn nodes(&self) -> BTreeMap<String, Node> {
        self.0.borrow().nodes.clone()
    }

    /// What it holds under `dir`, by paths relative to it.
    pub fn tree(&self, dir: &str) -> BTreeMap<String, Node> {
        let prefix = alloc::format!("{dir}/");
        self.0
            .borrow()
            .nodes
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|rest| (rest.to_string(), v.clone())))
            .collect()
    }

    /// The power goes as the `n`th change from now is made (none of it is).
    pub fn lose_power_after(&self, n: usize) {
        self.0.borrow_mut().left = Some(n);
    }

    /// Changes made so far.
    pub fn changes(&self) -> usize {
        self.0.borrow().changes
    }

    /// Bounds the bytes its files may hold together.
    pub fn set_room(&self, bytes: usize) {
        self.0.borrow_mut().room = Some(bytes);
    }

    /// Puts a file at `path`, making its directories.
    pub fn put(&self, path: &str, data: &[u8]) {
        let mut s = self.0.borrow_mut();
        let mut at = 0;
        while let Some(i) = path[at + 1..].find('/') {
            at += i + 1;
            s.nodes.insert(path[..at].to_string(), Node::Dir);
        }
        s.nodes.insert(path.to_string(), Node::File(data.to_vec()));
    }

    /// Makes the directory `path` and those it is in.
    pub fn put_dir(&self, path: &str) {
        let mut s = self.0.borrow_mut();
        let mut at = 0;
        while let Some(i) = path[at + 1..].find('/') {
            at += i + 1;
            s.nodes.insert(path[..at].to_string(), Node::Dir);
        }
        s.nodes.insert(path.to_string(), Node::Dir);
    }
}

/// A file being written.
struct MemSink {
    fs: MemFiles,
    path: String,
}

impl Sink for MemSink {
    fn write(&mut self, data: &[u8]) -> Result<(), FilesError> {
        let mut s = self.fs.0.borrow_mut();
        s.change()?;
        if s.room.is_some_and(|room| s.used() + data.len() > room) {
            return Err(error(Failure::NoSpace, "no space left"));
        }
        match s.nodes.get_mut(&self.path) {
            Some(Node::File(d)) => {
                d.extend_from_slice(data);
                Ok(())
            }
            _ => Err(error(Failure::NotFound, "the file is gone")),
        }
    }

    fn finish(self: Box<Self>) -> Result<(), FilesError> {
        self.fs.0.borrow().look()
    }
}

impl Files for MemFiles {
    fn kind(&mut self, path: &str) -> Result<Option<Kind>, FilesError> {
        let s = self.0.borrow();
        s.look()?;
        Ok(s.kind(path))
    }

    fn read(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, FilesError> {
        let s = self.0.borrow();
        s.look()?;
        match s.nodes.get(path) {
            Some(Node::File(d)) if d.len() as u64 <= limit => Ok(d.clone()),
            Some(Node::File(_)) => Err(error(Failure::Other, "larger than allowed")),
            Some(Node::Dir) => Err(error(Failure::Other, "is a directory")),
            None => Err(error(Failure::NotFound, "no such file")),
        }
    }

    fn create(&mut self, path: &str) -> Result<Box<dyn Sink>, FilesError> {
        let mut s = self.0.borrow_mut();
        s.change()?;
        if s.kind(parent(path)) != Some(Kind::Dir) {
            return Err(error(Failure::NotFound, "no such directory"));
        }
        if s.kind(path) == Some(Kind::Dir) {
            return Err(error(Failure::Other, "is a directory"));
        }
        s.nodes.insert(path.to_string(), Node::File(Vec::new()));
        Ok(Box::new(MemSink { fs: self.clone(), path: path.to_string() }))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FilesError> {
        let mut s = self.0.borrow_mut();
        s.change()?;
        if s.kind(from) != Some(Kind::File) {
            return Err(error(Failure::NotFound, "no such file"));
        }
        if s.kind(parent(to)) != Some(Kind::Dir) {
            return Err(error(Failure::NotFound, "no such directory"));
        }
        if s.kind(to) == Some(Kind::Dir) {
            return Err(error(Failure::Other, "is a directory"));
        }
        let node = s.nodes.remove(from).unwrap_or(Node::Dir);
        s.nodes.insert(to.to_string(), node);
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FilesError> {
        let mut s = self.0.borrow_mut();
        s.change()?;
        match s.kind(path) {
            Some(Kind::File) => {
                s.nodes.remove(path);
                Ok(())
            }
            Some(Kind::Dir) => Err(error(Failure::Other, "is a directory")),
            None => Err(error(Failure::NotFound, "no such file")),
        }
    }

    fn make_dir(&mut self, path: &str) -> Result<(), FilesError> {
        let mut s = self.0.borrow_mut();
        s.change()?;
        if s.kind(parent(path)) != Some(Kind::Dir) {
            return Err(error(Failure::NotFound, "no such directory"));
        }
        if s.kind(path).is_some() {
            return Err(error(Failure::Other, "already exists"));
        }
        s.nodes.insert(path.to_string(), Node::Dir);
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FilesError> {
        let mut s = self.0.borrow_mut();
        s.change()?;
        if s.kind(path) != Some(Kind::Dir) || path == "/" {
            return Err(error(Failure::NotFound, "no such directory"));
        }
        if !s.children(path).is_empty() {
            return Err(error(Failure::Other, "directory not empty"));
        }
        s.nodes.remove(path);
        Ok(())
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, FilesError> {
        let s = self.0.borrow();
        s.look()?;
        if s.kind(path) != Some(Kind::Dir) {
            return Err(error(Failure::NotFound, "no such directory"));
        }
        Ok(s.children(path))
    }

    fn sync(&mut self) -> Result<(), FilesError> {
        self.0.borrow().look()
    }
}
