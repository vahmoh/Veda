// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Miscellaneous system calls: logging, clocks, sleeping, system info.

use abi::{Error, RawHandle, SystemInfo, clock, power_action, resource_kind};

use super::{SysResult, deadline, get_resource, ok};
use crate::memory::user;
use crate::scheduler::{self, WakeReason};
use crate::time;

pub fn debug_write(ptr: usize, len: usize) -> SysResult {
    let len = len.min(4096);
    let bytes = user::read_vec(ptr as u64, len, 4096)?;
    let cur = scheduler::current();
    crate::log::write_user(cur.process_name(), &bytes);
    ok(len)
}

pub fn log_read(offset: usize, buf: usize, len: usize) -> SysResult {
    super::task::allowed(abi::restrict::NO_LOG)?;
    let mut tmp = user::buffer(len.min(64 * 1024))?;
    let (n, next) = crate::log::read(offset as u64, &mut tmp);
    user::copy_to_user(buf as u64, &tmp[..n])?;
    Ok((n, Some(next as usize)))
}

pub fn clock_get(id: usize) -> SysResult {
    match id {
        clock::MONOTONIC => ok(time::now_ns() as usize),
        clock::REALTIME => ok(time::realtime_ns() as usize),
        clock::UTC => ok(time::utc_ns() as usize),
        _ => Err(Error::InvalidArgs),
    }
}

pub fn clock_info(out: usize) -> SysResult {
    crate::memory::user::write(out as u64, &time::info())?;
    ok(0)
}

pub fn sleep(d: usize) -> SysResult {
    match deadline(d as u64) {
        Some(d) => match scheduler::sleep_until(d) {
            WakeReason::Killed => Err(Error::Canceled),
            _ => ok(0),
        },
        None => match scheduler::block(None) {
            WakeReason::Killed => Err(Error::Canceled),
            _ => ok(0),
        },
    }
}

pub fn system_info(ptr: usize) -> SysResult {
    let (total, _) = crate::memory::phys::stats();
    let free = crate::memory::phys::available_to_user();
    let mut version = [0u8; 32];
    let v = concat!("Veda ", env!("CARGO_PKG_VERSION"));
    version[..v.len()].copy_from_slice(v.as_bytes());
    let procs = crate::object::process::all();
    let info = SystemInfo {
        version,
        cpu_count: crate::percpu::online().count() as u32,
        page_size: crate::memory::PAGE_SIZE as u32,
        total_memory: total,
        free_memory: free,
        uptime_ns: time::now_ns(),
        process_count: procs.iter().filter(|p| !p.is_terminated()).count() as u32,
        thread_count: procs.iter().map(|p| p.thread_count() as u32).sum(),
        idle_time_ns: scheduler::total_idle_ns(),
        cpu_brand: crate::architecture::cpu::brand_string(),
        timer_interrupts: scheduler::total_timer_interrupts(),
    };
    user::write(ptr as u64, &info)?;
    ok(0)
}

pub fn system_power(res: RawHandle, action: usize) -> SysResult {
    let r = get_resource(res, abi::Rights::NONE)?;
    if !r.permits(resource_kind::POWER, 0, 0) {
        return Err(Error::AccessDenied);
    }
    match action {
        power_action::POWER_OFF => {
            crate::kinfo!("power: shutting down");
            crate::architecture::power::off();
            Err(Error::NotSupported)
        }
        power_action::REBOOT => {
            crate::kinfo!("power: rebooting");
            crate::architecture::power::reboot();
            Err(Error::NotSupported)
        }
        _ => Err(Error::InvalidArgs),
    }
}

pub fn random(buf: usize, len: usize) -> SysResult {
    if len > 4096 {
        return Err(Error::InvalidArgs);
    }
    let mut tmp = user::buffer(len)?;
    crate::random::fill(&mut tmp);
    user::copy_to_user(buf as u64, &tmp)?;
    ok(len)
}
