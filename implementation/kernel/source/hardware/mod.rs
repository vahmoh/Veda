// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Hardware for user-space drivers: the resources that authorise access to
//! it ([`resource`]), and the I/O port ranges drivers are given
//! ([`ioport`]). A driver's interrupts are [`crate::interrupt`]'s objects,
//! and its device's registers and DMA memory are VMOs ([`crate::memory`]).

pub mod ioport;
pub mod resource;
