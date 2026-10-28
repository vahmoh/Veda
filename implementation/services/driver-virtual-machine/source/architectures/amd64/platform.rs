// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The PC's own devices the guest has: its keyboard controller, if the
//! device manager hands its ports and interrupts over (`i8042`). And what
//! the guest's processors do that is not a hypercall: reach I/O ports (the
//! platform has none but the keyboard controller's), shut down (a triple
//! fault), reach memory that is not there, or fail.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use abi::{VcpuExit, vcpu_exit};
use proto::startup::driver_virtual_machine as roles;
use runtime::object::{Interrupt, IoPorts, Vcpu};
use runtime::println;
use runtime::sync::Mutex;

use super::i8042::{self, I8042};
use crate::lines::Lines;
use crate::machine::exit_code;

pub struct Platform {
    /// The keyboard controller, if the guest has it.
    i8042: Option<I8042>,
    /// I/O ports the guest touched (each reported once).
    ports: Mutex<Vec<u16>>,
}

impl Platform {
    /// What the device manager handed over of the PC: the keyboard
    /// controller, whose lines join `lines`.
    pub fn take(lines: &mut Lines) -> Platform {
        let take = |role| runtime::env::take_handle(role);
        let i8042 = match (
            take(roles::I8042_DATA),
            take(roles::I8042_COMMAND),
            take(roles::I8042_KEYBOARD),
            take(roles::I8042_MOUSE),
        ) {
            (Some(data), Some(command), Some(keyboard), Some(mouse)) => {
                lines.add(i8042::KEYBOARD_GSI, Interrupt::from_handle(keyboard), false);
                lines.add(i8042::MOUSE_GSI, Interrupt::from_handle(mouse), false);
                Some(I8042::new(IoPorts::from_handle(data), IoPorts::from_handle(command)))
            }
            _ => None,
        };
        Platform { i8042, ports: Mutex::new(Vec::new()) }
    }

    /// What the ACPI tables say of the PC the guest has.
    pub fn acpi(&self) -> hv::acpi::Platform {
        hv::amd64::acpi::platform(self.i8042.is_some())
    }

    /// Answers processor `id`'s exit that is not a hypercall: `Ok` when it
    /// may run on, else why the machine stops, and how it ends.
    pub fn exit(&self, id: u32, vcpu: &Vcpu, exit: &mut VcpuExit) -> Result<(), (String, i32)> {
        match exit.reason {
            vcpu_exit::IO => {
                self.port(exit);
                Ok(())
            }
            vcpu_exit::MEMORY => Err((
                format!(
                    "processor {id} reached unmapped memory at {:#x} (access {}, rip {:#x})",
                    exit.data[0], exit.data[1], exit.data[2]
                ),
                exit_code::FAULT,
            )),
            vcpu_exit::SHUTDOWN => Err((format!("processor {id} shut down"), exit_code::CRASHED)),
            _ => {
                let state = vcpu.state().map(|s| format!("{s:x?}")).unwrap_or_default();
                Err((
                    format!(
                        "processor {id} failed (exit {:#x}, {:#x}, rip {:#x}): {state}",
                        exit.data[0], exit.data[1], exit.data[2]
                    ),
                    exit_code::FAULT,
                ))
            }
        }
    }

    /// The platform has no I/O ports but the keyboard controller's, if the
    /// guest has it: elsewhere reads find nothing, writes go nowhere. Each
    /// port is reported the first time.
    fn port(&self, exit: &mut VcpuExit) {
        let port = exit.data[0] as u16;
        if let Some(i8042) = &self.i8042
            && matches!(port, i8042::DATA | i8042::COMMAND)
            && exit.data[1] == 1
        {
            if exit.data[2] == 1 {
                i8042.write(port, exit.data[3] as u8);
            } else {
                exit.data[3] = i8042.read(port) as u64;
            }
            return;
        }
        let mut seen = self.ports.lock();
        if !seen.contains(&port) && seen.len() < 32 {
            seen.push(port);
            println!("Linux touched I/O port {:#x}, which does not exist", port);
        }
        exit.data[3] = u64::MAX;
    }
}
