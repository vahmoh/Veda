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
use crate::render::{Camera, CarView, Frame, Livery, Quality, Renderer};
use crate::world::World;
use vmath::{Mat4, Vec3};

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
    r.render(c, &Frame { camera: *cam, cars, time: 12.0 }, w as i32, h as i32);
    assert_eq!(c.get_error(), gl::NO_ERROR);
    let mut px = vec![0u32; (w * h) as usize];
    c.present_to(&mut Present { pixels: &mut px, stride: w as usize, width: w, height: h, opaque: true });
    px
}

fn save(path: &str, px: &[u32], w: u32, h: u32) {
    let image = vimage::Image { width: w, height: h, pixels: px.to_vec() };
    std::fs::write(path, vimage::png::encode(&image, 6).unwrap()).unwrap();
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
        siren: (kind == Kind::Police).then_some(0.1),
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
