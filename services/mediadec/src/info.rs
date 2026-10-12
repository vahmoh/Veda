//! What a source holds, as the protocol says it ([`MediaInfo`]): built
//! from the shim's descriptions, kept to the protocol's limits, with times
//! from the start of the source.

use alloc::vec::Vec;

use vproto::media::{
    self, AudioStream, Chapter, Colour, MAX_AUDIO_STREAMS, MAX_CHAPTERS, MAX_LANGUAGE, MAX_NAME, MAX_SIDE, MAX_TEXT,
    MAX_VIDEO_STREAMS, MediaInfo, Rational, VideoStream, bounded,
};

use crate::ffi::{self, KIND_AUDIO, KIND_SUBTITLE, KIND_VIDEO, NO_TIME};

/// H.273's PQ and HLG transfers: high dynamic range.
const TRANSFER_PQ: u32 = 16;
const TRANSFER_HLG: u32 = 18;

/// A stream the source holds, as described.
pub fn video_stream(s: &ffi::Stream) -> VideoStream {
    VideoStream {
        index: s.index,
        codec: bounded(&ffi::text(&s.codec), MAX_NAME),
        width: s.width.min(MAX_SIDE),
        height: s.height.min(MAX_SIDE),
        sample_aspect: Rational::new(s.sar_num, s.sar_den),
        rotation: if matches!(s.rotation, 90 | 180 | 270) { s.rotation } else { 0 },
        mirrored: s.mirrored != 0,
        frame_rate: Rational::new(s.fps_num, s.fps_den),
        bit_depth: s.bit_depth.min(16) as u8,
        colour: Colour {
            primaries: s.primaries.min(255) as u8,
            transfer: s.transfer.min(255) as u8,
            matrix: s.matrix.min(255) as u8,
            full_range: s.full_range != 0,
        },
        hdr: s.transfer == TRANSFER_PQ || s.transfer == TRANSFER_HLG,
        decodable: s.decodable != 0,
        default: s.is_default != 0,
        language: bounded(&ffi::text(&s.language), MAX_LANGUAGE),
        title: bounded(&ffi::text(&s.title), MAX_TEXT),
    }
}

pub fn audio_stream(s: &ffi::Stream) -> AudioStream {
    AudioStream {
        index: s.index,
        codec: bounded(&ffi::text(&s.codec), MAX_NAME),
        rate: s.rate,
        channels: s.channels,
        decodable: s.decodable != 0,
        default: s.is_default != 0,
        language: bounded(&ffi::text(&s.language), MAX_LANGUAGE),
        title: bounded(&ffi::text(&s.title), MAX_TEXT),
    }
}

/// The whole description: the source's `info`, its `streams` and its
/// `chapters` (start in the source's own times, title), with the streams
/// chosen to show.
pub fn media_info(
    info: &ffi::Info,
    streams: &[ffi::Stream],
    chapters: &[(i64, Vec<u8>)],
    video: Option<u32>,
    audio: Option<u32>,
) -> MediaInfo {
    let start = if info.start_us == NO_TIME { 0 } else { info.start_us };
    let video_streams: Vec<VideoStream> = streams
        .iter()
        .filter(|s| s.kind == KIND_VIDEO && s.cover == 0)
        .take(MAX_VIDEO_STREAMS)
        .map(video_stream)
        .collect();
    let audio_streams: Vec<AudioStream> =
        streams.iter().filter(|s| s.kind == KIND_AUDIO).take(MAX_AUDIO_STREAMS).map(audio_stream).collect();
    let chapters = chapters
        .iter()
        .take(MAX_CHAPTERS)
        .map(|(at, title)| Chapter {
            start_us: at.saturating_sub(start).max(0),
            title: bounded(&ffi::text(title), MAX_TEXT),
        })
        .collect();
    // Only streams that are listed can be the ones shown.
    let video = video.filter(|i| video_streams.iter().any(|v| v.index == *i));
    let audio = audio.filter(|i| audio_streams.iter().any(|a| a.index == *i));
    MediaInfo {
        container: bounded(&ffi::text(&info.container), MAX_NAME),
        duration_us: (info.duration_us != NO_TIME && info.duration_us >= 0).then_some(info.duration_us),
        seekable: info.seekable != 0,
        bit_rate: info.bit_rate,
        title: bounded(&ffi::text(&info.title), MAX_TEXT),
        artist: bounded(&ffi::text(&info.artist), MAX_TEXT),
        video: video_streams,
        audio: audio_streams,
        subtitles: streams.iter().filter(|s| s.kind == KIND_SUBTITLE).count() as u32,
        chapters,
        selected_video: video,
        selected_audio: audio,
    }
}

/// A stream index from the shim (`< 0`: none).
pub fn index(i: i32) -> Option<u32> {
    (i >= 0).then_some(i as u32)
}

/// Whether `info` is something a player can show: some decodable stream.
pub fn playable(info: &MediaInfo) -> Result<(), media::MediaError> {
    if info.selected_video.is_none() && info.selected_audio.is_none() {
        let listed = !info.video.is_empty() || !info.audio.is_empty();
        return Err(if listed { media::MediaError::Unsupported } else { media::MediaError::Corrupt });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;

    fn put(dst: &mut [u8], s: &str) {
        dst[..s.len()].copy_from_slice(s.as_bytes());
    }

    fn stream(index: u32, kind: u32, codec: &str) -> ffi::Stream {
        let mut s = ffi::Stream::zeroed();
        s.index = index;
        s.kind = kind;
        put(&mut s.codec, codec);
        s.decodable = 1;
        s
    }

    #[test]
    fn descriptions_become_media_info() {
        let mut info = ffi::Info::zeroed();
        info.duration_us = 16_000_000;
        info.start_us = 500_000;
        info.seekable = 1;
        put(&mut info.container, "matroska");
        put(&mut info.title, "Aurora Timelapse");
        let mut v = stream(0, KIND_VIDEO, "av1");
        (v.width, v.height, v.fps_num, v.fps_den, v.rotation, v.transfer) = (1280, 720, 24, 1, 90, 16);
        let cover = ffi::Stream { cover: 1, ..stream(1, KIND_VIDEO, "mjpeg") };
        let a = ffi::Stream { rate: 48_000, channels: 2, ..stream(2, KIND_AUDIO, "flac") };
        let sub = stream(3, KIND_SUBTITLE, "subrip");
        let chapters = vec![(500_000, b"Start\0".to_vec()), (8_500_000, b"Middle".to_vec())];
        let m = media_info(&info, &[v, cover, a, sub], &chapters, Some(0), Some(2));
        assert!(m.is_valid());
        assert_eq!(m.container, "matroska");
        assert_eq!(m.duration_us, Some(16_000_000));
        assert_eq!(m.video.len(), 1, "the cover picture is not a video");
        let vs = m.video_stream().unwrap();
        assert_eq!((vs.codec.as_str(), vs.rotation, vs.hdr), ("av1", 90, true));
        assert_eq!(vs.display_size(), (720, 1280));
        assert_eq!(m.audio_stream().unwrap().rate, 48_000);
        assert_eq!(m.subtitles, 1);
        assert_eq!(m.chapters[1], Chapter { start_us: 8_000_000, title: "Middle".into() });
        assert_eq!(playable(&m), Ok(()));
    }

    #[test]
    fn unknown_and_hostile_values() {
        let mut info = ffi::Info::zeroed();
        info.duration_us = NO_TIME;
        info.start_us = NO_TIME;
        let mut v = stream(0, KIND_VIDEO, "h264");
        (v.width, v.rotation, v.bit_depth) = (u32::MAX, 45, 99);
        let m = media_info(&info, &[v], &[], Some(7), None);
        assert_eq!(m.duration_us, None);
        assert_eq!(m.selected_video, None, "a stream that is not listed");
        assert_eq!((m.video[0].width, m.video[0].rotation, m.video[0].bit_depth), (MAX_SIDE, 0, 16));
        assert_eq!(playable(&m), Err(media::MediaError::Unsupported));
        assert_eq!(playable(&media_info(&info, &[], &[], None, None)), Err(media::MediaError::Corrupt));
        assert_eq!((index(-3), index(4)), (None, Some(4)));
    }
}
