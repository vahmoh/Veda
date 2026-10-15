// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! What Veda's media generators (`generation/`: `assetgen`, `musicgen`,
//! `videogen`) share: colours and the hashes of lattice points their
//! pictures are painted with, and their output, written whole and listed
//! in a manifest, which the system image is packed from
//! (`orchestration`). It is a library of the build machine's tools, on `std`.

pub mod colour;
pub mod hash;
pub mod output;
