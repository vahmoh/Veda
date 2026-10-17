// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Events: a bare signal word for cross-thread and cross-process
//! notification.

use alloc::sync::Arc;

use crate::object::Signals;

pub struct Event {
    pub koid: u64,
    pub signals: Signals,
}

impl Event {
    pub fn new() -> Arc<Event> {
        Arc::new(Event { koid: crate::object::new_koid(), signals: Signals::new(0) })
    }
}
