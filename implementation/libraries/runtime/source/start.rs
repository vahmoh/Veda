// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Process entry point, exit codes, and the note that marks the program as
//! a Veda program.

use abi::RawHandle;
use elf::veda;

/// An ELF note as a note section holds it: its header (the sizes of its
/// name and descriptor, and its type), then its name and descriptor, each
/// padded to 4 bytes.
#[repr(C, align(4))]
struct Note<const NAME: usize, const DESC: usize> {
    name_size: u32,
    desc_size: u32,
    kind: u32,
    name: [u8; NAME],
    desc: [u8; DESC],
}

/// [`veda::NOTE_NAME`] with its NUL, padded to 4 bytes.
const NAME_BYTES: usize = (veda::NOTE_NAME.len() + 1).next_multiple_of(4);

const fn padded_name() -> [u8; NAME_BYTES] {
    let mut name = [0; NAME_BYTES];
    let mut i = 0;
    while i < veda::NOTE_NAME.len() {
        name[i] = veda::NOTE_NAME[i];
        i += 1;
    }
    name
}

/// The note by which a loader knows this program for a Veda program
/// (`elf::veda`). The linker keeps every note section of the objects it
/// takes; [`start`] names this one, so that the object holding it is taken
/// wherever the entry point is.
#[used]
#[unsafe(link_section = ".note.veda")]
static PROGRAM_NOTE: Note<NAME_BYTES, 4> = Note {
    name_size: veda::NOTE_NAME.len() as u32 + 1,
    desc_size: 4,
    kind: veda::NOTE_TYPE,
    name: padded_name(),
    desc: veda::VERSION.to_le_bytes(),
};

/// Values a program's `main` may return.
pub trait IntoExitCode {
    fn into_exit_code(self) -> i32;
}

impl IntoExitCode for () {
    fn into_exit_code(self) -> i32 {
        0
    }
}

impl IntoExitCode for i32 {
    fn into_exit_code(self) -> i32 {
        self
    }
}

impl<E: core::fmt::Debug> IntoExitCode for Result<(), E> {
    fn into_exit_code(self) -> i32 {
        match self {
            Ok(()) => 0,
            Err(e) => {
                crate::println!("error: {:?}", e);
                1
            }
        }
    }
}

unsafe extern "Rust" {
    /// Defined by [`crate::entry!`] in the program crate.
    fn __rt_main() -> i32;
}

/// The process's start, from the architecture's entry point
/// (`architecture`): it takes the bootstrap channel the kernel passed, runs
/// `main` and exits.
pub(crate) fn start(bootstrap: RawHandle) -> ! {
    core::hint::black_box(&PROGRAM_NOTE);
    crate::env::adopt(bootstrap);
    // SAFETY: provided by the program through `runtime::entry!`.
    let code = unsafe { __rt_main() };
    crate::sys::process_exit(code as i64)
}

/// Declares the program's `main` function:
///
/// ```ignore
/// #![no_std]
/// #![no_main]
/// runtime::entry!(main);
/// fn main() { runtime::println!("hello"); }
/// ```
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub fn __rt_main() -> i32 {
            $crate::start::IntoExitCode::into_exit_code($main())
        }
    };
}
