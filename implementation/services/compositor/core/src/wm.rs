// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The window manager: the windows and their stacking order, focus, window
//! states and snapping, popups, the clients and what each may do, and the
//! events on their way to them.
//!
//! [`Wm`] is what the window system decides, apart from how it draws and
//! whom it talks to: the compositor turns its clients' requests and the
//! input drivers' events into calls of its methods, draws what it says
//! (adding to it what changed on the screen, its damage), and writes each
//! window the events it queues ([`Wm::deliver`]).
//!
//! **Popups** are the compositor's to keep in check. An application opens
//! one only while one of its windows has the focus, for that window (its
//! parent), and no larger than half the screen; the shell's (the start
//! menu, the calendar) have no parent. A popup takes the focus, and the
//! moment the focus goes anywhere but to it or to a popup of its own, the
//! compositor hides it and asks its client to close it: what a client
//! does with that request, the popup is gone from the screen already.
//!
//! **A client that does not answer** is found out when the user asks it to
//! close a window (its close button, Alt+F4, the taskbar): if it has asked
//! nothing of the compositor [`UNRESPONSIVE_NS`] later, the window's title
//! says it is not responding, and asking to close the window again closes
//! the client's connection and takes away its windows ([`Wm::take_dropped`]).
//! Ending its process is the launcher's (the task manager's), not the
//! window system's.
//!
//! **Sizes and states** reach a client as `Configure` events: one a frame
//! at most for each window ([`Wm::flush_configures`]), however often the
//! user's drag changes its size, and with `Minimized` when it goes to the
//! taskbar. A window that cannot be seen is not composed again for its
//! frames, and hears that they were shown (`FrameDone`) only once it can
//! be seen again, so that a minimised video stops at the frame it was at.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use vgfx::{Damage, Rect};
use vproto::display::{
    self as dp, Cursor, DisplayError, ScreenInfo, WindowEvent, WindowInfo, WindowKind, WindowSpec, WindowState,
};

use crate::buffers::{Buffers, Layout};
use crate::clients::{Client, Identity, Role};
use crate::input::Source;
use crate::keymap::Keyboard;
use crate::limits::{self, Limits};
use crate::outbox::Write;
use crate::switcher::Switcher;
use crate::window::{Anim, AnimKind, Part, Snap, TITLE_HEIGHT, Window};

/// How long a client may leave a request to close one of its windows
/// unanswered (asking the compositor nothing) before it counts as not
/// responding.
pub const UNRESPONSIVE_NS: u64 = 5_000_000_000;

/// How many windows `list_windows` lists at most (the topmost): as many as
/// fit in its answer, whatever their titles (see the tests).
pub const LIST_MAX: usize = 100;

/// How long windows take to open, close, go to the taskbar and come back.
const OPEN_NS: u64 = 180_000_000;
const CLOSE_NS: u64 = 130_000_000;
const MINIMIZE_NS: u64 = 170_000_000;
const RESTORE_NS: u64 = 160_000_000;

/// How deep popups may nest (a menu's submenus).
const POPUP_DEPTH: usize = 8;

/// The caption buttons the pointer is over or presses: how title bars are
/// drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecorState {
    pub hover: Option<(u32, Part)>,
    pub pressed: Option<(u32, Part)>,
}

/// A grab of the pointer, from a button's press to its release: an
/// interactive move or resize, a caption button held, or a client's
/// window, which hears of every button until all are released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    /// Moving by the title bar. `restore`: the window is maximised or
    /// snapped and returns to its normal size once the pointer really
    /// moves away from `origin`. `snap`: where it snaps if released now
    /// (and the preview's rectangle).
    Move { id: u32, button: u8, dx: i32, dy: i32, origin: (i32, i32), restore: bool, snap: Option<(Snap, Rect)> },
    /// Resizing by `edge` from `start`, the pointer pressed at (`px`, `py`).
    Resize { id: u32, button: u8, edge: (i8, i8), start: Rect, px: i32, py: i32 },
    /// A button held inside a client's window.
    Client { id: u32 },
    /// A caption button held (it acts if released over it).
    Button { id: u32, button: u8, part: Part },
}

impl Drag {
    /// The window grabbed.
    pub fn window(&self) -> u32 {
        match *self {
            Drag::Move { id, .. } | Drag::Resize { id, .. } | Drag::Client { id } | Drag::Button { id, .. } => id,
        }
    }
}

/// The last press: when, on which window, with which button, where.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Click {
    pub(crate) time: u64,
    pub(crate) window: u32,
    pub(crate) button: u8,
    pub(crate) at: (i32, i32),
}

/// The client rectangle of an automatically placed `w` x `h` window, with a
/// title bar `tb` high: centred in the work area `area`, its top-left corner
/// kept inside when the window is larger than the area.
fn auto_rect(area: Rect, w: i32, h: i32, tb: i32) -> Rect {
    let x = area.x + (area.w - w) / 2;
    let y = area.y + (area.h - h - tb) / 2;
    Rect::new(x.max(area.x), y.max(area.y) + tb, w, h)
}

/// The client rectangle of a window that fills `r` (title bar included).
fn below_title(r: Rect) -> Rect {
    Rect::new(r.x, r.y + TITLE_HEIGHT, r.w, r.h - TITLE_HEIGHT)
}

/// `r` moved onto `screen`, as far as it fits.
fn onto(screen: Rect, r: Rect) -> Rect {
    let x = r.x.min(screen.right() - r.w).max(screen.x);
    let y = r.y.min(screen.bottom() - r.h).max(screen.y);
    Rect::new(x, y, r.w, r.h)
}

/// The window manager (see the module's comment). `M` is the memory
/// clients' buffers are in.
pub struct Wm<M> {
    pub(crate) limits: Limits,
    pub(crate) work_area: Rect,
    pub(crate) windows: BTreeMap<u32, Window<M>>,
    /// Paint order, bottom to top: by layer (see [`Wm::layer`]), and within
    /// a layer most recently raised last.
    pub(crate) order: Vec<u32>,
    pub(crate) focused: Option<u32>,
    next_id: u32,
    pub(crate) damage: Damage,
    pub(crate) clients: BTreeMap<u64, Client>,
    pub(crate) keyboard: Keyboard,
    pub(crate) pointer: (i32, i32),
    pub(crate) cursor: Cursor,
    pub(crate) buttons: u8,
    pub(crate) drag: Option<Drag>,
    pub(crate) decor: DecorState,
    pub(crate) hover_window: Option<u32>,
    /// What the pointer was over when that was last worked out: windows
    /// open, close, change their state and come to the front under a
    /// pointer that stays put, and then it is worked out again.
    pub(crate) hover_hit: Option<(u32, Part)>,
    pub(crate) last_click: Click,
    pub(crate) click_count: u8,
    clipboard: String,
    /// The Alt+Tab switcher while Alt is held, and how many were opened.
    pub(crate) switcher: Option<Switcher>,
    pub(crate) switchers: u64,
    /// Windows minimised by Super+D, restored by the next Super+D.
    pub(crate) desktop_shown: Vec<u32>,
    /// What each input driver's connection holds down.
    pub(crate) sources: BTreeMap<u64, Source>,
    /// Windows with events to write, and those whose channel was full.
    queued: BTreeSet<u32>,
    blocked: BTreeSet<u32>,
    /// Windows whose clients are to hear their size and state.
    configure_due: BTreeSet<u32>,
    /// Windows that animate, whose clients owe an answer to a request to
    /// close them, and whose frames are to be acknowledged: what each
    /// frame and each wake-up look at, rather than every window.
    animated: BTreeSet<u32>,
    asked: BTreeSet<u32>,
    owed: BTreeSet<u32>,
    /// Windows gone, and connections the compositor closes, since the
    /// compositor last took them.
    destroyed: Vec<u32>,
    dropped: Vec<u64>,
    /// Windows whose clients did not answer a request to close them.
    unresponsive: BTreeSet<u32>,
    /// When the last frame is seen, and how often frames follow.
    frame: (u64, u64),
    /// The time of what is being done (ns): animations start then.
    pub(crate) now: u64,
}

impl<M> Wm<M> {
    /// A window manager for `screen`, without windows.
    pub fn new(screen: Rect) -> Wm<M> {
        Wm {
            limits: Limits::new(screen),
            work_area: screen,
            windows: BTreeMap::new(),
            order: Vec::new(),
            focused: None,
            next_id: 0,
            damage: Damage::new(),
            clients: BTreeMap::new(),
            keyboard: Keyboard::default(),
            pointer: (screen.x + screen.w / 2, screen.y + screen.h / 2),
            cursor: Cursor::Arrow,
            buttons: 0,
            drag: None,
            decor: DecorState::default(),
            hover_window: None,
            hover_hit: None,
            last_click: Click::default(),
            click_count: 0,
            clipboard: String::new(),
            switcher: None,
            switchers: 0,
            desktop_shown: Vec::new(),
            sources: BTreeMap::new(),
            queued: BTreeSet::new(),
            blocked: BTreeSet::new(),
            configure_due: BTreeSet::new(),
            animated: BTreeSet::new(),
            asked: BTreeSet::new(),
            owed: BTreeSet::new(),
            destroyed: Vec::new(),
            dropped: Vec::new(),
            unresponsive: BTreeSet::new(),
            frame: (0, 0),
            now: 0,
        }
    }

    // ---- what the compositor reads -------------------------------------------

    /// The screen.
    pub fn screen(&self) -> Rect {
        self.limits.screen
    }

    /// The screen without the panels: where windows are maximised.
    pub fn work_area(&self) -> Rect {
        self.work_area
    }

    /// Window `id`.
    pub fn window(&self, id: u32) -> Option<&Window<M>> {
        self.windows.get(&id)
    }

    /// Window `id`, to change how it is drawn (not where).
    pub fn window_mut(&mut self, id: u32) -> Option<&mut Window<M>> {
        self.windows.get_mut(&id)
    }

    /// The windows, by id.
    pub fn windows(&self) -> impl Iterator<Item = &Window<M>> {
        self.windows.values()
    }

    /// The windows, by id, to change how they are drawn (not where).
    pub fn windows_mut(&mut self) -> impl Iterator<Item = &mut Window<M>> {
        self.windows.values_mut()
    }

    /// The windows in paint order, bottom to top.
    pub fn paint_order(&self) -> &[u32] {
        &self.order
    }

    /// The window that has the keyboard.
    pub fn focused(&self) -> Option<u32> {
        self.focused
    }

    /// Where the pointer is.
    pub fn pointer(&self) -> (i32, i32) {
        self.pointer
    }

    /// The cursor the pointer shows.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// The caption buttons hovered or pressed.
    pub fn decor(&self) -> DecorState {
        self.decor
    }

    /// The window being moved, and where it snaps if released now.
    pub fn snap_preview(&self) -> Option<(u32, Rect)> {
        match self.drag {
            Some(Drag::Move { id, snap: Some((_, r)), .. }) => Some((id, r)),
            _ => None,
        }
    }

    /// The Alt+Tab switcher, while it shows.
    pub fn switcher(&self) -> Option<&Switcher> {
        self.switcher.as_ref()
    }

    /// The client on connection `key`.
    pub fn client(&self, key: u64) -> Option<&Client> {
        self.clients.get(&key)
    }

    /// Whether a window's client did not answer a request to close it.
    pub fn unresponsive(&self, id: u32) -> bool {
        self.unresponsive.contains(&id)
    }

    /// Marks `r` of the screen to be composed again.
    pub fn add_damage(&mut self, r: Rect) {
        self.damage.add(r);
    }

    /// What is to be composed again, taken.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        self.damage.take()
    }

    /// Whether nothing is to be composed again.
    pub fn undamaged(&self) -> bool {
        self.damage.is_empty()
    }

    /// Windows gone since the last call: their channels and textures go.
    pub fn take_destroyed(&mut self) -> Vec<u32> {
        core::mem::take(&mut self.destroyed)
    }

    /// Connections the compositor closes (their clients did not answer),
    /// since the last call; their windows are gone already.
    pub fn take_dropped(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.dropped)
    }

    // ---- clients -------------------------------------------------------------

    /// A client connected on `key`: `Denied` if its process, or every
    /// application together, has as many connections open as may be (the
    /// system's services have only their own limit, so that applications
    /// cannot leave the shell without).
    pub fn connect(&mut self, key: u64, identity: Identity, now: u64) -> Result<(), DisplayError> {
        let own = self.clients.values().filter(|c| c.identity.koid == identity.koid).count();
        let apps = self.clients.values().filter(|c| !c.identity.service).count();
        if (!identity.service && apps >= limits::CONNECTIONS_MAX) || own >= limits::CONNECTIONS_PER_PROCESS {
            return Err(DisplayError::Denied);
        }
        self.clients.insert(key, Client::new(identity, now));
        Ok(())
    }

    /// The client on `key` went away: its windows go at once.
    pub fn disconnect(&mut self, key: u64) {
        if self.clients.remove(&key).is_none() {
            return;
        }
        let ids: Vec<u32> = self.windows.values().filter(|w| w.client == key).map(|w| w.id).collect();
        for id in ids {
            self.destroy_window(id);
        }
    }

    /// The client on `key` asks for something at `now`: it is alive.
    pub fn request(&mut self, key: u64, now: u64) {
        let Some(c) = self.clients.get_mut(&key) else { return };
        c.last_request = now;
        let windows = &self.windows;
        self.asked.retain(|id| windows.get(id).is_some_and(|w| w.client != key));
        let revived: Vec<u32> = self
            .unresponsive
            .iter()
            .copied()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.client == key))
            .collect();
        for id in revived {
            self.unresponsive.remove(&id);
            self.damage_title(id);
        }
    }

    fn role(&self, key: u64) -> Role {
        self.clients.get(&key).map_or(Role::App, |c| c.role)
    }

    fn is_shell(&self, key: u64) -> bool {
        self.role(key) == Role::Shell
    }

    /// Window `id`, if it is `client`'s.
    fn own(&self, client: u64, id: u32) -> Result<&Window<M>, DisplayError> {
        self.windows.get(&id).filter(|w| w.client == client).ok_or(DisplayError::NoSuchWindow)
    }

    /// The windows of `koid`'s process.
    fn process_windows(&self, koid: u64) -> impl Iterator<Item = &Window<M>> {
        self.windows.values().filter(move |w| self.clients.get(&w.client).is_some_and(|c| c.identity.koid == koid))
    }

    // ---- the display protocol ------------------------------------------------

    /// `client` creates a window (see `display::create_window`).
    pub fn create_window(&mut self, client: u64, spec: WindowSpec, now: u64) -> Result<u32, DisplayError> {
        self.now = now;
        let caller = self.clients.get(&client).ok_or(DisplayError::Denied)?;
        let (role, koid) = (caller.role, caller.identity.koid);
        let kind = spec.kind;
        // The desktop, panels and notifications are the shell's.
        if matches!(kind, WindowKind::Desktop | WindowKind::Panel | WindowKind::Notification) && role != Role::Shell {
            return Err(DisplayError::Denied);
        }
        // An application's popup is for its window that has the focus.
        let parent = match kind {
            WindowKind::Popup if role != Role::Shell => Some(
                self.focused
                    .filter(|f| self.windows.get(f).is_some_and(|w| w.client == client && w.interactive()))
                    .ok_or(DisplayError::Denied)?,
            ),
            _ => None,
        };
        if parent.is_some_and(|p| self.popup_depth(p) >= POPUP_DEPTH) {
            return Err(DisplayError::Denied);
        }
        // Applications together have only so many windows; the system's
        // services (the shell) only their own limit.
        let service = self.clients.get(&client).is_some_and(|c| c.identity.service);
        let apps = self.windows.values().filter(|w| self.clients.get(&w.client).is_some_and(|c| !c.identity.service));
        if (!service && apps.count() >= limits::WINDOWS_MAX)
            || self.process_windows(koid).count() >= limits::WINDOWS_PER_PROCESS
        {
            return Err(DisplayError::NoMemory);
        }
        let position = match (spec.x, spec.y) {
            (i32::MIN, _) | (_, i32::MIN) => None,
            (x, y) => Some((limits::coordinate(x)?, limits::coordinate(y)?)),
        };
        let max = if parent.is_some() { self.limits.popup() } else { self.limits.window() };
        let (w, h) = Limits::size(spec.width, spec.height, max);
        let id = self.allocate_id();
        let (rect, auto_placed) = self.place(kind, w, h, position);
        let mut win = Window::new(id, client, kind, rect);
        win.title = limits::truncated(spec.title, limits::TEXT_MAX);
        win.app_id = limits::truncated(spec.app_id, limits::TEXT_MAX);
        let least = |v: u32, floor: i32, max: i32| (v.min(i32::MAX as u32) as i32).max(floor).min(max);
        let largest = self.limits.window();
        win.min_w = least(spec.min_width, limits::MIN_WIDTH, largest.0);
        win.min_h = least(spec.min_height, limits::MIN_HEIGHT, largest.1);
        win.resizable = spec.resizable && kind == WindowKind::Normal;
        win.parent = parent;
        win.auto_placed = auto_placed;
        self.windows.insert(id, win);
        self.order.push(id);
        if matches!(kind, WindowKind::Normal | WindowKind::Popup) {
            self.animate(id, AnimKind::Open, OPEN_NS);
        }
        // The client may know its size already; it is told before it can
        // draw its first frame all the same.
        self.tell(id);
        if matches!(kind, WindowKind::Normal | WindowKind::Borderless | WindowKind::Popup) {
            self.raise(id);
            self.focus(Some(id));
        } else {
            self.restack();
        }
        if kind == WindowKind::Panel {
            self.update_work_area();
        }
        self.damage_window(id);
        self.notify_shell();
        Ok(id)
    }

    /// A window id that is neither 0 nor in use (nor just given up).
    fn allocate_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1);
            let id = self.next_id;
            if id != 0 && !self.windows.contains_key(&id) && !self.destroyed.contains(&id) {
                return id;
            }
        }
    }

    /// How many popups lead to popup `id` (0 for a window that is not one).
    fn popup_depth(&self, id: u32) -> usize {
        let mut depth = 0;
        let mut at = id;
        while let Some(p) = self.windows.get(&at).and_then(|w| w.parent) {
            depth += 1;
            at = p;
            if depth > POPUP_DEPTH {
                break;
            }
        }
        depth
    }

    /// Where a new `w` x `h` window of `kind` goes: at `at` (its frame's
    /// top-left corner), or in the middle of the work area. Its client
    /// rectangle, and whether it was placed automatically.
    fn place(&self, kind: WindowKind, w: i32, h: i32, at: Option<(i32, i32)>) -> (Rect, bool) {
        let tb = if kind == WindowKind::Normal { TITLE_HEIGHT } else { 0 };
        match (kind, at) {
            (WindowKind::Desktop, _) => (self.screen(), false),
            (WindowKind::Popup, Some((x, y))) => (onto(self.screen(), Rect::new(x, y, w, h)), false),
            (_, Some((x, y))) => (Rect::new(x, y + tb, w, h), false),
            (_, None) => (auto_rect(self.work_area, w, h, tb), true),
        }
    }

    /// Checks buffers `client` wants to attach to window `id`: their layout
    /// (see [`Layout::new`]) in `memory` bytes, and its process's limit.
    pub fn buffer_layout(
        &self,
        client: u64,
        id: u32,
        size: (u32, u32),
        stride: u32,
        count: u8,
        memory: usize,
    ) -> Result<Layout, DisplayError> {
        let w = self.own(client, id)?;
        let layout = Layout::new(size.0, size.1, stride, count, self.limits.window(), memory)?;
        let koid = self.clients.get(&client).map_or(0, |c| c.identity.koid);
        // What it has, but the buffers these replace before they were shown.
        let used: usize = self.process_windows(koid).map(|w| w.surface.bytes()).sum();
        let used = used.saturating_sub(w.surface.pending_bytes());
        if used.saturating_add(layout.bytes()) > self.limits.buffer_bytes_per_process() {
            return Err(DisplayError::NoMemory);
        }
        Ok(layout)
    }

    /// `client` attaches buffers (checked by [`Wm::buffer_layout`]) to
    /// window `id`: shown from its next present on.
    pub fn attach(&mut self, client: u64, id: u32, buffers: Buffers<M>) -> Result<(), DisplayError> {
        self.own(client, id)?;
        if let Some(w) = self.windows.get_mut(&id) {
            w.surface.attach(buffers);
        }
        Ok(())
    }

    /// `client` presents buffer `index` of window `id`, `damage` (the
    /// buffer's pixels) having changed, or everything if it is empty.
    pub fn present(&mut self, client: u64, id: u32, index: u8, damage: Vec<dp::Rect>) -> Result<(), DisplayError> {
        self.own(client, id)?;
        let rects = damage.into_iter().map(limits::rect).collect::<Result<Vec<Rect>, DisplayError>>()?;
        let Some(w) = self.windows.get_mut(&id) else { return Err(DisplayError::NoSuchWindow) };
        let p = w.surface.present(index, &rects)?;
        self.owed.insert(id);
        // A window that cannot be seen is not composed again; its frame is
        // acknowledged once it can be.
        if !w.visible() {
            return Ok(());
        }
        let c = w.client_rect;
        if p.first || w.anim.is_some() {
            // It appears, or is animating: drawn again with its frame.
            let r = w.paint_bounds();
            self.damage.add(r);
        } else if p.new_buffers || rects.is_empty() {
            self.damage.add(c);
        } else {
            for d in rects {
                self.damage.add(d.translate(c.x, c.y).intersect(&c));
            }
        }
        Ok(())
    }

    /// `client` gives window `id` a title (cut to [`limits::TEXT_MAX`]).
    pub fn set_title(&mut self, client: u64, id: u32, title: String) -> Result<(), DisplayError> {
        self.own(client, id)?;
        if let Some(w) = self.windows.get_mut(&id) {
            w.title = limits::truncated(title, limits::TEXT_MAX);
        }
        self.damage_title(id);
        self.notify_shell();
        Ok(())
    }

    /// `client` asks for window `id` to be `state` (normal and borderless
    /// windows have states; popups and the shell's surfaces do not).
    pub fn set_window_state(&mut self, client: u64, id: u32, state: WindowState, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        let w = self.own(client, id)?;
        if !matches!(w.kind, WindowKind::Normal | WindowKind::Borderless) {
            return Err(DisplayError::Invalid);
        }
        if !w.closing {
            self.set_state(id, state);
        }
        Ok(())
    }

    /// `client` destroys window `id`: a normal window fades out first.
    pub fn destroy(&mut self, client: u64, id: u32, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        let w = self.own(client, id)?;
        if w.closing {
            return Ok(());
        }
        if w.kind == WindowKind::Normal && w.interactive() {
            let refocus = self.in_focus_chain(id);
            if let Some(w) = self.windows.get_mut(&id) {
                w.closing = true;
            }
            self.animate(id, AnimKind::Close, CLOSE_NS);
            if self.drag.is_some_and(|d| d.window() == id) {
                self.end_drag();
            }
            if refocus {
                self.focus_top();
            }
            self.notify_shell();
        } else {
            self.destroy_window(id);
        }
        Ok(())
    }

    /// `client` sets the cursor shown over window `id`.
    pub fn set_cursor(&mut self, client: u64, id: u32, cursor: Cursor) -> Result<(), DisplayError> {
        self.own(client, id)?;
        if let Some(w) = self.windows.get_mut(&id) {
            w.cursor = cursor;
        }
        if self.hover_window == Some(id) {
            // Its new cursor, nothing having moved.
            self.update_hover(false);
        }
        Ok(())
    }

    /// `client` moves window `id`'s frame to (`x`, `y`): a popup no
    /// further than the screen keeps it; a maximised or full-screen window
    /// where it returns to.
    pub fn set_position(&mut self, client: u64, id: u32, x: i32, y: i32) -> Result<(), DisplayError> {
        self.own(client, id)?;
        let (x, y) = (limits::coordinate(x)?, limits::coordinate(y)?);
        self.damage_window(id);
        let screen = self.screen();
        let Some(w) = self.windows.get_mut(&id) else { return Err(DisplayError::NoSuchWindow) };
        let state = if w.state == WindowState::Minimized { w.unminimized } else { w.state };
        let tb = if w.kind == WindowKind::Normal { TITLE_HEIGHT } else { 0 };
        if w.kind == WindowKind::Popup {
            w.client_rect = onto(screen, Rect::new(x, y, w.client_rect.w, w.client_rect.h));
        } else if state == WindowState::Normal {
            w.client_rect = Rect::new(x, y + tb, w.client_rect.w, w.client_rect.h);
            w.snapped = None;
        } else {
            w.restore_rect = Rect::new(x, y + tb, w.restore_rect.w, w.restore_rect.h);
        }
        w.auto_placed = false;
        let kind = w.kind;
        self.damage_window(id);
        if kind == WindowKind::Panel {
            self.update_work_area();
        }
        Ok(())
    }

    /// The screen and the work area.
    pub fn screen_info(&self) -> ScreenInfo {
        let (s, a) = (self.screen(), self.work_area);
        ScreenInfo {
            width: s.w as u32,
            height: s.h as u32,
            work_area: dp::Rect::new(a.x, a.y, a.w.max(0) as u32, a.h.max(0) as u32),
        }
    }

    /// Whether `client`'s process has the focused window.
    fn has_focus(&self, client: u64) -> bool {
        let Some(caller) = self.clients.get(&client) else { return false };
        self.focused
            .and_then(|f| self.windows.get(&f))
            .and_then(|w| self.clients.get(&w.client))
            .is_some_and(|owner| owner.identity.same_process(&caller.identity))
    }

    /// `client` puts `text` on the clipboard: the focused window's process
    /// may, and the shell.
    pub fn set_clipboard(&mut self, client: u64, text: String) -> Result<(), DisplayError> {
        if text.len() > limits::CLIPBOARD_MAX {
            return Err(DisplayError::Invalid);
        }
        if !self.is_shell(client) && !self.has_focus(client) {
            return Err(DisplayError::Denied);
        }
        self.clipboard = text;
        Ok(())
    }

    /// The clipboard's text, for the focused window's process alone.
    pub fn clipboard(&self, client: u64) -> Result<String, DisplayError> {
        if !self.has_focus(client) {
            return Err(DisplayError::Denied);
        }
        Ok(self.clipboard.clone())
    }

    /// The top-level windows (the topmost [`LIST_MAX`]), bottom to top, for
    /// the shell's taskbar and the agent; nothing for anyone else.
    pub fn list_windows(&self, client: u64) -> Vec<WindowInfo> {
        if !matches!(self.role(client), Role::Shell | Role::Agent) {
            return Vec::new();
        }
        let root = self.focus_root();
        let listed: Vec<&Window<M>> = self
            .order
            .iter()
            .filter_map(|id| self.windows.get(id))
            .filter(|w| w.kind == WindowKind::Normal && !w.closing)
            .collect();
        let skip = listed.len().saturating_sub(LIST_MAX);
        listed[skip..]
            .iter()
            .map(|w| WindowInfo {
                id: w.id,
                title: w.title.clone(),
                app_id: w.app_id.clone(),
                state: w.state,
                focused: root == Some(w.id),
                kind: w.kind,
            })
            .collect()
    }

    /// Raises, restores and focuses window `id`: the shell's request for any
    /// window, another client's for its own.
    pub fn activate(&mut self, client: u64, id: u32, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        let w = self.windows.get(&id).ok_or(DisplayError::NoSuchWindow)?;
        if w.client != client && !self.is_shell(client) {
            return Err(DisplayError::NoSuchWindow);
        }
        if !matches!(w.kind, WindowKind::Normal | WindowKind::Borderless) {
            return Err(DisplayError::Invalid);
        }
        if w.closing {
            return Ok(());
        }
        if w.state == WindowState::Minimized {
            self.unminimize(id);
        }
        self.raise(id);
        self.focus(Some(id));
        Ok(())
    }

    /// The window `id` of the shell's request, if it is one the shell may
    /// manage: a normal or borderless window, not closing.
    fn managed(&self, client: u64, id: u32) -> Result<WindowState, DisplayError> {
        if !self.is_shell(client) {
            return Err(DisplayError::Denied);
        }
        let w = self.windows.get(&id).ok_or(DisplayError::NoSuchWindow)?;
        if !matches!(w.kind, WindowKind::Normal | WindowKind::Borderless) || w.closing {
            return Err(DisplayError::Invalid);
        }
        Ok(w.state)
    }

    /// (Shell) minimises window `id`.
    pub fn minimize(&mut self, client: u64, id: u32, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        self.managed(client, id)?;
        self.set_state(id, WindowState::Minimized);
        Ok(())
    }

    /// (Shell) maximises, restores or snaps window `id`
    /// (`display::arrangement`), bringing it to the front.
    pub fn arrange(&mut self, client: u64, id: u32, arrangement: u32, now: u64) -> Result<(), DisplayError> {
        use vproto::display::arrangement as a;
        use vproto::input::keys;
        self.now = now;
        let state = self.managed(client, id)?;
        let key = match arrangement {
            a::MAXIMIZE => keys::UP,
            a::RESTORE => keys::DOWN,
            a::SNAP_LEFT => keys::LEFT,
            a::SNAP_RIGHT => keys::RIGHT,
            _ => return Err(DisplayError::Invalid),
        };
        if state == WindowState::Minimized {
            self.unminimize(id);
        }
        self.raise(id);
        self.focus(Some(id));
        // Restoring a minimised window is bringing it back.
        if key != keys::DOWN || state != WindowState::Minimized {
            self.arrange_focused(key);
        }
        Ok(())
    }

    /// (Shell) closes window `id` as its close button would.
    pub fn close(&mut self, client: u64, id: u32, now: u64) -> Result<(), DisplayError> {
        self.now = now;
        if !self.is_shell(client) {
            return Err(DisplayError::Denied);
        }
        if !self.windows.contains_key(&id) {
            return Err(DisplayError::NoSuchWindow);
        }
        self.request_close(id);
        Ok(())
    }

    /// (Shell) whether `client` may say the desktop is ready.
    pub fn may_ready_desktop(&self, client: u64) -> Result<(), DisplayError> {
        if self.is_shell(client) { Ok(()) } else { Err(DisplayError::Denied) }
    }

    // ---- closing, and clients that do not answer ------------------------------

    /// The user asks to close window `id`: its client is asked, or, if it
    /// did not answer the last time it was, its connection is closed.
    pub(crate) fn request_close(&mut self, id: u32) {
        let Some(w) = self.windows.get(&id) else { return };
        let client = w.client;
        if self.unresponsive.contains(&id) {
            self.force_close(client);
            return;
        }
        // The first request of those unanswered counts.
        if self.asked.insert(id)
            && let Some(w) = self.windows.get_mut(&id)
        {
            w.close_asked = Some(self.now);
        }
        self.send(id, WindowEvent::CloseRequested {});
        // A window not shown holds its frame's acknowledgement: given now,
        // so that its client can draw its answer.
        self.release_frame(id);
    }

    /// Closes the connection of `client`, which does not answer: its
    /// windows go, and the compositor closes the channel
    /// ([`Wm::take_dropped`]).
    fn force_close(&mut self, client: u64) {
        self.dropped.push(client);
        self.disconnect(client);
    }

    /// When a client that was asked to close a window counts as not
    /// responding unless it asks for something first.
    pub fn liveness_deadline(&self) -> Option<u64> {
        let asked = self.asked.iter().filter_map(|id| self.windows.get(id)?.close_asked);
        asked.map(|t| t.saturating_add(UNRESPONSIVE_NS)).min()
    }

    /// Finds the windows whose clients did not answer a request to close
    /// them in time: their titles say so.
    pub fn check_liveness(&mut self, now: u64) {
        let late: Vec<u32> = self
            .asked
            .iter()
            .copied()
            .filter(|id| {
                let asked = self.windows.get(id).and_then(|w| w.close_asked);
                asked.is_some_and(|t| now >= t.saturating_add(UNRESPONSIVE_NS))
            })
            .collect();
        for id in late {
            self.asked.remove(&id);
            if self.windows.get(&id).is_some_and(|w| !w.closing) {
                self.unresponsive.insert(id);
                self.damage_title(id);
            }
        }
    }

    // ---- windows going away -----------------------------------------------------

    /// Removes window `id` at once.
    pub(crate) fn destroy_window(&mut self, id: u32) {
        let Some(w) = self.windows.get(&id) else { return };
        let (kind, parent) = (w.kind, w.parent);
        let refocus = self.in_focus_chain(id);
        self.damage_window(id);
        self.dismiss_children(id);
        if self.drag.is_some_and(|d| d.window() == id) {
            self.end_drag();
        }
        self.windows.remove(&id);
        self.order.retain(|&x| x != id);
        self.queued.remove(&id);
        self.blocked.remove(&id);
        self.configure_due.remove(&id);
        self.unresponsive.remove(&id);
        self.animated.remove(&id);
        self.asked.remove(&id);
        self.owed.remove(&id);
        self.desktop_shown.retain(|&x| x != id);
        self.destroyed.push(id);
        if self.hover_window == Some(id) {
            self.hover_window = None;
        }
        if self.hover_hit.is_some_and(|(h, _)| h == id) {
            self.hover_hit = None;
        }
        for state in [&mut self.decor.hover, &mut self.decor.pressed] {
            if state.is_some_and(|(h, _)| h == id) {
                *state = None;
            }
        }
        if refocus {
            // Back to the window a popup belonged to, or the topmost.
            self.focused = None;
            match parent.filter(|p| self.windows.get(p).is_some_and(|w| w.interactive())) {
                Some(p) => self.focus(Some(p)),
                None => self.focus_top(),
            }
        }
        if kind == WindowKind::Panel {
            self.update_work_area();
        }
        self.notify_shell();
    }

    /// Hides the popups opened for window `id` (and theirs).
    fn dismiss_children(&mut self, id: u32) {
        let children: Vec<u32> = self.windows.values().filter(|w| w.parent == Some(id)).map(|w| w.id).collect();
        for child in children {
            self.dismiss(child);
        }
    }

    /// Hides popup `id` (and the popups opened for it), and asks its client
    /// to close it.
    fn dismiss(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.dismissed {
            return;
        }
        w.dismissed = true;
        let r = w.paint_bounds();
        self.damage.add(r.inflate(2));
        if self.drag.is_some_and(|d| d.window() == id) {
            self.end_drag();
        }
        self.send(id, WindowEvent::CloseRequested {});
        self.dismiss_children(id);
    }

    /// Ends the grab of the pointer, erasing its snap preview and the
    /// pressed caption button; the grab it was.
    pub(crate) fn end_drag(&mut self) -> Option<Drag> {
        let drag = self.drag.take();
        match drag {
            Some(Drag::Move { snap: Some((_, r)), .. }) => self.damage.add(r.inflate(8)),
            Some(Drag::Button { id, .. }) => {
                self.decor.pressed = None;
                self.damage_title(id);
            }
            _ => {}
        }
        drag
    }

    // ---- focus and stacking ---------------------------------------------------------

    /// The stacking layer of window `w` (higher layers are drawn on top),
    /// the focus being on `root`'s (itself, or a popup of its). A
    /// full-screen window that has the focus covers the panels; when
    /// another window takes the focus it drops back among the others.
    fn layer(w: &Window<M>, root: Option<u32>) -> u8 {
        match w.kind {
            WindowKind::Desktop => 0,
            WindowKind::Normal | WindowKind::Borderless if root == Some(w.id) && w.state == WindowState::Fullscreen => {
                3
            }
            WindowKind::Normal | WindowKind::Borderless => 1,
            WindowKind::Panel => 2,
            WindowKind::Popup => 4,
            WindowKind::Notification => 5,
        }
    }

    /// Puts the windows back in order of their layers (keeping the order
    /// within each).
    pub(crate) fn restack(&mut self) {
        let root = self.focus_root();
        let windows = &self.windows;
        self.order.sort_by_key(|id| windows.get(id).map_or(0, |w| Self::layer(w, root)));
    }

    /// Brings window `id` to the top of its layer.
    pub(crate) fn raise(&mut self, id: u32) {
        self.order.retain(|&x| x != id);
        self.order.push(id);
        self.restack();
        self.damage_window(id);
    }

    /// The window the focus belongs to: the focused window, or the window
    /// the focused popup (and those it was opened from) belongs to.
    pub(crate) fn focus_root(&self) -> Option<u32> {
        let mut at = self.focused?;
        for _ in 0..=POPUP_DEPTH {
            match self.windows.get(&at).and_then(|w| w.parent) {
                Some(p) => at = p,
                None => break,
            }
        }
        Some(at)
    }

    /// Whether window `id` is the focused window, or one its focused popup
    /// belongs to.
    pub(crate) fn in_focus_chain(&self, id: u32) -> bool {
        let mut at = self.focused;
        for _ in 0..=POPUP_DEPTH {
            match at {
                Some(a) if a == id => return true,
                Some(a) => at = self.windows.get(&a).and_then(|w| w.parent),
                None => return false,
            }
        }
        false
    }

    /// Gives the keyboard to window `id` (or to none). Popups the focus is
    /// not in (or below) are hidden.
    pub(crate) fn focus(&mut self, id: Option<u32>) {
        if self.focused == id {
            return;
        }
        let old_root = self.focus_root();
        let old = core::mem::replace(&mut self.focused, id);
        if let Some(old) = old {
            self.send(old, WindowEvent::Focus { focused: false });
            self.damage_window(old);
        }
        self.keyboard.reset_repeat();
        if let Some(new) = id {
            self.send(new, WindowEvent::Focus { focused: true });
            self.damage_window(new);
        }
        let outside: Vec<u32> = self
            .windows
            .values()
            .filter(|w| w.kind == WindowKind::Popup && !w.dismissed)
            .map(|w| w.id)
            .filter(|p| !self.in_focus_chain(*p))
            .collect();
        for p in outside {
            self.dismiss(p);
        }
        if self.focus_root() != old_root {
            // A full-screen window gains or loses its layer.
            self.restack();
        }
        self.notify_shell();
    }

    /// Focuses the topmost normal window that can be seen.
    pub(crate) fn focus_top(&mut self) {
        let top = self
            .order
            .iter()
            .rev()
            .copied()
            .find(|id| self.windows.get(id).is_some_and(|w| w.kind == WindowKind::Normal && w.interactive()));
        self.focus(top);
    }

    /// The pointer pressed outside the popups: they close, the focus going
    /// back to the window they belong to (or the topmost).
    pub(crate) fn close_popups(&mut self) {
        let in_popup = self.focused.and_then(|f| self.windows.get(&f)).is_some_and(|w| w.kind == WindowKind::Popup);
        if !in_popup {
            return;
        }
        match self.focus_root().filter(|r| self.windows.get(r).is_some_and(|w| w.kind != WindowKind::Popup)) {
            Some(root) => self.focus(Some(root)),
            None => self.focus_top(),
        }
    }

    // ---- states, snapping and the work area ------------------------------------------

    /// Changes window `id`'s state as the user or its client asks. A
    /// minimised window comes back in the state it had first.
    pub(crate) fn set_state(&mut self, id: u32, state: WindowState) {
        let Some(w) = self.windows.get(&id) else { return };
        if w.state == state {
            return;
        }
        let before = w.paint_bounds();
        if w.state == WindowState::Minimized {
            if let Some(w) = self.windows.get_mut(&id) {
                w.state = w.unminimized;
            }
            self.animate(id, AnimKind::Restore, RESTORE_NS);
        }
        if state == WindowState::Minimized {
            if let Some(w) = self.windows.get_mut(&id) {
                w.unminimized = w.state;
                w.state = WindowState::Minimized;
            }
            self.animate(id, AnimKind::Minimize, MINIMIZE_NS);
        } else {
            self.change_state(id, state);
        }
        let after = self.windows.get(&id).map_or(before, |w| w.paint_bounds());
        self.damage.add(before);
        self.damage.add(after);
        self.configure(id);
        if state == WindowState::Minimized {
            if self.drag.is_some_and(|d| d.window() == id) {
                self.end_drag();
            }
            if self.in_focus_chain(id) {
                self.focus_top();
            }
            self.dismiss_children(id);
            self.restack();
        } else {
            self.raise(id);
            self.focus(Some(id));
        }
        self.notify_shell();
    }

    /// Brings minimised window `id` back in the state it had.
    pub(crate) fn unminimize(&mut self, id: u32) {
        if let Some(state) = self.windows.get(&id).filter(|w| w.state == WindowState::Minimized).map(|w| w.unminimized)
        {
            self.set_state(id, state);
        }
    }

    /// Moves window `id` (not minimised) between normal, maximised and
    /// full screen.
    fn change_state(&mut self, id: u32, state: WindowState) {
        let (area, screen) = (self.work_area, self.screen());
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.state == state {
            return;
        }
        let old = w.state;
        match state {
            WindowState::Maximized => {
                // A snapped window keeps the size it had before snapping.
                if old == WindowState::Normal && w.snapped.is_none() {
                    w.restore_rect = w.client_rect;
                }
                w.snapped = None;
                w.client_rect = below_title(area);
            }
            WindowState::Fullscreen => {
                if old == WindowState::Normal && w.snapped.is_none() {
                    w.restore_rect = w.client_rect;
                }
                w.client_rect = screen;
            }
            WindowState::Normal => {
                w.client_rect = match w.snapped {
                    Some(snap) => below_title(snap.rect(area)),
                    None => w.restore_rect,
                };
            }
            WindowState::Minimized => return,
        }
        w.state = state;
    }

    /// A maximised or snapped window starts being dragged: it returns to its
    /// normal size, keeping the grabbed point of the title bar under the
    /// pointer.
    pub(crate) fn unsnap_for_drag(&mut self, id: u32, button: u8, origin: (i32, i32), (x, y): (i32, i32)) {
        let Some(w) = self.windows.get(&id) else { return };
        let frac = (origin.0 - w.client_rect.x) as f32 / w.client_rect.w.max(1) as f32;
        let rest = w.restore_rect;
        if w.state == WindowState::Maximized {
            self.set_state(id, WindowState::Normal);
        }
        let Some(w) = self.windows.get_mut(&id) else { return };
        let before = w.paint_bounds();
        w.snapped = None;
        w.auto_placed = false;
        w.client_rect = Rect::new(x - (rest.w as f32 * frac) as i32, y + TITLE_HEIGHT / 2, rest.w, rest.h);
        let c = w.client_rect;
        let after = w.paint_bounds();
        self.damage.add(before);
        self.damage.add(after);
        self.configure(id);
        self.drag = Some(Drag::Move { id, button, dx: x - c.x, dy: y - c.y, origin, restore: false, snap: None });
    }

    /// Snaps window `id` into `r` (the whole work area or one half of it).
    pub(crate) fn apply_snap(&mut self, id: u32, snap: Snap, r: Rect) {
        if snap == Snap::Maximize {
            self.set_state(id, WindowState::Maximized);
            return;
        }
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.state != WindowState::Normal {
            return;
        }
        if w.snapped.is_none() {
            w.restore_rect = w.client_rect;
        }
        w.snapped = Some(snap);
        w.auto_placed = false;
        let before = w.paint_bounds();
        w.client_rect = below_title(r);
        let after = w.paint_bounds();
        self.damage.add(before);
        self.damage.add(after);
        self.configure(id);
    }

    /// Recomputes the work area (the screen minus the panels) and fits the
    /// windows to it if it changed.
    pub(crate) fn update_work_area(&mut self) {
        let s = self.screen();
        let old = self.work_area;
        let mut area = s;
        for w in self.windows.values() {
            if w.kind == WindowKind::Panel && w.state != WindowState::Minimized && !w.dismissed {
                let f = w.frame();
                if f.bottom() >= s.bottom() - 2 && f.w > s.w / 2 {
                    area.h = area.h.min(f.y - area.y);
                } else if f.y <= s.y + 2 && f.w > s.w / 2 {
                    let cut = (f.bottom() - area.y).clamp(0, area.h);
                    area.y += cut;
                    area.h -= cut;
                }
            }
        }
        area.h = area.h.max(TITLE_HEIGHT + 1);
        self.work_area = area;
        if area != old {
            self.fit_to_work_area();
        }
    }

    /// After the work area changed (a panel appeared or went away):
    /// maximised and snapped windows take their new space, and windows that
    /// were placed automatically and never moved are placed again — so
    /// where a window opens does not depend on whether the taskbar existed
    /// yet.
    fn fit_to_work_area(&mut self) {
        let area = self.work_area;
        let mut resized = Vec::new();
        for w in self.windows.values_mut() {
            if !matches!(w.kind, WindowKind::Normal | WindowKind::Borderless) || w.closing {
                continue;
            }
            let tb = if w.kind == WindowKind::Normal { TITLE_HEIGHT } else { 0 };
            if w.auto_placed {
                w.restore_rect = auto_rect(area, w.restore_rect.w, w.restore_rect.h, tb);
            }
            // A minimised window comes back to its new place.
            let state = if w.state == WindowState::Minimized { w.unminimized } else { w.state };
            let target = match (state, w.snapped) {
                (WindowState::Maximized, _) => below_title(area),
                (WindowState::Normal, Some(snap)) => below_title(snap.rect(area)),
                (WindowState::Normal, None) if w.auto_placed => w.restore_rect,
                _ => continue,
            };
            if target == w.client_rect {
                continue;
            }
            let before = w.paint_bounds();
            w.client_rect = target;
            self.damage.add(before);
            self.damage.add(w.paint_bounds());
            resized.push(w.id);
        }
        for id in resized {
            self.configure(id);
        }
    }

    // ---- what clients hear -----------------------------------------------------------

    /// Queues `ev` for window `id`'s client.
    pub(crate) fn send(&mut self, id: u32, ev: WindowEvent) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        w.outbox.push(ev);
        if !w.outbox.is_empty() && !self.blocked.contains(&id) {
            self.queued.insert(id);
        }
    }

    /// Writes the events queued since the last call, `write` putting each
    /// on its window's channel. A window whose channel is full keeps the
    /// rest until [`Wm::writable`].
    pub fn deliver(&mut self, mut write: impl FnMut(u32, &WindowEvent) -> Write) {
        for id in core::mem::take(&mut self.queued) {
            let Some(w) = self.windows.get_mut(&id) else { continue };
            if !w.outbox.flush(|ev| write(id, ev)) {
                self.blocked.insert(id);
            }
        }
    }

    /// Window `id`'s channel has room again: what waits for it is written.
    pub fn writable(&mut self, id: u32, mut write: impl FnMut(u32, &WindowEvent) -> Write) {
        if !self.blocked.remove(&id) {
            return;
        }
        if let Some(w) = self.windows.get_mut(&id)
            && !w.outbox.flush(|ev| write(id, ev))
        {
            self.blocked.insert(id);
        }
    }

    /// The windows whose channels are full, events waiting.
    pub fn blocked(&self) -> impl Iterator<Item = u32> + '_ {
        self.blocked.iter().copied()
    }

    /// Window `id`'s client is to hear its size and state with the next
    /// frame.
    pub(crate) fn configure(&mut self, id: u32) {
        self.configure_due.insert(id);
    }

    /// Tells window `id`'s client its size and state, if they changed.
    fn tell(&mut self, id: u32) {
        self.configure_due.remove(&id);
        let Some(w) = self.windows.get_mut(&id) else { return };
        let now = (w.client_rect.w, w.client_rect.h, w.state);
        if w.told == Some(now) {
            return;
        }
        w.told = Some(now);
        let (width, height, state) = (now.0.max(1) as u32, now.1.max(1) as u32, now.2);
        self.send(id, WindowEvent::Configure { width, height, state });
    }

    /// Whether clients are to hear of sizes and states.
    pub fn configures_due(&self) -> bool {
        !self.configure_due.is_empty()
    }

    /// Tells clients of their windows' sizes and states: once a frame.
    pub fn flush_configures(&mut self) {
        for id in core::mem::take(&mut self.configure_due) {
            self.tell(id);
        }
    }

    /// A frame was composed, to be seen at `seen` with frames every
    /// `period`: the windows that can be seen hear that their frames were
    /// shown (the others once they can be seen again).
    pub fn frames_done(&mut self, seen: u64, period: u64) {
        self.frame = (seen, period);
        let shown: Vec<u32> =
            self.owed.iter().copied().filter(|id| self.windows.get(id).is_some_and(|w| w.visible())).collect();
        for id in shown {
            self.release_frame(id);
        }
    }

    /// Acknowledges window `id`'s last frame now, shown or not.
    fn release_frame(&mut self, id: u32) {
        let (seen, period) = self.frame;
        self.owed.remove(&id);
        if let Some(shown) = self.windows.get_mut(&id).and_then(|w| w.surface.frame_owed.take()) {
            self.send(id, WindowEvent::FrameDone { shown, seen_ns: seen, period_ns: period });
        }
    }

    /// Tells the shell the windows changed.
    pub(crate) fn notify_shell(&mut self) {
        let panels: Vec<u32> = self
            .windows
            .values()
            .filter(|w| w.kind == WindowKind::Panel && self.is_shell(w.client))
            .map(|w| w.id)
            .collect();
        for id in panels {
            self.send(id, WindowEvent::WindowsChanged {});
        }
    }

    /// Sends `ev` to the shell (its first panel, the taskbar).
    pub(crate) fn send_to_shell(&mut self, ev: WindowEvent) {
        let taskbar = self.windows.values().find(|w| w.kind == WindowKind::Panel && self.is_shell(w.client));
        if let Some(id) = taskbar.map(|w| w.id) {
            self.send(id, ev);
        }
    }

    /// Sends `ev` to the shell's desktop.
    pub fn send_to_desktop(&mut self, ev: WindowEvent) {
        if let Some(id) = self.windows.values().find(|w| w.kind == WindowKind::Desktop).map(|w| w.id) {
            self.send(id, ev);
        }
    }

    // ---- frames ------------------------------------------------------------------------

    /// Moves the animations on to `now`: windows that finished closing go.
    pub fn advance(&mut self, now: u64) {
        let mut closed = Vec::new();
        for id in core::mem::take(&mut self.animated) {
            let Some(w) = self.windows.get_mut(&id) else { continue };
            let Some(a) = w.anim else { continue };
            self.damage.add(w.paint_bounds().inflate(4));
            if !a.done(now) {
                self.animated.insert(id);
            } else {
                w.anim = None;
                if a.kind == AnimKind::Close {
                    closed.push(id);
                }
            }
        }
        for id in closed {
            self.destroy_window(id);
        }
    }

    /// Whether a window animates.
    pub fn animating(&self) -> bool {
        !self.animated.is_empty()
    }

    /// Starts window `id`'s animation of `kind`, `duration` long, now.
    fn animate(&mut self, id: u32, kind: AnimKind, duration: u64) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.anim = Some(Anim { kind, start: self.now, duration });
            self.animated.insert(id);
        }
    }

    /// Whether the desktop has drawn itself: the shell's desktop window and
    /// its panels (the taskbar) have each presented a frame.
    pub fn desktop_drawn(&self) -> bool {
        let Some(desktop) = self.windows.values().find(|w| w.kind == WindowKind::Desktop) else { return false };
        desktop.surface.size().is_some()
            && self
                .windows
                .values()
                .filter(|w| w.kind == WindowKind::Panel && w.client == desktop.client)
                .all(|w| w.surface.size().is_some())
    }

    /// Marks window `id` (with its shadow) to be composed again.
    pub(crate) fn damage_window(&mut self, id: u32) {
        if let Some(w) = self.windows.get(&id) {
            let r = w.paint_bounds();
            self.damage.add(r.inflate(2));
        }
    }

    /// Marks window `id`'s title bar to be composed again.
    pub(crate) fn damage_title(&mut self, id: u32) {
        if let Some(w) = self.windows.get(&id)
            && w.decorated()
        {
            let r = w.title_rect();
            self.damage.add(r);
        }
    }
}
