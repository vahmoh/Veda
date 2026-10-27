// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `network` — the network service.
//!
//! * Network devices (Linux's network cards in the driver VM, which speak
//!   as the device manager, and the Wi-Fi service for each wireless
//!   connection) attach through the `netdev` protocol; their frames flow
//!   through shared rings, which the stack reads and writes directly.
//! * Applications use the `net` protocol: sockets (each its own channel),
//!   name resolution, status and configuration. Changing the configuration
//!   (the interfaces', the DNS servers, the firewall's) is for Settings
//!   alone; reading the firewall, and seeing other processes' sockets and
//!   the hardware addresses, for Settings and the Terminal.
//! * The firewall (`netstack`'s) decides what other machines may start
//!   with this one; `network` tells it which sockets are open to them, as
//!   their applications' rules say, and keeps the applications the rules
//!   keep off the network from it: their connections, pings and lookups are
//!   refused, and their datagrams to other machines dropped. Its settings
//!   are kept on the home disk, where no application reaches them
//!   (`store`); until they are read, the defaults let nothing in.
//! * When an application the settings say nothing about starts taking
//!   connections from other machines, the user is asked whether it may:
//!   the desktop shell, alone, hears the question and answers it, and the
//!   answer becomes the application's rule. Until then nothing reaches the
//!   application; one question waits per application, and none is asked
//!   before the saved settings are read (they may answer it).
//! * The protocol work itself — interfaces, DHCP, IPv6 autoconfiguration,
//!   routing, DNS, TCP, UDP, ICMP — happens in `netstack` (smoltcp
//!   underneath).
//!
//! Everything runs on one thread around a single wait: on device rings,
//! device and client channels, socket channels, lookups' reply channels and
//! the stack's next timer. Each wake-up attends to what changed: the
//! sockets the stack reports ready, and those whose client wrote or made
//! room. Every peer is untrusted: malformed messages fail only the
//! offending socket or connection, and per-process limits bound what a
//! program can consume, however many connections it opens.

#![no_std]
#![no_main]

extern crate alloc;

mod socket;
mod store;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

use abi::signals;
use netstack::{IfaceId, MAX_PENDING_LOOKUPS, MAX_SOCKETS, ResolveId, SockId, Stack, StackEvent};
use proto::init::ClientIdentity;
use proto::net::{
    AddrFamily, AppRule, DeviceInfo, FIREWALL_NOTICE, FirewallConfig, FirewallEvent, FirewallNotice, FirewallProtocol,
    FirewallQuestion, FirewallStats, InterfaceInfo, IpConfig, NET_EVENT, NetError, NetEvent, NetStatus, NetdevError,
    RESOLVE_RESULT, RouteInfo, Scope, SocketEvent, SocketInfo, TcpOptions, format_mac, net, netdev,
};
use proto::netring::{self, LinkEndpoints, SlotMeta, kind};
use rt::object::{Channel, Socket};
use rt::println;

use socket::{Conn, Events, Turn};

rt::entry!(main);

/// `net` connections, of all processes and of one.
const MAX_CLIENTS: usize = 256;
const MAX_CLIENTS_PER_PROCESS: usize = 32;
/// Sockets of one process (connections its listeners accepted included);
/// the stack's table bounds them all together.
const MAX_SOCKETS_PER_PROCESS: usize = 256;
/// Lookups in progress for one process; the stack bounds them all
/// together.
const MAX_LOOKUPS_PER_PROCESS: usize = 32;
/// Status watchers, of all processes and of one.
const MAX_WATCHERS: usize = 32;
const MAX_WATCHERS_PER_PROCESS: usize = 4;
/// What of each shared resource only system services may take, so that
/// applications together cannot leave the system without: connections,
/// sockets, lookups and watchers.
const RESERVED_CLIENTS: usize = 32;
const RESERVED_SOCKETS: usize = 128;
const RESERVED_LOOKUPS: usize = 32;
const RESERVED_WATCHERS: usize = 8;
/// Device connections (attached or not yet).
const MAX_DEVICES: usize = 32;
/// Requests handled per connection per loop iteration.
const CLIENT_BUDGET: usize = 32;
/// Non-Ethernet frames skipped per frame asked for (a device must send
/// Ethernet frames; this bounds what a broken one costs).
const MAX_SKIPPED: usize = 64;
/// Reseed the stack's generator with kernel entropy this often.
const RESEED_NS: u64 = 300_000_000_000;
/// How long a UDP socket its client closed waits for the stack to send its
/// datagrams before they are dropped (Linux gives up on a neighbor that
/// does not answer after as long).
const LINGER_US: u64 = 3_000_000;
/// Questions about applications waiting for the user's answer.
const MAX_QUESTIONS: usize = 16;

/// Wait-set keys: the kind of handle in the high bits, its key below.
const NET_LISTENER: u64 = 0;
const DEV_LISTENER: u64 = 1;
const DEVICE: u64 = 1 << 56;
const DEVICE_LINK: u64 = 1 << 57;
const CLIENT: u64 = 1 << 58;
const SOCKET: u64 = 1 << 59;
const LOOKUP: u64 = 1 << 60;
/// The kernel socket a connection's bytes go on.
const BYTES: u64 = 1 << 61;
/// The firewall store's channel.
const STORE: u64 = 1 << 62;
const KEY: u64 = (1 << 56) - 1;

/// A device's frame link, as the stack reads and writes it.
struct RingLink {
    link: netring::Link,
    /// Frames of another kind than Ethernet, skipped.
    skipped: u64,
}

impl netstack::Link for RingLink {
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        for _ in 0..MAX_SKIPPED {
            let (meta, len) = self.link.recv(buf)?;
            if meta.kind == kind::ETHERNET {
                return Some(len);
            }
            self.skipped += 1;
        }
        None
    }

    fn can_send(&self) -> bool {
        self.link.send_space() > 0
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        self.link.send(SlotMeta::ethernet(), frame)
    }

    fn dropped(&self) -> u64 {
        self.link.peer_dropped()
    }

    fn malformed(&self) -> u64 {
        self.link.bad_frames() + self.skipped
    }
}

/// A device attached through `netdev` (only the device manager's drivers
/// and the Wi-Fi service connect).
struct Device {
    channel: Channel,
    link: Option<RingLink>,
    iface: Option<IfaceId>,
    name: String,
}

/// The devices' links by interface, for the stack.
struct DeviceLinks<'a>(&'a mut BTreeMap<u64, Device>);

impl netstack::Links for DeviceLinks<'_> {
    fn link(&mut self, id: IfaceId) -> Option<&mut dyn netstack::Link> {
        let dev = self.0.values_mut().find(|d| d.iface == Some(id))?;
        dev.link.as_mut().map(|l| l as &mut dyn netstack::Link)
    }
}

/// A `net` client connection.
struct Client {
    channel: Channel,
    who: ClientIdentity,
    /// Settings or the Terminal: may see everything.
    manager: bool,
    /// Settings: may change the configuration too.
    settings: bool,
    /// The desktop shell: asks the user the firewall's questions.
    shell: bool,
}

/// Whether one more of a shared resource (`used` of `max`) may go to a
/// client: system services may take the last `reserved`, others not.
fn room(service: bool, used: usize, max: usize, reserved: usize) -> bool {
    used < if service { max } else { max.saturating_sub(reserved) }
}

/// A lookup in progress.
struct Lookup {
    reply: Channel,
    owner: u64,
}

/// A status watcher.
struct Watcher {
    channel: Channel,
    owner: u64,
}

/// What one process holds.
#[derive(Default)]
struct Usage {
    clients: usize,
    sockets: usize,
    lookups: usize,
    watchers: usize,
}

impl Usage {
    fn is_empty(&self) -> bool {
        self.clients == 0 && self.sockets == 0 && self.lookups == 0 && self.watchers == 0
    }
}

struct Network {
    stack: Stack,
    /// What it waits on (see `Network::wait`).
    ws: ipc::WaitSet,
    net_listener: Channel,
    dev_listener: Channel,
    clients: BTreeMap<u64, Client>,
    devices: BTreeMap<u64, Device>,
    sockets: BTreeMap<SockId, Conn>,
    lookups: BTreeMap<ResolveId, Lookup>,
    watchers: Vec<Watcher>,
    /// What each process holds, by process id.
    usage: BTreeMap<u64, Usage>,
    /// Sockets to attend to on the next pump (their client wrote or made
    /// room, or they are new).
    work: BTreeSet<SockId>,
    /// UDP sockets whose client closed them, attended to after every poll
    /// until the stack has sent their datagrams, and by when they are
    /// dropped all the same (µs).
    lingering: BTreeMap<SockId, u64>,
    events: Events,
    next_key: u64,
    /// The firewall's settings, as applied.
    firewall: FirewallConfig,
    /// Where they are kept (`None` if its thread could not start).
    store: Option<store::Store>,
    /// Settings changed them since the service started: what the store
    /// reads after that is older.
    firewall_changed: bool,
    /// The saved settings were read (or there is no store): questions may
    /// be asked.
    settings_read: bool,
    /// Questions waiting for the user's answer, by application.
    questions: BTreeMap<String, FirewallQuestion>,
    /// Applications to ask about once the saved settings are read, with
    /// what they listen with.
    deferred: BTreeMap<String, (FirewallProtocol, u16)>,
    /// The desktop shell's channel for the questions.
    asker: Option<Channel>,
    last_reseed: u64,
}

fn now_us() -> u64 {
    rt::time::now_ns() / 1000
}

/// Whether `who` may attach network devices: the device manager (whose
/// drivers, the driver VM's programs included, speak as it) and the Wi-Fi
/// service.
fn may_attach(who: &ClientIdentity) -> bool {
    who.service && matches!(who.name.as_str(), "device-manager" | "wlan")
}

/// Whether `who` manages the network: Settings and the Terminal.
fn manages(who: &ClientIdentity) -> bool {
    who.app && matches!(who.name.as_str(), "settings" | "terminal")
}

/// The time now, in Unix seconds.
fn unix_s() -> u64 {
    rt::time::unix_time_ns() / 1_000_000_000
}

fn protocol_name(p: FirewallProtocol) -> &'static str {
    match p {
        FirewallProtocol::Tcp => "TCP",
        FirewallProtocol::Udp => "UDP",
    }
}

impl Network {
    fn key(&mut self) -> u64 {
        self.next_key = (self.next_key + 1) & KEY;
        self.next_key
    }

    fn usage(&mut self, owner: u64) -> &mut Usage {
        self.usage.entry(owner).or_default()
    }

    /// Gives back what `owner` held, as `f` says.
    fn release(&mut self, owner: u64, f: impl FnOnce(&mut Usage)) {
        if let Some(u) = self.usage.get_mut(&owner) {
            f(u);
            if u.is_empty() {
                self.usage.remove(&owner);
            }
        }
    }

    fn sockets_of(&self, owner: u64) -> usize {
        self.usage.get(&owner).map_or(0, |u| u.sockets)
    }

    fn add_socket(&mut self, conn: Conn) {
        let (id, owner) = (conn.id, conn.owner);
        self.usage(owner).sockets += 1;
        self.sockets.insert(id, conn);
        self.work.insert(id);
    }

    /// Applies the firewall's settings: to the stack, and to every socket,
    /// where it is open to other machines from, and whether its application
    /// may use the network (a connection of one the settings now keep off
    /// it, to elsewhere than the machine itself, is reset).
    fn apply_firewall(&mut self, config: FirewallConfig) {
        self.firewall = config;
        self.stack.set_firewall(self.firewall.clone());
        let ids: Vec<SockId> = self.sockets.keys().copied().collect();
        for id in ids {
            let Some(conn) = self.sockets.get_mut(&id) else { continue };
            let may = self.firewall.allows_network(&conn.app);
            conn.set_network(may);
            if conn.receives() {
                self.stack.set_exposure(id, self.firewall.incoming_for(&conn.app), &conn.app);
            }
            if !may && conn.is_connection() && self.stack.tcp_remote(id).is_some_and(|r| !r.ip().is_loopback()) {
                self.stack.tcp_abort(id);
                self.work.insert(id);
            }
        }
        // Questions the settings now answer, or make pointless.
        let settled: Vec<String> = self.questions.keys().filter(|app| !self.firewall.undecided(app)).cloned().collect();
        for app in settled {
            self.questions.remove(&app);
            self.tell_asker(FirewallNotice::Settled { app });
        }
    }

    /// `app` takes connections from other machines (with `protocol` on
    /// `port`): the user is asked whether it may, if the settings say
    /// nothing about it and nobody asked yet.
    fn ask(&mut self, app: &str, protocol: FirewallProtocol, port: u16) {
        if !self.firewall.undecided(app) || self.questions.contains_key(app) {
            return;
        }
        if !self.settings_read {
            // The saved settings may answer it.
            if self.deferred.len() < MAX_QUESTIONS {
                self.deferred.entry(app.into()).or_insert((protocol, port));
            }
            return;
        }
        if self.questions.len() >= MAX_QUESTIONS {
            println!("firewall: too many questions waiting; {app} is not asked about");
            return;
        }
        println!("firewall: asking whether {app} may accept connections ({} port {port})", protocol_name(protocol));
        let question = FirewallQuestion { app: app.into(), protocol, port, at: unix_s() };
        self.questions.insert(app.into(), question.clone());
        self.tell_asker(FirewallNotice::Asked { question });
    }

    /// Whether `app` may reach `remote` (`PermissionDenied`, noted, if the
    /// firewall keeps it off the network).
    fn may_reach(&mut self, app: &str, protocol: u8, remote: SocketAddr) -> Result<(), NetError> {
        if remote.ip().is_loopback() || self.firewall.allows_network(app) {
            return Ok(());
        }
        self.stack.note_app_blocked(protocol, remote, app);
        Err(NetError::PermissionDenied)
    }

    /// A socket of `app`'s (a system service's if `service`) bound to
    /// `local` (asked for; `bound` taken): one that takes what other
    /// machines start, a listener or a datagram socket on a port of the
    /// application's choosing (one on a port the system picked is a
    /// client's, whose replies come in anyway), has the user asked whether
    /// the application may. A system service is part of the system: nobody
    /// is asked.
    fn listens(&mut self, app: &str, service: bool, protocol: FirewallProtocol, local: SocketAddr, bound: SocketAddr) {
        let server = protocol == FirewallProtocol::Tcp || local.port() != 0;
        if server && !local.ip().is_loopback() && !service {
            self.ask(app, protocol, bound.port());
        }
    }

    /// Tells the desktop shell, if it listens.
    fn tell_asker(&mut self, notice: FirewallNotice) {
        if let Some(ch) = &self.asker
            && matches!(ipc::send_event(ch, FIREWALL_NOTICE, notice), Err(ipc::IpcError::PeerClosed))
        {
            self.asker = None;
        }
    }

    /// The saved settings are read (or will not be): the questions held
    /// back are asked, those they leave open.
    fn settings_now_read(&mut self) {
        if self.settings_read {
            return;
        }
        self.settings_read = true;
        for (app, (protocol, port)) in core::mem::take(&mut self.deferred) {
            self.ask(&app, protocol, port);
        }
    }

    /// What the firewall's store says: the settings it read.
    fn store_heard(&mut self, observed: u32) {
        let Some(store) = &self.store else { return };
        match store.take_loaded() {
            Some(store::Loaded::Saved(config)) if !self.firewall_changed => {
                println!("firewall: settings read from the home disk: {}", summary(&config));
                self.apply_firewall(config);
            }
            // Settings changed them first.
            Some(store::Loaded::Saved(_)) => {}
            Some(store::Loaded::Missing) => println!("firewall: no settings saved; the defaults hold"),
            Some(store::Loaded::Unreadable) => {
                println!("firewall: the saved settings are unreadable; the defaults hold")
            }
            None if observed & signals::PEER_CLOSED != 0 => {
                println!("firewall: the store stopped; changes from now on are not kept");
                self.store = None;
            }
            None => return,
        }
        self.settings_now_read();
    }

    /// The client closed a socket: the stack finishes it (a TCP connection
    /// gracefully).
    fn close_socket(&mut self, id: SockId) {
        if let Some(conn) = self.sockets.remove(&id) {
            self.stack.close(id);
            self.work.remove(&id);
            self.lingering.remove(&id);
            self.release(conn.owner, |u| u.sockets -= 1);
        }
    }

    // ---------------------------------------------------------------
    // Devices

    fn accept_devices(&mut self) {
        while let Some((ch, who)) = proto::accept_with_identity(&self.dev_listener) {
            if !may_attach(&who) {
                println!("{} may not attach network devices; refused", who.name);
                continue;
            }
            if self.devices.len() >= MAX_DEVICES {
                println!("too many network devices; refused one from {}", who.name);
                continue;
            }
            let k = self.key();
            self.devices.insert(k, Device { channel: ch, link: None, iface: None, name: String::new() });
        }
    }

    fn handle_device(&mut self, key: u64) {
        struct Session<'a> {
            network: &'a mut Network,
            key: u64,
        }
        impl netdev::Server for Session<'_> {
            fn attach(&mut self, info: DeviceInfo, link: LinkEndpoints, link_up: bool) -> Result<String, NetdevError> {
                if self.network.devices.get(&self.key).is_none_or(|d| d.iface.is_some()) {
                    return Err(NetdevError::Invalid);
                }
                let link = netring::Link::attach(link).map_err(|_| NetdevError::BadLink)?;
                let id = self.network.stack.add_interface(&info, link_up)?;
                let name = String::from(self.network.stack.interface_name(id).unwrap_or("?"));
                println!(
                    "{}: {} {} ({}, {}), link {}",
                    name,
                    match info.kind {
                        proto::net::InterfaceKind::Wireless => "wireless",
                        _ => "ethernet",
                    },
                    format_mac(&info.mac),
                    info.driver,
                    info.location,
                    if link_up { "up" } else { "down" }
                );
                if let Some(dev) = self.network.devices.get_mut(&self.key) {
                    dev.link = Some(RingLink { link, skipped: 0 });
                    dev.iface = Some(id);
                    dev.name = name.clone();
                }
                Ok(name)
            }

            fn set_link(&mut self, up: bool) {
                let Some(dev) = self.network.devices.get(&self.key) else { return };
                if let Some(id) = dev.iface {
                    println!("{}: link {}", dev.name, if up { "up" } else { "down" });
                    self.network.stack.set_link(id, up);
                }
            }
        }
        for _ in 0..CLIENT_BUDGET {
            let Some(dev) = self.devices.get(&key) else { return };
            let msg = match dev.channel.read() {
                Ok(m) => m,
                Err(_) => return,
            };
            let mut s = Session { network: self, key };
            match netdev::dispatch(&mut s, msg) {
                Ok(reply) => {
                    if let Some(dev) = self.devices.get(&key) {
                        let _ = reply.send(&dev.channel);
                    }
                }
                Err(e) => println!("bad netdev request: {}", e),
            }
        }
    }

    fn remove_device(&mut self, key: u64) {
        if let Some(dev) = self.devices.remove(&key)
            && let Some(id) = dev.iface
        {
            println!("{}: device gone", dev.name);
            self.stack.remove_interface(id);
        }
    }

    // ---------------------------------------------------------------
    // Clients

    fn accept_clients(&mut self) {
        while let Some((ch, who)) = proto::accept_with_identity(&self.net_listener) {
            let mine = self.usage.get(&who.koid).map_or(0, |u| u.clients);
            if !room(who.service, self.clients.len(), MAX_CLIENTS, RESERVED_CLIENTS) || mine >= MAX_CLIENTS_PER_PROCESS
            {
                println!("{} has too many connections; refused one", who.name);
                continue;
            }
            let k = self.key();
            self.usage(who.koid).clients += 1;
            let manager = manages(&who);
            let settings = who.app && who.name == "settings";
            let shell = who.service && who.name == "shell";
            self.clients.insert(k, Client { channel: ch, who, manager, settings, shell });
        }
    }

    fn remove_client(&mut self, key: u64) {
        if let Some(c) = self.clients.remove(&key) {
            self.release(c.who.koid, |u| u.clients -= 1);
        }
    }

    fn handle_client(&mut self, key: u64) {
        struct Session<'a> {
            network: &'a mut Network,
            /// The client's process.
            owner: u64,
            manager: bool,
            settings: bool,
            shell: bool,
            /// A system service.
            service: bool,
            /// The client's name, which the firewall's rules use.
            app: String,
        }
        impl Session<'_> {
            fn check_quota(&self) -> Result<(), NetError> {
                let shared = room(self.service, self.network.stack.socket_count(), MAX_SOCKETS, RESERVED_SOCKETS);
                if !shared || self.network.sockets_of(self.owner) >= MAX_SOCKETS_PER_PROCESS {
                    Err(NetError::LimitReached)
                } else {
                    Ok(())
                }
            }

            fn check_manager(&self) -> Result<(), NetError> {
                if self.manager { Ok(()) } else { Err(NetError::PermissionDenied) }
            }

            /// Only Settings changes how the network is set up: the programs
            /// run in the Terminal speak for it.
            fn check_settings(&self) -> Result<(), NetError> {
                if self.settings { Ok(()) } else { Err(NetError::PermissionDenied) }
            }

            /// Whether the client may reach `remote` (see
            /// [`Network::may_reach`]).
            fn may_reach(&mut self, protocol: u8, remote: SocketAddr) -> Result<(), NetError> {
                self.network.may_reach(&self.app, protocol, remote)
            }

            /// A new socket of the client's: where it is open to other
            /// machines from, and whether it may use the network.
            fn track(&mut self, mut conn: Conn) {
                conn.set_network(self.network.firewall.allows_network(&self.app));
                if conn.receives() {
                    let from = self.network.firewall.incoming_for(&self.app);
                    self.network.stack.set_exposure(conn.id, from, &self.app);
                }
                self.network.add_socket(conn);
            }

            /// A socket of the client's bound to `local` (asked for; `bound`
            /// taken): see [`Network::listens`].
            fn listens(&mut self, protocol: FirewallProtocol, local: SocketAddr, bound: SocketAddr) {
                self.network.listens(&self.app, self.service, protocol, local, bound);
            }

            /// Binds with `bind`, and once more if the port was taken by
            /// sockets whose clients closed them already (see
            /// [`Network::reap`]).
            fn bind<T>(
                &mut self,
                protocol: FirewallProtocol,
                local: SocketAddr,
                mut bind: impl FnMut(&mut Stack) -> Result<T, NetError>,
            ) -> Result<T, NetError> {
                match bind(&mut self.network.stack) {
                    Err(NetError::AddressInUse) if local.port() != 0 => {
                        self.network.reap(protocol, local.port());
                        bind(&mut self.network.stack)
                    }
                    other => other,
                }
            }

            fn check_shell(&self) -> Result<(), NetError> {
                if self.shell { Ok(()) } else { Err(NetError::PermissionDenied) }
            }
        }
        impl net::Server for Session<'_> {
            fn tcp_connect(
                &mut self,
                remote: SocketAddr,
                options: TcpOptions,
                socket: Channel,
            ) -> Result<(), NetError> {
                self.check_quota()?;
                self.may_reach(6, remote)?;
                let id = self.network.stack.tcp_connect(remote, &options, self.owner)?;
                self.track(Conn::tcp(socket, id, self.owner, self.app.clone()));
                Ok(())
            }

            fn tcp_connect_bytes(
                &mut self,
                remote: SocketAddr,
                options: TcpOptions,
                socket: Channel,
                bytes: Socket,
            ) -> Result<(), NetError> {
                self.check_quota()?;
                self.may_reach(6, remote)?;
                let id = self.network.stack.tcp_connect(remote, &options, self.owner)?;
                self.track(Conn::tcp_bytes(socket, bytes, id, self.owner, self.app.clone()));
                Ok(())
            }

            fn tcp_listen(
                &mut self,
                local: SocketAddr,
                v6only: bool,
                backlog: u32,
                options: TcpOptions,
                listener: Channel,
            ) -> Result<SocketAddr, NetError> {
                self.check_quota()?;
                let owner = self.owner;
                let (id, bound) =
                    self.bind(FirewallProtocol::Tcp, local, |s| s.tcp_listen(local, v6only, backlog, &options, owner))?;
                self.track(Conn::listener(listener, id, owner, false, self.app.clone()));
                self.listens(FirewallProtocol::Tcp, local, bound);
                Ok(bound)
            }

            fn tcp_listen_bytes(
                &mut self,
                local: SocketAddr,
                v6only: bool,
                backlog: u32,
                options: TcpOptions,
                listener: Channel,
            ) -> Result<SocketAddr, NetError> {
                self.check_quota()?;
                let owner = self.owner;
                let (id, bound) =
                    self.bind(FirewallProtocol::Tcp, local, |s| s.tcp_listen(local, v6only, backlog, &options, owner))?;
                self.track(Conn::listener(listener, id, owner, true, self.app.clone()));
                self.listens(FirewallProtocol::Tcp, local, bound);
                Ok(bound)
            }

            fn tcp_bind(&mut self, local: SocketAddr, v6only: bool, socket: Channel) -> Result<SocketAddr, NetError> {
                self.check_quota()?;
                let owner = self.owner;
                let (id, bound) = self.bind(FirewallProtocol::Tcp, local, |s| s.tcp_bind(local, v6only, owner))?;
                self.track(Conn::bound(socket, id, owner, self.service, self.app.clone()));
                Ok(bound)
            }

            fn udp_bind(&mut self, local: SocketAddr, v6only: bool, socket: Channel) -> Result<SocketAddr, NetError> {
                self.check_quota()?;
                let owner = self.owner;
                let (id, bound) = self.bind(FirewallProtocol::Udp, local, |s| s.udp_bind(local, v6only, owner))?;
                self.track(Conn::udp(socket, id, owner, true, self.app.clone()));
                self.listens(FirewallProtocol::Udp, local, bound);
                Ok(bound)
            }

            fn udp_bind_unmetered(
                &mut self,
                local: SocketAddr,
                v6only: bool,
                socket: Channel,
            ) -> Result<SocketAddr, NetError> {
                self.check_quota()?;
                let owner = self.owner;
                let (id, bound) = self.bind(FirewallProtocol::Udp, local, |s| s.udp_bind(local, v6only, owner))?;
                self.track(Conn::udp(socket, id, owner, false, self.app.clone()));
                self.listens(FirewallProtocol::Udp, local, bound);
                Ok(bound)
            }

            fn ping_open(&mut self, socket: Channel) -> Result<(), NetError> {
                self.check_quota()?;
                // Pings reach other machines alone.
                self.may_reach(1, SocketAddr::new(IpAddr::V4(core::net::Ipv4Addr::UNSPECIFIED), 0))?;
                let id = self.network.stack.ping_open(self.owner)?;
                self.track(Conn::ping(socket, id, self.owner, self.app.clone()));
                Ok(())
            }

            fn resolve(&mut self, name: String, family: AddrFamily, reply: Channel) -> Result<(), NetError> {
                let mine = self.network.usage.get(&self.owner).map_or(0, |u| u.lookups);
                let shared = room(self.service, self.network.lookups.len(), MAX_PENDING_LOOKUPS, RESERVED_LOOKUPS);
                if !shared || mine >= MAX_LOOKUPS_PER_PROCESS {
                    return Err(NetError::LimitReached);
                }
                // A lookup asks the DNS servers, which the name reaches: an
                // application kept off the network could tell them anything
                // through it. What the machine knows itself it may ask.
                if name != "localhost" && name.parse::<IpAddr>().is_err() {
                    self.may_reach(17, SocketAddr::new(IpAddr::V4(core::net::Ipv4Addr::UNSPECIFIED), 53))?;
                }
                let id = self.network.stack.resolve(&name, family)?;
                self.network.usage(self.owner).lookups += 1;
                self.network.lookups.insert(id, Lookup { reply, owner: self.owner });
                Ok(())
            }

            fn status(&mut self) -> NetStatus {
                self.network.stack.status()
            }

            fn interfaces(&mut self) -> Vec<InterfaceInfo> {
                let mut list = self.network.stack.interfaces();
                if !self.manager {
                    // A hardware address identifies the machine wherever it
                    // goes: not every program needs it.
                    for i in &mut list {
                        i.mac = [0; 6];
                    }
                }
                list
            }

            fn routes(&mut self) -> Vec<RouteInfo> {
                self.network.stack.routes()
            }

            fn sockets(&mut self) -> Vec<SocketInfo> {
                self.network.stack.socket_infos(if self.manager { None } else { Some(self.owner) })
            }

            fn watch(&mut self, events: Channel) -> Result<(), NetError> {
                let mine = self.network.watchers.iter().filter(|w| w.owner == self.owner).count();
                let shared = room(self.service, self.network.watchers.len(), MAX_WATCHERS, RESERVED_WATCHERS);
                if !shared || mine >= MAX_WATCHERS_PER_PROCESS {
                    return Err(NetError::LimitReached);
                }
                let status = self.network.stack.status();
                let _ = ipc::send_event(&events, NET_EVENT, NetEvent::StatusChanged { status });
                self.network.usage(self.owner).watchers += 1;
                self.network.watchers.push(Watcher { channel: events, owner: self.owner });
                Ok(())
            }

            fn configure(&mut self, interface: String, config: IpConfig) -> Result<(), NetError> {
                self.check_settings()?;
                let id = self.network.stack.find_interface(&interface).ok_or(NetError::InvalidArgument)?;
                println!("{}: configuration set to {:?}", interface, config);
                self.network.stack.configure(id, config)
            }

            fn renew(&mut self, interface: String) -> Result<(), NetError> {
                self.check_settings()?;
                let id = self.network.stack.find_interface(&interface).ok_or(NetError::InvalidArgument)?;
                self.network.stack.renew(id)
            }

            fn set_dns_servers(&mut self, servers: Vec<IpAddr>) -> Result<(), NetError> {
                self.check_settings()?;
                self.network.stack.set_dns_servers(servers)
            }

            fn flush_dns_cache(&mut self) -> Result<(), NetError> {
                self.check_manager()?;
                self.network.stack.flush_dns_cache();
                Ok(())
            }

            fn firewall(&mut self) -> Result<FirewallConfig, NetError> {
                self.check_manager()?;
                Ok(self.network.firewall.clone())
            }

            fn set_firewall(&mut self, config: FirewallConfig) -> Result<(), NetError> {
                self.check_settings()?;
                config.check()?;
                println!("firewall: settings changed by {}: {}", self.app, summary(&config));
                self.network.firewall_changed = true;
                self.network.apply_firewall(config);
                if let Some(store) = &self.network.store {
                    store.save(&self.network.firewall);
                }
                Ok(())
            }

            fn firewall_log(&mut self) -> Result<Vec<FirewallEvent>, NetError> {
                self.check_manager()?;
                // The stack's clock is the network service's: when each
                // happened, in Unix time.
                let (now, unix_s) = (now_us(), rt::time::unix_time_ns() / 1_000_000_000);
                let at = |us: u64| unix_s.saturating_sub(now.saturating_sub(us) / 1_000_000);
                Ok(self
                    .network
                    .stack
                    .firewall_log()
                    .map(|e| FirewallEvent { at: at(e.at_us), ..e.event.clone() })
                    .collect())
            }

            fn firewall_stats(&mut self) -> Result<FirewallStats, NetError> {
                self.check_manager()?;
                Ok(self.network.stack.firewall_stats())
            }

            fn firewall_questions(&mut self) -> Result<Vec<FirewallQuestion>, NetError> {
                self.check_manager()?;
                Ok(self.network.questions.values().cloned().collect())
            }

            /// The desktop shell's channel for the questions (a shell
            /// started again takes the place of the one before).
            fn watch_firewall(&mut self, events: Channel) -> Result<(), NetError> {
                self.check_shell()?;
                for question in self.network.questions.values() {
                    let _ =
                        ipc::send_event(&events, FIREWALL_NOTICE, FirewallNotice::Asked { question: question.clone() });
                }
                self.network.asker = Some(events);
                Ok(())
            }

            /// The user's answer, as the desktop shell asked: the rule for
            /// the application asked about, and nothing else.
            fn answer_firewall(&mut self, app: String, from: Option<Scope>) -> Result<(), NetError> {
                self.check_shell()?;
                if !self.network.questions.contains_key(&app) {
                    return Err(NetError::InvalidArgument);
                }
                let mut config = self.network.firewall.clone();
                config.incoming.retain(|r| r.app != app);
                config.incoming.push(AppRule { app: app.clone(), from });
                config.check()?;
                match from {
                    Some(scope) => println!("firewall: {app} may accept connections from {scope} (the user's answer)"),
                    None => println!("firewall: {app} may not accept connections (the user's answer)"),
                }
                self.network.firewall_changed = true;
                self.network.apply_firewall(config);
                if let Some(store) = &self.network.store {
                    store.save(&self.network.firewall);
                }
                Ok(())
            }
        }
        for _ in 0..CLIENT_BUDGET {
            let Some(c) = self.clients.get(&key) else { return };
            let (owner, manager, settings, shell, service) =
                (c.who.koid, c.manager, c.settings, c.shell, c.who.service);
            let app = c.who.name.clone();
            let msg = match c.channel.read() {
                Ok(m) => m,
                Err(_) => return,
            };
            let mut s = Session { network: self, owner, manager, settings, shell, service, app };
            match net::dispatch(&mut s, msg) {
                Ok(reply) => {
                    if let Some(c) = self.clients.get(&key) {
                        let _ = reply.send(&c.channel);
                    }
                }
                Err(_) => {
                    // Not even a message header: drop the client.
                    self.remove_client(key);
                    return;
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // Sockets, lookups, events

    /// Attends to the sockets with something to do: those the stack reports
    /// ready, those whose client wrote, made room or is new, and those that
    /// linger. A socket waiting for its client's room is left alone until it
    /// has it.
    fn pump_sockets(&mut self) {
        let mut work = core::mem::take(&mut self.work);
        work.extend(self.stack.take_ready());
        work.extend(self.lingering.keys());
        let now = now_us();
        let mut accepted = Vec::new();
        for id in work {
            let Some(conn) = self.sockets.get_mut(&id) else { continue };
            if conn.blocked {
                continue;
            }
            let owner = conn.owner;
            let mut room = MAX_SOCKETS_PER_PROCESS.saturating_sub(self.usage.get(&owner).map_or(0, |u| u.sockets));
            conn.pump(&mut self.stack, &mut self.events, &mut room, &mut accepted);
            let (finished, lingers) = (conn.finished(&self.stack), conn.lingers());
            for conn in accepted.drain(..) {
                self.add_socket(conn);
            }
            // Its clients are gone, and all they sent is in the stack, which
            // finishes a connection; or a UDP socket's datagrams are sent, or
            // waited for long enough.
            if finished || self.lingering.get(&id).is_some_and(|&until| now >= until) {
                self.close_socket(id);
            } else if lingers {
                self.lingering.entry(id).or_insert(now + LINGER_US);
            }
        }
    }

    fn deliver_lookups(&mut self) {
        for id in self.stack.resolve_ready() {
            let Some(result) = self.stack.resolve_result(id) else { continue };
            if let Some(l) = self.lookups.remove(&id) {
                let _ = ipc::send_event(&l.reply, RESOLVE_RESULT, result);
                self.release(l.owner, |u| u.lookups -= 1);
            }
        }
    }

    /// A lookup's client went away.
    fn cancel_lookup(&mut self, id: ResolveId) {
        if let Some(l) = self.lookups.remove(&id) {
            self.stack.resolve_cancel(id);
            self.release(l.owner, |u| u.lookups -= 1);
        }
    }

    fn publish_events(&mut self) {
        for ev in self.stack.take_events() {
            let ev = match ev {
                StackEvent::StatusChanged(status) => {
                    println!(
                        "status: {:?}{}{}",
                        status.connectivity,
                        if status.default_interface.is_empty() {
                            String::new()
                        } else {
                            alloc::format!(" via {}", status.default_interface)
                        },
                        match status.default_gateway {
                            Some(g) => alloc::format!(" (gateway {g})"),
                            None => String::new(),
                        }
                    );
                    NetEvent::StatusChanged { status }
                }
                StackEvent::InterfaceChanged(name) => {
                    if let Some(info) = self.stack.interfaces().into_iter().find(|i| i.name == name) {
                        let addrs: Vec<String> =
                            info.addresses.iter().map(|a| alloc::format!("{}/{}", a.address, a.prefix_len)).collect();
                        let dns: Vec<String> = info.dns.iter().map(|d| alloc::format!("{d}")).collect();
                        println!(
                            "{}: link {}, addresses [{}], DNS [{}], DHCP {:?}",
                            name,
                            if info.link_up { "up" } else { "down" },
                            addrs.join(", "),
                            dns.join(", "),
                            info.dhcp.state
                        );
                    }
                    NetEvent::InterfaceChanged { name }
                }
            };
            let mut gone = Vec::new();
            self.watchers.retain(|w| {
                let closed =
                    matches!(ipc::send_event(&w.channel, NET_EVENT, ev.clone()), Err(ipc::IpcError::PeerClosed));
                if closed {
                    gone.push(w.owner);
                }
                !closed
            });
            for owner in gone {
                self.release(owner, |u| u.watchers -= 1);
            }
        }
    }

    // ---------------------------------------------------------------
    // The loop

    fn run(&mut self) -> ! {
        loop {
            if rt::time::now_ns().saturating_sub(self.last_reseed) >= RESEED_NS {
                // The stack is seeded already: a reseed the kernel cannot
                // give now waits for the next.
                let mut seed = [0u8; 32];
                if rt::object::try_random_bytes(&mut seed).is_ok() {
                    self.stack.add_entropy(&seed);
                }
                self.last_reseed = rt::time::now_ns();
            }
            // Frames in and out, timers, housekeeping; then the clients.
            self.stack.poll(now_us(), &mut DeviceLinks(&mut self.devices));
            self.pump_sockets();
            self.deliver_lookups();
            self.publish_events();
            self.wait();
        }
    }

    /// Waits for the next thing to do, and does it.
    fn wait(&mut self) {
        // The set is kept from one wait to the next: its handles stay
        // registered with the kernel, so that a wait costs what is ready,
        // however many sockets there are.
        let mut ws = core::mem::take(&mut self.ws);
        ws.clear();
        ws.add(self.net_listener.raw(), signals::READABLE, NET_LISTENER);
        ws.add(self.dev_listener.raw(), signals::READABLE, DEV_LISTENER);
        // Something to do at once: frames waiting, a full link with room
        // again, or sockets the stack reported meanwhile.
        let mut busy = !self.work.is_empty();
        for (&k, d) in &self.devices {
            ws.add(d.channel.raw(), signals::READABLE | signals::PEER_CLOSED, DEVICE | k);
            if let (Some(ring), Some(id)) = (&d.link, d.iface) {
                // A link the stack could not send everything to wakes us
                // when it has room again.
                if ring.link.prepare_wait(self.stack.link_blocked(id)) {
                    ws.add(ring.link.wake_event().raw(), signals::SIGNALED, DEVICE_LINK | k);
                } else {
                    busy = true;
                }
            }
        }
        for (&k, c) in &self.clients {
            ws.add(c.channel.raw(), signals::READABLE | signals::PEER_CLOSED, CLIENT | k);
        }
        for (&id, s) in &self.sockets {
            let mut want = signals::PEER_CLOSED;
            if s.wants_requests() {
                want |= signals::READABLE;
            }
            if s.blocked {
                want |= signals::WRITABLE;
            }
            if s.channel_heard() {
                ws.add(s.channel.raw(), want, SOCKET | id as u64);
            }
            if let Some((end, want)) = s.bytes_wait() {
                ws.add(end, want, BYTES | id as u64);
            }
        }
        for (&id, l) in &self.lookups {
            ws.add(l.reply.raw(), signals::PEER_CLOSED, LOOKUP | id as u64);
        }
        if let Some(store) = &self.store {
            ws.add(store.channel().raw(), signals::READABLE | signals::PEER_CLOSED, STORE);
        }
        let deadline = if busy {
            0
        } else {
            // A lingering socket is dropped in time, whatever the stack waits
            // for.
            let lingering = self.lingering.values().min().copied();
            match self.stack.next_deadline().into_iter().chain(lingering).min() {
                Some(t) => t.saturating_mul(1000),
                None => abi::DEADLINE_INFINITE,
            }
        };
        let ready = ws.wait(deadline).unwrap_or_default();
        self.ws = ws;
        for d in self.devices.values() {
            if let Some(ring) = &d.link {
                ring.link.finish_wait();
            }
        }
        for (key, observed) in ready {
            let k = key & KEY;
            match key & !KEY {
                _ if key == NET_LISTENER => self.accept_clients(),
                _ if key == DEV_LISTENER => self.accept_devices(),
                _ if key == STORE => self.store_heard(observed),
                // Frames or room: the next poll sees to them.
                DEVICE_LINK => {}
                DEVICE => {
                    if observed & signals::READABLE != 0 {
                        self.handle_device(k);
                    }
                    if observed & signals::PEER_CLOSED != 0 && observed & signals::READABLE == 0 {
                        self.remove_device(k);
                    }
                }
                CLIENT => {
                    if observed & signals::READABLE != 0 {
                        self.handle_client(k);
                    } else if observed & signals::PEER_CLOSED != 0 {
                        self.remove_client(k);
                    }
                }
                SOCKET => self.socket_signalled(k as SockId, observed),
                BYTES => {
                    if let Some(conn) = self.sockets.get_mut(&(k as SockId)) {
                        conn.bytes_signalled(observed);
                        self.work.insert(k as SockId);
                    }
                }
                LOOKUP => self.cancel_lookup(k as ResolveId),
                _ => {}
            }
        }
    }

    fn socket_signalled(&mut self, id: SockId, observed: u32) {
        let Some(conn) = self.sockets.get_mut(&id) else { return };
        if observed & signals::WRITABLE != 0 {
            conn.blocked = false;
            self.work.insert(id);
        }
        let mut open = true;
        if observed & signals::READABLE != 0 {
            open = conn.handle_requests(&mut self.stack);
            self.work.insert(id);
        }
        if !open || (observed & signals::PEER_CLOSED != 0 && observed & signals::READABLE == 0) {
            // A connection whose bytes its clients still hold, or that has
            // theirs still to send, goes on without its channel, as a UDP
            // socket with datagrams to send does.
            if conn.outlives_channel(&self.stack) {
                conn.forget_channel();
                self.work.insert(id);
            } else {
                self.close_socket(id);
            }
        } else if let Some(turn) = conn.take_turn() {
            self.turn(id, turn);
        }
    }

    /// Does what the client of a bound socket asked to make of it (the
    /// firewall having its say), then reads on.
    fn turn(&mut self, id: SockId, turn: Turn) {
        let Some(conn) = self.sockets.get_mut(&id) else { return };
        let app = conn.app.clone();
        match turn {
            Turn::Listen { backlog, options, reply } => {
                let answer = match conn.bound_by_service() {
                    Some(service) => {
                        let listening = self.stack.tcp_listen_bound(id, backlog, &options);
                        if let Ok(local) = listening {
                            conn.listening();
                            let from = self.firewall.incoming_for(&app);
                            self.stack.set_exposure(id, from, &app);
                            self.listens(&app, service, FirewallProtocol::Tcp, local, local);
                        }
                        listening
                    }
                    // Listening already (another of the processes that share
                    // it asked first).
                    None => self.stack.tcp_listener_address(id).ok_or(NetError::InvalidArgument),
                };
                let answer = match answer {
                    Ok(local) => SocketEvent::Listening { local },
                    Err(error) => SocketEvent::Error { error },
                };
                // An answer nobody waits for any more is no matter.
                let _ = ipc::send_event(&reply, 0, answer);
            }
            Turn::Connect { remote, options, bytes } => {
                let connecting =
                    self.may_reach(&app, 6, remote).and_then(|()| self.stack.tcp_connect_bound(id, remote, &options));
                let Some(conn) = self.sockets.get_mut(&id) else { return };
                match connecting {
                    Ok(()) => conn.connecting(bytes),
                    // Reported on the channel, as a connection's failure is.
                    Err(error) => conn.fail(error),
                }
            }
        }
        // What it has to say now (a connection's failure); the channel's
        // next requests are read when the service next waits.
        self.work.insert(id);
    }

    /// Closes the sockets on `port` (of `protocol`) whose clients closed
    /// them, which the service has not seen to yet: a program that closes a
    /// socket and binds its port again finds it free, as on Linux (the close
    /// came first).
    fn reap(&mut self, protocol: FirewallProtocol, port: u16) {
        for id in self.stack.port_holders(protocol, port) {
            let observed = match self.sockets.get(&id) {
                Some(conn) if conn.channel_heard() => conn.channel.wait(signals::PEER_CLOSED, 0).unwrap_or(0),
                _ => 0,
            };
            if observed & signals::PEER_CLOSED != 0 {
                self.socket_signalled(id, observed);
            }
        }
    }
}

/// The firewall's settings, in a line of the log.
fn summary(c: &FirewallConfig) -> String {
    if !c.enabled {
        return String::from("off");
    }
    let ping = match &c.ping {
        None => String::from("pings unanswered"),
        Some(s) => alloc::format!("pings answered from {s}"),
    };
    let count = |n: usize, one: &str, many: &str| alloc::format!("{n} {}", if n == 1 { one } else { many });
    let let_in = c.incoming.iter().filter(|r| r.from.is_some()).count();
    let refused = c.incoming.len() - let_in;
    alloc::format!(
        "on{}{}, {ping}, {} let in{}, {} kept off the network, {}{}",
        if c.block_incoming { ", everything incoming blocked" } else { "" },
        if c.stealth { ", stealth" } else { "" },
        count(let_in, "application", "applications"),
        if refused > 0 { alloc::format!(", {refused} refused") } else { String::new() },
        count(c.blocked.len(), "application", "applications"),
        count(c.ports.len(), "port rule", "port rules"),
        if c.log { "" } else { ", no log" },
    )
}

fn main() -> i32 {
    let mut seed = [0u8; 64];
    rt::object::random_bytes(&mut seed);
    let mut stack = Stack::new(&seed, now_us());
    // Until the saved settings are read, the defaults: nothing comes in
    // that this machine did not ask for.
    let firewall = FirewallConfig::default();
    stack.set_firewall(firewall.clone());
    let store = store::Store::start();
    if store.is_none() {
        println!("firewall: its store could not start; the defaults hold, and changes are not kept");
    }
    let (Ok(net_listener), Ok(dev_listener)) = (proto::register(net::NAME), proto::register(netdev::NAME)) else {
        println!("cannot register the network services");
        return 1;
    };
    println!("network service ready");
    let mut network = Network {
        stack,
        ws: ipc::WaitSet::new(),
        net_listener,
        dev_listener,
        clients: BTreeMap::new(),
        devices: BTreeMap::new(),
        sockets: BTreeMap::new(),
        lookups: BTreeMap::new(),
        watchers: Vec::new(),
        usage: BTreeMap::new(),
        work: BTreeSet::new(),
        lingering: BTreeMap::new(),
        events: Events::new(),
        next_key: 16,
        last_reseed: rt::time::now_ns(),
        firewall,
        settings_read: store.is_none(),
        store,
        firewall_changed: false,
        questions: BTreeMap::new(),
        deferred: BTreeMap::new(),
        asker: None,
    };
    network.run()
}
