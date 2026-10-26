// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Veda desktop shell.
//!
//! The shell owns the compositor's shell surfaces:
//!
//! * the **desktop** ([`desktop`]): wallpaper and desktop icons, the Trash
//!   first;
//! * the **taskbar** ([`taskbar`]): start button, the apps kept on it and
//!   running ones, clock;
//! * the **start menu** ([`start`]), **calendar** ([`calendar`]),
//!   **volume** ([`volume`]) and **network** ([`wifi`]) popups, one at a
//!   time ([`Popup`]);
//! * **notifications** ([`notify`]);
//! * the voice **agent**'s tray item, window and approval requests
//!   ([`agent`], [`approval`]);
//! * the **firewall**'s questions: whether an application may accept
//!   connections from other machines ([`firewall`]);
//! * **locking the screen** (the start menu's lock), which the window
//!   system does and the lock screen undoes
//!   (`implementation/shell/components/lock`);
//! * **restarting and shutting down** once the applications have closed
//!   ([`power`]).
//!
//! Every surface is a [`ui::Host`] window. Surfaces draw from the shared
//! [`Model`] and request changes as [`Action`]s, which the main loop performs
//! after each round of drawing. The shell also serves the `shell` protocol so
//! applications can change the wallpaper (loaded on a thread of its own, the
//! request answered once it is done) and post notifications (shown as the
//! application's, and only so many). It waits for the services it calls no
//! longer than a few seconds, so that one that hangs does not freeze the
//! desktop.

mod agent;
mod approval;
mod apps;
mod calendar;
mod chrome;
mod desktop;
mod firewall;
mod notify;
mod power;
mod start;
mod startup;
mod taskbar;
mod tooltip;
mod volume;
mod wifi;

use abi::{WaitItem, signals};
use gfx::Rect;
use ipc::{Decode, Encode};
use proto::audio::{AudioStatus, audio};
use proto::display::{WindowEvent, WindowInfo, WindowKind, WindowSpec, WindowState};
use proto::init::{AppInfo, ClientIdentity, TaskEvent, launcher};
use proto::shell::{ShellError, shell};
use proto::vfs;
use rt::object::{Channel, Message, Vmo};
use rt::println;
use shell_common::wallpaper::{self, Loader, Wallpaper};
use ui::window::Display;
use ui::{Host, Icon, Ui};

/// How often the desktop looks whether `~/Desktop` or the Trash changed.
const DESKTOP_POLL_NS: u64 = 1_500_000_000;

/// How long after a popup was dismissed by an outside click a click on its
/// taskbar button still counts as "close" rather than "open again".
pub const DISMISS_GRACE_NS: u64 = 250_000_000;

/// How long the shell waits for an answer from the window system, the
/// launcher or the file system before it carries on without one.
const SERVICE_TIMEOUT_NS: u64 = 10_000_000_000;
/// ... and from the audio service.
const AUDIO_TIMEOUT_NS: u64 = 2_000_000_000;
/// How long writing the home folder to disk before the power goes may take.
const SYNC_TIMEOUT_NS: u64 = 120_000_000_000;

/// Connections to the shell's service: in all, and from one process.
const MAX_CLIENTS: usize = 32;
const MAX_CLIENTS_EACH: usize = 4;
/// Notifications a process may post in a period.
const NOTES_PER_PERIOD: u32 = 5;
const NOTE_PERIOD_NS: u64 = 10_000_000_000;

/// `shell::set_wallpaper`'s method number: the shell answers it once the
/// picture is loaded, rather than as `shell::dispatch` would, at once.
const SET_WALLPAPER: u32 = 1;

/// The taskbar's popups (one is open at a time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Start,
    Calendar,
    Volume,
    Wifi,
}

/// A request from a shell surface, performed by the main loop.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Starts an installed application with arguments.
    Launch(String, Vec<String>),
    /// Raises, restores and focuses a window.
    Activate(u32),
    Minimize(u32),
    /// Opens a popup (closing the one open), or closes it if it is open.
    TogglePopup(PopupKind),
    ClosePopup(PopupKind),
    /// Turns the Wi-Fi radio on or off.
    WifiRadio(bool),
    WifiScan,
    /// Joins a Wi-Fi network: `security` is that of the network chosen from
    /// the list (`None` for a hidden one), so that the password never goes
    /// to an open impostor of the same name.
    WifiConnect {
        ssid: Vec<u8>,
        security: Option<net::wifi::Security>,
        password: Option<String>,
        auto: bool,
        hidden: bool,
    },
    WifiDisconnect,
    /// Opens the network page of Settings.
    OpenNetworkSettings,
    /// Opens or closes the agent's window.
    ToggleAgent,
    CloseAgent,
    /// Starts or ends a conversation with the agent.
    AgentWake,
    AgentSleep,
    /// The user's answer to an approval request.
    AgentDecide {
        id: u64,
        allow: bool,
        always: bool,
    },
    /// Turns the agent's microphone off (or on).
    AgentMute(bool),
    /// The user's answer to the firewall's question about an application:
    /// where it may accept connections from (`None`: nowhere).
    FirewallAnswer {
        app: String,
        from: Option<net::Scope>,
    },
    /// Sets the master volume (0..=1) and mute state.
    SetVolume(f32, bool),
    /// Minimises every window, or restores them if the desktop is showing.
    ShowDesktop,
    /// Locks the screen.
    Lock,
    /// Restarts or shuts down (`power::REBOOT`, `power::SHUTDOWN`) once
    /// the applications have closed.
    Power(u32),
    NextWallpaper,
    /// Re-reads the desktop folder.
    RefreshDesktop,
    /// Moves an item of the desktop to the Trash.
    Trash(String),
    /// Moves an item of the desktop into a folder.
    MoveInto(String, String),
}

/// State shared by all shell surfaces.
pub struct Model {
    pub screen: Rect,
    pub apps: Vec<AppInfo>,
    /// Application windows (normal and borderless), bottom to top.
    pub windows: Vec<WindowInfo>,
    pub wallpaper: Wallpaper,
    /// Incremented whenever the wallpaper changes.
    pub wallpaper_generation: u64,
    /// The audio system's state (`None` without the audio service).
    pub audio: Option<AudioStatus>,
    /// The Wi-Fi state (`None` without the Wi-Fi service).
    pub wifi: Option<net::wifi::WlanStatus>,
    /// Wi-Fi networks in range.
    pub wifi_networks: Vec<net::wifi::NetworkInfo>,
    /// The last failed connection attempt: SSID and why.
    pub wifi_error: Option<(Vec<u8>, String)>,
    /// A wired interface is connected, and how to describe it.
    pub wired_up: bool,
    pub wired_label: String,
    /// The popup that is open.
    pub popup: Option<PopupKind>,
    /// The popup an outside click dismissed last, and when (see
    /// [`DISMISS_GRACE_NS`]).
    pub dismissed: Option<(PopupKind, u64)>,
    /// The taskbar button under the pointer, for its tooltip.
    pub tip: Option<tooltip::Tip>,
    /// The voice agent's state.
    pub agent: agent::AgentModel,
    /// The firewall's questions waiting for the user's answer, the oldest
    /// first.
    pub firewall: Vec<net::FirewallQuestion>,
    pub actions: Vec<Action>,
}

impl Model {
    pub fn app(&self, id: &str) -> Option<&AppInfo> {
        self.apps.iter().find(|a| a.id == id)
    }

    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }

    /// Whether a press on `kind`'s button at `pressed_at` is the click that
    /// dismissed it, which must not open it again.
    pub fn just_dismissed(&self, kind: PopupKind, pressed_at: u64) -> bool {
        self.dismissed.is_some_and(|(k, at)| k == kind && pressed_at <= at + DISMISS_GRACE_NS)
    }
}

/// What an open popup shows.
enum PopupUi {
    Start(start::StartMenu),
    Calendar(calendar::Calendar),
    Volume(volume::VolumeFlyout),
    Wifi(wifi::WifiFlyout),
}

impl PopupUi {
    fn update(&mut self, ui: &mut Ui, m: &mut Model) {
        match self {
            PopupUi::Start(p) => p.update(ui, m),
            PopupUi::Calendar(p) => p.update(ui, m),
            PopupUi::Volume(p) => p.update(ui, m),
            PopupUi::Wifi(p) => p.update(ui, m),
        }
    }
}

/// The open popup: which it is, its window and what it shows.
struct Popup {
    kind: PopupKind,
    host: Host,
    ui: PopupUi,
}

/// The agent's window.
struct AgentWin {
    host: Host,
    ui: agent::AgentWindow,
}

/// A connection to the shell's service.
struct Client {
    /// Its number (connections come and go).
    id: u64,
    channel: Channel,
    who: ClientIdentity,
    /// A wallpaper it asked for is being loaded: it asks for one at a time.
    loading: bool,
}

struct Shell {
    display: Display,
    vfs: Option<vfs::Client>,
    /// The files of the desktop and the Trash.
    fs: files::Fs,
    launcher: Option<launcher::Client>,
    model: Model,
    desktop_host: Host,
    desktop: desktop::Desktop,
    /// When to next look whether the desktop's icons are out of date.
    next_desktop_poll: u64,
    taskbar_host: Host,
    taskbar: taskbar::Taskbar,
    audio: Option<audio::Client>,
    popup: Option<Popup>,
    /// Wi-Fi events (`None` while the Wi-Fi service is not running).
    wifi_watch: Option<net::wifi::Watcher>,
    /// The firewall's questions (`None` while the network service is not
    /// running).
    firewall_watch: Option<Channel>,
    /// When to next look at the network state (and retry the watcher).
    next_net_poll: u64,
    tooltip: Option<tooltip::Tooltip>,
    notes: notify::Notifications,
    /// Notifications each process posted in the current period: (process,
    /// when the period started, how many).
    note_counts: Vec<(u64, u64, u32)>,
    listener: Option<Channel>,
    clients: Vec<Client>,
    next_client: u64,
    /// The clients with requests waiting (as the last wait found).
    ready: Vec<u64>,
    /// Who sent the request being answered.
    caller: ClientIdentity,
    /// Loads wallpapers (`None` without a thread for it: loaded at once).
    loader: Option<Loader>,
    /// Wallpapers being loaded: the ticket, and the client and request to
    /// answer (`None` for the desktop's own "Next wallpaper").
    loading: Vec<(u64, Option<(u64, u32)>)>,
    next_ticket: u64,
    /// A restart or shutdown waiting for the applications to close.
    power: Option<power::PowerOff>,
    /// Crash reports from the launcher, and news that the installed
    /// applications changed (`launcher::watch`).
    task_events: Option<Channel>,
    /// Windows minimised by "show desktop", restored by the next click.
    hidden_by_show_desktop: Vec<u32>,
    /// The agent's interface link (`None` while the agent is not running).
    agent: Option<agent::AgentLink>,
    agent_win: Option<AgentWin>,
    /// When to try attaching the agent's interface again.
    next_agent_try: u64,
    /// The startup sound, until it has played (at system start only).
    startup: Option<startup::StartupSound>,
}

/// Connects to a service only if it is running: a call to a service that
/// never registers would wait forever.
pub fn connect_running(name: &str) -> Option<Channel> {
    let names = proto::with_registry(|r| r.list()).ok()?.ok()?;
    if names.iter().any(|n| n == name) { proto::connect(name).ok() } else { None }
}

/// Replaces a file's contents, creating parent directories as needed.
fn write_file(vfs: &vfs::Client, path: &str, data: &[u8]) -> bool {
    let mut dir = String::new();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        dir.push('/');
        dir.push_str(part);
        let _ = vfs.mkdir(dir.clone());
    }
    let Ok(vmo) = Vmo::create(data.len().max(1)) else { return false };
    if vmo.write(0, data).is_err() {
        return false;
    }
    matches!(vfs.write_file(path.into(), vmo, data.len() as u64), Ok(Ok(())))
}

impl Shell {
    /// Loads the wallpaper at `path` (or the procedural one) on the
    /// loader's thread; `asker` is the client and request to answer once it
    /// is done. Without a loader, it is loaded now: the result.
    fn load_wallpaper(&mut self, path: &str, asker: Option<(u64, u32)>) -> Option<Result<(), ShellError>> {
        let (w, h) = (self.model.screen.w, self.model.screen.h);
        let Some(loader) = &self.loader else {
            let result = Wallpaper::open(self.vfs.as_ref(), path, w, h).map(|wp| self.set_wallpaper(wp));
            return Some(result);
        };
        self.next_ticket += 1;
        loader.load(self.next_ticket, path, w, h);
        self.loading.push((self.next_ticket, asker));
        None
    }

    /// Takes the wallpapers the loader has finished: the last becomes the
    /// wallpaper, and those who asked are answered.
    fn take_wallpapers(&mut self) {
        let Some(loader) = &self.loader else { return };
        for done in loader.take() {
            let asker = match self.loading.iter().position(|(t, _)| *t == done.ticket) {
                Some(i) => self.loading.remove(i).1,
                None => None,
            };
            let result = done.result.map(|wp| self.set_wallpaper(wp));
            if let Err(e) = result {
                println!("wallpaper not changed: {:?}", e);
            }
            if let Some((client, txid)) = asker
                && let Some(c) = self.clients.iter_mut().find(|c| c.id == client)
            {
                c.loading = false;
                answer_wallpaper(&c.channel, txid, result);
            }
        }
    }

    fn set_wallpaper(&mut self, wp: Wallpaper) {
        if let Some(vfs) = &self.vfs {
            write_file(vfs, wallpaper::CONFIG, wp.path.as_bytes());
        }
        self.model.wallpaper = wp;
        self.model.wallpaper_generation += 1;
        self.invalidate_all();
    }

    fn wallpapers(&self) -> Vec<String> {
        let mut list = self.vfs.as_ref().map(wallpaper::list).unwrap_or_default();
        list.push(wallpaper::PROCEDURAL.into());
        list
    }

    fn next_wallpaper(&mut self) {
        let list = self.wallpapers();
        let cur = list.iter().position(|p| *p == self.model.wallpaper.path);
        let next = cur.map_or(0, |i| (i + 1) % list.len());
        if let Some(Err(e)) = self.load_wallpaper(&list[next].clone(), None) {
            println!("wallpaper not changed: {:?}", e);
        }
    }

    fn invalidate_all(&mut self) {
        self.desktop_host.invalidate();
        self.taskbar_host.invalidate();
        if let Some(p) = &mut self.popup {
            p.host.invalidate();
        }
        self.notes.invalidate_all();
    }

    /// Re-reads the audio system's state, reconnecting if the audio
    /// service was restarted.
    fn refresh_audio(&mut self) {
        if self.audio.is_none() {
            self.audio = connect_running(audio::NAME).map(audio::Client::new);
            if let Some(a) = &self.audio {
                a.set_timeout(AUDIO_TIMEOUT_NS);
            }
        }
        self.model.audio = self.audio.as_ref().and_then(|a| a.status().ok());
        if self.model.audio.is_none() {
            self.audio = None;
        }
        self.taskbar_host.invalidate();
    }

    fn set_volume(&mut self, volume: f32, muted: bool) {
        let Some(a) = &self.audio else { return };
        let volume = volume.clamp(0.0, 1.0);
        let _ = a.set_master(volume, muted);
        if let Some(st) = &mut self.model.audio {
            st.master_volume = volume;
            st.muted = muted;
        }
        self.taskbar_host.invalidate();
        if let Some(p) = &mut self.popup
            && p.kind == PopupKind::Volume
        {
            p.host.invalidate();
        }
    }

    fn refresh_windows(&mut self) {
        if let Ok(list) = self.display.list_windows() {
            self.model.windows =
                list.into_iter().filter(|w| matches!(w.kind, WindowKind::Normal | WindowKind::Borderless)).collect();
        }
        self.taskbar_host.invalidate();
        if let Some(p) = &mut self.power {
            p.invalidate();
        }
    }

    fn launch(&mut self, app: &str, args: Vec<String>) {
        let Some(launcher) = &self.launcher else { return };
        let name = self.model.app(app).map(|a| a.name.clone()).unwrap_or_else(|| app.into());
        match launcher.launch_app(app.into(), args) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                println!("cannot start {app}: {e}");
                self.post_notification(
                    &format!("Couldn't open {name}"),
                    &format!("The application could not be started: {e}."),
                    "warning",
                );
            }
            Err(e) => println!("launcher unavailable: {}", e),
        }
    }

    /// Shows a notification of the system's: `icon` is an installed
    /// application's id (its tile) or an icon name (see
    /// `ui::Icon::by_name`).
    fn post_notification(&mut self, title: &str, body: &str, icon: &str) {
        let glyph = self.model.app(icon).map(apps::icon_for).or_else(|| Icon::by_name(icon)).unwrap_or(Icon::Info);
        let sender = notify::Sender { tile: (icon, glyph), name: None };
        self.notes.post(&self.display, &self.model, title, body, sender);
    }

    /// Whether process `koid` may post another notification now (and
    /// counts it).
    fn note_allowed(&mut self, koid: u64, now: u64) -> bool {
        self.note_counts.retain(|&(_, start, _)| now < start + NOTE_PERIOD_NS);
        match self.note_counts.iter_mut().find(|e| e.0 == koid) {
            Some(e) if e.2 >= NOTES_PER_PERIOD => false,
            Some(e) => {
                e.2 += 1;
                true
            }
            None => {
                self.note_counts.push((koid, now, 1));
                true
            }
        }
    }

    fn popup_spec(title: &str, r: Rect) -> WindowSpec {
        let mut spec = WindowSpec::new(title, r.w as u32, r.h as u32);
        spec.kind = WindowKind::Popup;
        spec.x = r.x;
        spec.y = r.y;
        spec.resizable = false;
        spec.app_id = "shell".into();
        spec
    }

    /// Opens popup `kind`, closing the one open.
    fn open_popup(&mut self, kind: PopupKind) {
        self.close_popup();
        let screen = self.model.screen;
        let (r, title, what) = match kind {
            PopupKind::Start => (start::placement(screen), "Start", "the start menu"),
            PopupKind::Calendar => (calendar::placement(screen), "Calendar", "the calendar"),
            PopupKind::Volume => {
                self.refresh_audio();
                (volume::placement(screen), "Volume", "the volume control")
            }
            PopupKind::Wifi => {
                self.refresh_wifi();
                let _ = net::wifi::scan();
                (wifi::placement(screen), "Network", "the network flyout")
            }
        };
        let origin = (r.x, r.y);
        let ui = match kind {
            PopupKind::Start => PopupUi::Start(start::StartMenu::new(origin)),
            PopupKind::Calendar => PopupUi::Calendar(calendar::Calendar::new(origin)),
            PopupKind::Volume => PopupUi::Volume(volume::VolumeFlyout::new(origin)),
            PopupKind::Wifi => PopupUi::Wifi(wifi::WifiFlyout::new(origin)),
        };
        match Host::new(&self.display, Self::popup_spec(title, r)) {
            Ok(host) => {
                self.popup = Some(Popup { kind, host, ui });
                self.model.popup = Some(kind);
                self.taskbar_host.invalidate();
            }
            Err(e) => println!("cannot open {what}: {:?}", e),
        }
    }

    /// Closes the popup that is open.
    fn close_popup(&mut self) {
        if self.popup.take().is_some() {
            self.model.popup = None;
            self.taskbar_host.invalidate();
        }
    }

    /// Closes popup `kind` if it is the one open.
    fn close_popup_of(&mut self, kind: PopupKind) {
        if self.model.popup == Some(kind) {
            self.close_popup();
        }
    }

    fn open_agent(&mut self) {
        self.close_popup();
        let r = agent::placement(self.model.screen);
        let mut spec = WindowSpec::new(self.model.agent.name(), r.w as u32, r.h as u32);
        // A panel: it stays above the windows the agent opens or the user
        // works in, without taking the keyboard from them.
        spec.kind = WindowKind::Panel;
        spec.x = r.x;
        spec.y = r.y;
        spec.resizable = false;
        spec.app_id = "agent".into();
        match Host::new(&self.display, spec) {
            Ok(host) => {
                self.agent_win = Some(AgentWin { host, ui: agent::AgentWindow::new() });
                self.model.agent.window_open = true;
                // Approvals now appear in the window.
                self.notes.remove_approvals(self.model.screen);
                if let Some(a) = &self.agent {
                    a.set_window_open(true);
                    // Opening the agent means wanting to talk.
                    if !self.model.agent.active() {
                        a.wake();
                    }
                }
            }
            Err(e) => println!("cannot open the agent's window: {:?}", e),
        }
    }

    fn close_agent(&mut self) {
        if self.agent_win.take().is_none() {
            return;
        }
        self.model.agent.window_open = false;
        if let Some(a) = &self.agent {
            a.set_window_open(false);
        }
        for req in self.model.agent.approvals.clone() {
            self.notes.post_approval(&self.display, &self.model, &req);
        }
    }

    /// Attaches the agent's interface and takes its events.
    fn check_agent(&mut self) {
        let now = rt::time::now_ns();
        if self.agent.is_none() && now >= self.next_agent_try {
            self.next_agent_try = now + 2_000_000_000;
            self.agent = agent::AgentLink::connect();
            if let Some(a) = &self.agent {
                a.set_window_open(self.agent_win.is_some());
            }
        }
        let Some(link) = &self.agent else { return };
        let (voice, mic) = link.levels();
        let (v0, m0) = self.model.agent.levels;
        let smooth = |old: f32, new: f32| if new > old { new } else { old * 0.75 + new * 0.25 };
        self.model.agent.levels = (smooth(v0, voice), smooth(m0, mic));
        let Some(events) = link.take_events() else {
            // The agent service went away; it will be restarted.
            self.agent = None;
            self.model.agent.status = None;
            self.model.agent.approvals.clear();
            self.notes.remove_approvals(self.model.screen);
            self.taskbar_host.invalidate();
            return;
        };
        let changed = !events.is_empty();
        for ev in events {
            match ev {
                proto::agent::AgentEvent::Status { status } => self.model.agent.set_status(status, now),
                proto::agent::AgentEvent::Approval { request } => {
                    if !self.model.agent.approvals.iter().any(|r| r.id == request.id) {
                        if self.agent_win.is_none() {
                            self.notes.post_approval(&self.display, &self.model, &request);
                        }
                        self.model.agent.approvals.push(request);
                    }
                }
                proto::agent::AgentEvent::ApprovalDone { id } => {
                    self.model.agent.approvals.retain(|r| r.id != id);
                    self.notes.remove_approval(id, self.model.screen);
                }
            }
        }
        // The orb is redrawn when something changed, and keeps moving
        // while the agent talks or listens (the wait wakes us for that).
        if changed || self.model.agent.active() {
            self.taskbar_host.invalidate();
        }
        if changed && let Some(p) = &mut self.agent_win {
            p.host.invalidate();
        }
    }

    fn invalidate_network(&mut self) {
        self.taskbar_host.invalidate();
        if let Some(p) = &mut self.popup
            && p.kind == PopupKind::Wifi
        {
            p.host.invalidate();
        }
    }

    /// Re-reads the Wi-Fi status and the networks in range.
    fn refresh_wifi(&mut self) {
        match net::wifi::status() {
            Ok(s) => {
                self.model.wifi = Some(s);
                self.model.wifi_networks = net::wifi::networks().unwrap_or_default();
            }
            Err(_) => {
                self.model.wifi = None;
                self.model.wifi_networks.clear();
                self.wifi_watch = None;
            }
        }
        self.invalidate_network();
    }

    /// Looks at the wired network, and (re)connects to the Wi-Fi service's
    /// events if needed. Runs every few seconds.
    fn poll_network(&mut self) {
        let now = rt::time::now_ns();
        if now < self.next_net_poll {
            return;
        }
        // Every second while the Wi-Fi or the network service is away (it
        // may be restarting), otherwise every five.
        let away = self.wifi_watch.is_none() || self.firewall_watch.is_none();
        self.next_net_poll = now + if away { 1_000_000_000 } else { 5_000_000_000 };
        let (up, label) = match net::interfaces() {
            Ok(list) => match list
                .iter()
                .find(|i| i.kind == net::InterfaceKind::Ethernet && i.link_up && !i.gateways.is_empty())
            {
                Some(i) => (true, format!("Wired: {} connected", i.name)),
                None => (false, String::new()),
            },
            Err(_) => (false, String::new()),
        };
        if (up, &label) != (self.model.wired_up, &self.model.wired_label) {
            self.model.wired_up = up;
            self.model.wired_label = label;
            self.invalidate_network();
        }
        if self.wifi_watch.is_none() && net::wifi::available() {
            self.wifi_watch = net::wifi::Watcher::new().ok();
            self.refresh_wifi();
        }
        if self.firewall_watch.is_none()
            && let Ok(ch) = net::watch_firewall()
        {
            // The questions waiting come first on it: those known before
            // may have been settled meanwhile.
            self.firewall_watch = Some(ch);
            self.model.firewall.clear();
            self.notes.show_questions(&self.display, &self.model);
        }
    }

    /// Takes the firewall's notices: questions to ask, and those settled.
    fn check_firewall(&mut self) {
        let Some(ch) = &self.firewall_watch else { return };
        let mut notices = Vec::new();
        let gone = loop {
            match net::next_firewall_notice(ch) {
                Ok(Some(n)) => notices.push(n),
                Ok(None) => break false,
                Err(_) => break true,
            }
        };
        if !gone && notices.is_empty() {
            return;
        }
        if gone {
            // The network service went away, and its questions with it.
            self.firewall_watch = None;
            self.model.firewall.clear();
        }
        for n in notices {
            match n {
                net::FirewallNotice::Asked { question } => {
                    if !self.model.firewall.iter().any(|q| q.app == question.app) {
                        self.model.firewall.push(question);
                    }
                }
                net::FirewallNotice::Settled { app } => self.model.firewall.retain(|q| q.app != app),
            }
        }
        self.notes.show_questions(&self.display, &self.model);
    }

    /// Handles Wi-Fi events: status changes, finished scans, failures.
    fn check_wifi(&mut self) {
        let Some(w) = &self.wifi_watch else { return };
        let mut events = Vec::new();
        let gone = loop {
            match w.try_next() {
                Ok(Some(ev)) => events.push(ev),
                Ok(None) => break false,
                Err(_) => break true,
            }
        };
        if gone {
            self.wifi_watch = None;
            self.model.wifi = None;
            self.model.wifi_networks.clear();
            self.invalidate_network();
        }
        for ev in events {
            match ev {
                net::wifi::WlanEvent::StatusChanged { status } => {
                    let changed_network = self.model.wifi.as_ref().map(|s| (s.state, s.ssid.clone()))
                        != Some((status.state, status.ssid.clone()));
                    if status.state == net::wifi::ConnState::Connected {
                        self.model.wifi_error = None;
                    }
                    self.model.wifi = Some(status);
                    if changed_network {
                        self.model.wifi_networks = net::wifi::networks().unwrap_or_default();
                    }
                    self.invalidate_network();
                }
                net::wifi::WlanEvent::ScanDone {} => {
                    self.model.wifi_networks = net::wifi::networks().unwrap_or_default();
                    self.invalidate_network();
                }
                net::wifi::WlanEvent::ConnectFailed { ssid, name, reason } => {
                    let why = format!("{reason}");
                    let mut msg = why.clone();
                    if let Some(c) = msg.get_mut(0..1) {
                        c.make_ascii_uppercase();
                    }
                    self.model.wifi_error = Some((ssid.0, msg));
                    if self.model.popup != Some(PopupKind::Wifi) {
                        self.post_notification(
                            &format!("Couldn't connect to {name}"),
                            &format!("Because {why}."),
                            "wifi",
                        );
                    }
                    self.invalidate_network();
                }
            }
        }
    }

    fn wifi_connect(
        &mut self,
        ssid: Vec<u8>,
        security: Option<net::wifi::Security>,
        password: Option<String>,
        auto: bool,
        hidden: bool,
    ) {
        self.model.wifi_error = None;
        let opts = net::wifi::ConnectOptions { security, save: true, auto_connect: auto, hidden };
        if let Err(e) = net::wifi::connect_with(&ssid, password.as_deref(), &opts) {
            let mut msg = format!("{e}");
            if let Some(c) = msg.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            self.model.wifi_error = Some((ssid, msg));
        }
        self.invalidate_network();
    }

    fn show_desktop(&mut self) {
        let visible: Vec<u32> =
            self.model.windows.iter().filter(|w| w.state != WindowState::Minimized).map(|w| w.id).collect();
        if visible.is_empty() {
            for id in core::mem::take(&mut self.hidden_by_show_desktop) {
                let _ = self.display.activate_window(id);
            }
        } else {
            for &id in &visible {
                let _ = self.display.minimize_window(id);
            }
            self.hidden_by_show_desktop = visible;
        }
    }

    /// Shows, replaces or hides the taskbar tooltip.
    fn update_tooltip(&mut self) {
        let popup_open = self.popup.is_some();
        let due = self.model.tip.as_ref().filter(|t| !popup_open && rt::time::now_ns() >= t.since + tooltip::DELAY_NS);
        match due {
            Some(tip) => {
                if self.tooltip.as_ref().map(|t| t.label()) != Some(tip.label.as_str()) {
                    self.tooltip = tooltip::Tooltip::open(&self.display, self.model.screen, tip);
                }
            }
            None => self.tooltip = None,
        }
        if let Some(t) = &mut self.tooltip {
            t.pump();
        }
    }

    // ---- main loop ----------------------------------------------------------

    /// Lets every surface process its events and draw.
    fn pump(&mut self) {
        let now = rt::time::now_ns();
        let Shell { model, desktop_host, desktop, taskbar_host, taskbar, popup, notes, .. } = self;
        let desktop_events = desktop_host.pump(|ui| desktop.update(ui, model));
        let appearing = desktop_events.iter().any(|e| matches!(e, WindowEvent::Appearing {}));
        let mut windows_changed = false;
        for ev in taskbar_host.pump(|ui| taskbar.update(ui, model)) {
            match ev {
                WindowEvent::WindowsChanged {} => windows_changed = true,
                WindowEvent::StartMenuKey {} => model.push(Action::TogglePopup(PopupKind::Start)),
                WindowEvent::AgentKey {} => model.push(Action::ToggleAgent),
                _ => {}
            }
        }
        if let Some(p) = &mut self.agent_win {
            let events = p.host.pump(|ui| p.ui.update(ui, model));
            if p.host.window.closed
                || p.host.close_requested
                || events.iter().any(|e| matches!(e, WindowEvent::CloseRequested {}))
            {
                model.push(Action::CloseAgent);
            }
        }
        if let Some(p) = popup {
            let events = p.host.pump(|ui| p.ui.update(ui, model));
            // Closed by a click outside it (or by the window system).
            if p.host.window.closed || events.iter().any(|e| matches!(e, WindowEvent::CloseRequested {})) {
                model.dismissed = Some((p.kind, now));
                model.push(Action::ClosePopup(p.kind));
            } else if p.host.close_requested {
                model.push(Action::ClosePopup(p.kind));
            }
        }
        notes.pump(model);
        if windows_changed {
            self.refresh_windows();
        }
        if appearing && let Some(s) = &mut self.startup {
            s.appearing(self.audio.as_ref(), now);
        }
    }

    fn perform_actions(&mut self) {
        for action in core::mem::take(&mut self.model.actions) {
            match action {
                Action::Launch(app, args) => {
                    self.close_popup_of(PopupKind::Start);
                    self.launch(&app, args);
                }
                Action::Activate(id) => {
                    let _ = self.display.activate_window(id);
                }
                Action::Minimize(id) => {
                    let _ = self.display.minimize_window(id);
                }
                Action::TogglePopup(kind) => {
                    if self.model.popup == Some(kind) {
                        self.close_popup();
                    } else {
                        self.open_popup(kind);
                    }
                }
                Action::ClosePopup(kind) => self.close_popup_of(kind),
                Action::WifiRadio(on) => {
                    let _ = net::wifi::set_radio(on);
                    self.refresh_wifi();
                }
                Action::WifiScan => {
                    let _ = net::wifi::scan();
                    self.refresh_wifi();
                }
                Action::WifiConnect { ssid, security, password, auto, hidden } => {
                    self.wifi_connect(ssid, security, password, auto, hidden)
                }
                Action::WifiDisconnect => {
                    let _ = net::wifi::disconnect();
                    self.refresh_wifi();
                }
                Action::ToggleAgent => {
                    if self.agent_win.is_some() {
                        self.close_agent();
                    } else {
                        self.open_agent();
                    }
                }
                Action::CloseAgent => self.close_agent(),
                Action::AgentWake => {
                    if let Some(a) = &self.agent {
                        a.wake();
                    }
                }
                Action::AgentSleep => {
                    if let Some(a) = &self.agent {
                        a.sleep();
                    }
                }
                Action::AgentDecide { id, allow, always } => {
                    if let Some(a) = &self.agent {
                        a.decide(id, allow, always);
                    }
                    self.model.agent.approvals.retain(|r| r.id != id);
                    self.notes.remove_approval(id, self.model.screen);
                    self.taskbar_host.invalidate();
                }
                Action::AgentMute(muted) => {
                    if let Some(a) = &self.agent {
                        a.set_muted(muted);
                    }
                }
                Action::FirewallAnswer { app, from } => {
                    if let Err(e) = net::answer_firewall(&app, from) {
                        // Watched again, the service says which questions
                        // still wait.
                        println!("the firewall did not take the answer about {app}: {e}");
                        self.firewall_watch = None;
                    }
                    self.model.firewall.retain(|q| q.app != app);
                    self.notes.show_questions(&self.display, &self.model);
                }
                Action::OpenNetworkSettings => {
                    self.close_popup_of(PopupKind::Wifi);
                    self.launch("settings", vec![String::from("network")]);
                }
                Action::SetVolume(v, muted) => self.set_volume(v, muted),
                Action::ShowDesktop => self.show_desktop(),
                Action::Lock => {
                    self.close_popup_of(PopupKind::Start);
                    if !matches!(self.display.lock(), Ok(Ok(()))) {
                        println!("the window system did not lock the screen");
                    }
                }
                Action::Power(what) => {
                    self.close_popup_of(PopupKind::Start);
                    self.begin_power_off(what);
                }
                Action::NextWallpaper => self.next_wallpaper(),
                Action::RefreshDesktop => self.refresh_desktop(),
                Action::Trash(path) => {
                    match files::trash::put(&self.fs, &path) {
                        Ok(_) => println!("moved {} to the Trash", path),
                        Err(e) => {
                            let name = files::path::file_name(&path);
                            let title = format!("Couldn't move “{name}” to the Trash");
                            self.post_notification(&title, &format!("Because {e}."), "files");
                        }
                    }
                    self.refresh_desktop();
                }
                Action::MoveInto(path, dir) => {
                    let name = files::path::file_name(&path).to_string();
                    let to = files::path::join(&dir, &self.fs.unique_name(&dir, &name));
                    match self.fs.move_to(&path, &to) {
                        Ok(()) => println!("moved {} into {}", path, dir),
                        Err(e) => {
                            let into = files::path::display_path(&dir);
                            let title = format!("Couldn't move “{name}” into {into}");
                            self.post_notification(&title, &format!("Because {e}."), "files");
                        }
                    }
                    self.refresh_desktop();
                }
            }
        }
    }

    /// Rebuilds the desktop's icons.
    fn refresh_desktop(&mut self) {
        self.desktop.refresh(&self.model, &self.fs);
        self.desktop_host.invalidate();
    }

    /// Brings the desktop's icons up to date when `~/Desktop` or the Trash
    /// changed (Files, the Terminal and the agent change them too), unless
    /// an icon is being dragged or a menu is open.
    fn poll_desktop(&mut self) {
        let now = rt::time::now_ns();
        if now < self.next_desktop_poll {
            return;
        }
        self.next_desktop_poll = now + DESKTOP_POLL_NS;
        if !self.desktop.busy() && self.desktop.stale(&self.fs) {
            self.refresh_desktop();
        }
    }

    /// Accepts connections (as many as allowed) and answers the `shell`
    /// protocol requests of the clients that sent any.
    fn serve(&mut self) {
        if let Some(l) = &self.listener {
            while let Some((channel, who)) = proto::accept_with_identity(l) {
                let own = self.clients.iter().filter(|c| c.who.koid == who.koid).count();
                if self.clients.len() >= MAX_CLIENTS || own >= MAX_CLIENTS_EACH {
                    // Dropped: the client sees the connection closed.
                    println!("{} has too many connections to the shell; one refused", who.name);
                    continue;
                }
                self.next_client += 1;
                // Its first requests may be waiting already.
                self.ready.push(self.next_client);
                self.clients.push(Client { id: self.next_client, channel, who, loading: false });
            }
        }
        for id in core::mem::take(&mut self.ready) {
            let Some(i) = self.clients.iter().position(|c| c.id == id) else { continue };
            let mut client = self.clients.swap_remove(i);
            self.caller = client.who.clone();
            let alive = loop {
                match client.channel.read() {
                    Ok(msg) => self.request(&mut client, msg),
                    Err(abi::Error::ShouldWait) => break true,
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
        if ipc::Header::parse(&msg.bytes).is_ok_and(|h| h.ordinal == SET_WALLPAPER) {
            return self.wallpaper_request(client, msg);
        }
        match shell::dispatch(self, msg) {
            Ok(reply) => {
                let _ = reply.send(&client.channel);
            }
            Err(e) => println!("bad request: {}", e),
        }
    }

    /// `shell::set_wallpaper`: the picture is loaded on the loader's
    /// thread, and the request answered once it is set. A client asks for
    /// one wallpaper at a time: a second request while its first loads is
    /// refused.
    fn wallpaper_request(&mut self, client: &mut Client, mut msg: Message) {
        let decoded = ipc::open(&mut msg).and_then(|(h, mut d)| {
            let path = String::decode(&mut d)?;
            d.finish()?;
            Ok((h, path))
        });
        let (header, path) = match decoded {
            Ok(v) => v,
            Err(_) => {
                if let Ok(h) = ipc::Header::parse(&msg.bytes) {
                    let _ = ipc::refuse(&client.channel, h, ipc::Refusal::Malformed);
                }
                return;
            }
        };
        if client.loading {
            return answer_wallpaper(&client.channel, header.txid, Err(ShellError::Unavailable));
        }
        match self.load_wallpaper(&path, Some((client.id, header.txid))) {
            None => client.loading = true,
            Some(result) => answer_wallpaper(&client.channel, header.txid, result),
        }
    }

    /// Restart or Shut down: every application window is asked to close
    /// first (see [`power`]).
    fn begin_power_off(&mut self, what: u32) {
        self.refresh_windows();
        if self.model.windows.is_empty() {
            return self.power_off(what);
        }
        for w in &self.model.windows {
            let _ = self.display.close_window(w.id);
        }
        self.power = Some(power::PowerOff::new(what, rt::time::now_ns()));
    }

    /// Follows a restart or shutdown that waits for the applications: on
    /// once none is left (or the user says so), off if the user cancels.
    fn check_power(&mut self) {
        let Some(p) = &mut self.power else { return };
        let what = p.what;
        if self.model.windows.is_empty() {
            return self.power_off(what);
        }
        match p.pump(&self.display, &self.model, rt::time::now_ns()) {
            Some(power::Choice::Force) => self.power_off(what),
            Some(power::Choice::Cancel) => self.power = None,
            None => {}
        }
    }

    /// Writes the home folder to disk and restarts or shuts down.
    fn power_off(&mut self, what: u32) {
        self.power = None;
        if let Some(vfs) = &self.vfs {
            vfs.set_timeout(SYNC_TIMEOUT_NS);
            let _ = vfs.sync();
            vfs.set_timeout(SERVICE_TIMEOUT_NS);
        }
        let why = match self.launcher.as_ref().map(|l| l.power(what)) {
            Some(Ok(Ok(()))) => return,
            Some(Ok(Err(e))) => e.to_string(),
            Some(Err(_)) => "the launcher is not responding".to_string(),
            None => "the launcher is not available".to_string(),
        };
        let what = if what == proto::init::power::REBOOT { "restart" } else { "shut down" };
        println!("cannot {what}: {why}");
        self.post_notification(&format!("Couldn't {what}"), &format!("Because {why}."), "warning");
    }

    /// Turns the launcher's crash reports into notifications, and takes the
    /// installed applications anew when they changed.
    fn check_tasks(&mut self) {
        let Some(ch) = self.task_events.take() else { return };
        let mut crashed = Vec::new();
        let mut apps_changed = false;
        let alive = loop {
            match ch.read() {
                Ok(msg) => match ipc::decode_event::<TaskEvent>(msg) {
                    Ok((_, TaskEvent::Crashed { name, .. })) => crashed.push(name),
                    Ok((_, TaskEvent::AppsChanged {})) => apps_changed = true,
                    Err(_) => {}
                },
                Err(abi::Error::ShouldWait) => break true,
                Err(_) => break false,
            }
        };
        if alive {
            self.task_events = Some(ch);
        }
        if apps_changed {
            self.refresh_apps();
        }
        for name in crashed {
            let (title, body, icon) = match self.model.app(&name) {
                Some(app) => (
                    app.name.clone(),
                    "It stopped because of an error. You can open it again from the start menu.",
                    app.id.clone(),
                ),
                None => (name, "It stopped because of an error.", String::from("warning")),
            };
            println!("{} closed unexpectedly", title);
            self.post_notification(&format!("{title} closed unexpectedly"), body, &icon);
        }
    }

    /// Takes the installed applications from the launcher again (packages
    /// added or removed some), for the start menu and the taskbar.
    fn refresh_apps(&mut self) {
        let Some(Ok(mut apps)) = self.launcher.as_ref().map(|l| l.apps()) else { return };
        apps::sort(&mut apps);
        self.model.apps = apps;
        println!("the applications changed ({} apps)", self.model.apps.len());
        self.taskbar_host.invalidate();
        if let Some(p) = &mut self.popup {
            p.host.invalidate();
        }
    }

    /// Blocks until a surface, client or timer needs attention.
    fn wait(&mut self) {
        let mut items: Vec<WaitItem> = Vec::new();
        let mut deadline = abi::DEADLINE_INFINITE;
        let mut add_host = |h: &Host, items: &mut Vec<WaitItem>| {
            items.push(h.wait_item());
            deadline = deadline.min(h.deadline());
        };
        add_host(&self.desktop_host, &mut items);
        add_host(&self.taskbar_host, &mut items);
        if let Some(p) = &self.popup {
            add_host(&p.host, &mut items);
        }
        if let Some(p) = &self.agent_win {
            add_host(&p.host, &mut items);
        }
        if let Some(h) = self.power.as_ref().and_then(|p| p.host()) {
            add_host(h, &mut items);
        }
        if let Some(t) = &self.tooltip {
            add_host(t.host(), &mut items);
        }
        for h in self.notes.hosts() {
            add_host(h, &mut items);
        }
        deadline = deadline.min(self.notes.deadline());
        if let Some(p) = &self.power {
            deadline = deadline.min(p.deadline());
        }
        if let (Some(tip), None) = (&self.model.tip, &self.tooltip) {
            deadline = deadline.min(tip.since + tooltip::DELAY_NS);
        }
        let readable = signals::READABLE | signals::PEER_CLOSED;
        if let Some(l) = &self.listener {
            items.push(WaitItem { handle: l.raw(), signals: readable, ..Default::default() });
        }
        let first_client = items.len();
        for c in &self.clients {
            items.push(WaitItem { handle: c.channel.raw(), signals: readable, ..Default::default() });
        }
        if let Some(l) = &self.loader {
            items.push(WaitItem { handle: l.event_handle(), signals: signals::SIGNALED, ..Default::default() });
        }
        if let Some(c) = &self.task_events {
            items.push(WaitItem { handle: c.raw(), signals: readable, ..Default::default() });
        }
        if let Some(w) = &self.wifi_watch {
            items.push(WaitItem { handle: w.handle().raw(), signals: readable, ..Default::default() });
        }
        if let Some(ch) = &self.firewall_watch {
            items.push(WaitItem { handle: ch.raw(), signals: readable, ..Default::default() });
        }
        match &self.agent {
            Some(a) => {
                items.push(WaitItem { handle: a.events_handle(), signals: readable, ..Default::default() });
                // The tray's orb moves while the agent talks or listens.
                if self.model.agent.active() {
                    deadline = deadline.min(rt::time::now_ns() + 33_000_000);
                }
            }
            None => deadline = deadline.min(self.next_agent_try),
        }
        deadline = deadline.min(self.next_net_poll).min(self.next_desktop_poll);
        if let Some(s) = &self.startup {
            deadline = deadline.min(s.deadline());
        }
        items.truncate(abi::WAIT_MANY_MAX);
        if !self.model.actions.is_empty() {
            deadline = 0;
        }
        let _ = rt::object::wait_many(&mut items, deadline);
        // The clients to read: those with something to read.
        for (c, it) in self.clients.iter().zip(items.iter().skip(first_client)) {
            if it.observed & it.signals != 0 {
                self.ready.push(c.id);
            }
        }
    }

    /// Runs until the display goes away (the window system stopped); `init`
    /// then restarts the shell along with it. Returns the exit code.
    fn run(&mut self) -> i32 {
        loop {
            self.pump();
            self.update_tooltip();
            if self.desktop_host.window.closed || self.taskbar_host.window.closed {
                println!("lost the display; exiting");
                return 2;
            }
            self.perform_actions();
            self.serve();
            self.take_wallpapers();
            self.check_power();
            self.check_tasks();
            self.poll_network();
            self.poll_desktop();
            self.check_wifi();
            self.check_firewall();
            self.check_agent();
            self.play_startup_sound();
            self.wait();
        }
    }

    /// Readies the startup sound once a sound card is there, saying then
    /// that the desktop may appear, and closes its stream once it has
    /// played (it plays as the desktop appears: `pump`).
    fn play_startup_sound(&mut self) {
        if self.startup.is_none() {
            return;
        }
        if self.audio.is_none() {
            self.refresh_audio();
        }
        if let Some(s) = &mut self.startup {
            if s.poll(self.audio.as_ref(), self.vfs.as_ref(), rt::time::now_ns()) {
                self.desktop_may_appear();
            }
            if self.startup.as_ref().is_some_and(|s| s.done()) {
                self.startup = None;
            }
        }
    }

    /// Tells the window system that what comes with the desktop's first
    /// appearance is ready: the startup sequence may dissolve into it.
    fn desktop_may_appear(&self) {
        if let Ok(Err(e)) = self.display.desktop_ready() {
            println!("the window system does not take the desktop's appearance from the shell: {:?}", e);
        }
    }
}

/// Answers a `shell::set_wallpaper` request.
fn answer_wallpaper(channel: &Channel, txid: u32, result: Result<(), ShellError>) {
    let mut e = ipc::Encoder::with_header(SET_WALLPAPER, txid, ipc::FLAG_RESPONSE);
    result.encode(&mut e);
    let _ = e.send(channel);
}

impl shell::Server for Shell {
    /// Not reached: `Shell::wallpaper_request` answers these once the
    /// picture is loaded.
    fn set_wallpaper(&mut self, _path: String) -> Result<(), ShellError> {
        Err(ShellError::Unavailable)
    }

    fn wallpaper(&mut self) -> String {
        self.model.wallpaper.path.clone()
    }

    /// A notification from the caller. A system service's shows the tile it
    /// names (the agent's ring); an application's shows that it is the
    /// application's, its tile and its name, whatever it says of itself.
    /// A process posts at most [`NOTES_PER_PERIOD`] in a period.
    fn notify(&mut self, title: String, body: String, icon: String) {
        let who = self.caller.clone();
        if !self.note_allowed(who.koid, rt::time::now_ns()) {
            println!("{} posts too many notifications; one left out", who.name);
            return;
        }
        if who.service {
            return self.post_notification(&title, &body, &icon);
        }
        let (key, glyph, name) = match self.model.app(&who.name) {
            Some(app) => (app.id.clone(), apps::icon_for(app), app.name.clone()),
            None => {
                let name = if who.name.is_empty() { String::from("A program") } else { who.name.clone() };
                (String::from("program"), Icon::Info, name)
            }
        };
        let sender = notify::Sender { tile: (&key, glyph), name: Some(&name) };
        self.notes.post(&self.display, &self.model, &title, &body, sender);
    }

    fn wallpapers(&mut self) -> Vec<String> {
        Shell::wallpapers(self)
    }

    fn window_action(&mut self, window: u32, action: String) -> Result<(), ShellError> {
        if !(self.caller.service && self.caller.name == "agent") {
            return Err(ShellError::Unavailable);
        }
        use proto::display::arrangement;
        let r = match action.as_str() {
            "show_desktop" => {
                self.show_desktop();
                return Ok(());
            }
            "focus" => self.display.activate_window(window),
            "minimize" => self.display.minimize_window(window),
            "close" => self.display.close_window(window),
            "maximize" => self.display.arrange_window(window, arrangement::MAXIMIZE),
            "restore" => self.display.arrange_window(window, arrangement::RESTORE),
            "snap_left" => self.display.arrange_window(window, arrangement::SNAP_LEFT),
            "snap_right" => self.display.arrange_window(window, arrangement::SNAP_RIGHT),
            _ => return Err(ShellError::NotFound),
        };
        match r {
            Ok(Ok(())) => Ok(()),
            _ => Err(ShellError::NotFound),
        }
    }
}

fn main() {
    std::process::exit(run());
}

/// Runs the program; its exit code.
fn run() -> i32 {
    let display = match ui::window::connect() {
        Ok(d) => d,
        Err(e) => {
            println!("cannot connect to the display: {:?}", e);
            return 1;
        }
    };
    // A window system, launcher or file system that hangs must not freeze
    // the desktop: calls to them give up after a while.
    display.set_timeout(SERVICE_TIMEOUT_NS);
    let screen = match display.screen_info() {
        Ok(s) => Rect::new(0, 0, s.width as i32, s.height as i32),
        Err(e) => {
            println!("cannot query the screen: {}", e);
            return 1;
        }
    };
    let vfs = proto::connect(vfs::NAME).ok().map(vfs::Client::new);
    let launcher = proto::connect(launcher::NAME).ok().map(launcher::Client::new);
    if let Some(c) = &vfs {
        c.set_timeout(SERVICE_TIMEOUT_NS);
    }
    if let Some(c) = &launcher {
        c.set_timeout(SERVICE_TIMEOUT_NS);
    }
    let task_events = launcher.as_ref().and_then(|l| l.watch().ok()).and_then(|r| r.ok());
    let audio = connect_running(audio::NAME).map(audio::Client::new);
    if let Some(a) = &audio {
        a.set_timeout(AUDIO_TIMEOUT_NS);
    }
    let mut apps = launcher.as_ref().and_then(|l| l.apps().ok()).unwrap_or_default();
    apps::sort(&mut apps);

    let wallpaper = wallpaper::chosen(vfs.as_ref(), screen.w, screen.h);

    let mut spec = WindowSpec::new("Desktop", screen.w as u32, screen.h as u32);
    spec.kind = WindowKind::Desktop;
    spec.x = 0;
    spec.y = 0;
    spec.resizable = false;
    spec.app_id = "shell".into();
    let desktop_host = match Host::new(&display, spec) {
        Ok(h) => h,
        Err(e) => {
            println!("cannot create the desktop: {:?}", e);
            return 1;
        }
    };
    let mut spec = WindowSpec::new("Taskbar", screen.w as u32, taskbar::HEIGHT as u32);
    spec.kind = WindowKind::Panel;
    spec.x = 0;
    spec.y = screen.h - taskbar::HEIGHT;
    spec.resizable = false;
    spec.app_id = "shell".into();
    let taskbar_host = match Host::new(&display, spec) {
        Ok(h) => h,
        Err(e) => {
            println!("cannot create the taskbar: {:?}", e);
            return 1;
        }
    };

    let listener = proto::register(shell::NAME).ok();
    if listener.is_none() {
        println!("cannot register the shell service");
    }
    let model = Model {
        screen,
        apps,
        windows: Vec::new(),
        wallpaper,
        wallpaper_generation: 0,
        audio: None,
        wifi: None,
        wifi_networks: Vec::new(),
        wifi_error: None,
        wired_up: false,
        wired_label: String::new(),
        popup: None,
        dismissed: None,
        tip: None,
        agent: agent::AgentModel::default(),
        firewall: Vec::new(),
        actions: Vec::new(),
    };
    let fs = files::Fs::connect();
    if let Some(c) = fs.client() {
        c.set_timeout(SERVICE_TIMEOUT_NS);
    }
    let mut desktop = desktop::Desktop::new();
    desktop.refresh(&model, &fs);
    let mut shell = Shell {
        display,
        vfs,
        fs,
        launcher,
        model,
        desktop_host,
        desktop,
        next_desktop_poll: 0,
        taskbar_host,
        taskbar: taskbar::Taskbar::new(),
        audio,
        popup: None,
        wifi_watch: None,
        firewall_watch: None,
        next_net_poll: 0,
        tooltip: None,
        notes: notify::Notifications::new(),
        note_counts: Vec::new(),
        listener,
        clients: Vec::new(),
        next_client: 0,
        ready: Vec::new(),
        caller: ClientIdentity::default(),
        loader: Loader::start(),
        loading: Vec::new(),
        next_ticket: 0,
        power: None,
        agent: None,
        agent_win: None,
        next_agent_try: 0,
        // Started with the system: the startup sound as the desktop appears.
        startup: rt::env::args().iter().any(|a| a == "startup").then(|| startup::StartupSound::new(rt::time::now_ns())),
        task_events,
        hidden_by_show_desktop: Vec::new(),
    };
    shell.refresh_windows();
    shell.refresh_audio();
    // Nothing comes with the desktop but the startup sound.
    if shell.startup.is_none() {
        shell.desktop_may_appear();
    }
    println!("desktop ready ({}x{}, {} apps)", screen.w, screen.h, shell.model.apps.len());
    shell.run()
}
