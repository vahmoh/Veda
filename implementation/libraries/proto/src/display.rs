// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The display protocol: how applications get windows from the compositor.
//!
//! A client creates a window, then attaches pixel buffers that live in a
//! shared VMO (premultiplied `0xAARRGGBB`, typically two buffers for double
//! buffering). It draws into a buffer and `present`s it with the damaged
//! rectangles; the compositor answers with a `FrameDone` event when it has
//! finished reading the previous buffer, which paces clients to the display.
//!
//! The compositor draws window decorations (title bar, frame, shadow) itself,
//! so a window stays movable when its client hangs. A client that does not
//! answer the user's request to close a window (asking the compositor for
//! nothing for a few seconds) is shown as not responding, and a second
//! request closes its connection, which takes its windows away.
//! Events for a window (input, resize requests, focus) arrive on a separate
//! per-window event channel. A client that falls behind reading them loses
//! nothing that carries state: pointer motion is merged, the latest size
//! and focus replace older ones, and a key or button pressed while it is far
//! behind is dropped together with its release.
//!
//! **Subsurfaces.** A window may show pictures of its own in parts of it
//! (a web page, a video) that are presented apart from its frames, so that
//! their frames reach the screen without the client drawing, copying or
//! presenting the window again. A process makes a subsurface for one of its
//! windows (`create_subsurface`, from any of its connections: the one that
//! presents the pictures may be a thread's own), attaches buffers to it,
//! each a memory object of its own (`attach_subsurface_buffer`), and
//! presents one at a time with what changed (`present_subsurface`): the
//! answer names the buffer shown before, which the compositor reads no more,
//! so that it can be drawn into again (or, while the GPU may still be
//! reading it, as the compositor samples buffers in place, a
//! `SubsurfaceEvent::Released` names it once it does not). A frame of the
//! window places its subsurfaces (`present`): where each shows, cut to its
//! rectangle, and what of the window's own buffer is drawn over them (its
//! overlays: a menu, a bar), blended with its alpha; elsewhere a subsurface
//! hides the window's buffer, which is not read there. A subsurface's
//! pictures are opaque, as an opaque window's are, and input goes to the
//! window as before. Its frames are paced as the window's are: a
//! `SubsurfaceEvent::FrameDone` on its own event channel once the picture
//! presented last is on the screen, and not while it cannot be seen (not
//! placed, or its window minimised).
//!
//! Who a client is decides what it may do: the desktop, panels and
//! notifications, managing other windows and hearing the shell's keys are
//! the shell's (the system service `shell`), listing the windows the
//! shell's and the agent's, the clipboard the focused window's process's.
//!
//! **The lock.** While the screen is locked, the window system draws only
//! the lock screen's windows (the system service `lock`'s), and only they
//! hear the keyboard and the pointer; its own shortcuts do nothing. It
//! locks at once when asked (by the shell, the agent or the lock service,
//! or with Super+L), and only the lock service unlocks it. A window
//! system started with the lock screen installed starts locked, after a
//! crash too: it is for the lock service to unlock it, at once if no
//! password is set. Until a lock screen has drawn itself, the window
//! system draws its own backdrop, so nothing is shown that the lock hides.
//!
//! **The idle lock.** The window system also locks the screen once no
//! input has come for the time the lock service sets (`set_idle_timeout`;
//! never until it does), unless a window keeps the screen awake: one whose
//! client asks it to (`keep_awake`, while a video plays, say) and that can
//! be seen. Being full screen is not enough, so that a film paused and left
//! full screen does not keep an unattended computer unlocked. The agent
//! alone may say that the user is there (`user_active`), which counts as
//! input: it does while the user talks with it in a conversation they asked
//! for, and never for what it overhears while it listens for its name, so
//! that talk near an unattended computer does not keep it unlocked.

use alloc::string::String;
use alloc::vec::Vec;
use ipc::{enumeration, message, protocol, union};
use rt::object::{Channel, Vmo};

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Rect {
        pub x: i32,
        pub y: i32,
        pub w: u32,
        pub h: u32,
    }
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
}

enumeration! {
    /// What kind of surface a window is (determines layering and decoration).
    ///
    /// Normal and desktop windows are opaque: the compositor copies their
    /// pixels and ignores alpha, and does not draw what they cover. The other
    /// kinds are blended (premultiplied alpha), so they may be translucent or
    /// have rounded corners.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WindowKind {
        /// An ordinary application window with a title bar.
        Normal = 1,
        /// An undecorated window (splash screens, games in borderless mode).
        Borderless = 2,
        /// The desktop background (shell only), below everything.
        Desktop = 3,
        /// A panel such as the taskbar (shell only), above normal windows.
        Panel = 4,
        /// A popup or menu, above everything. An application opens one only
        /// while one of its windows has the focus, for that window, and no
        /// larger than half the screen; it takes the focus, and as soon as
        /// the focus goes elsewhere the compositor hides it and asks its
        /// client to close it (`CloseRequested`).
        Popup = 5,
        /// A notification bubble (shell only), above windows, no focus.
        Notification = 6,
        /// The lock screen (the lock service only): the whole screen, above
        /// everything. While the screen is locked only lock screens are
        /// drawn, and only they hear the keyboard and the pointer.
        Lock = 7,
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WindowState {
        Normal = 1,
        Maximized = 2,
        Minimized = 3,
        Fullscreen = 4,
    }
}

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Cursor {
        Arrow = 1,
        Text = 2,
        Hand = 3,
        Move = 4,
        ResizeHorizontal = 5,
        ResizeVertical = 6,
        ResizeDiagonal = 7,
        ResizeAntiDiagonal = 8,
        Busy = 9,
        Crosshair = 10,
        Hidden = 11,
    }
}

/// The longest text the clipboard holds (bytes), so that the answer that
/// carries it fits in a message.
pub const CLIPBOARD_MAX: usize = abi::CHANNEL_MAX_BYTES - 64;

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DisplayError {
        NoSuchWindow = 1,
        BadBuffer = 2,
        Denied = 3,
        NoMemory = 4,
        Invalid = 5,
        /// No such subsurface of the caller's.
        NoSuchSubsurface = 6,
    }
}

/// The most subsurfaces one frame of a window places.
pub const PLACEMENTS_MAX: usize = 8;
/// The most rectangles one frame of a window draws over its subsurfaces
/// (its overlays).
pub const OVERLAYS_MAX: usize = 32;
/// The most rectangles of damage a frame (of a window or a subsurface)
/// lists: more are refused (`Invalid`), as one rectangle around them says
/// as much for less.
pub const DAMAGE_MAX: usize = 64;

message! {
    /// Where a frame of a window shows one of its subsurfaces.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Placement {
        pub subsurface: u32,
        /// Where, in the window's client area (its pixels): the subsurface's
        /// buffer is shown from this rectangle's top-left corner on, cut to
        /// the rectangle.
        pub rect: Rect,
        /// What shows of the rectangle where the subsurface's buffer does
        /// not reach (none presented yet, or a smaller one): `0xRRGGBB`.
        pub background: u32,
    }
}

message! {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WindowSpec {
        pub title: String,
        pub kind: WindowKind,
        /// Client-area size: at most the screen's (a popup's half of it);
        /// the first `Configure` says the size the window has.
        pub width: u32,
        pub height: u32,
        /// Requested position (`i32::MIN` = let the compositor choose);
        /// at most 2^20 from the origin.
        pub x: i32,
        pub y: i32,
        pub min_width: u32,
        pub min_height: u32,
        pub resizable: bool,
        /// Application id (matches the `.app` manifest; used for the taskbar).
        pub app_id: String,
    }
}

impl WindowSpec {
    pub fn new(title: &str, width: u32, height: u32) -> WindowSpec {
        WindowSpec {
            title: title.into(),
            kind: WindowKind::Normal,
            width,
            height,
            x: i32::MIN,
            y: i32::MIN,
            min_width: 160,
            min_height: 100,
            resizable: true,
            app_id: String::new(),
        }
    }
}

message! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ScreenInfo {
        pub width: u32,
        pub height: u32,
        /// Area not covered by panels (where windows are maximised).
        pub work_area: Rect,
    }
}

message! {
    /// A window as seen by the shell's taskbar (titles and application ids
    /// at most 256 bytes, cut at a character boundary).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WindowInfo {
        pub id: u32,
        pub title: String,
        pub app_id: String,
        pub state: WindowState,
        pub focused: bool,
        pub kind: WindowKind,
    }
}

/// Arrangements for [`display::Client::arrange_window`].
pub mod arrangement {
    pub const MAXIMIZE: u32 = 1;
    /// Back to the normal size and place (from maximised or snapped).
    pub const RESTORE: u32 = 2;
    pub const SNAP_LEFT: u32 = 3;
    pub const SNAP_RIGHT: u32 = 4;
}

/// Keyboard modifier bits in [`WindowEvent::Key`].
pub mod modifiers {
    pub const SHIFT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const ALT: u32 = 4;
    pub const SUPER: u32 = 8;
    pub const CAPS_LOCK: u32 = 16;
}

union! {
    /// Events delivered on a window's event channel.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum WindowEvent {
        /// The window should become `width` x `height` (attach new buffers),
        /// in `state`: sent as it is created, and at most once a frame as
        /// it changes (`Minimized` when it goes to the taskbar, its size
        /// unchanged).
        1 => Configure { width: u32, height: u32, state: WindowState },
        /// The user asked to close the window.
        2 => CloseRequested {},
        3 => Focus { focused: bool },
        /// Pointer moved inside the client area (window-relative pixels).
        4 => PointerMove { x: i32, y: i32 },
        /// `clicks` counts rapid successive presses (2 = double click).
        5 => PointerButton { x: i32, y: i32, button: u8, pressed: bool, clicks: u8 },
        6 => PointerLeave {},
        7 => Scroll { dx: i32, dy: i32 },
        /// A key event. `text` is the character produced (empty if none).
        8 => Key { code: u16, pressed: bool, repeat: bool, modifiers: u32, text: String },
        /// Buffer `shown` is now on screen: draw the next frame into another
        /// buffer and present it. The frame it is in reaches the screen at
        /// `seen_ns` (monotonic ns: the vertical blank it is flipped at,
        /// where a display driver flips and keeps time; when it was composed
        /// otherwise), and frames follow each other every `period_ns`, so a
        /// frame presented next is seen at `seen_ns + period_ns` at the
        /// earliest. A window that cannot be seen (minimised) hears it once
        /// it can be again, or when it is asked to close.
        9 => FrameDone { shown: u8, seen_ns: u64, period_ns: u64 },
        /// (Shell) the set of windows or their states changed.
        10 => WindowsChanged {},
        /// (Shell) the user pressed the Start/Super key.
        11 => StartMenuKey {},
        /// (Shell) the user pressed Super+Space: the agent.
        12 => AgentKey {},
        /// (Shell, to its desktop) the startup sequence dissolves into the
        /// desktop from the frame about to be shown: it appears.
        13 => Appearing {},
    }
}

/// Ordinal of window events on the event channel.
pub const EVENT: u32 = 1;

union! {
    /// Events on a subsurface's event channel (see
    /// [`display::Client::create_subsurface`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SubsurfaceEvent {
        /// The buffer presented last of `subsurface` is on the screen: the
        /// next may be presented. As [`WindowEvent::FrameDone`]: it is seen
        /// at `seen_ns`, frames following every `period_ns`; a subsurface
        /// that cannot be seen (not placed, or its window minimised) hears
        /// it once it can be again.
        1 => FrameDone { subsurface: u32, seen_ns: u64, period_ns: u64 },
        /// `buffer` of `subsurface`, which [`display::Client::present_subsurface`]
        /// replaced while the GPU might still read it, is read no more: it
        /// may be drawn into again. Whether or not the subsurface can be
        /// seen; not if the buffer was detached meanwhile.
        2 => Released { subsurface: u32, buffer: u32 },
    }
}

/// Ordinal of [`SubsurfaceEvent`]s on a subsurface's event channel.
pub const SUBSURFACE_EVENT: u32 = 1;

union! {
    /// Notices on a channel given to [`display::Client::watch_lock`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum LockNotice {
        /// The screen is locked, or not: the first notice says how it is
        /// as the channel is taken, each one after that a change.
        1 => Changed { locked: bool },
    }
}

/// Event ordinal of [`LockNotice`]s on a lock watch channel.
pub const LOCK_NOTICE: u32 = 1;

protocol! {
    /// The window system.
    pub mod display = "display" {
        /// Creates a window; returns its id and its event channel. `Denied`
        /// for the shell's kinds asked by another client, and for an
        /// application's popup while none of its windows has the focus;
        /// `NoMemory` past a process's 64 windows.
        1 => fn create_window(spec: WindowSpec) -> Result<(u32, Channel), DisplayError>;
        /// Attaches `count` (at most 3) buffers of `width` x `height`
        /// pixels (at most the screen's), laid out back to back in
        /// `buffers`, each row `stride` pixels from the next (at most 64
        /// more than `width`). They are shown from the next `present` on:
        /// until then the buffers attached before are. A process attaches
        /// a few screens' worth at most (`NoMemory`).
        2 => fn attach_buffers(id: u32, buffers: Vmo, width: u32, height: u32, stride: u32, count: u8) -> Result<(), DisplayError>;
        /// Shows buffer `index` (of the buffers attached last); `damage`
        /// lists the changed rectangles (empty = everything), at most
        /// [`DAMAGE_MAX`]. From this frame on the window shows its
        /// subsurfaces where `subsurfaces` places them (none: none, at most
        /// [`PLACEMENTS_MAX`], each once), and its buffer over them within
        /// `overlays` (at most [`OVERLAYS_MAX`]), blended; elsewhere within
        /// a subsurface's rectangle its buffer is not shown. `Invalid` for
        /// another window's subsurface, and for placements or overlays that
        /// overlap one another; one that is no more shows the placement's
        /// background.
        3 => fn present(id: u32, index: u8, damage: Vec<Rect>, subsurfaces: Vec<Placement>, overlays: Vec<Rect>) -> Result<(), DisplayError>;
        4 => fn set_title(id: u32, title: String) -> Result<(), DisplayError>;
        5 => fn set_state(id: u32, state: WindowState) -> Result<(), DisplayError>;
        6 => fn destroy_window(id: u32) -> Result<(), DisplayError>;
        7 => fn set_cursor(id: u32, cursor: Cursor) -> Result<(), DisplayError>;
        9 => fn screen_info() -> ScreenInfo;
        /// Puts text on the clipboard: for the process of the focused
        /// window, and the shell (`Denied` for others); at most
        /// [`CLIPBOARD_MAX`] bytes (`Invalid` for more).
        10 => fn set_clipboard(text: String) -> Result<(), DisplayError>;
        /// The clipboard's text: for the process of the focused window
        /// (`Denied` for others).
        11 => fn get_clipboard() -> Result<String, DisplayError>;
        /// Moves a window's frame to (`x`, `y`) (e.g. to place a popup,
        /// which stays on the screen); positions are at most 2^20 from the
        /// origin.
        12 => fn set_position(id: u32, x: i32, y: i32) -> Result<(), DisplayError>;
        /// (Shell and agent) the top-level windows, bottom to top: the
        /// topmost 100 at most. Other clients get none.
        20 => fn list_windows() -> Vec<WindowInfo>;
        /// Raises, restores and focuses a window. The shell may activate
        /// any window, other clients only their own.
        21 => fn activate_window(id: u32) -> Result<(), DisplayError>;
        /// (Shell) minimises a window.
        22 => fn minimize_window(id: u32) -> Result<(), DisplayError>;
        /// (Shell) closes a window as if its close button was pressed (its
        /// client's connection, if it did not answer the last request).
        23 => fn close_window(id: u32) -> Result<(), DisplayError>;
        /// Shell only: maximises, restores or snaps any window (see
        /// [`arrangement`]), bringing it to the front.
        24 => fn arrange_window(id: u32, arrangement: u32) -> Result<(), DisplayError>;
        /// (Shell) what comes with the desktop's first appearance is ready
        /// (the startup sound, or none): the startup sequence dissolves
        /// into the desktop once it has drawn itself and the splash has
        /// been seen long enough (telling it [`WindowEvent::Appearing`]).
        /// Without it, it waits a little, then dissolves all the same.
        25 => fn desktop_ready() -> Result<(), DisplayError>;
        /// Locks the screen at once (see the module's documentation): the
        /// shell, the agent and the lock service may (`Denied` for others).
        26 => fn lock() -> Result<(), DisplayError>;
        /// The lock service only: unlocks the screen (`Denied` for others).
        27 => fn unlock() -> Result<(), DisplayError>;
        /// Sends [`LockNotice`]s (ordinal [`LOCK_NOTICE`]) on `events`:
        /// whether the screen is locked, then every change, until it is
        /// closed. System services only (`Denied` for others); `NoMemory`
        /// past a few watchers.
        28 => fn watch_lock(events: Channel) -> Result<(), DisplayError>;
        /// The lock service only (`Denied` for others): locks the screen
        /// once no input has come for `seconds` (0: never), unless a window
        /// keeps it awake (`keep_awake`). The time counts from the last
        /// input, unlock or moment the screen stopped being kept awake. It
        /// holds until it is set again, and goes with the window system:
        /// the lock service sets it again as it reconnects.
        29 => fn set_idle_timeout(seconds: u32) -> Result<(), DisplayError>;
        /// While `awake`, window `id` keeps the screen from locking for
        /// want of input, as long as it can be seen: neither minimised nor
        /// hidden whole by a window above it. For an application while
        /// something plays (a video, a slideshow, a game), not merely while
        /// it is open. A client's own normal and borderless windows only
        /// (`NoSuchWindow` for another's, `Invalid` for other kinds).
        30 => fn keep_awake(id: u32, awake: bool) -> Result<(), DisplayError>;
        /// The agent only (`Denied` for others): the user is there, though
        /// no input came (they talk with it, hands-free), which counts as
        /// input for the idle lock. It does nothing while the screen is
        /// locked.
        31 => fn user_active() -> Result<(), DisplayError>;
        /// Makes a subsurface for window `window` (see the module's
        /// documentation), a window of the caller's process, whichever of
        /// its connections made it (`NoSuchWindow` for another's): this
        /// connection attaches its buffers and presents them, the window's
        /// frames place it. Its events go on `events` ([`SubsurfaceEvent`],
        /// ordinal [`SUBSURFACE_EVENT`]), which may be the same channel for
        /// several. It goes with the window, with this connection, or when
        /// destroyed. `NoMemory` past a process's limit.
        40 => fn create_subsurface(window: u32, events: Channel) -> Result<u32, DisplayError>;
        /// Attaches a buffer to `subsurface`, named `buffer` (not 0, nor the
        /// name of another of its buffers: `Invalid`): `width` x `height`
        /// pixels (at most the screen's) from the start of `memory`, each row
        /// `stride` pixels from the next (at most 64 more than `width`),
        /// premultiplied `0xAARRGGBB`, shown opaque. The compositor reads it
        /// only while it is the one shown. `BadBuffer` if it does not fit in
        /// `memory`; `NoMemory` past a few buffers, or a process's limit.
        41 => fn attach_subsurface_buffer(subsurface: u32, buffer: u32, memory: Vmo, width: u32, height: u32, stride: u32) -> Result<(), DisplayError>;
        /// The caller has done with `buffer` of `subsurface` (its memory is
        /// going): it is shown still, if it is the one shown, until another
        /// is presented.
        42 => fn detach_subsurface_buffer(subsurface: u32, buffer: u32) -> Result<(), DisplayError>;
        /// Shows `buffer` of `subsurface` from the next frame on, `damage`
        /// (its pixels, at most [`DAMAGE_MAX`] rectangles; empty =
        /// everything) having changed since the buffer presented before.
        /// Returns the buffer shown before, which the compositor reads no
        /// more (none if it was this one, if none was, or if it was
        /// detached); none either if the GPU may still be reading it, which a
        /// [`SubsurfaceEvent::Released`] then gives back, once it is not
        /// (within a frame). A [`SubsurfaceEvent::FrameDone`] says when the
        /// buffer presented is on the screen.
        43 => fn present_subsurface(subsurface: u32, buffer: u32, damage: Vec<Rect>) -> Result<Option<u32>, DisplayError>;
        /// Destroys `subsurface`: the window shows its frames' backgrounds
        /// where it was placed, until they place it no more.
        44 => fn destroy_subsurface(subsurface: u32) -> Result<(), DisplayError>;
    }
}
