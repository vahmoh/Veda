// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Browser: the web, with WPE WebKit, in tabs.
//!
//! WebKit runs on a thread of its own ([`engine`]: its C bridge,
//! `engine/`), which draws each tab's page into a frame the window shows
//! and hears its input; its web content and network processes are this
//! program started again, which [`engine::auxiliary_process`] runs. What
//! the browser decides (addresses, search, history, bookmarks, settings,
//! the session, downloads' names, keys) is the host-tested
//! `browser-core`. One browser runs at a time ([`instance`]).
//!
//! * [`browser`]: the tabs, the engine's events, the user's commands, and
//!   input to the page.
//! * [`chrome`]: the tabs and the toolbar, and what opens from them.
//! * [`page`]: the page, or the browser's own page in its place.
//! * [`dialogs`]: what the browser asks or shows over everything.
//! * [`store`]: its files. [`agent`]: the voice agent's hands.
//! * [`sound`]: pages' sound, which the engine's media plays through the
//!   audio service.

mod agent;
mod browser;
mod chrome;
mod dialogs;
mod engine;
mod instance;
mod network;
mod page;
mod sound;
mod store;
mod tab;

use ui::WindowSpec;

use crate::browser::Browser;
use crate::engine::{Config, Engine};
use crate::instance::Claim;

/// The window's size for a screen of `w` x `h`.
fn window_size(w: u32, h: u32) -> (u32, u32) {
    let width = (w * 85 / 100).clamp(640, 1600).min(w.saturating_sub(40).max(640));
    let height = (h * 85 / 100).clamp(480, 1000).min(h.saturating_sub(60).max(480));
    (width, height)
}

fn main() {
    // WebKit's processes are this program started again.
    if let Some(code) = engine::auxiliary_process() {
        std::process::exit(code);
    }
    let urls: Vec<String> = std::env::args().skip(1).collect();
    let instance = match instance::claim(&urls) {
        Claim::First(instance) => Some(instance),
        // The browser that runs opens them.
        Claim::Running => return,
        Claim::Alone => None,
    };
    let screen = ui::window::connect().ok().and_then(|d| d.screen_info().ok());
    let (sw, sh) = screen.as_ref().map_or((1280, 800), |s| (s.width, s.height));
    let store = store::Store::load();
    let [data, cache, downloads, temporary] = Browser::engine_config_dirs();
    // What a browser that stopped without closing left of its temporary
    // files goes; their folder is there for the engine.
    store::clear_temporary(true);
    // Pages composited on the GPU if there is one; painted on the CPU
    // otherwise, which takes a fraction of the processor that compositing
    // them in software would.
    let gpu = if store.settings.acceleration { egl::gpu_renderer() } else { None };
    match &gpu {
        Some(renderer) => eprintln!("browser: pages are composited on the GPU: {renderer}"),
        None => eprintln!("browser: pages are painted on the CPU"),
    }
    let config = Config {
        data_dir: &data,
        cache_dir: &cache,
        downloads_dir: &downloads,
        temporary_dir: &temporary,
        application: Browser::application(),
        block_third_party_cookies: store.settings.block_third_party_cookies,
        accelerated: gpu.is_some(),
        screen: (sw as i32, sh as i32),
        refresh_rate: 60,
        scale: 1.0,
    };
    let engine = match Engine::start(&config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("browser: {e}");
            std::process::exit(1);
        }
    };
    let app = Browser::new(engine, instance, store, &urls);
    let (w, h) = window_size(sw, sh);
    let mut spec = WindowSpec::new("Browser", w, h);
    spec.min_width = 480;
    spec.min_height = 360;
    spec.app_id = "browser".into();
    let code = ui::run(spec, app);
    // The engine stopped with the browser: its temporary files go too.
    store::clear_temporary(false);
    std::process::exit(code);
}
