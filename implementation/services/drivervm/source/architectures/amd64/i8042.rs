// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The PC's keyboard controller (the i8042), when the device manager gives
//! it the guest: Linux's driver of it reaches its two ports through the
//! monitor, which carries the reads and writes out on the controller, the
//! commands only if they are those Linux's drivers give
//! (`drivervm_core::architecture::i8042`): the controller also drives the
//! processor's reset line, which none of them touches. Its interrupts are
//! the guest's lines 1 and 12 (the keyboard's and the mouse's, edges: see
//! `crate::lines`). The ports around them (0x61's NMI controls among them)
//! the guest does not reach.

use drivervm_core::architecture::i8042::Controller;
use rt::object::IoPorts;
use rt::println;
use rt::sync::Mutex;

/// The controller's data port, and its status and command port; the
/// keyboard's and the mouse's interrupt lines.
pub use hv::amd64::acpi::{I8042_COMMAND as COMMAND, I8042_DATA as DATA, KEYBOARD_GSI, MOUSE_GSI};

/// The controller as the guest drives it: what it does with the next data
/// byte, and the commands dropped (each said once).
struct State {
    controller: Controller,
    dropped: [u64; 4],
}

pub struct I8042 {
    data: IoPorts,
    command: IoPorts,
    state: Mutex<State>,
}

impl I8042 {
    pub fn new(data: IoPorts, command: IoPorts) -> I8042 {
        I8042 { data, command, state: Mutex::new(State { controller: Controller::new(), dropped: [0; 4] }) }
    }

    /// Reads a port of the controller's.
    pub fn read(&self, port: u16) -> u8 {
        if port == COMMAND { self.command.in8(port) } else { self.data.in8(port) }
    }

    /// Carries out the guest's write of a port of the controller's: a
    /// command if it is one of those Linux's drivers give (the others are
    /// dropped), a data byte as it is.
    pub fn write(&self, port: u16, value: u8) {
        let mut s = self.state.lock();
        if port != COMMAND {
            s.controller.data();
            self.data.out8(port, value);
        } else if s.controller.command(value) {
            self.command.out8(port, value);
        } else {
            let (word, bit) = (value as usize / 64, 1u64 << (value % 64));
            if s.dropped[word] & bit == 0 {
                s.dropped[word] |= bit;
                println!("the keyboard controller is not given the guest's command {value:#04x}");
            }
        }
    }
}
