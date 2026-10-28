// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Giving up, for good, what a program does not need before it handles what
//! it cannot trust (`<veda/sandbox.h>`), as the browser's page processes
//! do: the files it reaches, the services it reaches, and abilities the
//! kernel gives.
//!
//! * [`veda_confine_files`] confines the layer's connection to the VFS,
//!   which the program's files and Unix domain sockets all go through, to a
//!   few places (`vfs::confine`).
//! * [`veda_confine_services`] confines the layer's registry channel to a
//!   few services (`registry::confine`), and so the channels cloned from
//!   it from then on: a program on Rust's standard library takes its own
//!   from it (`veda_registry`), and a child would. Without the network
//!   service, the program's Internet sockets connect, bind and listen no
//!   more, nor are names looked up (`EACCES`). A sandbox leaves the VFS's
//!   name out too, or a connection of its own would be a way round the
//!   first.
//! * [`veda_process_restrict`] gives up creating, listing and opening
//!   processes, reading the kernel log, and new executable memory
//!   (`process_restrict`).
//! * [`veda_lend_files`] and [`veda_borrow_files`] lend a sandboxed program
//!   a place to read, which a program that is not sandboxed reaches and
//!   hands it a loan of (`vfs::lend`, `vfs::borrow`), and
//!   [`veda_take_back_files`] takes it back (`vfs::take_back`): the browser
//!   lends its network process the folder of a page the user opens, while
//!   a page it has open needs it.
//!
//! What the program holds already (open files, sockets, connections) stays.
//! Each returns 0, or an `errno` value.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int, c_uint};

use ipc::Bytes;

use proto::fs::{LOAN_SIZE, MAX_PLACES, Place};
use proto::init::RegistryError;

use crate::linux::errno::{EACCES, EFAULT, EILSEQ, EINVAL, EIO};
use crate::{error, network, path, vfs};

/// A place of [`veda_confine_files`], as `<veda/sandbox.h>` has it.
#[repr(C)]
pub struct VedaPlace {
    /// Absolute, or from the working directory.
    pub path: *const c_char,
    /// Nonzero: what is there may be changed too.
    pub write: c_int,
}

/// Turns a result into what the functions return: 0, or an `errno` value.
fn status(r: Result<(), isize>) -> c_int {
    match r {
        Ok(()) => 0,
        Err(e) => e as c_int,
    }
}

/// `veda_confine_files(places, count)`: the program's files are those of
/// the `count` places from now on (`vfs::confine`).
///
/// # Safety
/// `places` holds `count` places (it may be null for none), whose paths
/// are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_confine_files(places: *const VedaPlace, count: usize) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    status((|| {
        if count > MAX_PLACES {
            return Err(EINVAL);
        }
        if places.is_null() && count > 0 {
            return Err(EFAULT);
        }
        let cwd = vfs::cwd();
        let mut view = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: as the caller promises.
            let place = unsafe { &*places.add(i) };
            if place.path.is_null() {
                return Err(EFAULT);
            }
            // SAFETY: as the caller promises.
            let name = unsafe { CStr::from_ptr(place.path) }.to_bytes();
            let path = path::resolve(&cwd, name)?.path;
            view.push(Place { path, write: place.write != 0 });
        }
        error::ipc(vfs::with(|c| c.confine(view))?)
    })())
}

/// The absolute path of NUL-terminated `path` (absolute, or from the
/// working directory).
///
/// # Safety
/// `path` is null or NUL-terminated.
unsafe fn absolute(path: *const c_char) -> Result<String, isize> {
    if path.is_null() {
        return Err(EFAULT);
    }
    // SAFETY: as the caller promises.
    let name = unsafe { CStr::from_ptr(path) }.to_bytes();
    Ok(path::resolve(&vfs::cwd(), name)?.path)
}

/// `veda_lend_files(path, loan)`: lends the place `path` (absolute, or from
/// the working directory) to read, and writes the loan, `VEDA_LOAN_SIZE`
/// bytes, to `loan` (`vfs::lend`). EACCES if the program is sandboxed,
/// ENOENT if nothing is there.
///
/// # Safety
/// `path` is NUL-terminated; `loan` has room for `LOAN_SIZE` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_lend_files(path: *const c_char, loan: *mut u8) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    status((|| {
        // SAFETY: as the caller promises.
        let path = unsafe { absolute(path) }?;
        if loan.is_null() {
            return Err(EFAULT);
        }
        let secret = error::ipc(vfs::with(|c| c.lend(path))?)?;
        if secret.0.len() != LOAN_SIZE {
            return Err(EIO);
        }
        // SAFETY: as the caller promises.
        unsafe { core::ptr::copy_nonoverlapping(secret.0.as_ptr(), loan, LOAN_SIZE) };
        Ok(())
    })())
}

/// `veda_borrow_files(path, loan)`: takes `loan`, of the place `path` as it
/// was lent: the program reaches it, to read, from now on (`vfs::borrow`).
/// EACCES if there is no such loan.
///
/// # Safety
/// `path` is NUL-terminated; `loan` holds `LOAN_SIZE` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_borrow_files(path: *const c_char, loan: *const u8) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    status((|| {
        // SAFETY: as the caller promises.
        let path = unsafe { absolute(path) }?;
        if loan.is_null() {
            return Err(EFAULT);
        }
        // SAFETY: as the caller promises.
        let secret = unsafe { core::slice::from_raw_parts(loan, LOAN_SIZE) }.to_vec();
        error::ipc(vfs::with(|c| c.borrow(path, Bytes(secret)))?)
    })())
}

/// `veda_take_back_files(loan)`: takes back `loan`, which the program made
/// (`vfs::take_back`): the sandboxed program that took it reaches its
/// place no more. Nothing left to take back is no error.
///
/// # Safety
/// `loan` holds `LOAN_SIZE` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_take_back_files(loan: *const u8) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    status((|| {
        if loan.is_null() {
            return Err(EFAULT);
        }
        // SAFETY: as the caller promises.
        let secret = unsafe { core::slice::from_raw_parts(loan, LOAN_SIZE) }.to_vec();
        error::ipc(vfs::with(|c| c.take_back(Bytes(secret)))?)
    })())
}

/// `veda_confine_services(names, count)`: the program reaches the `count`
/// services named, and no other, from now on (`registry::confine`).
///
/// # Safety
/// `names` holds `count` NUL-terminated names (it may be null for none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veda_confine_services(names: *const *const c_char, count: usize) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    status((|| {
        if names.is_null() && count > 0 {
            return Err(EFAULT);
        }
        let mut services: Vec<String> = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: as the caller promises.
            let name = unsafe { *names.add(i) };
            if name.is_null() {
                return Err(EFAULT);
            }
            // SAFETY: as the caller promises.
            let name = unsafe { CStr::from_ptr(name) }.to_str().map_err(|_| EILSEQ)?;
            services.push(name.into());
        }
        let network = services.iter().any(|n| n == proto::net::net::NAME);
        match proto::with_registry(|r| r.confine(services)) {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(RegistryError::Denied))) => return Err(EACCES),
            _ => return Err(EIO),
        }
        if !network {
            network::refuse();
        }
        Ok(())
    })())
}

/// `veda_process_restrict(flags)`: takes `flags` (`abi::restrict`, as
/// `VEDA_RESTRICT_*`) from the process, for good.
#[unsafe(no_mangle)]
pub extern "C" fn veda_process_restrict(flags: c_uint) -> c_int {
    let _layer = crate::interrupt::Layer::enter();
    match runtime::process::restrict(flags as usize) {
        Ok(()) => 0,
        Err(e) => error::kernel(e) as c_int,
    }
}
