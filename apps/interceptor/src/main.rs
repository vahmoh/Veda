//! Interceptor: police pursuits through Veda Bay, in OpenGL ES 3.0.
//!
//! The game itself (the world, the cars, the pictures) is
//! `interceptor_game`; this program gives it a window, the keyboard and a
//! renderer (the GPU through Veda's renderer where there is one, the
//! software renderer otherwise), and draws the dashboard over its frames.
//! The scene is rendered at a resolution that adapts to what rendering
//! costs, and scaled to the window.
//!
//! Keys: arrows or WASD drive, Space is the handbrake, Shift the nitrous,
//! Q the siren, C changes the camera, R puts the car back on the road, F3
//! shows statistics, F8 lets the computer drive, F11 toggles full screen,
//! Esc quits.

#![no_std]
#![no_main]

extern crate alloc;

mod hud;
mod speakers;

use alloc::format;
use alloc::string::String;

use interceptor_game::audio::Cue;
use interceptor_game::carmodel::Kind;
use interceptor_game::env::{Environment, TimeOfDay, Weather};
use interceptor_game::game::Game;
use interceptor_game::pursuit::{Pursuit, State, Wanted};
use interceptor_game::render::{Frame, Quality, Renderer};
use interceptor_game::scale::{SCALES, Scale};
use interceptor_game::sound::Sound;
use interceptor_game::vehicle::Spec;
use interceptor_game::world::World;
use speakers::{Speakers, Tune};
use vabi::{WaitItem, signals};
use vgl::backend::Present;
use vgl::{Config, gl};
use vmath::Vec3;
use vproto::display::{WindowEvent, WindowSpec, WindowState};
use vproto::input::keys;
use vui::window::{Window, connect};

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

/// The suspects, in turn.
const WANTED: [Wanted; 3] = [
    Wanted {
        kind: Kind::Coupe,
        spec: Spec::sports(),
        paint: Vec3::new(0.55, 0.02, 0.02),
        skill: 0.92,
        aggression: 0.3,
    },
    Wanted {
        kind: Kind::Muscle,
        spec: Spec::sports(),
        paint: Vec3::new(0.85, 0.55, 0.02),
        skill: 0.95,
        aggression: 0.6,
    },
    Wanted {
        kind: Kind::Supercar,
        spec: Spec::supercar(),
        paint: Vec3::new(0.02, 0.18, 0.45),
        skill: 0.97,
        aggression: 0.4,
    },
];

/// A new chase: the next suspect, a little ahead of the player.
fn new_chase(game: &mut Game, n: u32) -> Pursuit {
    let wanted = &WANTED[n as usize % WANTED.len()];
    Pursuit::start(game, wanted, 70.0, 20.0)
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
    let speakers = Speakers::start();
    let (mut text, fonts) = vui::load_fonts();
    let fonts = hud::Fonts { regular: fonts[0], bold: fonts[1], mono: fonts[2], mono_bold: fonts[3] };

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
    game.start_traffic();
    game.cars[game.player].siren = true;
    let mut chase = new_chase(&mut game, 0);
    let mut chases = 1u32;
    let mut after = 0.0f32;
    let mut sound = Sound::new();
    if let Some(s) = &speakers {
        s.cue(Cue::Radio);
    }
    vrt::println!("ready");

    let mut show_stats = false;
    let start = vrt::time::now_ns();
    let mut last = start;
    let (mut frames, mut fps, mut fps_since) = (0u32, 0.0f32, start);
    let mut log_since = start;
    let mut render_ms = 0.0f32;
    let mut show_ms = 0.0f32;
    let mut judged = start;
    loop {
        let mut events = core::mem::take(&mut pending);
        events.extend(window.poll_events());
        for ev in events {
            match ev {
                WindowEvent::Key { code, pressed, repeat, .. } => {
                    let k = &mut game.keys;
                    match code {
                        keys::UP | keys::W => k.up = pressed,
                        keys::DOWN | keys::S => k.down = pressed,
                        keys::LEFT | keys::A => k.left = pressed,
                        keys::RIGHT | keys::D => k.right = pressed,
                        keys::SPACE => k.handbrake = pressed,
                        keys::LEFTSHIFT | keys::RIGHTSHIFT => k.boost = pressed,
                        _ => {}
                    }
                    if pressed && !repeat {
                        match code {
                            keys::Q => game.toggle_siren(),
                            keys::C => game.follow.view = game.follow.view.next(),
                            keys::R => game.reset_player(),
                            keys::F3 => show_stats = !show_stats,
                            keys::F8 => {
                                let on = !game.autopilot;
                                game.set_autopilot(on, Some(chase.suspect));
                                vrt::println!("autopilot {}", if on { "on" } else { "off" });
                            }
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
        chase.update(&mut game, dt);
        if !chase.on() {
            if after == 0.0 {
                vrt::println!("chase {}: {:?} after {:.0} s, {} hits", chases, chase.state, chase.time, chase.hits);
                if let Some(s) = &speakers {
                    s.cue(if chase.state == State::Escaped { Cue::Lose } else { Cue::Win });
                }
            }
            after += dt;
            if after > 6.0 {
                after = 0.0;
                game.remove_car(chase.suspect);
                chase = new_chase(&mut game, chases);
                chases += 1;
                if let Some(s) = &speakers {
                    s.cue(Cue::Radio);
                }
                if game.autopilot {
                    game.set_autopilot(true, Some(chase.suspect));
                }
            }
        }
        let (cars, lights) = game.frame(dt);
        if let Some(s) = &speakers {
            let (scene, cues) = sound.update(&game, 1.0, dt);
            s.play(scene, cues);
            // The chase's music heats up as the suspect is caught up with.
            let heat = if chase.on() { 1.0 - ((chase.gap - 50.0) / 150.0).clamp(0.0, 1.0) } else { 0.0 };
            s.music(Some(Tune::Pursuit), heat);
        }

        // Render at the internal resolution.
        let (iw, ih) = internal_size(window.width, window.height, scale.divisor());
        gl.resize(iw as u32, ih as u32);
        let t_render = vrt::time::now_ns();
        let frame = Frame { camera: game.camera, cars: &cars, lights: &lights, time: game.clock };
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
        let t_waited = t_ready;
        hud::dashboard(&mut canvas, &mut text, &fonts, &game);
        hud::pursuit(&mut canvas, &mut text, &fonts, &game, &chase);
        if show_stats {
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
        show_ms = show_ms * 0.9 + (t_shown - t_waited) as f32 / 1e6 * 0.1;
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
