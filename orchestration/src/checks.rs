// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the automation scripts check, apart from the machine: screenshots
//! (alike, black), QEMU's recording of the sound output (its level, its
//! dropouts), and the agent's function results (JSON).

use std::path::Path;

use vaudio::wav::{SampleFormat, WavInfo};

use crate::script::Region;
use crate::util::{self, Result};

/// The screenshot at `path`, decoded.
fn picture(path: &Path) -> Result<vimage::Image> {
    let data = util::read(path)?;
    vimage::decode(&data).map_err(|e| format!("decoding {}: {e:?}", path.display()))
}

/// The pixels of `region` (fractions of the screen) in a picture of
/// `width` by `height`.
fn pixels(region: Region, width: u32, height: u32) -> impl Iterator<Item = (u32, u32)> {
    let (w, h) = (width as f64, height as f64);
    let x = (region[0] * w) as u32..((region[2] * w) as u32).min(width);
    let y = (region[1] * h) as u32..((region[3] * h) as u32).min(height);
    y.flat_map(move |y| x.clone().map(move |x| (x, y)))
}

/// Fails unless the screenshots `a` and `b` are alike, pixel for pixel, in
/// `region`.
pub fn same_pictures(a: &Path, b: &Path, region: Region) -> Result {
    let (first, second) = (picture(a)?, picture(b)?);
    if (first.width, first.height) != (second.width, second.height) {
        return Err(format!("{} and {} differ in size", a.display(), b.display()));
    }
    let mut differing = pixels(region, first.width, first.height)
        .filter(|&(x, y)| {
            let i = (y * first.width + x) as usize;
            first.pixels[i] != second.pixels[i]
        })
        .peekable();
    match differing.peek().copied() {
        None => Ok(()),
        Some((x, y)) => Err(format!(
            "{} and {} differ in {} pixels, the first at ({x}, {y})",
            a.display(),
            b.display(),
            differing.count()
        )),
    }
}

/// Fails unless the screenshot `p` is black in `region`.
pub fn black_picture(p: &Path, region: Region) -> Result {
    let picture = picture(p)?;
    let mut lit = pixels(region, picture.width, picture.height)
        .filter(|&(x, y)| picture.pixels[(y * picture.width + x) as usize] & 0xFF_FFFF != 0)
        .peekable();
    match lit.peek().copied() {
        None => Ok(()),
        Some((x, y)) => {
            Err(format!("{} is not black: {} pixels are lit, the first at ({x}, {y})", p.display(), lit.count()))
        }
    }
}

/// The samples of QEMU's recording of the sound output (`audio.wav`), 16-bit
/// PCM: the data, the channels and the rate. QEMU leaves the data chunk's
/// length at zero while it records; that means "to the end".
fn recorded_pcm(wav: &[u8]) -> Result<(&[u8], usize, u32)> {
    let info = WavInfo::parse(wav).map_err(|e| format!("audio.wav: {e}"))?;
    if info.format != SampleFormat::S16 {
        return Err("audio.wav is not 16-bit PCM".into());
    }
    if info.channels == 0 {
        return Err("audio.wav has no channels".into());
    }
    let data = wav.get(info.data_offset..info.data_offset + info.data_len).ok_or("audio.wav is cut short")?;
    Ok((data, info.channels as usize, info.rate))
}

/// The loudest sample of QEMU's recording of the sound output.
pub fn recorded_peak(wav: &[u8]) -> Result<u16> {
    let (pcm, ..) = recorded_pcm(wav)?;
    Ok(pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b).unsigned_abs()).max().unwrap_or(0))
}

/// The stretches of digital silence (every channel exactly zero) longer
/// than `limit_ms` in QEMU's recording of the sound output, between its
/// first and last sound: where (s) and how long (ms).
pub fn silent_gaps(wav: &[u8], limit_ms: f64) -> Result<Vec<(f64, f64)>> {
    let (data, channels, rate) = recorded_pcm(wav)?;
    let frames: Vec<&[u8]> = data.chunks_exact(2 * channels).collect();
    let silent = |f: &[u8]| f.iter().all(|&b| b == 0);
    let loud = |f: &[u8]| f.as_chunks::<2>().0.iter().any(|s| i16::from_le_bytes(*s).unsigned_abs() > 64);
    let (Some(first), Some(last)) = (frames.iter().position(|f| loud(f)), frames.iter().rposition(|f| loud(f))) else {
        return Ok(Vec::new());
    };
    let mut gaps = Vec::new();
    let mut run = 0usize;
    for (i, f) in frames.iter().enumerate().take(last + 1).skip(first) {
        if silent(f) {
            run += 1;
            continue;
        }
        let ms = run as f64 * 1000.0 / rate as f64;
        if ms > limit_ms {
            gaps.push(((i - run) as f64 / rate as f64, ms));
        }
        run = 0;
    }
    Ok(gaps)
}

/// Whether a function's result (JSON) has what a script expects of it: the
/// members it writes as JSON (`"page":"processes","shown":1`), all in one
/// object of the result and in any order (an object's members have none).
/// Values are compared as JSON's: numbers by value (`2` is `2.0`), objects
/// by the members given, arrays element by element; or else, the text
/// itself is looked for (a word, a key, the start of a value).
pub fn result_has(result: &str, expected: &str) -> bool {
    use serde_json::{Map, Value};
    /// Whether `actual` is `expected`, as described above.
    fn matches(actual: &Value, expected: &Value) -> bool {
        match (actual, expected) {
            (Value::Object(a), Value::Object(e)) => has_all(a, e),
            (Value::Array(a), Value::Array(e)) => a.len() == e.len() && a.iter().zip(e).all(|(a, e)| matches(a, e)),
            (Value::Number(a), Value::Number(e)) => a.as_f64() == e.as_f64(),
            _ => actual == expected,
        }
    }
    fn has_all(object: &Map<String, Value>, members: &Map<String, Value>) -> bool {
        members.iter().all(|(k, v)| object.get(k).is_some_and(|x| matches(x, v)))
    }
    /// Whether `value`, or a value in it, is an object with `members`.
    fn holds(value: &Value, members: &Map<String, Value>) -> bool {
        match value {
            Value::Object(o) => has_all(o, members) || o.values().any(|v| holds(v, members)),
            Value::Array(a) => a.iter().any(|v| holds(v, members)),
            _ => false,
        }
    }
    match (serde_json::from_str::<Map<String, Value>>(&format!("{{{expected}}}")), serde_json::from_str(result)) {
        (Ok(members), Ok(value)) => holds(&value, &members),
        _ => result.contains(expected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16], data_len: Option<u32>) -> Vec<u8> {
        let mut w = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        w.extend_from_slice(&16u32.to_le_bytes());
        // PCM, 2 channels, 1000 Hz, 4000 bytes/s, 4 bytes/frame, 16 bits.
        for v in [1u16, 2] {
            w.extend_from_slice(&v.to_le_bytes());
        }
        w.extend_from_slice(&1000u32.to_le_bytes());
        w.extend_from_slice(&4000u32.to_le_bytes());
        for v in [4u16, 16] {
            w.extend_from_slice(&v.to_le_bytes());
        }
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.unwrap_or(samples.len() as u32 * 2).to_le_bytes());
        for s in samples {
            w.extend_from_slice(&s.to_le_bytes());
        }
        w
    }

    #[test]
    fn results_are_compared_as_json() {
        let result =
            r#"{"ok":true,"result":{"items":8,"pictures":["a.jpg"],"showing":"~/Pictures","x":{"y":1,"z":2}}}"#;
        // Members in any order, in one object, at any depth.
        assert!(result_has(result, r#""showing":"~/Pictures","items":8"#));
        assert!(result_has(result, r#""items":8,"showing":"~/Pictures""#));
        assert!(result_has(result, r#""ok":true"#));
        assert!(result_has(result, r#""x":{"z":2}"#), "an object with the members given");
        assert!(result_has(result, r#""pictures":["a.jpg"]"#));
        assert!(!result_has(result, r#""showing":"~/Pictures","items":7"#));
        assert!(!result_has(result, r#""ok":true,"items":8"#), "members of different objects");
        assert!(!result_has(result, r#""pictures":[]"#));
        // Numbers by value; arrays element by element, of the same length.
        let numbers = r#"{"seconds":2.0,"size":20,"up_next":[{"artist":"A","title":"T"}]}"#;
        assert!(result_has(numbers, r#""seconds":2,"size":20.0"#));
        assert!(!result_has(numbers, r#""seconds":2.5"#));
        assert!(result_has(numbers, r#""up_next":[{"title":"T"}]"#));
        assert!(!result_has(numbers, r#""up_next":[{"title":"T"},{"title":"T"}]"#));
        // Anything else is looked for as text.
        assert!(result_has(result, r#""showing":"~/Pic"#));
        assert!(result_has(result, "Pictures"));
        assert!(result_has("written", "written"));
        assert!(!result_has(result, "nothing"));
    }

    #[test]
    fn silent_gaps_inside_the_sound() {
        // 1000 frames per second, stereo: silence, sound, a 30 ms
        // dropout, sound, a 10 ms one, sound, silence.
        let mut s = vec![0i16; 2 * 50];
        s.extend(std::iter::repeat_n(3000, 2 * 100));
        s.extend(std::iter::repeat_n(0, 2 * 30));
        s.extend(std::iter::repeat_n(-3000, 2 * 100));
        s.extend(std::iter::repeat_n(0, 2 * 10));
        s.extend(std::iter::repeat_n(2000, 2 * 100));
        s.extend(std::iter::repeat_n(0, 2 * 500));
        let gaps = silent_gaps(&wav(&s, None), 20.0).unwrap();
        assert_eq!(gaps, [(0.15, 30.0)]);
        // QEMU's header while recording: no data length.
        assert_eq!(silent_gaps(&wav(&s, Some(0)), 5.0).unwrap(), [(0.15, 30.0), (0.28, 10.0)]);
        assert!(silent_gaps(b"RIFF", 20.0).is_err());
        assert_eq!(recorded_peak(&wav(&s, None)), Ok(3000));
        assert_eq!(recorded_peak(&wav(&[0; 8], None)), Ok(0));
    }

    #[test]
    fn pictures_are_compared_in_their_regions() {
        let dir = std::env::temp_dir().join(format!("orchestration-checks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let save = |name: &str, pixels: Vec<u32>| {
            let image = vimage::Image::from_pixels(4, 2, pixels).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, vimage::encode_png(&image).unwrap()).unwrap();
            path
        };
        let black = 0xFF00_0000;
        let a = save("a.png", vec![black; 8]);
        let mut lit = vec![black; 8];
        lit[3] = 0xFFFF_FFFF;
        let b = save("b.png", lit);
        assert_eq!(same_pictures(&a, &a, crate::script::SCREEN), Ok(()));
        assert!(
            same_pictures(&a, &b, crate::script::SCREEN).is_err_and(|e| e.contains("1 pixels, the first at (3, 0)"))
        );
        // The lit pixel is in the right half of the top row.
        assert_eq!(same_pictures(&a, &b, [0.0, 0.0, 0.5, 1.0]), Ok(()));
        assert_eq!(black_picture(&a, crate::script::SCREEN), Ok(()));
        assert!(black_picture(&b, crate::script::SCREEN).is_err());
        assert_eq!(black_picture(&b, [0.0, 0.5, 1.0, 1.0]), Ok(()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
