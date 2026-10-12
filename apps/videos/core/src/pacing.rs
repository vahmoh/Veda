//! When a picture presented now reaches the screen.
//!
//! With every frame it acknowledges, the compositor tells the window when
//! that frame is seen (`seen_ns`: the vertical blank it is flipped at) and
//! how often frames follow (`period_ns`). A frame presented later is
//! composed at the compositor's next frame and seen at the first blank at
//! least its *lead* after the present: the time the compositor needs before
//! a blank (a quarter of a frame and the composing). The pacer learns the
//! lead from what it saw, then says when a present made now is seen, and
//! when to wake to present a picture due at a given time.

/// One acknowledged frame: when it was presented, when it is seen, and the
/// time between frames (ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    pub presented_ns: u64,
    pub seen_ns: u64,
    pub period_ns: u64,
}

/// 60 Hz, until the compositor says otherwise.
const DEFAULT_PERIOD_NS: u64 = 16_666_667;
/// Recent observations the lead is learned from.
const WINDOW: usize = 16;
/// Kept in hand when waking to present (ns).
const MARGIN_NS: u64 = 1_000_000;

#[derive(Debug, Clone)]
pub struct Pacer {
    period_ns: u64,
    last_seen_ns: u64,
    last: Timing,
    /// What each recent observation says of the lead: `(at least, at most)`.
    bounds: [(u64, u64); WINDOW],
    count: usize,
    next: usize,
    lead_ns: u64,
    draw_ns: u64,
}

impl Default for Pacer {
    fn default() -> Pacer {
        Pacer::new()
    }
}

impl Pacer {
    pub fn new() -> Pacer {
        Pacer {
            period_ns: DEFAULT_PERIOD_NS,
            last_seen_ns: 0,
            last: Timing::default(),
            bounds: [(0, 0); WINDOW],
            count: 0,
            next: 0,
            lead_ns: DEFAULT_PERIOD_NS / 2,
            draw_ns: 0,
        }
    }

    /// The time between two frames on the screen.
    pub fn period(&self) -> u64 {
        self.period_ns
    }

    /// The learned lead (ns).
    pub fn lead(&self) -> u64 {
        self.lead_ns
    }

    /// Takes the timing of the last acknowledged frame (the same one may
    /// come again: it counts once).
    pub fn observe(&mut self, t: Timing) {
        if t.seen_ns == 0 || t == self.last {
            return;
        }
        if t.period_ns > 0 {
            self.period_ns = t.period_ns;
        }
        // Presented while the compositor had nothing pending (after the
        // frame before was seen): it was composed at the first chance, so
        // the lead lies within the last period before it was seen.
        let idle = t.presented_ns >= self.last_seen_ns && t.presented_ns > 0 && t.seen_ns >= t.presented_ns;
        self.last_seen_ns = self.last_seen_ns.max(t.seen_ns);
        self.last = t;
        if !idle {
            return;
        }
        let latency = t.seen_ns - t.presented_ns;
        self.bounds[self.next] = (latency.saturating_sub(self.period_ns), latency);
        self.next = (self.next + 1) % WINDOW;
        self.count = (self.count + 1).min(WINDOW);
        let recent = &self.bounds[..self.count];
        let low = recent.iter().map(|b| b.0).max().unwrap_or(0);
        let high = recent.iter().map(|b| b.1).min().unwrap_or(self.period_ns);
        // Consistent observations narrow it down; when the compositor was
        // slow once, they do not, and the smallest latency seen is the
        // safest guess.
        self.lead_ns = if low <= high { (low + high) / 2 } else { high };
    }

    /// Notes how long drawing a frame took (ns): a moving average.
    pub fn note_draw(&mut self, ns: u64) {
        self.draw_ns = if self.draw_ns == 0 { ns } else { (self.draw_ns * 7 + ns) / 8 };
    }

    /// The first blank at or after `t` (a time on the screen's grid; `t`
    /// itself when no frame was seen yet).
    fn snap(&self, t: u64) -> u64 {
        if self.last_seen_ns == 0 || self.period_ns == 0 || t <= self.last_seen_ns {
            return t.max(self.last_seen_ns);
        }
        self.last_seen_ns + (t - self.last_seen_ns).div_ceil(self.period_ns) * self.period_ns
    }

    /// When a frame presented at `now_ns` (once drawn) is seen.
    pub fn seen_if_presented(&self, now_ns: u64) -> u64 {
        let earliest = now_ns + self.draw_ns + self.lead_ns;
        let after_last = if self.last_seen_ns == 0 { 0 } else { self.last_seen_ns + self.period_ns };
        self.snap(earliest.max(after_last))
    }

    /// When to wake (not before `now_ns`) to draw and present a picture due
    /// on the screen at `due_ns`: in time for the blank nearest it.
    pub fn wake_for(&self, due_ns: u64, now_ns: u64) -> u64 {
        let blank = self.snap(due_ns.saturating_sub(self.period_ns / 2));
        blank.saturating_sub(self.lead_ns + self.draw_ns + MARGIN_NS).max(now_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 13_333_333; // 75 Hz

    /// A compositor flipping at 75 Hz with a lead of `lead`: when a frame
    /// presented at `t` is seen (blanks at multiples of P).
    fn seen(t: u64, lead: u64) -> u64 {
        (t + lead).div_ceil(P) * P
    }

    #[test]
    fn it_learns_the_lead() {
        let mut p = Pacer::new();
        let lead = 4_000_000;
        let mut presented = 1_000_000_000u64;
        for k in 0..40u64 {
            // Presents land anywhere in the period.
            presented += P + (k * 3_700_000) % P;
            let s = seen(presented, lead);
            p.observe(Timing { presented_ns: presented, seen_ns: s, period_ns: P });
        }
        assert_eq!(p.period(), P);
        assert!(p.lead().abs_diff(lead) < 1_500_000, "lead {} for {}", p.lead(), lead);
        // A present a millisecond after a blank is seen where the compositor
        // would show it: at the next one.
        let blank = seen(presented, lead) + P;
        let now = blank + 1_000_000;
        assert_eq!(p.seen_if_presented(now), seen(now, lead));
        assert_eq!(p.seen_if_presented(now), blank + P);
    }

    #[test]
    fn waking_in_time_for_the_nearest_blank() {
        let mut p = Pacer::new();
        let lead = 3_000_000;
        let mut t = 2_000_000_000u64;
        for k in 0..30u64 {
            t += P + (k * 1_900_000) % P;
            p.observe(Timing { presented_ns: t, seen_ns: seen(t, lead), period_ns: P });
        }
        // A picture due on screen at `due`: woken at `w` and presented
        // there, it is seen at the blank nearest `due`.
        for due in (t + 5 * P..t + 9 * P).step_by(2_345_678) {
            let w = p.wake_for(due, t);
            let shown = seen(w, lead);
            assert!(shown.abs_diff(due) <= P / 2 + 1, "due {due}, seen {shown}");
        }
    }

    #[test]
    fn repeated_and_busy_frames_teach_nothing() {
        let mut p = Pacer::new();
        let t = Timing { presented_ns: 100, seen_ns: 5_000_100, period_ns: P };
        p.observe(t);
        let lead = p.lead();
        p.observe(t);
        assert_eq!(p.count, 1);
        // Presented before the last frame was seen: composed late, not idle.
        p.observe(Timing { presented_ns: 200, seen_ns: 5_000_100 + P, period_ns: P });
        assert_eq!((p.count, p.lead()), (1, lead));
        assert!(p.seen_if_presented(0) >= 5_000_100 + P);
        p.note_draw(2_000_000);
        p.note_draw(2_000_000);
        assert_eq!(p.draw_ns, 2_000_000);
    }
}
