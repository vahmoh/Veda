// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Client-side windows: connection to the compositor, double- or
//! triple-buffered shared surfaces, frame pacing and event delivery.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use vgfx::{Canvas, Damage, DrawCache, Rect};
use vproto::display::{self as dp, Cursor, WindowEvent, WindowSpec, WindowState, display};
use vrt::object::{Channel, Vmo};
use vrt::vm::Mapping;

/// Errors creating or driving a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    /// The display service is unavailable.
    NoDisplay,
    Ipc(vipc::IpcError),
    Display(dp::DisplayError),
    NoMemory,
}

/// A connection to the display service, shared by all windows of a process.
pub type Display = Rc<display::Client>;

/// Connects to the compositor.
pub fn connect() -> Result<Display, WindowError> {
    let ch = vproto::connect(display::NAME).map_err(|_| WindowError::NoDisplay)?;
    Ok(Rc::new(display::Client::new(ch)))
}

struct Surface {
    map: Mapping,
    width: i32,
    height: i32,
    stride: i32,
}

/// Buffers a window draws into, unless it asks for three
/// ([`Window::triple_buffer`]).
const BUFFERS: u8 = 2;

/// When the last acknowledged frame was presented and when it reaches the
/// screen, as the compositor reported with its `FrameDone`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameTiming {
    /// When the client presented it (monotonic ns).
    pub presented_ns: u64,
    /// When it is seen (monotonic ns; 0: no frame acknowledged yet).
    pub seen_ns: u64,
    /// The time between two frames on the screen (ns).
    pub period_ns: u64,
}

impl FrameTiming {
    /// The earliest a frame presented at `now` can be seen: a period after
    /// the last one, and not before `now`.
    pub fn earliest(&self, now: u64) -> u64 {
        if self.seen_ns == 0 { now } else { (self.seen_ns + self.period_ns).max(now) }
    }
}

pub struct Window {
    pub display: Display,
    pub id: u32,
    events: Channel,
    surface: Option<Surface>,
    /// How many buffers the surface has.
    buffers: u8,
    /// Buffer currently on screen (draw into the next one).
    shown: Option<u8>,
    /// A presented frame has not been acknowledged yet.
    in_flight: bool,
    /// When the frame in flight was presented.
    presented_ns: u64,
    /// The last acknowledged frame's timing.
    timing: FrameTiming,
    pub width: i32,
    pub height: i32,
    pub state: WindowState,
    pub focused: bool,
    pub closed: bool,
}

impl Window {
    pub fn new(display: &Display, spec: WindowSpec) -> Result<Window, WindowError> {
        let (w, h) = (spec.width as i32, spec.height as i32);
        let (id, events) = display.create_window(spec).map_err(WindowError::Ipc)?.map_err(WindowError::Display)?;
        Ok(Window {
            display: display.clone(),
            id,
            events,
            surface: None,
            buffers: BUFFERS,
            shown: None,
            in_flight: false,
            presented_ns: 0,
            timing: FrameTiming::default(),
            width: w,
            height: h,
            state: WindowState::Normal,
            focused: false,
            closed: false,
        })
    }

    /// The channel on which this window's events arrive (for wait sets).
    pub fn event_channel(&self) -> &Channel {
        &self.events
    }

    /// Reads all pending events (non-blocking), applying the ones that
    /// affect the window itself (configure, frame pacing, focus).
    pub fn poll_events(&mut self) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        loop {
            match self.events.read() {
                Ok(msg) => {
                    if let Ok((_, ev)) = vipc::decode_event::<WindowEvent>(msg) {
                        match &ev {
                            WindowEvent::Configure { width, height, state } => {
                                self.width = *width as i32;
                                self.height = *height as i32;
                                self.state = *state;
                            }
                            WindowEvent::FrameDone { seen_ns, period_ns, .. } => {
                                self.in_flight = false;
                                self.timing = FrameTiming {
                                    presented_ns: self.presented_ns,
                                    seen_ns: *seen_ns,
                                    period_ns: *period_ns,
                                };
                            }
                            WindowEvent::Focus { focused } => self.focused = *focused,
                            _ => {}
                        }
                        out.push(ev);
                    }
                }
                Err(vabi::Error::PeerClosed) => {
                    self.closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        out
    }

    /// True once the compositor has taken the frame presented last (its
    /// `FrameDone` came), so that the next may be presented. With two
    /// buffers the next is drawn only then, as the buffer it goes into was
    /// the compositor's until then; with three ([`Window::triple_buffer`])
    /// it may be drawn while the last is still on its way.
    pub fn can_draw(&self) -> bool {
        !self.in_flight && !self.closed
    }

    /// Gives the window three buffers rather than two, from its next frame
    /// on: one for the frame on the screen, one for the frame presented
    /// last, and one to draw the next into while the compositor has yet to
    /// take that one. For windows that draw every frame and should not wait
    /// to start, such as a game's.
    pub fn triple_buffer(&mut self) {
        if self.buffers != 3 {
            self.buffers = 3;
            self.surface = None;
        }
    }

    /// The last acknowledged frame's timing.
    pub fn frame_timing(&self) -> FrameTiming {
        self.timing
    }

    fn ensure_surface(&mut self) -> Result<(), WindowError> {
        let (w, h) = (self.width.max(1), self.height.max(1));
        if self.surface.as_ref().is_some_and(|s| s.width == w && s.height == h) {
            return Ok(());
        }
        let stride = w;
        let bytes = (stride * h * 4) as usize * self.buffers as usize;
        let vmo = Vmo::create(bytes).map_err(|_| WindowError::NoMemory)?;
        let theirs = Vmo::from_handle(vmo.0.duplicate(None).map_err(|_| WindowError::NoMemory)?);
        let map = Mapping::new(vmo, bytes.next_multiple_of(4096), vabi::map_flags::READ | vabi::map_flags::WRITE)
            .map_err(|_| WindowError::NoMemory)?;
        self.display
            .attach_buffers(self.id, theirs, w as u32, h as u32, stride as u32, self.buffers)
            .map_err(WindowError::Ipc)?
            .map_err(WindowError::Display)?;
        self.surface = Some(Surface { map, width: w, height: h, stride });
        self.shown = None;
        Ok(())
    }

    fn back_index(&self) -> u8 {
        match self.shown {
            Some(i) => (i + 1) % self.buffers,
            None => 0,
        }
    }

    /// Returns a canvas for the next frame (the buffer that is not on
    /// screen), resizing the surface if the window size changed.
    pub fn begin_frame(&mut self) -> Result<Canvas<'_>, WindowError> {
        let (pixels, width, height, stride) = self.back_buffer()?;
        Ok(Canvas::new(pixels, width, height, stride))
    }

    /// Like [`Window::begin_frame`], drawing with `cache` (kept by the
    /// caller from frame to frame).
    pub fn begin_frame_with<'a>(&'a mut self, cache: &'a mut DrawCache) -> Result<Canvas<'a>, WindowError> {
        let (pixels, width, height, stride) = self.back_buffer()?;
        Ok(Canvas::with_cache(pixels, width, height, stride, cache))
    }

    /// The buffer the next frame is drawn into, with its width, height and
    /// stride.
    fn back_buffer(&mut self) -> Result<(&mut [u32], i32, i32, i32), WindowError> {
        self.ensure_surface()?;
        let index = self.back_index();
        let s = self.surface.as_ref().ok_or(WindowError::NoMemory)?;
        let per = (s.stride * s.height) as usize;
        // SAFETY: the mapping holds `self.buffers` buffers of `per` pixels
        // (the surface is made again when that changes); the compositor only
        // reads the one on screen.
        let pixels =
            unsafe { core::slice::from_raw_parts_mut((s.map.as_ptr() as *mut u32).add(per * index as usize), per) };
        Ok((pixels, s.width, s.height, s.stride))
    }

    /// What the frame drawn since `begin_frame` changed of the one on
    /// screen, in bands of rows; `None` when there is none on screen to
    /// compare with (the first frame, or the first at a new size). Comparing
    /// reads both frames, which costs far less than the compositor redrawing,
    /// recomposing and copying a whole window.
    pub fn changes(&self) -> Option<Damage> {
        let shown = self.shown?;
        let s = self.surface.as_ref()?;
        let per = (s.stride * s.height) as usize;
        // SAFETY: the mapping holds `self.buffers` buffers of `per` pixels,
        // among them the one on screen, which the compositor only reads, and
        // the one the frame was drawn into, which nothing writes now.
        let buffer =
            |i: u8| unsafe { core::slice::from_raw_parts((s.map.as_ptr() as *const u32).add(per * i as usize), per) };
        Some(Damage::of_changes(buffer(shown), buffer(self.back_index()), s.width, s.height, s.stride as usize))
    }

    /// Shows the frame drawn since `begin_frame`. `damage` lists changed
    /// rectangles (empty = everything).
    pub fn present(&mut self, damage: &[Rect]) -> Result<(), WindowError> {
        let index = self.back_index();
        let rects: Vec<dp::Rect> = damage.iter().map(|r| dp::Rect::new(r.x, r.y, r.w as u32, r.h as u32)).collect();
        self.presented_ns = vrt::time::now_ns();
        self.display.present(self.id, index, rects).map_err(WindowError::Ipc)?.map_err(WindowError::Display)?;
        self.shown = Some(index);
        self.in_flight = true;
        Ok(())
    }

    pub fn set_title(&self, title: &str) {
        let _ = self.display.set_title(self.id, String::from(title));
    }

    pub fn set_cursor(&self, cursor: Cursor) {
        let _ = self.display.set_cursor(self.id, cursor);
    }

    pub fn set_state(&self, state: WindowState) {
        let _ = self.display.set_state(self.id, state);
    }

    pub fn set_position(&self, x: i32, y: i32) {
        let _ = self.display.set_position(self.id, x, y);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.display.destroy_window(self.id);
    }
}
