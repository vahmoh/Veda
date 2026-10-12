//! Where a video goes in a window, and at what size it is decoded.
//!
//! The decoder scales pictures to the size they are shown at, so the pool's
//! pictures are copied to the window as they are. That size is the video's
//! shape fitted into the stage (letterboxed or pillarboxed), never larger
//! than the screen nor than the decoder allows.

/// A rectangle in window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The rectangle a `video_w` x `video_h` picture is shown in on a stage of
/// `stage_w` x `stage_h`: as large as fits, in proportion, centred.
pub fn fit_frame(video_w: u32, video_h: u32, stage_w: u32, stage_h: u32) -> Frame {
    if stage_w == 0 || stage_h == 0 {
        return Frame::default();
    }
    let (w, h) = vproto::media::fit(video_w, video_h, stage_w, stage_h);
    Frame { x: ((stage_w - w) / 2) as i32, y: ((stage_h - h) / 2) as i32, w, h }
}

/// The largest picture a pool holds, each way: the screen of the largest
/// supported window (the compositor's screen is at most about 1920x1200; a
/// 4K panel is shown at that too).
pub const MAX_OUTPUT: (u32, u32) = (2560, 1600);

/// The size pictures are decoded at for `frame`: its size, within the
/// screen (0 x 0 when not known) and [`MAX_OUTPUT`], and at least 16 pixels
/// each way.
pub fn output_size(frame: Frame, screen_w: u32, screen_h: u32) -> (u32, u32) {
    let known = screen_w > 0 && screen_h > 0;
    let max_w = if known { screen_w.clamp(16, MAX_OUTPUT.0) } else { MAX_OUTPUT.0 };
    let max_h = if known { screen_h.clamp(16, MAX_OUTPUT.1) } else { MAX_OUTPUT.1 };
    let (w, h) = (frame.w.max(16), frame.h.max(16));
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    vproto::media::fit(w, h, max_w, max_h)
}

/// Bytes the frame pool may take.
pub const POOL_BUDGET: usize = 48 << 20;

/// How many slots a pool of `w` x `h` pictures gets for `fps` frames a
/// second: enough to decode a quarter of a second ahead, within the budget,
/// at least three (one shown, one ready, one being written) and at most
/// eight.
pub fn slots(w: u32, h: u32, fps_num: u32, fps_den: u32) -> u32 {
    let (_, bytes) = vproto::media::FramePool::geometry(w, h);
    let budget = (POOL_BUDGET / bytes.max(1)) as u32;
    let fps = if fps_num > 0 && fps_den > 0 { fps_num.div_ceil(fps_den) } else { 30 };
    let ahead = fps.div_ceil(4) + 1;
    ahead.min(budget).clamp(3, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videos_are_letterboxed() {
        assert_eq!(fit_frame(1280, 720, 1100, 640), Frame { x: 0, y: 10, w: 1100, h: 619 });
        // Portrait in a landscape window: pillarboxed.
        assert_eq!(fit_frame(360, 640, 1280, 800), Frame { x: 415, y: 0, w: 450, h: 800 });
        assert_eq!(fit_frame(640, 360, 0, 100), Frame::default());
    }

    #[test]
    fn output_sizes_keep_within_the_screen() {
        assert_eq!(output_size(Frame { x: 0, y: 0, w: 1100, h: 619 }, 1920, 1080), (1100, 619));
        assert_eq!(output_size(Frame { x: 0, y: 0, w: 3000, h: 1688 }, 1920, 1080), (1919, 1080));
        assert_eq!(output_size(Frame { x: 0, y: 0, w: 2, h: 3 }, 1920, 1080), (16, 16));
        // The screen not known yet: the frame's own size.
        assert_eq!(output_size(Frame { x: 0, y: 0, w: 1100, h: 619 }, 0, 0), (1100, 619));
    }

    #[test]
    fn pools_are_sized_by_rate_and_budget() {
        assert_eq!(slots(1280, 720, 24, 1), 7);
        assert_eq!(slots(640, 360, 30, 1), 8, "a quarter second at 30 fps, capped");
        assert_eq!(slots(1920, 1080, 60, 1), 6, "the budget: 48 MiB of 8 MiB pictures");
        assert_eq!(slots(2560, 1600, 60, 1), 3);
        assert_eq!(slots(320, 180, 0, 0), 8);
    }
}
