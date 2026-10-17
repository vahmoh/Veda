// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `init` — the first user-space process.
//!
//! * Starts the system services and the desktop shell from the initrd, giving
//!   each only the capabilities it needs (narrow hardware resources).
//! * Implements the service **registry**: every process gets a registry
//!   channel; services register by name and clients connect by name.
//!   Connections to a service that has not registered yet are queued (a
//!   bounded number per process). Each connection carries the client's
//!   identity (its process, which init knows from the registry channel it
//!   came through). System services may register any name, and the names
//!   they register stay theirs; other programs only names of their own.
//! * Implements the **launcher** service: starting programs and installed
//!   applications, listing and ending tasks, and power control, each as
//!   far as the caller's identity allows (see [`Init::end`]).

#![no_std]
#![no_main]

extern crate alloc;

mod apps;
#[cfg(target_arch = "x86_64")]
#[path = "architectures/amd64/mod.rs"]
mod arch;
mod boot;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use vabi::startup::role;
use vabi::{resource_kind, signals};
use vipc::WaitSet;
use vproto::init::{
    AppInfo, ClientIdentity, LISTENER_CONNECT, LaunchError, RegistryError, TASK_EVENT, TaskEvent, TaskInfo, launcher,
    registry,
};
use vrt::object::{Channel, Handle, Process, Resource, Vmo};
use vrt::println;

vrt::entry!(main);

/// What a channel served by init speaks.
enum ConnKind {
    /// A process's registry channel (the koid of the process, if known).
    Registry { owner: u64 },
    /// A launcher connection, and who opened it.
    Launcher { who: ClientIdentity },
}

struct Conn {
    channel: Channel,
    kind: ConnKind,
}

/// Why init started a process, which decides what the process may ask of
/// init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A system service (restarted if it exits and is restartable).
    Service,
    /// A program the kernel command line asked for (`run=`, `systest`):
    /// chosen by the machine's owner, as the services are, so it may manage
    /// the system's processes (systest ends the window system to see it
    /// restarted).
    Requested,
    /// An application or program started through the launcher.
    Launched,
}

/// A process started by init.
struct Child {
    name: String,
    process: Process,
    origin: Origin,
}

/// The applications through which the user manages the system, which may
/// end its processes as well as applications: Task Manager, and the
/// Terminal's `kill -f` (the programs the Terminal starts speak for it).
const MANAGERS: [&str; 2] = ["taskmgr", "terminal"];

/// Connections that may wait for a service to register: from one process,
/// and from all together. A connection beyond them is refused.
const PENDING_PER_PROCESS: usize = 64;
const PENDING_MAX: usize = 1024;

/// How far up its parents init looks for whether a process is an
/// application's (process trees are shallow; an unknown lineage counts as
/// the system's).
const MAX_ANCESTORS: usize = 64;

pub struct Init {
    /// Init's own process id: no one may end init.
    koid: u64,
    root: Resource,
    initrd: initrd::Archive<'static>,
    initrd_vmo: Vmo,
    framebuffer: Option<Vmo>,
    boot_info: Option<Vmo>,
    /// The firmware's variables (only the device manager's, which gives
    /// them to the driver VM).
    firmware_variables: Option<Vmo>,
    services: BTreeMap<String, (Channel, u64)>,
    /// Names registered by system services: only system services may
    /// register them again (after a restart).
    reserved: BTreeSet<String>,
    pending: BTreeMap<String, Vec<(Channel, ClientIdentity)>>,
    conns: BTreeMap<u64, Conn>,
    children: BTreeMap<u64, Child>,
    apps: Vec<AppInfo>,
    next_key: u64,
    /// Recent restart times of each supervised service.
    restarts: BTreeMap<String, Vec<u64>>,
    /// Channels that receive [`TaskEvent`]s (from `launcher::watch`).
    watchers: Vec<Channel>,
}

const CHILD_KEY: u64 = 1 << 48;

impl Init {
    fn add_conn(&mut self, channel: Channel, kind: ConnKind) {
        self.next_key += 1;
        self.conns.insert(self.next_key, Conn { channel, kind });
    }

    /// A fresh registry channel (we keep the server end) belonging to
    /// process `owner` (0 until a new process's koid is known).
    fn new_registry_channel(&mut self, owner: u64) -> Option<Channel> {
        let (ours, theirs) = Channel::create().ok()?;
        self.add_conn(ours, ConnKind::Registry { owner });
        Some(theirs)
    }

    /// The identity of the process that owns registry connection `key`.
    fn identity(&self, key: u64) -> ClientIdentity {
        let owner = match self.conns.get(&key) {
            Some(Conn { kind: ConnKind::Registry { owner }, .. }) => *owner,
            _ => 0,
        };
        match self.children.get(&owner) {
            Some(c) => ClientIdentity {
                koid: owner,
                name: c.name.clone(),
                service: c.origin == Origin::Service,
                app: c.origin != Origin::Service,
                requested: c.origin == Origin::Requested,
            },
            None => ClientIdentity { koid: owner, ..Default::default() },
        }
    }

    fn image(&self, path: &str) -> Option<&'static [u8]> {
        let p = path.trim_start_matches('/').trim_start_matches("system/");
        self.initrd.find(p).map(|f| f.data)
    }

    /// Starts `path` from the system image with a registry channel plus
    /// `extra` handles.
    pub fn spawn(
        &mut self,
        name: &str,
        path: &str,
        args: &[String],
        extra: Vec<(u32, Handle)>,
        origin: Origin,
    ) -> Result<u64, LaunchError> {
        let image = self.image(path).ok_or(LaunchError::NotFound)?;
        let registry = self.new_registry_channel(0).ok_or(LaunchError::NoMemory)?;
        let mut spawn = vrt::process::Spawn::new(name).handle(role::REGISTRY, registry.into_handle());
        for a in args {
            spawn = spawn.arg(a);
        }
        spawn = spawn.env("HOME=/home/user");
        for (r, h) in extra {
            spawn = spawn.handle(r, h);
        }
        let process = spawn.start(image).map_err(|e| {
            println!("failed to start {}: {}", path, e);
            match e {
                vrt::process::SpawnError::NotExecutable
                | vrt::process::SpawnError::BadElf(_)
                | vrt::process::SpawnError::Incompatible => LaunchError::BadImage,
                vrt::process::SpawnError::Kernel(vabi::Error::NoMemory) => LaunchError::NoMemory,
                _ => LaunchError::Failed,
            }
        })?;
        let koid = process.0.koid();
        println!("started {} (process {})", name, koid);
        // Attribute the registry channel we just created to this process.
        if let Some(c) = self.conns.get_mut(&self.next_key) {
            c.kind = ConnKind::Registry { owner: koid };
        }
        self.children.insert(koid, Child { name: name.to_string(), process, origin });
        Ok(koid)
    }

    /// Ends process `koid` for `who`, with every process it started (and
    /// those they started): its job, so that nothing it ran (a decoder, a
    /// program in a terminal) outlives it. An application may end
    /// applications and the processes they started; the system's processes
    /// (its services, and what they started: drivers, the driver VM) only
    /// the system services, the user's tools for managing the system
    /// ([`MANAGERS`]) and the programs the kernel command line asked for.
    /// No one ends init.
    fn end(&self, who: &ClientIdentity, koid: u64) -> Result<(), LaunchError> {
        if koid == self.koid {
            return Err(LaunchError::Denied);
        }
        let manages = who.service || who.requested || (who.app && MANAGERS.contains(&who.name.as_str()));
        let processes = self.root.create(resource_kind::PROCESS, 0, 0).map_err(|_| LaunchError::Failed)?;
        let opened;
        let process = match self.children.get(&koid) {
            Some(child) => &child.process,
            None => {
                opened = vrt::object::process_open(&processes, koid).map_err(|_| LaunchError::NotFound)?;
                &opened
            }
        };
        if !manages && !self.is_application(&processes, process) {
            println!("{} may not end process {}, which is the system's", who.name, koid);
            return Err(LaunchError::Denied);
        }
        process.kill_tree().map_err(|_| LaunchError::Failed)
    }

    /// Whether `process` is an application, or descends from one: whether
    /// the first of its ancestors (itself included) that init started was
    /// started as an application. A process whose lineage is lost (a parent
    /// that ended) counts as the system's.
    fn is_application(&self, processes: &Resource, process: &Process) -> bool {
        let Ok(info) = process.info() else { return false };
        let (mut koid, mut parent) = (info.koid, info.parent_koid);
        for _ in 0..MAX_ANCESTORS {
            if let Some(child) = self.children.get(&koid) {
                return child.origin != Origin::Service;
            }
            if parent == 0 || parent == self.koid {
                return false;
            }
            let Ok(info) = vrt::object::process_open(processes, parent).and_then(|p| p.info()) else {
                return false;
            };
            (koid, parent) = (info.koid, info.parent_koid);
        }
        false
    }

    /// Forgets service `name` if the process that registered it is gone (its
    /// listener's other end closed): the name is free to register again, and
    /// connections to it wait for whoever does (a driver started again).
    fn forget_if_gone(&mut self, name: &str) {
        let gone = self.services.get(name).is_some_and(|(listener, _)| {
            listener.wait(signals::PEER_CLOSED, 0).is_ok_and(|s| s & signals::PEER_CLOSED != 0)
        });
        if gone {
            println!("service '{}' is gone", name);
            self.services.remove(name);
        }
    }

    /// Hands a new connection to the service `name`, or queues it until the
    /// service registers: `Denied` if its process has too many queued
    /// already (or all processes together have).
    fn connect(&mut self, name: &str, server_end: Channel, client: ClientIdentity) -> Result<(), RegistryError> {
        self.forget_if_gone(name);
        if self.services.contains_key(name) {
            self.deliver(name, server_end, client);
            return Ok(());
        }
        // Connections whose clients have gone stop waiting.
        self.pending.retain(|_, queued| {
            queued.retain(|(end, _)| !end.wait(signals::PEER_CLOSED, 0).is_ok_and(|s| s & signals::PEER_CLOSED != 0));
            !queued.is_empty()
        });
        let queued = self.pending.values().flatten();
        let (all, own) = queued.fold((0, 0), |(all, own), (_, c)| (all + 1, own + usize::from(c.koid == client.koid)));
        if all >= PENDING_MAX || own >= PENDING_PER_PROCESS {
            println!("{} has too many connections waiting; refused one to '{}'", client.name, name);
            return Err(RegistryError::Denied);
        }
        self.pending.entry(name.to_string()).or_default().push((server_end, client));
        Ok(())
    }

    /// Hands a connection to the registered service `name`.
    fn deliver(&mut self, name: &str, server_end: Channel, client: ClientIdentity) {
        let Some((listener, _)) = self.services.get(name) else { return };
        if vipc::send_event(listener, LISTENER_CONNECT, (server_end, client)).is_err() {
            // The service went away; forget it. The connection is lost (the
            // client will see PEER_CLOSED).
            println!("service '{}' is gone", name);
            self.services.remove(name);
        }
    }

    fn handle_registry(&mut self, key: u64, msg: vrt::Message) {
        struct Reg<'a> {
            init: &'a mut Init,
            key: u64,
        }
        impl registry::Server for Reg<'_> {
            fn connect(&mut self, name: String, server_end: Channel) -> Result<(), RegistryError> {
                let client = self.init.identity(self.key);
                if name == launcher::NAME {
                    self.init.add_conn(server_end, ConnKind::Launcher { who: client });
                    return Ok(());
                }
                self.init.connect(&name, server_end, client)
            }

            fn register(&mut self, name: String, listener: Channel) -> Result<(), RegistryError> {
                self.init.forget_if_gone(&name);
                if self.init.services.contains_key(&name) || name == launcher::NAME || name == registry::NAME {
                    return Err(RegistryError::AlreadyRegistered);
                }
                // A system service may register any name, which then stays
                // the system's. Another program registers only names of its
                // own (`music`, `music/...`), so that it cannot stand in for
                // a service that has not registered yet.
                let client = self.init.identity(self.key);
                let own = |n: &str| {
                    !client.name.is_empty()
                        && n.strip_prefix(client.name.as_str())
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
                };
                if client.service {
                    self.init.reserved.insert(name.clone());
                } else if self.init.reserved.contains(&name) || !own(&name) {
                    println!("{} may not register the service name '{}'", client.name, name);
                    return Err(RegistryError::Denied);
                }
                println!("service '{}' registered", name);
                self.init.services.insert(name.clone(), (listener, client.koid));
                for (ch, client) in self.init.pending.remove(&name).unwrap_or_default() {
                    // Queued again should the service be gone already.
                    let _ = self.init.connect(&name, ch, client);
                }
                Ok(())
            }

            fn list(&mut self) -> Vec<String> {
                let names: Vec<String> = self.init.services.keys().cloned().collect();
                for name in &names {
                    self.init.forget_if_gone(name);
                }
                self.init.services.keys().cloned().collect()
            }

            fn clone_registry(&mut self) -> Result<Channel, RegistryError> {
                // Whoever receives the copy speaks for the same process.
                let owner = self.init.identity(self.key).koid;
                self.init.new_registry_channel(owner).ok_or(RegistryError::Denied)
            }
        }
        let mut reg = Reg { init: self, key };
        let reply = registry::dispatch(&mut reg, msg);
        if let (Ok(reply), Some(conn)) = (reply, self.conns.get(&key)) {
            let _ = reply.send(&conn.channel);
        }
    }

    fn handle_launcher(&mut self, key: u64, msg: vrt::Message) {
        let Some(Conn { kind: ConnKind::Launcher { who }, .. }) = self.conns.get(&key) else { return };
        struct Launch<'a> {
            init: &'a mut Init,
            who: ClientIdentity,
        }
        impl launcher::Server for Launch<'_> {
            fn launch(&mut self, path: String, args: Vec<String>) -> Result<u64, LaunchError> {
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                self.init.spawn(&name, &path, &args, Vec::new(), Origin::Launched)
            }

            fn launch_app(&mut self, id: String, args: Vec<String>) -> Result<u64, LaunchError> {
                let app = self.init.apps.iter().find(|a| a.id == id).cloned().ok_or(LaunchError::NotFound)?;
                self.init.spawn(&app.id, &app.exe, &args, Vec::new(), Origin::Launched)
            }

            fn apps(&mut self) -> Vec<AppInfo> {
                self.init.apps.clone()
            }

            fn tasks(&mut self) -> Vec<TaskInfo> {
                let mut infos = alloc::vec![vabi::ProcessInfo::default(); 256];
                let n = vrt::object::process_list(&mut infos).unwrap_or(0).min(infos.len());
                infos[..n]
                    .iter()
                    .filter(|p| p.state == vabi::process_state::RUNNING)
                    .map(|p| TaskInfo {
                        koid: p.koid,
                        name: p.name().to_string(),
                        state: p.state,
                        threads: p.threads,
                        memory: p.memory_bytes,
                        cpu_ns: p.cpu_time_ns,
                        is_app: self.init.children.get(&p.koid).is_some_and(|c| c.origin != Origin::Service),
                    })
                    .collect()
            }

            fn kill(&mut self, koid: u64) -> Result<(), LaunchError> {
                self.init.end(&self.who, koid)
            }

            /// For system services alone (the shell's power menu, the agent).
            fn power(&mut self, action: u32) -> Result<(), LaunchError> {
                if !self.who.service {
                    println!("{} may not shut down or restart the machine", self.who.name);
                    return Err(LaunchError::Denied);
                }
                println!("power action {} for {}", action, self.who.name);
                let res = self.init.root.create(resource_kind::POWER, 0, 0).map_err(|_| LaunchError::Denied)?;
                vrt::object::power(&res, action as usize).map_err(|_| LaunchError::Failed)
            }

            /// For system services (the shell tells the user of crashes) and
            /// the programs the kernel command line asked for (systest).
            fn watch(&mut self) -> Result<Channel, LaunchError> {
                const MAX_WATCHERS: usize = 16;
                if !self.who.service && !self.who.requested {
                    return Err(LaunchError::Denied);
                }
                // Watchers that went away make room.
                self.init
                    .watchers
                    .retain(|w| !w.wait(signals::PEER_CLOSED, 0).is_ok_and(|s| s & signals::PEER_CLOSED != 0));
                if self.init.watchers.len() >= MAX_WATCHERS {
                    return Err(LaunchError::Denied);
                }
                let (ours, theirs) = Channel::create().map_err(|_| LaunchError::NoMemory)?;
                self.init.watchers.push(ours);
                Ok(theirs)
            }
        }
        let who = who.clone();
        let reply = launcher::dispatch(&mut Launch { init: self, who }, msg);
        if let (Ok(reply), Some(conn)) = (reply, self.conns.get(&key)) {
            let _ = reply.send(&conn.channel);
        }
    }

    fn on_child_exit(&mut self, koid: u64) {
        let Some(child) = self.children.remove(&koid) else { return };
        let code = child.process.info().map(|i| i.exit_code).unwrap_or(0);
        match code {
            vabi::EXIT_CODE_CRASHED => println!("{} crashed", child.name),
            vabi::EXIT_CODE_PANICKED => println!("{} panicked", child.name),
            // A POSIX program's fault: a program on the standard library
            // that panics aborts.
            c if vabi::crashed(c) => println!("{} crashed (signal {})", child.name, vabi::exit_signal(c).unwrap_or(0)),
            _ => println!("{} exited with code {}", child.name, code),
        }
        if child.origin != Origin::Service && vabi::crashed(code) {
            // Tell the watchers (the shell shows a notification). Watchers
            // that went away are dropped; a full channel just misses this.
            let event = TaskEvent::Crashed { koid, name: child.name.clone() };
            self.watchers
                .retain(|w| !matches!(vipc::send_event(w, TASK_EVENT, event.clone()), Err(vipc::IpcError::PeerClosed)));
        }
        // Drop the services it provided.
        self.services.retain(|name, (_, owner)| {
            let keep = *owner != koid;
            if !keep {
                println!("service '{}' unregistered", name);
            }
            keep
        });
        if child.origin == Origin::Service {
            self.supervise(&child.name);
        }
    }

    /// Restarts a system service that exited, unless it is not restartable
    /// or keeps failing.
    fn supervise(&mut self, name: &str) {
        const WINDOW_NS: u64 = 60_000_000_000;
        const MAX_RESTARTS: usize = 3;
        if !boot::RESTARTABLE.contains(&name) {
            println!("warning: system service {} is no longer running", name);
            return;
        }
        let now = vrt::time::now_ns();
        let history = self.restarts.entry(name.to_string()).or_default();
        history.retain(|&t| now - t < WINDOW_NS);
        if history.len() >= MAX_RESTARTS {
            println!("{} failed {} times within a minute; giving up on it", name, MAX_RESTARTS + 1);
            return;
        }
        history.push(now);
        println!("restarting {}", name);
        boot::start_service(self, name, false);
    }

    fn run(&mut self) -> ! {
        // Kept from one wait to the next: its handles (a registry channel
        // for every process) stay registered with the kernel.
        let mut ws = WaitSet::new();
        loop {
            ws.clear();
            for (&k, c) in &self.conns {
                ws.add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, k);
            }
            for (&koid, c) in &self.children {
                ws.add(c.process.raw(), signals::TERMINATED, CHILD_KEY | koid);
            }
            let ready = match ws.wait(vabi::DEADLINE_INFINITE) {
                Ok(r) => r,
                Err(e) => {
                    println!("wait failed: {}", e);
                    continue;
                }
            };
            for (key, observed) in ready {
                if key & CHILD_KEY != 0 {
                    self.on_child_exit(key & !CHILD_KEY);
                    continue;
                }
                if observed & signals::READABLE != 0 {
                    // Drain everything queued on this connection.
                    while let Some(conn) = self.conns.get(&key) {
                        let Ok(msg) = conn.channel.read() else { break };
                        match conn.kind {
                            ConnKind::Registry { .. } => self.handle_registry(key, msg),
                            ConnKind::Launcher { .. } => self.handle_launcher(key, msg),
                        }
                    }
                } else if observed & signals::PEER_CLOSED != 0 {
                    self.conns.remove(&key);
                }
            }
        }
    }
}

fn main() -> i32 {
    println!("starting Veda {}", env!("CARGO_PKG_VERSION"));
    let root = Resource::from_handle(vrt::env::take_handle(role::ROOT_RESOURCE).expect("init: no root resource"));
    let initrd_vmo = Vmo::from_handle(vrt::env::take_handle(role::INITRD).expect("init: no initrd"));
    let size = initrd_vmo.size().expect("init: initrd size");
    let addr = initrd_vmo.map(0, size, vabi::map_flags::READ).expect("init: cannot map the initrd");
    // SAFETY: the read-only mapping lives for the rest of init's life.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    let archive = initrd::Archive::open(bytes).expect("init: the initrd is corrupt");

    let mut init = Init {
        koid: vrt::object::process_self_info().map_or(0, |i| i.koid),
        root,
        initrd: archive,
        initrd_vmo,
        framebuffer: vrt::env::take_handle(role::FRAMEBUFFER).map(Vmo::from_handle),
        boot_info: vrt::env::take_handle(role::BOOT_INFO).map(Vmo::from_handle),
        firmware_variables: vrt::env::take_handle(role::FIRMWARE_VARIABLES).map(Vmo::from_handle),
        services: BTreeMap::new(),
        reserved: BTreeSet::new(),
        pending: BTreeMap::new(),
        conns: BTreeMap::new(),
        children: BTreeMap::new(),
        apps: Vec::new(),
        next_key: 0,
        restarts: BTreeMap::new(),
        watchers: Vec::new(),
    };
    init.apps = apps::load(&init.initrd);
    println!("{} application(s) installed", init.apps.len());
    boot::start_system(&mut init);
    boot::start_requested(&mut init);
    init.run()
}
