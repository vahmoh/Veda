// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firewall, on the "Network & Internet" page: whether it is on, which
//! applications and ports it lets other machines reach, whether the
//! computer answers scans and pings, which applications it keeps off the
//! network, and what it turned away lately.
//!
//! The network service keeps the settings and checks that Settings alone
//! changes them; the voice agent has no action for them. What the page
//! offers first is the cautious choice: an application let in is let in
//! from the local network, and reaching it from anywhere takes another
//! click.
//!
//! An application that started taking connections, which the settings
//! say nothing about, is asked about on the desktop; the question waits at
//! the top of the applications here too, and a choice made here answers
//! it.

use std::time::{SystemTime, UNIX_EPOCH};

use vnet::{
    AppRule, FirewallConfig, FirewallEvent, FirewallProtocol, FirewallQuestion, FirewallReason, FirewallStats,
    NetError, PortRule, Scope,
};
use vui::{Align, ButtonKind, Font, Icon, Rect, Ui};

/// How often the log and the counts are read again while shown.
const REFRESH_NS: u64 = 2_000_000_000;
/// The latest things turned away that the page lists.
const SHOWN: usize = 10;

pub struct FirewallPanel {
    /// `None`: the network service did not say (not running).
    config: Option<FirewallConfig>,
    /// Applications waiting for the user's answer.
    questions: Vec<FirewallQuestion>,
    log: Vec<FirewallEvent>,
    stats: FirewallStats,
    new_app: String,
    new_ports: String,
    new_from: String,
    new_udp: bool,
    error: Option<String>,
    refresh_at: u64,
    dirty: bool,
}

/// What the user is told when a change does not go through.
fn explain(e: NetError) -> String {
    match e {
        NetError::PermissionDenied => String::from("Only Settings can change the firewall."),
        NetError::InvalidArgument => String::from("That is not a rule the firewall takes."),
        NetError::Unavailable => String::from("The network service is not running."),
        e => format!("The firewall did not take the change: {e:?}."),
    }
}

fn protocol_name(p: u8) -> String {
    match p {
        6 => String::from("TCP"),
        17 => String::from("UDP"),
        1 => String::from("ICMP"),
        58 => String::from("ICMPv6"),
        n => format!("IP protocol {n}"),
    }
}

fn reason_text(r: FirewallReason) -> &'static str {
    match r {
        FirewallReason::NotAllowed => "not allowed",
        FirewallReason::Probe => "probe of a closed port",
        FirewallReason::Ping => "ping",
        FirewallReason::Invalid => "invalid or forged",
        FirewallReason::RateLimited => "too many at once",
        FirewallReason::Fragment => "fragment",
        FirewallReason::AppBlocked => "kept off the network",
    }
}

fn ago(at: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    match now.saturating_sub(at) {
        0..=4 => String::from("just now"),
        s @ 5..=59 => format!("{s} s ago"),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

/// A port, or a range of them (`8000-8100`).
fn parse_ports(text: &str) -> Option<(u16, u16)> {
    let (first, last) = match text.trim().split_once('-') {
        Some((a, b)) => (a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?),
        None => {
            let p = text.trim().parse::<u16>().ok()?;
            (p, p)
        }
    };
    (first >= 1 && first <= last).then_some((first, last))
}

impl FirewallPanel {
    pub fn new() -> FirewallPanel {
        FirewallPanel {
            config: None,
            questions: Vec::new(),
            log: Vec::new(),
            stats: FirewallStats::default(),
            new_app: String::new(),
            new_ports: String::new(),
            new_from: String::new(),
            new_udp: false,
            error: None,
            refresh_at: 0,
            dirty: false,
        }
    }

    /// Reads the settings, the log and the counts again.
    pub fn refresh(&mut self) {
        self.config = vnet::firewall().ok();
        self.questions = vnet::firewall_questions().unwrap_or_default();
        self.log = vnet::firewall_log().unwrap_or_default();
        self.stats = vnet::firewall_stats().unwrap_or_default();
        self.dirty = true;
    }

    /// Refreshes when due; returns when to look again.
    pub fn poll(&mut self, now: u64) -> u64 {
        if now >= self.refresh_at {
            self.refresh();
            self.refresh_at = now + REFRESH_NS;
        }
        self.refresh_at
    }

    /// Changes the settings as `f` does, through the network service.
    fn change(&mut self, f: impl FnOnce(&mut FirewallConfig)) {
        let Some(mut c) = self.config.clone() else { return };
        f(&mut c);
        match vnet::set_firewall(&c) {
            Ok(()) => {
                self.error = None;
                self.config = Some(c);
            }
            Err(e) => self.error = Some(explain(e)),
        }
        self.refresh();
    }

    /// Draws the firewall's cards from `y`; returns where they end.
    pub fn draw(&mut self, ui: &mut Ui, x: i32, y: i32, w: i32) -> i32 {
        let Some(c) = self.config.clone() else {
            let t = ui.theme().clone();
            let card = Rect::new(x, y, w, 74);
            ui.card(card);
            ui.icon(Rect::new(card.x + 22, card.y + 14, 28, 44), Icon::Lock, 22.0, t.text_dim);
            ui.label(
                Rect::new(card.x + 62, card.y + 14, w - 84, 22),
                "Firewall",
                Font::Bold,
                t.font_size,
                t.text,
                Align::Left,
            );
            let msg = "The network service is not running: the firewall's settings cannot be read.";
            ui.label(
                Rect::new(card.x + 62, card.y + 36, w - 84, 20),
                msg,
                Font::Regular,
                t.small_size,
                t.text_dim,
                Align::Left,
            );
            return card.bottom() + 20;
        };
        let mut y = self.switches_card(ui, x, y, w, &c);
        if c.enabled {
            y = self.apps_card(ui, x, y, w, &c);
            y = self.ports_card(ui, x, y, w, &c);
        }
        y = self.activity_card(ui, x, y, w, &c);
        if core::mem::take(&mut self.dirty) {
            ui.repaint();
        }
        y
    }

    fn switches_card(&mut self, ui: &mut Ui, x: i32, y: i32, w: i32, c: &FirewallConfig) -> i32 {
        let t = ui.theme().clone();
        let rows = if c.enabled { 4 } else { 0 };
        let error_h = if self.error.is_some() { 30 } else { 0 };
        let card = Rect::new(x, y, w, 74 + rows * 50 + error_h + if rows > 0 { 10 } else { 0 });
        ui.card(card);
        let (ix, iw) = (card.x + 22, card.w - 44);
        ui.icon(Rect::new(ix, card.y + 14, 28, 44), Icon::Lock, 22.0, if c.enabled { t.accent } else { t.danger });
        ui.label(
            Rect::new(ix + 40, card.y + 14, iw - 120, 22),
            "Firewall",
            Font::Bold,
            t.font_size,
            t.text,
            Align::Left,
        );
        let status = if !c.enabled {
            "Off: other machines can reach any program that listens."
        } else if c.block_incoming {
            "On: only replies to what this computer sent come in."
        } else {
            "On: other machines reach only what you allow."
        };
        ui.label(
            Rect::new(ix + 40, card.y + 36, iw - 120, 20),
            status,
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        let mut on = c.enabled;
        if ui.toggle(Rect::new(card.right() - 22 - 44, card.y + 25, 44, 22), "settings-fw-enabled", &mut on) {
            self.change(|c| c.enabled = on);
        }
        let mut ry = card.y + 74;
        if c.enabled {
            let switch = |ui: &mut Ui, ry: i32, id: &str, title: &str, hint: &str, value: bool| -> Option<bool> {
                ui.label(Rect::new(ix, ry, iw - 70, 22), title, Font::Regular, t.font_size, t.text, Align::Left);
                ui.label(
                    Rect::new(ix, ry + 22, iw - 70, 20),
                    hint,
                    Font::Regular,
                    t.small_size,
                    t.text_dim,
                    Align::Left,
                );
                let mut v = value;
                ui.toggle(Rect::new(card.right() - 22 - 44, ry + 10, 44, 22), id, &mut v).then_some(v)
            };
            if let Some(v) = switch(
                ui,
                ry,
                "settings-fw-block",
                "Block all incoming connections",
                "Only replies come in, whatever the rules below allow.",
                c.block_incoming,
            ) {
                self.change(|c| c.block_incoming = v);
            }
            ry += 50;
            if let Some(v) = switch(
                ui,
                ry,
                "settings-fw-stealth",
                "Stealth mode",
                "Closed ports give no answer, so a scan finds nothing.",
                c.stealth,
            ) {
                self.change(|c| c.stealth = v);
            }
            ry += 50;
            if let Some(v) =
                switch(ui, ry, "settings-fw-log", "Keep a log", "Record what the firewall turns away.", c.log)
            {
                self.change(|c| c.log = v);
            }
            ry += 50;
            ui.label(Rect::new(ix, ry, iw - 330, 22), "Answer pings", Font::Regular, t.font_size, t.text, Align::Left);
            ui.label(
                Rect::new(ix, ry + 22, iw - 330, 20),
                "Let other machines check that this computer is there.",
                Font::Regular,
                t.small_size,
                t.text_dim,
                Align::Left,
            );
            let choices =
                [("No", None), ("Local network", Some(Scope::LocalNetwork {})), ("Anywhere", Some(Scope::Anywhere {}))];
            let mut bx = card.right() - 22 - 320;
            for (label, scope) in choices {
                let bw = if label == "Local network" { 130 } else { 90 };
                let kind = if c.ping == scope { ButtonKind::Primary } else { ButtonKind::Secondary };
                if ui.button_full(Rect::new(bx, ry + 6, bw - 6, 32), None, label, kind) && c.ping != scope {
                    self.change(|c| c.ping = scope);
                }
                bx += bw + 4;
            }
            ry += 50;
        }
        if let Some(e) = &self.error {
            ui.label(Rect::new(ix, ry + 4, iw, 22), e, Font::Regular, t.small_size, t.danger, Align::Left);
        }
        card.bottom() + 20
    }

    /// "No", "Local network" and "Anywhere" ending at `right` on the row at
    /// `ry`, `current` marked: the one clicked, if another.
    fn choice(ui: &mut Ui, right: i32, ry: i32, current: Option<Option<Scope>>) -> Option<Option<Scope>> {
        let choices =
            [("No", None), ("Local network", Some(Scope::LocalNetwork {})), ("Anywhere", Some(Scope::Anywhere {}))];
        let mut bx = right - 322;
        let mut picked = None;
        for (label, scope) in choices {
            let bw = if label == "Local network" { 130 } else { 90 };
            let kind = if current == Some(scope) { ButtonKind::Primary } else { ButtonKind::Secondary };
            if ui.button_full(Rect::new(bx, ry, bw - 6, 32), None, label, kind) && current != Some(scope) {
                picked = Some(scope);
            }
            bx += bw + 4;
        }
        picked
    }

    fn apps_card(&mut self, ui: &mut Ui, x: i32, y: i32, w: i32, c: &FirewallConfig) -> i32 {
        let t = ui.theme().clone();
        // The questions the settings have not answered meanwhile.
        let asking: Vec<FirewallQuestion> = self.questions.iter().filter(|q| c.undecided(&q.app)).cloned().collect();
        let rows = (asking.len() + c.incoming.len() + c.blocked.len()).max(1) as i32;
        let card = Rect::new(x, y, w, 88 + rows * 40 + 56);
        ui.card(card);
        let (ix, iw) = (card.x + 22, card.w - 44);
        ui.label(
            Rect::new(ix, card.y + 16, iw, 24),
            "Applications",
            Font::Bold,
            t.heading_size - 2.0,
            t.text,
            Align::Left,
        );
        ui.label(
            Rect::new(ix, card.y + 42, iw, 20),
            "Which applications other machines may connect to, and which may not use the network at all.",
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        let mut ry = card.y + 76;
        if asking.is_empty() && c.incoming.is_empty() && c.blocked.is_empty() {
            ui.label(
                Rect::new(ix, ry, iw, 32),
                "No application accepts connections from other machines.",
                Font::Regular,
                t.font_size,
                t.text_faint,
                Align::Left,
            );
            ry += 40;
        }
        // The choices end left of the rows' Remove buttons.
        let choices_right = card.right() - 22 - 40 - 12;
        let text_w = choices_right - 322 - 10 - (ix + 190);
        for q in &asking {
            let protocol = if q.protocol == FirewallProtocol::Tcp { "TCP" } else { "UDP" };
            let text = format!("Asks to accept connections ({protocol} port {})", q.port);
            ui.label(Rect::new(ix, ry, 180, 32), &q.app, Font::Bold, t.font_size, t.text, Align::Left);
            ui.label(Rect::new(ix + 190, ry, text_w, 32), &text, Font::Regular, t.small_size, t.accent, Align::Left);
            if let Some(from) = Self::choice(ui, choices_right, ry, None) {
                let app = q.app.clone();
                self.change(|c| c.incoming.push(AppRule { app, from }));
            }
            ry += 40;
        }
        for rule in &c.incoming {
            let text = match &rule.from {
                Some(scope) => format!("Accepts connections from {scope}"),
                None => String::from("Not let in"),
            };
            ui.label(Rect::new(ix, ry, 180, 32), &rule.app, Font::Bold, t.font_size, t.text, Align::Left);
            ui.label(Rect::new(ix + 190, ry, text_w, 32), &text, Font::Regular, t.small_size, t.text_dim, Align::Left);
            let app = rule.app.clone();
            if let Some(from) = Self::choice(ui, choices_right, ry, Some(rule.from)) {
                self.change(|c| c.incoming.iter_mut().filter(|r| r.app == app).for_each(|r| r.from = from));
            }
            let app = rule.app.clone();
            if ui.icon_button(Rect::new(card.right() - 22 - 40, ry, 40, 32), Icon::Trash, "Remove") {
                self.change(|c| c.incoming.retain(|r| r.app != app));
            }
            ry += 40;
        }
        for app in &c.blocked {
            ui.label(Rect::new(ix, ry, 180, 32), app, Font::Bold, t.font_size, t.text, Align::Left);
            ui.label(
                Rect::new(ix + 190, ry, iw - 260, 32),
                "Kept off the network",
                Font::Regular,
                t.small_size,
                t.danger,
                Align::Left,
            );
            let app = app.clone();
            if ui.icon_button(Rect::new(card.right() - 22 - 40, ry, 40, 32), Icon::Trash, "Remove") {
                self.change(|c| c.blocked.retain(|b| *b != app));
            }
            ry += 40;
        }
        let field = Rect::new(ix, ry + 8, (iw - 300).max(140), 34);
        ui.text_input(field, "settings-fw-app", &mut self.new_app, "Application, e.g. music");
        let name = self.new_app.trim().to_string();
        let allow = ui.button_full(Rect::new(field.right() + 10, ry + 8, 130, 34), None, "Let in", ButtonKind::Primary);
        let block = ui.button_full(
            Rect::new(field.right() + 150, ry + 8, 140, 34),
            None,
            "Keep off network",
            ButtonKind::Danger,
        );
        if (allow || block) && !name.is_empty() {
            self.change(|c| {
                c.incoming.retain(|r| r.app != name);
                c.blocked.retain(|b| *b != name);
                if allow {
                    c.incoming.push(AppRule { app: name.clone(), from: Some(Scope::LocalNetwork {}) });
                } else {
                    c.blocked.push(name.clone());
                }
            });
            if self.error.is_none() {
                self.new_app.clear();
            }
        }
        card.bottom() + 20
    }

    fn ports_card(&mut self, ui: &mut Ui, x: i32, y: i32, w: i32, c: &FirewallConfig) -> i32 {
        let t = ui.theme().clone();
        let rows = c.ports.len().max(1) as i32;
        let card = Rect::new(x, y, w, 88 + rows * 40 + 56);
        ui.card(card);
        let (ix, iw) = (card.x + 22, card.w - 44);
        ui.label(Rect::new(ix, card.y + 16, iw, 24), "Ports", Font::Bold, t.heading_size - 2.0, t.text, Align::Left);
        ui.label(
            Rect::new(ix, card.y + 42, iw, 20),
            "Ports opened to other machines, or closed to them, whatever program uses them.",
            Font::Regular,
            t.small_size,
            t.text_dim,
            Align::Left,
        );
        let mut ry = card.y + 76;
        if c.ports.is_empty() {
            let msg = "No port rules.";
            ui.label(Rect::new(ix, ry, iw, 32), msg, Font::Regular, t.font_size, t.text_faint, Align::Left);
            ry += 40;
        }
        for (i, rule) in c.ports.iter().enumerate() {
            let proto = if rule.protocol == FirewallProtocol::Tcp { "TCP" } else { "UDP" };
            let ports = if rule.first == rule.last {
                format!("{}", rule.first)
            } else {
                format!("{}-{}", rule.first, rule.last)
            };
            let what = format!("{proto} {ports}");
            let verdict = format!("{} from {}", if rule.allow { "Open" } else { "Closed" }, rule.from);
            ui.label(Rect::new(ix, ry, 180, 32), &what, Font::Bold, t.font_size, t.text, Align::Left);
            let color = if rule.allow { t.text_dim } else { t.danger };
            ui.label(Rect::new(ix + 190, ry, iw - 250, 32), &verdict, Font::Regular, t.small_size, color, Align::Left);
            if ui.icon_button(Rect::new(card.right() - 22 - 40, ry, 40, 32), Icon::Trash, "Remove") {
                self.change(|c| {
                    if i < c.ports.len() {
                        c.ports.remove(i);
                    }
                });
            }
            ry += 40;
        }
        let proto_label = if self.new_udp { "UDP" } else { "TCP" };
        if ui.button_full(Rect::new(ix, ry + 8, 64, 34), None, proto_label, ButtonKind::Secondary) {
            self.new_udp = !self.new_udp;
        }
        let ports = Rect::new(ix + 72, ry + 8, 110, 34);
        ui.text_input(ports, "settings-fw-ports", &mut self.new_ports, "Port");
        let from = Rect::new(ports.right() + 8, ry + 8, (iw - 72 - 110 - 8 - 8 - 230).max(120), 34);
        ui.text_input(from, "settings-fw-from", &mut self.new_from, "From: local, any, 192.0.2.0/24");
        let open = ui.button_full(Rect::new(from.right() + 10, ry + 8, 100, 34), None, "Open", ButtonKind::Primary);
        let close = ui.button_full(Rect::new(from.right() + 118, ry + 8, 100, 34), None, "Close", ButtonKind::Danger);
        if open || close {
            let scope = if self.new_from.trim().is_empty() {
                Some(Scope::LocalNetwork {})
            } else {
                Scope::parse(self.new_from.trim())
            };
            match (parse_ports(&self.new_ports), scope) {
                (Some((first, last)), Some(from)) => {
                    let protocol = if self.new_udp { FirewallProtocol::Udp } else { FirewallProtocol::Tcp };
                    self.change(|c| c.ports.push(PortRule { protocol, first, last, from, allow: open }));
                    if self.error.is_none() {
                        self.new_ports.clear();
                        self.new_from.clear();
                    }
                }
                (None, _) => {
                    self.error = Some(String::from("A port is a number from 1 to 65535, or a range (8000-8100)."))
                }
                (_, None) => {
                    self.error = Some(String::from("Where from: local, any, or a prefix such as 192.0.2.0/24."))
                }
            }
        }
        card.bottom() + 20
    }

    fn activity_card(&mut self, ui: &mut Ui, x: i32, y: i32, w: i32, c: &FirewallConfig) -> i32 {
        let t = ui.theme().clone();
        let shown: Vec<FirewallEvent> = self.log.iter().rev().take(SHOWN).cloned().collect();
        let card = Rect::new(x, y, w, 96 + shown.len().max(1) as i32 * 36);
        ui.card(card);
        let (ix, iw) = (card.x + 22, card.w - 44);
        ui.label(
            Rect::new(ix, card.y + 16, iw, 24),
            "Recently turned away",
            Font::Bold,
            t.heading_size - 2.0,
            t.text,
            Align::Left,
        );
        let s = &self.stats;
        let counts = format!(
            "Since the computer started: {} let in by a rule; {} not allowed, {} probes, {} pings, {} invalid, {} too many at once, {} fragments, {} from applications kept off.",
            s.allowed, s.not_allowed, s.probes, s.pings, s.invalid, s.rate_limited, s.fragments, s.app_blocked
        );
        ui.label(Rect::new(ix, card.y + 42, iw, 40), &counts, Font::Regular, t.small_size, t.text_dim, Align::Left);
        let mut ry = card.y + 86;
        if shown.is_empty() {
            let msg = if c.log { "Nothing turned away yet." } else { "The log is off." };
            ui.label(Rect::new(ix, ry, iw, 30), msg, Font::Regular, t.font_size, t.text_faint, Align::Left);
        }
        for e in &shown {
            let port = if e.port == 0 { String::new() } else { format!(" to port {}", e.port) };
            let from =
                if e.interface.is_empty() { format!("to {}", e.remote) } else { format!("from {}", e.remote.ip()) };
            let app = if e.app.is_empty() { String::new() } else { format!(" ({})", e.app) };
            let times = if e.count > 1 { format!(" \u{00d7}{}", e.count) } else { String::new() };
            let line = format!(
                "{} {from}{port}: {}{app}{times}, {}",
                protocol_name(e.protocol),
                reason_text(e.reason),
                ago(e.at)
            );
            ui.label(Rect::new(ix, ry, iw - 120, 30), &line, Font::Regular, t.small_size, t.text, Align::Left);
            // An application that was not reached, as one asked for: let it in.
            let known = c.incoming.iter().any(|r| r.app == e.app);
            if e.reason == FirewallReason::NotAllowed && !e.app.is_empty() && !known && c.enabled {
                let app = e.app.clone();
                let br = Rect::new(card.right() - 22 - 100, ry, 100, 30);
                if ui.button_full(br, None, "Let in", ButtonKind::Ghost) {
                    self.change(|c| c.incoming.push(AppRule { app: app.clone(), from: Some(Scope::LocalNetwork {}) }));
                }
            }
            ry += 36;
        }
        card.bottom() + 20
    }
}
