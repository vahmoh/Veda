//! `viommu` — what Veda knows of IOMMUs that touches no hardware.
//!
//! Each architecture's IOMMU is in `lib/iommu/arch/<architecture>/`:
//! [`amd64`], Intel VT-d. The kernel drives the units with it (its
//! architecture's `iommu`), and `devmgr` reads what the firmware's tables
//! say of them; the host's unit tests check the encodings against the
//! specification.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[path = "../arch/amd64/mod.rs"]
pub mod amd64;
