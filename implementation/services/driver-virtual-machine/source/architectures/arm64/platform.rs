// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What the guest's processors do that is not one of the platform's
//! hypercalls: PSCI's calls, which Linux makes as it starts its processors
//! and powers the machine off; reach memory that is not there, or fail. The
//! guest has no devices of the machine's own: everything it has is on PCI.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use abi::{VcpuExit, vcpu_exit};
use hv::arm64::platform::{cpu_of, is_psci, psci};
use hv::platform::{error, power};
use runtime::object::Vcpu;

use crate::lines::Lines;
use crate::machine::{Machine, exit_code};

pub struct Platform;

impl Platform {
    /// What the device manager handed over of the machine: nothing.
    pub fn take(_lines: &mut Lines) -> Platform {
        Platform
    }

    /// What the ACPI tables say of the machine, which go unused.
    pub fn acpi(&self) -> hv::acpi::Platform {
        hv::acpi::Platform::default()
    }

    /// Answers processor `id`'s exit that is not a hypercall: `Ok` when it
    /// may run on, else why the machine stops, and how it ends.
    pub fn exit(&self, id: u32, vcpu: &Vcpu, exit: &mut VcpuExit) -> Result<(), (String, i32)> {
        match exit.reason {
            vcpu_exit::MEMORY => Err((
                format!(
                    "processor {id} reached unmapped memory at {:#x} (access {}, pc {:#x}, syndrome {:#x})",
                    exit.data[0], exit.data[1], exit.data[2], exit.data[3]
                ),
                exit_code::FAULT,
            )),
            vcpu_exit::SHUTDOWN => Err((format!("processor {id} shut down"), exit_code::CRASHED)),
            _ => {
                let state = vcpu.state().map(|s| format!("{s:x?}")).unwrap_or_default();
                Err((
                    format!(
                        "processor {id} failed (syndrome {:#x}, address {:#x}, pc {:#x}): {state}",
                        exit.data[0], exit.data[1], exit.data[2]
                    ),
                    exit_code::FAULT,
                ))
            }
        }
    }
}

/// Answers a hypercall that is not one of the platform's: PSCI's.
pub fn call(machine: &Arc<Machine>, data: &[u64; 7]) -> u64 {
    if !is_psci(data[0]) {
        return error::UNKNOWN;
    }
    match data[0] {
        psci::VERSION => psci::VERSION_1_0,
        psci::MIGRATE_INFO_TYPE => psci::NO_MIGRATION,
        psci::FEATURES => match data[1] {
            psci::VERSION
            | psci::CPU_ON
            | psci::AFFINITY_INFO
            | psci::MIGRATE_INFO_TYPE
            | psci::SYSTEM_OFF
            | psci::SYSTEM_RESET
            | psci::FEATURES => psci::SUCCESS,
            _ => psci::NOT_SUPPORTED,
        },
        psci::CPU_ON => match machine.start_cpu(cpu_of(data[1]), data[2], data[3]) {
            0 => psci::SUCCESS,
            _ => psci::ALREADY_ON,
        },
        psci::AFFINITY_INFO => psci::SUCCESS,
        psci::SYSTEM_OFF => machine.power(power::OFF),
        psci::SYSTEM_RESET => machine.power(power::RESTART),
        _ => psci::NOT_SUPPORTED,
    }
}
