// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What is kept between sessions: the career's best results and the
//! settings, in `~/.config/interceptor/save` (the text of
//! [`Save::to_text`]), read at start and written whenever they change.

use interceptor_game::career::Save;
use vproto::fs::vfs;
use vrt::object::Vmo;

const DIR: &str = "/home/user/.config/interceptor";
const PATH: &str = "/home/user/.config/interceptor/save";
/// The largest save read (bytes): what the game writes is well under a
/// kilobyte; anything much larger is not its file.
const MOST: u64 = 8 * 1024;

/// The save file's keeper.
pub struct Store {
    vfs: Option<vfs::Client>,
    /// Its directory has been made (it is made before the first save).
    made: bool,
}

impl Store {
    pub fn new() -> Store {
        Store { vfs: vproto::connect(vfs::NAME).ok().map(vfs::Client::new), made: false }
    }

    /// The save, or a new one where there is none (or it cannot be read,
    /// or is too large to be one).
    pub fn load(&self) -> Save {
        let Some(vfs) = &self.vfs else { return Save::default() };
        let Some(stat) = vfs.stat(PATH.into()).ok().and_then(|r| r.ok()) else { return Save::default() };
        if stat.size > MOST {
            // Not read (and replaced at the next save).
            println!("{PATH} is too large to be a save: ignored");
            return Save::default();
        }
        let Some((vmo, len)) = vfs.read_file(PATH.into()).ok().and_then(|r| r.ok()) else {
            return Save::default();
        };
        // No more than was looked at, should it have grown since.
        let mut buf: Vec<u8> = vec![0; len.min(MOST) as usize];
        if vmo.read(0, &mut buf).is_err() {
            return Save::default();
        }
        Save::parse(&String::from_utf8_lossy(&buf))
    }

    /// Writes `save`; false if it could not be.
    pub fn store(&mut self, save: &Save) -> bool {
        let Some(vfs) = &self.vfs else { return false };
        let text = save.to_text();
        let data = text.as_bytes();
        // The directories, the first time (each may exist already).
        if !self.made {
            let mut dir = String::new();
            for part in DIR.split('/').filter(|p| !p.is_empty()) {
                dir.push('/');
                dir.push_str(part);
                let _ = vfs.mkdir(dir.clone());
            }
            self.made = true;
        }
        let Ok(vmo) = Vmo::create(data.len().max(1)) else { return false };
        if vmo.write(0, data).is_err() {
            return false;
        }
        let stored = matches!(vfs.write_file(PATH.into(), vmo, data.len() as u64), Ok(Ok(())));
        // Its directory made again next time, should it have gone.
        self.made &= stored;
        stored
    }
}
