//! Veda Bay renders on the host: in software, and through Veda's
//! renderer (on softpipe, or on this machine's Intel GPU with
//! `VGL_GALLIUM_DEVICE=iris`), as it renders in Veda.
//!
//! `INTERCEPTOR_SHOTS=dir` saves views from round the loop, at every time
//! of day, as `INTERCEPTOR_SIZE` (1280x720 by default) PNGs, through the
//! renderer where it is built (in software otherwise).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use vgl::backend::Present;
use vgl::soft::{Serial, SoftBackend};
use vgl::{Config, Context, gl};

use crate::carmodel::Kind;
use crate::env::{Environment, TimeOfDay, Weather};
use crate::render::{Camera, CarView, Frame, Livery, Passing, Quality, Renderer};
use crate::world::World;
use glam::{Mat4, Vec3};

fn software(w: u32, h: u32) -> Context {
    let config = Config { width: w, height: h, ..Config::default() };
    Context::new(Box::new(SoftBackend::new(Box::new(Serial))), config)
}

fn renderer(w: u32, h: u32) -> Option<Context> {
    let config = Config { width: w, height: h, ..Config::default() };
    vgl::virgl::gallium::context(config)
}

/// A camera behind and above a car in the forward lane at `s`.
pub fn chase(world: &World, s: f32) -> Camera {
    let lane = 2.1;
    let eye = world.road.point(s - 9.0, lane) + Vec3::Y * 3.2;
    let target = world.road.point(s + 14.0, lane) + Vec3::Y * 1.2;
    Camera { eye, target, up: Vec3::Y, fov: 1.05 }
}

/// Renders a frame and reads it back as the window would show it.
fn frame(c: &mut Context, r: &mut Renderer, cam: &Camera, w: u32, h: u32) -> Vec<u32> {
    frame_with(c, r, cam, &[], w, h)
}

fn frame_with(c: &mut Context, r: &mut Renderer, cam: &Camera, cars: &[CarView], w: u32, h: u32) -> Vec<u32> {
    r.render(
        c,
        &Frame { camera: *cam, cars, lights: &[], time: 12.0, passing: Passing::default() },
        w as i32,
        h as i32,
    );
    assert_eq!(c.get_error(), gl::NO_ERROR);
    let mut px = vec![0u32; (w * h) as usize];
    c.present_to(&mut Present { pixels: &mut px, stride: w as usize, width: w, height: h, opaque: true });
    px
}

fn save(path: &str, px: &[u32], w: u32, h: u32) {
    let image = vimage::Image { width: w, height: h, pixels: px.to_vec() };
    std::fs::write(path, vimage::encode_png(&image).unwrap()).unwrap();
}

/// The sky at the top, the road at the bottom, all painted, and many
/// colours.
fn looks_like_a_view(px: &[u32], w: u32, h: u32) {
    assert!(px.iter().all(|&p| p >> 24 == 0xFF));
    let top = px[(w * 2 + w / 2) as usize];
    let bottom = px[(w * (h - 3) + w / 2) as usize];
    assert_ne!(top, bottom);
    let mut v = px.to_vec();
    v.sort_unstable();
    v.dedup();
    assert!(v.len() > 200, "{} colours", v.len());
}

#[test]
fn says_what_renders() {
    use crate::render::device_name;
    // Intel's GPUs as iris names them in the driver VM, and as virgl does
    // under QEMU (the host's GPU, through QEMU).
    assert_eq!(device_name("Mesa Intel(R) Graphics (ADL GT2)"), "Intel Graphics (ADL GT2)");
    assert_eq!(
        device_name("virgl (Mesa Intel(R) Iris(R) Xe Graphics (TGL GT2))"),
        "Intel Iris Xe Graphics (TGL GT2) through QEMU"
    );
    assert_eq!(device_name("virgl (NVIDIA GeForce RTX 3060/PCIe/SSE2)"), "NVIDIA GeForce RTX 3060 through QEMU");
    assert_eq!(
        device_name("virgl (llvmpipe (LLVM 19.1.7, 256 bits))"),
        "llvmpipe (LLVM 19.1.7, 256 bits) through QEMU"
    );
    assert_eq!(device_name("Veda software renderer"), "Veda software renderer");
    assert_eq!(Quality::for_renderer("virgl (llvmpipe (LLVM 19.1.7, 256 bits))"), Quality::Low);
}

#[test]
fn renders_veda_bay_in_software() {
    let (w, h) = (160, 90);
    let world = World::new();
    let mut c = software(w, h);
    let env = Environment::new(TimeOfDay::Day, Weather::Clear);
    let mut r = Renderer::new(&mut c, &world, env, Quality::Low).unwrap_or_else(|e| panic!("{e}"));
    let px = frame(&mut c, &mut r, &chase(&world, 300.0), w, h);
    looks_like_a_view(&px, w, h);
}

#[test]
fn renders_veda_bay_through_the_renderer() {
    let (w, h) = (320, 180);
    let Some(mut c) = renderer(w, h) else {
        std::println!("skipped: the renderer (vgallium) is not built");
        return;
    };
    let world = World::new();
    for time in [TimeOfDay::Day, TimeOfDay::Night] {
        let env = Environment::new(time, Weather::Clear);
        let mut r = Renderer::new(&mut c, &world, env, Quality::High).unwrap_or_else(|e| panic!("{e}"));
        let px = frame(&mut c, &mut r, &chase(&world, 300.0), w, h);
        looks_like_a_view(&px, w, h);
    }
}

/// Views from round the loop (`INTERCEPTOR_SHOTS=dir`).
#[test]
fn shots() {
    let Ok(dir) = std::env::var("INTERCEPTOR_SHOTS") else { return };
    let parse = |s: String| {
        let (a, b) = s.split_once('x')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let (w, h): (u32, u32) = std::env::var("INTERCEPTOR_SIZE").ok().and_then(parse).unwrap_or((1280, 720));
    let world = World::new();
    let (mut c, quality) = match renderer(w, h) {
        Some(c) => (c, Quality::High),
        None => (software(w, h), Quality::Low),
    };
    let places = [
        ("downtown", 120.0),
        ("plaza", 1150.0),
        ("hills", 2350.0),
        ("tunnel", 3050.0),
        ("coast", 3700.0),
        ("bridge", 4520.0),
        ("port", 5300.0),
    ];
    let only = std::env::var("INTERCEPTOR_ONLY").ok();
    for (tname, time) in [("day", TimeOfDay::Day), ("sunset", TimeOfDay::Sunset), ("night", TimeOfDay::Night)] {
        if only.as_deref().is_some_and(|o| !o.contains(tname)) {
            continue;
        }
        let env = Environment::new(time, Weather::Clear);
        let t0 = std::time::Instant::now();
        let mut r = Renderer::new(&mut c, &world, env, quality).unwrap_or_else(|e| panic!("{e}"));
        std::println!("{tname}: set up in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        for (pname, s) in places {
            if only.as_deref().is_some_and(|o| o.contains(':') && !o.contains(pname)) {
                continue;
            }
            let t0 = std::time::Instant::now();
            let px = frame(&mut c, &mut r, &chase(&world, s), w, h);
            std::println!(
                "{tname} {pname}: {:.1} ms, {} draws, {} triangles",
                t0.elapsed().as_secs_f64() * 1000.0,
                r.stats.draws,
                r.stats.triangles
            );
            save(&format!("{dir}/{tname}-{pname}.png"), &px, w, h);
        }
    }
}

/// A car standing on the road at `(s, lateral)`, facing along it (or
/// against it).
pub fn car_on_road(world: &World, kind: Kind, s: f32, lateral: f32, backwards: bool, paint: Vec3) -> CarView {
    let (p, f, r, u) = world.road.frame(s);
    let f = if backwards { -f } else { f };
    let left = u.cross(f).normalize();
    let pos = p + r * lateral;
    let transform = Mat4::from_cols(left.extend(0.0), u.extend(0.0), f.extend(0.0), pos.extend(1.0));
    let livery = match kind {
        Kind::Police => Livery::Police,
        Kind::Taxi => Livery::Taxi,
        _ => Livery::Plain,
    };
    CarView {
        kind,
        transform,
        paint,
        livery,
        spin: 0.0,
        steer: 0.0,
        headlights: 1.0,
        brake: 0.0,
        bar: if kind == Kind::Police { [1.0, 0.0] } else { [0.0, 0.0] },
    }
}

/// Every kind of car in a row on the avenue (`INTERCEPTOR_SHOWROOM=dir`).
#[test]
fn showroom() {
    let Ok(dir) = std::env::var("INTERCEPTOR_SHOWROOM") else { return };
    let (w, h) = (1280, 720);
    let world = World::new();
    let (mut c, quality) = match renderer(w, h) {
        Some(c) => (c, Quality::High),
        None => (software(w, h), Quality::Low),
    };
    let paints = [
        Vec3::new(0.02, 0.02, 0.02),
        Vec3::new(0.25, 0.27, 0.30),
        Vec3::new(0.50, 0.03, 0.02),
        Vec3::new(0.02, 0.10, 0.30),
        Vec3::new(0.60, 0.60, 0.62),
        Vec3::new(0.85, 0.85, 0.85),
        Vec3::new(0.70, 0.20, 0.01),
        Vec3::new(0.02, 0.25, 0.08),
        Vec3::new(0.80, 0.55, 0.02),
        Vec3::new(0.85, 0.62, 0.02),
    ];
    for (tname, time) in [("day", TimeOfDay::Day), ("night", TimeOfDay::Night)] {
        let env = Environment::new(time, Weather::Clear);
        let mut r = Renderer::new(&mut c, &world, env, quality).unwrap_or_else(|e| panic!("{e}"));
        let s0 = 200.0;
        let cars: Vec<CarView> = Kind::ALL
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                let s = s0 + (i / 2) as f32 * 7.5;
                let lateral = if i % 2 == 0 { 2.1 } else { 5.7 };
                car_on_road(&world, k, s, lateral, false, paints[i])
            })
            .collect();
        for (view, eye, target) in [
            (
                "side",
                world.road.point(s0 + 14.0, -4.0) + Vec3::Y * 2.2,
                world.road.point(s0 + 14.0, 4.0) + Vec3::Y * 0.6,
            ),
            (
                "front",
                world.road.point(s0 + 44.0, 0.5) + Vec3::Y * 2.0,
                world.road.point(s0 + 12.0, 3.5) + Vec3::Y * 0.6,
            ),
            ("rear", world.road.point(s0 - 9.0, 1.0) + Vec3::Y * 2.4, world.road.point(s0 + 10.0, 3.5) + Vec3::Y * 0.6),
        ] {
            let cam = Camera { eye, target, up: Vec3::Y, fov: 0.75 };
            let px = frame_with(&mut c, &mut r, &cam, &cars, w, h);
            save(&format!("{dir}/cars-{tname}-{view}.png"), &px, w, h);
        }
    }
}

/// A chase at night through the real game: the police car, siren on,
/// following a suspect, seen from the chase camera
/// (`INTERCEPTOR_CHASE=dir`; `INTERCEPTOR_AT=s` picks the place).
#[test]
fn chase_scene() {
    use crate::game::{Car, Game};
    use crate::vehicle::Spec;
    let Ok(dir) = std::env::var("INTERCEPTOR_CHASE") else { return };
    let (w, h) = (1280, 720);
    let at: f32 = std::env::var("INTERCEPTOR_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(150.0);
    // INTERCEPTOR_SOFT=1: the software renderer drawing as a GPU does.
    let soft = std::env::var("INTERCEPTOR_SOFT").is_ok();
    let (mut c, quality) = match renderer(w, h).filter(|_| !soft) {
        Some(c) => (c, Quality::High),
        None if soft => (software(w, h), Quality::High),
        None => (software(w, h), Quality::Low),
    };
    for (tname, time, weather) in [
        ("night", TimeOfDay::Night, Weather::Clear),
        ("day", TimeOfDay::Day, Weather::Clear),
        ("sunset", TimeOfDay::Sunset, Weather::Clear),
        ("rain", TimeOfDay::Night, Weather::Rain),
    ] {
        let env = Environment::new(time, weather);
        let world = World::new();
        let mut r = Renderer::new(&mut c, &world, env, quality).unwrap_or_else(|e| panic!("{e}"));
        let mut game = Game::new(world, env);
        let mut suspect = Car::on_road(&game.world, Kind::Coupe, Spec::sports(), at + 16.0, 5.7, false, 30.0);
        suspect.paint = Vec3::new(0.55, 0.02, 0.02);
        game.cars.push(suspect);
        let p = game.player;
        game.cars[p] = Car::on_road(&game.world, Kind::Police, Spec::interceptor(), at, 2.1, false, 30.0);
        game.cars[p].paint = Vec3::new(0.02, 0.02, 0.02);
        game.cars[p].siren = true;
        game.follow = crate::camera::Follow::new(&game.cars[p].vehicle);
        game.cars[1].controls.throttle = 0.6;
        game.cars[p].controls.throttle = 0.6;
        for k in 0..40 {
            game.update(1.0 / 60.0);
            let (cars, lights) = game.frame(1.0 / 60.0);
            if k == 39 {
                r.render(
                    &mut c,
                    &Frame {
                        camera: game.camera,
                        cars: &cars,
                        lights: &lights,
                        time: game.clock,
                        passing: Passing { rain: game.env.rain, ..Passing::default() },
                    },
                    w as i32,
                    h as i32,
                );
            }
        }
        let mut px = vec![0u32; (w * h) as usize];
        c.present_to(&mut Present { pixels: &mut px, stride: w as usize, width: w, height: h, opaque: true });
        save(&format!("{dir}/chase-{tname}.png"), &px, w, h);
    }
}

/// `INTERCEPTOR_FILM=dir`: the menus' showcase as the cinematic camera
/// films it, a frame every two seconds for forty, on one contact sheet.
#[test]
fn showcase_film() {
    use crate::session::{Mode, Session};
    let Ok(dir) = std::env::var("INTERCEPTOR_FILM") else { return };
    let (w, h) = (384u32, 216u32);
    let (cols, rows) = (5u32, 4u32);
    let (mut c, quality) = match renderer(w, h) {
        Some(c) => (c, Quality::High),
        None => (software(w, h), Quality::Low),
    };
    let world = alloc::sync::Arc::new(World::new());
    let mut session = Session::new(world.clone(), Mode::Showcase);
    let mut r = Renderer::new(&mut c, &world, session.env(), quality).unwrap_or_else(|e| panic!("{e}"));
    let mut sheet = vec![0u32; (w * cols * h * rows) as usize];
    let dt = 1.0 / 30.0;
    let mut shot = 0;
    for k in 0..(40.0 / dt) as usize {
        session.update(dt);
        let (cars, lights) = session.frame(dt);
        if k % 60 == 59 && shot < cols * rows {
            let frame = session.picture(&cars, &lights);
            r.render(&mut c, &frame, w as i32, h as i32);
            let mut px = vec![0u32; (w * h) as usize];
            c.present_to(&mut Present { pixels: &mut px, stride: w as usize, width: w, height: h, opaque: true });
            let (x0, y0) = ((shot % cols) * w, (shot / cols) * h);
            for y in 0..h {
                let row = ((y0 + y) * w * cols + x0) as usize;
                sheet[row..row + w as usize].copy_from_slice(&px[(y * w) as usize..((y + 1) * w) as usize]);
            }
            shot += 1;
        }
    }
    save(&format!("{dir}/film.png"), &sheet, w * cols, h * rows);
}

/// `INTERCEPTOR_EFFECTS=dir`: a handbrake slide at night downtown (smoke
/// and skid marks), then a scrape along a guardrail in the hills
/// (sparks), on one contact sheet.
#[test]
fn effects_scene() {
    use crate::driver::Role;
    use crate::effects::Effects;
    use crate::game::{Car, Game, STEP};
    use crate::vehicle::Spec;
    let Ok(dir) = std::env::var("INTERCEPTOR_EFFECTS") else { return };
    let (w, h) = (640u32, 360u32);
    let (cols, rows) = (3u32, 3u32);
    let (mut c, quality) = match renderer(w, h) {
        Some(c) => (c, Quality::High),
        None => (software(w, h), Quality::Low),
    };
    let env = Environment::new(TimeOfDay::Night, Weather::Clear);
    let world = World::new();
    let mut r = Renderer::new(&mut c, &world, env, quality).unwrap_or_else(|e| panic!("{e}"));
    let mut game = Game::new(world, env);
    let p = game.player;
    let place = |game: &mut Game, s: f32, lateral: f32, turn: f32, speed: f32| {
        let mut car = Car::on_road(&game.world, Kind::Police, Spec::interceptor(), s, lateral, false, speed);
        car.vehicle.yaw += turn;
        car.vehicle.set_speed(speed);
        car.role = Role::Player;
        car.siren = true;
        game.cars[p] = car;
        game.follow = crate::camera::Follow::new(&game.cars[p].vehicle);
    };
    place(&mut game, 150.0, 2.1, 0.0, 28.0);
    let mut fx = Effects::new();
    let mut sheet = vec![0u32; (w * cols * h * rows) as usize];
    let mut shot = 0;
    let dt = STEP * 4.0;
    let mut sparks_seen = 0;
    for k in 0..135 {
        let t = k as f32 * dt;
        if k == 60 {
            // Into the guardrail of the hills, glancing.
            let s = crate::career::section_start(&game.world.road, crate::road::Section::Hills) + 300.0;
            let (_, wall) = game.world.road.widths(s);
            place(&mut game, s, wall - 1.6, -0.12, 27.0);
        }
        game.keys.handbrake = t < 1.2;
        game.keys.left = t < 1.0;
        game.keys.up = k >= 60;
        game.update(dt);
        let (cars, lights) = game.frame(dt);
        fx.update(&game, game.camera.eye, dt);
        sparks_seen += fx.particles.iter().filter(|p| p.kind == crate::effects::Kind::Spark).count();
        if (k % 15 == 14 || (k > 60 && k % 8 == 0)) && shot < cols * rows {
            let (skids, skids_made) = fx.skids();
            let passing = Passing { particles: &fx.particles, skids, skids_made, rain: 0.0 };
            let frame = Frame { camera: game.camera, cars: &cars, lights: &lights, time: game.clock, passing };
            r.render(&mut c, &frame, w as i32, h as i32);
            assert_eq!(c.get_error(), gl::NO_ERROR);
            let mut px = vec![0u32; (w * h) as usize];
            c.present_to(&mut Present { pixels: &mut px, stride: w as usize, width: w, height: h, opaque: true });
            let (x0, y0) = ((shot % cols) * w, (shot / cols) * h);
            for y in 0..h {
                let row = ((y0 + y) * w * cols + x0) as usize;
                sheet[row..row + w as usize].copy_from_slice(&px[(y * w) as usize..((y + 1) * w) as usize]);
            }
            shot += 1;
        }
    }
    std::println!("{sparks_seen} spark-frames");
    save(&format!("{dir}/effects.png"), &sheet, w * cols, h * rows);
}

/// `INTERCEPTOR_SIGNS=dir`: the neon signs and the billboards, framed from
/// the road at night and by day, on one contact sheet.
#[test]
fn signs_scene() {
    let Ok(dir) = std::env::var("INTERCEPTOR_SIGNS") else { return };
    let (w, h) = (480u32, 270u32);
    let world = World::new();
    let (mut c, quality) = match renderer(w, h) {
        Some(c) => (c, Quality::High),
        None => (software(w, h), Quality::Low),
    };
    // From the road in front of each, looking at it.
    let mut views = Vec::new();
    for n in world.neons.iter().take(3) {
        let at = world.road.locate(n.base, None);
        let face = Vec3::new(n.yaw.sin(), 0.0, n.yaw.cos());
        let eye = world.road.point(at.s, 0.0) + face * 30.0 + Vec3::Y * 4.0;
        views.push(Camera { eye, target: n.base + Vec3::Y * 4.0, up: Vec3::Y, fov: 0.9 });
    }
    for b in world.billboards.iter().take(3) {
        let face = Vec3::new(b.yaw.sin(), 0.0, b.yaw.cos());
        let eye = b.base + face * 26.0 + Vec3::Y * 3.0;
        views.push(Camera { eye, target: b.base + Vec3::Y * 7.0, up: Vec3::Y, fov: 0.8 });
    }
    let (cols, rows) = (views.len() as u32, 2u32);
    let mut sheet = vec![0u32; (w * cols * h * rows) as usize];
    for (row, time) in [TimeOfDay::Night, TimeOfDay::Day].into_iter().enumerate() {
        let mut r = Renderer::new(&mut c, &world, Environment::new(time, Weather::Clear), quality)
            .unwrap_or_else(|e| panic!("{e}"));
        for (k, cam) in views.iter().enumerate() {
            let px = frame(&mut c, &mut r, cam, w, h);
            let (x0, y0) = (k as u32 * w, row as u32 * h);
            for y in 0..h {
                let at = ((y0 + y) * w * cols + x0) as usize;
                sheet[at..at + w as usize].copy_from_slice(&px[(y * w) as usize..((y + 1) * w) as usize]);
            }
        }
    }
    save(&format!("{dir}/signs.png"), &sheet, w * cols, h * rows);
}

/// `INTERCEPTOR_TIMING=1`: how long making the world and its pictures takes.
#[test]
fn startup_timing() {
    if std::env::var("INTERCEPTOR_TIMING").is_err() {
        return;
    }
    let t0 = std::time::Instant::now();
    let world = World::new();
    let t1 = std::time::Instant::now();
    let (mut c, quality) = match renderer(1280, 720).filter(|_| std::env::var("INTERCEPTOR_SOFT").is_err()) {
        Some(c) => (c, Quality::High),
        None => (software(1280, 720), Quality::High),
    };
    let t2 = std::time::Instant::now();
    let r = Renderer::new(&mut c, &world, Environment::new(TimeOfDay::Night, Weather::Clear), quality);
    let t3 = std::time::Instant::now();
    assert!(r.is_ok());
    // The parts.
    let time = |name: &str, f: &mut dyn FnMut()| {
        let t = std::time::Instant::now();
        f();
        std::println!("  {name}: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    };
    time("road", &mut || drop(crate::road::Road::new(crate::layout::VEDA_BAY)));
    time("terrain", &mut || drop(crate::terrain::Terrain::new(&world.road, crate::world::SEED)));
    time("buildings", &mut || drop(crate::city::buildings(&world.road, &world.terrain, crate::world::SEED)));
    time("lamps", &mut || drop(crate::lamps::place(&world.road)));
    time("billboards", &mut || drop(crate::scenery::billboards(&world.road, &world.terrain)));
    time("neons", &mut || drop(crate::scenery::neons(&world.road, &world.blocks)));
    time("trees", &mut || {
        drop(crate::scenery::trees(&world.road, &world.terrain, &world.blocks, &world.billboards, 1))
    });
    time("asphalt", &mut || drop(crate::texgen::asphalt(512)));
    time("surfaces", &mut || drop(crate::texgen::surfaces(256)));
    time("walls", &mut || drop(crate::texgen::walls(256)));
    time("ground", &mut || drop(crate::texgen::ground(512)));
    time("ripples", &mut || drop(crate::texgen::ripples(256)));
    time("boards", &mut || drop(crate::texgen::billboards(512)));
    time("road meshes", &mut || drop(crate::roadmesh::chunks(&world.road)));
    time("ground meshes", &mut || drop(crate::groundmesh::ground_chunks(&world.terrain)));
    time("sea", &mut || drop(crate::groundmesh::sea(&world.terrain)));
    time("cars", &mut || {
        for k in crate::carmodel::Kind::ALL {
            drop(crate::carmodel::build(k));
        }
    });
    time("bridge", &mut || drop(crate::scenery::bridge(&world.road)));
    std::println!(
        "world {:.0} ms, context {:.0} ms, renderer {:.0} ms ({quality:?})",
        (t1 - t0).as_secs_f64() * 1e3,
        (t2 - t1).as_secs_f64() * 1e3,
        (t3 - t2).as_secs_f64() * 1e3
    );
}
