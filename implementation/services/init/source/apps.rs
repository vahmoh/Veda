// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The applications the launcher lists and starts: the system image's
//! (`apps/<id>.app`), read at start, and the installed packages'
//! (`/home/packages/apps/<id>.app`). `init_core::apps` describes their
//! manifests, and which of the packages' applications are listed and
//! started, and how.
//!
//! **Through the VFS, never waiting for it.** Every process's registry is
//! `init`'s, so `init` waits for no other process. It reads what packages
//! install through a connection of its own to the VFS, confined first of
//! all to reading `/home/packages` ([`Installed`]): it sends its requests,
//! and takes their answers as they come, in its loop, as it does all else.
//! It reads the packages' manifests once the VFS has registered (`init`
//! starts before it), and again whenever the packages service has changed
//! what is installed (`launcher::refresh_apps`), and tells the launcher's
//! watchers when the list changed (`TaskEvent::AppsChanged`).
//!
//! **Started as the system's are.** An installed package's application
//! starts once its program is read whole (its `launcher::launch_app` is
//! answered then), as one of the system image does: with its id for its
//! identity, the same priority and the same memory class.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use abi::signals;
use init_core::apps::{
    MAX_MANIFEST, MAX_PACKAGED, PACKAGES, PACKAGES_APPS, Program, manifest_id, merge, packaged, program,
};
use ipc::{Decode, Decoder, Encode};
use proto::fs::{DirPage, FsError, Place, vfs};
use proto::init::{AppInfo, ClientIdentity, LaunchError, TaskEvent};
use rt::object::{Channel, Vmo};
use rt::println;

use crate::{Image, Init, Origin};

/// `launcher::launch_app`'s method number: `init` answers it itself, as an
/// installed package's application starts once its program is read.
pub const LAUNCH_APP: u32 = 2;

/// The VFS's method numbers (`proto::fs::vfs`) of the requests `init` sends
/// it, whose answers it takes as they come.
const READ_DIR_PAGE: u32 = 6;
const READ_FILE: u32 = 10;
const CONFINE: u32 = 24;

/// The system service that changes what is installed, and alone has the
/// launcher read the installed packages' applications again.
const INSTALLER: &str = "packages";

/// Programs of installed packages' applications read at once, each for a
/// launcher's call that waits for it.
const MAX_STARTING: usize = 16;

/// What a request to the VFS is for.
enum Want {
    /// Confining the connection to reading the installed packages.
    Confine,
    /// A page of the listing of `/home/packages/apps`.
    Page,
    /// The manifest of the application of this id.
    Manifest(String),
    /// The program of an application to start.
    Program(Start),
}

/// An installed package's application to start once its program is read,
/// for a launcher's call.
struct Start {
    /// The launcher connection, and the call's transaction.
    conn: u64,
    txid: u32,
    id: String,
    path: String,
    args: Vec<String>,
}

/// A reading of the installed packages' applications, under way.
#[derive(Default)]
struct Reading {
    found: Vec<AppInfo>,
    /// A page of the listing is awaited, the one after this name.
    paging: bool,
    after: String,
    /// Manifests asked for, and those of them not answered yet.
    asked: usize,
    waiting: usize,
    /// More manifests are there than are read ([`MAX_PACKAGED`]).
    full: bool,
    /// The listing failed: the applications listed stay as they were.
    failed: bool,
}

/// What `init` reads of the installed packages through the VFS.
#[derive(Default)]
pub struct Installed {
    /// The connection to the VFS (once it has registered), and the next
    /// request's transaction.
    pub vfs: Option<Channel>,
    next_txid: u32,
    /// The requests sent and not answered yet, by transaction.
    waiting: BTreeMap<u32, Want>,
    reading: Option<Reading>,
    /// What is installed changed during the reading under way: it is read
    /// again once that ends.
    again: bool,
}

/// Reads the system image's applications.
pub fn load(archive: &initrd::Archive) -> Vec<AppInfo> {
    let mut apps: Vec<AppInfo> = archive
        .files()
        .filter_map(|f| {
            let id = f.path.strip_prefix("apps/").and_then(manifest_id)?;
            Some(init_core::apps::system(id, core::str::from_utf8(f.data).ok()?))
        })
        .collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps
}

/// What a request's answer `d` holds: an error for one the VFS refused
/// (`refused`), or that does not decode.
fn answer<T: Decode>(refused: bool, d: &mut Decoder) -> Result<T, FsError> {
    if refused {
        return Err(FsError::Invalid);
    }
    Result::<T, FsError>::decode(d).unwrap_or(Err(FsError::Io))
}

impl Init {
    /// Whether `name` is the system's: a system application's id, or the
    /// name of a program of the system image.
    fn is_system_name(&self, name: &str) -> bool {
        self.system_apps.iter().any(|a| a.id == name) || self.initrd.find(&format!("bin/{name}")).is_some()
    }

    /// Whether `init` has its connection to the VFS, made now if the
    /// system's VFS has registered: confined first of all to reading the
    /// installed packages, as `init` reads nothing else through it.
    fn connect_vfs(&mut self) -> bool {
        if self.installed.vfs.is_some() {
            return true;
        }
        self.forget_if_gone(vfs::NAME);
        let owner = self.services.get(vfs::NAME).map(|(_, owner)| *owner);
        if !owner.is_some_and(|owner| self.children.get(&owner).is_some_and(|c| c.origin == Origin::Service)) {
            return false;
        }
        let Ok((ours, theirs)) = Channel::create() else { return false };
        let me = ClientIdentity { koid: self.koid, name: "init".into(), service: true, ..Default::default() };
        self.deliver(vfs::NAME, theirs, me);
        self.installed.vfs = Some(ours);
        let place = Place { path: PACKAGES.into(), write: false };
        self.send(CONFINE, vec![place], Want::Confine).is_ok()
    }

    /// Sends the VFS the request `ordinal` of `args`, for `want`; gives
    /// `want` back if it cannot be sent (the connection then goes).
    fn send(&mut self, ordinal: u32, args: impl Encode, want: Want) -> Result<(), Want> {
        let Some(vfs) = &self.installed.vfs else { return Err(want) };
        let txid = self.installed.next_txid.max(1);
        self.installed.next_txid = txid.wrapping_add(1);
        let mut e = ipc::Encoder::with_header(ordinal, txid, ipc::FLAG_REQUEST);
        args.encode(&mut e);
        if let Err(err) = e.send(vfs) {
            println!("cannot ask the VFS: {}", err);
            self.vfs_gone();
            return Err(want);
        }
        self.installed.waiting.insert(txid, want);
        Ok(())
    }

    /// Forgets the connection to the VFS, which closed or failed: the
    /// launcher's calls waiting for a program fail, and a reading under
    /// way ends, the applications listed as they were.
    fn vfs_gone(&mut self) {
        println!("the connection to the VFS is lost");
        self.installed.vfs = None;
        self.installed.reading = None;
        for want in core::mem::take(&mut self.installed.waiting).into_values() {
            if let Want::Program(start) = want {
                self.answer_launch(start.conn, start.txid, Err(LaunchError::Failed));
            }
        }
    }

    /// Takes what the VFS sent on `init`'s connection, whose signals were
    /// `observed`.
    pub fn take_vfs_answers(&mut self, observed: u32) {
        if observed & signals::READABLE == 0 {
            return self.vfs_gone();
        }
        while let Some(vfs) = &self.installed.vfs {
            match vfs.read() {
                Ok(msg) => self.vfs_answer(msg),
                Err(abi::Error::ShouldWait) => return,
                Err(_) => return self.vfs_gone(),
            }
        }
    }

    /// Takes the VFS's answer `msg` to one of `init`'s requests.
    fn vfs_answer(&mut self, mut msg: rt::Message) {
        let Ok((header, mut d)) = ipc::open(&mut msg) else { return };
        let Some(want) = self.installed.waiting.remove(&header.txid) else { return };
        let refused = header.flags & ipc::FLAG_ERROR != 0;
        match want {
            Want::Confine => {
                if let Err(e) = answer::<()>(refused, &mut d) {
                    println!("the VFS did not confine init's connection to {}: {}", PACKAGES, e);
                    self.vfs_gone();
                }
            }
            Want::Page => self.take_page(answer(refused, &mut d)),
            Want::Manifest(id) => self.take_manifest(&id, answer(refused, &mut d)),
            Want::Program(start) => self.start_installed(start, answer(refused, &mut d)),
        }
    }

    /// Reads the installed packages' applications: now, or once the
    /// reading under way ends, or once the VFS has registered.
    pub fn read_installed_apps(&mut self) {
        if self.installed.reading.is_some() {
            self.installed.again = true;
            return;
        }
        if !self.connect_vfs() {
            return;
        }
        self.installed.reading = Some(Reading { paging: true, ..Reading::default() });
        let _ = self.send(READ_DIR_PAGE, (String::from(PACKAGES_APPS), String::new(), 0u32), Want::Page);
    }

    /// Takes a page of the listing of `/home/packages/apps`: asks for the
    /// manifests on it, and for the next page.
    fn take_page(&mut self, page: Result<DirPage, FsError>) {
        let Some(reading) = &mut self.installed.reading else { return };
        let page = match page {
            Ok(page) => page,
            // No package has installed an application.
            Err(FsError::NotFound) => DirPage::default(),
            Err(e) => {
                println!("cannot list {}: {}", PACKAGES_APPS, e);
                reading.failed = true;
                DirPage::default()
            }
        };
        let mut ids = Vec::new();
        for entry in page.entries.iter().filter(|e| !e.is_dir && !e.socket) {
            let Some(id) = manifest_id(&entry.name) else { continue };
            if reading.asked == MAX_PACKAGED {
                reading.full = true;
                break;
            }
            if entry.size > MAX_MANIFEST as u64 {
                println!("{}/{} is left out: it is larger than {} bytes", PACKAGES_APPS, entry.name, MAX_MANIFEST);
                continue;
            }
            reading.asked += 1;
            ids.push(id.to_string());
        }
        reading.waiting += ids.len();
        // The next page follows the last entry of this one, which a page
        // that says more follows always has.
        let next = page.entries.last().map(|e| e.name.clone()).filter(|last| page.more && *last > reading.after);
        reading.full |= next.is_some() && reading.asked == MAX_PACKAGED;
        let next = next.filter(|_| !reading.full);
        reading.paging = next.is_some();
        if let Some(after) = &next {
            reading.after.clone_from(after);
        }
        for id in ids {
            let path = format!("{}/{}.app", PACKAGES_APPS, id);
            if self.send(READ_FILE, path, Want::Manifest(id)).is_err() {
                return;
            }
        }
        match next {
            Some(after) => {
                let _ = self.send(READ_DIR_PAGE, (String::from(PACKAGES_APPS), after, 0u32), Want::Page);
            }
            None => self.finish_reading(),
        }
    }

    /// Takes the manifest of application `id`, as read.
    fn take_manifest(&mut self, id: &str, file: Result<(Vmo, u64), FsError>) {
        let text = file.map_err(|e| e.to_string()).and_then(|(vmo, len)| {
            let len = usize::try_from(len).ok().filter(|&len| len <= MAX_MANIFEST);
            let len = len.ok_or_else(|| format!("it is larger than {} bytes", MAX_MANIFEST))?;
            let mut bytes = vec![0; len];
            vmo.read(0, &mut bytes).map_err(|e| e.to_string())?;
            String::from_utf8(bytes).map_err(|_| String::from("it is not UTF-8 text"))
        });
        let app = text.and_then(|text| packaged(id, &text, |n| self.is_system_name(n)).map_err(|why| why.to_string()));
        let Some(reading) = &mut self.installed.reading else { return };
        reading.waiting = reading.waiting.saturating_sub(1);
        match app {
            Ok(app) => reading.found.push(app),
            Err(why) => println!("{}/{}.app is left out: {}", PACKAGES_APPS, id, why),
        }
        self.finish_reading();
    }

    /// Ends the reading under way once every answer to it is in: the
    /// applications listed are the system's and those found, and the
    /// watchers hear if they changed.
    fn finish_reading(&mut self) {
        if !self.installed.reading.as_ref().is_some_and(|r| !r.paging && r.waiting == 0) {
            return;
        }
        let Some(reading) = self.installed.reading.take() else { return };
        if reading.full {
            println!("{} has more than {} applications: the rest are left out", PACKAGES_APPS, MAX_PACKAGED);
        }
        if !reading.failed {
            let apps = merge(&self.system_apps, reading.found);
            if apps != self.apps {
                println!("{} application(s) of installed packages", apps.len() - self.system_apps.len());
                self.apps = apps;
                self.tell(TaskEvent::AppsChanged {});
            }
        }
        if core::mem::take(&mut self.installed.again) {
            self.read_installed_apps();
        }
    }

    /// `launcher::refresh_apps`, for the packages service alone.
    pub fn refresh_apps(&mut self, who: &ClientIdentity) -> Result<(), LaunchError> {
        if !(who.service && who.name == INSTALLER) {
            println!("{} may not have the installed applications read again", who.name);
            return Err(LaunchError::Denied);
        }
        self.read_installed_apps();
        Ok(())
    }

    /// Answers a launcher's call on connection `conn` to start an
    /// application (`launcher::launch_app`): at once, or once the program
    /// of an installed package's application is read.
    pub fn launch_app_call(&mut self, conn: u64, mut msg: rt::Message) {
        let Ok((header, mut d)) = ipc::open(&mut msg) else { return };
        let call = (|| -> Result<(String, Vec<String>), ipc::DecodeError> {
            let id = String::decode(&mut d)?;
            let args = Vec::<String>::decode(&mut d)?;
            d.finish()?;
            Ok((id, args))
        })();
        let Ok((id, args)) = call else {
            if let Some(c) = self.conns.get(&conn) {
                let _ = ipc::refuse(&c.channel, header, ipc::Refusal::Malformed);
            }
            return;
        };
        if let Some(result) = self.launch_app(conn, header.txid, id, args) {
            self.answer_launch(conn, header.txid, result);
        }
    }

    /// Starts application `id` for the launcher's call `txid` on connection
    /// `conn`: at once, or once its program is read (`None`: the call is
    /// answered then).
    fn launch_app(&mut self, conn: u64, txid: u32, id: String, args: Vec<String>) -> Option<Result<u64, LaunchError>> {
        let Some(app) = self.apps.iter().find(|a| a.id == id) else { return Some(Err(LaunchError::NotFound)) };
        let exe = app.exe.clone();
        let Some(Program::Packages(path)) = program(&exe) else {
            return Some(self.spawn(&id, &exe, &args, Vec::new(), Origin::Launched));
        };
        let starting = self.installed.waiting.values().filter(|w| matches!(w, Want::Program(_))).count();
        if starting >= MAX_STARTING {
            println!("cannot start {}: {} programs are being read already", id, starting);
            return Some(Err(LaunchError::Failed));
        }
        if !self.connect_vfs() {
            return Some(Err(LaunchError::Failed));
        }
        let start = Start { conn, txid, id, path: path.to_string(), args };
        match self.send(READ_FILE, path.to_string(), Want::Program(start)) {
            Ok(()) => None,
            Err(_) => Some(Err(LaunchError::Failed)),
        }
    }

    /// Starts the installed package's application `start` from its program,
    /// as read.
    fn start_installed(&mut self, start: Start, file: Result<(Vmo, u64), FsError>) {
        let result = match file {
            Ok((vmo, len)) => {
                let image = Image::Read(&vmo, len as usize);
                self.spawn_image(&start.id, &start.path, &start.args, Vec::new(), Origin::Launched, image)
            }
            Err(FsError::NotFound) => Err(LaunchError::NotFound),
            Err(e) => {
                println!("cannot read {}: {}", start.path, e);
                Err(LaunchError::Failed)
            }
        };
        self.answer_launch(start.conn, start.txid, result);
    }

    /// Answers the launcher's call `txid` on connection `conn`, if it is
    /// still open, to start an application.
    fn answer_launch(&self, conn: u64, txid: u32, result: Result<u64, LaunchError>) {
        let Some(c) = self.conns.get(&conn) else { return };
        let mut e = ipc::Encoder::with_header(LAUNCH_APP, txid, ipc::FLAG_RESPONSE);
        result.encode(&mut e);
        let _ = e.send(&c.channel);
    }
}
