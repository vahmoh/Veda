// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The speakers: a thread that renders the game's sounds
//! ([`interceptor_game::audio`]) into a stream to the audio service, and
//! plays the music on a stream of its own.
//!
//! The game hands over what is sounding each frame and the cues of what
//! happened; the thread renders 10 ms blocks as the effects stream drains,
//! keeping about 40 ms queued (what is heard follows what is seen closely,
//! and a frame or two late from the game still plays on), and keeps the
//! music's longer queue topped up. Without an audio service (or sound
//! hardware) the thread ends, and the game plays on silently.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use interceptor_game::audio::music::Music;
use interceptor_game::audio::{Cue, Mixer, RATE, Scene};
use vabi::{WaitItem, signals};
use vproto::audio::{OutputStream, StreamSpec, audio};
use vproto::fs::vfs;
use vrt::object::Event;
use vrt::sync::Mutex;
use vrt::vm::Mapping;

/// The tunes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tune {
    /// The menus.
    Theme,
    /// Driving about.
    Patrol,
    /// A chase, its second stem joining as it heats up.
    Pursuit,
}

/// Where the tunes are, and their stems.
const DIR: &str = "/system/games/interceptor";
const TUNES: [(Tune, &[&str]); 3] = [
    (Tune::Theme, &["theme.qoa"]),
    (Tune::Patrol, &["patrol.qoa"]),
    (Tune::Pursuit, &["pursuit.qoa", "pursuit-high.qoa"]),
];

/// Frames rendered at a time (10 ms), and kept queued (about 40 ms).
const BLOCK: usize = RATE as usize / 100;
const QUEUED: u32 = BLOCK as u32 * 4;
/// The music's queue (ms) and the frames it is topped up by.
const MUSIC_MS: u32 = 500;
const MUSIC_BLOCK: usize = 2048;
/// The most cues waiting for the thread: more come only while it is
/// starved of time, and the oldest of those are long past.
const MOST_CUES: usize = 64;

/// What the game asks of the thread.
struct Asked {
    scene: Option<Scene>,
    cues: Vec<Cue>,
    tune: Option<Tune>,
    intensity: f32,
    music_volume: f32,
}

struct Shared {
    asked: Mutex<Asked>,
    wake: Event,
    /// The thread is running (it ends without an audio service).
    alive: AtomicBool,
}

/// The game's end of the speakers.
pub struct Speakers {
    shared: Arc<Shared>,
}

impl Speakers {
    /// Starts the speakers' thread.
    pub fn start() -> Option<Speakers> {
        let shared = Arc::new(Shared {
            asked: Mutex::new(Asked { scene: None, cues: Vec::new(), tune: None, intensity: 0.0, music_volume: 0.8 }),
            wake: Event::create().ok()?,
            alive: AtomicBool::new(true),
        });
        let theirs = shared.clone();
        vrt::thread::Builder::new()
            .name("speakers")
            .priority(vabi::priority::HIGH)
            .spawn(move || {
                run(&theirs);
                theirs.alive.store(false, Ordering::Release);
            })
            .ok()?;
        Some(Speakers { shared })
    }

    /// What is sounding now, and what just happened (nothing is kept once
    /// the thread has ended).
    pub fn play(&self, scene: Scene, cues: Vec<Cue>) {
        if !self.shared.alive.load(Ordering::Acquire) {
            return;
        }
        let mut a = self.shared.asked.lock();
        a.scene = Some(scene);
        a.cues.extend(cues);
        let over = a.cues.len().saturating_sub(MOST_CUES);
        a.cues.drain(..over);
    }

    /// Plays `tune` (crossfading), or fades the music out; `intensity`
    /// (0..1) brings in a tune's later stems.
    pub fn music(&self, tune: Option<Tune>, intensity: f32) {
        let mut a = self.shared.asked.lock();
        a.tune = tune;
        a.intensity = intensity;
    }

    /// The music's volume (0..1).
    pub fn set_music_volume(&self, v: f32) {
        self.shared.asked.lock().music_volume = v;
    }
}

/// A file mapped from the VMO the file system returned.
struct FileData {
    map: Mapping,
    len: usize,
}

impl AsRef<[u8]> for FileData {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the read-only mapping covers at least `len` bytes and lives
        // as long as `self`.
        unsafe { core::slice::from_raw_parts(self.map.as_ptr(), self.len) }
    }
}

fn read_file(vfs: &vfs::Client, path: &str) -> Option<FileData> {
    let (vmo, len) = vfs.read_file(path.into()).ok()?.ok()?;
    let size = vmo.size().ok()?;
    let map = Mapping::new(vmo, size.max(4096), vabi::map_flags::READ).ok()?;
    Some(FileData { map, len: (len as usize).min(size) })
}

/// Loads the tunes; the music's player and which of its tunes is which.
fn load_music() -> (Music<FileData>, Vec<(Tune, usize)>) {
    let mut music = Music::new();
    let mut tunes = Vec::new();
    let Some(vfs) = vproto::connect(vfs::NAME).ok().map(vfs::Client::new) else {
        return (music, tunes);
    };
    for (tune, stems) in TUNES {
        let files: Option<Vec<FileData>> = stems.iter().map(|s| read_file(&vfs, &format!("{DIR}/{s}"))).collect();
        match files.map(|f| music.add(f)) {
            Some(Ok(n)) => tunes.push((tune, n)),
            Some(Err(e)) => println!("cannot play the {:?} music: {}", tune, e),
            None => println!("no {:?} music in {}", tune, DIR),
        }
    }
    (music, tunes)
}

/// True once `name` is registered (waiting a few seconds for it, since a
/// connection to a service that does not exist would wait forever).
fn registered(name: &str) -> bool {
    let deadline = vrt::time::now_ns() + 5_000_000_000;
    loop {
        let found =
            vproto::with_registry(|r| r.list().map(|l| l.iter().any(|s| s == name)).unwrap_or(false)).unwrap_or(false);
        if found || vrt::time::now_ns() > deadline {
            return found;
        }
        vrt::time::sleep(vrt::time::Duration::from_millis(100));
    }
}

fn run(shared: &Shared) {
    let audio = registered(audio::NAME).then(|| vproto::connect(audio::NAME).ok().map(audio::Client::new)).flatten();
    let Some(audio) = audio else {
        println!("no sound: the audio service is unavailable");
        return;
    };
    let spec = StreamSpec {
        rate: RATE,
        channels: 2,
        buffer_frames: QUEUED * 2,
        notify_frames: BLOCK as u32,
        name: "Interceptor".into(),
        paused: false,
        volume: 1.0,
    };
    let effects = match OutputStream::open(&audio, spec) {
        Ok(s) => s,
        Err(e) => {
            println!("no sound: cannot open a stream ({:?})", e);
            return;
        }
    };
    let (mut music, tunes) = load_music();
    let tracks = if tunes.is_empty() {
        None
    } else {
        OutputStream::open(&audio, StreamSpec::stereo(music.rate(), MUSIC_MS, "Interceptor music")).ok()
    };
    println!("sound: {} tunes", tunes.len());
    let mut mixer = Mixer::new();
    let mut block = vec![0i16; BLOCK * 2];
    let mut music_block = vec![0i16; MUSIC_BLOCK * 2];
    let mut cues = Vec::new();
    let mut underruns = 0;
    // How busy the thread is (the time it works, how often it wakes) and
    // how often the stream ran dry, for the log every 20 s.
    let (mut busy, mut wakes, mut since) = (0u64, 0u32, vrt::time::now_ns());
    loop {
        let mut items = [
            WaitItem { handle: shared.wake.raw(), signals: signals::SIGNALED, ..Default::default() },
            WaitItem { handle: effects.event().raw(), signals: signals::SIGNALED, ..Default::default() },
            WaitItem {
                handle: tracks.as_ref().map_or(shared.wake.raw(), |t| t.event().raw()),
                signals: signals::SIGNALED,
                ..Default::default()
            },
        ];
        let _ = vrt::object::wait_many(&mut items, vrt::time::now_ns() + 20_000_000);
        let woke = vrt::time::now_ns();
        wakes += 1;
        let _ = shared.wake.clear();
        let (scene, tune, intensity, volume) = {
            let mut a = shared.asked.lock();
            core::mem::swap(&mut cues, &mut a.cues);
            (a.scene.take(), a.tune, a.intensity, a.music_volume)
        };
        if let Some(s) = scene {
            mixer.set_scene(s);
        }
        for c in cues.drain(..) {
            mixer.cue(c);
        }
        // The effects: kept about 40 ms ahead of what is heard.
        let _ = effects.event().clear();
        while effects.queued() + BLOCK as u32 <= QUEUED {
            mixer.render(&mut block);
            effects.write(&block);
        }
        if let Some(t) = &tracks {
            music.play(tune.and_then(|w| tunes.iter().find(|(t, _)| *t == w)).map(|(_, n)| *n));
            music.set_intensity(intensity);
            music.set_volume(volume);
            let _ = t.event().clear();
            while t.free() as usize >= MUSIC_BLOCK {
                music.render(&mut music_block);
                t.write(&music_block);
            }
        }
        let now = vrt::time::now_ns();
        // (A clock read on another processor may be a hair behind.)
        busy += now.saturating_sub(woke);
        if now.saturating_sub(since) >= 20_000_000_000 {
            let secs = now.saturating_sub(since) as f64 / 1e9;
            let u = effects.ring().underruns();
            println!(
                "sound: mixing takes {:.1}% of a CPU, {:.0} wakes a second, {} underruns",
                busy as f64 / 1e7 / secs,
                wakes as f64 / secs,
                u - underruns
            );
            (busy, wakes, since, underruns) = (0, 0, now, u);
        }
    }
}
