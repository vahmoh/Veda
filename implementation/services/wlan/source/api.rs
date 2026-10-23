// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The `wlan` protocol for applications. What each may do depends on who it
//! is ([`Rights`]): changing Wi-Fi is for the desktop shell, the agent,
//! Settings and the Terminal; access points' addresses, saved networks and
//! diagnostics (where the machine is and has been) for Settings and the
//! Terminal.

use alloc::vec::Vec;

use ipc::Bytes;
use proto::wlan::{
    BssInfo, ConnectRequest, NetworkInfo, SavedNetwork, WlanDiagnostics, WlanError, WlanStatus, ssid_display, wlan,
};
use rt::object::Channel;

use crate::{Rights, Wlan, without_addresses};

pub struct Session<'a> {
    pub w: &'a mut Wlan,
    pub now: u64,
    /// The client's process.
    pub owner: u64,
    pub rights: Rights,
}

impl Session<'_> {
    fn ready(&self) -> Result<(), WlanError> {
        if !self.w.radio_usable() {
            Err(WlanError::NoAdapter)
        } else if !self.w.radio_on {
            Err(WlanError::RadioOff)
        } else {
            Ok(())
        }
    }

    fn may_change(&self) -> Result<(), WlanError> {
        if self.rights.change { Ok(()) } else { Err(WlanError::Denied) }
    }

    fn may_inspect(&self) -> Result<(), WlanError> {
        if self.rights.inspect { Ok(()) } else { Err(WlanError::Denied) }
    }
}

impl wlan::Server for Session<'_> {
    fn status(&mut self) -> WlanStatus {
        let s = self.w.status(self.now);
        if self.rights.inspect { s } else { without_addresses(s) }
    }

    fn scan(&mut self) -> Result<(), WlanError> {
        self.may_change()?;
        self.ready()?;
        self.w.start_scan(true, self.now);
        Ok(())
    }

    fn networks(&mut self) -> Vec<NetworkInfo> {
        let connected = self
            .w
            .current
            .as_ref()
            .filter(|c| c.state == proto::wlan::ConnState::Connected)
            .map(|c| c.request.ssid.clone());
        self.w.table.networks(self.now, &self.w.profiles, connected.as_deref())
    }

    fn access_points(&mut self) -> Result<Vec<BssInfo>, WlanError> {
        self.may_inspect()?;
        Ok(self.w.table.access_points(self.now))
    }

    fn connect(&mut self, request: ConnectRequest) -> Result<(), WlanError> {
        self.may_change()?;
        self.w.connect_request(request, self.now)
    }

    fn disconnect(&mut self) -> Result<(), WlanError> {
        self.may_change()?;
        self.ready()?;
        self.w.log("disconnected by the user".into());
        self.w.user_disconnect(self.now);
        Ok(())
    }

    fn saved(&mut self) -> Result<Vec<SavedNetwork>, WlanError> {
        self.may_inspect()?;
        Ok(self
            .w
            .profiles
            .iter()
            .map(|p| SavedNetwork {
                ssid: Bytes(p.ssid.clone()),
                name: ssid_display(&p.ssid),
                security: p.security,
                auto_connect: p.auto_connect,
                hidden: p.hidden,
                last_connected: p.last_connected,
            })
            .collect())
    }

    fn forget(&mut self, ssid: Bytes) -> Result<(), WlanError> {
        self.may_change()?;
        self.w.forget(&ssid.0, self.now)
    }

    fn set_auto_connect(&mut self, ssid: Bytes, enabled: bool) -> Result<(), WlanError> {
        self.may_change()?;
        self.w.set_auto_connect(&ssid.0, enabled)
    }

    fn set_radio(&mut self, on: bool) -> Result<(), WlanError> {
        self.may_change()?;
        if self.w.radio.is_none() {
            return Err(WlanError::NoAdapter);
        }
        self.w.set_radio(on, self.now);
        Ok(())
    }

    fn watch(&mut self, events: Channel) -> Result<(), WlanError> {
        if self.w.add_watcher(events, self.owner, self.rights) { Ok(()) } else { Err(WlanError::LimitReached) }
    }

    fn diagnostics(&mut self) -> Result<WlanDiagnostics, WlanError> {
        self.may_inspect()?;
        Ok(WlanDiagnostics {
            status: self.w.status(self.now),
            counters: self.w.counters.clone(),
            log: self.w.log_lines(),
            radio: self.w.radio.as_mut().and_then(|r| r.stats()),
        })
    }
}
