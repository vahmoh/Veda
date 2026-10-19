// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The window manager as its clients and the user meet it: a shell, an
//! agent and applications, driving it through its requests and input.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use gfx::Rect;
use proto::display::{
    self as dp, DisplayError, SubsurfaceEvent, WindowEvent, WindowInfo, WindowKind, WindowSpec, WindowState,
};
use proto::input::{InputEvent, keys};

use crate::buffers::Buffers;
use crate::clients::Identity;
use crate::limits;
use crate::outbox::Write;
use crate::subsurfaces::BUFFERS_MAX;
use crate::window::{Part, TITLE_HEIGHT};
use crate::wm::{LIST_MAX, UNRESPONSIVE_NS, Wm};

const SCREEN: Rect = Rect::new(0, 0, 1280, 800);
const SHELL: u64 = 1;
const AGENT: u64 = 2;
const APP: u64 = 3;
const OTHER: u64 = 4;
/// A second connection of `APP`'s process.
const APP_TOO: u64 = 5;
/// An application named like the shell.
const IMPOSTOR: u64 = 6;
/// The lock service, and an application named like it.
const LOCK: u64 = 7;
const LOCK_IMPOSTOR: u64 = 8;
/// The input driver's connection.
const MOUSE: u64 = 100;

fn identity(name: &str, service: bool, koid: u64) -> Identity {
    Identity { koid, name: name.into(), service }
}

/// A window manager with the shell, the agent, the lock service and three
/// applications connected (two connections of the same process).
fn wm() -> Wm<()> {
    let mut wm = Wm::new(SCREEN);
    for (key, id) in [
        (SHELL, identity("shell", true, 10)),
        (AGENT, identity("agent", true, 11)),
        (APP, identity("files", false, 12)),
        (OTHER, identity("music", false, 13)),
        (APP_TOO, identity("files", false, 12)),
        (IMPOSTOR, identity("shell", false, 14)),
        (LOCK, identity("lock", true, 15)),
        (LOCK_IMPOSTOR, identity("lock", false, 16)),
    ] {
        wm.connect(key, id, 0).unwrap();
    }
    wm
}

fn spec(kind: WindowKind, width: u32, height: u32) -> WindowSpec {
    let mut s = WindowSpec::new("window", width, height);
    s.kind = kind;
    s
}

fn spec_at(kind: WindowKind, r: Rect) -> WindowSpec {
    let mut s = spec(kind, r.w as u32, r.h as u32);
    (s.x, s.y) = (r.x, r.y);
    s
}

/// The events delivered since the last call, by window.
fn sent(wm: &mut Wm<()>) -> Vec<(u32, WindowEvent)> {
    let mut out = Vec::new();
    wm.deliver(|id, ev| {
        out.push((id, ev.clone()));
        Write::Sent
    });
    out
}

/// The events window `id` was sent since the last call.
fn sent_to(wm: &mut Wm<()>, id: u32) -> Vec<WindowEvent> {
    sent(wm).into_iter().filter(|(to, _)| *to == id).map(|(_, ev)| ev).collect()
}

/// Gives window `id` a frame of `w` x `h` (two buffers).
fn draw(wm: &mut Wm<()>, client: u64, id: u32, w: u32, h: u32) {
    let layout = wm.buffer_layout(client, id, (w, h), w, 2, usize::MAX).unwrap();
    wm.attach(client, id, Buffers { layout, memory: () }).unwrap();
    wm.present(client, id, 0, Vec::new(), Vec::new(), Vec::new()).unwrap();
}

/// A desktop with the shell's desktop and taskbar, and an application
/// window: its id.
fn desktop(wm: &mut Wm<()>) -> u32 {
    wm.create_window(SHELL, spec(WindowKind::Desktop, 1280, 800), 0).unwrap();
    wm.create_window(SHELL, spec_at(WindowKind::Panel, Rect::new(0, 752, 1280, 48)), 0).unwrap();
    let id = wm.create_window(APP, spec(WindowKind::Normal, 600, 400), 0).unwrap();
    sent(wm);
    id
}

fn configure(width: i32, height: i32, state: WindowState) -> WindowEvent {
    WindowEvent::Configure { width: width as u32, height: height as u32, state }
}

// ---- who may do what (shell privileges, the clipboard, the window list) ----

#[test]
fn shell_surfaces_are_the_shells_alone() {
    let mut wm = wm();
    for kind in [WindowKind::Desktop, WindowKind::Panel, WindowKind::Notification] {
        assert_eq!(wm.create_window(APP, spec(kind, 100, 100), 0), Err(DisplayError::Denied));
        assert_eq!(wm.create_window(IMPOSTOR, spec(kind, 100, 100), 0), Err(DisplayError::Denied));
        assert!(wm.create_window(SHELL, spec(kind, 100, 100), 0).is_ok());
    }
}

#[test]
fn only_the_shell_manages_other_windows() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    for client in [OTHER, IMPOSTOR, AGENT] {
        assert_eq!(wm.minimize(client, id, 0), Err(DisplayError::Denied));
        assert_eq!(wm.close(client, id, 0), Err(DisplayError::Denied));
        assert_eq!(wm.arrange(client, id, dp::arrangement::MAXIMIZE, 0), Err(DisplayError::Denied));
        assert_eq!(wm.activate(client, id, 0), Err(DisplayError::NoSuchWindow));
        assert_eq!(wm.may_ready_desktop(client), Err(DisplayError::Denied));
    }
    // The shell may, and a client its own window.
    assert_eq!(wm.activate(APP, id, 0), Ok(()));
    assert_eq!(wm.minimize(SHELL, id, 0), Ok(()));
    assert_eq!(wm.may_ready_desktop(SHELL), Ok(()));
}

#[test]
fn the_window_list_is_the_shells_and_the_agents() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let listed = |wm: &Wm<()>, client| wm.list_windows(client).iter().map(|w| w.id).collect::<Vec<_>>();
    assert_eq!(listed(&wm, SHELL), [id]);
    assert_eq!(listed(&wm, AGENT), [id]);
    assert!(listed(&wm, OTHER).is_empty() && listed(&wm, IMPOSTOR).is_empty());
}

#[test]
fn the_clipboard_is_the_focused_processs() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    assert_eq!(wm.focused(), Some(id));
    assert_eq!(wm.set_clipboard(APP, "copied".into()), Ok(()));
    // Another connection of the same process (Files puts paths there so).
    assert_eq!(wm.clipboard(APP_TOO), Ok(String::from("copied")));
    // Other applications neither read nor overwrite it.
    assert_eq!(wm.clipboard(OTHER), Err(DisplayError::Denied));
    assert_eq!(wm.set_clipboard(OTHER, "mine".into()), Err(DisplayError::Denied));
    // The shell may set it (a path copied in the start menu, say).
    assert_eq!(wm.set_clipboard(SHELL, "shell".into()), Ok(()));
    assert_eq!(wm.clipboard(SHELL), Err(DisplayError::Denied));
    // What a message cannot carry back is refused.
    assert_eq!(wm.set_clipboard(APP, "x".repeat(limits::CLIPBOARD_MAX + 1)), Err(DisplayError::Invalid));
}

#[test]
fn titles_are_cut_and_the_window_list_fits_in_a_message() {
    let mut wm = wm();
    let long = "ü".repeat(10_000);
    let mut s = spec(WindowKind::Normal, 100, 100);
    s.app_id = long.clone();
    let first = wm.create_window(APP, s, 0).unwrap();
    wm.set_title(APP, first, long.clone()).unwrap();
    let w = wm.window(first).unwrap();
    assert_eq!((w.title.len(), w.app_id.len()), (limits::TEXT_MAX, limits::TEXT_MAX));
    // As many windows as may be, each with the longest title and id.
    for (i, client) in (0..300).map(|i| (i, if i % 2 == 0 { APP } else { OTHER })) {
        if let Ok(id) = wm.create_window(client, spec(WindowKind::Normal, 100, 100), i) {
            wm.set_title(client, id, long.clone()).unwrap();
        }
    }
    let list = wm.list_windows(SHELL);
    assert_eq!(list.len(), LIST_MAX);
    let mut e = ipc::Encoder::with_header(20, 1, ipc::FLAG_RESPONSE);
    let widest: Vec<WindowInfo> =
        list.into_iter().map(|w| WindowInfo { app_id: String::from(&long[..limits::TEXT_MAX]), ..w }).collect();
    ipc::Encode::encode(widest, &mut e);
    assert!(e.bytes.len() <= abi::CHANNEL_MAX_BYTES);
}

// ---- limits ----------------------------------------------------------------------

#[test]
fn a_process_has_only_so_many_windows() {
    let mut wm = wm();
    for i in 0..limits::WINDOWS_PER_PROCESS {
        // Both connections of the process count.
        let client = if i % 2 == 0 { APP } else { APP_TOO };
        wm.create_window(client, spec(WindowKind::Normal, 100, 100), 0).unwrap();
    }
    assert_eq!(wm.create_window(APP, spec(WindowKind::Normal, 100, 100), 0), Err(DisplayError::NoMemory));
    assert!(wm.create_window(OTHER, spec(WindowKind::Normal, 100, 100), 0).is_ok());
}

#[test]
fn windows_are_no_larger_than_the_screen() {
    let mut wm = wm();
    let id = wm.create_window(APP, spec(WindowKind::Normal, 8192, 8192), 0).unwrap();
    assert_eq!(sent_to(&mut wm, id).first(), Some(&configure(1280, 800, WindowState::Normal)));
    assert_eq!(wm.buffer_layout(APP, id, (8192, 8192), 8192, 2, usize::MAX), Err(DisplayError::BadBuffer));
    assert!(wm.buffer_layout(APP, id, (1280, 800), 1280, 2, usize::MAX).is_ok());
}

#[test]
fn a_process_attaches_only_so_much_memory() {
    let mut wm = wm();
    let screens = limits::BUFFER_SCREENS_PER_PROCESS;
    let mut ids = Vec::new();
    for _ in 0..screens / 2 {
        let id = wm.create_window(APP, spec(WindowKind::Normal, 1280, 800), 0).unwrap();
        draw(&mut wm, APP, id, 1280, 800);
        ids.push(id);
    }
    let one_more = wm.create_window(APP, spec(WindowKind::Normal, 1280, 800), 0).unwrap();
    assert_eq!(wm.buffer_layout(APP, one_more, (1280, 800), 1280, 2, usize::MAX), Err(DisplayError::NoMemory));
    // Buffers that replace others not presented yet take their place.
    wm.destroy(APP, ids[0], 0).unwrap();
    wm.advance(1_000_000_000);
    let layout = wm.buffer_layout(APP, one_more, (1280, 800), 1280, 2, usize::MAX).unwrap();
    wm.attach(APP, one_more, Buffers { layout, memory: () }).unwrap();
    assert!(wm.buffer_layout(APP, one_more, (1280, 800), 1280, 2, usize::MAX).is_ok());
    let last = wm.create_window(APP, spec(WindowKind::Normal, 1280, 800), 0).unwrap();
    assert_eq!(wm.buffer_layout(APP, last, (640, 400), 640, 1, usize::MAX), Err(DisplayError::NoMemory));
}

#[test]
fn positions_are_checked_where_they_come_in() {
    let mut wm = wm();
    let big = limits::COORD_MAX + 1;
    let mut s = spec(WindowKind::Normal, 100, 100);
    (s.x, s.y) = (0, i32::MAX);
    assert_eq!(wm.create_window(APP, s, 0), Err(DisplayError::Invalid));
    let id = wm.create_window(APP, spec(WindowKind::Normal, 100, 100), 0).unwrap();
    assert_eq!(wm.set_position(APP, id, big, 0), Err(DisplayError::Invalid));
    assert_eq!(wm.set_position(APP, id, -limits::COORD_MAX, limits::COORD_MAX), Ok(()));
    draw(&mut wm, APP, id, 100, 100);
    let damage = vec![dp::Rect::new(0, 0, u32::MAX, 5)];
    assert_eq!(wm.present(APP, id, 1, damage, Vec::new(), Vec::new()), Err(DisplayError::Invalid));
    assert_eq!(wm.present(APP, id, 1, vec![dp::Rect::new(-50, -50, 70, 70)], Vec::new(), Vec::new()), Ok(()));
}

#[test]
fn window_ids_in_use_are_never_given_again() {
    let mut wm = wm();
    let first = wm.create_window(APP, spec(WindowKind::Borderless, 10, 10), 0).unwrap();
    let mut last = first;
    for _ in 0..10 {
        let id = wm.create_window(APP, spec(WindowKind::Borderless, 10, 10), 0).unwrap();
        wm.destroy(APP, id, 0).unwrap();
        assert!(id != first && id != last);
        last = id;
    }
    assert!(wm.take_destroyed().len() == 10 && wm.window(first).is_some());
}

// ---- popups ------------------------------------------------------------------------

#[test]
fn an_application_opens_popups_only_for_its_focused_window() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    // Not focused: no popup.
    assert_eq!(wm.create_window(OTHER, spec(WindowKind::Popup, 100, 100), 0), Err(DisplayError::Denied));
    let popup = wm.create_window(APP, spec_at(WindowKind::Popup, Rect::new(1200, 700, 2000, 2000)), 0).unwrap();
    let p = wm.window(popup).unwrap();
    // Half the screen at most, and on the screen.
    assert_eq!(p.client_rect, Rect::new(640, 400, 640, 400));
    assert_eq!((p.parent, wm.focused()), (Some(id), Some(popup)));
    // The shell's popups have no parent and no such limit.
    wm.activate(SHELL, id, 0).unwrap();
    let start = wm.create_window(SHELL, spec(WindowKind::Popup, 1000, 700), 0).unwrap();
    assert_eq!(wm.window(start).map(|w| (w.parent, w.client_rect.w)), Some((None, 1000)));
}

#[test]
fn popups_are_hidden_as_soon_as_the_focus_leaves_them() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let menu = wm.create_window(APP, spec(WindowKind::Popup, 200, 300), 0).unwrap();
    let sub = wm.create_window(APP, spec(WindowKind::Popup, 200, 300), 0).unwrap();
    // A submenu keeps its menu.
    assert!(!wm.window(menu).unwrap().dismissed);
    sent(&mut wm);
    // A click on the application's window: both go, whatever the client does.
    let (cx, cy) = (wm.window(id).unwrap().client_rect.x + 5, wm.window(id).unwrap().client_rect.y + 5);
    wm.pointer_moved(cx, cy);
    wm.pointer_button(0, true, 1);
    wm.pointer_button(0, false, 2);
    for p in [menu, sub] {
        let w = wm.window(p).unwrap();
        assert!(w.dismissed && !w.visible());
    }
    let events = sent(&mut wm);
    assert!(events.contains(&(menu, WindowEvent::CloseRequested {})));
    assert!(events.contains(&(sub, WindowEvent::CloseRequested {})));
    assert_eq!(wm.focused(), Some(id));
    // A hidden popup is gone for the pointer, and cannot be activated.
    assert_eq!(wm.window_at(cx, cy).map(|(w, _)| w), Some(id));
    assert!(wm.activate(APP, menu, 3).is_err());
}

#[test]
fn pressing_a_panel_closes_the_shells_popup() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let start = wm.create_window(SHELL, spec_at(WindowKind::Popup, Rect::new(0, 300, 400, 400)), 0).unwrap();
    assert_eq!(wm.focused(), Some(start));
    wm.pointer_moved(800, 770);
    wm.pointer_button(0, true, 1);
    assert!(wm.window(start).unwrap().dismissed);
    assert_eq!(wm.focused(), Some(id));
}

// ---- clients that do not answer -------------------------------------------------------

#[test]
fn a_client_that_does_not_answer_is_closed_by_a_second_request() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.close(SHELL, id, 1_000).unwrap();
    assert_eq!(sent_to(&mut wm, id), [WindowEvent::CloseRequested {}]);
    assert_eq!(wm.liveness_deadline(), Some(1_000 + UNRESPONSIVE_NS));
    wm.check_liveness(1_000 + UNRESPONSIVE_NS - 1);
    assert!(!wm.unresponsive(id));
    wm.check_liveness(1_000 + UNRESPONSIVE_NS);
    assert!(wm.unresponsive(id));
    // Asked again: its connection goes, and its windows with it.
    wm.close(SHELL, id, 2 * UNRESPONSIVE_NS).unwrap();
    assert_eq!(wm.take_dropped(), [APP]);
    assert!(wm.window(id).is_none() && wm.client(APP).is_none());
}

#[test]
fn a_client_that_answers_is_never_forced() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.close(SHELL, id, 1_000).unwrap();
    // It asks something (draws "save your changes?").
    wm.request(APP, 2_000);
    wm.check_liveness(1_000 + UNRESPONSIVE_NS);
    assert!(!wm.unresponsive(id) && wm.liveness_deadline().is_none());
    wm.close(SHELL, id, 2 * UNRESPONSIVE_NS).unwrap();
    assert!(wm.take_dropped().is_empty() && wm.window(id).is_some());
}

#[test]
fn a_minimised_window_asked_to_close_may_draw_its_answer() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    draw(&mut wm, APP, id, 600, 400);
    wm.minimize(SHELL, id, 0).unwrap();
    wm.advance(10_000_000_000);
    wm.present(APP, id, 1, Vec::new(), Vec::new(), Vec::new()).unwrap();
    wm.frames_done(1, 2);
    assert!(!sent_to(&mut wm, id).iter().any(|e| matches!(e, WindowEvent::FrameDone { .. })));
    wm.close(SHELL, id, 3).unwrap();
    let events = sent_to(&mut wm, id);
    assert!(events.contains(&WindowEvent::CloseRequested {}));
    assert!(events.contains(&WindowEvent::FrameDone { shown: 1, seen_ns: 1, period_ns: 2 }));
}

// ---- states --------------------------------------------------------------------------------

#[test]
fn a_maximised_window_comes_back_maximised() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.set_window_state(APP, id, WindowState::Maximized, 0).unwrap();
    wm.flush_configures();
    let area = wm.work_area();
    let maximized = (area.w, area.h - TITLE_HEIGHT);
    assert_eq!(sent_to(&mut wm, id).last(), Some(&configure(maximized.0, maximized.1, WindowState::Maximized)));
    // To the taskbar: its client hears it is minimised.
    wm.minimize(SHELL, id, 0).unwrap();
    wm.flush_configures();
    assert_eq!(sent_to(&mut wm, id).last(), Some(&configure(maximized.0, maximized.1, WindowState::Minimized)));
    // And back from it (the taskbar activates it): maximised again.
    wm.activate(SHELL, id, 1).unwrap();
    wm.flush_configures();
    assert_eq!(wm.window(id).unwrap().state, WindowState::Maximized);
    assert_eq!(sent_to(&mut wm, id).last(), Some(&configure(maximized.0, maximized.1, WindowState::Maximized)));
    // Super+D and back, full screen.
    wm.set_window_state(APP, id, WindowState::Fullscreen, 2).unwrap();
    wm.key(keys::LEFTMETA, true, 3);
    wm.key(keys::D, true, 3);
    wm.key(keys::D, false, 3);
    wm.key(keys::D, true, 4);
    assert_eq!(wm.window(id).map(|w| (w.state, w.client_rect)), Some((WindowState::Fullscreen, SCREEN)));
}

#[test]
fn a_minimised_window_is_not_composed_and_waits_for_its_frames() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    draw(&mut wm, APP, id, 600, 400);
    wm.frames_done(10, 16);
    assert!(sent_to(&mut wm, id).contains(&WindowEvent::FrameDone { shown: 0, seen_ns: 10, period_ns: 16 }));
    wm.minimize(SHELL, id, 0).unwrap();
    wm.advance(1_000_000_000);
    wm.take_damage();
    wm.present(APP, id, 1, Vec::new(), Vec::new(), Vec::new()).unwrap();
    assert!(wm.undamaged());
    wm.frames_done(20, 16);
    assert!(!sent_to(&mut wm, id).iter().any(|e| matches!(e, WindowEvent::FrameDone { .. })));
    // Back on the screen: its frame is acknowledged with the next.
    wm.activate(SHELL, id, 2_000_000_000).unwrap();
    wm.frames_done(30, 16);
    assert!(sent_to(&mut wm, id).contains(&WindowEvent::FrameDone { shown: 1, seen_ns: 30, period_ns: 16 }));
}

#[test]
fn a_resize_tells_the_client_once_a_frame() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.window_mut(id).unwrap().resizable = true;
    let f = wm.window(id).unwrap().frame();
    wm.pointer_moved(f.right() - 1, f.bottom() - 1);
    wm.pointer_button(0, true, 0);
    for d in 1..=20 {
        wm.pointer_moved(f.right() - 1 + d, f.bottom() - 1 + d);
    }
    let events = sent_to(&mut wm, id);
    assert!(!events.iter().any(|e| matches!(e, WindowEvent::Configure { .. })));
    wm.flush_configures();
    let r = wm.window(id).unwrap().client_rect;
    assert_eq!(sent_to(&mut wm, id), [configure(r.w, r.h, WindowState::Normal)]);
    assert_eq!((r.w, r.h), (f.w + 20, f.h - TITLE_HEIGHT + 20));
}

// ---- the pointer's grab ---------------------------------------------------------------------

#[test]
fn a_second_button_goes_to_the_window_that_has_the_pointer() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let other = wm.create_window(OTHER, spec_at(WindowKind::Normal, Rect::new(900, 100, 300, 300)), 0).unwrap();
    let c = wm.window(id).unwrap().client_rect;
    wm.pointer_moved(c.x + 10, c.y + 10);
    wm.pointer_button(0, true, 0);
    // Dragged over the other window, the right button pressed there.
    let o = wm.window(other).unwrap().client_rect;
    wm.pointer_moved(o.x + 10, o.y + 10);
    wm.pointer_button(1, true, 1);
    wm.pointer_button(1, false, 2);
    wm.pointer_button(0, false, 3);
    let events = sent(&mut wm);
    let buttons =
        |to: u32| events.iter().filter(|(w, e)| *w == to && matches!(e, WindowEvent::PointerButton { .. })).count();
    assert_eq!((buttons(id), buttons(other)), (4, 0));
    assert_eq!(wm.focused(), Some(id));
}

#[test]
fn a_move_cut_short_leaves_no_snap_behind() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.window_mut(id).unwrap().resizable = true;
    let t = wm.window(id).unwrap().title_rect();
    wm.pointer_moved(t.x + 100, t.y + 10);
    wm.pointer_button(0, true, 0);
    // Dragged to the left edge: it would snap there...
    wm.pointer_moved(0, 300);
    assert!(wm.snap_preview().is_some());
    // ...but its client closes it.
    wm.destroy(APP, id, 1).unwrap();
    wm.advance(1_000_000_000);
    assert!(wm.snap_preview().is_none());
    wm.pointer_button(0, false, 2);
    // A plain click on another window's title bar moves nothing.
    let other = wm.create_window(OTHER, spec(WindowKind::Normal, 400, 300), 3).unwrap();
    wm.window_mut(other).unwrap().resizable = true;
    let before = wm.window(other).unwrap().client_rect;
    let t = wm.window(other).unwrap().title_rect();
    wm.pointer_moved(t.x + 100, t.y + 10);
    wm.pointer_button(0, true, 4);
    wm.pointer_button(0, false, 5);
    assert_eq!(wm.window(other).map(|w| (w.client_rect, w.snapped)), Some((before, None)));
}

#[test]
fn other_buttons_do_not_end_a_move() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let t = wm.window(id).unwrap().title_rect();
    wm.pointer_moved(t.x + 100, t.y + 10);
    wm.pointer_button(0, true, 0);
    wm.pointer_button(1, true, 1);
    wm.pointer_button(1, false, 2);
    wm.pointer_moved(t.x + 150, t.y + 30);
    assert_eq!(wm.window(id).unwrap().title_rect().x, t.x + 50);
    wm.pointer_button(0, false, 3);
    wm.pointer_moved(t.x + 300, t.y + 30);
    assert_eq!(wm.window(id).unwrap().title_rect().x, t.x + 50);
    assert_eq!(wm.window_at(t.x + 60, t.y + 30), Some((id, Part::Title)));
}

// ---- input drivers going away ----------------------------------------------------------------

#[test]
fn what_a_driver_held_is_released_when_it_goes() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.input(MOUSE, InputEvent::Key { code: keys::LEFTALT, pressed: true }, 0);
    wm.input(MOUSE, InputEvent::Key { code: keys::A, pressed: true }, 0);
    assert!(wm.repeat_deadline().is_some());
    let c = wm.window(id).unwrap().client_rect;
    wm.input(MOUSE, InputEvent::Absolute { x: c.x as u32 + 5, y: c.y as u32 + 5, max_x: 1280, max_y: 800 }, 0);
    wm.input(MOUSE, InputEvent::Button { button: 0, pressed: true }, 0);
    sent(&mut wm);
    wm.source_gone(MOUSE, 1);
    assert_eq!(wm.repeat_deadline(), None);
    let events = sent_to(&mut wm, id);
    assert!(events.iter().any(|e| matches!(e, WindowEvent::Key { code: keys::A, pressed: false, .. })));
    assert!(events.iter().any(|e| matches!(e, WindowEvent::PointerButton { pressed: false, .. })));
    // Alt is up again: keys type.
    wm.input(MOUSE + 1, InputEvent::Key { code: keys::B, pressed: true }, 2);
    let typed = sent_to(&mut wm, id);
    assert!(matches!(&typed[..], [WindowEvent::Key { text, .. }] if text == "b"));
}

// ---- delivery ---------------------------------------------------------------------------------

#[test]
fn events_wait_while_a_channel_is_full() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    wm.key(keys::A, true, 0);
    wm.deliver(|_, _| Write::Full);
    assert_eq!(wm.blocked().collect::<Vec<_>>(), [id]);
    wm.key(keys::A, false, 1);
    // Nothing goes out until the channel has room.
    assert!(sent_to(&mut wm, id).is_empty());
    let mut out = Vec::new();
    wm.writable(id, |_, ev| {
        out.push(ev.clone());
        Write::Sent
    });
    assert!(matches!(&out[..], [WindowEvent::Key { pressed: true, .. }, WindowEvent::Key { pressed: false, .. }]));
    assert_eq!(wm.blocked().count(), 0);
}

// ---- the keyboard's shortcuts ------------------------------------------------------------

/// Presses and releases `code` while `modifier` is held.
fn chord(wm: &mut Wm<()>, modifier: u16, code: u16, now: u64) {
    wm.key(modifier, true, now);
    wm.key(code, true, now);
    wm.key(code, false, now);
    wm.key(modifier, false, now);
}

#[test]
fn alt_tab_brings_back_the_window_used_before() {
    let mut wm = wm();
    let first = desktop(&mut wm);
    let second = wm.create_window(OTHER, spec(WindowKind::Normal, 300, 200), 0).unwrap();
    wm.minimize(SHELL, first, 0).unwrap();
    assert_eq!(wm.focused(), Some(second));
    chord(&mut wm, keys::LEFTALT, keys::TAB, 1);
    assert!(wm.switcher().is_none());
    assert_eq!(wm.focused(), Some(first));
    assert_eq!(wm.window(first).unwrap().state, WindowState::Normal);
    // Escape leaves things as they were.
    wm.key(keys::LEFTALT, true, 2);
    wm.key(keys::TAB, true, 2);
    assert_eq!(wm.switcher().and_then(|s| s.choice()), Some(second));
    wm.key(keys::ESC, true, 2);
    wm.key(keys::LEFTALT, false, 2);
    assert_eq!(wm.focused(), Some(first));
}

#[test]
fn super_arrows_snap_maximise_and_restore() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let before = wm.window(id).unwrap().client_rect;
    chord(&mut wm, keys::LEFTMETA, keys::LEFT, 0);
    let area = wm.work_area();
    let w = wm.window(id).unwrap();
    assert_eq!(w.client_rect, Rect::new(0, TITLE_HEIGHT, area.w / 2, area.h - TITLE_HEIGHT));
    chord(&mut wm, keys::LEFTMETA, keys::UP, 1);
    assert_eq!(wm.window(id).unwrap().state, WindowState::Maximized);
    chord(&mut wm, keys::LEFTMETA, keys::DOWN, 2);
    // Back to where it was before it was snapped.
    assert_eq!(wm.window(id).map(|w| (w.state, w.client_rect)), Some((WindowState::Normal, before)));
    chord(&mut wm, keys::LEFTMETA, keys::DOWN, 3);
    assert_eq!(wm.window(id).unwrap().state, WindowState::Minimized);
    // Super alone, released after the chords, opened no start menu.
    assert!(!sent(&mut wm).iter().any(|(_, e)| *e == WindowEvent::StartMenuKey {}));
}

#[test]
fn alt_f4_twice_closes_a_client_that_does_not_answer() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    chord(&mut wm, keys::LEFTALT, keys::F4, 1);
    assert!(sent_to(&mut wm, id).contains(&WindowEvent::CloseRequested {}));
    wm.check_liveness(1 + UNRESPONSIVE_NS);
    assert!(wm.unresponsive(id));
    wm.take_damage();
    chord(&mut wm, keys::LEFTALT, keys::F4, 2 + UNRESPONSIVE_NS);
    assert_eq!(wm.take_dropped(), [APP]);
    assert!(wm.window(id).is_none());
    // The focus goes to what is left: the shell's surfaces take none.
    assert_eq!(wm.focused(), None);
}

// ---- focus -------------------------------------------------------------------------------

#[test]
fn closing_the_focused_window_focuses_the_one_below() {
    let mut wm = wm();
    let below = desktop(&mut wm);
    let top = wm.create_window(OTHER, spec(WindowKind::Normal, 300, 200), 0).unwrap();
    assert_eq!(wm.focused(), Some(top));
    wm.destroy(OTHER, top, 1).unwrap();
    // At once, while it fades out.
    assert_eq!(wm.focused(), Some(below));
    assert!(wm.window(top).is_some_and(|w| w.closing));
    wm.advance(1_000_000_000);
    assert!(wm.window(top).is_none());
    assert!(wm.take_destroyed().contains(&top));
}

#[test]
fn a_minimised_windows_popups_go_with_it() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let menu = wm.create_window(APP, spec(WindowKind::Popup, 200, 300), 0).unwrap();
    wm.minimize(SHELL, id, 1).unwrap();
    assert!(wm.window(menu).unwrap().dismissed);
    assert_ne!(wm.focused(), Some(menu));
}

#[test]
fn dragging_a_window_to_the_left_edge_snaps_it() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let t = wm.window(id).unwrap().title_rect();
    wm.pointer_moved(t.x + 100, t.y + 10);
    wm.pointer_button(0, true, 0);
    wm.pointer_moved(0, 300);
    let area = wm.work_area();
    let left = Rect::new(area.x, area.y, area.w / 2, area.h);
    assert_eq!(wm.snap_preview(), Some((id, left)));
    wm.pointer_button(0, false, 1);
    let w = wm.window(id).unwrap();
    assert_eq!(
        (w.client_rect, w.snapped),
        (Rect::new(0, TITLE_HEIGHT, left.w, left.h - TITLE_HEIGHT), Some(crate::window::Snap::Left))
    );
    // Dragged away again, it has its size back.
    let t = w.title_rect();
    let before = wm.window(id).unwrap().restore_rect;
    wm.pointer_moved(t.x + 50, t.y + 10);
    wm.pointer_button(0, true, 2);
    wm.pointer_moved(t.x + 300, t.y + 200);
    let w = wm.window(id).unwrap();
    assert_eq!((w.client_rect.w, w.client_rect.h, w.snapped), (before.w, before.h, None));
}

#[test]
fn applications_cannot_leave_the_shell_without_windows() {
    let mut wm = Wm::<()>::new(SCREEN);
    wm.connect(SHELL, identity("shell", true, 10), 0).unwrap();
    // As many applications as it takes to fill the window system.
    let processes = limits::WINDOWS_MAX / limits::WINDOWS_PER_PROCESS;
    for p in 0..processes as u64 {
        let key = 1000 + p;
        wm.connect(key, identity("app", false, 1000 + p), 0).unwrap();
        for _ in 0..limits::WINDOWS_PER_PROCESS {
            wm.create_window(key, spec(WindowKind::Borderless, 10, 10), 0).unwrap();
        }
    }
    wm.connect(9999, identity("late", false, 9999), 0).unwrap();
    assert_eq!(wm.create_window(9999, spec(WindowKind::Normal, 10, 10), 0), Err(DisplayError::NoMemory));
    assert!(wm.create_window(SHELL, spec(WindowKind::Notification, 10, 10), 0).is_ok());
}

// ---- the lock ------------------------------------------------------------------------------

/// A lock screen of the lock service's, drawn: its id.
fn lock_screen(wm: &mut Wm<()>) -> u32 {
    let id = wm.create_window(LOCK, spec(WindowKind::Lock, 1280, 800), 0).unwrap();
    draw(wm, LOCK, id, 1280, 800);
    id
}

/// The windows on the screen, bottom to top.
fn shown(wm: &Wm<()>) -> Vec<u32> {
    wm.paint_order().iter().copied().filter(|id| wm.window(*id).is_some_and(|w| wm.shown(w))).collect()
}

#[test]
fn only_the_lock_service_draws_lock_screens_and_unlocks() {
    let mut wm = wm();
    for client in [APP, SHELL, AGENT, LOCK_IMPOSTOR] {
        assert_eq!(wm.create_window(client, spec(WindowKind::Lock, 1280, 800), 0), Err(DisplayError::Denied));
    }
    wm.lock(SHELL, 0).unwrap();
    for client in [APP, SHELL, AGENT, LOCK_IMPOSTOR] {
        assert_eq!(wm.unlock(client, 1), Err(DisplayError::Denied));
    }
    assert!(wm.locked());
    // A lock screen covers the screen, whatever size it asks for.
    let id = wm.create_window(LOCK, spec(WindowKind::Lock, 100, 100), 1).unwrap();
    assert_eq!(wm.window(id).unwrap().client_rect, SCREEN);
    assert_eq!(wm.unlock(LOCK, 2), Ok(()));
    assert!(!wm.locked());
}

#[test]
fn the_system_locks_the_screen_and_applications_do_not() {
    for client in [SHELL, AGENT, LOCK] {
        let mut wm = wm();
        assert_eq!(wm.lock(client, 0), Ok(()));
        assert!(wm.locked());
    }
    let mut wm = wm();
    for client in [APP, OTHER, IMPOSTOR, LOCK_IMPOSTOR] {
        assert_eq!(wm.lock(client, 0), Err(DisplayError::Denied));
    }
    assert!(!wm.locked());
    // The system's services watch the lock; applications do not.
    assert_eq!(wm.may_watch_lock(AGENT), Ok(()));
    assert_eq!(wm.may_watch_lock(LOCK), Ok(()));
    assert_eq!(wm.may_watch_lock(APP), Err(DisplayError::Denied));
    assert_eq!(wm.may_watch_lock(LOCK_IMPOSTOR), Err(DisplayError::Denied));
}

#[test]
fn a_locked_screen_shows_the_lock_screen_alone() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    draw(&mut wm, APP, app, 600, 400);
    wm.advance(1_000_000_000);
    assert!(shown(&wm).contains(&app));
    let c = wm.window(app).unwrap().client_rect;
    // The window hides the desktop below it (but for its rounded corners).
    let inside = Rect::new(c.x, c.y, c.w, c.h / 2);
    assert!(wm.covering(inside).is_some());
    wm.take_damage();
    wm.lock(SHELL, 1).unwrap();
    // At once: the whole screen is drawn again, from the backdrop.
    assert!(wm.take_damage().iter().any(|r| r.contains_rect(&SCREEN)));
    assert!(shown(&wm).is_empty());
    assert_eq!(wm.covering(inside), None);
    assert!(!wm.screen_drawn());
    let lock = lock_screen(&mut wm);
    assert_eq!(shown(&wm), [lock]);
    assert!(wm.screen_drawn());
    // The pointer reaches it alone, and so does the wheel.
    assert_eq!(wm.window_at(c.x + 5, c.y + 5).map(|(id, _)| id), Some(lock));
    sent(&mut wm);
    wm.pointer_moved(c.x + 5, c.y + 5);
    wm.scroll(0, 1);
    assert!(sent(&mut wm).iter().all(|(to, _)| *to == lock));
    wm.unlock(LOCK, 2).unwrap();
    assert!(shown(&wm).contains(&app));
}

#[test]
fn a_locked_screen_hears_the_keyboard_alone() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    wm.lock(SHELL, 1).unwrap();
    assert!(sent_to(&mut wm, app).contains(&WindowEvent::Focus { focused: false }));
    let lock = lock_screen(&mut wm);
    assert_eq!(wm.focused(), Some(lock));
    sent(&mut wm);
    wm.key(keys::A, true, 2);
    wm.key(keys::A, false, 2);
    let events = sent(&mut wm);
    assert!(events.iter().all(|(to, _)| *to == lock));
    assert!(events.iter().any(|(_, e)| matches!(e, WindowEvent::Key { code: keys::A, pressed: true, .. })));
    // The window system's shortcuts do nothing: no switcher, no closing,
    // no desktop, and the shell hears neither Super nor Super+Space.
    chord(&mut wm, keys::LEFTALT, keys::TAB, 3);
    assert!(wm.switcher().is_none());
    chord(&mut wm, keys::LEFTALT, keys::F4, 4);
    chord(&mut wm, keys::LEFTMETA, keys::SPACE, 5);
    chord(&mut wm, keys::LEFTMETA, keys::D, 6);
    chord(&mut wm, keys::LEFTMETA, keys::L, 7);
    wm.key(keys::LEFTMETA, true, 8);
    wm.key(keys::LEFTMETA, false, 8);
    let events = sent(&mut wm);
    assert!(events.iter().all(|(to, _)| *to == lock), "{events:?}");
    assert!(!events.iter().any(|(_, e)| matches!(
        e,
        WindowEvent::CloseRequested {} | WindowEvent::StartMenuKey {} | WindowEvent::AgentKey {}
    )));
    assert_eq!(wm.window(app).unwrap().state, WindowState::Normal);
}

#[test]
fn super_l_locks_the_screen() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    chord(&mut wm, keys::LEFTMETA, keys::L, 1);
    assert!(wm.locked());
    assert_eq!(wm.focused(), None);
    // The press was the window system's, not the application's.
    let events = sent_to(&mut wm, app);
    assert!(!events.iter().any(|e| matches!(e, WindowEvent::Key { code: keys::L, .. })), "{events:?}");
}

#[test]
fn nothing_takes_the_focus_from_the_lock_screen() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    wm.lock(SHELL, 1).unwrap();
    let lock = lock_screen(&mut wm);
    // Not the shell activating a window, a window opening, nor a popup.
    wm.activate(SHELL, app, 2).unwrap();
    let other = wm.create_window(OTHER, spec(WindowKind::Normal, 300, 200), 3).unwrap();
    assert_eq!(wm.create_window(OTHER, spec(WindowKind::Popup, 100, 100), 4), Err(DisplayError::Denied));
    assert_eq!(wm.focused(), Some(lock));
    // Nor a click where a window is.
    let c = wm.window(other).unwrap().client_rect;
    wm.pointer_moved(c.x + 10, c.y + 10);
    wm.pointer_button(0, true, 5);
    wm.pointer_button(0, false, 6);
    assert_eq!(wm.focused(), Some(lock));
    // Applications cannot reach the clipboard meanwhile.
    assert_eq!(wm.clipboard(APP), Err(DisplayError::Denied));
    assert_eq!(wm.set_clipboard(OTHER, "typed".into()), Err(DisplayError::Denied));
}

#[test]
fn unlocking_gives_the_keyboard_back() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    let popup = wm.create_window(APP, spec(WindowKind::Popup, 200, 300), 0).unwrap();
    wm.lock(AGENT, 1).unwrap();
    // The popup went with the focus.
    assert!(wm.window(popup).unwrap().dismissed);
    let lock = lock_screen(&mut wm);
    wm.unlock(LOCK, 2).unwrap();
    assert_eq!(wm.focused(), Some(app));
    // The lock screen is drawn (fading out) until its service closes it.
    assert!(wm.window(lock).is_some_and(|w| wm.shown(w)));
    wm.destroy(LOCK, lock, 3).unwrap();
    assert_eq!(wm.focused(), Some(app));
}

#[test]
fn the_lock_outlasts_its_screen() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    wm.lock(SHELL, 1).unwrap();
    lock_screen(&mut wm);
    // The lock service crashes: the screen stays locked, showing nothing
    // but the backdrop, and the keyboard is nobody's.
    wm.disconnect(LOCK);
    assert!(wm.locked() && wm.focused().is_none());
    assert!(shown(&wm).is_empty() && !wm.screen_drawn());
    let c = wm.window(app).unwrap().client_rect;
    assert_eq!(wm.window_at(c.x + 5, c.y + 5), None);
    // Started again, its new screen has the keyboard.
    wm.connect(LOCK, identity("lock", true, 17), 2).unwrap();
    let lock = lock_screen(&mut wm);
    assert_eq!(wm.focused(), Some(lock));
    wm.unlock(LOCK, 3).unwrap();
    assert_eq!(wm.focused(), Some(app));
}

#[test]
fn a_window_system_started_locked_shows_nothing_until_it_is_unlocked() {
    let mut wm = wm();
    wm.lock_at_start();
    let back = wm.create_window(SHELL, spec(WindowKind::Desktop, 1280, 800), 0).unwrap();
    let bar = wm.create_window(SHELL, spec_at(WindowKind::Panel, Rect::new(0, 752, 1280, 48)), 0).unwrap();
    draw(&mut wm, SHELL, back, 1280, 800);
    draw(&mut wm, SHELL, bar, 1280, 48);
    // The desktop has drawn itself, but what the screen shows has not.
    assert!(wm.desktop_drawn() && !wm.screen_drawn());
    assert!(shown(&wm).is_empty());
    lock_screen(&mut wm);
    assert!(wm.screen_drawn());
    wm.unlock(LOCK, 1).unwrap();
    assert!(wm.screen_drawn() && shown(&wm).contains(&back));
}

#[test]
fn locking_ends_a_move_and_the_switcher() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let t = wm.window(id).unwrap().title_rect();
    let before = wm.window(id).unwrap().client_rect;
    wm.pointer_moved(t.x + 100, t.y + 10);
    wm.pointer_button(0, true, 0);
    wm.lock(SHELL, 1).unwrap();
    wm.pointer_moved(t.x + 400, t.y + 300);
    wm.pointer_button(0, false, 2);
    assert_eq!(wm.window(id).unwrap().client_rect, before);
    wm.unlock(LOCK, 3).unwrap();
    wm.create_window(OTHER, spec(WindowKind::Normal, 300, 200), 4).unwrap();
    wm.key(keys::LEFTALT, true, 5);
    wm.key(keys::TAB, true, 5);
    assert!(wm.switcher().is_some());
    wm.lock(AGENT, 6).unwrap();
    assert!(wm.switcher().is_none());
    // Alt's release, after unlocking, chooses nothing either.
    wm.unlock(LOCK, 7).unwrap();
    let focused = wm.focused();
    wm.key(keys::LEFTALT, false, 8);
    assert_eq!(wm.focused(), focused);
}

#[test]
fn windows_behind_the_lock_wait_for_their_frames() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    draw(&mut wm, APP, app, 600, 400);
    wm.frames_done(10, 16);
    assert!(sent_to(&mut wm, app).contains(&WindowEvent::FrameDone { shown: 0, seen_ns: 10, period_ns: 16 }));
    wm.lock(SHELL, 1).unwrap();
    let lock = lock_screen(&mut wm);
    wm.take_damage();
    // Its frames go unseen: not composed, not acknowledged.
    wm.present(APP, app, 1, Vec::new(), Vec::new(), Vec::new()).unwrap();
    wm.frames_done(20, 16);
    assert!(!sent_to(&mut wm, app).iter().any(|e| matches!(e, WindowEvent::FrameDone { .. })));
    // The lock screen's are.
    wm.present(LOCK, lock, 1, Vec::new(), Vec::new(), Vec::new()).unwrap();
    wm.frames_done(30, 16);
    assert!(sent_to(&mut wm, lock).iter().any(|e| matches!(e, WindowEvent::FrameDone { .. })));
    // Unlocked, the window's frame is acknowledged with the next.
    wm.unlock(LOCK, 2).unwrap();
    wm.frames_done(40, 16);
    assert!(sent_to(&mut wm, app).contains(&WindowEvent::FrameDone { shown: 1, seen_ns: 40, period_ns: 16 }));
}

// ---- the idle lock -------------------------------------------------------------------------

/// A second, in the window manager's time (ns).
const S: u64 = 1_000_000_000;

#[test]
fn only_the_lock_service_sets_the_idle_timeout() {
    let mut wm = wm();
    for client in [SHELL, AGENT, APP, LOCK_IMPOSTOR] {
        assert_eq!(wm.set_idle_timeout(client, 60), Err(DisplayError::Denied));
    }
    // Until it does, the screen never locks for want of input.
    assert_eq!(wm.idle_deadline(), None);
    assert!(!wm.check_idle(1000 * S) && !wm.locked());
    wm.input(MOUSE, InputEvent::Motion { dx: 1, dy: 0 }, 1000 * S);
    assert_eq!(wm.set_idle_timeout(LOCK, 60), Ok(()));
    assert_eq!(wm.idle_deadline(), Some(1060 * S));
    // Set again (the lock service reconnected), it counts from the same
    // input: a service that restarts does not hold the lock off.
    wm.set_idle_timeout(LOCK, 300).unwrap();
    assert_eq!(wm.idle_deadline(), Some(1300 * S));
    // 0: never.
    wm.set_idle_timeout(LOCK, 0).unwrap();
    assert_eq!(wm.idle_deadline(), None);
    assert!(!wm.check_idle(5000 * S) && !wm.locked());
}

#[test]
fn the_screen_locks_once_no_input_has_come_for_the_idle_timeout() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    wm.set_idle_timeout(LOCK, 60).unwrap();
    // Every kind of input starts the time again.
    let events = [
        InputEvent::Key { code: keys::A, pressed: true },
        InputEvent::Key { code: keys::A, pressed: false },
        InputEvent::Motion { dx: 3, dy: 0 },
        InputEvent::Absolute { x: 10, y: 10, max_x: 1280, max_y: 800 },
        InputEvent::Button { button: 0, pressed: true },
        InputEvent::Button { button: 0, pressed: false },
        InputEvent::Scroll { dx: 0, dy: 1 },
    ];
    let mut t = 0;
    for ev in events {
        t += 50 * S;
        assert!(!wm.check_idle(t), "{ev:?}");
        wm.input(MOUSE, ev, t);
        assert_eq!(wm.idle_deadline(), Some(t + 60 * S));
    }
    sent(&mut wm);
    assert!(!wm.check_idle(t + 60 * S - 1) && !wm.locked());
    assert!(wm.check_idle(t + 60 * S));
    assert!(wm.locked());
    // As any lock does, it takes the keyboard from the application.
    assert!(sent_to(&mut wm, app).contains(&WindowEvent::Focus { focused: false }));
    // While it is locked, there is nothing to wait for.
    assert_eq!(wm.idle_deadline(), None);
    assert!(!wm.check_idle(t + 1000 * S));
}

#[test]
fn unlocking_starts_the_idle_time_again() {
    let mut wm = wm();
    desktop(&mut wm);
    wm.set_idle_timeout(LOCK, 60).unwrap();
    chord(&mut wm, keys::LEFTMETA, keys::L, 10 * S);
    lock_screen(&mut wm);
    // Long after the last key, the user unlocks: the time counts from then.
    wm.unlock(LOCK, 500 * S).unwrap();
    assert_eq!(wm.idle_deadline(), Some(560 * S));
    assert!(!wm.check_idle(559 * S));
    assert!(wm.check_idle(560 * S) && wm.locked());
}

#[test]
fn a_window_keeps_the_screen_awake_while_its_client_asks() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    draw(&mut wm, APP, app, 600, 400);
    wm.set_idle_timeout(LOCK, 60).unwrap();
    // A client's own normal and borderless windows only.
    assert_eq!(wm.keep_awake(OTHER, app, true), Err(DisplayError::NoSuchWindow));
    let popup = wm.create_window(APP, spec(WindowKind::Popup, 100, 100), 0).unwrap();
    assert_eq!(wm.keep_awake(APP, popup, true), Err(DisplayError::Invalid));
    wm.destroy(APP, popup, 0).unwrap();
    let borderless = wm.create_window(OTHER, spec(WindowKind::Borderless, 100, 100), 0).unwrap();
    assert_eq!(wm.keep_awake(OTHER, borderless, true), Ok(()));
    wm.keep_awake(OTHER, borderless, false).unwrap();
    // A video plays for ten minutes, nothing pressed: the screen stays
    // awake.
    assert_eq!(wm.keep_awake(APP, app, true), Ok(()));
    assert!(wm.kept_awake());
    assert_eq!(wm.idle_deadline(), None);
    assert!(!wm.check_idle(600 * S) && !wm.locked());
    // It ends: the time is due at once, to note when, and counts whole
    // from then.
    wm.keep_awake(APP, app, false).unwrap();
    assert!(!wm.kept_awake());
    assert_eq!(wm.idle_deadline(), Some(0));
    assert!(!wm.check_idle(601 * S));
    assert_eq!(wm.idle_deadline(), Some(661 * S));
    assert!(!wm.check_idle(660 * S) && !wm.locked());
    assert!(wm.check_idle(661 * S) && wm.locked());
}

#[test]
fn a_window_that_cannot_be_seen_does_not_keep_the_screen_awake() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    draw(&mut wm, APP, app, 600, 400);
    wm.set_idle_timeout(LOCK, 60).unwrap();
    wm.keep_awake(APP, app, true).unwrap();
    // Minimised, it is not seen; restored, it is again.
    wm.minimize(SHELL, app, S).unwrap();
    assert!(!wm.kept_awake());
    assert!(!wm.check_idle(2 * S));
    wm.activate(APP, app, 3 * S).unwrap();
    assert!(wm.kept_awake());
    // A window over part of it leaves it seen ...
    let small = wm.create_window(OTHER, spec(WindowKind::Normal, 200, 100), 4 * S).unwrap();
    draw(&mut wm, OTHER, small, 200, 100);
    wm.advance(10 * S);
    assert!(wm.kept_awake());
    // ... one that hides all of it does not.
    let big = wm.create_window(OTHER, spec(WindowKind::Normal, 600, 400), 11 * S).unwrap();
    wm.arrange(SHELL, big, dp::arrangement::MAXIMIZE, 11 * S).unwrap();
    let c = wm.window(big).unwrap().client_rect;
    draw(&mut wm, OTHER, big, c.w as u32, c.h as u32);
    wm.advance(20 * S);
    assert!(!wm.kept_awake());
    // It goes: the window is seen again, until it closes.
    wm.destroy(OTHER, big, 21 * S).unwrap();
    wm.advance(30 * S);
    assert!(wm.kept_awake());
    assert!(!wm.check_idle(90 * S));
    wm.destroy(APP, app, 100 * S).unwrap();
    assert!(!wm.kept_awake());
    // The compositor's next look notes it, and the time counts from then.
    assert!(!wm.check_idle(100 * S));
    assert!(!wm.check_idle(159 * S));
    assert!(wm.check_idle(160 * S) && wm.locked());
}

#[test]
fn only_the_agent_says_the_user_is_there() {
    let mut wm = wm();
    desktop(&mut wm);
    wm.set_idle_timeout(LOCK, 60).unwrap();
    wm.input(MOUSE, InputEvent::Motion { dx: 1, dy: 0 }, 10 * S);
    // Not the shell, the lock service, applications, nor one named like
    // the agent: the time counts on from the input.
    let named_like_the_agent = 9;
    wm.connect(named_like_the_agent, identity("agent", false, 17), 0).unwrap();
    for client in [SHELL, LOCK, APP, IMPOSTOR, LOCK_IMPOSTOR, named_like_the_agent] {
        assert_eq!(wm.user_active(client, 50 * S), Err(DisplayError::Denied));
    }
    assert_eq!(wm.idle_deadline(), Some(70 * S));
    // The agent's word starts it again, as input does ...
    assert_eq!(wm.user_active(AGENT, 50 * S), Ok(()));
    assert_eq!(wm.idle_deadline(), Some(110 * S));
    assert!(!wm.check_idle(109 * S) && !wm.locked());
    // ... and never moves it back.
    wm.input(MOUSE, InputEvent::Motion { dx: 1, dy: 0 }, 80 * S);
    wm.user_active(AGENT, 60 * S).unwrap();
    assert_eq!(wm.idle_deadline(), Some(140 * S));
    assert!(wm.check_idle(140 * S) && wm.locked());
    // Locked, it does nothing: the screen stays locked, and the time
    // starts again only as the user unlocks it.
    lock_screen(&mut wm);
    assert_eq!(wm.user_active(AGENT, 150 * S), Ok(()));
    assert!(wm.locked());
    assert_eq!(wm.idle_deadline(), None);
    wm.unlock(LOCK, 300 * S).unwrap();
    assert_eq!(wm.idle_deadline(), Some(360 * S));
}

#[test]
fn full_screen_alone_does_not_keep_the_screen_awake() {
    let mut wm = wm();
    let app = desktop(&mut wm);
    draw(&mut wm, APP, app, 600, 400);
    wm.set_window_state(APP, app, WindowState::Fullscreen, 0).unwrap();
    wm.set_idle_timeout(LOCK, 60).unwrap();
    // A film paused full screen, or a picture: the screen locks all the
    // same.
    assert!(!wm.kept_awake());
    assert!(wm.check_idle(60 * S) && wm.locked());
}

// ---- subsurfaces ------------------------------------------------------------------------------

/// The window's page: a subsurface placed under a 100-pixel toolbar.
const PAGE: Rect = Rect::new(0, 100, 600, 300);

/// The acknowledgements of subsurfaces' frames written since the last call.
fn acknowledged(wm: &mut Wm<()>) -> Vec<u32> {
    let mut out = Vec::new();
    wm.deliver_subsurfaces(|id, ev| {
        let SubsurfaceEvent::FrameDone { subsurface, .. } = ev else { panic!("{ev:?}") };
        assert_eq!(*subsurface, id);
        out.push(id);
        Write::Sent
    });
    out
}

/// What subsurfaces' clients were told since the last call.
fn told(wm: &mut Wm<()>) -> Vec<SubsurfaceEvent> {
    let mut out = Vec::new();
    wm.deliver_subsurfaces(|_, ev| {
        out.push(ev.clone());
        Write::Sent
    });
    out
}

/// Attaches a buffer of `w` x `h` named `name` to `client`'s subsurface `s`.
fn attach(wm: &mut Wm<()>, client: u64, s: u32, name: u32, w: u32, h: u32) {
    let layout = wm.subsurface_layout(client, s, (w, h), w, usize::MAX).unwrap();
    wm.attach_subsurface(client, s, name, Buffers { layout, memory: () }).unwrap();
}

/// A frame of window `id` placing `subsurfaces` at `PAGE`, with `overlays`.
fn place(wm: &mut Wm<()>, id: u32, subsurfaces: &[u32], overlays: &[Rect]) -> Result<(), DisplayError> {
    let placements = subsurfaces
        .iter()
        .map(|&s| dp::Placement {
            subsurface: s,
            rect: dp::Rect::new(PAGE.x, PAGE.y, PAGE.w as u32, PAGE.h as u32),
            background: 0xFFFFFF,
        })
        .collect();
    let overlays = overlays.iter().map(|r| dp::Rect::new(r.x, r.y, r.w as u32, r.h as u32)).collect();
    wm.present(APP, id, 1, vec![dp::Rect::new(0, 0, 1, 1)], placements, overlays)
}

/// A desktop with the application's window drawn, and a subsurface of it
/// presented by the process's other connection, with two buffers, the
/// first presented.
fn page(wm: &mut Wm<()>) -> (u32, u32) {
    let id = desktop(wm);
    draw(wm, APP, id, 600, 400);
    let s = wm.create_subsurface(APP_TOO, id).unwrap();
    attach(wm, APP_TOO, s, 1, 600, 300);
    attach(wm, APP_TOO, s, 2, 600, 300);
    wm.present_subsurface(APP_TOO, s, 1, Vec::new()).unwrap();
    wm.advance(1_000_000_000);
    (id, s)
}

#[test]
fn a_process_makes_subsurfaces_for_its_own_windows() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    // Another process's window is none of its business.
    assert_eq!(wm.create_subsurface(OTHER, id), Err(DisplayError::NoSuchWindow));
    assert_eq!(wm.create_subsurface(APP, 999), Err(DisplayError::NoSuchWindow));
    // Another connection of the window's process may; then it alone
    // attaches and presents.
    let s = wm.create_subsurface(APP_TOO, id).unwrap();
    assert_eq!(wm.subsurface_layout(APP, s, (10, 10), 10, usize::MAX), Err(DisplayError::NoSuchSubsurface));
    assert_eq!(wm.present_subsurface(OTHER, s, 1, Vec::new()), Err(DisplayError::NoSuchSubsurface));
    assert_eq!(wm.destroy_subsurface(APP, s), Err(DisplayError::NoSuchSubsurface));
    attach(&mut wm, APP_TOO, s, 1, 10, 10);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 1, Vec::new()), Ok(None));
    // The window's frames place it, and only the window's own subsurfaces.
    let theirs = wm.create_window(OTHER, spec(WindowKind::Normal, 100, 100), 0).unwrap();
    let foreign = wm.create_subsurface(OTHER, theirs).unwrap();
    draw(&mut wm, APP, id, 600, 400);
    assert_eq!(place(&mut wm, id, &[foreign], &[]), Err(DisplayError::Invalid));
    assert_eq!(place(&mut wm, id, &[s, s], &[]), Err(DisplayError::Invalid));
    assert_eq!(place(&mut wm, id, &[s], &[]), Ok(()));
}

#[test]
fn a_frame_places_few_subsurfaces_and_overlays_that_do_not_overlap() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    draw(&mut wm, APP, id, 600, 400);
    let many: Vec<u32> = (0..=dp::PLACEMENTS_MAX).map(|_| wm.create_subsurface(APP, id).unwrap()).collect();
    // Side by side, ten pixels wide each; and overlays a pixel each.
    let strips = |n: usize, overlap: i32| -> Vec<dp::Placement> {
        let at = |i: usize| dp::Rect::new(i as i32 * (10 - overlap), 0, 10, 10);
        (0..n).map(|i| dp::Placement { subsurface: many[i], rect: at(i), background: 0 }).collect()
    };
    let dots = |n: usize, overlap: i32| -> Vec<dp::Rect> {
        (0..n).map(|i| dp::Rect::new(i as i32 * (2 - overlap), 50, 1 + overlap as u32, 1)).collect()
    };
    let frame = |wm: &mut Wm<()>, placements, overlays| wm.present(APP, id, 1, Vec::new(), placements, overlays);
    assert_eq!(frame(&mut wm, strips(dp::PLACEMENTS_MAX + 1, 0), Vec::new()), Err(DisplayError::Invalid));
    assert_eq!(frame(&mut wm, strips(dp::PLACEMENTS_MAX, 0), Vec::new()), Ok(()));
    assert_eq!(frame(&mut wm, strips(2, 1), Vec::new()), Err(DisplayError::Invalid));
    assert_eq!(frame(&mut wm, strips(1, 0), dots(dp::OVERLAYS_MAX + 1, 0)), Err(DisplayError::Invalid));
    assert_eq!(frame(&mut wm, strips(1, 0), dots(dp::OVERLAYS_MAX, 0)), Ok(()));
    assert_eq!(frame(&mut wm, strips(1, 0), dots(2, 1)), Err(DisplayError::Invalid));
    let damage = vec![dp::Rect::new(0, 0, 1, 1); dp::DAMAGE_MAX + 1];
    assert_eq!(wm.present(APP, id, 1, damage.clone(), Vec::new(), Vec::new()), Err(DisplayError::Invalid));
    assert_eq!(wm.present_subsurface(APP, many[0], 1, damage), Err(DisplayError::Invalid));
}

#[test]
fn presenting_gives_back_the_buffer_shown_before() {
    let mut wm = wm();
    let (_, s) = page(&mut wm);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 2, Vec::new()), Ok(Some(1)));
    assert_eq!(wm.present_subsurface(APP_TOO, s, 1, Vec::new()), Ok(Some(2)));
    assert_eq!(wm.present_subsurface(APP_TOO, s, 3, Vec::new()), Err(DisplayError::BadBuffer));
    // A buffer detached while shown is nobody's to give back.
    wm.detach_subsurface(APP_TOO, s, 1).unwrap();
    assert_eq!(wm.present_subsurface(APP_TOO, s, 2, Vec::new()), Ok(None));
}

#[test]
fn a_subsurface_draws_again_where_it_changed_and_is_seen() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    let c = wm.window(id).unwrap().client_rect;
    // Not placed yet: nothing on the screen changes.
    wm.take_damage();
    wm.present_subsurface(APP_TOO, s, 2, vec![dp::Rect::new(10, 20, 30, 40)]).unwrap();
    assert!(wm.undamaged());
    // Placed: what changed, where it is placed, and no more of it.
    place(&mut wm, id, &[s], &[]).unwrap();
    wm.take_damage();
    wm.present_subsurface(APP_TOO, s, 1, vec![dp::Rect::new(10, 20, 30, 40), dp::Rect::new(590, 290, 50, 50)]).unwrap();
    let at = |r: Rect| r.translate(c.x + PAGE.x, c.y + PAGE.y);
    let mut damage = wm.take_damage();
    damage.sort_by_key(|r| r.y);
    assert_eq!(damage, [at(Rect::new(10, 20, 30, 40)), at(Rect::new(590, 290, 10, 10))]);
    // All of its place when the client says everything changed.
    wm.present_subsurface(APP_TOO, s, 2, Vec::new()).unwrap();
    assert_eq!(wm.take_damage(), [at(Rect::new(0, 0, PAGE.w, PAGE.h))]);
    // Minimised, its window is not composed again.
    wm.minimize(SHELL, id, 0).unwrap();
    wm.advance(2_000_000_000);
    wm.take_damage();
    wm.present_subsurface(APP_TOO, s, 1, Vec::new()).unwrap();
    assert!(wm.undamaged());
}

#[test]
fn a_frame_that_moves_a_subsurface_or_its_overlays_draws_them_again() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    let c = wm.window(id).unwrap().client_rect;
    let screen = |r: Rect| r.translate(c.x, c.y);
    place(&mut wm, id, &[s], &[]).unwrap();
    wm.take_damage();
    // A bar drawn over the page: there.
    let bar = Rect::new(200, 110, 380, 42);
    place(&mut wm, id, &[s], &[bar]).unwrap();
    let damage = wm.take_damage();
    assert!(
        damage.iter().any(|r| r.contains_rect(&screen(bar))) && !damage.iter().any(|r| r.contains_rect(&screen(PAGE)))
    );
    // The bar gone, and the page gone: all of the page's place.
    place(&mut wm, id, &[], &[]).unwrap();
    assert!(wm.take_damage().iter().any(|r| r.contains_rect(&screen(PAGE))));
    assert_eq!(wm.window(id).unwrap().placements.placed, []);
}

#[test]
fn subsurface_frames_are_acknowledged_once_seen() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    // Not placed: it cannot be seen, and waits.
    wm.frames_done(10, 16);
    assert!(acknowledged(&mut wm).is_empty());
    place(&mut wm, id, &[s], &[]).unwrap();
    wm.frames_done(20, 16);
    assert_eq!(acknowledged(&mut wm), [s]);
    // Once a frame: nothing presented, nothing to acknowledge.
    wm.frames_done(30, 16);
    assert!(acknowledged(&mut wm).is_empty());
    // Minimised: once it is back.
    wm.present_subsurface(APP_TOO, s, 2, Vec::new()).unwrap();
    wm.minimize(SHELL, id, 0).unwrap();
    wm.advance(2_000_000_000);
    wm.frames_done(40, 16);
    assert!(acknowledged(&mut wm).is_empty());
    wm.activate(SHELL, id, 3_000_000_000).unwrap();
    wm.frames_done(50, 16);
    let mut out = Vec::new();
    wm.deliver_subsurfaces(|_, ev| {
        out.push(ev.clone());
        Write::Sent
    });
    assert_eq!(out, [SubsurfaceEvent::FrameDone { subsurface: s, seen_ns: 50, period_ns: 16 }]);
}

#[test]
fn a_subsurfaces_acknowledgement_waits_while_its_channel_is_full() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    place(&mut wm, id, &[s], &[]).unwrap();
    wm.frames_done(10, 16);
    wm.deliver_subsurfaces(|_, _| Write::Full);
    assert_eq!(wm.blocked_subsurfaces().collect::<Vec<_>>(), [s]);
    // Frames composed meanwhile add nothing: one acknowledgement waits.
    wm.present_subsurface(APP_TOO, s, 2, Vec::new()).unwrap();
    wm.frames_done(26, 16);
    assert!(acknowledged(&mut wm).is_empty());
    let mut out = Vec::new();
    wm.subsurface_writable(s, |id, _| {
        out.push(id);
        Write::Sent
    });
    assert_eq!(out, [s]);
    assert_eq!(wm.blocked_subsurfaces().count(), 0);
}

#[test]
fn buffers_the_gpu_may_still_read_go_back_once_its_frame_is_done() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    place(&mut wm, id, &[s], &[]).unwrap();
    // The GPU composes a frame reading buffer 1 in place (its fence 5),
    // which it has yet to finish as 2 is presented: 1 is not given back.
    wm.subsurfaces_sampled(&[s], 5);
    wm.gpu_passed(4);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 2, Vec::new()), Ok(None));
    wm.frames_done(10, 16);
    assert_eq!(told(&mut wm), [SubsurfaceEvent::FrameDone { subsurface: s, seen_ns: 10, period_ns: 16 }]);
    // Once it has, the client hears it has 1 back.
    wm.gpu_passed(5);
    assert_eq!(told(&mut wm), [SubsurfaceEvent::Released { subsurface: s, buffer: 1 }]);
    // Given back before the frame's acknowledgement, when both are due; and
    // kept for when there is room on a full channel.
    wm.subsurfaces_sampled(&[s], 6);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 1, Vec::new()), Ok(None));
    wm.frames_done(26, 16);
    wm.gpu_passed(6);
    wm.deliver_subsurfaces(|_, _| Write::Full);
    assert_eq!(wm.blocked_subsurfaces().collect::<Vec<_>>(), [s]);
    let mut out = Vec::new();
    wm.subsurface_writable(s, |_, ev| {
        out.push(ev.clone());
        Write::Sent
    });
    assert_eq!(
        out,
        [
            SubsurfaceEvent::Released { subsurface: s, buffer: 2 },
            SubsurfaceEvent::FrameDone { subsurface: s, seen_ns: 26, period_ns: 16 },
        ]
    );
    // A frame whose fence has passed holds nothing back.
    wm.subsurfaces_sampled(&[s], 7);
    wm.gpu_passed(7);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 2, Vec::new()), Ok(Some(1)));
    // The GPU gone, what it may have read goes back, and the next one's
    // fences count afresh.
    wm.subsurfaces_sampled(&[s], 9);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 1, Vec::new()), Ok(None));
    wm.gpu_changed();
    assert_eq!(told(&mut wm), [SubsurfaceEvent::Released { subsurface: s, buffer: 2 }]);
    wm.subsurfaces_sampled(&[s], 1);
    assert_eq!(wm.present_subsurface(APP_TOO, s, 2, Vec::new()), Ok(None));
    // A buffer detached before it went back is the client's: nothing told.
    wm.detach_subsurface(APP_TOO, s, 1).unwrap();
    wm.gpu_passed(1);
    assert!(told(&mut wm).is_empty());
}

#[test]
fn what_is_made_of_subsurfaces_buffers_goes_with_them() {
    let mut wm = wm();
    let (_, s) = page(&mut wm);
    assert!(wm.take_gone_buffers().is_empty());
    let serial = |wm: &Wm<()>| wm.subsurface(s).and_then(|s| s.shown_serial()).map(|(_, n)| n).unwrap();
    // Buffer 1, shown, then detached: gone once another is shown.
    let first = serial(&wm);
    wm.detach_subsurface(APP_TOO, s, 1).unwrap();
    assert!(wm.take_gone_buffers().is_empty());
    wm.present_subsurface(APP_TOO, s, 2, Vec::new()).unwrap();
    assert_eq!(wm.take_gone_buffers(), [first]);
    // A name given again is another buffer.
    let second = serial(&wm);
    attach(&mut wm, APP_TOO, s, 1, 600, 300);
    wm.present_subsurface(APP_TOO, s, 1, Vec::new()).unwrap();
    assert!(serial(&wm) != first && serial(&wm) != second);
    // A buffer not shown goes as it is detached; the rest with their
    // subsurface.
    wm.detach_subsurface(APP_TOO, s, 2).unwrap();
    assert_eq!(wm.take_gone_buffers(), [second]);
    let third = serial(&wm);
    wm.destroy_subsurface(APP_TOO, s).unwrap();
    assert_eq!(wm.take_gone_buffers(), [third]);
}

#[test]
fn subsurfaces_go_with_their_window_and_their_connection() {
    let mut wm = wm();
    let (id, s) = page(&mut wm);
    let other = wm.create_subsurface(APP, id).unwrap();
    place(&mut wm, id, &[s], &[]).unwrap();
    // Destroyed: its place shows its background, until the window's next
    // frame places it no more.
    let c = wm.window(id).unwrap().client_rect;
    wm.take_damage();
    wm.destroy_subsurface(APP_TOO, s).unwrap();
    assert_eq!(wm.take_damage(), [PAGE.translate(c.x, c.y)]);
    assert!(wm.subsurface(s).is_none() && wm.window(id).unwrap().placements.of(s).is_some());
    assert_eq!(wm.take_destroyed_subsurfaces(), [s]);
    // A frame on its way as it went may place it still.
    assert_eq!(place(&mut wm, id, &[s], &[]), Ok(()));
    assert!(wm.placed_subsurface(id, s).is_none());
    // Its connection gone, a subsurface goes; its window gone, the rest.
    let third = wm.create_subsurface(APP_TOO, id).unwrap();
    wm.disconnect(APP_TOO);
    assert_eq!(wm.take_destroyed_subsurfaces(), [third]);
    wm.destroy(APP, id, 0).unwrap();
    wm.advance(1_000_000_000);
    assert_eq!(wm.take_destroyed_subsurfaces(), [other]);
    // Their ids are not given again at once.
    let theirs = wm.create_window(OTHER, spec(WindowKind::Normal, 10, 10), 0).unwrap();
    let again = wm.create_subsurface(OTHER, theirs);
    assert!(again.is_ok_and(|n| ![s, other, third].contains(&n)));
}

#[test]
fn a_process_attaches_only_so_much_to_its_subsurfaces() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    let s = wm.create_subsurface(APP, id).unwrap();
    // As large as the screen at most, and within the memory.
    assert_eq!(wm.subsurface_layout(APP, s, (1281, 10), 1281, usize::MAX), Err(DisplayError::BadBuffer));
    assert_eq!(wm.subsurface_layout(APP, s, (100, 100), 100, 100 * 100 * 4 - 1), Err(DisplayError::BadBuffer));
    // The process's subsurfaces together, from both its connections, a few
    // buffers each.
    let mut subsurfaces = Vec::new();
    for i in 0..limits::SUBSURFACE_SCREENS_PER_PROCESS {
        let client = if (i / BUFFERS_MAX).is_multiple_of(2) { APP } else { APP_TOO };
        if i.is_multiple_of(BUFFERS_MAX) {
            subsurfaces.push(wm.create_subsurface(client, id).unwrap());
        }
        let sub = *subsurfaces.last().unwrap();
        let layout = wm.subsurface_layout(client, sub, (1280, 800), 1280, usize::MAX).unwrap();
        wm.attach_subsurface(client, sub, i as u32 + 1, Buffers { layout, memory: () }).unwrap();
    }
    assert_eq!(wm.subsurface_layout(APP, s, (1, 1), 1, usize::MAX), Err(DisplayError::NoMemory));
    // Another process has its own.
    let theirs = wm.create_window(OTHER, spec(WindowKind::Normal, 100, 100), 0).unwrap();
    let t = wm.create_subsurface(OTHER, theirs).unwrap();
    assert!(wm.subsurface_layout(OTHER, t, (1280, 800), 1280, usize::MAX).is_ok());
}

#[test]
fn a_process_has_only_so_many_subsurfaces() {
    let mut wm = wm();
    let id = desktop(&mut wm);
    for i in 0..limits::SUBSURFACES_PER_PROCESS {
        let client = if i % 2 == 0 { APP } else { APP_TOO };
        wm.create_subsurface(client, id).unwrap();
    }
    assert_eq!(wm.create_subsurface(APP, id), Err(DisplayError::NoMemory));
}
