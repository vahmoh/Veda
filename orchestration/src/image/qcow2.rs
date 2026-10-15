// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Overlays of disk images in QEMU's qcow2 format (version 3): a small
//! file that holds the clusters of a disk that differ from a raw image,
//! its backing file, which QEMU reads for all the others. A GUI script's
//! machine boots from an overlay of the system image its run wrote once,
//! with its own boot configuration, rather than from a copy of it; what
//! the machine writes to its disk goes into the overlay too.
//!
//! The overlay is laid out as QEMU reads it (`docs/interop/qcow2.txt`):
//! the header, with the backing file's name and format, in the first
//! cluster; then the reference count table and its one block, the L1
//! table, the L2 tables of the clusters there are, and those clusters.
//! Every cluster of the file is used once (a count of 1), so QEMU can add
//! clusters after them.

use std::path::Path;

use crate::util::{self, Result};

/// The magic number that starts the file: `QFI\xfb`.
const MAGIC: u32 = 0x5146_49fb;
/// Clusters of 64 KiB, QEMU's own.
pub const CLUSTER_BITS: u32 = 16;
pub const CLUSTER: u64 = 1 << CLUSTER_BITS;
/// Entries in a table of a cluster (each eight bytes).
const ENTRIES: u64 = CLUSTER / 8;
/// Reference counts of 16 bits (`refcount_order` 4).
const REFCOUNT_ORDER: u32 = 4;
/// The header's length (version 3's fields, without the compression type).
const HEADER_LENGTH: u32 = 104;
/// The header extension that names the backing file's format.
const BACKING_FORMAT: u32 = 0xE279_2ACA;
/// An L1 or L2 entry's flag: the cluster it points to is used once.
const COPIED: u64 = 1 << 63;

/// Writes at `path` an overlay of the raw image `backing` (whose length is
/// `size`) that holds `clusters`: each one's index (its offset divided by
/// [`CLUSTER`]) and its 64 KiB.
pub fn write_overlay(path: &Path, backing: &Path, size: u64, clusters: &[(u64, Vec<u8>)]) -> Result {
    let name = backing.to_str().ok_or_else(|| format!("{}: not UTF-8", backing.display()))?;
    if !backing.is_absolute() || name.len() > 1023 {
        return Err(format!("{name}: an overlay's backing file is named by an absolute path of at most 1023 bytes"));
    }
    let mut indices: Vec<u64> = clusters.iter().map(|(i, _)| *i).collect();
    indices.sort_unstable();
    indices.dedup();
    if indices.len() != clusters.len()
        || clusters.iter().any(|(i, data)| data.len() as u64 != CLUSTER || *i >= size.div_ceil(CLUSTER))
    {
        return Err("an overlay's clusters are whole, each once, and within the disk".into());
    }
    let l1_entries = size.div_ceil(CLUSTER * ENTRIES).max(1);
    let l1_clusters = (l1_entries * 8).div_ceil(CLUSTER);
    // The L2 tables there are: one for each L1 entry a cluster is under.
    let mut l2s: Vec<u64> = clusters.iter().map(|(i, _)| i / ENTRIES).collect();
    l2s.sort_unstable();
    l2s.dedup();
    // Cluster 0 the header, 1 the reference count table, 2 its block,
    // then the L1 table, the L2 tables and the data.
    let (refcount_table, refcount_block, l1) = (1u64, 2u64, 3u64);
    let first_l2 = l1 + l1_clusters;
    let first_data = first_l2 + l2s.len() as u64;
    let total = first_data + clusters.len() as u64;
    if total > CLUSTER / 2 {
        return Err("an overlay too large for one block of reference counts".into());
    }
    let mut file = vec![0u8; (total * CLUSTER) as usize];
    let put32 =
        |file: &mut Vec<u8>, at: u64, v: u32| file[at as usize..at as usize + 4].copy_from_slice(&v.to_be_bytes());
    let put64 =
        |file: &mut Vec<u8>, at: u64, v: u64| file[at as usize..at as usize + 8].copy_from_slice(&v.to_be_bytes());

    // The header and its extensions, then the backing file's name.
    put32(&mut file, 0, MAGIC);
    put32(&mut file, 4, 3);
    put32(&mut file, 20, CLUSTER_BITS);
    put64(&mut file, 24, size);
    put32(&mut file, 36, l1_entries as u32);
    put64(&mut file, 40, l1 * CLUSTER);
    put64(&mut file, 48, refcount_table * CLUSTER);
    put32(&mut file, 56, 1);
    put32(&mut file, 96, REFCOUNT_ORDER);
    put32(&mut file, 100, HEADER_LENGTH);
    let mut at = HEADER_LENGTH as u64;
    put32(&mut file, at, BACKING_FORMAT);
    put32(&mut file, at + 4, 3);
    file[at as usize + 8..at as usize + 11].copy_from_slice(b"raw");
    at += 8 + 8;
    // The end of the extensions: a type and a length of 0.
    at += 8;
    put64(&mut file, 8, at);
    put32(&mut file, 16, name.len() as u32);
    file[at as usize..at as usize + name.len()].copy_from_slice(name.as_bytes());

    // Reference counts: one block, one count for each cluster.
    put64(&mut file, refcount_table * CLUSTER, refcount_block * CLUSTER);
    for cluster in 0..total {
        let entry = refcount_block * CLUSTER + cluster * 2;
        file[entry as usize..entry as usize + 2].copy_from_slice(&1u16.to_be_bytes());
    }
    // The L1 table points at the L2 tables, they at the data.
    for (n, l2) in l2s.iter().enumerate() {
        put64(&mut file, l1 * CLUSTER + l2 * 8, ((first_l2 + n as u64) * CLUSTER) | COPIED);
    }
    for (n, (index, data)) in clusters.iter().enumerate() {
        let l2 = first_l2 + l2s.iter().position(|t| *t == index / ENTRIES).unwrap_or_default() as u64;
        let host = (first_data + n as u64) * CLUSTER;
        put64(&mut file, l2 * CLUSTER + (index % ENTRIES) * 8, host | COPIED);
        file[host as usize..(host + CLUSTER) as usize].copy_from_slice(data);
    }
    util::write_atomically(path, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], at: u64) -> u32 {
        u32::from_be_bytes(b[at as usize..at as usize + 4].try_into().unwrap())
    }

    fn be64(b: &[u8], at: u64) -> u64 {
        u64::from_be_bytes(b[at as usize..at as usize + 8].try_into().unwrap())
    }

    /// The disk an overlay stands for, read as QEMU reads it: each
    /// cluster from the overlay where its L1 and L2 tables point, else
    /// from `backing`; and every cluster of the file counted once.
    fn read(overlay: &[u8], backing: &[u8]) -> (Vec<u8>, String, String) {
        assert_eq!(be32(overlay, 0), MAGIC);
        assert_eq!((be32(overlay, 4), be32(overlay, 20), be32(overlay, 96)), (3, CLUSTER_BITS, REFCOUNT_ORDER));
        let size = be64(overlay, 24);
        // The extensions: the backing format, then the end.
        let mut at = be32(overlay, 100) as u64;
        let mut format = String::new();
        loop {
            let (kind, len) = (be32(overlay, at), be32(overlay, at + 4) as u64);
            if kind == 0 {
                break;
            }
            if kind == BACKING_FORMAT {
                format = String::from_utf8(overlay[(at + 8) as usize..(at + 8 + len) as usize].to_vec()).unwrap();
            }
            at += 8 + len.next_multiple_of(8);
        }
        let (name_at, name_len) = (be64(overlay, 8), be32(overlay, 16) as u64);
        assert!(name_at + name_len <= CLUSTER);
        let name = String::from_utf8(overlay[name_at as usize..(name_at + name_len) as usize].to_vec()).unwrap();
        // Every cluster of the file has a count of one.
        let table = be64(overlay, 48);
        let block = be64(overlay, table);
        for cluster in 0..overlay.len() as u64 / CLUSTER {
            let count = u16::from_be_bytes(overlay[(block + cluster * 2) as usize..][..2].try_into().unwrap());
            assert_eq!(count, 1, "cluster {cluster}");
        }
        let (l1, l1_entries) = (be64(overlay, 40), be32(overlay, 36) as u64);
        let mut disk = backing.to_vec();
        disk.resize(size as usize, 0);
        for i in 0..l1_entries {
            let l2 = be64(overlay, l1 + i * 8);
            if l2 == 0 {
                continue;
            }
            assert_ne!(l2 & COPIED, 0);
            for j in 0..ENTRIES {
                let host = be64(overlay, (l2 & !COPIED) + j * 8);
                if host != 0 {
                    assert_ne!(host & COPIED, 0);
                    let guest = ((i * ENTRIES + j) * CLUSTER) as usize;
                    let data = &overlay[(host & !COPIED) as usize..][..CLUSTER as usize];
                    let end = (guest + CLUSTER as usize).min(disk.len());
                    disk[guest..end].copy_from_slice(&data[..end - guest]);
                }
            }
        }
        (disk, name, format)
    }

    #[test]
    fn an_overlay_holds_the_clusters_that_differ() {
        // A disk of 9.5 clusters; the second and the last differ.
        let size = 9 * CLUSTER + CLUSTER / 2;
        let backing: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let mut second = backing[CLUSTER as usize..2 * CLUSTER as usize].to_vec();
        second[100..105].copy_from_slice(b"hello");
        let mut last = vec![0u8; CLUSTER as usize];
        last[..10].copy_from_slice(b"0123456789");
        let path = std::env::temp_dir().join(format!("orchestration-overlay-{}.qcow2", std::process::id()));
        let name = Path::new("/images/veda.img");
        write_overlay(&path, name, size, &[(1, second.clone()), (9, last.clone())]).unwrap();
        let overlay = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let (disk, backing_name, format) = read(&overlay, &backing);
        assert_eq!((backing_name.as_str(), format.as_str()), ("/images/veda.img", "raw"));
        let mut want = backing.clone();
        want[CLUSTER as usize + 100..CLUSTER as usize + 105].copy_from_slice(b"hello");
        want[9 * CLUSTER as usize..9 * CLUSTER as usize + 10].copy_from_slice(b"0123456789");
        want[9 * CLUSTER as usize + 10..].fill(0);
        assert_eq!(disk, want);
        // Small: its tables and the two clusters.
        assert_eq!(overlay.len() as u64, 7 * CLUSTER);
    }

    #[test]
    fn overlays_are_refused_what_they_cannot_hold() {
        let path = std::env::temp_dir().join(format!("orchestration-overlay-bad-{}.qcow2", std::process::id()));
        let cluster = vec![0u8; CLUSTER as usize];
        assert!(write_overlay(&path, Path::new("relative.img"), CLUSTER, &[]).is_err());
        assert!(write_overlay(&path, Path::new("/a.img"), CLUSTER, &[(1, cluster.clone())]).is_err());
        assert!(write_overlay(&path, Path::new("/a.img"), CLUSTER, &[(0, vec![0; 10])]).is_err());
        assert!(write_overlay(&path, Path::new("/a.img"), 2 * CLUSTER, &[(0, cluster.clone()), (0, cluster)]).is_err());
        assert!(!path.exists());
    }
}
