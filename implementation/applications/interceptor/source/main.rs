// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Interceptor: police pursuits through Veda Bay, in OpenGL ES 3.0.
//!
//! The game itself (the world, the cars, the missions, the pictures, the
//! sounds) is `interceptor_game`; this program gives it a window, the
//! keyboard and the mouse, a renderer (the GPU through Veda's renderer
//! where there is one, the software renderer otherwise) and the speakers,
//! and draws the menus and the dashboard over its frames. It starts full
//! screen (Full HD on a 1080p display); on a GPU the scene is rendered at
//! the window's full resolution, while the software renderer's adapts to
//! what rendering costs, scaled to the window.
//!
//! In play: arrows or WASD drive, Space is the handbrake, Shift the
//! nitrous, Q the siren, H the horn, C changes the camera, R puts the car
//! back on the road, Esc pauses; F11 toggles full screen, F8 lets the
//! computer drive (for demonstrations). The frame rate and the GPU that
//! renders are shown in a corner ([`perf`]); F3 adds where frames' time
//! goes, or hides it all. The voice agent can start, pause and change
//! things too ([`agent`]).
//!
//! While a game is in play the screen does not lock for want of input
//! (`display::keep_awake`), since the computer may be driving; on the
//! menus and paused, it may.

mod agent;
mod app;
mod hud;
mod menu;
mod paint;
mod perf;
mod speakers;
mod store;

use std::sync::Arc;

use ::gl::backend::Present;
use ::gl::{Config, gl};
use abi::{WaitItem, signals};
use interceptor_game::career::Stats;
use interceptor_game::render::{Quality, Renderer};
use interceptor_game::scale::Scale;
use interceptor_game::world::World;
use proto::display::{Cursor, WindowEvent, WindowSpec, WindowState};
use ui::window::{Window, connect};

use app::{App, Back, Screen};
use perf::{Perf, Phases};
use speakers::Speakers;
use store::Store;

/// The frame time the software renderer keeps to (ms): about 29 frames
/// a second.
const SOFTWARE_TARGET_MS: f32 = 34.4;

/// The size to render at for a `w` x `h` window: all of it on a GPU, as
/// `scale` has it in software.
fn internal_size(w: i32, h: i32, scale: Option<&Scale>) -> (i32, i32) {
    let divisor = scale.map_or(1.0, Scale::divisor);
    (((w as f32 / divisor) as i32).max(32), ((h as f32 / divisor) as i32).max(18))
}

/// Waits until the window can take a frame, keeping the events that
/// arrive meanwhile in `pending` (for the next frame); false if it closed.
fn wait_for_window(window: &mut Window, pending: &mut Vec<WindowEvent>) -> bool {
    while !window.can_draw() {
        let mut items = [WaitItem {
            handle: window.event_channel().raw(),
            signals: signals::READABLE | signals::PEER_CLOSED,
            ..Default::default()
        }];
        let _ = runtime::object::wait_many(&mut items, runtime::time::now_ns() + 50_000_000);
        for ev in window.poll_events() {
            if let WindowEvent::CloseRequested {} = ev {
                return false;
            }
            pending.push(ev);
        }
        if window.closed {
            return false;
        }
    }
    true
}

fn main() {
    std::process::exit(run());
}

/// Runs the program; its exit code.
fn run() -> i32 {
    let display = match connect() {
        Ok(d) => d,
        Err(e) => {
            println!("cannot connect to the display: {:?}", e);
            return 1;
        }
    };
    // The settings say whether it starts full screen (as it does unless
    // played in a window last time): so from its first frame.
    let store = Store::new();
    let save = store.load();
    let mut spec = WindowSpec::new("Interceptor", 1280, 720);
    spec.app_id = "interceptor".into();
    spec.min_width = 480;
    spec.min_height = 270;
    let mut window = match Window::new(&display, spec) {
        Ok(w) => w,
        Err(e) => {
            println!("cannot create a window: {:?}", e);
            return 1;
        }
    };
    if save.settings.fullscreen {
        window.set_state(WindowState::Fullscreen);
    }
    // Each frame is drawn while the last is still on its way to the screen.
    window.triple_buffer();
    // The sounds start loading while the world is made.
    let speakers = Speakers::start();
    let (mut text, fonts) = ui::load_fonts();
    let fonts = paint::Fonts { regular: fonts[0], bold: fonts[1], mono: fonts[2], mono_bold: fonts[3] };

    // Events that came while a frame waited for the window.
    let mut pending = Vec::new();
    // A loading screen while the world is made.
    for _ in 0..2 {
        let _ = window.poll_events();
        if let Ok(mut canvas) = window.begin_frame() {
            hud::loading(&mut canvas, &mut text, &fonts);
            let _ = window.present(&[]);
        }
        if !wait_for_window(&mut window, &mut pending) {
            return 0;
        }
    }

    let (iw, ih) = internal_size(window.width, window.height, None);
    let config = Config { width: iw as u32, height: ih as u32, ..Config::default() };
    let mut gl = ::gl::veda::context(config);
    let renderer_name: String = gl.get_string(gl::RENDERER).unwrap_or_default();
    let quality = Quality::for_renderer(&renderer_name);
    // A GPU renders every frame at the window's full resolution (whatever
    // that costs: the frame rate shows it); a software renderer starts at
    // half of it, and goes up or down to keep to its frame rate.
    let mut scale = (quality == Quality::Low).then(|| Scale::new(4, 6, SOFTWARE_TARGET_MS));
    println!("{} ({:?} quality)", renderer_name, quality);

    let t0 = runtime::time::now_ns();
    let world = Arc::new(World::new());
    let t1 = runtime::time::now_ns();
    let mut app = App::new(world.clone(), speakers, store, save);
    let mut renderer = match Renderer::new(&mut gl, &world, app.session.env(), quality) {
        Ok(r) => r,
        Err(e) => {
            println!("{}", e);
            return 1;
        }
    };
    app.relight = false;
    let mut map = hud::MiniMap::new(&world);
    let t2 = runtime::time::now_ns();
    println!(
        "Veda Bay made in {} ms ({} buildings), its pictures in {} ms",
        (t1 - t0) / 1_000_000,
        world.blocks.len(),
        (t2 - t1) / 1_000_000
    );
    println!("ready");

    let start = runtime::time::now_ns();
    let mut last = start;
    let mut perf = Perf::new(renderer_name, start);
    // When the last frame ended (where the next one's time starts).
    let mut frame_end = start;
    let mut log_since = start;
    let mut judged = start;
    let mut screen = app.screen;
    // The voice agent's link, and whether the window has the keyboard (play
    // pauses when it has not).
    let mut link = agent::Link::start();
    let mut focused = true;
    // The pointer over the game: hidden while driving (the keyboard
    // drives), shown on the menus, which it works too; and where it last
    // was (it selects what it moves onto, not what comes under it).
    let mut cursor = Cursor::Arrow;
    let mut pointer = None;
    // Whether the window keeps the screen awake: while a game is in play.
    let mut awake = false;
    loop {
        let mut events = core::mem::take(&mut pending);
        events.extend(window.poll_events());
        for ev in events {
            match ev {
                WindowEvent::Key { code, pressed, repeat, .. } => app.key(code, pressed, repeat),
                WindowEvent::PointerMove { x, y } if pointer != Some((x, y)) => {
                    pointer = Some((x, y));
                    app.point(x, y);
                }
                WindowEvent::PointerButton { x, y, button: 0, pressed: true, .. } => app.click(x, y),
                WindowEvent::Focus { focused: f } => {
                    focused = f;
                    if !f {
                        app.unfocused();
                    }
                }
                WindowEvent::CloseRequested {} => return 0,
                _ => {}
            }
        }
        // The agent's requests come after the keyboard's, so what they see
        // is what the keys left.
        if let Some(l) = &mut link {
            agent::serve(l, &mut app, focused);
        }
        if window.closed || app.quit {
            return 0;
        }
        if let Some(full) = app.fullscreen.take() {
            window.set_state(if full { WindowState::Fullscreen } else { WindowState::Normal });
        }
        let pointer = if app.screen == Screen::Playing { Cursor::Hidden } else { Cursor::Arrow };
        if pointer != cursor {
            cursor = pointer;
            window.set_cursor(cursor);
        }
        if app.screen != screen {
            screen = app.screen;
            println!("screen {:?}", screen);
        }
        // The computer may be driving (F8), with nothing pressed for long;
        // the menus and a paused game let the screen lock.
        if (app.screen == Screen::Playing) != awake {
            awake = !awake;
            window.keep_awake(awake);
        }
        let now = runtime::time::now_ns();
        // Never a negative frame (from a clock read on another processor a
        // hair behind), nor more than a quarter of a second.
        let dt = (now.saturating_sub(last) as f64 / 1e9).min(0.25) as f32;
        last = now;
        app.update(dt);
        if app.relight {
            app.relight = false;
            if let Err(e) = renderer.set_environment(&mut gl, app.session.env()) {
                println!("cannot change the light: {}", e);
            }
        }
        let frozen = matches!(app.screen, Screen::Paused(_) | Screen::Options(_, Back::Pause));
        let (cars, lights) = app.session.frame(if frozen { 0.0 } else { dt });
        app.sound(dt);

        // Render at the internal resolution.
        let (iw, ih) = internal_size(window.width, window.height, scale.as_ref());
        gl.resize(iw as u32, ih as u32);
        let t_render = runtime::time::now_ns();
        let frame = app.session.picture(&cars, &lights);
        renderer.render(&mut gl, &frame, iw, ih);
        gl.flush();
        let t_drawn = runtime::time::now_ns();

        // The frame goes into the window's third buffer while the last one
        // is still on its way to the screen, and is handed over the moment
        // the compositor has taken that one. Waiting for it first, and only
        // then reading the frame back and drawing over it, the game and the
        // compositor would take turns every frame.
        let Ok(mut canvas) = window.begin_frame() else {
            if !wait_for_window(&mut window, &mut pending) {
                return 0;
            }
            continue;
        };
        let (cw, ch) = (canvas.width(), canvas.height());
        {
            let (pixels, stride) = canvas.pixels_mut();
            if let Some(mut dst) = Present::new(pixels, stride as usize, cw as u32, ch as u32, true) {
                gl.present_to(&mut dst);
            }
        }
        let t_shown = runtime::time::now_ns();
        let settings = app.save.settings;
        if app.screen == Screen::Playing {
            hud::play(&mut canvas, &mut text, &fonts, &app.session, &mut map, settings.units, app.clock);
        }
        menu::draw(&mut canvas, &mut text, &fonts, &mut app);
        if settings.stats != Stats::Off {
            // Top left in play, clear of the dashboard; top right on the
            // menus, which run down the left.
            let s = paint::scale(ch);
            let menus = matches!(
                app.screen,
                Screen::Title
                    | Screen::Main(_)
                    | Screen::Career(_)
                    | Screen::Quick(_)
                    | Screen::Patrol(_)
                    | Screen::Options(..)
            );
            let (x, y) = if menus {
                (cw as f32 - 16.0 * s, 16.0 * s)
            } else if app.screen == Screen::Playing {
                hud::performance_in_play(&canvas, &mut text, &fonts, &perf, &app.session)
            } else {
                (16.0 * s, 16.0 * s)
            };
            let height = hud::performance(&mut canvas, &mut text, &fonts, &perf, x, y, menus);
            if settings.stats == Stats::Detailed {
                let lines = [
                    format!("render {:.1} ms   show {:.1} ms", perf.render_ms, perf.show_ms),
                    format!("{iw} x {ih} -> {cw} x {ch}"),
                    format!("{} draws, {} triangles", renderer.stats.draws, renderer.stats.triangles),
                ];
                // Under the map in play, under the frame rate on the menus.
                let (right, top) = if menus { (x, y + height + 8.0 * s) } else { (cw as f32 - 16.0 * s, 210.0 * s) };
                hud::stats(&mut canvas, &mut text, &fonts, &perf, &lines, right, top);
            }
        }
        drop(canvas);
        let t_painted = runtime::time::now_ns();
        if !wait_for_window(&mut window, &mut pending) {
            return 0;
        }
        let _ = window.present(&[]);

        // The frame's length (from the last one's end) and what it went to;
        // once a second, the frame rate, the log, and the resolution it
        // calls for.
        let t_end = runtime::time::now_ns();
        let ms = |from: u64, to: u64| to.saturating_sub(from) as f32 / 1e6;
        let spent = Phases {
            update: ms(frame_end, t_render),
            render: ms(t_render, t_drawn),
            show: ms(t_drawn, t_shown),
            draw: ms(t_shown, t_painted),
            wait: ms(t_painted, t_end),
        };
        let length = ms(frame_end, t_end);
        frame_end = t_end;
        if perf.frame(t_end, length, spent) {
            if t_end - log_since >= 5_000_000_000 {
                log_since = t_end;
                let longest = perf.report();
                println!(
                    "{:.1} fps, render {:.1} ms, show {:.1} ms, {}x{} -> {}x{}, {} draws; {}",
                    perf.fps, perf.render_ms, perf.show_ms, iw, ih, cw, ch, renderer.stats.draws, longest
                );
            }
            // Every two seconds, the software renderer's resolution as the
            // frame rate calls for it.
            if let Some(scale) = &mut scale
                && t_end - judged >= 2_000_000_000
            {
                let secs = (t_end - judged) as f32 / 1e9;
                judged = t_end;
                if scale.judge(1000.0 / perf.fps.max(1.0), secs) {
                    println!("{:.1} fps: rendering at 1/{} of the window", perf.fps, scale.divisor());
                }
            }
        }
    }
}
