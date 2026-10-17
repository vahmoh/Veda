// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Network commands: `wifi`, `ifconfig`, `ping`, `nslookup`, `netstat`,
//! `firewall`, `route` and `curl`. They talk to the network and Wi-Fi
//! services through `net`; every wait is bounded, and the long ones
//! (pinging, scanning, joining a network) stop between steps after Ctrl+C.
//! `firewall` only reads: the programs run here speak for the Terminal, so
//! Settings alone changes the firewall.

use net::wifi::{self, ConnState, WlanEvent};
use net::{Duration, IpAddr, NetError};
use terminal_core::screen::{Style, color};

use crate::commands::pad;
use crate::shell::{INTERRUPTED, Io, Shell};

fn net_err(io: &mut Io, cmd: &str, e: NetError) -> i32 {
    let status = io.error(cmd, &format!("{e}"));
    if e == NetError::Unavailable {
        io.hint("The network service is not running.");
    }
    status
}

fn wifi_err(io: &mut Io, e: wifi::WifiError) -> i32 {
    io.error("wifi", &format!("{e}"))
}

fn bars(n: u8) -> &'static str {
    [
        "\u{2581}    ",
        "\u{2581}\u{2583}   ",
        "\u{2581}\u{2583}\u{2585}  ",
        "\u{2581}\u{2583}\u{2585}\u{2587} ",
        "\u{2581}\u{2583}\u{2585}\u{2587}\u{2588}",
    ][n.min(4) as usize]
}

fn print_status(io: &mut Io, s: &wifi::WlanStatus) {
    let state = match s.state {
        ConnState::NoAdapter => "no Wi-Fi adapter",
        ConnState::RadioOff => "off",
        ConnState::Disconnected => "disconnected",
        ConnState::Authenticating => "authenticating",
        ConnState::Associating => "associating",
        ConnState::Securing => "exchanging keys",
        ConnState::Connected => "connected",
    };
    io.styled("Wi-Fi: ", Style::BOLD);
    io.println(state);
    if s.state != ConnState::NoAdapter {
        io.println(&format!("  adapter   {} ({})", s.adapter, net::format_mac(&s.mac)));
    }
    if !s.ssid.0.is_empty()
        && matches!(
            s.state,
            ConnState::Connected | ConnState::Authenticating | ConnState::Associating | ConnState::Securing
        )
    {
        io.println(&format!("  network   {}", s.name));
        io.println(&format!("  security  {}", s.security.label()));
        io.println(&format!("  access pt {} on channel {}", net::format_mac(&s.bssid), s.channel));
        io.println(&format!("  signal    {} dBm {}", s.signal_dbm, bars(wifi::signal_bars(s.signal_dbm))));
        if s.state == ConnState::Connected {
            io.println(&format!("  connected {} s", s.connected_s));
        }
    }
    if let Some(f) = s.last_failure {
        io.styled(&format!("  last failure: {f}\n"), Style::DIM);
    }
}

fn print_networks(io: &mut Io, nets: &[wifi::NetworkInfo]) {
    if nets.is_empty() {
        io.println("No networks found.");
        return;
    }
    let w = nets.iter().map(|n| n.name.chars().count()).max().unwrap_or(4).max(4) + 2;
    io.styled(&format!("  {}SIGNAL         SECURITY\n", pad("NETWORK", w)), Style::DIM);
    for n in nets {
        let mark = if n.connected { "*" } else { " " };
        let line = format!(
            "{mark} {}{:>4} dBm {}  {}{}",
            pad(&n.name, w),
            n.signal_dbm,
            bars(n.bars),
            n.security.label(),
            if n.saved { " (saved)" } else { "" }
        );
        if n.connected {
            io.styled(&format!("{line}\n"), Style::fg(color::BRIGHT_GREEN));
        } else {
            io.println(&line);
        }
    }
}

/// Starts a scan and waits (briefly) for it to finish.
fn scan_and_wait(sh: &Shell, io: &mut Io) -> Result<(), i32> {
    let watcher = wifi::Watcher::new().map_err(|e| wifi_err(io, e))?;
    wifi::scan().map_err(|e| wifi_err(io, e))?;
    let end = rt::time::now_ns() + 8_000_000_000;
    while rt::time::now_ns() < end {
        if sh.interrupted() {
            return Err(INTERRUPTED);
        }
        match watcher.next(Duration::from_millis(500)) {
            Ok(Some(WlanEvent::ScanDone {})) => return Ok(()),
            Ok(_) => {}
            Err(e) => return Err(wifi_err(io, e)),
        }
    }
    Ok(())
}

const WIFI_USAGE: &str = "\
usage: wifi [status]                 show the connection
       wifi scan | list              look for networks / list them
       wifi connect SSID [PASSWORD]  join a network (saved for next time)
       wifi disconnect
       wifi saved | forget SSID | auto SSID on|off
       wifi on | off                 switch the radio
       wifi aps | log                access points / diagnostics";

pub fn cmd_wifi(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let sub = args.get(1).map(String::as_str).unwrap_or("status");
    match sub {
        "status" => match wifi::status() {
            Ok(s) => {
                print_status(io, &s);
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "scan" => {
            if let Err(code) = scan_and_wait(sh, io) {
                return code;
            }
            match wifi::networks() {
                Ok(n) => {
                    print_networks(io, &n);
                    0
                }
                Err(e) => wifi_err(io, e),
            }
        }
        "list" | "networks" => match wifi::networks() {
            Ok(n) => {
                print_networks(io, &n);
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "connect" => {
            let Some(ssid) = args.get(2) else {
                return io.error("wifi", "connect: which network? (wifi connect SSID [PASSWORD])");
            };
            let password = args.get(3).map(String::as_str);
            let watcher = wifi::Watcher::new().ok();
            if let Err(e) = wifi::connect(ssid, password, true) {
                return wifi_err(io, e);
            }
            io.styled(&format!("Joining {ssid}\u{2026}\n"), Style::DIM);
            // Wait for the outcome.
            let end = rt::time::now_ns() + 30_000_000_000;
            loop {
                if sh.interrupted() {
                    return INTERRUPTED;
                }
                if let Some(w) = &watcher
                    && let Ok(Some(WlanEvent::ConnectFailed { reason, .. })) = w.next(Duration::from_millis(200))
                {
                    return io.error("wifi", &format!("could not join {ssid}: {reason}"));
                }
                match wifi::status() {
                    Ok(s) if s.state == ConnState::Connected && s.name == *ssid => {
                        io.styled(&format!("Connected to {ssid}",), Style::fg(color::BRIGHT_GREEN));
                        io.println(&format!(" ({}, channel {}, {} dBm)", s.security.label(), s.channel, s.signal_dbm));
                        return 0;
                    }
                    Ok(_) => {}
                    Err(e) => return wifi_err(io, e),
                }
                if rt::time::now_ns() > end {
                    return io.error("wifi", "no answer yet; 'wifi status' shows the progress");
                }
                if watcher.is_none() {
                    rt::time::sleep(Duration::from_millis(200));
                }
            }
        }
        "disconnect" => match wifi::disconnect() {
            Ok(()) => {
                io.println("Disconnected.");
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "saved" => match wifi::saved() {
            Ok(list) if list.is_empty() => {
                io.println("No saved networks.");
                0
            }
            Ok(list) => {
                for s in list {
                    io.println(&format!(
                        "{}  {}{}{}",
                        s.name,
                        s.security.label(),
                        if s.auto_connect { ", connects automatically" } else { "" },
                        if s.hidden { ", hidden" } else { "" }
                    ));
                }
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "forget" => {
            let Some(ssid) = args.get(2) else { return io.error("wifi", "forget: which network?") };
            match wifi::forget(ssid.as_bytes()) {
                Ok(()) => {
                    io.println(&format!("Forgot {ssid}."));
                    0
                }
                Err(e) => wifi_err(io, e),
            }
        }
        "auto" => {
            let (Some(ssid), Some(v)) = (args.get(2), args.get(3)) else {
                return io.error("wifi", "usage: wifi auto SSID on|off");
            };
            let on = match v.as_str() {
                "on" | "yes" | "1" => true,
                "off" | "no" | "0" => false,
                _ => return io.error("wifi", "usage: wifi auto SSID on|off"),
            };
            match wifi::set_auto_connect(ssid.as_bytes(), on) {
                Ok(()) => 0,
                Err(e) => wifi_err(io, e),
            }
        }
        "on" | "off" => match wifi::set_radio(sub == "on") {
            Ok(()) => {
                io.println(if sub == "on" { "Wi-Fi is on." } else { "Wi-Fi is off." });
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "aps" => match wifi::access_points() {
            Ok(list) => {
                io.styled("BSSID              CH  SIGNAL  AGE   SECURITY          SSID\n", Style::DIM);
                for b in list {
                    io.println(&format!(
                        "{}  {:>3}  {:>4} dBm {:>4}s  {}{}",
                        net::format_mac(&b.bssid),
                        b.channel,
                        b.signal_dbm,
                        b.age_ms / 1000,
                        pad(b.security.label(), 18),
                        if b.ssid.0.is_empty() { String::from("(hidden)") } else { wifi::ssid_display(&b.ssid.0) }
                    ));
                }
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "log" | "diag" | "diagnostics" => match wifi::diagnostics() {
            Ok(d) => {
                print_status(io, &d.status);
                let c = d.counters;
                io.println(&format!(
                    "  scans {}, attempts {}, connections {}, disconnections {}, authentication failures {}",
                    c.scans, c.connect_attempts, c.connections, c.disconnections, c.auth_failures
                ));
                io.println(&format!(
                    "  frames sent {}, received {}, decryption errors {}, replays {}, unprotected dropped {}, rekeys {}, roams {}",
                    c.data_tx, c.data_rx, c.decrypt_errors, c.replays, c.unprotected_dropped, c.group_rekeys, c.roams
                ));
                for e in d.log {
                    io.styled(&format!("[{:>6}.{:03}] ", e.time_ms / 1000, e.time_ms % 1000), Style::DIM);
                    io.println(&e.message);
                }
                0
            }
            Err(e) => wifi_err(io, e),
        },
        "help" | "-h" | "--help" => {
            io.println(WIFI_USAGE);
            0
        }
        other => {
            io.error("wifi", &format!("unknown command '{other}'"));
            io.hint(WIFI_USAGE);
            2
        }
    }
}

pub fn cmd_ifconfig(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let all = args.iter().any(|a| a == "-a");
    let list = match net::interfaces() {
        Ok(l) => l,
        Err(e) => return net_err(io, "ifconfig", e),
    };
    for i in list.iter().filter(|i| all || i.kind != net::InterfaceKind::Loopback) {
        let kind = match i.kind {
            net::InterfaceKind::Loopback => "loopback",
            net::InterfaceKind::Wireless => "wireless",
            _ => "ethernet",
        };
        io.styled(&i.name, Style::BOLD);
        io.println(&format!(
            ": {kind}, link {}, mtu {}, {} ({})",
            if i.link_up { "up" } else { "down" },
            i.mtu,
            net::format_mac(&i.mac),
            i.driver
        ));
        for a in &i.addresses {
            let fam = if a.address.is_ipv4() { "inet " } else { "inet6" };
            io.println(&format!("    {fam} {}/{} ({:?})", a.address, a.prefix_len, a.origin));
        }
        if !i.gateways.is_empty() {
            let g: Vec<String> = i.gateways.iter().map(|g| g.to_string()).collect();
            io.println(&format!("    gateway {}", g.join(", ")));
        }
        if !i.dns.is_empty() {
            let d: Vec<String> = i.dns.iter().map(|d| d.to_string()).collect();
            io.println(&format!("    dns     {}", d.join(", ")));
        }
        io.println(&format!("    dhcp    {:?}, lease {} s", i.dhcp.state, i.dhcp.lease_s));
        let s = i.stats;
        io.styled(
            &format!(
                "    rx {} packets ({} bytes, {} dropped, {} errors), tx {} packets ({} bytes, {} dropped)\n",
                s.rx_packets, s.rx_bytes, s.rx_dropped, s.rx_errors, s.tx_packets, s.tx_bytes, s.tx_dropped
            ),
            Style::DIM,
        );
    }
    match net::status() {
        Ok(s) => {
            io.println(&format!(
                "status: {:?}{}",
                s.connectivity,
                if s.default_interface.is_empty() { String::new() } else { format!(" via {}", s.default_interface) }
            ));
            0
        }
        Err(e) => net_err(io, "ifconfig", e),
    }
}

pub fn cmd_ping(sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let mut count = 4u16;
    let mut host = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" | "-n" => count = it.next().and_then(|v| v.parse().ok()).unwrap_or(4).clamp(1, 20),
            h => host = Some(h.to_string()),
        }
    }
    let Some(host) = host else { return io.error("ping", "usage: ping HOST [-c COUNT]") };
    let target: IpAddr = match host.parse() {
        Ok(a) => a,
        Err(_) => match net::resolve(&host) {
            Ok(list) if !list.is_empty() => list.iter().find(|a| a.is_ipv4()).copied().unwrap_or(list[0]),
            Ok(_) => return io.error("ping", &format!("{host}: no addresses")),
            Err(e) => return net_err(io, "ping", e),
        },
    };
    let mut p = match net::Pinger::new() {
        Ok(p) => p,
        Err(e) => return net_err(io, "ping", e),
    };
    io.println(&format!("PING {host} ({target}): 56 data bytes"));
    let mut rtts = Vec::new();
    let mut sent = 0u16;
    for seq in 1..=count {
        // Stopped with Ctrl+C: the summary covers what was sent.
        if sh.interrupted() {
            break;
        }
        sent += 1;
        match p.ping(target, seq, 56, 0) {
            Ok(r) => {
                rtts.push(r.rtt_us);
                io.println(&format!(
                    "{} bytes from {}: seq={} time={}.{:03} ms",
                    r.size + 8,
                    r.from,
                    r.seq,
                    r.rtt_us / 1000,
                    r.rtt_us % 1000
                ));
            }
            Err(e) => io.styled(&format!("seq={seq}: {e}\n"), Style::fg(color::YELLOW)),
        }
    }
    let lost = sent as usize - rtts.len();
    io.println(&format!(
        "--- {host}: {sent} sent, {} received, {}% loss",
        rtts.len(),
        lost * 100 / (sent as usize).max(1)
    ));
    if let (Some(min), Some(max)) = (rtts.iter().min(), rtts.iter().max()) {
        let avg = rtts.iter().map(|&x| x as u64).sum::<u64>() / rtts.len() as u64;
        io.println(&format!(
            "round trip min/avg/max = {}.{:03}/{}.{:03}/{}.{:03} ms",
            min / 1000,
            min % 1000,
            avg / 1000,
            avg % 1000,
            max / 1000,
            max % 1000
        ));
    }
    if rtts.is_empty() { 1 } else { 0 }
}

pub fn cmd_nslookup(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let Some(name) = args.get(1) else { return io.error("nslookup", "usage: nslookup NAME") };
    match net::lookup(name, net::AddrFamily::Any, net::RESOLVE_TIMEOUT) {
        Ok(l) => {
            if l.canonical != *name && !l.canonical.is_empty() {
                io.println(&format!("{name} is an alias for {}", l.canonical));
            }
            for a in &l.addresses {
                io.println(&format!("{name} has address {a}"));
            }
            io.styled(&format!("(cached for {} s)\n", l.ttl_s), Style::DIM);
            0
        }
        Err(e) => net_err(io, "nslookup", e),
    }
}

pub fn cmd_netstat(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    match net::sockets() {
        Ok(list) => {
            io.styled("PROTO  LOCAL                    REMOTE                   STATE         PID\n", Style::DIM);
            for s in list {
                let a = |x: Option<net::SocketAddr>| x.map(|a| a.to_string()).unwrap_or_else(|| String::from("*"));
                io.println(&format!(
                    "{}  {}{}{}{}",
                    pad(&format!("{:?}", s.kind).to_ascii_lowercase(), 5),
                    pad(&a(s.local), 25),
                    pad(&a(s.remote), 25),
                    pad(&s.state, 14),
                    s.owner
                ));
            }
            0
        }
        Err(e) => net_err(io, "netstat", e),
    }
}

/// `firewall [log | stats]`: the firewall's settings, what it turned away
/// lately, or its counts.
pub fn cmd_firewall(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    let result = match args.get(1).map(String::as_str) {
        None | Some("show") => net::firewall().map(|c| {
            let questions = net::firewall_questions().unwrap_or_default();
            show_firewall(io, &c, &questions)
        }),
        Some("log") => net::firewall_log().map(|log| show_firewall_log(io, &log)),
        Some("stats") => net::firewall_stats().map(|s| show_firewall_stats(io, &s)),
        Some(other) => {
            return io.error("firewall", &format!("no subcommand '{other}': usage: firewall [log | stats]"));
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => net_err(io, "firewall", e),
    }
}

fn show_firewall(io: &mut Io, c: &net::FirewallConfig, questions: &[net::FirewallQuestion]) {
    let onoff = |b: bool| if b { "on" } else { "off" };
    io.println(&format!("Firewall: {}", onoff(c.enabled)));
    if c.enabled {
        let incoming = if c.block_incoming {
            "everything blocked: only replies come in"
        } else {
            "other machines reach only what is allowed below"
        };
        io.println(&format!("  Incoming: {incoming}"));
        io.println(&format!("  Stealth:  {} (closed ports and scans get no answer)", onoff(c.stealth)));
        let pings = match &c.ping {
            None => String::from("unanswered"),
            Some(s) => format!("answered from {s}"),
        };
        io.println(&format!("  Pings:    {pings}"));
        io.println(&format!("  Log:      {}", onoff(c.log)));
    }
    if !questions.is_empty() {
        io.styled("Waiting for your answer (on the desktop, or in Settings):\n", Style::DIM);
        for q in questions {
            let proto = if q.protocol == net::FirewallProtocol::Tcp { "TCP" } else { "UDP" };
            io.println(&format!("  {}asks to accept connections ({proto} port {})", pad(&q.app, 16), q.port));
        }
    }
    if !c.incoming.is_empty() {
        io.styled("Applications that accept connections, or not:\n", Style::DIM);
        for r in &c.incoming {
            match &r.from {
                Some(scope) => io.println(&format!("  {}from {scope}", pad(&r.app, 16))),
                None => io.println(&format!("  {}not let in", pad(&r.app, 16))),
            }
        }
    }
    if !c.blocked.is_empty() {
        io.styled("Applications kept off the network:\n", Style::DIM);
        for app in &c.blocked {
            io.println(&format!("  {app}"));
        }
    }
    if !c.ports.is_empty() {
        io.styled("Port rules:\n", Style::DIM);
        for r in &c.ports {
            let proto = if r.protocol == net::FirewallProtocol::Tcp { "TCP" } else { "UDP" };
            let ports = if r.first == r.last { format!("{}", r.first) } else { format!("{}-{}", r.first, r.last) };
            let verdict = if r.allow { "open" } else { "closed" };
            io.println(&format!("  {}{verdict} from {}", pad(&format!("{proto} {ports}"), 16), r.from));
        }
    }
    io.hint("Settings changes the firewall (Network & Internet).");
}

fn show_firewall_log(io: &mut Io, log: &[net::FirewallEvent]) {
    if log.is_empty() {
        io.println("Nothing turned away.");
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    io.styled(
        "AGO     REASON          PROTO   REMOTE                                  PORT   APP          COUNT\n",
        Style::DIM,
    );
    for e in log.iter().rev() {
        let reason = match e.reason {
            net::FirewallReason::NotAllowed => "not allowed",
            net::FirewallReason::Probe => "probe",
            net::FirewallReason::Ping => "ping",
            net::FirewallReason::Invalid => "invalid",
            net::FirewallReason::RateLimited => "rate limited",
            net::FirewallReason::Fragment => "fragment",
            net::FirewallReason::AppBlocked => "app blocked",
        };
        let proto = match e.protocol {
            6 => String::from("tcp"),
            17 => String::from("udp"),
            1 => String::from("icmp"),
            58 => String::from("icmpv6"),
            n => format!("ip/{n}"),
        };
        let port = if e.port == 0 { String::from("-") } else { e.port.to_string() };
        let app = if e.app.is_empty() { "-" } else { e.app.as_str() };
        io.println(&format!(
            "{}{}{}{}{}{}{}",
            pad(&format!("{}s", now.saturating_sub(e.at)), 8),
            pad(reason, 16),
            pad(&proto, 8),
            pad(&e.remote.to_string(), 40),
            pad(&port, 7),
            pad(app, 13),
            e.count
        ));
    }
}

fn show_firewall_stats(io: &mut Io, s: &net::FirewallStats) {
    for (label, n) in [
        ("Let in by a rule", s.allowed),
        ("Not allowed", s.not_allowed),
        ("Probes of closed ports", s.probes),
        ("Pings unanswered", s.pings),
        ("Invalid or forged", s.invalid),
        ("Too many at once", s.rate_limited),
        ("Fragments", s.fragments),
        ("From applications kept off", s.app_blocked),
    ] {
        io.println(&format!("{}{n}", pad(label, 30)));
    }
}

pub fn cmd_route(_sh: &mut Shell, io: &mut Io, _args: &[String]) -> i32 {
    match net::routes() {
        Ok(list) => {
            io.styled("DESTINATION                GATEWAY                  INTERFACE  METRIC\n", Style::DIM);
            for r in list {
                let dest = format!("{}/{}", r.destination, r.prefix_len);
                let gw = r.gateway.map(|g| g.to_string()).unwrap_or_else(|| String::from("(direct)"));
                io.println(&format!("{}{}{}{}", pad(&dest, 27), pad(&gw, 25), pad(&r.interface, 11), r.metric));
            }
            0
        }
        Err(e) => net_err(io, "route", e),
    }
}

pub fn cmd_curl(_sh: &mut Shell, io: &mut Io, args: &[String]) -> i32 {
    // What may be shown, and what may be saved (`curl URL > FILE`).
    const SHOWN: usize = 1 << 20;
    const SAVED: usize = 32 << 20;
    let head_only = args.iter().any(|a| a == "-I");
    let Some(url) = args.iter().skip(1).find(|a| !a.starts_with('-')) else {
        return io.error("curl", "usage: curl [-I] [http[s]://]HOST[:PORT][/PATH]");
    };
    let url = if url.contains("://") { url.clone() } else { format!("http://{url}") };
    let terminal = io.is_terminal();
    match web::http::get(&url, Duration::from_secs(20), if terminal { SHOWN } else { SAVED }) {
        Ok(r) => {
            if head_only {
                io.println(&format!("HTTP/1.1 {} {}", r.status, r.reason));
                for (k, v) in &r.headers {
                    io.println(&format!("{k}: {v}"));
                }
            } else if !terminal {
                // Into a file or a pipe: the body exactly as it came.
                io.write(&r.body);
            } else if terminal_core::text::is_binary(&r.body) {
                io.error("curl", &format!("binary data ({} bytes): save it with > FILE", r.body.len()));
            } else {
                io.write(&r.body);
                if !r.body.ends_with(b"\n") {
                    io.print("\n");
                }
            }
            if r.status >= 400 { 22 } else { 0 }
        }
        Err(e) => io.error("curl", &format!("{e}")),
    }
}
