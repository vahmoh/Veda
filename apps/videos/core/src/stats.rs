//! How playback went: pictures shown and dropped, how far the pictures
//! were from the sound, how often the window presented and how long its
//! frames took; summed up in one log line now and then.

use alloc::format;
use alloc::string::String;

/// A/V offsets kept for the percentile (the last ones).
const OFFSETS: usize = 512;

/// Above these, pictures and sound are out of step (µs).
pub const OUT_OF_SYNC_MEAN_US: i64 = 40_000;
pub const OUT_OF_SYNC_P95_US: i64 = 80_000;

#[derive(Debug, Clone)]
pub struct PlaybackStats {
    pub shown: u32,
    pub dropped: u32,
    /// Pictures shown later than due (by more than a refresh).
    pub late: u32,
    pub presents: u32,
    offsets: [i32; OFFSETS],
    count: usize,
    next: usize,
    draw_ns: u64,
    draws: u32,
    blit_ns: u64,
    started_ns: u64,
}

impl Default for PlaybackStats {
    fn default() -> PlaybackStats {
        PlaybackStats::new(0)
    }
}

impl PlaybackStats {
    /// A window starting at `now_ns`.
    pub fn new(now_ns: u64) -> PlaybackStats {
        PlaybackStats {
            shown: 0,
            dropped: 0,
            late: 0,
            presents: 0,
            offsets: [0; OFFSETS],
            count: 0,
            next: 0,
            draw_ns: 0,
            draws: 0,
            blit_ns: 0,
            started_ns: now_ns,
        }
    }

    /// A picture of time `pts_us` was seen while the clock said `clock_us`.
    pub fn offset(&mut self, pts_us: i64, clock_us: i64) {
        let d = (pts_us - clock_us).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        self.offsets[self.next] = d;
        self.next = (self.next + 1) % OFFSETS;
        self.count = (self.count + 1).min(OFFSETS);
    }

    /// A frame drawn in `draw_ns`, of which copying the picture took
    /// `blit_ns`.
    pub fn drawn(&mut self, draw_ns: u64, blit_ns: u64) {
        self.draw_ns += draw_ns;
        self.blit_ns += blit_ns;
        self.draws += 1;
    }

    /// The mean and 95th percentile of the offsets' sizes (µs).
    pub fn sync(&self) -> Option<(i64, i64)> {
        if self.count == 0 {
            return None;
        }
        let recent = &self.offsets[..self.count];
        let mean = recent.iter().map(|&o| o as i64).sum::<i64>() / self.count as i64;
        let mut sizes: alloc::vec::Vec<i64> = recent.iter().map(|&o| (o as i64).abs()).collect();
        sizes.sort_unstable();
        let p95 = sizes[(sizes.len() * 95 / 100).min(sizes.len() - 1)];
        Some((mean, p95))
    }

    /// Whether pictures and sound were out of step.
    pub fn out_of_sync(&self) -> bool {
        self.sync().is_some_and(|(mean, p95)| mean.abs() > OUT_OF_SYNC_MEAN_US || p95 > OUT_OF_SYNC_P95_US)
    }

    /// The line for the log, for the window ending at `now_ns`.
    pub fn line(&self, now_ns: u64) -> String {
        let secs = (now_ns.saturating_sub(self.started_ns) as f64 / 1e9).max(0.001);
        let total = (self.shown + self.dropped).max(1);
        let mut line = format!(
            "{} pictures shown, {} dropped ({}.{}%), {} late",
            self.shown,
            self.dropped,
            self.dropped * 100 / total,
            self.dropped * 1000 / total % 10,
            self.late
        );
        if let Some((mean, p95)) = self.sync() {
            line += &format!(
                ", A/V offset mean {}{} ms, p95 {} ms",
                if mean < 0 { "-" } else { "+" },
                mean.abs() / 1000,
                p95 / 1000
            );
        }
        line += &format!(", {:.0} presents/s", self.presents as f64 / secs);
        if self.draws > 0 {
            let ms = |ns: u64| ns as f64 / self.draws as f64 / 1e6;
            line += &format!(", draw {:.1} ms (blit {:.1} ms)", ms(self.draw_ns), ms(self.blit_ns));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_says_how_it_went() {
        let mut s = PlaybackStats::new(0);
        s.shown = 238;
        s.dropped = 2;
        s.presents = 240;
        for k in 0..100 {
            s.offset(k * 1000 + 5_000, k * 1000);
        }
        s.drawn(2_000_000, 500_000);
        assert_eq!(s.sync(), Some((5_000, 5_000)));
        assert!(!s.out_of_sync());
        let line = s.line(10_000_000_000);
        assert_eq!(
            line,
            "238 pictures shown, 2 dropped (0.8%), 0 late, A/V offset mean +5 ms, p95 5 ms, 24 presents/s, draw 2.0 ms (blit 0.5 ms)"
        );
    }

    #[test]
    fn out_of_step_is_noticed() {
        let mut s = PlaybackStats::new(0);
        for k in 0..100 {
            s.offset(0, if k % 10 == 0 { 200_000 } else { 0 });
        }
        let (mean, p95) = s.sync().unwrap();
        assert_eq!(mean, -20_000);
        assert_eq!(p95, 200_000);
        assert!(s.out_of_sync());
        assert_eq!(PlaybackStats::new(0).sync(), None);
    }
}
