// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `wifi`: the Wi-Fi radio and networks, through the Wi-Fi service.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Value, json};
use vnet::wifi;
use vproto::wlan::ConnState;
use vrt::time::Duration;

use super::Ctx;

impl Ctx {
    /// `wifi`: status, networks, joining and leaving, the radio.
    pub(super) fn wifi(&self, args: &Value) -> Result<Value, String> {
        let err = |e: wifi::WifiError| e.to_string();
        match args["operation"].as_str().unwrap_or("status") {
            "status" => {
                let s = wifi::status().map_err(err)?;
                Ok(json!({
                    "state": format!("{:?}", s.state).to_lowercase(),
                    "network": s.name.as_str(),
                    "radio_on": s.state != ConnState::RadioOff && s.state != ConnState::NoAdapter,
                }))
            }
            "scan" => {
                let _ = wifi::scan();
                vrt::time::sleep(Duration::from_secs(3));
                let nets = wifi::networks().map_err(err)?;
                let list: Vec<Value> = nets
                    .iter()
                    .take(20)
                    .map(|n| json!({ "name": n.name.as_str(), "security": format!("{:?}", n.security), "bars": n.bars, "saved": n.saved, "connected": n.connected }))
                    .collect();
                Ok(Value::Array(list))
            }
            "connect" => {
                let net = args["network"].as_str().ok_or("which network?")?;
                wifi::connect(net, args["password"].as_str().filter(|p| !p.is_empty()), true).map_err(err)?;
                let s = wifi::wait_for(Duration::from_secs(20), |s| s.state == ConnState::Connected).map_err(err)?;
                Ok(json!({ "connected": s.state == ConnState::Connected, "network": net }))
            }
            "disconnect" => wifi::disconnect().map(|_| Value::Null).map_err(err),
            "on" => wifi::set_radio(true).map(|_| Value::Null).map_err(err),
            "off" => wifi::set_radio(false).map(|_| Value::Null).map_err(err),
            other => Err(format!("unknown Wi-Fi operation {other}")),
        }
    }
}
