//! Interceptor: police pursuits through Veda Bay, in OpenGL ES 3.0.
//!
//! The game itself (the world, the cars, the pictures) is
//! `interceptor_game`; this program gives it a window, the keyboard and a
//! renderer (the GPU through Veda's renderer where there is one, the
//! software renderer otherwise), and draws the dashboard over its frames.
//! The scene is rendered at a resolution that adapts to what rendering
//! costs, and scaled to the window.
//!
//! Keys: arrows or WASD drive, Space is the handbrake, Q the siren, C
//! changes the camera, R puts the car back on the road, F3 shows
//! statistics, F11 toggles full screen, Esc quits.

#![no_std]
#![no_main]

extern crate alloc;

mod hud;

use alloc::format;
use alloc::string::String;

use interceptor_game::env::{Environment, TimeOfDay, Weather};
use interceptor_game::game::Game;
use interceptor_game::render::{Frame, Quality, Renderer};
use interceptor_game::world::World;
use vabi::{WaitItem, signals};
use vgl::backend::Present;
use vgl::{Config, gl};
use vproto::display::{WindowEvent, WindowSpec, WindowState};
use vproto::input::keys;
use vui::window::{Window, connect};

vrt::entry!(main);

/// Internal resolution divisors to choose from.
const SCALES: [f32; 7] = [1.0, 1.15, 1.33, 1.6, 2.0, 2.5, 3.0];
/// The time a frame's rendering may take (rendering and reading it back):
/// the resolution drops while it takes longer, and rises again when it
/// takes much less.
const BUDGET_MS: f32 = 12.0;

fn internal_size(w: i32, h: i32, scale: f32) -> (i32, i32) {
    (((w as f32 / scale) as i32).max(32), ((h as f32 / scale) as i32).max(18))
}

/// Waits until the window can take a frame; false if it closed.
fn wait_for_window(window: &mut Window) -> bool {
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
    let (mut text, fonts) = vui::load_fonts();
    let fonts = hud::Fonts { regular: fonts[0], bold: fonts[1], mono: fonts[2], mono_bold: fonts[3] };

    // A loading screen while the world is made.
    for _ in 0..2 {
        let _ = window.poll_events();
        if let Ok(mut canvas) = window.begin_frame() {
            hud::loading(&mut canvas, &mut text, &fonts);
            let _ = window.present(&[]);
        }
        if !wait_for_window(&mut window) {
            return 0;
        }
    }

    let mut level = 0usize;
    let (iw, ih) = internal_size(window.width, window.height, SCALES[level]);
    let config = Config { width: iw as u32, height: ih as u32, ..Config::default() };
    let mut gl = vgl::veda::context(config);
    let renderer_name: String = gl.get_string(gl::RENDERER).unwrap_or_default();
    let quality = Quality::for_renderer(&renderer_name);
    if quality == Quality::Low {
        // A software renderer: start small.
        level = 4;
    }
    vrt::println!("{} ({:?} quality)", renderer_name, quality);

    let t0 = vrt::time::now_ns();
    let world = World::new();
    let t1 = vrt::time::now_ns();
    let env = Environment::new(TimeOfDay::Night, Weather::Clear);
    let mut renderer = match Renderer::new(&mut gl, &world, env, quality) {
        Ok(r) => r,
        Err(e) => {
            vrt::println!("{}", e);
            return 1;
        }
    };
    let t2 = vrt::time::now_ns();
    vrt::println!(
        "Veda Bay made in {} ms ({} buildings), its pictures in {} ms",
        (t1 - t0) / 1_000_000,
        world.blocks.len(),
        (t2 - t1) / 1_000_000
    );
    let mut game = Game::new(world, env);
    vrt::println!("ready");

    let mut show_stats = false;
    let start = vrt::time::now_ns();
    let mut last = start;
    let (mut frames, mut fps, mut fps_since) = (0u32, 0.0f32, start);
    let mut log_since = start;
    let mut render_ms = 0.0f32;
    let mut since_change = 0u32;
    loop {
        for ev in window.poll_events() {
            match ev {
                WindowEvent::Key { code, pressed, repeat, .. } => {
                    let k = &mut game.keys;
                    match code {
                        keys::UP | keys::W => k.up = pressed,
                        keys::DOWN | keys::S => k.down = pressed,
                        keys::LEFT | keys::A => k.left = pressed,
                        keys::RIGHT | keys::D => k.right = pressed,
                        keys::SPACE => k.handbrake = pressed,
                        _ => {}
                    }
                    if pressed && !repeat {
                        match code {
                            keys::Q => game.toggle_siren(),
                            keys::C => game.follow.view = game.follow.view.next(),
                            keys::R => game.reset_player(),
                            keys::F3 => show_stats = !show_stats,
                            keys::F11 => {
                                let full = window.state == WindowState::Fullscreen;
                                window.set_state(if full { WindowState::Normal } else { WindowState::Fullscreen });
                            }
                            keys::ESC => return 0,
                            _ => {}
                        }
                    }
                }
                WindowEvent::Focus { focused: false } => game.release(),
                WindowEvent::CloseRequested {} => return 0,
                _ => {}
            }
        }
        if window.closed {
            return 0;
        }
        let now = vrt::time::now_ns();
        let dt = ((now - last) as f64 / 1e9).clamp(0.0, 0.25) as f32;
        last = now;
        game.update(dt);
        let (cars, lights) = game.frame(dt);

        // Render at the internal resolution.
        let (iw, ih) = internal_size(window.width, window.height, SCALES[level]);
        gl.resize(iw as u32, ih as u32);
        let t_render = vrt::time::now_ns();
        let frame = Frame { camera: game.camera, cars: &cars, lights: &lights, time: game.clock };
        renderer.render(&mut gl, &frame, iw, ih);
        gl.flush();

        if !wait_for_window(&mut window) {
            return 0;
        }
        let Ok(mut canvas) = window.begin_frame() else { continue };
        let (cw, ch) = (canvas.width(), canvas.height());
        {
            let (pixels, stride) = canvas.pixels_mut();
            let mut dst =
                Present { pixels, stride: stride as usize, width: cw as u32, height: ch as u32, opaque: true };
            gl.present_to(&mut dst);
        }
        let t_shown = vrt::time::now_ns();
        hud::dashboard(&mut canvas, &mut text, &fonts, &game);
        if show_stats {
            let lines = [
                format!("{fps:.1} fps   render {render_ms:.1} ms"),
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
        render_ms = render_ms * 0.9 + (t_shown - t_render) as f32 / 1e6 * 0.1;
        if now - fps_since >= 1_000_000_000 {
            fps = frames as f32 / ((now - fps_since) as f32 / 1e9);
            frames = 0;
            fps_since = now;
            if now - log_since >= 5_000_000_000 {
                log_since = now;
                vrt::println!(
                    "{:.1} fps, render {:.1} ms, {}x{} -> {}x{}, {} draws",
                    fps,
                    render_ms,
                    iw,
                    ih,
                    cw,
                    ch,
                    renderer.stats.draws
                );
            }
            since_change += 1;
            if since_change >= 2 {
                let old = level;
                if render_ms > BUDGET_MS && level + 1 < SCALES.len() {
                    level += 1;
                } else if render_ms < BUDGET_MS * 0.45 && level > 0 {
                    level -= 1;
                }
                if level != old {
                    since_change = 0;
                }
            }
        }
    }
}
