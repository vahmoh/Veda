// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Assembly of the bootable Veda disk image, of the live system's ISO
//! image, and of overlays of a disk image with a boot configuration of
//! their own. Images are laid out as their pieces and written with their
//! free space left out (`pieces`), their EFI system partitions as large as
//! their files and the room the logs may take.

pub mod fat32;
pub mod gpt;
pub mod iso9660;
pub mod pieces;
pub mod qcow2;

use std::fs::File;
use std::path::Path;

use fat32::{Fat32Builder, SECTOR};
use pieces::{Part, ReadAt};

use crate::util::{self, Result};

/// Files placed on the EFI system partition.
pub struct EspContents<'a> {
    pub bootloader: &'a [u8],
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
    pub boot_cfg: &'a str,
}

/// Where the boot configuration is on the partition.
const BOOT_CFG: &str = "VEDA/BOOT.CFG";

impl<'a> EspContents<'a> {
    /// The partition's files: where they go, and what they hold.
    fn files(&self) -> [(&'static str, &'a [u8]); 4] {
        [
            (crate::architecture::UEFI_BOOT_PATH, self.bootloader),
            ("VEDA/KERNEL.ELF", self.kernel),
            ("VEDA/INITRD.IMG", self.initrd),
            (BOOT_CFG, self.boot_cfg.as_bytes()),
        ]
    }

    /// A FAT32 file system with the partition's files and directories; and
    /// the files' size.
    fn fat(&self) -> Result<(Fat32Builder<'a>, u64)> {
        let mut fat = Fat32Builder::new("VEDA");
        let mut payload = 0;
        for (path, data) in self.files() {
            fat.add_file(path, data)?;
            payload += data.len() as u64;
        }
        fat.add_dir(LOGS)?;
        Ok((fat, payload))
    }
}

/// Where a stick Veda started from keeps the system's logs (the driver
/// VM's `logkeeper`, which keeps them on a disk that has the directory).
pub const LOGS: &str = "VEDA/LOGS";
/// The room an image leaves for them (a disk image written to a stick
/// keeps them too).
const LOGS_ROOM_MIB: u64 = 64;

/// The sectors of an EFI system partition for `payload` bytes of files: a
/// little room beyond them (for the file system's own tables and the
/// clusters' slack), and the logs'; at least the 40 MiB a FAT32 file
/// system takes (65525 clusters).
fn esp_sectors(payload: u64) -> u32 {
    let mib = (payload * 11 / 10 / (1024 * 1024) + 4 + LOGS_ROOM_MIB).max(40);
    (mib * 1024 * 1024 / SECTOR as u64) as u32
}

/// Builds a GPT disk with a single FAT32 EFI system partition and writes it
/// to `out`; returns its size.
pub fn write_disk_image(out: &Path, esp: &EspContents) -> Result<u64> {
    let (fat, payload) = esp.fat()?;
    let volume = fat.build(esp_sectors(payload), gpt::FIRST_PARTITION_LBA as u32)?;
    gpt::build_disk(vec![gpt::Partition::esp(volume)]).write(out)
}

/// Builds the live system's ISO image (see [`iso9660`]) and writes it to
/// `out`: the partition's files in an ISO 9660 file system, and the
/// partition itself as the UEFI boot image and a stick's EFI system
/// partition.
pub fn write_iso_image(out: &Path, esp: &EspContents) -> Result<u64> {
    let (fat, payload) = esp.fat()?;
    let sectors = esp_sectors(payload);
    let mut iso = iso9660::IsoBuilder::new("VEDA");
    for (path, data) in esp.files() {
        iso.add_file(path, data)?;
    }
    // A stick the ISO's files are copied onto (by Rufus) keeps the logs too.
    iso.add_dir(LOGS)?;
    iso.build(|start| fat.build(sectors, start))?.write(out)
}

/// Where the EFI system partition of `image` is (its first byte, its
/// length): a disk's, which its GUID partition table names, or a stick's
/// the live system's image was written to, which its MBR names.
fn esp(image: &(impl ReadAt + ?Sized)) -> Result<(u64, u64)> {
    if let Ok(found) = gpt::esp(image) {
        return Ok(found);
    }
    const MBR_TYPE_ESP: u8 = 0xEF;
    let mbr = image.bytes(0, SECTOR).map_err(|_| "no partition table")?;
    if mbr[510..512] != [0x55, 0xAA] {
        return Err(String::from("no partition table"));
    }
    let entry =
        mbr[446..510].as_chunks::<16>().0.iter().find(|e| e[4] == MBR_TYPE_ESP).ok_or("no EFI system partition")?;
    let start = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64 * SECTOR as u64;
    let len = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as u64 * SECTOR as u64;
    if start + len > image.length()? {
        return Err(String::from("the partition goes beyond the image"));
    }
    Ok((start, len))
}

/// The file at `path` on the EFI system partition of `image` (a disk or a
/// stick): what a test's guest wrote there. Only the partition's tables
/// and the file are read.
pub fn esp_file(image: &(impl ReadAt + ?Sized), path: &str) -> Result<Vec<u8>> {
    let (start, len) = esp(image)?;
    fat32::read_file(&Part { image, start, len }, path)
}

/// Writes at `overlay` an overlay of the disk image `base` (see `qcow2`)
/// whose boot configuration is `boot_cfg`: the file's clusters and its
/// directory entry (its size) are the overlay's, the rest the image's.
pub fn write_boot_overlay(base: &Path, overlay: &Path, boot_cfg: &str) -> Result {
    let file = File::open(base).map_err(|e| format!("reading {}: {e}", base.display()))?;
    let in_base = |e: String| format!("{}: {e}", base.display());
    let size = file.length().map_err(in_base)?;
    let (start, len) = esp(&file).map_err(in_base)?;
    let found = fat32::locate(&Part { image: &file, start, len }, BOOT_CFG).map_err(in_base)?;
    let data = boot_cfg.as_bytes();
    if data.len() > found.clusters.len() * found.cluster_bytes {
        return Err(format!("{}: the boot configuration has no room for {} bytes", base.display(), data.len()));
    }
    // What changes: the file's clusters (what it does not fill of them
    // zeros), and the size in its entry.
    let mut content = data.to_vec();
    content.resize(found.clusters.len() * found.cluster_bytes, 0);
    let mut patches: Vec<(u64, &[u8])> =
        found.clusters.iter().zip(content.chunks(found.cluster_bytes)).map(|(at, bytes)| (start + at, bytes)).collect();
    let size_field = (data.len() as u32).to_le_bytes();
    patches.push((start + found.entry + 28, &size_field));
    // The overlay's clusters: those the changes are in, as the image has
    // them, changed.
    let mut clusters: Vec<(u64, Vec<u8>)> = Vec::new();
    for (at, bytes) in patches {
        for (i, &byte) in bytes.iter().enumerate() {
            let place = at + i as u64;
            let index = place / qcow2::CLUSTER;
            let cluster = match clusters.iter().position(|(c, _)| *c == index) {
                Some(n) => n,
                None => {
                    let mut data = vec![0u8; qcow2::CLUSTER as usize];
                    let from = index * qcow2::CLUSTER;
                    let available = size.saturating_sub(from).min(qcow2::CLUSTER) as usize;
                    file.read_at(from, &mut data[..available]).map_err(in_base)?;
                    clusters.push((index, data));
                    clusters.len() - 1
                }
            };
            clusters[cluster].1[(place % qcow2::CLUSTER) as usize] = byte;
        }
    }
    let base = std::fs::canonicalize(base).map_err(|e| format!("{}: {e}", base.display()))?;
    qcow2::write_overlay(overlay, &base, size, &clusters)?;
    util::status("Imaging", format!("{} (an overlay of {})", overlay.display(), base.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esp_contents(boot_cfg: &str) -> EspContents<'_> {
        EspContents { bootloader: b"MZ loader", kernel: b"ELF kernel", initrd: &[7; 10_000], boot_cfg }
    }

    #[test]
    fn the_live_image_has_room_for_the_logs() {
        let path = std::env::temp_dir().join(format!("automation-live-{}.iso", std::process::id()));
        write_iso_image(&path, &esp_contents("run=editor")).unwrap();
        let image = File::open(&path).unwrap();
        assert_eq!(esp_file(&image, "VEDA/BOOT.CFG").unwrap(), b"run=editor");
        assert_eq!(esp_file(&image, &crate::architecture::UEFI_BOOT_PATH.to_ascii_lowercase()).unwrap(), b"MZ loader");
        assert!(esp_file(&image, LOGS).unwrap_err().contains("a directory"));
        assert!(image.length().unwrap() > LOGS_ROOM_MIB * 1024 * 1024);
        assert!(esp_file(&vec![0u8; 4096], "VEDA/BOOT.CFG").is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn disk_images_are_as_large_as_their_files_and_the_logs() {
        let path = std::env::temp_dir().join(format!("automation-disk-{}.img", std::process::id()));
        let initrd = vec![9u8; 30 << 20];
        let esp = EspContents { initrd: &initrd, ..esp_contents("cmdline=x") };
        let size = write_disk_image(&path, &esp).unwrap();
        // The files, a tenth more, the logs' room, and the partition table.
        let mib = size >> 20;
        assert!((30 + 64..=30 + 3 + 4 + 64 + 3).contains(&mib), "{mib} MiB");
        let image = File::open(&path).unwrap();
        assert_eq!(esp_file(&image, "VEDA/BOOT.CFG").unwrap(), b"cmdline=x");
        assert_eq!(esp_file(&image, "VEDA/INITRD.IMG").unwrap(), initrd);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn overlays_have_a_boot_configuration_of_their_own() {
        let dir = std::env::temp_dir().join(format!("automation-boot-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base, overlay) = (dir.join("veda.img"), dir.join("boot.qcow2"));
        write_disk_image(&base, &esp_contents("cmdline=base\n")).unwrap();
        let longer = format!("resolution=1280x800\ncmdline={}\n", "x".repeat(256));
        write_boot_overlay(&base, &overlay, &longer).unwrap();
        // The disk the overlay stands for: the base with the overlay's
        // clusters.
        let mut disk = std::fs::read(&base).unwrap();
        let file = std::fs::read(&overlay).unwrap();
        let be64 = |at: usize| u64::from_be_bytes(file[at..at + 8].try_into().unwrap());
        let l2 = be64(be64(40) as usize) & !(1 << 63);
        for j in 0..qcow2::CLUSTER as usize / 8 {
            let host = be64(l2 as usize + j * 8) & !(1 << 63);
            if host != 0 {
                let at = j * qcow2::CLUSTER as usize;
                let end = (at + qcow2::CLUSTER as usize).min(disk.len());
                disk[at..end].copy_from_slice(&file[host as usize..host as usize + end - at]);
            }
        }
        assert_eq!(esp_file(&disk, "VEDA/BOOT.CFG").unwrap(), longer.as_bytes());
        assert_eq!(esp_file(&disk, "VEDA/INITRD.IMG").unwrap(), [7; 10_000]);
        // The base is as it was.
        assert_eq!(esp_file(&File::open(&base).unwrap(), "VEDA/BOOT.CFG").unwrap(), b"cmdline=base\n");
        // A configuration larger than its clusters is refused.
        assert!(write_boot_overlay(&base, &overlay, &"x".repeat(1 << 20)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
