// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! OpenGL ES 3.0's functions, on the current context
//! ([`crate::current::with_gl`]): each takes C's arguments (pointers and
//! counts, `GLboolean`s, offsets passed as pointers) to `gl`'s, which
//! checks them as the specification says. What `gl` cannot see, the
//! functions here check: negative counts and sizes (`GL_INVALID_VALUE`)
//! and client-side arrays (`GL_INVALID_OPERATION`).
//!
//! Also here: the entry points of the OpenGL ES 2.0 extensions `gl` lists,
//! which name 3.0's (`glBindVertexArrayOES`, `glDrawBuffersEXT`, ...).

pub mod buffers;
pub mod draw;
pub mod framebuffers;
pub mod programs;
pub mod queries;
pub mod state;
pub mod textures;

use std::borrow::Cow;
use std::ffi::CStr;

use crate::{GLboolean, GLchar, GLsizei};

/// `n` values at `p`: none for a null pointer or a count below 1.
///
/// # Safety
/// `p` is null or points at `n` values, which live as long as the slice.
pub(crate) unsafe fn slice<'a, T>(p: *const T, n: usize) -> &'a [T] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        // SAFETY: as the caller says.
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

/// `n` values at `p`, to write: none for a null pointer or a count below 1.
///
/// # Safety
/// `p` is null or points at room for `n` values, which nothing else uses
/// while the slice lives.
pub(crate) unsafe fn slice_mut<'a, T>(p: *mut T, n: usize) -> &'a mut [T] {
    if p.is_null() || n == 0 {
        &mut []
    } else {
        // SAFETY: as the caller says.
        unsafe { std::slice::from_raw_parts_mut(p, n) }
    }
}

/// A count, if it is not negative.
pub(crate) fn count(n: GLsizei) -> Option<usize> {
    usize::try_from(n).ok()
}

/// A NUL-terminated string (bytes that are no UTF-8 replaced): empty for a
/// null pointer.
///
/// # Safety
/// `p` is null or points at a NUL-terminated string.
pub(crate) unsafe fn c_str<'a>(p: *const GLchar) -> Cow<'a, str> {
    if p.is_null() {
        return Cow::Borrowed("");
    }
    // SAFETY: as the caller says.
    String::from_utf8_lossy(unsafe { CStr::from_ptr(p) }.to_bytes())
}

/// `n` strings at `strings`, each `lengths`' length long (or up to its NUL,
/// for no lengths, or a negative one).
///
/// # Safety
/// `strings` points at `n` strings, `lengths` at `n` lengths or is null.
pub(crate) unsafe fn c_strings(strings: *const *const GLchar, lengths: *const i32, n: usize) -> Vec<String> {
    // SAFETY: as the caller says.
    let (strings, lengths) = unsafe { (slice(strings, n), slice(lengths, if lengths.is_null() { 0 } else { n })) };
    strings
        .iter()
        .enumerate()
        .map(|(i, &s)| match lengths.get(i).and_then(|&l| usize::try_from(l).ok()) {
            // SAFETY: the string has `l` bytes.
            Some(l) => String::from_utf8_lossy(unsafe { slice(s.cast::<u8>(), l) }).into_owned(),
            // SAFETY: the string ends with a NUL.
            None => unsafe { c_str(s) }.into_owned(),
        })
        .collect()
}

/// Writes `s` as OpenGL ES returns strings: at most `buf_size - 1` bytes of
/// it and a NUL at `out`, and how many bytes (without the NUL) at `length`.
///
/// # Safety
/// `out` is null or has room for `buf_size` bytes; `length` is null or
/// valid for a write.
pub(crate) unsafe fn put_str(s: &str, buf_size: GLsizei, length: *mut GLsizei, out: *mut GLchar) {
    let room = count(buf_size).unwrap_or(0);
    let n = s.len().min(room.saturating_sub(1));
    if !out.is_null() && room > 0 {
        // SAFETY: `out` has room for `buf_size >= n + 1` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), out.cast::<u8>(), n);
            out.add(n).write(0);
        }
    }
    if !length.is_null() {
        // SAFETY: as the caller says.
        unsafe { length.write(n as GLsizei) };
    }
}

/// Copies `values` to `out`, if it is not null.
///
/// # Safety
/// `out` is null or has room for `values.len()` values.
pub(crate) unsafe fn put_all<T: Copy>(values: &[T], out: *mut T) {
    if !out.is_null() {
        // SAFETY: as the caller says.
        unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), out, values.len()) };
    }
}

/// Writes `v` to `p`, if it is not null.
///
/// # Safety
/// `p` is null or valid for a write.
pub(crate) unsafe fn put<T>(p: *mut T, v: T) {
    if !p.is_null() {
        // SAFETY: as the caller says.
        unsafe { p.write(v) };
    }
}

/// A `GLboolean`'s truth.
pub(crate) fn boolean(b: GLboolean) -> bool {
    b != 0
}

/// A truth as a `GLboolean`.
pub(crate) fn gl_boolean(b: bool) -> GLboolean {
    GLboolean::from(b)
}
