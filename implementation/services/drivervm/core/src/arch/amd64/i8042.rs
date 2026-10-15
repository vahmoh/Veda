// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The PC's keyboard controller (the i8042), as the guest may drive it.
//!
//! The controller also drives the processor's reset line and the A20 gate:
//! its output port holds them (bit 0 the reset line, active low; bit 1
//! A20), which command 0xD1 writes with the next data byte, and commands
//! 0xF0 to 0xFF pulse (0xFE resets the machine); some controllers have
//! commands of their own for A20, or for their vendor's purposes. So the
//! monitor gives the controller only the commands Linux's drivers of it
//! give ([`allowed`]: those of `i8042`, through which `atkbd` and `psmouse`
//! reach their devices), none of which touches the output port, and drops
//! the rest. What the guest writes to the data port goes as it is: it is
//! the parameter of the last command the controller was given that takes
//! one, or else the keyboard's ([`Controller`] follows which, for the
//! commands the controller was given only: a command dropped changes
//! nothing), never the output port's.

/// The commands Linux's i8042 driver gives (`drivers/input/serio/i8042.h`).
pub mod command {
    use core::ops::RangeInclusive;

    /// Reads the configuration byte (`I8042_CMD_CTL_RCTR`).
    pub const READ_CONFIG: u8 = 0x20;
    /// Writes it (`I8042_CMD_CTL_WCTR`): a parameter.
    pub const WRITE_CONFIG: u8 = 0x60;
    /// Sends the next data byte to auxiliary port n of an active
    /// multiplexing controller, or gives it the next command
    /// (`I8042_CMD_MUX_SEND`, `I8042_CMD_MUX_PFX`): a parameter.
    pub const MUX_SEND: RangeInclusive<u8> = 0x90..=0x93;
    /// The auxiliary (mouse) port: off, on, tested.
    pub const AUX_DISABLE: u8 = 0xA7;
    pub const AUX_ENABLE: u8 = 0xA8;
    pub const AUX_TEST: u8 = 0xA9;
    /// The controller's self-test (`I8042_CMD_CTL_TEST`).
    pub const SELF_TEST: u8 = 0xAA;
    /// The keyboard port: tested, off, on.
    pub const KBD_TEST: u8 = 0xAB;
    pub const KBD_DISABLE: u8 = 0xAD;
    pub const KBD_ENABLE: u8 = 0xAE;
    /// Writes the next data byte back as the keyboard's, or the mouse's
    /// (`I8042_CMD_KBD_LOOP`, `I8042_CMD_AUX_LOOP`): a parameter.
    pub const KBD_LOOP: u8 = 0xD2;
    pub const AUX_LOOP: u8 = 0xD3;
    /// Sends the next data byte to the mouse (`I8042_CMD_AUX_SEND`): a
    /// parameter.
    pub const AUX_SEND: u8 = 0xD4;

    /// Writes the output port, which holds the reset line and A20, with
    /// the next data byte: never given.
    pub const WRITE_OUTPUT: u8 = 0xD1;
    /// Pulse lines of the output port (0xFE: the reset line): never given.
    pub const PULSE: RangeInclusive<u8> = 0xF0..=0xFF;
}

/// The commands the monitor gives the controller: Linux's.
pub fn allowed(c: u8) -> bool {
    use command::*;
    matches!(
        c,
        READ_CONFIG
            | WRITE_CONFIG
            | AUX_DISABLE
            | AUX_ENABLE
            | AUX_TEST
            | SELF_TEST
            | KBD_TEST
            | KBD_DISABLE
            | KBD_ENABLE
            | KBD_LOOP
            | AUX_LOOP
            | AUX_SEND
    ) || MUX_SEND.contains(&c)
}

/// Whether command `c` takes the next data byte as its parameter.
fn takes_parameter(c: u8) -> bool {
    use command::*;
    matches!(c, WRITE_CONFIG | KBD_LOOP | AUX_LOOP | AUX_SEND) || MUX_SEND.contains(&c)
}

/// What the controller does with the next byte written to its data port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// It sends it to the keyboard.
    Keyboard,
    /// It takes it as the parameter of this command.
    Parameter(u8),
}

/// The controller's state, as far as the commands it was given make it.
#[derive(Debug)]
pub struct Controller {
    next: Next,
}

impl Default for Controller {
    fn default() -> Controller {
        Controller::new()
    }
}

impl Controller {
    pub const fn new() -> Controller {
        Controller { next: Next::Keyboard }
    }

    /// What it does with the next data byte.
    pub fn next(&self) -> Next {
        self.next
    }

    /// The guest writes `c` to the command port: whether the controller is
    /// to be given it (it is one of Linux's commands). A command it is given
    /// ends what the one before waited for.
    pub fn command(&mut self, c: u8) -> bool {
        if !allowed(c) {
            return false;
        }
        self.next = if takes_parameter(c) { Next::Parameter(c) } else { Next::Keyboard };
        true
    }

    /// The guest writes a byte to the data port, which the controller is
    /// given as it is: what it does with it.
    pub fn data(&mut self) -> Next {
        core::mem::replace(&mut self.next, Next::Keyboard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_reaches_the_output_port_or_pulses_its_lines() {
        for c in 0..=u8::MAX {
            let mut k = Controller::new();
            if k.command(c) {
                assert_ne!(c, command::WRITE_OUTPUT);
                assert!(!command::PULSE.contains(&c), "{c:#x}");
                assert_ne!(k.data(), Next::Parameter(command::WRITE_OUTPUT));
            }
        }
        assert!(!allowed(0xFE) && !allowed(0xD1) && !allowed(0xDD) && !allowed(0xDF));
    }

    #[test]
    fn a_dropped_command_changes_nothing() {
        // The sequence the filter once let through: a command that awaits
        // the output port's byte, given; one dropped meanwhile; the byte.
        // Now neither is given, and the byte is the keyboard's.
        let mut k = Controller::new();
        assert!(!k.command(command::WRITE_OUTPUT));
        assert!(!k.command(0xFE));
        assert_eq!(k.data(), Next::Keyboard);
        // A parameter awaited stays awaited across a dropped command.
        assert!(k.command(command::WRITE_CONFIG));
        assert!(!k.command(command::WRITE_OUTPUT));
        assert_eq!(k.data(), Next::Parameter(command::WRITE_CONFIG));
        assert_eq!(k.data(), Next::Keyboard);
    }

    #[test]
    fn linux_drives_the_controller_as_it_does_on_a_pc() {
        let mut k = Controller::new();
        // Reading and writing the configuration byte, the self-test.
        for c in [command::READ_CONFIG, command::SELF_TEST, command::KBD_TEST, command::AUX_TEST] {
            assert!(k.command(c));
            assert_eq!(k.next(), Next::Keyboard);
        }
        assert!(k.command(command::WRITE_CONFIG));
        assert_eq!(k.data(), Next::Parameter(command::WRITE_CONFIG));
        // The mouse: through the controller, loopback tests, multiplexing.
        for c in [command::AUX_SEND, command::AUX_LOOP, command::KBD_LOOP, 0x90, 0x93] {
            assert!(k.command(c));
            assert_eq!(k.data(), Next::Parameter(c));
        }
        // A multiplexer's port prefix, then a command for that port.
        assert!(k.command(0x91));
        assert!(k.command(command::AUX_ENABLE));
        assert_eq!(k.data(), Next::Keyboard);
        assert!(k.command(command::KBD_DISABLE) && k.command(command::KBD_ENABLE) && k.command(command::AUX_DISABLE));
    }
}
