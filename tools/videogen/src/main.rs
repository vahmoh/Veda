//! `videogen` — films and encodes the sample videos shipped with Veda.
//!
//! ```text
//! videogen OUT_DIR [FILTER]
//! ```
//!
//! Writes, under `OUT_DIR`:
//!
//! * `samples/Videos/Aurora Timelapse.mkv`: northern lights over a lake,
//!   1280x720 at 24 fps, 16 s, with an ambient soundtrack;
//! * `samples/Videos/Countdown.mkv`: an A/V sync test, 640x360 at 30 fps,
//!   12 s: colour bars, then a countdown with a white flash and a beep on
//!   every whole second;
//! * `samples/Videos/Rising Bubbles.mkv`: 360x640 (portrait) at 30 fps,
//!   6 s, without sound;
//! * `tests/videos/Countdown.md5`: the MD5 of every picture of the
//!   countdown as a decoder outputs it (the visible Y, U and V planes, one
//!   lowercase line per picture in display order);
//! * `tests/videos/Truncated.mkv`: the first 60% of `Countdown.mkv`.
//!
//! The videos are AV1 (rav1e: 8-bit 4:2:0, BT.709, limited range) and FLAC
//! (48 kHz stereo, 16 bits) in Matroska. Every key frame starts a group of
//! pictures that is encoded on its own, all of them in parallel; the output
//! is the same on every run. Files are written under a temporary name and
//! renamed, so a stopped run never leaves half a file. The optional
//! `FILTER` makes only the videos whose file name contains it (handy while
//! tuning), and `VIDEOGEN_PREVIEW=DIR` also writes PNGs of a picture a
//! second, as drawn and as decoded, and every video's MD5 list.

mod aurora;
mod av1;
mod bubbles;
mod countdown;
mod flac;
mod md5;
mod mkv;
mod paint;
mod yuv;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The fonts the countdown is set in.
pub const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.otf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.otf");
pub const MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

const VIDEOS: &str = "samples/Videos";
const TESTS: &str = "tests/videos";

/// rav1e's speed preset for every video.
const SPEED: u8 = 10;

/// The sample rate of every video's sound (stereo, 16 bits).
pub const RATE: u32 = 48_000;

/// A sample video.
struct Film {
    file: &'static str,
    title: &'static str,
    settings: av1::Settings,
    frames: u32,
    picture: Box<dyn Fn(u32) -> Vec<u32> + Sync>,
    sound: Option<fn() -> Vec<i16>>,
}

impl Film {
    fn seconds(&self) -> f64 {
        self.frames as f64 / self.settings.fps as f64
    }
}

fn films() -> Vec<Film> {
    let settings = |width, height, fps: u32, key_seconds: u32, quantizer| av1::Settings {
        width,
        height,
        fps: fps as u64,
        key_interval: (fps * key_seconds) as u64,
        quantizer,
        speed: SPEED,
    };
    let scene = aurora::Aurora::new();
    let water = bubbles::Bubbles::new();
    vec![
        Film {
            file: "Aurora Timelapse.mkv",
            title: "Aurora Timelapse",
            settings: settings(aurora::WIDTH, aurora::HEIGHT, aurora::FPS, 2, 90),
            frames: aurora::FRAMES,
            picture: Box::new(move |f| scene.picture(f)),
            sound: Some(aurora::sound),
        },
        Film {
            file: "Countdown.mkv",
            title: "Countdown",
            settings: settings(countdown::WIDTH, countdown::HEIGHT, countdown::FPS, 1, 120),
            frames: countdown::FRAMES,
            picture: Box::new(countdown::picture),
            sound: Some(countdown::sound),
        },
        Film {
            file: "Rising Bubbles.mkv",
            title: "Rising Bubbles",
            settings: settings(bubbles::WIDTH, bubbles::HEIGHT, bubbles::FPS, 2, 135),
            frames: bubbles::FRAMES,
            picture: Box::new(move |f| water.picture(f)),
            sound: None,
        },
    ]
}

/// A piece of work for the pool.
#[derive(Clone, Copy)]
enum Job {
    /// Draw and encode a group of pictures: `count` frames from `first`.
    Pictures { film: usize, first: u32, count: u32 },
    /// Make a film's sound.
    Sound { film: usize },
}

enum Output {
    Pictures(av1::Video, Duration),
    Sound(Vec<i16>),
}

fn run_job(films: &[Film], job: Job, preview: Option<&Path>) -> Result<Output, String> {
    match job {
        Job::Pictures { film, first, count } => {
            let f = &films[film];
            let started = Instant::now();
            let mut enc = av1::Encoder::new(f.settings).map_err(|e| e.to_string())?;
            let fps = f.settings.fps as u32;
            let previews: Vec<u32> = (first..first + count).filter(|n| n % fps == fps / 2).collect();
            if preview.is_some() {
                enc.keep(previews.iter().map(|&n| (n - first) as u64).collect());
            }
            for n in first..first + count {
                let pixels = (f.picture)(n);
                if let Some(dir) = preview.filter(|_| previews.contains(&n)) {
                    write_png(dir, &format!("{} {n:03}.png", f.title), &pixels, f.settings.width)?;
                }
                enc.push(&yuv::from_rgb(&pixels, f.settings.width, f.settings.height))
                    .map_err(|e| format!("{}: {e}", f.file))?;
            }
            let video = enc.finish().map_err(|e| format!("{}: {e}", f.file))?;
            if let Some(dir) = preview {
                for (n, picture) in &video.kept {
                    let name = format!("{} {:03} decoded.png", f.title, first as u64 + n);
                    write_png(dir, &name, &yuv::to_rgb(picture), f.settings.width)?;
                }
            }
            Ok(Output::Pictures(video, started.elapsed()))
        }
        Job::Sound { film } => Ok(Output::Sound((films[film].sound.expect("a film with sound"))())),
    }
}

/// Runs `jobs` on every processor, in order of the list; the results come
/// back in the same order.
fn run_all(films: &[Film], jobs: &[Job], preview: Option<&Path>) -> Result<Vec<Output>, String> {
    let queue = Mutex::new(jobs.iter().copied().enumerate().collect::<VecDeque<_>>());
    let results: Mutex<Vec<Option<Result<Output, String>>>> = Mutex::new((0..jobs.len()).map(|_| None).collect());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(jobs.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let next = queue.lock().expect("the queue").pop_front();
                    let Some((i, job)) = next else { break };
                    let out = run_job(films, job, preview);
                    let failed = out.is_err();
                    results.lock().expect("the results")[i] = Some(out);
                    if failed {
                        // Stop early: nothing more is needed.
                        queue.lock().expect("the queue").clear();
                    }
                }
            });
        }
    });
    results.into_inner().expect("the results").into_iter().map(|r| r.unwrap_or(Err("not run".into()))).collect()
}

/// Puts a film's groups of pictures together.
fn join(groups: Vec<av1::Video>) -> Result<av1::Video, String> {
    let mut groups = groups.into_iter();
    let mut video = groups.next().ok_or("no pictures")?;
    for g in groups {
        // Every group is encoded alike, so the sequence headers agree.
        if g.config != video.config {
            return Err("the groups of pictures have different sequence headers".into());
        }
        video.packets.extend(g.packets);
        video.md5.extend(g.md5);
    }
    Ok(video)
}

/// Builds a film's file from its pictures and sound.
fn mux(film: &Film, video: &av1::Video, sound: Option<&flac::Flac>) -> Result<Vec<u8>, String> {
    let fps = film.settings.fps;
    let frames = video
        .packets
        .iter()
        .enumerate()
        .map(|(i, p)| mkv::Frame { time_ms: (i as u64 * 1000 + fps / 2) / fps, key: p.key, data: &p.data })
        .collect();
    let header = sound.map(|s| s.header());
    let audio = sound.zip(header.as_deref()).map(|(s, header)| mkv::Audio {
        rate: s.rate,
        channels: s.channels as u8,
        bit_depth: 16,
        codec_private: header,
        frames: s
            .frames
            .iter()
            .enumerate()
            .map(|(i, data)| {
                let at = i as u64 * flac::BLOCK as u64;
                mkv::Frame { time_ms: (at * 1000 + s.rate as u64 / 2) / s.rate as u64, key: true, data }
            })
            .collect(),
    });
    let movie = mkv::Movie {
        title: film.title,
        duration_ms: film.seconds() * 1000.0,
        video: mkv::Video {
            width: film.settings.width as u32,
            height: film.settings.height as u32,
            frame_ns: (1_000_000_000 + fps / 2) / fps,
            codec_private: &video.config,
            frames,
        },
        audio,
    };
    mkv::mux(&movie).map_err(|e| format!("{}: {e}", film.file))
}

/// Writes `bytes` to `path` under a temporary name, then renames it.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.part"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {}: {e}", tmp.display()))
}

fn write_png(dir: &Path, name: &str, pixels: &[u32], width: usize) -> Result<(), String> {
    let image = vimage::Image::from_pixels(width as u32, (pixels.len() / width) as u32, pixels.to_vec())
        .map_err(|e| e.to_string())?;
    let png = vimage::encode_png(&image).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    std::fs::write(dir.join(name), png).map_err(|e| format!("writing a preview: {e}"))
}

/// Removes what earlier runs left in a directory this tool owns: files it
/// did not write now, and temporary files.
fn clean(dir: &Path, keep: &[&str]) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && !keep.contains(&name.as_str()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / 1e6
}

fn run(out: &Path, filter: Option<&str>) -> Result<(), String> {
    let started = Instant::now();
    let preview = std::env::var_os("VIDEOGEN_PREVIEW").map(PathBuf::from);
    let films: Vec<Film> = films().into_iter().filter(|f| filter.is_none_or(|w| f.file.contains(w))).collect();
    if films.is_empty() {
        return Err(format!("no video matches {:?}", filter.unwrap_or("")));
    }
    // Remove temporary files of a stopped run first.
    for dir in [VIDEOS, TESTS] {
        let dir = out.join(dir);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".part") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }

    // The largest pictures first, then the sound.
    let mut jobs = Vec::new();
    let mut order: Vec<usize> = (0..films.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(films[i].settings.width * films[i].settings.height));
    for &film in &order {
        let f = &films[film];
        let group = f.settings.key_interval as u32;
        for first in (0..f.frames).step_by(group as usize) {
            jobs.push(Job::Pictures { film, first, count: group.min(f.frames - first) });
        }
    }
    for (film, f) in films.iter().enumerate() {
        if f.sound.is_some() {
            jobs.push(Job::Sound { film });
        }
    }
    let mut outputs = run_all(&films, &jobs, preview.as_deref())?.into_iter();

    let mut groups: Vec<Vec<av1::Video>> = films.iter().map(|_| Vec::new()).collect();
    let mut busy: Vec<Duration> = vec![Duration::ZERO; films.len()];
    let mut sounds: Vec<Option<Vec<i16>>> = films.iter().map(|_| None).collect();
    for (job, out) in jobs.iter().zip(outputs.by_ref()) {
        match (job, out) {
            (Job::Pictures { film, .. }, Output::Pictures(v, took)) => {
                groups[*film].push(v);
                busy[*film] += took;
            }
            (Job::Sound { film }, Output::Sound(pcm)) => sounds[*film] = Some(pcm),
            _ => return Err("a job's output does not match it".into()),
        }
    }

    let mut written = 0;
    let mut total = 0;
    let mut countdown: Option<(Vec<u8>, Vec<[u8; 16]>)> = None;
    for (i, film) in films.iter().enumerate() {
        let video = join(std::mem::take(&mut groups[i]))?;
        if video.packets.len() != film.frames as usize {
            return Err(format!("{}: {} pictures of {}", film.file, video.packets.len(), film.frames));
        }
        let sound = sounds[i].take().map(|pcm| flac::encode(&pcm, 2, RATE));
        let bytes = mux(film, &video, sound.as_ref())?;
        write_atomically(&out.join(VIDEOS).join(film.file), &bytes)?;
        if let Some(dir) = &preview {
            let text: String = video.md5.iter().map(|d| format!("{}\n", md5::hex(d))).collect();
            std::fs::write(dir.join(format!("{}.md5", film.title)), text)
                .map_err(|e| format!("writing a preview: {e}"))?;
        }
        let picture_bytes: usize = video.packets.iter().map(|p| p.data.len()).sum();
        let sound_text = match &sound {
            Some(s) => format!("FLAC {:4.2} MB", mb(s.frames.iter().map(|f| f.len()).sum())),
            None => "no sound    ".into(),
        };
        println!(
            "  {:<38} {:>4}x{:<4} {} fps {:4.1} s  AV1 {:4.2} MB  {}  {:5.2} MB  ({:.1} s encoding)",
            format!("{VIDEOS}/{}", film.file),
            film.settings.width,
            film.settings.height,
            film.settings.fps,
            film.seconds(),
            mb(picture_bytes),
            sound_text,
            mb(bytes.len()),
            busy[i].as_secs_f64()
        );
        written += 1;
        total += bytes.len();
        if film.file == "Countdown.mkv" {
            countdown = Some((bytes, video.md5));
        }
    }

    if let Some((bytes, md5)) = countdown {
        let text: String = md5.iter().map(|d| format!("{}\n", md5::hex(d))).collect();
        write_atomically(&out.join(TESTS).join("Countdown.md5"), text.as_bytes())?;
        println!("  {:<38} {} pictures", format!("{TESTS}/Countdown.md5"), md5.len());
        let truncated = &bytes[..bytes.len() * 3 / 5];
        write_atomically(&out.join(TESTS).join("Truncated.mkv"), truncated)?;
        println!("  {:<38} 60% of Countdown.mkv  {:5.2} MB", format!("{TESTS}/Truncated.mkv"), mb(truncated.len()));
        written += 2;
        total += text.len() + truncated.len();
    }
    if filter.is_none() {
        let names: Vec<&str> = films.iter().map(|f| f.file).collect();
        clean(&out.join(VIDEOS), &names);
        clean(&out.join(TESTS), &["Countdown.md5", "Truncated.mkv"]);
    }
    println!("videogen: {written} files, {:.2} MB in {:.1} s", mb(total), started.elapsed().as_secs_f64());
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(out) = args.next().map(PathBuf::from) else {
        eprintln!("usage: videogen OUT_DIR [FILTER]");
        std::process::exit(2);
    };
    let filter = args.next();
    if let Err(e) = run(&out, filter.as_deref()) {
        eprintln!("videogen: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mkv::{id, read};

    /// A tiny film through the whole pipeline: groups of pictures encoded on
    /// the pool and joined, its sound, and the file read back.
    #[test]
    fn a_tiny_film_parses_back() {
        let films = [Film {
            file: "Tiny.mkv",
            title: "Tiny",
            settings: av1::Settings { width: 64, height: 48, fps: 30, key_interval: 10, quantizer: 120, speed: SPEED },
            frames: 25,
            picture: Box::new(|f| {
                (0..64 * 48u32).map(|i| 0xFF00_0000 | ((i % 64 * 4 + f * 3) & 0xFF) << 16 | (i / 64 * 5) << 8).collect()
            }),
            sound: Some(|| (0..2 * 40_000).map(|i| ((i as f32 * 0.013).sin() * 4000.0) as i16).collect()),
        }];
        let jobs = [
            Job::Pictures { film: 0, first: 0, count: 10 },
            Job::Pictures { film: 0, first: 10, count: 10 },
            Job::Pictures { film: 0, first: 20, count: 5 },
            Job::Sound { film: 0 },
        ];
        let mut groups = Vec::new();
        let mut pcm = Vec::new();
        for out in run_all(&films, &jobs, None).unwrap() {
            match out {
                Output::Pictures(v, _) => groups.push(v),
                Output::Sound(s) => pcm = s,
            }
        }
        let video = join(groups).unwrap();
        assert_eq!(video.md5.len(), 25);
        let sound = flac::encode(&pcm, 2, RATE);
        let file = mux(&films[0], &video, Some(&sound)).unwrap();

        let top = read::children(&file).unwrap();
        assert_eq!(top.iter().map(|e| e.id).collect::<Vec<_>>(), [id::EBML, id::SEGMENT]);
        let segment = top[1].children();
        let info = segment.iter().find(|e| e.id == id::INFO).unwrap();
        assert!((info.child(id::DURATION).float() - 2500.0 / 3.0).abs() < 1e-6);
        assert_eq!(info.child(id::TITLE).string(), "Tiny");
        let tracks = segment.iter().find(|e| e.id == id::TRACKS).unwrap().children();
        assert_eq!(tracks[0].child(id::CODEC_PRIVATE).data, video.config);
        assert_eq!(tracks[0].child(id::DEFAULT_DURATION).uint(), 33_333_333);
        assert_eq!(tracks[1].child(id::CODEC_PRIVATE).data, sound.header());

        let clusters: Vec<_> = segment.iter().filter(|e| e.id == id::CLUSTER).collect();
        let starts: Vec<u64> = clusters.iter().map(|c| c.child(id::TIMESTAMP).uint()).collect();
        assert_eq!(starts, [0, 333, 667]);
        let (mut pictures, mut flac_file) = (Vec::new(), sound.header());
        let mut audio_times = Vec::new();
        for c in &clusters {
            let start = c.child(id::TIMESTAMP).uint() as i64;
            for b in c.children().iter().filter(|e| e.id == id::SIMPLE_BLOCK) {
                let (track, rel, flags, data) = read::simple_block(b.data);
                let t = start + rel as i64;
                if track == 1 {
                    let n = pictures.len() as i64;
                    assert_eq!(t, (n * 1000 + 15) / 30, "picture {n}'s time");
                    assert_eq!(flags & 0x80 != 0, n % 10 == 0, "picture {n}'s key flag");
                    pictures.push(data);
                } else {
                    assert_eq!(flags, 0x80);
                    audio_times.push(t);
                    flac_file.extend_from_slice(data);
                }
            }
        }
        let packets: Vec<&[u8]> = video.packets.iter().map(|p| p.data.as_slice()).collect();
        assert_eq!(pictures, packets);
        // 40000 samples: ten blocks, 85.3 ms apart.
        assert_eq!(audio_times, (0..10).map(|j| (j * 4096 * 1000 + 24_000) / 48_000).collect::<Vec<i64>>());
        assert_eq!(flac::decode::decode(&flac_file).unwrap().pcm, pcm);
        let cues = segment.iter().find(|e| e.id == id::CUES).unwrap().children();
        assert_eq!(cues.len(), 3);
    }

    #[test]
    fn files_appear_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("videogen-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Some File.mkv");
        write_atomically(&path, b"first").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["Some File.mkv"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
