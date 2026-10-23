// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Threads.
//!
//! A thread runs on a stack of private memory with an inaccessible guard
//! region below it, so that an overflow faults instead of running into
//! other memory. The stack is freed once the thread is done with it, by
//! whichever of two things happens last: the thread ending, or its
//! [`JoinHandle`] going. Joining waits for the thread to end, then frees
//! the stack. Dropping the handle detaches the thread: if it is still
//! running, it frees its own stack as it ends (unmapping it and ending in
//! one step that touches no memory); if it has finished already, the
//! handle waits for it to end, which is a matter of moments, and frees the
//! stack itself. A word they share decides which. So a program that starts
//! threads and lets them go (a service with a thread per connection, a
//! [pool](crate::pool) dropped) gives every stack back.
//!
//! On the standard library (in a guest of the driver VM, and in Veda's own
//! programs on it) threads are the standard library's, which the C library
//! starts and joins: the program's runtime is its own there. A priority
//! asked for then is the one a thread sets itself as it starts (Linux's
//! threads keep theirs: they are Linux's business).

#[cfg(not(any(veda_guest, target_os = "veda")))]
use alloc::boxed::Box;
#[cfg(not(any(veda_guest, target_os = "veda")))]
use alloc::sync::Arc;
#[cfg(not(any(veda_guest, target_os = "veda")))]
use core::sync::atomic::{AtomicU32, Ordering};

use abi::{Error, nr};
#[cfg(not(any(veda_guest, target_os = "veda")))]
use abi::{map_flags, signals};

#[cfg(not(any(veda_guest, target_os = "veda")))]
use crate::object::{Handle, Thread};
#[cfg(not(any(veda_guest, target_os = "veda")))]
use crate::sync::Mutex;
use crate::sys::call;
#[cfg(not(any(veda_guest, target_os = "veda")))]
use crate::vm;

/// Default stack size for spawned threads.
pub const DEFAULT_STACK: usize = 256 * 1024;
/// The smallest stack a thread gets.
const MIN_STACK: usize = 16 * 1024;

/// [`Packet::state`]: the thread runs, and its handle is there.
#[cfg(not(any(veda_guest, target_os = "veda")))]
const RUNNING: u32 = 0;
/// The thread has stored its result and ends leaving its stack to the
/// handle.
#[cfg(not(any(veda_guest, target_os = "veda")))]
const FINISHED: u32 = 1;
/// The handle is gone: the thread frees its stack as it ends.
#[cfg(not(any(veda_guest, target_os = "veda")))]
const DETACHED: u32 = 2;

/// What a thread and its handle share.
#[cfg(not(any(veda_guest, target_os = "veda")))]
struct Packet<T> {
    /// [`RUNNING`], then [`FINISHED`] (by the thread) or [`DETACHED`] (by
    /// the handle), whichever comes first.
    state: AtomicU32,
    result: Mutex<Option<T>>,
}

/// A thread's stack: its guard region, then the stack proper, one private
/// mapping.
#[cfg(not(any(veda_guest, target_os = "veda")))]
#[derive(Debug, Clone, Copy)]
struct Stack {
    base: usize,
    len: usize,
}

#[cfg(not(any(veda_guest, target_os = "veda")))]
impl Stack {
    /// A stack of `size` bytes (a multiple of the page size) and its guard.
    fn new(size: usize) -> Result<Stack, Error> {
        let len = size.checked_add(crate::process::STACK_GUARD).ok_or(Error::NoMemory)?;
        let base = vm::allocate(None, len, 0, map_flags::READ | map_flags::WRITE)?;
        if let Err(e) = vm::protect(None, base, crate::process::STACK_GUARD, 0) {
            let _ = vm::unmap(None, base, len);
            return Err(e);
        }
        Ok(Stack { base, len })
    }

    /// Frees the stack.
    ///
    /// # Safety
    /// No thread may run on it, now or later.
    unsafe fn free(self) {
        let _ = vm::unmap(None, self.base, self.len);
    }
}

/// What a new thread starts with: its body, which runs the closure, stores
/// its result and says whether the handle is gone, and its stack.
#[cfg(not(any(veda_guest, target_os = "veda")))]
struct Start {
    body: Box<dyn FnOnce() -> bool + Send>,
    stack: Stack,
}

/// A thread [`Builder::spawn`] started: joining it gives its result;
/// dropping it detaches the thread (see the module's documentation).
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub struct JoinHandle<T> {
    thread: Thread,
    packet: Arc<Packet<T>>,
    /// The thread's stack, until this handle has freed it or left it to
    /// the thread.
    stack: Option<Stack>,
}

#[cfg(not(any(veda_guest, target_os = "veda")))]
impl<T> JoinHandle<T> {
    /// Waits for the thread to finish and returns its result.
    pub fn join(mut self) -> Result<T, Error> {
        self.thread.wait(signals::TERMINATED, abi::DEADLINE_INFINITE)?;
        // The thread has ended, so its stack can go.
        if let Some(stack) = self.stack.take() {
            // SAFETY: the thread no longer runs on it.
            unsafe { stack.free() };
        }
        self.packet.result.lock().take().ok_or(Error::Canceled)
    }

    /// The thread's kernel object.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

#[cfg(not(any(veda_guest, target_os = "veda")))]
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        let Some(stack) = self.stack.take() else { return };
        // Acquire/release with the thread's exchange: exactly one of them
        // changes `RUNNING`, and each sees the other's write if it lost.
        if self.packet.state.compare_exchange(RUNNING, DETACHED, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            // The thread frees its stack as it ends.
            return;
        }
        // It has finished, and ends without touching the stack again: free
        // it once it has.
        if self.thread.wait(signals::TERMINATED, abi::DEADLINE_INFINITE).is_ok() {
            // SAFETY: the thread no longer runs on it.
            unsafe { stack.free() };
        }
    }
}

/// A thread of the standard library's.
#[cfg(any(veda_guest, target_os = "veda"))]
pub struct JoinHandle<T>(std::thread::JoinHandle<T>);

#[cfg(any(veda_guest, target_os = "veda"))]
impl<T> JoinHandle<T> {
    /// Waits for the thread to finish and returns its result.
    pub fn join(self) -> Result<T, Error> {
        self.0.join().map_err(|_| Error::Canceled)
    }
}

/// A thread [`Builder::spawn`] started, from the architecture's thread
/// entry point (`architecture`): `arg` is the [`Start`] it leaked.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub(crate) fn run(arg: usize) -> ! {
    let (body, stack) = {
        // SAFETY: `arg` is the box leaked by `Builder::spawn`, ours now.
        let start = unsafe { Box::from_raw(arg as *mut Start) };
        let Start { body, stack } = *start;
        (body, stack)
    };
    // Runs the closure; everything it owned is dropped by the time this
    // returns, so nothing of the thread's is left but its stack.
    if body() {
        // SAFETY: nothing else refers to the stack: the handle is gone.
        unsafe { crate::architecture::exit_unmapping(stack.base, stack.len) }
    }
    crate::sys::thread_exit()
}

/// Configures a thread before spawning it.
pub struct Builder {
    name: &'static str,
    stack_size: usize,
    priority: Option<usize>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// A thread named "thread", with [`DEFAULT_STACK`] bytes of stack and
    /// its process's priority.
    pub fn new() -> Builder {
        Builder { name: "thread", stack_size: DEFAULT_STACK, priority: None }
    }

    /// The thread's name (shown by the task manager).
    pub fn name(mut self, name: &'static str) -> Builder {
        self.name = name;
        self
    }

    /// The stack's size: at least 16 KiB, rounded up to whole pages.
    pub fn stack_size(mut self, size: usize) -> Builder {
        self.stack_size = size.max(MIN_STACK);
        self
    }

    /// The thread's scheduling priority (see `abi::priority`), at most its
    /// process's maximum (`NORMAL` for an application's).
    pub fn priority(mut self, p: usize) -> Builder {
        self.priority = Some(p);
        self
    }

    /// Starts a thread running `f`.
    #[cfg(not(any(veda_guest, target_os = "veda")))]
    pub fn spawn<F, T>(self, f: F) -> Result<JoinHandle<T>, Error>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let size = self.stack_size.checked_next_multiple_of(abi::PAGE_SIZE).ok_or(Error::NoMemory)?;
        let stack = Stack::new(size)?;
        let thread = match call(nr::THREAD_CREATE, [0, self.name.as_ptr() as usize, self.name.len(), 0, 0, 0]) {
            // SAFETY: the kernel just created this handle for us.
            Ok(h) => Thread(unsafe { Handle::from_raw(h as u32) }),
            Err(e) => {
                // SAFETY: no thread runs on it.
                unsafe { stack.free() };
                return Err(e);
            }
        };
        let started = self
            .priority
            .map_or(Ok(0), |p| call(nr::THREAD_SET_PRIORITY, [thread.raw() as usize, p, 0, 0, 0, 0]).map(|_| 0));
        let packet = Arc::new(Packet { state: AtomicU32::new(RUNNING), result: Mutex::new(None) });
        let theirs = packet.clone();
        let body: Box<dyn FnOnce() -> bool + Send> = Box::new(move || {
            let v = f();
            *theirs.result.lock() = Some(v);
            // Whether the handle is gone; if it is not, it frees the stack
            // once this thread has ended.
            let detached =
                theirs.state.compare_exchange(RUNNING, FINISHED, Ordering::AcqRel, Ordering::Acquire).is_err();
            drop(theirs);
            detached
        });
        let arg = Box::into_raw(Box::new(Start { body, stack })) as usize;
        let sp = abi::entry_stack_pointer(stack.base + stack.len);
        let started = started.and_then(|_| {
            call(
                nr::THREAD_START,
                [thread.raw() as usize, crate::architecture::thread_entry as *const () as usize, sp, arg, 0, 0],
            )
        });
        if let Err(e) = started {
            // SAFETY: the thread never started, so the box and the stack are
            // still ours.
            unsafe {
                drop(Box::from_raw(arg as *mut Start));
                stack.free();
            }
            return Err(e);
        }
        Ok(JoinHandle { thread, packet, stack: Some(stack) })
    }

    /// Starts a thread running `f`.
    #[cfg(any(veda_guest, target_os = "veda"))]
    pub fn spawn<F, T>(self, f: F) -> Result<JoinHandle<T>, Error>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // Priorities are Linux's business in the guest; on Veda a thread
        // sets its own (the C library keeps its handle).
        let priority = if cfg!(veda_guest) { None } else { self.priority };
        std::thread::Builder::new()
            .name(self.name.into())
            .stack_size(self.stack_size)
            .spawn(move || {
                if let Some(p) = priority {
                    let _ = set_priority(p);
                }
                f()
            })
            .map(JoinHandle)
            .map_err(|_| Error::NoMemory)
    }
}

/// Spawns a thread with default settings.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).expect("failed to spawn thread")
}

/// Sets the calling thread's scheduling priority (see `abi::priority`):
/// for a thread the standard library started, which [`Builder`] did not.
/// `AccessDenied` above its process's maximum (`NORMAL` for an
/// application's).
pub fn set_priority(priority: usize) -> Result<(), Error> {
    call(nr::THREAD_SET_PRIORITY, [abi::INVALID_HANDLE as usize, priority, 0, 0, 0, 0]).map(|_| ())
}

/// Gives up the rest of this time slice.
pub fn yield_now() {
    let _ = call(nr::YIELD, [0; 6]);
}

/// Ends the calling thread after unmapping `[base, base + len)`, its own
/// stack, which no code may touch in between (for a thread library whose
/// threads free their stacks as they end).
///
/// # Safety
/// Nothing may use the range afterwards: it is gone once the thread ends.
#[cfg(not(any(veda_guest, target_os = "veda")))]
pub unsafe fn exit_unmapping(base: usize, len: usize) -> ! {
    // SAFETY: per the caller.
    unsafe { crate::architecture::exit_unmapping(base, len) }
}
