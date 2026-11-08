// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Signals, as a Linux program has them: run in its code, on its stack (or
//! the alternate one, `sigaltstack`), in Linux's frame, from which
//! `rt_sigreturn` takes back the registers its handler leaves.
//!
//! The POSIX layer keeps what Linux's kernel keeps of signals (their
//! dispositions, masks, what is pending, who gets what), as for C
//! programs; Linux compatibility acts on them as the kernel does on the
//! way back to user code: as each system call returns, and as foreign code
//! stops for an exception or an interruption ([`deliver`]). A handler is
//! entered by laying out a frame on the stack and sending the thread
//! there; the thread then blocks what the handler's mask says. A system
//! call a signal ended starts again as Linux has it (`SA_RESTART`), by
//! going back over its instruction. A fault (a bad access, an invalid
//! instruction) is the signal it stands for, with where it happened
//! ([`fault`]), which ends the process if no handler takes it.

use posix::host::{Info, Next, Sigaction, Siginfo};

use crate::architecture;
use crate::linux::errno::*;
use crate::linux::sig;
use crate::task::Task;

/// A thread's alternate signal stack (`sigaltstack`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    /// `SS_DISABLE`, or 0 with `SS_AUTODISARM` perhaps.
    pub flags: i32,
}

impl Default for AltStack {
    fn default() -> Self {
        AltStack { sp: 0, size: 0, flags: sig::SS_DISABLE }
    }
}

impl AltStack {
    fn enabled(&self) -> bool {
        self.flags & sig::SS_DISABLE == 0
    }

    /// Whether `sp` is on it.
    pub fn on(&self, sp: u64) -> bool {
        self.enabled() && sp > self.sp && sp - self.sp <= self.size
    }

    /// Where a frame for a handler that runs on it (`onstack`) starts: its
    /// top, unless the thread is on it already (`None`: below `sp`).
    pub fn top_for(&self, sp: u64, onstack: bool) -> Option<u64> {
        // One that ends past the address space is no place for a frame,
        // which then fails.
        (onstack && self.enabled() && !self.on(sp)).then(|| self.sp.saturating_add(self.size))
    }

    /// Whether a frame at `frame` fits: not below the alternate stack, if
    /// it is on it (`sp` being where the thread was).
    pub fn fits(&self, frame: u64, sp: u64, switched: bool) -> bool {
        if switched || self.on(sp) { frame >= self.sp } else { true }
    }

    /// What the context in a frame says of it, the thread at `sp`:
    /// `ss_sp`, `ss_flags` (whether the thread is on it) and `ss_size`.
    pub fn reported(&self, sp: u64) -> (u64, i32, u64) {
        let flags = if !self.enabled() {
            sig::SS_DISABLE
        } else if self.on(sp) {
            sig::SS_ONSTACK | (self.flags & sig::SS_AUTODISARM)
        } else {
            self.flags & sig::SS_AUTODISARM
        };
        (self.sp, flags, self.size)
    }
}

/// A fault's signal, as the exception that raised it says: which signal,
/// how (`si_code`), where (`si_addr`), and the exception, as the frame's
/// context reports it (`architecture::Exception`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub signal: u32,
    pub code: i32,
    pub addr: u64,
    pub exception: architecture::Exception,
}

/// What a signal's frame holds and enters: the handler, its restorer (the
/// code it returns to, which makes `rt_sigreturn`), the mask the thread
/// goes back to, what the signal says (and whether the handler takes it:
/// `SA_SIGINFO`), and the fault it is, if it is one.
pub struct Frame {
    pub handler: u64,
    pub restorer: u64,
    pub restore_mask: u64,
    pub info: Siginfo,
    pub siginfo: bool,
    pub fault: Option<Fault>,
    pub onstack: bool,
}

/// Acts on the signals ready for thread `t` as it returns to its code: the
/// default action (ending the process, or nothing), or a handler entered
/// with a frame, one after the other; then starts again the system call a
/// signal ended, if no handler took that over.
pub fn deliver(t: &mut Task) {
    if t.restart.is_none() && !posix::host::signals_ready() {
        return;
    }
    while let Some(next) = posix::host::take_next() {
        match next.action.handler {
            sig::IGN => {}
            sig::DFL => {
                if abi::posix_signal::ends_process(next.signal as usize) {
                    end(t, next.signal);
                }
            }
            _ => enter_handler(t, &next),
        }
    }
    if let Some(r) = t.restart.take() {
        architecture::restart(&mut t.state, &r);
    }
}

/// Ends the process as signal `s` does, saying so in the log when it is
/// a fault's (a crash, for the person who looks).
fn end(t: &mut Task, s: u32) -> ! {
    // A child `vfork` made is a process of its own: it alone ends.
    if t.apart.is_some() {
        crate::task::end_apart(t, abi::EXIT_CODE_SIGNALED - i64::from(s));
    }
    if let Some(f) = t.fault.filter(|f| f.signal == s) {
        let (pc, sp) = architecture::describe(&t.state);
        runtime::println!(
            "{}: signal {} ({}) at {:#x}: code {:#x}, stack {:#x} (thread {})",
            crate::process_name(),
            s,
            if sig::dumps_core(s) { "a fault" } else { "ended" },
            f.addr,
            pc,
            sp,
            t.tid,
        );
    }
    posix::host::exit_by_signal(s)
}

/// Enters the handler of signal `next`: what the system call it ended
/// returns (or its starting again), the frame, and the mask.
fn enter_handler(t: &mut Task, next: &Next) {
    let action: &Sigaction = &next.action;
    if let Some(r) = t.restart.take() {
        let again = match r.code {
            ERESTARTSYS => action.flags & sig::SA_RESTART != 0,
            ERESTARTNOINTR => true,
            _ => false,
        };
        if again {
            architecture::restart(&mut t.state, &r);
        } else {
            architecture::set_result(&mut t.state, -EINTR);
        }
    }
    let fault = t.fault.take().filter(|f| f.signal == next.signal);
    // A handler returns through the restorer the C library gives; without
    // one, through the vDSO's, where the architecture's Linux has one
    // (ARM64's `__kernel_rt_sigreturn`). x86-64's has none: there is
    // nowhere to return to.
    let restorer =
        if action.flags & sig::SA_RESTORER != 0 { Some(action.restorer as u64) } else { crate::vdso::restorer() };
    let Some(restorer) = restorer else {
        force_sigsegv(next.signal);
        return;
    };
    let frame = Frame {
        handler: action.handler as u64,
        restorer,
        restore_mask: next.restore,
        info: next.info.siginfo(next.signal),
        siginfo: action.flags & sig::SA_SIGINFO != 0,
        fault,
        onstack: action.flags & sig::SA_ONSTACK != 0,
    };
    let sp = architecture::sp(&t.state);
    let switched = t.altstack.top_for(sp, frame.onstack).is_some();
    if architecture::push_frame(&mut t.state, &frame, &t.altstack).is_none() {
        force_sigsegv(next.signal);
        return;
    }
    posix::host::block_for_handler(next.signal, action);
    if switched && t.altstack.flags & sig::SS_AUTODISARM != 0 {
        t.altstack = AltStack::default();
    }
}

/// What Linux does when the frame of signal `s`'s handler cannot be had
/// (`force_sigsegv`): the thread is sent `SIGSEGV`, which ends the process
/// unless a handler can run; for `SIGSEGV` itself, whose handler could not
/// run either, its default action, which ends it.
fn force_sigsegv(s: u32) {
    let info = Info { code: crate::linux::si::KERNEL, ..Info::default() };
    if s == sig::SEGV {
        posix::host::force_default(sig::SEGV, info);
    } else {
        posix::host::force(sig::SEGV, info);
    }
}

/// `rt_sigreturn`: the registers, mask and alternate stack the frame on
/// the stack holds, as the handler left them; a frame that cannot be had
/// is `SIGSEGV`.
pub fn sigreturn(t: &mut Task) {
    match architecture::pop_frame(&mut t.state) {
        Some((mask, (sp, flags, size))) => {
            posix::host::restore_mask(mask);
            // As Linux restores it: as `sigaltstack` would set it, which
            // may refuse (the thread is on it).
            let _ = set_altstack(t, AltStack { sp, size, flags: flags & !sig::SS_ONSTACK });
        }
        None => posix::host::force(sig::SEGV, Info { code: crate::linux::si::KERNEL, ..Info::default() }),
    }
}

/// Sets thread `t`'s alternate stack to `new`, as `sigaltstack` does.
fn set_altstack(t: &mut Task, new: AltStack) -> Result<(), isize> {
    if t.altstack.on(architecture::sp(&t.state)) {
        return Err(EPERM);
    }
    let mode = new.flags & !sig::SS_AUTODISARM;
    if mode != 0 && mode != sig::SS_DISABLE {
        return Err(EINVAL);
    }
    if mode == sig::SS_DISABLE {
        t.altstack = AltStack::default();
        return Ok(());
    }
    if new.size < sig::MINSIGSTKSZ {
        return Err(ENOMEM);
    }
    t.altstack = new;
    Ok(())
}

/// `sigaltstack(ss, old)`.
pub fn sigaltstack(t: &mut Task, new: usize, old: usize) -> Result<usize, isize> {
    let before = t.altstack;
    if new != 0 {
        // `stack_t`: `ss_sp`, `ss_flags`, `ss_size`.
        // SAFETY: the program's memory, which a fault reading it ends.
        let (sp, flags, size) = unsafe {
            (
                crate::user::read::<u64>(new).ok_or(EFAULT)?,
                crate::user::read::<i32>(new + 8).ok_or(EFAULT)?,
                crate::user::read::<u64>(new + 16).ok_or(EFAULT)?,
            )
        };
        // `SS_ONSTACK` asks for what 0 does (old programs pass it).
        let flags = if flags & !sig::SS_AUTODISARM == sig::SS_ONSTACK { flags & sig::SS_AUTODISARM } else { flags };
        set_altstack(t, AltStack { sp, size, flags })?;
    }
    if old != 0 {
        let (sp, flags, size) = before.reported(architecture::sp(&t.state));
        let mut bytes = [0u8; 24];
        bytes[0..8].copy_from_slice(&sp.to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[16..24].copy_from_slice(&size.to_le_bytes());
        // SAFETY: the program's memory, which a fault writing it ends.
        unsafe { crate::user::write_bytes(old, &bytes).ok_or(EFAULT)? };
    }
    Ok(0)
}

/// Foreign code took an exception: a legacy virtual system call is
/// carried out; anything else is the signal it stands for, forced on the
/// thread as Linux forces it.
pub fn fault(t: &mut Task) {
    if let Some(n) = architecture::vsyscall(&t.state) {
        // `gettimeofday(tv, tz)`, `time(t)`, `getcpu(cpu, node, cache)`:
        // their arguments where a call's are.
        let (_, a) = architecture::syscall(&t.state);
        // SAFETY: the program's arguments, as for the system call.
        let r = posix::host::encode(unsafe { posix::host::syscall(n, a) });
        if architecture::vsyscall_return(&mut t.state, r) {
            return;
        }
    }
    let f = architecture::fault(&t.state);
    t.fault = Some(f);
    posix::host::force(f.signal, Info { code: f.code, addr: Some(f.addr), ..Info::default() });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternate_stacks_say_where_handlers_run() {
        let off = AltStack::default();
        assert_eq!(off.top_for(0x7000, true), None);
        assert_eq!(off.reported(0x7000), (0, sig::SS_DISABLE, 0));
        let s = AltStack { sp: 0x1_0000, size: 0x4000, flags: 0 };
        assert!(s.on(0x1_2000) && s.on(0x1_4000) && !s.on(0x1_0000) && !s.on(0x7000));
        // A handler that asks for it runs at its top, unless the thread is
        // on it already; one that does not, where the thread is.
        assert_eq!(s.top_for(0x7000, true), Some(0x1_4000));
        assert_eq!(s.top_for(0x1_2000, true), None);
        assert_eq!(s.top_for(0x7000, false), None);
        // A frame must not run off it.
        assert!(s.fits(0x1_3000, 0x7000, true));
        assert!(!s.fits(0x0_f000, 0x7000, true));
        assert!(s.fits(0x0_f000, 0x7000, false));
        assert_eq!(s.reported(0x1_2000).1, sig::SS_ONSTACK);
        let disarm = AltStack { flags: sig::SS_AUTODISARM, ..s };
        assert_eq!(disarm.reported(0x7000).1, sig::SS_AUTODISARM);
    }
}
