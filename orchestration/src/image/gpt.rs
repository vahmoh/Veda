// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! GUID Partition Table disk images: written (as their pieces), and read
//! for where their EFI system partition is.

use super::fat32::SECTOR;
use super::pieces::{Pieces, ReadAt};

/// Type GUID of an EFI System Partition.
const ESP_TYPE_GUID: [u8; 16] = guid(0xC12A7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);

const ENTRY_COUNT: usize = 128;
const ENTRY_SIZE: usize = 128;
const ENTRY_SECTORS: u64 = (ENTRY_COUNT * ENTRY_SIZE / SECTOR) as u64; // 32

/// Encodes a GUID in its on-disk mixed-endian form.
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// A partition to place on the disk.
pub struct Partition<'a> {
    pub name: &'a str,
    pub type_guid: [u8; 16],
    /// Volume contents; the partition is exactly this many bytes.
    pub data: Pieces<'a>,
}

impl<'a> Partition<'a> {
    pub fn esp(data: Pieces<'a>) -> Self {
        Partition { name: "EFI System", type_guid: ESP_TYPE_GUID, data }
    }
}

/// First LBA of the first partition (1 MiB alignment).
pub const FIRST_PARTITION_LBA: u64 = 2048;

/// Lays out a complete GPT disk containing `parts`, back to back on 1 MiB
/// boundaries.
pub fn build_disk(parts: Vec<Partition<'_>>) -> Pieces<'_> {
    let align = |lba: u64| lba.div_ceil(2048) * 2048;
    let mut lba = FIRST_PARTITION_LBA;
    let mut ranges = Vec::new();
    for p in &parts {
        assert_eq!(p.data.len() % SECTOR as u64, 0, "partition data must be whole sectors");
        let sectors = p.data.len() / SECTOR as u64;
        ranges.push((lba, lba + sectors - 1));
        lba = align(lba + sectors);
    }
    let total_sectors = lba + 2048; // room for the backup GPT, 1 MiB aligned
    let mut disk = Pieces::new(total_sectors * SECTOR as u64);

    // Protective MBR.
    let mut mbr = vec![0u8; SECTOR];
    let pe = &mut mbr[446..462];
    pe[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    pe[4] = 0xEE;
    pe[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    pe[8..12].copy_from_slice(&1u32.to_le_bytes());
    pe[12..16].copy_from_slice(&((total_sectors - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    disk.put(0, mbr);

    // Partition entry array.
    let mut entries = vec![0u8; ENTRY_COUNT * ENTRY_SIZE];
    for (i, (p, &(start, end))) in parts.iter().zip(&ranges).enumerate() {
        let e = &mut entries[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
        e[0..16].copy_from_slice(&p.type_guid);
        e[16..32].copy_from_slice(&pseudo_guid(0x7061_7274 + i as u64));
        e[32..40].copy_from_slice(&start.to_le_bytes());
        e[40..48].copy_from_slice(&end.to_le_bytes());
        for (j, ch) in p.name.encode_utf16().take(36).enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
    let entries_crc = crc32fast::hash(&entries);
    let last_lba = total_sectors - 1;
    let backup_entries_lba = last_lba - ENTRY_SECTORS;

    let header = |my_lba: u64, alt_lba: u64, entries_lba: u64| -> Vec<u8> {
        let mut h = vec![0u8; SECTOR];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my_lba.to_le_bytes());
        h[32..40].copy_from_slice(&alt_lba.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + ENTRY_SECTORS).to_le_bytes());
        h[48..56].copy_from_slice(&(backup_entries_lba - 1).to_le_bytes());
        h[56..72].copy_from_slice(&pseudo_guid(0x6469_736b));
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRY_COUNT as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32fast::hash(&h[0..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };

    let sector = SECTOR as u64;
    disk.put(sector, header(1, last_lba, 2));
    disk.put(2 * sector, entries.clone());
    disk.put(backup_entries_lba * sector, entries);
    disk.put(last_lba * sector, header(last_lba, 1, backup_entries_lba));
    for (p, (start, _)) in parts.into_iter().zip(ranges) {
        disk.place(start * sector, p.data);
    }
    disk
}

/// Where the EFI system partition of the GPT disk `disk` is: its first
/// byte and its length.
pub fn esp(disk: &(impl ReadAt + ?Sized)) -> Result<(u64, u64), String> {
    let header = disk.bytes(SECTOR as u64, SECTOR)?;
    if header[0..8] != *b"EFI PART" {
        return Err("no GUID partition table".into());
    }
    let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap()) as u64;
    let u64_at = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
    let (entries_lba, count, size) = (u64_at(72), u32_at(80), u32_at(84));
    if !(128..=4096).contains(&size) || count > 1024 {
        return Err("a GUID partition table with entries of no known size".into());
    }
    let entries = disk.bytes(entries_lba * SECTOR as u64, (count * size) as usize)?;
    let entry =
        entries.chunks_exact(size as usize).find(|e| e[0..16] == ESP_TYPE_GUID).ok_or("no EFI system partition")?;
    let (start, end) =
        (u64::from_le_bytes(entry[32..40].try_into().unwrap()), u64::from_le_bytes(entry[40..48].try_into().unwrap()));
    if end < start {
        return Err("an EFI system partition that ends before it starts".into());
    }
    Ok((start * SECTOR as u64, (end - start + 1) * SECTOR as u64))
}

/// A deterministic (reproducible) RFC 4122 version-4-shaped GUID.
fn pseudo_guid(seed: u64) -> [u8; 16] {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = [0u8; 16];
    for chunk in out.chunks_mut(8) {
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 33;
        chunk.copy_from_slice(&x.to_le_bytes());
    }
    out[7] = (out[7] & 0x0F) | 0x40;
    out[8] = (out[8] & 0x3F) | 0x80;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UEFI's CRC is CRC-32 of ISO-HDLC (IEEE 802.3), whose check value
    /// (of "123456789") is CBF43926.
    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn disk_layout() {
        let data = vec![0x11u8; 4 * 1024 * 1024];
        let mut part = Pieces::new(data.len() as u64);
        part.put(0, &data[..]);
        let disk = build_disk(vec![Partition::esp(part)]).to_vec();
        assert_eq!(&disk[SECTOR..SECTOR + 8], b"EFI PART");
        assert_eq!(disk[2048 * SECTOR], 0x11);
        let last = disk.len() - SECTOR;
        assert_eq!(&disk[last..last + 8], b"EFI PART");
        // Both headers' CRCs verify, and so does the entry array's.
        for at in [SECTOR, last] {
            let mut h = disk[at..at + 92].to_vec();
            let stored = u32::from_le_bytes(h[16..20].try_into().unwrap());
            h[16..20].fill(0);
            assert_eq!(crc32fast::hash(&h), stored);
            let entries_crc = u32::from_le_bytes(h[88..92].try_into().unwrap());
            let lba = u64::from_le_bytes(h[72..80].try_into().unwrap()) as usize;
            assert_eq!(crc32fast::hash(&disk[lba * SECTOR..lba * SECTOR + ENTRY_COUNT * ENTRY_SIZE]), entries_crc);
        }
        // The partition is where the table says.
        assert_eq!(esp(&disk), Ok((2048 * SECTOR as u64, 4 * 1024 * 1024)));
        assert!(esp(&vec![0u8; 4096]).is_err());
    }
}
