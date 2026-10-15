// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Interceptor's music: loops for the menus, for driving about and for
//! the chase, written to `OUT_DIR/games/interceptor/` (QOA, 44.1 kHz).
//!
//! Each loop is composed twice over and rendered, and the second time
//! through is kept: it begins with the echoes of the first time's end, so
//! played round and round it never shows a seam. The chase's music comes
//! in two stems mastered at the same gain: its drums, bass, pads and arp,
//! and on top the lead, the stabs and the extra percussion, which the game
//! brings in as the suspect is caught up with.
//!
//! | File | Tune |
//! |------|------|
//! | `theme.qoa` | "Interceptor": D minor, 108 BPM, the menus |
//! | `patrol.qoa` | "Night Shift": F minor, 96 BPM, driving about |
//! | `pursuit.qoa`, `pursuit-high.qoa` | "Hot Pursuit": E minor, 140 BPM, a chase, in two stems |

use gencommon::output::Output;

use vaudio::synth::{self, Part, Song};

use crate::dsl::{self, BAR, Humanize, key};
use crate::patches::{self, SR};

/// Every loop is this many bars.
const BARS: f64 = 32.0;
/// The beats of one time through.
const LOOP: f64 = BARS * BAR;

/// Each time through: the start beat of each of the two.
const TIMES: [f64; 2] = [0.0, LOOP];

/// Automation points `(bar, value)` for both times through.
fn twice(points: &[(f64, f32)]) -> Vec<(f64, f32)> {
    TIMES.iter().flat_map(|&o| points.iter().map(move |&(b, v)| (o + b * BAR, v))).collect()
}

/// A song of `parts` playing the loop twice at `bpm`.
fn song(bpm: f64, parts: Vec<Part>, duck: Vec<f64>) -> Song {
    let mut s = Song::new(bpm, SR, 2.0 * LOOP);
    s.tail = 0.0;
    s.fade_out = 0.0;
    s.parts = parts;
    s.duck_times = duck;
    s.master.target_rms_db = -18.0;
    s
}

/// The second time through of a song playing its loop twice.
fn second_time(song: &Song) -> (Vec<f32>, synth::seq::MixStats) {
    let (mix, stats) = synth::render(song);
    let frames = (LOOP * song.seconds_per_beat() * song.sr as f64).round() as usize;
    (mix[frames * 2..frames * 4].to_vec(), stats)
}

// ---- "Interceptor": the theme -------------------------------------------

const THEME_INTRO: &[&str] = &["Dm", "Bb", "F", "C", "Dm", "Bb", "Gm", "A"];
const THEME_CHORUS: &[&str] = &["Bb", "C", "Dm", "Dm", "Bb", "C", "A", "A"];
const THEME_CHORUS2: &[&str] = &["Bb", "C", "Dm", "F", "Gm", "Bb", "A", "A"];

const THEME_HOOK: &str = "D5/1 F5/0.5 A5/1.5! G5/1 \
    E5/1 G5/0.5 C6/1.5! A5/1 \
    A5/2! F5/1 D5/1 \
    E5/1.5 F5/0.5 D5/2 \
    D5/1 F5/0.5 A5/1.5! Bb5/1 \
    C6/1.5! Bb5/0.5 A5/1 G5/1 \
    A5/2 C#6/1! E6/1 \
    D6/1 C#6/1 A5/2";
const THEME_HOOK2: &str = "F5/1 A5/0.5 D6/1.5! C6/1 \
    G5/1 C6/0.5 E6/1.5! D6/1 \
    D6/2! A5/1 F5/1 \
    A5/1.5 C6/0.5 A5/2 \
    Bb5/1 A5/0.5 G5/1.5 D5/1 \
    F5/1 G5/0.5 A5/1.5! Bb5/1 \
    C#6/2! E6/1 C#6/1 \
    A5/2 -/2";

fn theme() -> Song {
    let mut h = Humanize::new(108, 0.0, 0.05);
    let mut dr = Part::new("drums", Box::new(patches::kit_80s()));
    let mut bass = Part::new("bass", Box::new(patches::synth_bass(600.0)));
    let mut arp = Part::new("arp", Box::new(patches::pluck_synth(1100.0, 0.18)));
    let mut pad = Part::new("pad", Box::new(patches::supersaw_pad(2400.0)));
    let mut lead = Part::new("lead", Box::new(patches::saw_lead(3600.0)));
    let mut bells = Part::new("bells", Box::new(patches::bell(3.5, 1.4)));
    for o in TIMES {
        let ob = o / BAR;
        // The intro: pads, the arp opening, a heartbeat of a kick.
        dsl::drums(&mut dr, key::KICK, "X.......X.......", ob + 4.0, 3, 0.0, &mut h);
        dsl::drums(&mut dr, key::KICK, "X.......X...X.X.", ob + 7.0, 1, 0.0, &mut h);
        dsl::drums(&mut dr, key::SNARE, "........x.x.xxXX", ob + 7.0, 1, 0.0, &mut h);
        dr.note(o + 5.0 * BAR, 12.0, key::FX, 0.8);
        // The verse.
        dsl::drums(&mut dr, key::KICK, "X.......x.....x.", ob + 8.0, 7, 0.0, &mut h);
        dsl::drums(&mut dr, key::SNARE, "....X.......X...", ob + 8.0, 7, 0.0, &mut h);
        dsl::drums(&mut dr, key::HAT, "x.x.x.x.x.x.x.x.", ob + 8.0, 8, 0.0, &mut h);
        dsl::drums(&mut dr, key::KICK, "X.......X.......", ob + 15.0, 1, 0.0, &mut h);
        dsl::drums(&mut dr, key::TOM_HI, "........x.x.....", ob + 15.0, 1, 0.0, &mut h);
        dsl::drums(&mut dr, key::TOM_LO, "............x.x.", ob + 15.0, 1, 0.0, &mut h);
        // The choruses.
        for (start, n) in [(16.0, 7usize), (24.0, 7)] {
            dsl::drums(&mut dr, key::KICK, "X...x...X...x...", ob + start, n + 1, 0.0, &mut h);
            dsl::drums(&mut dr, key::SNARE, "....X.......X...", ob + start, n, 0.0, &mut h);
            dsl::drums(&mut dr, key::HAT, "xoxoxoxoxoxoxoxo", ob + start, n + 1, 0.0, &mut h);
            dsl::drums(&mut dr, key::OPEN_HAT, "..x...x...x...x.", ob + start, n + 1, 0.0, &mut h);
            dsl::drums(&mut dr, key::SNARE, "....X.......X.xx", ob + start + n as f64, 1, 0.0, &mut h);
            dr.note(o + start * BAR, 4.0, key::CRASH, 1.0);
        }
        dr.note(o + 8.0 * BAR, 4.0, key::CRASH, 0.85);
        dr.note(o, 4.0, key::CRASH, 0.7);

        dsl::bass(&mut bass, o + 4.0 * BAR, &THEME_INTRO[4..], BAR, "R---------------", 2, 0.75, 0.0, &mut h);
        dsl::bass(&mut bass, o + 8.0 * BAR, THEME_INTRO, BAR, "R-r-R-r-R-r-R-r-", 2, 0.9, 0.0, &mut h);
        dsl::bass(&mut bass, o + 16.0 * BAR, THEME_CHORUS, BAR, "R-rRR-rRR-rRR-rR", 2, 0.95, 0.0, &mut h);
        dsl::bass(&mut bass, o + 24.0 * BAR, THEME_CHORUS2, BAR, "R-rRR-rRR-rRR-rR", 2, 0.95, 0.0, &mut h);

        let pat = [0usize, 1, 2, 3, 2, 1, 2, 3];
        dsl::arp(&mut arp, o + 2.0 * BAR, &THEME_INTRO[2..], BAR, 0.25, &pat, 62.0, 0.7, &mut h);
        dsl::arp(&mut arp, o + 8.0 * BAR, THEME_INTRO, BAR, 0.25, &pat, 64.0, 0.75, &mut h);
        dsl::arp(&mut arp, o + 16.0 * BAR, THEME_CHORUS, BAR, 0.25, &pat, 66.0, 0.7, &mut h);
        dsl::arp(&mut arp, o + 24.0 * BAR, THEME_CHORUS2, BAR, 0.25, &pat, 66.0, 0.7, &mut h);

        dsl::pads(&mut pad, o, &dsl::repeat(THEME_INTRO, 2), BAR, 60.0, 0.8);
        dsl::pads(&mut pad, o + 16.0 * BAR, THEME_CHORUS, BAR, 62.0, 0.85);
        dsl::pads(&mut pad, o + 24.0 * BAR, THEME_CHORUS2, BAR, 62.0, 0.85);

        dsl::melody(&mut lead, o + 16.0 * BAR, THEME_HOOK, 0.85, &mut h);
        dsl::melody(&mut lead, o + 24.0 * BAR, THEME_HOOK2, 0.9, &mut h);
        dsl::melody(&mut bells, o + 24.0 * BAR, &dsl::transpose(THEME_HOOK2, -12), 0.55, &mut h);
    }
    arp.lowpass = twice(&[(0.0, 300.0), (2.0, 400.0), (8.0, 5500.0), (31.5, 5500.0), (32.0, 300.0)]);
    arp.pan = 0.3;
    arp.delay = 0.3;
    arp.reverb = 0.2;
    arp.duck = 0.25;
    arp.gain = 1.1;
    pad.lowpass = twice(&[(0.0, 500.0), (8.0, 3200.0), (31.0, 3200.0), (32.0, 500.0)]);
    pad.chorus = Some((0.4, 3.0, 0.4));
    pad.reverb = 0.35;
    pad.duck = 0.45;
    pad.gain = 0.75;
    bass.duck = 0.35;
    lead.delay = 0.25;
    lead.reverb = 0.28;
    lead.gain = 1.6;
    bells.pan = -0.25;
    bells.reverb = 0.4;
    bells.gain = 0.6;
    dr.reverb = 0.18;
    let duck = crate::songs::hits_of(&dr, key::KICK);
    let mut s = song(108.0, vec![dr, bass, arp, pad, lead, bells], duck);
    s.reverb = (0.8, 0.45, 1.0, 0.025);
    s.reverb_return = 0.85;
    s.delay_beats = 0.75;
    s.delay_feedback = 0.35;
    s
}

// ---- "Night Shift": driving about ---------------------------------------

const PATROL: &[&str] = &["Fm9", "Dbmaj7", "Ab", "Eb"];

const PATROL_TUNE: &str = "C5/1.5 Ab4/0.5 C5/1 Eb5/1 \
    F5/2 Eb5/1 C5/1 \
    Eb5/1.5 C5/0.5 Ab4/2 \
    Bb4/1 G4/1 Bb4/1 C5/1 \
    C5/1.5 Ab4/0.5 C5/1 F5/1! \
    Ab5/2 G5/1 F5/1 \
    Eb5/1.5 F5/0.5 C5/2 \
    Bb4/3 -/1";

fn patrol() -> Song {
    let mut h = Humanize::new(96, 0.008, 0.08);
    let swing = 0.12;
    let mut dr = Part::new("drums", Box::new(patches::kit_lofi()));
    let mut bass = Part::new("bass", Box::new(patches::warm_bass()));
    let mut keys = Part::new("keys", Box::new(patches::epiano()));
    let mut pad = Part::new("pad", Box::new(patches::soft_pad(1800.0)));
    let mut lead = Part::new("lead", Box::new(patches::soft_lead()));
    let mut bells = Part::new("bells", Box::new(patches::glock()));
    for o in TIMES {
        let ob = o / BAR;
        for (start, n) in [(4.0, 12usize), (20.0, 12)] {
            dsl::drums(&mut dr, key::KICK, "X......x..X.....", ob + start, n, swing, &mut h);
            dsl::drums(&mut dr, key::SNARE, "....X.......X...", ob + start, n, swing, &mut h);
            dsl::drums(&mut dr, key::HAT, "x.xox.x.x.xox.xo", ob + start, n, swing, &mut h);
            dsl::drums(&mut dr, key::SHAKER, "..x...x...x...x.", ob + start + 4.0, n - 4, swing, &mut h);
        }
        // The break: rim clicks only.
        dsl::drums(&mut dr, key::RIM, "....x.......x...", ob + 16.0, 4, swing, &mut h);
        dsl::bass(&mut bass, o + 4.0 * BAR, &dsl::repeat(PATROL, 3), BAR, "R---..r-R---..5-", 2, 0.85, swing, &mut h);
        dsl::bass(&mut bass, o + 20.0 * BAR, &dsl::repeat(PATROL, 3), BAR, "R---..r-R---..5-", 2, 0.85, swing, &mut h);
        dsl::comp(&mut keys, o, &dsl::repeat(PATROL, 8), BAR, "x--.x--..x-.x---", 62.0, 0.6, swing, &mut h);
        dsl::pads(&mut pad, o, &dsl::repeat(PATROL, 8), BAR, 60.0, 0.6);
        dsl::melody(&mut lead, o + 8.0 * BAR, PATROL_TUNE, 0.75, &mut h);
        dsl::melody(&mut lead, o + 24.0 * BAR, PATROL_TUNE, 0.8, &mut h);
        dsl::melody(&mut bells, o + 16.0 * BAR, &dsl::transpose(PATROL_TUNE, 12), 0.45, &mut h);
        dsl::melody(&mut bells, o + 24.0 * BAR, &dsl::transpose(PATROL_TUNE, 12), 0.3, &mut h);
    }
    dr.gain = 0.9;
    dr.reverb = 0.12;
    bass.gain = 1.1;
    keys.reverb = 0.3;
    keys.delay = 0.15;
    keys.pan = -0.15;
    keys.gain = 1.1;
    pad.reverb = 0.45;
    pad.gain = 0.7;
    lead.delay = 0.3;
    lead.reverb = 0.35;
    lead.gain = 1.3;
    bells.reverb = 0.5;
    bells.delay = 0.3;
    bells.pan = 0.3;
    bells.gain = 0.6;
    let duck = crate::songs::hits_of(&dr, key::KICK);
    let mut s = song(96.0, vec![dr, bass, keys, pad, lead, bells], duck);
    s.reverb = (0.85, 0.55, 1.0, 0.03);
    s.reverb_return = 0.9;
    s.delay_beats = 0.75;
    s.delay_feedback = 0.4;
    s.duck_release = 0.2;
    s
}

// ---- "Hot Pursuit": the chase -------------------------------------------

/// Two bars each, then a bar each.
const CHASE_A: &[&str] = &["Em", "Em", "C", "C", "D", "D", "B", "B"];
const CHASE_B: &[&str] = &["Em", "C", "D", "B", "Em", "C", "Am", "B"];
const CHASE_C: &[&str] = &["C", "D", "Em", "Em", "C", "D", "B", "B"];
const CHASE_D: &[&str] = &["Am", "B", "Em", "C", "Am", "B", "Em", "B"];

const CHASE_RIFF: &str = "E5/0.75 G5/0.75 B5/0.5 A5/1 G5/1 \
    E5/0.75 G5/0.75 C6/0.5! B5/1 G5/1 \
    F#5/0.75 A5/0.75 D6/0.5! C6/1 A5/1 \
    B5/1.5 A5/0.5 G5/1 F#5/1 \
    E5/0.75 G5/0.75 B5/0.5 E6/1! D6/1 \
    C6/0.75 B5/0.75 G5/0.5 E5/2 \
    A5/0.75 C6/0.75 E6/0.5! D6/1 C6/1 \
    B5/2 D#6/2!";
const CHASE_HOOK: &str = "G5/1.5 E5/0.5 G5/1 C6/1! \
    A5/1.5 F#5/0.5 A5/1 D6/1! \
    B5/2! A5/1 G5/1 \
    E5/3 -/1 \
    G5/1.5 E5/0.5 G5/1 C6/1! \
    D6/1.5 C6/0.5 B5/1 A5/1 \
    B5/2! F#5/1 D#5/1 \
    F#5/2 B5/2!";
const CHASE_RIFF2: &str = "A5/0.75 C6/0.75 E6/0.5! D6/1 C6/1 \
    B5/0.75 D#6/0.75 F#6/0.5! E6/1 D#6/1 \
    E6/2! B5/1 G5/1 \
    C6/1.5 B5/0.5 G5/1 E5/1 \
    A5/0.75 C6/0.75 E6/0.5 D6/1 C6/1 \
    D#6/1.5 F#6/0.5! E6/1 D#6/1 \
    E6/1 B5/1 G5/1 E5/1 \
    F#5/1 A5/1 B5/1 D#6/1";

/// The chase's two stems: what always plays, and what joins it.
fn pursuit() -> (Vec<Part>, Vec<Part>, Vec<f64>) {
    let mut h = Humanize::new(140, 0.0, 0.05);
    let mut dr = Part::new("drums", Box::new(patches::kit_80s()));
    let mut bass = Part::new("bass", Box::new(patches::synth_bass(900.0)));
    let mut arp = Part::new("arp", Box::new(patches::pluck_synth(1500.0, 0.12)));
    let mut pad = Part::new("pad", Box::new(patches::supersaw_pad(1800.0)));
    let mut perc = Part::new("perc", Box::new(patches::kit_house()));
    let mut lead = Part::new("lead", Box::new(patches::saw_lead(4200.0)));
    let mut stabs = Part::new("stabs", Box::new(patches::supersaw_lead(3000.0)));
    let mut bells = Part::new("bells", Box::new(patches::bell(3.5, 1.0)));
    let sections = [(0.0, CHASE_A), (8.0, CHASE_B), (16.0, CHASE_C), (24.0, CHASE_D)];
    for o in TIMES {
        let ob = o / BAR;
        // The drums drive throughout; a fill closes each section.
        dsl::drums(&mut dr, key::KICK, "X...x...X...x...", ob, 32, 0.0, &mut h);
        for (start, _) in sections {
            dsl::drums(&mut dr, key::SNARE, "....X.......X...", ob + start, 7, 0.0, &mut h);
            dsl::drums(&mut dr, key::SNARE, "....X.......XxXX", ob + start + 7.0, 1, 0.0, &mut h);
            dsl::drums(&mut dr, key::HAT, "xoxoxoxoxoxoxoxo", ob + start, 8, 0.0, &mut h);
            dr.note(o + start * BAR, 4.0, key::CRASH, if start == 0.0 { 0.8 } else { 0.95 });
        }
        // The bass rolls in sixteenths; the arp spins over the pads.
        for (start, chords) in sections {
            let (beats, list): (f64, Vec<&str>) = if start == 0.0 {
                (2.0 * BAR, chords.iter().step_by(2).copied().collect())
            } else {
                (BAR, chords.to_vec())
            };
            dsl::bass(&mut bass, o + start * BAR, &list, beats, "RrRrRrRrRrRrRrRr", 2, 0.9, 0.0, &mut h);
            dsl::arp(&mut arp, o + start * BAR, &list, beats, 0.25, &[0, 2, 1, 2, 3, 2, 1, 2], 64.0, 0.7, &mut h);
            dsl::pads(&mut pad, o + start * BAR, &list, beats, 60.0, 0.75);
        }
        // The heat: claps, open hats, toms; the lead and its stabs.
        for (start, _) in sections {
            dsl::drums(&mut perc, key::CLAP, "....X.......X...", ob + start, 8, 0.0, &mut h);
            dsl::drums(&mut perc, key::OPEN_HAT, "..x...x...x...x.", ob + start, 8, 0.0, &mut h);
            dsl::drums(&mut perc, key::SHAKER, "xxxxxxxxxxxxxxxx", ob + start, 8, 0.0, &mut h);
        }
        dsl::drums(&mut dr, key::TOM_HI, "........x.x.....", ob + 15.0, 1, 0.0, &mut h);
        dsl::drums(&mut dr, key::TOM_LO, "............x.x.", ob + 15.0, 1, 0.0, &mut h);
        dsl::comp(
            &mut stabs,
            o,
            &CHASE_A.iter().step_by(2).copied().collect::<Vec<_>>(),
            2.0 * BAR,
            "X..x..x.....x...",
            64.0,
            0.7,
            0.0,
            &mut h,
        );
        dsl::comp(&mut stabs, o + 16.0 * BAR, CHASE_C, BAR, "x..x..x...x.x...", 64.0, 0.6, 0.0, &mut h);
        dsl::melody(&mut lead, o + 8.0 * BAR, CHASE_RIFF, 0.85, &mut h);
        dsl::melody(&mut lead, o + 16.0 * BAR, CHASE_HOOK, 0.9, &mut h);
        dsl::melody(&mut lead, o + 24.0 * BAR, CHASE_RIFF2, 0.9, &mut h);
        dsl::melody(&mut bells, o + 16.0 * BAR, &dsl::transpose(CHASE_HOOK, -12), 0.5, &mut h);
    }
    dr.reverb = 0.12;
    dr.gain = 1.05;
    bass.duck = 0.4;
    bass.gain = 1.0;
    arp.lowpass = twice(&[(0.0, 900.0), (8.0, 3500.0), (16.0, 2500.0), (24.0, 5000.0), (32.0, 900.0)]);
    arp.pan = -0.25;
    arp.delay = 0.2;
    arp.duck = 0.3;
    arp.gain = 1.0;
    pad.chorus = Some((0.5, 2.5, 0.35));
    pad.reverb = 0.3;
    pad.duck = 0.55;
    pad.gain = 0.6;
    perc.gain = 0.8;
    perc.reverb = 0.15;
    lead.delay = 0.2;
    lead.reverb = 0.22;
    lead.gain = 1.7;
    stabs.duck = 0.3;
    stabs.reverb = 0.25;
    stabs.pan = 0.2;
    stabs.gain = 1.4;
    bells.reverb = 0.4;
    bells.pan = -0.3;
    bells.gain = 0.55;
    let duck = crate::songs::hits_of(&dr, key::KICK);
    (vec![dr, bass, arp, pad], vec![perc, lead, stabs, bells], duck)
}

/// Writes `mix` as the QOA file `name` of the game's.
fn write(out: &mut Output, name: &str, mix: &[f32], seed: u64) -> Result<usize, String> {
    let pcm = crate::to_i16(mix, seed);
    let file = vaudio::qoa::encode(&pcm, 2, SR as u32).map_err(|e| format!("QOA encoding: {e}"))?;
    out.write(&format!("games/interceptor/{name}"), &file)?;
    Ok(file.len())
}

/// Renders and writes every loop to `games/interceptor/` of `out`; returns
/// the bytes written.
pub fn write_all(out: &mut Output) -> Result<usize, String> {
    let mut bytes = 0;
    let report = |name: &str, stats: &synth::seq::MixStats, size: usize| {
        println!(
            "    {:<18} peak {:5.1} dBFS  loudness {:5.1} dB  gain {:+5.1} dB  {:5.2} MB",
            name,
            stats.peak_db,
            stats.rms_db,
            stats.gain_db,
            size as f64 / 1e6
        );
    };
    for (name, s, seed) in [("theme.qoa", theme(), 31u64), ("patrol.qoa", patrol(), 37)] {
        let (mix, stats) = second_time(&s);
        let size = write(out, name, &mix, seed)?;
        report(name, &stats, size);
        bytes += size;
    }
    // The chase: the whole mix sets the gain both stems are mastered at.
    let (base, high, duck) = pursuit();
    let mut whole = song(140.0, base.into_iter().chain(high).collect(), duck.clone());
    whole.master.target_rms_db = -16.5;
    let (_, full) = synth::render(&whole);
    let (base, high, _) = pursuit();
    for (name, parts, seed) in [("pursuit.qoa", base, 41u64), ("pursuit-high.qoa", high, 43)] {
        let mut s = song(140.0, parts, duck.clone());
        s.master.gain_db = Some(full.gain_db);
        let (mix, stats) = second_time(&s);
        let size = write(out, name, &mix, seed)?;
        report(name, &stats, size);
        bytes += size;
    }
    Ok(bytes)
}
