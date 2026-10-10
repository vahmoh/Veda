//! Interceptor: police pursuits through Veda Bay, in OpenGL ES 3.0.
//!
//! The game itself (the world, the cars, the missions, the pictures, the
//! sounds) is `interceptor_game`; this program gives it a window, the
//! keyboard and the mouse, a renderer (the GPU through Veda's renderer
//! where there is one, the software renderer otherwise) and the speakers,
//! and draws the menus and the dashboard over its frames. The scene is
//! rendered at a resolution that adapts to what rendering costs, and
//! scaled to the window.
//!
//! In play: arrows or WASD drive, Space is the handbrake, Shift the
//! nitrous, Q the siren, H the horn, C changes the camera, R puts the car
//! back on the road, Esc pauses; F3 shows statistics, F11 toggles full
//! screen, F8 lets the computer drive (for demonstrations). The voice
//! agent can start, pause and change things too ([`agent`]).

#![no_std]
#![no_main]

extern crate alloc;

mod agent;
mod app;
mod hud;
mod menu;
mod paint;
mod speakers;
mod store;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use interceptor_game::render::{Quality, Renderer};
use interceptor_game::scale::{SCALES, Scale};
use interceptor_game::world::World;
use vabi::{WaitItem, signals};
use vgl::backend::Present;
use vgl::{Config, gl};
use vproto::display::{WindowEvent, WindowSpec, WindowState};
use vui::window::{Window, connect};

use app::{App, Back, Screen};
use speakers::Speakers;

vrt::entry!(main);

/// The frame time to keep to (ms): about 58 frames a second.
const TARGET_MS: f32 = 17.2;

fn internal_size(w: i32, h: i32, scale: f32) -> (i32, i32) {
    (((w as f32 / scale) as i32).max(32), ((h as f32 / scale) as i32).max(18))
}

/// Waits until the window can take a frame, keeping the events that
/// arrive meanwhile in `pending` (for the next frame); false if it closed.
fn wait_for_window(window: &mut Window, pending: &mut alloc::vec::Vec<WindowEvent>) -> bool {
    while !window.can_draw() {
        let mut items = [WaitItem {
            handle: window.event_channel().raw(),
            signals: signals::READABLE | signals::PEER_CLOSED,
            ..Default::default()
        }];
        let _ = vrt::object::wait_many(&mut items, vrt::time::now_ns() + 50_000_000);
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

fn main() -> i32 {
    let display = match connect() {
        Ok(d) => d,
        Err(e) => {
            vrt::println!("cannot connect to the display: {:?}", e);
            return 1;
        }
    };
    let mut spec = WindowSpec::new("Interceptor", 1280, 720);
    spec.app_id = "interceptor".into();
    spec.min_width = 480;
    spec.min_height = 270;
    let mut window = match Window::new(&display, spec) {
        Ok(w) => w,
        Err(e) => {
            vrt::println!("cannot create a window: {:?}", e);
            return 1;
        }
    };
    // The sounds start loading while the world is made.
    let speakers = Speakers::start();
    let (mut text, fonts) = vui::load_fonts();
    let fonts = paint::Fonts { regular: fonts[0], bold: fonts[1], mono: fonts[2], mono_bold: fonts[3] };

    // Events that came while a frame waited for the window.
    let mut pending = alloc::vec::Vec::new();
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

    let (iw, ih) = internal_size(window.width, window.height, SCALES[0]);
    let config = Config { width: iw as u32, height: ih as u32, ..Config::default() };
    let mut gl = vgl::veda::context(config);
    let renderer_name: String = gl.get_string(gl::RENDERER).unwrap_or_default();
    let quality = Quality::for_renderer(&renderer_name);
    // A software renderer starts small and may go smaller; a GPU starts at
    // the window's resolution and goes down no further than 1/2.
    let mut scale =
        if quality == Quality::Low { Scale::new(4, 6, TARGET_MS * 2.0) } else { Scale::new(0, 4, TARGET_MS) };
    vrt::println!("{} ({:?} quality)", renderer_name, quality);

    let t0 = vrt::time::now_ns();
    let world = Arc::new(World::new());
    let t1 = vrt::time::now_ns();
    let mut app = App::new(world.clone(), speakers);
    let mut renderer = match Renderer::new(&mut gl, &world, app.session.env(), quality) {
        Ok(r) => r,
        Err(e) => {
            vrt::println!("{}", e);
            return 1;
        }
    };
    app.relight = false;
    let map = hud::MiniMap::new(&world);
    let t2 = vrt::time::now_ns();
    vrt::println!(
        "Veda Bay made in {} ms ({} buildings), its pictures in {} ms",
        (t1 - t0) / 1_000_000,
        world.blocks.len(),
        (t2 - t1) / 1_000_000
    );
    vrt::println!("ready");

    let start = vrt::time::now_ns();
    let mut last = start;
    let (mut frames, mut fps, mut fps_since) = (0u32, 0.0f32, start);
    let mut log_since = start;
    let mut render_ms = 0.0f32;
    let mut show_ms = 0.0f32;
    let mut judged = start;
    let mut screen = app.screen;
    // The voice agent's link, and whether the window has the keyboard (play
    // pauses when it has not).
    let mut link = agent::Link::start();
    let mut focused = true;
    loop {
        let mut events = core::mem::take(&mut pending);
        events.extend(window.poll_events());
        for ev in events {
            match ev {
                WindowEvent::Key { code, pressed, repeat, .. } => app.key(code, pressed, repeat),
                WindowEvent::PointerMove { x, y } => app.point(x, y),
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
        if app.screen != screen {
            screen = app.screen;
            vrt::println!("screen {:?}", screen);
        }
        let now = vrt::time::now_ns();
        let dt = ((now - last) as f64 / 1e9).clamp(0.0, 0.25) as f32;
        last = now;
        app.update(dt);
        if app.relight {
            app.relight = false;
            if let Err(e) = renderer.set_environment(&mut gl, app.session.env()) {
                vrt::println!("cannot change the light: {}", e);
            }
        }
        let frozen = matches!(app.screen, Screen::Paused(_) | Screen::Options(_, Back::Pause));
        let (cars, lights) = app.session.frame(if frozen { 0.0 } else { dt });
        app.sound(dt);

        // Render at the internal resolution.
        let (iw, ih) = internal_size(window.width, window.height, scale.divisor());
        gl.resize(iw as u32, ih as u32);
        let t_render = vrt::time::now_ns();
        let frame = app.session.picture(&cars, &lights);
        renderer.render(&mut gl, &frame, iw, ih);
        gl.flush();
        let t_drawn = vrt::time::now_ns();

        if !wait_for_window(&mut window, &mut pending) {
            return 0;
        }
        let t_ready = vrt::time::now_ns();
        let Ok(mut canvas) = window.begin_frame() else { continue };
        let (cw, ch) = (canvas.width(), canvas.height());
        {
            let (pixels, stride) = canvas.pixels_mut();
            let mut dst =
                Present { pixels, stride: stride as usize, width: cw as u32, height: ch as u32, opaque: true };
            gl.present_to(&mut dst);
        }
        let t_shown = vrt::time::now_ns();
        let settings = app.save.settings;
        if app.screen == Screen::Playing {
            hud::play(&mut canvas, &mut text, &fonts, &app.session, &map, settings.units, app.clock);
        }
        menu::draw(&mut canvas, &mut text, &fonts, &mut app);
        if settings.stats {
            let lines = [
                format!("{fps:.1} fps   render {render_ms:.1} ms   show {show_ms:.1} ms"),
                format!("{iw} x {ih} -> {cw} x {ch}"),
                format!("{} draws, {} triangles", renderer.stats.draws, renderer.stats.triangles),
                renderer_name.clone(),
            ];
            hud::stats(&mut canvas, &mut text, &fonts, &lines);
        }
        drop(canvas);
        let _ = window.present(&[]);

        // Frame rate, and the resolution it calls for.
        frames += 1;
        render_ms = render_ms * 0.9 + (t_drawn - t_render) as f32 / 1e6 * 0.1;
        show_ms = show_ms * 0.9 + (t_shown - t_ready) as f32 / 1e6 * 0.1;
        if now - fps_since >= 1_000_000_000 {
            fps = frames as f32 / ((now - fps_since) as f32 / 1e9);
            frames = 0;
            fps_since = now;
            if now - log_since >= 5_000_000_000 {
                log_since = now;
                vrt::println!(
                    "{:.1} fps, render {:.1} ms, show {:.1} ms, {}x{} -> {}x{}, {} draws",
                    fps,
                    render_ms,
                    show_ms,
                    iw,
                    ih,
                    cw,
                    ch,
                    renderer.stats.draws
                );
            }
            // Every two seconds, the resolution the frame rate calls for.
            if now - judged >= 2_000_000_000 {
                let secs = (now - judged) as f32 / 1e9;
                judged = now;
                if scale.judge(1000.0 / fps.max(1.0), secs) {
                    vrt::println!("{:.1} fps: rendering at 1/{} of the window", fps, scale.divisor());
                }
            }
        }
    }
}
