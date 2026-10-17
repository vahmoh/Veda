// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The `display` protocol: one [`Session`] per request, for the client on
//! one connection. What a request may do and what it changes, the window
//! manager decides (`compositor_core::Wm`); this makes the channels and maps
//! the memory that go with it.

use alloc::string::String;
use alloc::vec::Vec;

use compositor_core::buffers::Buffers;
use proto::display::{self as dp, Cursor, DisplayError, ScreenInfo, WindowInfo, WindowSpec, WindowState, display};
use rt::object::{Channel, Vmo};
use rt::vm::Mapping;

use crate::state::Compositor;

/// A request of the client on connection `client`, at `now`.
pub(crate) struct Session<'a> {
    pub(crate) comp: &'a mut Compositor,
    pub(crate) client: u64,
    pub(crate) now: u64,
}

impl display::Server for Session<'_> {
    fn create_window(&mut self, spec: WindowSpec) -> Result<(u32, Channel), DisplayError> {
        let (ours, theirs) = Channel::create().map_err(|_| DisplayError::NoMemory)?;
        let id = self.comp.wm.create_window(self.client, spec, self.now)?;
        self.comp.events.insert(id, ours);
        Ok((id, theirs))
    }

    fn attach_buffers(
        &mut self,
        id: u32,
        buffers: Vmo,
        width: u32,
        height: u32,
        stride: u32,
        count: u8,
    ) -> Result<(), DisplayError> {
        let memory = buffers.size().map_err(|_| DisplayError::BadBuffer)?;
        let layout = self.comp.wm.buffer_layout(self.client, id, (width, height), stride, count, memory)?;
        let map = Mapping::new(buffers, layout.bytes().next_multiple_of(4096), abi::map_flags::READ)
            .map_err(|_| DisplayError::NoMemory)?;
        self.comp.wm.attach(self.client, id, Buffers { layout, memory: map })
    }

    fn present(&mut self, id: u32, index: u8, damage: Vec<dp::Rect>) -> Result<(), DisplayError> {
        self.comp.wm.present(self.client, id, index, damage)
    }

    fn set_title(&mut self, id: u32, title: String) -> Result<(), DisplayError> {
        self.comp.wm.set_title(self.client, id, title)
    }

    fn set_state(&mut self, id: u32, state: WindowState) -> Result<(), DisplayError> {
        self.comp.wm.set_window_state(self.client, id, state, self.now)
    }

    fn destroy_window(&mut self, id: u32) -> Result<(), DisplayError> {
        self.comp.wm.destroy(self.client, id, self.now)
    }

    fn set_cursor(&mut self, id: u32, cursor: Cursor) -> Result<(), DisplayError> {
        self.comp.wm.set_cursor(self.client, id, cursor)
    }

    fn screen_info(&mut self) -> ScreenInfo {
        self.comp.wm.screen_info()
    }

    fn set_clipboard(&mut self, text: String) -> Result<(), DisplayError> {
        self.comp.wm.set_clipboard(self.client, text)
    }

    fn get_clipboard(&mut self) -> Result<String, DisplayError> {
        self.comp.wm.clipboard(self.client)
    }

    fn set_position(&mut self, id: u32, x: i32, y: i32) -> Result<(), DisplayError> {
        self.comp.wm.set_position(self.client, id, x, y)
    }

    fn list_windows(&mut self) -> Vec<WindowInfo> {
        self.comp.wm.list_windows(self.client)
    }

    fn activate_window(&mut self, id: u32) -> Result<(), DisplayError> {
        self.comp.wm.activate(self.client, id, self.now)
    }

    fn minimize_window(&mut self, id: u32) -> Result<(), DisplayError> {
        self.comp.wm.minimize(self.client, id, self.now)
    }

    fn arrange_window(&mut self, id: u32, arrangement: u32) -> Result<(), DisplayError> {
        self.comp.wm.arrange(self.client, id, arrangement, self.now)
    }

    fn desktop_ready(&mut self) -> Result<(), DisplayError> {
        self.comp.wm.may_ready_desktop(self.client)?;
        if let Some(s) = &mut self.comp.startup {
            s.clear();
        }
        Ok(())
    }

    fn close_window(&mut self, id: u32) -> Result<(), DisplayError> {
        self.comp.wm.close(self.client, id, self.now)
    }

    fn lock(&mut self) -> Result<(), DisplayError> {
        self.comp.wm.lock(self.client, self.now)
    }

    fn unlock(&mut self) -> Result<(), DisplayError> {
        self.comp.wm.unlock(self.client, self.now)
    }

    fn watch_lock(&mut self, events: Channel) -> Result<(), DisplayError> {
        self.comp.wm.may_watch_lock(self.client)?;
        self.comp.watch_lock(events)
    }

    fn set_idle_timeout(&mut self, seconds: u32) -> Result<(), DisplayError> {
        self.comp.wm.set_idle_timeout(self.client, seconds)
    }

    fn keep_awake(&mut self, id: u32, awake: bool) -> Result<(), DisplayError> {
        self.comp.wm.keep_awake(self.client, id, awake)
    }

    fn user_active(&mut self) -> Result<(), DisplayError> {
        self.comp.wm.user_active(self.client, self.now)
    }
}
