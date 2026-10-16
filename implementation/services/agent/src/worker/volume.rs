// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `volume`: the sound's volume and mute, set through the audio service.

use serde_json::{Value, json};
use vproto::audio::audio;

use super::{Ctx, SERVICE_TIMEOUT_NS, connect};

/// The audio service, which must answer in time.
pub(super) fn audio() -> Option<audio::Client> {
    let a = connect(audio::NAME, audio::Client::new)?;
    a.set_timeout(SERVICE_TIMEOUT_NS);
    Some(a)
}

impl Ctx {
    /// `volume`: sets or reports the master volume and mute.
    pub(super) fn volume(&self, args: &Value) -> Result<Value, String> {
        let a = audio().ok_or("sound is not available")?;
        let s = a.status().map_err(|_| "the sound service did not answer")?;
        let mut level = (s.master_volume * 100.0 + 0.5) as i64;
        let mut muted = s.muted;
        if let Some(l) = args["level"].as_f64() {
            level = l as i64;
            muted = false;
        }
        if let Some(c) = args["change"].as_f64() {
            level += c as i64;
            muted = false;
        }
        if let Some(m) = args["mute"].as_bool() {
            muted = m;
        }
        let level = level.clamp(0, 100);
        a.set_master(level as f32 / 100.0, muted).map_err(|_| "the sound service did not answer")?;
        Ok(json!({ "volume": level, "muted": muted }))
    }
}
