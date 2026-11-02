// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The firmware's IORT table (Arm's I/O Remapping Table): the SMMUs of the
//! machine, and how a PCI root's requester ids reach them.
//!
//! A table is a list of nodes, each with mappings of ids to other nodes.
//! What is read here is one thing: the SMMUv3 a PCI root on segment 0 goes
//! through, and how its requester ids become stream ids.

use alloc::vec::Vec;
use core::fmt;

/// Where a table's list of nodes is said to be, and how many there are.
const NODE_COUNT_AT: usize = 36;
const NODE_OFFSET_AT: usize = 40;

/// Node types.
const NODE_ROOT_COMPLEX: u8 = 2;
const NODE_SMMU_V3: u8 = 4;

/// An ID mapping's length.
const MAPPING: usize = 20;

/// Why a table cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IortError {
    /// It ends before what it says it has.
    Truncated,
}

impl fmt::Display for IortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the table ends early")
    }
}

/// An SMMUv3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Smmu {
    /// Where its registers are.
    pub base: u64,
    /// Its walks of memory see the processors' caches.
    pub coherent: bool,
    /// The interrupt of its event queue (0: none, or an MSI).
    pub event_interrupt: u32,
    /// Its global errors' interrupt.
    pub error_interrupt: u32,
    /// Where in the table its node is, as mappings name it.
    offset: u32,
}

/// A PCI root on segment 0's requester ids, and the stream ids they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Streams {
    /// The first requester id, and how many past it.
    pub first: u32,
    pub count: u32,
    /// The stream id of the first.
    pub stream: u32,
    /// The SMMU, if the root goes through one.
    smmu: u32,
}

/// What a table says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Iort {
    pub smmus: Vec<Smmu>,
    pub streams: Vec<Streams>,
}

impl Iort {
    /// The SMMU every function of the PCI root on segment 0 goes through,
    /// if the firmware says there is one.
    pub fn smmu(&self) -> Option<&Smmu> {
        let first = self.streams.first()?;
        let smmu = self.smmus.iter().find(|s| s.offset == first.smmu)?;
        self.streams.iter().all(|s| s.smmu == smmu.offset).then_some(smmu)
    }

    /// The stream id of requester id `requester`, if an SMMU sees it.
    pub fn stream_of(&self, requester: u16) -> Option<u32> {
        let id = requester as u32;
        let s = self.streams.iter().find(|s| id >= s.first && id - s.first <= s.count)?;
        Some(s.stream + (id - s.first))
    }
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, IortError> {
    let bytes = b.get(at..at + 4).ok_or(IortError::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, IortError> {
    Ok(u32_at(b, at)? as u64 | (u32_at(b, at + 4)? as u64) << 32)
}

/// Reads the IORT table `table`.
pub fn parse(table: &[u8]) -> Result<Iort, IortError> {
    let mut iort = Iort::default();
    let count = u32_at(table, NODE_COUNT_AT)?;
    let mut at = u32_at(table, NODE_OFFSET_AT)? as usize;
    for _ in 0..count {
        let header = table.get(at..at + 16).ok_or(IortError::Truncated)?;
        let (kind, length) = (header[0], u16::from_le_bytes([header[1], header[2]]) as usize);
        let node = table.get(at..at + length).ok_or(IortError::Truncated)?;
        match kind {
            NODE_SMMU_V3 => {
                iort.smmus.push(Smmu {
                    base: u64_at(node, 16)?,
                    coherent: u32_at(node, 24)? & 1 != 0,
                    event_interrupt: u32_at(node, 44)?,
                    error_interrupt: u32_at(node, 52)?,
                    offset: at as u32,
                });
            }
            NODE_ROOT_COMPLEX if u32_at(node, 28)? == 0 => {
                let (mappings, offset) = (u32_at(node, 8)? as usize, u32_at(node, 12)? as usize);
                for m in 0..mappings {
                    let map = offset + m * MAPPING;
                    let target = u32_at(node, map + 12)?;
                    // Those to an SMMU; the ones to the ITS bypass it.
                    iort.streams.push(Streams {
                        first: u32_at(node, map)?,
                        count: u32_at(node, map + 4)?,
                        stream: u32_at(node, map + 8)?,
                        smmu: target,
                    });
                }
            }
            _ => {}
        }
        if length == 0 {
            break;
        }
        at += length;
    }
    let known: Vec<u32> = iort.smmus.iter().map(|s| s.offset).collect();
    iort.streams.retain(|s| known.contains(&s.smmu));
    Ok(iort)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table with an ITS group, an SMMUv3 and a root complex that sends
    /// its 65536 requester ids to it, as QEMU's `virt` machine has it.
    fn table() -> Vec<u8> {
        let mut t = alloc::vec![0u8; 48];
        t[36..40].copy_from_slice(&3u32.to_le_bytes());
        t[40..44].copy_from_slice(&48u32.to_le_bytes());
        // The ITS group.
        let mut its = alloc::vec![0u8; 24];
        its[1] = 24;
        t.extend(&its);
        // The SMMU, at 72.
        let mut smmu = alloc::vec![0u8; 68];
        smmu[0] = NODE_SMMU_V3;
        smmu[1] = 68;
        smmu[16..24].copy_from_slice(&0x0905_0000u64.to_le_bytes());
        smmu[24..28].copy_from_slice(&1u32.to_le_bytes());
        smmu[44..48].copy_from_slice(&106u32.to_le_bytes());
        smmu[52..56].copy_from_slice(&108u32.to_le_bytes());
        t.extend(&smmu);
        // The root complex, with one mapping at 36.
        let mut root = alloc::vec![0u8; 36 + MAPPING];
        root[0] = NODE_ROOT_COMPLEX;
        root[1] = (36 + MAPPING) as u8;
        root[8..12].copy_from_slice(&1u32.to_le_bytes());
        root[12..16].copy_from_slice(&36u32.to_le_bytes());
        root[36 + 4..36 + 8].copy_from_slice(&0xFFFFu32.to_le_bytes());
        root[36 + 8..36 + 12].copy_from_slice(&0x100u32.to_le_bytes());
        root[36 + 12..36 + 16].copy_from_slice(&72u32.to_le_bytes());
        t.extend(&root);
        t
    }

    #[test]
    fn the_smmu_of_the_root_is_found() {
        let iort = parse(&table()).unwrap();
        let smmu = iort.smmu().unwrap();
        assert_eq!(smmu.base, 0x0905_0000);
        assert!(smmu.coherent);
        assert_eq!((smmu.event_interrupt, smmu.error_interrupt), (106, 108));
    }

    #[test]
    fn requester_ids_become_stream_ids() {
        let iort = parse(&table()).unwrap();
        assert_eq!(iort.stream_of(0), Some(0x100));
        assert_eq!(iort.stream_of(0x0208), Some(0x308));
    }

    #[test]
    fn a_root_that_goes_to_no_smmu_has_none() {
        let mut t = table();
        // The mapping names the ITS group, at 48.
        let map = 72 + 68 + 36 + 12;
        t[map..map + 4].copy_from_slice(&48u32.to_le_bytes());
        let iort = parse(&t).unwrap();
        assert_eq!(iort.smmu(), None);
        assert_eq!(iort.stream_of(0), None);
    }

    #[test]
    fn a_table_that_ends_early_is_refused() {
        let t = table();
        assert_eq!(parse(&t[..60]), Err(IortError::Truncated));
        assert_eq!(parse(&[]), Err(IortError::Truncated));
    }
}
