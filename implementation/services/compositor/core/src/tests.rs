// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The window manager as its clients and the user meet it: a shell, an
//! agent and applications, driving it through its requests and input.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use vgfx::Rect;
use vproto::display::{self as dp, DisplayError, WindowEvent, WindowInfo, WindowKind, WindowSpec, WindowState};
use vproto::input::{InputEvent, keys};

use crate::buffers::Buffers;
use crate::clients::Identity;
use crate::limits;
use crate::outbox::Write;
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
/// The input driver's connection.
const MOUSE: u64 = 100;

fn identity(name: &str, service: bool, koid: u64) -> Identity {
    Identity { koid, name: name.into(), service }
}

/// A window manager with the shell, the agent and three applications
/// connected (two connections of the same process).
fn wm() -> Wm<()> {
    let mut wm = Wm::new(SCREEN);
    for (key, id) in [
        (SHELL, identity("shell", true, 10)),
        (AGENT, identity("agent", true, 11)),
        (APP, identity("files", false, 12)),
        (OTHER, identity("music", false, 13)),
        (APP_TOO, identity("files", false, 12)),
        (IMPOSTOR, identity("shell", false, 14)),
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
    wm.present(client, id, 0, Vec::new()).unwrap();
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
    let mut e = vipc::Encoder::with_header(20, 1, vipc::FLAG_RESPONSE);
    let widest: Vec<WindowInfo> =
        list.into_iter().map(|w| WindowInfo { app_id: String::from(&long[..limits::TEXT_MAX]), ..w }).collect();
    vipc::Encode::encode(widest, &mut e);
    assert!(e.bytes.len() <= vabi::CHANNEL_MAX_BYTES);
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
    assert_eq!(wm.present(APP, id, 1, damage), Err(DisplayError::Invalid));
    assert_eq!(wm.present(APP, id, 1, vec![dp::Rect::new(-50, -50, 70, 70)]), Ok(()));
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
    wm.present(APP, id, 1, Vec::new()).unwrap();
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
    wm.present(APP, id, 1, Vec::new()).unwrap();
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
