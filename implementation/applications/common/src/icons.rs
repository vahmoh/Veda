// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The icon and colour of a file's type, the same in Files and on the
//! desktop.

use files::kind::{FileKind, file_kind};
use files::path::extension;
use gfx::Color;
use ui::Icon;

/// Folders.
pub const FOLDER: Color = Color::hex(0xF5B43C);
const IMAGE: Color = Color::hex(0xE879C9);
const AUDIO: Color = Color::hex(0xF2766B);
const VIDEO: Color = Color::hex(0x3CC8F0);
const TEXT: Color = Color::hex(0x6EA8FE);
const PROGRAM: Color = Color::hex(0x4CC38A);
const OTHER: Color = Color::hex(0x9A9AA8);

/// The line icon of an item called `name` (`program` as for
/// [`files::kind::file_kind`]).
pub fn kind_icon(name: &str, is_dir: bool, program: bool) -> Icon {
    if is_dir {
        return Icon::Folder;
    }
    match file_kind(name, program) {
        FileKind::Image => Icon::Image,
        FileKind::Audio => Icon::Music,
        FileKind::Video => Icon::Video,
        FileKind::Text => Icon::Document,
        FileKind::Program => Icon::Cube,
        FileKind::Other => match extension(name).as_str() {
            "ttf" | "otf" => Icon::Edit,
            _ => Icon::File,
        },
    }
}

/// The accent colour of an item's type.
pub fn kind_color(name: &str, is_dir: bool, program: bool) -> Color {
    if is_dir {
        return FOLDER;
    }
    match file_kind(name, program) {
        FileKind::Image => IMAGE,
        FileKind::Audio => AUDIO,
        FileKind::Video => VIDEO,
        FileKind::Text => TEXT,
        FileKind::Program => PROGRAM,
        FileKind::Other => OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_by_type() {
        assert_eq!(kind_icon("Pictures", true, false), Icon::Folder);
        assert_eq!(kind_icon("cat.PNG", false, false), Icon::Image);
        assert_eq!(kind_icon("song.mp3", false, false), Icon::Music);
        assert_eq!(kind_icon("notes.txt", false, false), Icon::Document);
        assert_eq!(kind_icon("tool", false, true), Icon::Cube);
        assert_eq!(kind_icon("font.ttf", false, false), Icon::Edit);
        assert_eq!(kind_icon("data.bin", false, false), Icon::File);
        assert_eq!(kind_color("x", true, false), FOLDER);
    }
}
