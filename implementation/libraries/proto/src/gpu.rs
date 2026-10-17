// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The GPU protocol: 3D rendering on the GPU.
//!
//! Veda's renderer serves it, in the driver VM
//! (`implementation/guests/linux/renderer`), on Mesa's Gallium driver of
//! the GPU over the GPU's Linux driver. Each connection gets a context of
//! its own: the client (an OpenGL ES implementation, `vgl`) creates
//! resources (buffers, textures) and submits command streams in
//! virglrenderer's protocol, which the renderer carries out on Gallium.
//! Contexts are isolated from each other: a context names resources by
//! handles the renderer assigned to it, and resolves only its own.
//!
//! [`gpu::open`] gives the client the renderer's capabilities and a block of
//! memory shared with it:
//!
//! * the command area, where [`gpu::submit`] takes commands from;
//! * the shared area, which resources may use as their storage (the
//!   client's staging buffer and query results);
//! * the fence word: the number of the last fence that signaled, which the
//!   renderer updates (and signals the session's event) as fences signal.
//!   Waiting for a fence needs no call: wait for the event until the word
//!   reaches it.
//!
//! The renderer can render into memory it is given ([`gpu::import`]): the
//! display's pictures, into which the compositor draws its frames.
//!
//! The renderer is not trusted with its clients' memory (a driver VM that
//! has been taken over must not take its clients with it): a client checks
//! the layout it is given ([`Areas::layout`]) before it touches the shared
//! memory, and reads nothing from it that could make it go outside.

use alloc::string::String;
use core::ops::Range;
use vipc::{Bytes, enumeration, message, protocol};
use vrt::object::{Event, Vmo};

enumeration! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GpuError {
        /// The device has no 3D support, or failed.
        Unavailable = 1,
        /// Out of memory, or over the connection's limits.
        NoMemory = 2,
        /// A bad argument (an unknown resource, a range outside the shared
        /// memory, an impossible resource description).
        Invalid = 3,
        /// `open` was already called (or not yet, for the other calls).
        State = 4,
    }
}

impl core::fmt::Display for GpuError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            GpuError::Unavailable => "no 3D device",
            GpuError::NoMemory => "out of memory",
            GpuError::Invalid => "invalid request",
            GpuError::State => "not open, or opened twice",
        })
    }
}

message! {
    /// A resource as virglrenderer creates it (Gallium's description).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ResourceSpec {
        pub target: u32,
        pub format: u32,
        pub bind: u32,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub array_size: u32,
        pub last_level: u32,
        pub nr_samples: u32,
        pub flags: u32,
    }
}

message! {
    /// A connection's context, as [`gpu::open`] sets it up.
    ///
    /// Its memory holds three areas, which a renderer lays out so that:
    ///
    /// * each lies wholly inside the memory, which is no larger than the
    ///   areas need: their sizes and at most a page more for each
    ///   ([`PAGE`]);
    /// * no two overlap;
    /// * the fence word (8 bytes) starts at a multiple of 8, the command
    ///   area at a multiple of 4 (it holds 32-bit words) and the shared
    ///   area at a multiple of 16 (query results are 16-byte records in
    ///   it);
    /// * the command area is at most [`MAX_COMMAND_BYTES`], and the shared
    ///   area at most what `open` asked for, or [`MIN_SHARED_BYTES`] if
    ///   that is more, rounded up to a whole page.
    ///
    /// [`Areas::layout`] checks them all; a client uses no session that
    /// breaks one.
    pub struct Session {
        /// The renderer's capability set 2 (`struct virgl_caps_v2`).
        pub caps: Bytes,
        /// The shared memory.
        pub memory: Vmo,
        /// The command area: offset and size in bytes.
        pub command_offset: u64,
        pub command_size: u64,
        /// The area resources may use as storage.
        pub shared_offset: u64,
        pub shared_size: u64,
        /// Where the last signaled fence's number is (a little-endian u64,
        /// which the renderer writes atomically).
        pub fence_offset: u64,
        /// Signaled whenever a fence signals.
        pub fences: Event,
        /// The renderer's name (its GPU's), for display.
        pub renderer: String,
    }
}

/// A page of the shared memory: areas are rounded up to whole pages.
pub const PAGE: u64 = 4096;
/// The most bytes of commands one submission may have.
pub const MAX_COMMAND_BYTES: u64 = 16 << 20;
/// The shared area a renderer gives at least (what `open` asks for if it
/// asks for less).
pub const MIN_SHARED_BYTES: u64 = 1 << 20;

/// Where a session's areas are, as its renderer says ([`Session::areas`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Areas {
    pub command_offset: u64,
    pub command_size: u64,
    pub shared_offset: u64,
    pub shared_size: u64,
    pub fence_offset: u64,
}

/// A session's layout, checked: byte ranges of its memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub command: Range<usize>,
    pub shared: Range<usize>,
    /// The fence word's first byte.
    pub fence: usize,
    /// How much of the memory the areas span (from its start): what a
    /// client maps.
    pub size: usize,
}

impl Session {
    /// Where the areas are, as the renderer says (unchecked).
    pub fn areas(&self) -> Areas {
        Areas {
            command_offset: self.command_offset,
            command_size: self.command_size,
            shared_offset: self.shared_offset,
            shared_size: self.shared_size,
            fence_offset: self.fence_offset,
        }
    }
}

impl Areas {
    /// The layout, if it keeps every rule [`Session`] lists for a session
    /// opened with `open(asked)`; `None` if it breaks one. The memory's own
    /// size is the client's to check against [`Layout::size`].
    pub fn layout(&self, asked: u64) -> Option<Layout> {
        let area = |offset: u64, size: u64| -> Option<Range<u64>> { Some(offset..offset.checked_add(size)?) };
        let command = area(self.command_offset, self.command_size)?;
        let shared = area(self.shared_offset, self.shared_size)?;
        let fence = area(self.fence_offset, 8)?;
        let aligned = self.fence_offset.is_multiple_of(8)
            && self.command_offset.is_multiple_of(4)
            && self.shared_offset.is_multiple_of(16);
        let apart = |a: &Range<u64>, b: &Range<u64>| a.end <= b.start || b.end <= a.start;
        let disjoint = apart(&command, &shared) && apart(&command, &fence) && apart(&shared, &fence);
        let shared_limit = asked.max(MIN_SHARED_BYTES).checked_next_multiple_of(PAGE)?;
        let sizes = self.command_size <= MAX_COMMAND_BYTES && self.shared_size <= shared_limit;
        let end = command.end.max(shared.end).max(fence.end);
        // The areas, and a page at most around each.
        let needed = (8 + 3 * PAGE).checked_add(self.command_size)?.checked_add(self.shared_size)?;
        if !aligned || !disjoint || !sizes || end > needed {
            return None;
        }
        let range = |r: Range<u64>| Some(usize::try_from(r.start).ok()?..usize::try_from(r.end).ok()?);
        Some(Layout {
            command: range(command)?,
            shared: range(shared)?,
            fence: usize::try_from(fence.start).ok()?,
            size: usize::try_from(end).ok()?,
        })
    }
}

protocol! {
    /// 3D rendering contexts on the GPU.
    pub mod gpu = "gpu" {
        /// Creates this connection's context, with `shared` bytes of shared
        /// memory (the driver may give less, at least
        /// [`MIN_SHARED_BYTES`]); the session's memory is laid out as
        /// [`Session`] says.
        1 => fn open(shared: u64) -> Result<Session, GpuError>;
        /// Creates a resource and attaches it to the context. `backing`
        /// (offset, length) is a range of the shared area for its guest
        /// storage, or (0, 0). Returns its handle.
        2 => fn create(spec: ResourceSpec, backing_offset: u64, backing_len: u64) -> Result<u32, GpuError>;
        3 => fn destroy(resource: u32) -> ();
        /// Runs `words` 32-bit words of commands from the start of the
        /// command area. Replies once the renderer has executed them, as far
        /// as the shared memory goes (the GPU may still be busy with them).
        4 => fn submit(words: u32) -> Result<(), GpuError>;
        /// Queues a fence after everything submitted. Replies at once with
        /// its number; it signals when the GPU has done that work.
        5 => fn fence() -> Result<u64, GpuError>;
        /// Creates a 2D render target (`spec`: one level, single sampled,
        /// a format of 32-bit pixels) whose storage is `memory`, rows
        /// `stride` bytes apart: a display's picture, which the GPU renders
        /// into in place (the compositor draws its frames so). Once a fence
        /// after the drawing has signaled, the memory holds what was drawn.
        /// Returns its handle; `Unavailable` if the GPU cannot.
        6 => fn import(spec: ResourceSpec, stride: u32, memory: Vmo) -> Result<u32, GpuError>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the driver VM's renderer gives: the fence word in the first
    /// page, then a MiB of commands, then the shared area.
    fn renderer(shared: u64) -> Areas {
        let commands = 1 << 20;
        Areas {
            command_offset: PAGE,
            command_size: commands,
            shared_offset: PAGE + commands,
            shared_size: shared,
            fence_offset: 0,
        }
    }

    #[test]
    fn the_renderers_layout_is_accepted() {
        let asked = 16 * 1024 * 1024 + 16 * 1024;
        let l = renderer(asked).layout(asked).unwrap();
        assert_eq!(l.fence, 0);
        assert_eq!(l.command, 4096..4096 + (1 << 20));
        assert_eq!(l.shared, 4096 + (1 << 20)..4096 + (1 << 20) + asked as usize);
        assert_eq!(l.size, l.shared.end);
        // Less than the least is rounded up to it.
        assert!(renderer(MIN_SHARED_BYTES).layout(4096).is_some());
    }

    #[test]
    fn areas_outside_the_memory_or_wrapping_are_refused() {
        let asked = 1 << 20;
        // An offset whose end wraps around (the old client mapped a few
        // bytes and wrote its commands far outside them).
        let wrap = Areas { command_offset: u64::MAX - 7, ..renderer(asked) };
        assert_eq!(wrap.layout(asked), None);
        let wrap = Areas { shared_offset: u64::MAX - 15, shared_size: 32, ..renderer(asked) };
        assert_eq!(wrap.layout(asked), None);
        let wrap = Areas { fence_offset: u64::MAX - 7, ..renderer(asked) };
        assert_eq!(wrap.layout(asked), None);
        // An area far beyond the others: the memory would be mostly gap.
        let far = Areas { fence_offset: 1 << 40, ..renderer(asked) };
        assert_eq!(far.layout(asked), None);
        let far = Areas { command_offset: 1 << 40, ..renderer(asked) };
        assert_eq!(far.layout(asked), None);
    }

    #[test]
    fn overlapping_areas_are_refused() {
        let asked = 1 << 20;
        let r = renderer(asked);
        // The fence word inside the commands, or the shared area.
        assert_eq!(Areas { fence_offset: r.command_offset + 64, ..r }.layout(asked), None);
        assert_eq!(Areas { fence_offset: r.shared_offset, ..r }.layout(asked), None);
        // Commands running into the shared area.
        assert_eq!(Areas { command_size: r.command_size + 16, ..r }.layout(asked), None);
        assert_eq!(Areas { shared_offset: r.command_offset, ..r }.layout(asked), None);
    }

    #[test]
    fn misaligned_areas_are_refused() {
        let asked = 1 << 20;
        let r = renderer(asked);
        assert_eq!(Areas { fence_offset: 4, ..r }.layout(asked), None);
        assert_eq!(
            Areas { command_offset: r.command_offset + 2, command_size: r.command_size - 2, ..r }.layout(asked),
            None
        );
        assert_eq!(Areas { shared_offset: r.shared_offset + 8, ..r }.layout(asked), None);
    }

    #[test]
    fn areas_larger_than_allowed_are_refused() {
        let asked = 4 << 20;
        // More shared memory than asked for (rounded to a page)...
        assert_eq!(renderer(asked + PAGE).layout(asked), None);
        assert!(renderer(asked).layout(asked).is_some());
        // ... and more commands than a submission may have.
        let r = renderer(asked);
        let big =
            Areas { command_size: MAX_COMMAND_BYTES + 4, shared_offset: r.command_offset + MAX_COMMAND_BYTES + 4, ..r };
        assert_eq!(big.layout(asked), None);
    }
}
