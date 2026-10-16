// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `get_status`: the time, the open windows, the volume and the network.

use serde_json::{Value, json};

use super::{Ctx, now_text, windows::windows_json};

impl Ctx {
    /// `get_status`.
    pub(super) fn status(&self) -> Value {
        let mut v = json!({ "time": now_text() });
        if let Some(windows) = windows_json() {
            v["windows"] = windows;
        }
        if let Some(a) = super::volume::audio()
            && let Ok(s) = a.status()
        {
            v["volume"] = json!((s.master_volume * 100.0 + 0.5) as u32);
            v["muted"] = json!(s.muted);
        }
        if let Ok(s) = vnet::status() {
            v["internet"] = json!(format!("{:?}", s.connectivity).to_lowercase());
        }
        if let Ok(w) = vnet::wifi::status() {
            v["wifi"] = json!({ "state": format!("{:?}", w.state).to_lowercase(), "network": w.name.as_str() });
        }
        v
    }
}
