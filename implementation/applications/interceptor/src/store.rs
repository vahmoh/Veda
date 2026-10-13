// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is kept between sessions: the career's best results and the
//! settings, in `~/.config/interceptor/save` (the text of
//! [`Save::to_text`]), read at start and written whenever they change.

use alloc::string::String;
use alloc::vec::Vec;

use interceptor_game::career::Save;
use vproto::fs::vfs;
use vrt::object::Vmo;

const DIR: &str = "/home/user/.config/interceptor";
const PATH: &str = "/home/user/.config/interceptor/save";

/// The save file's keeper.
pub struct Store {
    vfs: Option<vfs::Client>,
}

impl Store {
    pub fn new() -> Store {
        Store { vfs: vproto::connect(vfs::NAME).ok().map(vfs::Client::new) }
    }

    /// The save, or a new one where there is none (or it cannot be read).
    pub fn load(&self) -> Save {
        let Some(vfs) = &self.vfs else { return Save::default() };
        let Some((vmo, len)) = vfs.read_file(PATH.into()).ok().and_then(|r| r.ok()) else {
            return Save::default();
        };
        let mut buf: Vec<u8> = alloc::vec![0; len as usize];
        if vmo.read(0, &mut buf).is_err() {
            return Save::default();
        }
        Save::parse(&String::from_utf8_lossy(&buf))
    }

    /// Writes `save`; false if it could not be.
    pub fn store(&self, save: &Save) -> bool {
        let Some(vfs) = &self.vfs else { return false };
        let text = save.to_text();
        let data = text.as_bytes();
        // The directories, as needed (each may exist already).
        let mut dir = String::new();
        for part in DIR.split('/').filter(|p| !p.is_empty()) {
            dir.push('/');
            dir.push_str(part);
            let _ = vfs.mkdir(dir.clone());
        }
        let Ok(vmo) = Vmo::create(data.len().max(1)) else { return false };
        if vmo.write(0, data).is_err() {
            return false;
        }
        matches!(vfs.write_file(PATH.into(), vmo, data.len() as u64), Ok(Ok(())))
    }
}
