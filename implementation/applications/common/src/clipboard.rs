// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Copying text to the clipboard, knowing whether it got there.
//!
//! The window system takes the text in one message, of at most
//! [`CLIPBOARD_MAX`] bytes; a larger text is not sent at all. An application
//! that removes what it copies (Cut) must remove it only once it is copied.

use core::fmt;

use proto::display::{CLIPBOARD_MAX, DisplayError};
use ui::Ui;

/// Why text did not get to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardError {
    /// It is longer than the clipboard takes.
    TooLarge,
    /// None of the application's windows has the focus: only the focused
    /// one's process may use the clipboard.
    NotFocused,
    /// The window system did not answer.
    Unavailable,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardError::TooLarge => {
                write!(f, "the text is too long for the clipboard, which holds about {} KB", CLIPBOARD_MAX / 1000)
            }
            ClipboardError::NotFocused => f.write_str("only the window with the focus can use the clipboard"),
            ClipboardError::Unavailable => f.write_str("the clipboard is not available"),
        }
    }
}

/// Puts `text` on the clipboard.
pub fn copy(ui: &mut Ui, text: &str) -> Result<(), ClipboardError> {
    if text.len() > CLIPBOARD_MAX {
        return Err(ClipboardError::TooLarge);
    }
    match ui.ctx.display.set_clipboard(text.into()) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(DisplayError::Invalid)) | Err(ipc::IpcError::TooLarge) => Err(ClipboardError::TooLarge),
        Ok(Err(DisplayError::Denied)) => Err(ClipboardError::NotFocused),
        Ok(Err(_)) | Err(_) => Err(ClipboardError::Unavailable),
    }
}
