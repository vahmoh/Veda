// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! System call dispatch. See `abi` for the calling convention and the
//! meaning of each call.

mod hv;
mod hw;
mod ipc;
mod misc;
mod task;
mod vm;

use alloc::sync::Arc;

use abi::{Error, RawHandle, Rights, nr};

use crate::architecture::trap::TrapFrame;
use crate::object::KObject;
use crate::object::handle::Handle;
use crate::object::process::Process;
use crate::scheduler;
use crate::synchronization::bkl;

/// Successful result: primary value and optional second value.
pub type SysResult = Result<(usize, Option<usize>), Error>;

fn ok(v: usize) -> SysResult {
    Ok((v, None))
}

/// Carries out the system call a thread made (from the architecture's
/// entry code), returning its result in `frame`.
pub fn syscall_dispatch(frame: &mut TrapFrame) {
    bkl::acquire();
    scheduler::entered_from_user();
    let (n, a) = frame.syscall();
    match dispatch(n, a) {
        Ok((v, second)) => frame.set_syscall_result(v as u64, second.map(|s| s as u64)),
        Err(e) => frame.set_syscall_result(e.as_return() as u64, None),
    }
    scheduler::return_to_user_hook(frame);
    bkl::release();
}

fn dispatch(n: usize, a: [usize; 6]) -> SysResult {
    match n {
        nr::DEBUG_WRITE => misc::debug_write(a[0], a[1]),
        nr::LOG_READ => misc::log_read(a[0], a[1], a[2]),
        nr::CLOCK_GET => misc::clock_get(a[0]),
        nr::SLEEP => misc::sleep(a[0]),
        nr::YIELD => {
            scheduler::yield_now();
            ok(0)
        }
        nr::SYSTEM_INFO => misc::system_info(a[0]),
        nr::SYSTEM_POWER => misc::system_power(a[0] as RawHandle, a[1]),
        nr::RANDOM => misc::random(a[0], a[1]),
        nr::CLOCK_INFO => misc::clock_info(a[0]),

        nr::HANDLE_CLOSE => ipc::handle_close(a[0] as RawHandle),
        nr::HANDLE_DUPLICATE => ipc::handle_duplicate(a[0] as RawHandle, a[1] as u32),
        nr::HANDLE_REPLACE => ipc::handle_replace(a[0] as RawHandle, a[1] as u32),
        nr::OBJECT_INFO => ipc::object_info(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::OBJECT_SIGNAL => ipc::object_signal(a[0] as RawHandle, a[1] as u32, a[2] as u32),
        nr::OBJECT_WAIT_ONE => ipc::wait_one(a[0] as RawHandle, a[1] as u32, a[2] as u64),
        nr::OBJECT_WAIT_MANY => ipc::wait_many(a[0], a[1], a[2] as u64),
        nr::PORT_CREATE => ipc::port_create(),
        nr::OBJECT_WAIT_ASYNC => ipc::wait_async(a[0] as RawHandle, a[1] as RawHandle, a[2] as u64, a[3] as u32),
        nr::OBJECT_WAIT_ASYNC_CANCEL => ipc::wait_async_cancel(a[0] as RawHandle, a[1] as RawHandle, a[2] as u64),
        nr::PORT_WAIT => ipc::port_wait(a[0] as RawHandle, a[1], a[2], a[3] as u64),
        nr::CHANNEL_CREATE => ipc::channel_create(a[0], a[1] as u32),
        nr::CHANNEL_WRITE => ipc::channel_write(a[0] as RawHandle, a[1], a[2], a[3], a[4]),
        nr::CHANNEL_READ => ipc::channel_read(a[0] as RawHandle, a[1], a[2], a[3], a[4], a[5]),
        nr::SOCKET_CREATE => ipc::socket_create(a[0]),
        nr::SOCKET_WRITE => ipc::socket_write(a[0] as RawHandle, a[1], a[2], a[3], a[4]),
        nr::SOCKET_READ => ipc::socket_read(a[0] as RawHandle, a[1], a[2], a[3], a[4], a[5]),
        nr::SOCKET_PEEK => ipc::socket_peek(a[0] as RawHandle, a[1], a[2]),
        nr::SOCKET_SHUTDOWN => ipc::socket_shutdown(a[0] as RawHandle),
        nr::EVENT_CREATE => ipc::event_create(),

        nr::VMO_CREATE => vm::vmo_create(a[0], a[1]),
        nr::VMO_READ => vm::vmo_read(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::VMO_WRITE => vm::vmo_write(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::VMO_GET_SIZE => vm::vmo_get_size(a[0] as RawHandle),
        nr::VMO_CREATE_PHYSICAL => vm::vmo_create_physical(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::VMO_CREATE_CONTIGUOUS => vm::vmo_create_contiguous(a[0] as RawHandle, a[1], a[2]),
        nr::VMO_PHYS_ADDR => vm::vmo_phys_addr(a[0] as RawHandle, a[1]),
        nr::VMO_PAGES => vm::vmo_pages(a[0] as RawHandle, a[1] as RawHandle, a[2], a[3], a[4]),
        nr::VMO_SET_SIZE => vm::vmo_set_size(a[0] as RawHandle, a[1]),
        nr::VMO_CREATE_SLICE => vm::vmo_create_slice(a[0] as RawHandle, a[1], a[2]),
        nr::VM_MAP => vm::vm_map(a[0] as RawHandle, a[1] as RawHandle, a[2], a[3], a[4], a[5]),
        nr::VM_UNMAP => vm::vm_unmap(a[0] as RawHandle, a[1], a[2]),
        nr::VM_PROTECT => vm::vm_protect(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::VM_ALLOCATE => vm::vm_allocate(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::VM_DECOMMIT => vm::vm_decommit(a[0] as RawHandle, a[1], a[2]),

        nr::PROCESS_CREATE => task::process_create(a[0], a[1], a[2]),
        nr::PROCESS_START => {
            task::process_start(a[0] as RawHandle, a[1] as RawHandle, a[2], a[3], a[4] as RawHandle, a[5])
        }
        nr::PROCESS_EXIT => task::process_exit(a[0] as i64),
        nr::PROCESS_KILL => task::process_kill(a[0] as RawHandle, a[1], a[2]),
        nr::THREAD_CREATE => task::thread_create(a[0] as RawHandle, a[1], a[2]),
        nr::THREAD_START => task::thread_start(a[0] as RawHandle, a[1], a[2], a[3], a[4]),
        nr::THREAD_EXIT => task::thread_exit(),
        nr::THREAD_SET_PRIORITY => task::thread_set_priority(a[0] as RawHandle, a[1]),
        nr::THREAD_SET_TLS => task::thread_set_tls(a[0]),
        nr::PROCESS_LIST => task::process_list(a[0], a[1]),
        nr::PROCESS_OPEN => task::process_open(a[0] as RawHandle, a[1] as u64),
        nr::THREAD_SET_EXIT_FUTEX => task::thread_set_exit_futex(a[0]),
        nr::THREAD_SUSPEND => task::thread_suspend(a[0] as RawHandle),
        nr::THREAD_RESUME => task::thread_resume(a[0] as RawHandle),
        nr::THREAD_READ_STATE => task::thread_read_state(a[0] as RawHandle, a[1]),
        nr::THREAD_SELF => task::thread_self(),
        nr::PROCESS_RESTRICT => task::process_restrict(a[0]),
        nr::PROCESS_SET_MEMORY_CLASS => task::process_set_memory_class(a[0] as RawHandle, a[1]),

        nr::FUTEX_WAIT => {
            crate::synchronization::futex::wait(a[0] as u64, a[1] as u32, deadline(a[2] as u64))?;
            ok(0)
        }
        nr::FUTEX_WAKE => ok(crate::synchronization::futex::wake(a[0] as u64, a[1])?),

        nr::RESOURCE_CREATE => hw::resource_create(a[0] as RawHandle, a[1], a[2] as u64, a[3] as u64),
        nr::IOPORT_CREATE => hw::ioport_create(a[0] as RawHandle, a[1], a[2]),
        nr::IOPORT_READ => hw::ioport_read(a[0] as RawHandle, a[1], a[2]),
        nr::IOPORT_WRITE => hw::ioport_write(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::IRQ_CREATE => hw::irq_create(a[0] as RawHandle, a[1], a[2]),
        nr::IRQ_ACK => hw::irq_ack(a[0] as RawHandle),
        nr::MSI_CREATE => hw::msi_create(a[0] as RawHandle, a[1], a[2], a[3]),
        nr::IRQ_CREATE_SOFTWARE => hw::irq_create_software(a[0], a[1] as RawHandle),
        nr::IRQ_RAISE => hw::irq_raise(a[0] as RawHandle),

        nr::GUEST_CREATE => hv::guest_create(a[0] as RawHandle, a[1]),
        nr::GUEST_MAP => hv::guest_map(a[0] as RawHandle, a[1] as RawHandle, a[2], a[3], a[4], a[5]),
        nr::GUEST_UNMAP => hv::guest_unmap(a[0] as RawHandle, a[1], a[2]),
        nr::VCPU_CREATE => hv::vcpu_create(a[0] as RawHandle, a[1], a[2]),
        nr::VCPU_RUN => hv::vcpu_run(a[0] as RawHandle, a[1]),
        nr::VCPU_INTERRUPT => hv::vcpu_interrupt(a[0] as RawHandle, a[1]),
        nr::VCPU_READ_STATE => hv::vcpu_read_state(a[0] as RawHandle, a[1]),
        nr::GUEST_ATTACH_DEVICE => hv::guest_attach_device(a[0] as RawHandle, a[1] as RawHandle, a[2]),
        nr::VCPU_BIND_INTERRUPT => hv::vcpu_bind_interrupt(a[0] as RawHandle, a[1] as RawHandle, a[2]),
        _ => Err(Error::UnknownSyscall),
    }
}

/// Converts an ABI deadline into an optional blocking deadline.
fn deadline(d: u64) -> Option<u64> {
    if d == abi::DEADLINE_INFINITE { None } else { Some(d) }
}

pub fn current_process() -> Result<Arc<Process>, Error> {
    scheduler::current().process.clone().ok_or(Error::BadState)
}

/// Looks up a handle in the current process and checks its rights.
fn handle(raw: RawHandle, rights: Rights) -> Result<Handle, Error> {
    let p = current_process()?;
    let h = p.handles.lock().get(raw)?.clone();
    if !h.rights.contains(rights) {
        return Err(Error::AccessDenied);
    }
    Ok(h)
}

/// Kernel memory that a new handle and its object may cost: user space may
/// make one only while that much is left outside the kernel's reserve
/// (`phys::user_may_use`), so that no process can exhaust the kernel's own
/// memory by making objects.
const NEW_OBJECT_BYTES: u64 = 4096;

/// Refuses (`NoMemory`) a new object for user space within the kernel's
/// reserve.
fn may_make_object() -> Result<(), Error> {
    if crate::memory::phys::user_may_use(NEW_OBJECT_BYTES) { Ok(()) } else { Err(Error::NoMemory) }
}

/// Adds a handle to a new object (or a duplicate) to the current process.
fn insert(object: KObject, rights: Rights) -> Result<RawHandle, Error> {
    may_make_object()?;
    current_process()?.handles.lock().insert(Handle { object, rights })
}

macro_rules! typed_getter {
    ($name:ident, $variant:ident, $ty:ty) => {
        fn $name(raw: RawHandle, rights: Rights) -> Result<Arc<$ty>, Error> {
            match handle(raw, rights)?.object {
                KObject::$variant(o) => Ok(o),
                _ => Err(Error::WrongType),
            }
        }
    };
}

typed_getter!(get_channel, Channel, crate::ipc::channel::ChannelEnd);
typed_getter!(get_socket, Socket, crate::ipc::socket::SocketEnd);
typed_getter!(get_vmo, Vmo, crate::memory::vmo::Vmo);
typed_getter!(get_process, Process, Process);
typed_getter!(get_thread, Thread, crate::scheduler::Thread);
typed_getter!(get_resource, Resource, crate::object::resource::Resource);
typed_getter!(get_ioports, IoPorts, crate::object::ioport::IoPorts);
typed_getter!(get_interrupt, Interrupt, crate::interrupt::Interrupt);
typed_getter!(get_event, Event, crate::ipc::event::Event);
typed_getter!(get_port, Port, crate::ipc::port::Port);

/// A process handle argument where 0 means "the calling process".
fn target_process(raw: RawHandle) -> Result<Arc<Process>, Error> {
    if raw == abi::INVALID_HANDLE { current_process() } else { get_process(raw, Rights::MANAGE) }
}

/// Reads a string argument (UTF-8, bounded).
fn read_str(ptr: usize, len: usize, max: usize) -> Result<alloc::string::String, Error> {
    let bytes = crate::memory::user::read_vec(ptr as u64, len, max)?;
    alloc::string::String::from_utf8(bytes).map_err(|_| Error::InvalidArgs)
}
