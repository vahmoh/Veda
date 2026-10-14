// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Linux's DRM, the part a display driver of Veda's needs: the card's
//! connectors and modes, framebuffers of imported memory (PRIME) or of the
//! card's own (dumb buffers), setting a mode with the picture where it goes
//! on the display (atomically: the primary plane, scaled by the display
//! engine when the picture is not the mode's size), page flips and their
//! events. Its ioctls as `drm.h` and `drm_mode.h` define them, made through
//! `rustix`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd};

use rustix::ioctl::{Opcode, Updater, ioctl, opcode};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

const DRM: u8 = b'd';

/// `drm_version`.
#[repr(C)]
struct Version {
    major: i32,
    minor: i32,
    patch: i32,
    name_len: usize,
    name: usize,
    date_len: usize,
    date: usize,
    desc_len: usize,
    desc: usize,
}

/// `drm_get_cap`, `drm_set_client_cap`.
#[repr(C)]
struct Cap {
    capability: u64,
    value: u64,
}

/// `drm_prime_handle`.
#[repr(C)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

/// `drm_mode_card_res`.
#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_ids: u64,
    crtc_ids: u64,
    connector_ids: u64,
    encoder_ids: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

/// `drm_mode_modeinfo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ModeInfo {
    /// Pixel clock, kHz.
    pub clock: u32,
    pub hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    pub htotal: u16,
    hskew: u16,
    pub vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    pub vtotal: u16,
    vscan: u16,
    pub vrefresh: u32,
    flags: u32,
    pub kind: u32,
    name: [u8; 32],
}

impl ModeInfo {
    /// The time a frame takes (ns).
    pub fn period_ns(&self) -> u64 {
        let pixels = self.htotal as u64 * self.vtotal as u64;
        if self.clock == 0 || pixels == 0 { 0 } else { pixels * 1_000_000 / self.clock as u64 }
    }

    pub fn preferred(&self) -> bool {
        self.kind & MODE_TYPE_PREFERRED != 0
    }

    /// Its width and height.
    pub fn size(&self) -> (u32, u32) {
        (self.hdisplay as u32, self.vdisplay as u32)
    }
}

pub const MODE_TYPE_PREFERRED: u32 = 1 << 3;

/// `drm_mode_get_encoder`.
#[repr(C)]
#[derive(Default)]
struct Encoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

/// `drm_mode_get_connector`.
#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders: u64,
    modes: u64,
    props: u64,
    prop_values: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

/// `drm_mode_fb_cmd2`.
#[repr(C)]
#[derive(Default)]
struct FbCmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

/// `drm_mode_crtc_page_flip`.
#[repr(C)]
struct PageFlip {
    crtc_id: u32,
    fb_id: u32,
    flags: u32,
    reserved: u32,
    user_data: u64,
}

/// `drm_mode_create_dumb`.
#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

/// `drm_mode_map_dumb`.
#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

/// `drm_mode_get_plane_res`.
#[repr(C)]
#[derive(Default)]
struct PlaneRes {
    plane_ids: u64,
    count_planes: u32,
    pad: u32,
}

/// `drm_mode_get_plane`.
#[repr(C)]
#[derive(Default)]
struct GetPlane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_types: u64,
}

/// `drm_mode_obj_get_properties`.
#[repr(C)]
#[derive(Default)]
struct ObjProperties {
    props: u64,
    prop_values: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
    pad: u32,
}

/// `drm_mode_get_property`.
#[repr(C)]
#[derive(Default)]
struct GetProperty {
    values: u64,
    enum_blobs: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

/// `drm_mode_create_blob`.
#[repr(C)]
#[derive(Default)]
struct CreateBlob {
    data: u64,
    length: u32,
    blob_id: u32,
}

/// `drm_mode_atomic`.
#[repr(C)]
#[derive(Default)]
struct Atomic {
    flags: u32,
    count_objs: u32,
    objs: u64,
    count_props: u64,
    props: u64,
    prop_values: u64,
    reserved: u64,
    user_data: u64,
}

const _: () = assert!(size_of::<Version>() == 64);
const _: () = assert!(size_of::<CardRes>() == 64);
const _: () = assert!(size_of::<ModeInfo>() == 68);
const _: () = assert!(size_of::<GetConnector>() == 80);
const _: () = assert!(size_of::<FbCmd2>() == 104);
const _: () = assert!(size_of::<PageFlip>() == 24);
const _: () = assert!(size_of::<PlaneRes>() == 16);
const _: () = assert!(size_of::<GetPlane>() == 32);
const _: () = assert!(size_of::<ObjProperties>() == 32);
const _: () = assert!(size_of::<GetProperty>() == 64);
const _: () = assert!(size_of::<CreateBlob>() == 16);
const _: () = assert!(size_of::<Atomic>() == 56);

const VERSION: Opcode = opcode::read_write::<Version>(DRM, 0x00);
/// `struct drm_gem_close`: the handle, and padding.
const GEM_CLOSE: Opcode = opcode::write::<[u32; 2]>(DRM, 0x09);
const GET_CAP: Opcode = opcode::read_write::<Cap>(DRM, 0x0C);
const SET_CLIENT_CAP: Opcode = opcode::write::<Cap>(DRM, 0x0D);
const PRIME_FD_TO_HANDLE: Opcode = opcode::read_write::<PrimeHandle>(DRM, 0x2E);
const MODE_GETRESOURCES: Opcode = opcode::read_write::<CardRes>(DRM, 0xA0);
const MODE_GETENCODER: Opcode = opcode::read_write::<Encoder>(DRM, 0xA6);
const MODE_GETCONNECTOR: Opcode = opcode::read_write::<GetConnector>(DRM, 0xA7);
/// Of a framebuffer's id.
const MODE_RMFB: Opcode = opcode::read_write::<u32>(DRM, 0xAF);
const MODE_PAGE_FLIP: Opcode = opcode::read_write::<PageFlip>(DRM, 0xB0);
const MODE_CREATE_DUMB: Opcode = opcode::read_write::<CreateDumb>(DRM, 0xB2);
const MODE_MAP_DUMB: Opcode = opcode::read_write::<MapDumb>(DRM, 0xB3);
const MODE_ADDFB2: Opcode = opcode::read_write::<FbCmd2>(DRM, 0xB8);
const MODE_GETPROPERTY: Opcode = opcode::read_write::<GetProperty>(DRM, 0xAA);
const MODE_GETPLANERESOURCES: Opcode = opcode::read_write::<PlaneRes>(DRM, 0xB5);
const MODE_GETPLANE: Opcode = opcode::read_write::<GetPlane>(DRM, 0xB6);
const MODE_OBJ_GETPROPERTIES: Opcode = opcode::read_write::<ObjProperties>(DRM, 0xB9);
const MODE_ATOMIC: Opcode = opcode::read_write::<Atomic>(DRM, 0xBC);
const MODE_CREATEPROPBLOB: Opcode = opcode::read_write::<CreateBlob>(DRM, 0xBD);
/// Of a property blob's id.
const MODE_DESTROYPROPBLOB: Opcode = opcode::read_write::<u32>(DRM, 0xBE);

/// `DRM_FORMAT_XRGB8888`: blue in the low byte.
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const CAP_TIMESTAMP_MONOTONIC: u64 = 0x6;
const CLIENT_CAP_ATOMIC: u64 = 3;
const OBJECT_CRTC: u32 = 0xCCCC_CCCC;
const OBJECT_CONNECTOR: u32 = 0xC0C0_C0C0;
const OBJECT_PLANE: u32 = 0xEEEE_EEEE;
const PLANE_TYPE_PRIMARY: u64 = 1;
const ATOMIC_TEST_ONLY: u32 = 0x100;
const ATOMIC_NONBLOCK: u32 = 0x200;
const ATOMIC_ALLOW_MODESET: u32 = 0x400;
const MODE_CONNECTED: u32 = 1;
const PAGE_FLIP_EVENT: u32 = 1;
const EVENT_FLIP_COMPLETE: u32 = 2;

/// A connected output: its connector, the CRTC that drives it (and that
/// CRTC's place in the card's list, which planes name it by), its modes.
pub struct Output {
    pub connector: u32,
    pub crtc: u32,
    crtc_index: u32,
    pub modes: Vec<ModeInfo>,
}

/// Where on the display a picture goes: its corner and its size there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What a CRTC's primary plane shows: framebuffer `fb` (`size`, its width
/// and height), at `at` on the display, which the display engine scales
/// the framebuffer to if that is not its size.
#[derive(Debug, Clone, Copy)]
pub struct Scanout {
    pub fb: u32,
    pub size: (u32, u32),
    pub at: Place,
}

/// A property of a KMS object: its name, id and value.
struct Property {
    name: String,
    id: u32,
    value: u64,
}

/// A page flip that completed: the request's own number, and the
/// vertical blank it happened at (Linux's monotonic clock, ns).
pub struct Flipped {
    pub user_data: u64,
    pub sequence: u32,
    pub blank_ns: u64,
}

/// A card (`/dev/dri/cardN`).
pub struct Card {
    file: File,
    /// Linux's driver ("bochs-drm").
    pub driver: String,
}

impl Card {
    pub fn open(path: &str) -> io::Result<Card> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut name = [0u8; 64];
        let mut v = Version {
            major: 0,
            minor: 0,
            patch: 0,
            name_len: name.len(),
            name: name.as_mut_ptr() as usize,
            date_len: 0,
            date: 0,
            desc_len: 0,
            desc: 0,
        };
        // SAFETY: the kernel writes at most `name_len` bytes of the name.
        unsafe { ioctl(&file, Updater::<VERSION, Version>::new(&mut v))? };
        let len = v.name_len.min(name.len());
        let driver = String::from_utf8_lossy(&name[..len]).into_owned();
        let card = Card { file, driver };
        // Flip timestamps on the monotonic clock (Linux's default), and
        // atomic modesetting, which places a picture on the display with
        // the planes (all of them, the primary one among them).
        let mut cap = Cap { capability: CAP_TIMESTAMP_MONOTONIC, value: 0 };
        card.call::<GET_CAP, _>(&mut cap)?;
        if cap.value != 1 {
            return Err(io::Error::other("flip timestamps are not on the monotonic clock"));
        }
        let mut set = Cap { capability: CLIENT_CAP_ATOMIC, value: 1 };
        card.call::<SET_CLIENT_CAP, _>(&mut set)
            .map_err(|e| io::Error::other(format!("no atomic modesetting: {e}")))?;
        Ok(card)
    }

    /// Its file, for `poll`.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Makes request `REQUEST` of the card with `arg`, the structure it
    /// takes (whose size is in the request).
    fn call<const REQUEST: Opcode, T>(&self, arg: &mut T) -> io::Result<()> {
        // SAFETY: `arg` is the structure the request takes, whose pointers
        // (if any) the caller set to memory of the sizes it gave.
        Ok(unsafe { ioctl(&self.file, Updater::<REQUEST, T>::new(arg)) }?)
    }

    /// The connected outputs, each with a CRTC that can drive it.
    pub fn outputs(&self) -> io::Result<Vec<Output>> {
        let mut res = CardRes::default();
        self.call::<MODE_GETRESOURCES, _>(&mut res)?;
        let mut crtcs = vec![0u32; res.count_crtcs as usize];
        let mut connectors = vec![0u32; res.count_connectors as usize];
        let mut encoders = vec![0u32; res.count_encoders as usize];
        let mut fill = CardRes {
            crtc_ids: crtcs.as_mut_ptr() as u64,
            connector_ids: connectors.as_mut_ptr() as u64,
            encoder_ids: encoders.as_mut_ptr() as u64,
            count_crtcs: res.count_crtcs,
            count_connectors: res.count_connectors,
            count_encoders: res.count_encoders,
            ..CardRes::default()
        };
        self.call::<MODE_GETRESOURCES, _>(&mut fill)?;
        let mut outputs = Vec::new();
        let mut taken = Vec::new();
        for &id in &connectors[..fill.count_connectors.min(res.count_connectors) as usize] {
            let mut c = GetConnector { connector_id: id, ..GetConnector::default() };
            self.call::<MODE_GETCONNECTOR, _>(&mut c)?;
            if c.connection != MODE_CONNECTED || c.count_modes == 0 {
                continue;
            }
            let mut modes = vec![ModeInfo::default(); c.count_modes as usize];
            let mut its_encoders = vec![0u32; c.count_encoders as usize];
            let mut fill = GetConnector {
                connector_id: id,
                modes: modes.as_mut_ptr() as u64,
                count_modes: c.count_modes,
                encoders: its_encoders.as_mut_ptr() as u64,
                count_encoders: c.count_encoders,
                ..GetConnector::default()
            };
            self.call::<MODE_GETCONNECTOR, _>(&mut fill)?;
            modes.truncate(fill.count_modes.min(c.count_modes) as usize);
            // A CRTC one of its encoders can use, and no other output has.
            let mut crtc = None;
            for &e in &its_encoders[..fill.count_encoders.min(c.count_encoders) as usize] {
                let mut enc = Encoder { encoder_id: e, ..Encoder::default() };
                self.call::<MODE_GETENCODER, _>(&mut enc)?;
                crtc = crtcs
                    .iter()
                    .enumerate()
                    .find(|&(i, c)| enc.possible_crtcs & (1 << i) != 0 && !taken.contains(c))
                    .map(|(i, &c)| (c, i as u32));
                if crtc.is_some() {
                    break;
                }
            }
            if let Some((crtc, crtc_index)) = crtc {
                taken.push(crtc);
                outputs.push(Output { connector: id, crtc, crtc_index, modes });
            }
        }
        Ok(outputs)
    }

    /// A framebuffer of memory imported from a dma-buf (`stride` bytes a
    /// row, XRGB8888): its id, or the error if the card cannot import.
    pub fn import_framebuffer(&self, dmabuf: impl AsFd, width: u32, height: u32, stride: u32) -> io::Result<u32> {
        let mut p = PrimeHandle { handle: 0, flags: 0, fd: dmabuf.as_fd().as_raw_fd() };
        self.call::<PRIME_FD_TO_HANDLE, _>(&mut p)?;
        let fb = self.framebuffer(p.handle, width, height, stride);
        // The framebuffer holds the buffer now (or nothing does).
        let mut close = [p.handle, 0];
        let _ = self.call::<GEM_CLOSE, _>(&mut close);
        fb
    }

    fn framebuffer(&self, handle: u32, width: u32, height: u32, stride: u32) -> io::Result<u32> {
        let mut f = FbCmd2 { width, height, pixel_format: XRGB8888, ..FbCmd2::default() };
        f.handles[0] = handle;
        f.pitches[0] = stride;
        self.call::<MODE_ADDFB2, _>(&mut f)?;
        Ok(f.fb_id)
    }

    /// A framebuffer of the card's own memory, mapped: its id, its stride,
    /// and its memory.
    pub fn dumb_framebuffer(&self, width: u32, height: u32) -> io::Result<(u32, u32, DumbMap)> {
        let mut d = CreateDumb { width, height, bpp: 32, ..CreateDumb::default() };
        self.call::<MODE_CREATE_DUMB, _>(&mut d)?;
        let fb = self.framebuffer(d.handle, width, height, d.pitch)?;
        let mut m = MapDumb { handle: d.handle, ..MapDumb::default() };
        self.call::<MODE_MAP_DUMB, _>(&mut m)?;
        let map = DumbMap::new(&self.file, m.offset, d.size as usize)?;
        Ok((fb, d.pitch, map))
    }

    pub fn remove_framebuffer(&self, fb: u32) {
        let mut id = fb;
        let _ = self.call::<MODE_RMFB, _>(&mut id);
    }

    /// Sets `output`'s `mode`, its CRTC's primary plane showing `scanout`
    /// (its flips keep the place), without waiting: a display takes a
    /// while to come up (a laptop's panel a second or more). Like a flip's,
    /// its completion is an event ([`Card::events`]), with `user_data`.
    pub fn set_mode(&self, output: &Output, mode: &ModeInfo, scanout: Scanout, user_data: u64) -> io::Result<()> {
        let flags = ATOMIC_NONBLOCK | ATOMIC_ALLOW_MODESET | PAGE_FLIP_EVENT;
        self.modeset(output, mode, scanout, flags, user_data)
    }

    /// What [`Card::set_mode`] would do, checked by Linux's driver: the
    /// error it would end in, if any. Changes nothing.
    pub fn test_mode(&self, output: &Output, mode: &ModeInfo, scanout: Scanout) -> io::Result<()> {
        self.modeset(output, mode, scanout, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, 0)
    }

    fn modeset(
        &self,
        output: &Output,
        mode: &ModeInfo,
        scanout: Scanout,
        flags: u32,
        user_data: u64,
    ) -> io::Result<()> {
        let plane = self.primary_plane(output)?;
        let blob = self.mode_blob(mode)?;
        let mut commit = Commit::default();
        let connector = self.properties(output.connector, OBJECT_CONNECTOR)?;
        commit.set(output.connector, &connector, "CRTC_ID", output.crtc as u64)?;
        // The connector's max bpc stays as it is: any change of it makes the
        // commit a full mode set, which powers a laptop's panel off and on.
        // Linux's i915 keeps the bits a colour the firmware drives the
        // display at instead (Veda's patch), so that the first commit only
        // changes the picture shown.
        let crtc = self.properties(output.crtc, OBJECT_CRTC)?;
        commit.set(output.crtc, &crtc, "MODE_ID", blob as u64)?;
        commit.set(output.crtc, &crtc, "ACTIVE", 1)?;
        let planes = self.properties(plane, OBJECT_PLANE)?;
        let Scanout { fb, size: (width, height), at } = scanout;
        for (name, value) in [
            ("FB_ID", fb as u64),
            ("CRTC_ID", output.crtc as u64),
            // The source's in 16.16 fixed point, the display's in pixels.
            ("SRC_X", 0),
            ("SRC_Y", 0),
            ("SRC_W", (width as u64) << 16),
            ("SRC_H", (height as u64) << 16),
            ("CRTC_X", at.x as u64),
            ("CRTC_Y", at.y as u64),
            ("CRTC_W", at.width as u64),
            ("CRTC_H", at.height as u64),
        ] {
            commit.set(plane, &planes, name, value)?;
        }
        let done = commit.commit(self, flags, user_data);
        // The CRTC's state holds the mode now, if it is set.
        let mut id = blob;
        let _ = self.call::<MODE_DESTROYPROPBLOB, _>(&mut id);
        done
    }

    /// The primary plane of `output`'s CRTC.
    fn primary_plane(&self, output: &Output) -> io::Result<u32> {
        let mut res = PlaneRes::default();
        self.call::<MODE_GETPLANERESOURCES, _>(&mut res)?;
        let mut ids = vec![0u32; res.count_planes as usize];
        let mut fill = PlaneRes { plane_ids: ids.as_mut_ptr() as u64, count_planes: res.count_planes, pad: 0 };
        self.call::<MODE_GETPLANERESOURCES, _>(&mut fill)?;
        ids.truncate(fill.count_planes.min(res.count_planes) as usize);
        for id in ids {
            let mut p = GetPlane { plane_id: id, ..GetPlane::default() };
            self.call::<MODE_GETPLANE, _>(&mut p)?;
            if p.possible_crtcs & (1 << output.crtc_index) == 0 {
                continue;
            }
            let primary =
                self.properties(id, OBJECT_PLANE)?.iter().any(|p| p.name == "type" && p.value == PLANE_TYPE_PRIMARY);
            if primary {
                return Ok(id);
            }
        }
        Err(io::Error::other("its CRTC has no primary plane"))
    }

    /// The properties of object `id` (of `kind`).
    fn properties(&self, id: u32, kind: u32) -> io::Result<Vec<Property>> {
        let mut count = ObjProperties { obj_id: id, obj_type: kind, ..ObjProperties::default() };
        self.call::<MODE_OBJ_GETPROPERTIES, _>(&mut count)?;
        let mut ids = vec![0u32; count.count_props as usize];
        let mut values = vec![0u64; count.count_props as usize];
        let mut fill = ObjProperties {
            props: ids.as_mut_ptr() as u64,
            prop_values: values.as_mut_ptr() as u64,
            count_props: count.count_props,
            obj_id: id,
            obj_type: kind,
            pad: 0,
        };
        self.call::<MODE_OBJ_GETPROPERTIES, _>(&mut fill)?;
        let n = fill.count_props.min(count.count_props) as usize;
        let mut out = Vec::with_capacity(n);
        for (&prop, &value) in ids[..n].iter().zip(&values[..n]) {
            let mut p = GetProperty { prop_id: prop, ..GetProperty::default() };
            self.call::<MODE_GETPROPERTY, _>(&mut p)?;
            let end = p.name.iter().position(|&b| b == 0).unwrap_or(p.name.len());
            out.push(Property { name: String::from_utf8_lossy(&p.name[..end]).into_owned(), id: prop, value });
        }
        Ok(out)
    }

    /// A property blob holding `mode` (for a CRTC's `MODE_ID`).
    fn mode_blob(&self, mode: &ModeInfo) -> io::Result<u32> {
        let mut b =
            CreateBlob { data: mode as *const ModeInfo as u64, length: size_of::<ModeInfo>() as u32, blob_id: 0 };
        self.call::<MODE_CREATEPROPBLOB, _>(&mut b)?;
        Ok(b.blob_id)
    }

    /// Shows `fb` from the next vertical blank on; [`Card::events`] says
    /// when, with `user_data`.
    pub fn flip(&self, crtc: u32, fb: u32, user_data: u64) -> io::Result<()> {
        let mut f = PageFlip { crtc_id: crtc, fb_id: fb, flags: PAGE_FLIP_EVENT, reserved: 0, user_data };
        self.call::<MODE_PAGE_FLIP, _>(&mut f)
    }

    /// The flips that completed (the card's file is readable then).
    pub fn events(&self) -> io::Result<Vec<Flipped>> {
        let mut buf = [0u8; 1024];
        let n = match rustix::io::read(&self.file, &mut buf) {
            Ok(n) => n,
            Err(rustix::io::Errno::AGAIN) => 0,
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        let mut rest = &buf[..n];
        while rest.len() >= 8 {
            let kind = u32::from_ne_bytes(rest[0..4].try_into().unwrap());
            let len = u32::from_ne_bytes(rest[4..8].try_into().unwrap()) as usize;
            if len < 8 || len > rest.len() {
                break;
            }
            // `drm_event_vblank`: the user's data, seconds and microseconds
            // of the blank, its sequence number, the CRTC.
            if kind == EVENT_FLIP_COMPLETE && len >= 32 {
                let word = |at: usize| u32::from_ne_bytes(rest[at..at + 4].try_into().unwrap());
                out.push(Flipped {
                    user_data: u64::from_ne_bytes(rest[8..16].try_into().unwrap()),
                    blank_ns: word(16) as u64 * 1_000_000_000 + word(20) as u64 * 1000,
                    sequence: word(24),
                });
            }
            rest = &rest[len..];
        }
        Ok(out)
    }
}

/// An atomic commit being made: the objects whose properties it sets, and
/// their values.
#[derive(Default)]
struct Commit {
    objects: Vec<(u32, Vec<(u32, u64)>)>,
}

impl Commit {
    /// Sets `object`'s property `name` (one of `properties`, its own) to
    /// `value`.
    fn set(&mut self, object: u32, properties: &[Property], name: &str, value: u64) -> io::Result<()> {
        let prop = properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.id)
            .ok_or_else(|| io::Error::other(format!("no property {name} on object {object}")))?;
        match self.objects.iter_mut().find(|(o, _)| *o == object) {
            Some((_, props)) => props.push((prop, value)),
            None => self.objects.push((object, vec![(prop, value)])),
        }
        Ok(())
    }

    /// Carries it out, all at once (with `flags`; its event, if it asks
    /// for one, has `user_data`).
    fn commit(&self, card: &Card, flags: u32, user_data: u64) -> io::Result<()> {
        // The objects, how many properties each sets, then those, object by
        // object, and their values.
        let objects: Vec<u32> = self.objects.iter().map(|&(o, _)| o).collect();
        let counts: Vec<u32> = self.objects.iter().map(|(_, p)| p.len() as u32).collect();
        let (props, values): (Vec<u32>, Vec<u64>) = self.objects.iter().flat_map(|(_, p)| p.iter().copied()).unzip();
        let mut a = Atomic {
            flags,
            count_objs: objects.len() as u32,
            objs: objects.as_ptr() as u64,
            count_props: counts.as_ptr() as u64,
            props: props.as_ptr() as u64,
            prop_values: values.as_ptr() as u64,
            reserved: 0,
            user_data,
        };
        card.call::<MODE_ATOMIC, _>(&mut a)
    }
}

/// A dumb buffer's memory, mapped.
pub struct DumbMap {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: plain memory, used by one thread at a time.
unsafe impl Send for DumbMap {}

impl DumbMap {
    fn new(card: &File, offset: u64, len: usize) -> io::Result<DumbMap> {
        // SAFETY: a new shared mapping of the card's file at the offset its
        // driver gave for the buffer.
        let ptr = unsafe {
            mmap(core::ptr::null_mut(), len, ProtFlags::READ | ProtFlags::WRITE, MapFlags::SHARED, card, offset)?
        };
        Ok(DumbMap { ptr: ptr.cast(), len })
    }

    /// Writes `rows` rows of `row_bytes` bytes from `from` (`from_stride`
    /// bytes apart) to the buffer (`stride` bytes apart).
    pub fn copy_from(&mut self, from: &[u8], from_stride: usize, stride: usize, row_bytes: usize, rows: usize) {
        for r in 0..rows {
            let (src, dst) = (r * from_stride, r * stride);
            if src + row_bytes > from.len() || dst + row_bytes > self.len {
                break;
            }
            // SAFETY: inside both, as checked.
            unsafe { core::ptr::copy_nonoverlapping(from.as_ptr().add(src), self.ptr.add(dst), row_bytes) };
        }
    }
}

impl Drop for DumbMap {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `new`, no longer used.
        let _ = unsafe { munmap(self.ptr.cast(), self.len) };
    }
}
