// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Descriptions for people: `0:04.033`, "720p", "1280 × 720 · AV1 ·
//! 24 fps", "FLAC · 48 kHz · stereo". Lengths and positions (`1:05`) are
//! `vfiles::format::duration`'s.

use alloc::format;
use alloc::string::{String, ToString};

use vfiles::format::duration;
use vproto::media::{AudioStream, Rational, VideoStream};

/// `m:ss.mmm` (a picture's time).
pub fn precise(us: i64) -> String {
    let ms = us.max(0) as u64 / 1000;
    format!("{}.{:03}", duration(ms), ms % 1000)
}

/// The usual name of a picture's size: by its shorter side ("720p", "4K").
pub fn resolution(width: u32, height: u32) -> String {
    let short = width.min(height);
    match short {
        2160.. => "4K".to_string(),
        1440.. => "1440p".to_string(),
        1080.. => "1080p".to_string(),
        720.. => "720p".to_string(),
        _ => format!("{short}p"),
    }
}

/// A codec's name as people know it.
pub fn codec(name: &str) -> String {
    let known = match name {
        "av1" | "libdav1d" => "AV1",
        "h264" => "H.264",
        "hevc" => "HEVC",
        "vp9" => "VP9",
        "vp8" => "VP8",
        "mpeg4" => "MPEG-4",
        "mpeg2video" => "MPEG-2",
        "mpeg1video" => "MPEG-1",
        "aac" | "aac_latm" => "AAC",
        "opus" => "Opus",
        "vorbis" => "Vorbis",
        "mp3" | "mp3float" => "MP3",
        "mp2" | "mp2float" => "MP2",
        "flac" => "FLAC",
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        n if n.starts_with("pcm_") => "PCM",
        n => return n.to_uppercase(),
    };
    known.to_string()
}

/// Frames a second: "24", "29.97", or `None` when not known.
pub fn frame_rate(r: Rational) -> Option<String> {
    if !r.is_known() {
        return None;
    }
    let hundredths = (r.num as u64 * 100 + r.den as u64 / 2) / r.den as u64;
    Some(if hundredths.is_multiple_of(100) {
        format!("{}", hundredths / 100)
    } else {
        format!("{}.{:02}", hundredths / 100, hundredths % 100)
    })
}

/// "1280 × 720 · AV1 · 24 fps" (the size it is shown at).
pub fn picture(v: &VideoStream) -> String {
    let (w, h) = v.display_size();
    let mut s = format!("{w} × {h} · {}", codec(&v.codec));
    if let Some(fps) = frame_rate(v.frame_rate) {
        s += &format!(" · {fps} fps");
    }
    if v.hdr {
        s += " · HDR";
    }
    s
}

/// "FLAC · 48 kHz · stereo".
pub fn sound(a: &AudioStream) -> String {
    let khz = if a.rate.is_multiple_of(1000) {
        format!("{}", a.rate / 1000)
    } else {
        format!("{}.{}", a.rate / 1000, a.rate % 1000 / 100)
    };
    let layout = match a.channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{n} channels"),
    };
    format!("{} · {khz} kHz · {layout}", codec(&a.codec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vproto::media::Colour;

    #[test]
    fn times() {
        assert_eq!(precise(4_033_333), "0:04.033");
        assert_eq!(precise(3_725_000_001), "1:02:05.000");
        assert_eq!(precise(-5), "0:00.000");
    }

    #[test]
    fn descriptions() {
        assert_eq!(resolution(1280, 720), "720p");
        assert_eq!(resolution(360, 640), "360p");
        assert_eq!(resolution(3840, 2160), "4K");
        assert_eq!(codec("h264"), "H.264");
        assert_eq!(codec("pcm_s16le"), "PCM");
        assert_eq!(codec("theora"), "THEORA");
        assert_eq!(frame_rate(Rational::new(30_000, 1001)).as_deref(), Some("29.97"));
        assert_eq!(frame_rate(Rational::new(24, 1)).as_deref(), Some("24"));
        assert_eq!(frame_rate(Rational::default()), None);
        let v = VideoStream {
            index: 0,
            codec: "av1".into(),
            width: 1280,
            height: 720,
            sample_aspect: Rational::new(1, 1),
            rotation: 0,
            mirrored: false,
            frame_rate: Rational::new(24, 1),
            bit_depth: 8,
            colour: Colour::default(),
            hdr: false,
            decodable: true,
            default: true,
            language: String::new(),
            title: String::new(),
        };
        assert_eq!(picture(&v), "1280 × 720 · AV1 · 24 fps");
        let a = AudioStream {
            index: 1,
            codec: "flac".into(),
            rate: 44_100,
            channels: 2,
            decodable: true,
            default: true,
            language: String::new(),
            title: String::new(),
        };
        assert_eq!(sound(&a), "FLAC · 44.1 kHz · stereo");
    }
}
