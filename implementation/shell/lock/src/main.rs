// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `lock` — the lock screen: the system service that shows it while the
//! screen is locked, asks for the password, and alone unlocks the screen.
//!
//! The window system keeps the lock (`vproto::display`): while the screen
//! is locked it draws only this service's windows, and gives the keyboard
//! and the pointer to nothing else. This service watches the lock
//! (`display::watch_lock`) and shows the lock screen ([`view`]) whenever
//! the screen is locked: by the user (Super+L, the start menu), by the
//! agent, or as the window system starts.
//!
//! * **As the window system starts** (with the system, or again after a
//!   crash) it is locked. With a password set, the lock screen asks for it;
//!   with none, the screen is unlocked at once, so that the desktop
//!   appears as it would without a lock. Locked later, the lock screen
//!   shows whether a password is set or not; with none, Enter unlocks.
//! * **The password** is checked on a thread of its own ([`hasher`])
//!   against its record in the service's private directory ([`store`]).
//!   Wrong ones count, and make the next wait (`lock_core::throttle`): the
//!   count is kept there too, so that restarting the service gains
//!   nothing.
//! * **Settings** sets, changes and removes the password through the
//!   `lock` protocol (`vproto::lock`), giving the one set now; no one else
//!   may. The request is answered once the hasher is done.
//! * **Locking after a while without input** is the window system's: this
//!   service tells it how long (`display::set_idle_timeout`) whenever it
//!   (re)connects, and whenever that changes: Settings alone reads and
//!   chooses the time, which is kept in the private directory; until one
//!   is chosen, it follows the password (`lock_core::idle`).
//! * **Failing safe.** Should this service crash, the window system stays
//!   locked, drawing its own backdrop, until `init` starts it again;
//!   should the window system restart, it starts locked, and this service
//!   reconnects and shows the lock screen again. A record of the password
//!   that is there but cannot be read keeps the screen locked: no password
//!   typed unlocks it.
//! * **The backdrop** is the desktop's wallpaper, made into the lock
//!   screen's ([`backdrop`]) on a thread of its own: as the service starts,
//!   and whenever the screen locks with another wallpaper chosen since.

mod backdrop;
mod hasher;
mod store;
mod view;

use lock_core::password::{SALT_LEN, acceptable};
use lock_core::screen::{Phase, Screen};
use lock_core::throttle::Throttle;
use vabi::{WaitItem, signals};
use vipc::{Decode, Encode};
use vproto::display::{LOCK_NOTICE, LockNotice};
use vproto::init::ClientIdentity;
use vproto::lock::{LockError, LockStatus, lock};
use vrt::object::{Channel, Message};
use vrt::println;
use vui::window::Display;
use zeroize::Zeroizing;

use backdrop::{Backdrop, Painter};
use hasher::{Done, Hasher, Job, SetError};
use store::{Found, Store};
use view::{Request, View};

/// How long the service waits for the window system's answers.
const DISPLAY_TIMEOUT_NS: u64 = 5_000_000_000;
/// How often it tries to reach the window system while it is away, or to
/// show the lock screen when the window system did not take it.
const RETRY_NS: u64 = 250_000_000;
/// How long a lock screen just opened waits for the wallpaper before its
/// first frame (meanwhile the window system shows its backdrop, or the
/// startup splash), so that it does not appear on the gradient and change
/// under the user's eyes.
const WALLPAPER_WAIT_NS: u64 = 4_000_000_000;
/// How long the wallpaper it replaced is kept, for it to fade out.
const REPLACED_NS: u64 = 1_000_000_000;
/// Connections to the service: in all, and from one process.
const MAX_CLIENTS: usize = 8;
const MAX_CLIENTS_EACH: usize = 2;
/// `lock::set_password`'s method number: answered once the hasher is done,
/// rather than as `lock::dispatch` would, at once.
const SET_PASSWORD: u32 = 2;

/// A connection to the service.
struct Client {
    /// Its number (connections come and go).
    id: u64,
    channel: Channel,
    who: ClientIdentity,
    /// A password it set is being hashed: it sets one at a time.
    setting: bool,
}

/// The lock service (see the module's comment).
struct Lock {
    /// The window system (`None` while it is away), the channel it tells
    /// the lock on, and whether the next notice is the first on it.
    display: Option<Display>,
    watch: Option<Channel>,
    first_notice: bool,
    /// The screen is locked, as the window system last said.
    locked: bool,
    screen: (i32, i32),
    /// When to try again to reach the window system, or to show the lock
    /// screen.
    next_try: u64,
    store: Store,
    password: Found,
    throttle: Throttle,
    /// How long the user chose the screen waits without input before it
    /// locks (s), if they did; what the window system was told, since it
    /// was last reached.
    idle: Option<u32>,
    told_idle: Option<u32>,
    hasher: Option<Hasher>,
    /// The check the lock screen waits for (its ticket).
    checking: Option<u64>,
    /// Settings' requests the hasher has: (ticket, client, request).
    setting: Vec<(u64, u64, u32)>,
    next_ticket: u64,
    painter: Option<Painter>,
    backdrop: Option<Backdrop>,
    /// The backdrop the last one replaced, and when, while it fades out.
    previous: Option<(Backdrop, u64)>,
    view: Option<View>,
    listener: Option<Channel>,
    clients: Vec<Client>,
    next_client: u64,
    /// The clients with requests waiting (as the last wait found).
    ready: Vec<u64>,
}

/// The Unix time, in seconds.
fn unix_s() -> u64 {
    vrt::time::unix_time_ns() / 1_000_000_000
}

/// A new salt, from the kernel's random number generator.
fn salt() -> [u8; SALT_LEN] {
    let mut s = [0u8; SALT_LEN];
    vrt::object::random_bytes(&mut s);
    s
}

/// Whether `name` is registered: a connection to a service not registered
/// yet would wait for it.
fn registered(name: &str) -> bool {
    vproto::with_registry(|r| r.list()).ok().and_then(|r| r.ok()).is_some_and(|names| names.iter().any(|n| n == name))
}

impl Lock {
    // ---- the window system ----------------------------------------------

    /// Reaches the window system, if it is away, and watches the lock.
    fn connect_display(&mut self, now: u64) {
        if self.display.is_some() || now < self.next_try {
            return;
        }
        self.next_try = now + RETRY_NS;
        if !registered(vproto::display::display::NAME) {
            return;
        }
        let Ok(display) = vui::window::connect() else { return };
        display.set_timeout(DISPLAY_TIMEOUT_NS);
        let Ok(info) = display.screen_info() else { return };
        let Ok((ours, theirs)) = Channel::create() else { return };
        if !matches!(display.watch_lock(theirs), Ok(Ok(()))) {
            println!("the window system does not tell the lock to this service");
            return;
        }
        let (w, h) = (info.width as i32, info.height as i32);
        self.screen = (w, h);
        if self.backdrop.as_ref().is_none_or(|b| b.size() != (w, h)) {
            self.backdrop = Some(Backdrop::gradient(w, h));
        }
        self.display = Some(display);
        self.watch = Some(ours);
        self.first_notice = true;
        // A window system that restarted knows nothing of it.
        self.told_idle = None;
        self.tell_idle_timeout();
        // Ready before the screen is first locked.
        self.refresh_backdrop();
    }

    /// How long the screen waits without input before it locks (s; 0:
    /// never).
    fn idle_timeout(&self) -> u32 {
        lock_core::idle::timeout(self.idle, !matches!(self.password, Found::None))
    }

    /// Tells the window system how long the screen waits without input
    /// before it locks, unless it was told so already.
    fn tell_idle_timeout(&mut self) {
        let seconds = self.idle_timeout();
        let Some(display) = &self.display else { return };
        if self.told_idle == Some(seconds) {
            return;
        }
        match display.set_idle_timeout(seconds) {
            Ok(Ok(())) => {
                self.told_idle = Some(seconds);
                println!("{}", idle_said(seconds));
            }
            Ok(Err(e)) => println!("the window system did not take the time to lock after: {:?}", e),
            Err(e) => println!("the window system did not answer: {}", e),
        }
    }

    /// The window system went away (it is restarting): what was shown on
    /// it is gone, and the service reaches it again once it is back. It
    /// comes back locked.
    fn lost_display(&mut self) {
        println!("the window system went away");
        self.view = None;
        self.watch = None;
        self.display = None;
        self.checking = None;
        self.locked = true;
        self.next_try = 0;
    }

    /// Takes the window system's notices of the lock.
    fn take_notices(&mut self, now: u64) {
        let Some(ch) = &self.watch else { return };
        let mut notices = Vec::new();
        let gone = loop {
            match ch.read() {
                Ok(msg) => {
                    if let Ok((LOCK_NOTICE, LockNotice::Changed { locked })) = vipc::decode_event::<LockNotice>(msg) {
                        notices.push(locked);
                    }
                }
                Err(vabi::Error::ShouldWait) => break false,
                Err(_) => break true,
            }
        };
        for locked in notices {
            self.lock_changed(locked, now);
        }
        if gone {
            self.lost_display();
        }
    }

    /// The screen is now `locked`, or not.
    fn lock_changed(&mut self, locked: bool, now: u64) {
        let first = core::mem::replace(&mut self.first_notice, false);
        self.locked = locked;
        if !locked {
            // Unlocked by this service: the lock screen goes, but as it
            // fades out over the desktop.
            if self.view.as_ref().is_some_and(|v| !matches!(v.phase(), Phase::Fading | Phase::Gone)) {
                self.view = None;
            }
            return;
        }
        // As the window system starts, with no password set, it is
        // unlocked at once: there is nothing to keep from anyone.
        if first && matches!(self.password, Found::None) {
            self.unlock("no password is set");
            return;
        }
        println!("the screen is locked");
        match &mut self.view {
            Some(v) if matches!(v.phase(), Phase::Unlocking | Phase::Fading | Phase::Gone) => v.relock(now),
            Some(_) => {}
            None => self.show(now),
        }
        self.refresh_backdrop();
    }

    /// Shows the lock screen (the window system draws its own backdrop
    /// until it has drawn itself).
    fn show(&mut self, now: u64) {
        let Some(display) = &self.display else { return };
        let (w, h) = self.screen;
        let password = !matches!(self.password, Found::None);
        match View::open(display, w, h, Screen::new(now, password)) {
            Ok(v) => self.view = Some(v),
            Err(e) => {
                println!("cannot show the lock screen: {:?}", e);
                self.next_try = now + RETRY_NS;
            }
        }
    }

    /// Unlocks the screen. It is unlocked as the window system answers (its
    /// notice follows): nothing is shown again before that.
    fn unlock(&mut self, why: &str) {
        let Some(display) = &self.display else { return };
        match display.unlock() {
            Ok(Ok(())) => {
                self.locked = false;
                println!("unlocked ({why})");
            }
            Ok(Err(e)) => println!("the window system did not unlock the screen: {:?}", e),
            Err(e) => println!("the window system did not answer: {}", e),
        }
    }

    /// Asks for the backdrop of the wallpaper chosen now, if it is not the
    /// one shown.
    fn refresh_backdrop(&mut self) {
        if let Some(p) = &self.painter {
            p.refresh(self.screen.0, self.screen.1, self.backdrop.as_ref());
        }
    }

    // ---- the lock screen --------------------------------------------------

    /// Lets the lock screen take its events and draw, and does what it asks.
    fn pump_view(&mut self, now: u64) {
        if self.locked && self.view.is_none() && self.display.is_some() && now >= self.next_try {
            self.show(now);
        }
        let wait_s = self.throttle.wait(unix_s());
        if self.previous.as_ref().is_some_and(|(_, since)| now.saturating_sub(*since) >= REPLACED_NS) {
            self.previous = None;
        }
        let (Some(v), Some(backdrop)) = (&mut self.view, &self.backdrop) else { return };
        if self.painter.is_some()
            && backdrop.source.is_none()
            && !v.opened().1
            && now < v.opened().0 + WALLPAPER_WAIT_NS
        {
            return;
        }
        let previous = self.previous.as_ref().map(|(b, since)| (b, *since));
        let requests = v.pump(backdrop, previous, wait_s);
        if v.closed() {
            return self.lost_display();
        }
        for r in requests {
            match r {
                Request::Check(password) => self.check(password, now),
                Request::Unlock => {
                    let why = if matches!(self.password, Found::None) {
                        "no password is set"
                    } else {
                        "the password is right"
                    };
                    self.unlock(why);
                }
                Request::Close => self.view = None,
            }
        }
    }

    /// The lock screen sends `password` to be checked.
    fn check(&mut self, password: Zeroizing<String>, now: u64) {
        let record = match &self.password {
            Found::Set(r) => r.clone(),
            // No password typed matches a record that cannot be read.
            Found::Damaged(_) => return self.checked(false, None, now),
            Found::None => return self.unlock("no password is set"),
        };
        let Some(hasher) = &self.hasher else { return self.checked(false, None, now) };
        self.next_ticket += 1;
        self.checking = Some(self.next_ticket);
        hasher.give(Job::Check { ticket: self.next_ticket, record, password, salt: salt() });
    }

    /// The password the lock screen sent was `right`, or not; `renewed` is
    /// its record made anew (the one kept was outdated).
    fn checked(&mut self, right: bool, renewed: Option<lock_core::password::Record>, now: u64) {
        if right {
            self.throttle.succeeded();
            let _ = self.store.set_throttle(&self.throttle);
            if let Some(r) = renewed
                && self.store.set_password(Some(&r)).is_ok()
            {
                println!("the password's record was made anew, with stronger parameters");
                self.password = Found::Set(r);
            }
        } else {
            self.failed();
        }
        if let Some(v) = &mut self.view {
            v.checked(now, right);
        }
    }

    /// A wrong password was given: it counts, and may make the next wait.
    fn failed(&mut self) {
        let wait = self.throttle.failed(unix_s());
        if self.store.set_throttle(&self.throttle).is_err() {
            println!("cannot keep the count of wrong passwords");
        }
        let n = self.throttle.failures();
        if wait > 0 {
            println!("a wrong password ({n} since the right one): the next waits {wait} s");
        } else {
            println!("a wrong password ({n} since the right one)");
        }
    }

    /// Takes what the hasher has done.
    fn take_hashed(&mut self, now: u64) {
        let Some(hasher) = &self.hasher else { return };
        for done in hasher.take() {
            match done {
                Done::Checked { ticket, right, renewed } => {
                    // Only the check the lock screen still waits for.
                    if self.checking == Some(ticket) {
                        self.checking = None;
                        self.checked(right, renewed, now);
                    }
                }
                Done::Set { ticket, result } => self.password_set(ticket, result),
            }
        }
    }

    // ---- the `lock` protocol ----------------------------------------------

    /// Accepts connections (as many as allowed) and answers the requests of
    /// the clients that sent any.
    fn serve(&mut self) {
        if let Some(l) = &self.listener {
            while let Some((channel, who)) = vproto::accept_with_identity(l) {
                let own = self.clients.iter().filter(|c| c.who.koid == who.koid).count();
                if self.clients.len() >= MAX_CLIENTS || own >= MAX_CLIENTS_EACH {
                    println!("{} has too many connections; one refused", who.name);
                    continue;
                }
                self.next_client += 1;
                self.ready.push(self.next_client);
                self.clients.push(Client { id: self.next_client, channel, who, setting: false });
            }
        }
        for id in core::mem::take(&mut self.ready) {
            let Some(i) = self.clients.iter().position(|c| c.id == id) else { continue };
            let mut client = self.clients.swap_remove(i);
            let alive = loop {
                match client.channel.read() {
                    Ok(msg) => self.request(&mut client, msg),
                    Err(vabi::Error::ShouldWait) => break true,
                    Err(_) => break false,
                }
            };
            if alive {
                self.clients.push(client);
            }
        }
    }

    /// Answers one request of `client`.
    fn request(&mut self, client: &mut Client, msg: Message) {
        if vipc::Header::parse(&msg.bytes).is_ok_and(|h| h.ordinal == SET_PASSWORD) {
            return self.set_password_request(client, msg);
        }
        let settings = is_settings(&client.who);
        match lock::dispatch(&mut Call { lock: self, settings }, msg) {
            Ok(reply) => {
                let _ = reply.send(&client.channel);
            }
            Err(e) => println!("bad request: {}", e),
        }
    }

    /// `lock::set_password`, from Settings alone: the current password is
    /// checked and the new one hashed on the hasher's thread, and the
    /// request answered once that is done ([`Lock::password_set`]).
    fn set_password_request(&mut self, client: &mut Client, mut msg: Message) {
        let decoded = vipc::open(&mut msg).and_then(|(h, mut d)| {
            let current = String::decode(&mut d)?;
            let new = String::decode(&mut d)?;
            d.finish()?;
            Ok((h, Zeroizing::new(current), Zeroizing::new(new)))
        });
        let (header, current, new) = match decoded {
            Ok(v) => v,
            Err(_) => {
                if let Ok(h) = vipc::Header::parse(&msg.bytes) {
                    let _ = vipc::refuse(&client.channel, h, vipc::Refusal::Malformed);
                }
                return;
            }
        };
        let refuse = |e: LockError| answer_set(&client.channel, header.txid, Err(e));
        if !is_settings(&client.who) {
            return refuse(LockError::Denied);
        }
        if client.setting {
            return refuse(LockError::Unavailable);
        }
        if self.throttle.wait(unix_s()) > 0 {
            return refuse(LockError::Throttled);
        }
        if !new.is_empty()
            && let Err(e) = acceptable(&new)
        {
            return refuse(e);
        }
        let Some(hasher) = &self.hasher else { return refuse(LockError::Unavailable) };
        // A record that cannot be read is no password to give: Settings
        // (on an unlocked screen) replaces it.
        let record = match &self.password {
            Found::Set(r) => Some(r.clone()),
            Found::None | Found::Damaged(_) => None,
        };
        self.next_ticket += 1;
        self.setting.push((self.next_ticket, client.id, header.txid));
        client.setting = true;
        hasher.give(Job::Set { ticket: self.next_ticket, record, current, new, salt: salt() });
    }

    /// The hasher is done with Settings' request `ticket`: the password is
    /// kept, and Settings answered.
    fn password_set(&mut self, ticket: u64, result: Result<Option<lock_core::password::Record>, SetError>) {
        let Some(i) = self.setting.iter().position(|(t, ..)| *t == ticket) else { return };
        let (_, client, txid) = self.setting.remove(i);
        let answer = match result {
            Ok(record) => match self.store.set_password(record.as_ref()) {
                Ok(()) => {
                    // The password set was given: the count starts again.
                    self.throttle.succeeded();
                    let _ = self.store.set_throttle(&self.throttle);
                    println!("the password was {}", if record.is_some() { "set" } else { "removed" });
                    self.password = record.map_or(Found::None, Found::Set);
                    let password = !matches!(self.password, Found::None);
                    if let Some(v) = &mut self.view {
                        v.set_password(password);
                    }
                    // Until a time is chosen, it follows the password.
                    self.tell_idle_timeout();
                    Ok(())
                }
                Err(_) => {
                    println!("cannot keep the password");
                    Err(LockError::Unavailable)
                }
            },
            Err(SetError::WrongPassword) => {
                self.failed();
                Err(LockError::WrongPassword)
            }
            Err(SetError::Hashing) => Err(LockError::Unavailable),
        };
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            c.setting = false;
            answer_set(&c.channel, txid, answer);
        }
    }

    // ---- the main loop ------------------------------------------------------

    /// Blocks until the window system, the lock screen, a thread or a client
    /// needs attention.
    fn wait(&mut self) {
        let readable = signals::READABLE | signals::PEER_CLOSED;
        let mut items: Vec<WaitItem> = Vec::new();
        let mut deadline = vabi::DEADLINE_INFINITE;
        if let Some(v) = &self.view {
            items.push(v.host().wait_item());
            deadline = deadline.min(v.host().deadline());
        }
        if let Some(w) = &self.watch {
            items.push(WaitItem { handle: w.raw(), signals: readable, ..Default::default() });
        }
        if self.display.is_none() || (self.locked && self.view.is_none()) {
            deadline = deadline.min(self.next_try);
        }
        if let Some(v) = &self.view
            && !v.opened().1
        {
            deadline = deadline.min(v.opened().0 + WALLPAPER_WAIT_NS);
        }
        if let Some(h) = &self.hasher {
            items.push(WaitItem { handle: h.event_handle(), signals: signals::SIGNALED, ..Default::default() });
        }
        if let Some(p) = &self.painter {
            items.push(WaitItem { handle: p.event_handle(), signals: signals::SIGNALED, ..Default::default() });
        }
        if let Some(l) = &self.listener {
            items.push(WaitItem { handle: l.raw(), signals: readable, ..Default::default() });
        }
        let first_client = items.len();
        for c in &self.clients {
            items.push(WaitItem { handle: c.channel.raw(), signals: readable, ..Default::default() });
        }
        items.truncate(vabi::WAIT_MANY_MAX);
        let _ = vrt::object::wait_many(&mut items, deadline);
        for (c, it) in self.clients.iter().zip(items.iter().skip(first_client)) {
            if it.observed & it.signals != 0 {
                self.ready.push(c.id);
            }
        }
    }

    fn run(&mut self) -> ! {
        loop {
            let now = vrt::time::now_ns();
            self.connect_display(now);
            self.take_notices(now);
            self.take_hashed(now);
            if let Some(b) = self.painter.as_ref().and_then(Painter::take) {
                let old = self.backdrop.replace(b);
                if let Some(v) = &mut self.view {
                    // Shown already: the new one fades in over it.
                    if v.opened().1 {
                        self.previous = old.map(|o| (o, now));
                    }
                    v.invalidate();
                }
            }
            self.pump_view(vrt::time::now_ns());
            self.serve();
            self.wait();
        }
    }
}

/// Answers a `lock::set_password` request.
fn answer_set(channel: &Channel, txid: u32, result: Result<(), LockError>) {
    let mut e = vipc::Encoder::with_header(SET_PASSWORD, txid, vipc::FLAG_RESPONSE);
    result.encode(&mut e);
    let _ = e.send(channel);
}

/// Whether `who` is Settings, which alone changes the lock screen.
fn is_settings(who: &ClientIdentity) -> bool {
    who.app && !who.service && who.name == "settings"
}

/// What the screen does without input for `seconds`, for the log.
fn idle_said(seconds: u32) -> String {
    match seconds {
        0 => "the screen does not lock for want of input".into(),
        60 => "the screen locks after 1 minute without input".into(),
        s if s % 60 == 0 => format!("the screen locks after {} minutes without input", s / 60),
        s => format!("the screen locks after {s} s without input"),
    }
}

/// A request of a client, who may be Settings.
struct Call<'a> {
    lock: &'a mut Lock,
    settings: bool,
}

impl lock::Server for Call<'_> {
    fn status(&mut self) -> LockStatus {
        let lock = &self.lock;
        LockStatus {
            password: !matches!(lock.password, Found::None),
            locked: lock.locked,
            wait_s: lock.throttle.wait(unix_s()) as u32,
        }
    }

    /// Not reached: `Lock::set_password_request` answers these once the
    /// hasher is done.
    fn set_password(&mut self, _current: String, _new: String) -> Result<(), LockError> {
        Err(LockError::Unavailable)
    }

    fn idle_timeout(&mut self) -> Result<u32, LockError> {
        if !self.settings {
            return Err(LockError::Denied);
        }
        Ok(self.lock.idle_timeout())
    }

    /// Settings' choice is kept first, then told to the window system (now,
    /// or once it is reached again).
    fn set_idle_timeout(&mut self, seconds: u32) -> Result<(), LockError> {
        if !self.settings {
            return Err(LockError::Denied);
        }
        lock_core::idle::acceptable(seconds)?;
        if self.lock.store.set_idle(seconds).is_err() {
            println!("cannot keep the time the screen locks after");
            return Err(LockError::Unavailable);
        }
        self.lock.idle = Some(seconds);
        self.lock.tell_idle_timeout();
        Ok(())
    }
}

fn main() {
    let store = Store::open();
    let password = store.password();
    if let Found::Damaged(e) = &password {
        println!("{e}: no password typed unlocks the screen");
    }
    let throttle = store.throttle(unix_s());
    let idle = store.idle();
    let hasher = Hasher::start();
    if hasher.is_none() {
        println!("cannot start the hasher: no password can be checked");
    }
    let listener = vproto::register(lock::NAME).ok();
    if listener.is_none() {
        println!("cannot register the lock service");
    }
    let mut lock = Lock {
        display: None,
        watch: None,
        first_notice: true,
        locked: true,
        screen: (0, 0),
        next_try: 0,
        store,
        password,
        throttle,
        idle,
        told_idle: None,
        hasher,
        checking: None,
        setting: Vec::new(),
        next_ticket: 0,
        painter: Painter::start(),
        backdrop: None,
        previous: None,
        view: None,
        listener,
        clients: Vec::new(),
        next_client: 0,
        ready: Vec::new(),
    };
    println!("ready ({})", if matches!(lock.password, Found::None) { "no password set" } else { "a password is set" });
    lock.run()
}
