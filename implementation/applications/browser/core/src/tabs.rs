// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The tabs: their order, the one shown, which shows when it closes, and
//! how wide the strip draws each.

use alloc::vec::Vec;

/// Tabs of `T`, one of them shown while there are any.
#[derive(Debug, Clone)]
pub struct Tabs<T> {
    tabs: Vec<T>,
    shown: usize,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Tabs { tabs: Vec::new(), shown: 0 }
    }
}

impl<T> Tabs<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Which tab is shown (0 when there is none).
    pub fn shown(&self) -> usize {
        self.shown
    }

    pub fn current(&self) -> Option<&T> {
        self.tabs.get(self.shown)
    }

    pub fn current_mut(&mut self) -> Option<&mut T> {
        self.tabs.get_mut(self.shown)
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        self.tabs.get(i)
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        self.tabs.get_mut(i)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.tabs.iter_mut()
    }

    /// The first tab `f` accepts.
    pub fn position(&self, f: impl Fn(&T) -> bool) -> Option<usize> {
        self.tabs.iter().position(f)
    }

    /// Adds `tab`: next to the shown one (a page opened from it), or at
    /// the end; it is shown if `show`, or if it is the only one. Returns
    /// where it is.
    pub fn open(&mut self, tab: T, next_to_shown: bool, show: bool) -> usize {
        let at = if next_to_shown && !self.tabs.is_empty() { self.shown + 1 } else { self.tabs.len() };
        self.tabs.insert(at, tab);
        if show || self.tabs.len() == 1 {
            self.shown = at;
        } else if at <= self.shown {
            self.shown += 1;
        }
        at
    }

    /// Closes tab `i`. If it was shown, the one after it is (at the end,
    /// the one before).
    pub fn close(&mut self, i: usize) -> Option<T> {
        if i >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(i);
        if i < self.shown || (i == self.shown && self.shown == self.tabs.len() && self.shown > 0) {
            self.shown -= 1;
        }
        Some(tab)
    }

    /// Shows tab `i`; `false` if there is no such tab.
    pub fn show(&mut self, i: usize) -> bool {
        let ok = i < self.tabs.len();
        if ok {
            self.shown = i;
        }
        ok
    }

    /// Shows the next tab (the first after the last), or the one before.
    pub fn cycle(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n > 1 {
            self.shown = if forward { (self.shown + 1) % n } else { (self.shown + n - 1) % n };
        }
    }

    /// Moves tab `from` to `to`, the shown one staying shown.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.shown = if self.shown == from {
            to
        } else if from < self.shown && self.shown <= to {
            self.shown - 1
        } else if to <= self.shown && self.shown < from {
            self.shown + 1
        } else {
            self.shown
        };
    }
}

/// How wide each of `n` tabs is drawn in a strip `width` wide: `widest` at
/// most, narrower as more come, but `narrowest` at least (the strip then
/// scrolls).
pub fn tab_width(n: usize, width: i32, narrowest: i32, widest: i32) -> i32 {
    if n == 0 {
        return widest;
    }
    (width / n as i32).clamp(narrowest, widest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(names: &[char]) -> Tabs<char> {
        let mut t = Tabs::new();
        for &n in names {
            t.open(n, false, false);
        }
        t
    }

    fn names(t: &Tabs<char>) -> std::string::String {
        t.iter().collect()
    }

    #[test]
    fn opening_puts_tabs_where_they_belong() {
        let mut t = tabs(&['a', 'b', 'c']);
        assert_eq!((t.shown(), names(&t)), (0, "abc".into()));
        // A page opened from the shown tab goes next to it, behind it.
        assert_eq!(t.open('x', true, false), 1);
        assert_eq!((t.shown(), names(&t)), (0, "axbc".into()));
        // A new tab goes at the end, in front.
        assert_eq!(t.open('n', false, true), 4);
        assert_eq!(*t.current().unwrap(), 'n');
        // One opened before the shown keeps the shown shown.
        t.show(2);
        t.open('y', false, false);
        assert_eq!(*t.current().unwrap(), 'b');
    }

    #[test]
    fn closing_shows_the_next() {
        let mut t = tabs(&['a', 'b', 'c', 'd']);
        t.show(1);
        assert_eq!(t.close(1), Some('b'));
        assert_eq!(*t.current().unwrap(), 'c');
        t.show(2);
        t.close(2);
        assert_eq!(*t.current().unwrap(), 'c', "the last's one before");
        t.close(0);
        assert_eq!(*t.current().unwrap(), 'c', "one before the shown keeps it shown");
        t.close(0);
        assert!(t.is_empty() && t.current().is_none() && t.close(0).is_none());
    }

    #[test]
    fn cycling_and_moving() {
        let mut t = tabs(&['a', 'b', 'c']);
        t.cycle(false);
        assert_eq!(*t.current().unwrap(), 'c');
        t.cycle(true);
        assert_eq!(*t.current().unwrap(), 'a');
        t.move_tab(0, 2);
        assert_eq!((names(&t), *t.current().unwrap()), ("bca".into(), 'a'));
        t.show(1);
        t.move_tab(2, 0);
        assert_eq!((names(&t), *t.current().unwrap()), ("abc".into(), 'c'));
        assert_eq!(tab_width(3, 900, 60, 240), 240);
        assert_eq!(tab_width(10, 900, 60, 240), 90);
        assert_eq!(tab_width(100, 900, 60, 240), 60);
    }
}
