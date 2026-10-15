// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A minimal FAT32 formatter, which lays a volume out as its pieces (the
//! files' data borrowed, not copied), and a reader of files on such a
//! volume (what a test's guest wrote), a piece at a time.
//!
//! Only what the EFI system partition needs is implemented: short (8.3)
//! upper-case names, nested directories (empty ones too) and regular
//! files. The result is a spec-conformant volume that OVMF (and Windows,
//! Linux, ...) can mount.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::pieces::{Pieces, ReadAt};

pub const SECTOR: usize = 512;
const RESERVED_SECTORS: u32 = 32;
const NUM_FATS: u32 = 2;
const MIN_FAT32_CLUSTERS: u32 = 65_525;
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;

const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_VOLUME_ID: u8 = 0x08;

#[derive(Default)]
struct Dir<'a> {
    dirs: BTreeMap<String, Dir<'a>>,
    files: BTreeMap<String, &'a [u8]>,
}

/// An in-memory directory tree that is laid out by [`Fat32Builder::build`].
#[derive(Default)]
pub struct Fat32Builder<'a> {
    root: Dir<'a>,
    label: String,
}

/// Fixed timestamp for reproducible images: 2026-01-01 12:00:00.
const FAT_DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const FAT_TIME: u16 = 12 << 11;

/// A component of a path as a directory entry has it (8.3, upper case).
pub fn short_name(component: &str) -> Result<[u8; 11], String> {
    let upper = component.to_ascii_uppercase();
    let (base, ext) = match upper.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (upper.as_str(), ""),
    };
    let valid = |s: &str| s.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-~!#$%&'()@^`{}".contains(&c));
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || !valid(base) || !valid(ext) {
        return Err(format!("'{component}' is not a valid 8.3 file name"));
    }
    let mut name = [b' '; 11];
    name[..base.len()].copy_from_slice(base.as_bytes());
    name[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(name)
}

impl<'a> Fat32Builder<'a> {
    pub fn new(label: &str) -> Self {
        Self { root: Dir::default(), label: label.to_ascii_uppercase() }
    }

    /// Adds a file at `path` (components separated by `/`), creating parent
    /// directories as needed.
    pub fn add_file(&mut self, path: &str, data: &'a [u8]) -> Result<(), String> {
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let file = parts.pop().ok_or("empty path")?;
        let dir = self.dir(&parts)?;
        short_name(file)?;
        dir.files.insert(file.to_ascii_uppercase(), data);
        Ok(())
    }

    /// Adds the directory at `path` (empty, if nothing goes into it),
    /// creating parent directories as needed.
    pub fn add_dir(&mut self, path: &str) -> Result<(), String> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        self.dir(&parts).map(drop)
    }

    /// The directory at `parts`, made where it is not.
    fn dir(&mut self, parts: &[&str]) -> Result<&mut Dir<'a>, String> {
        let mut dir = &mut self.root;
        for p in parts {
            short_name(p)?;
            dir = dir.dirs.entry(p.to_ascii_uppercase()).or_default();
        }
        Ok(dir)
    }

    /// Lays the volume out in exactly `total_sectors` sectors.
    /// `hidden_sectors` is the partition's starting LBA.
    pub fn build(&self, total_sectors: u32, hidden_sectors: u32) -> Result<Pieces<'a>, String> {
        let geometry = Geometry::choose(total_sectors)?;
        let mut vol = Volume {
            pieces: Pieces::new(total_sectors as u64 * SECTOR as u64),
            fat: vec![0u32; geometry.cluster_count as usize + 2],
            next_free: 2,
            g: geometry,
        };
        vol.fat[0] = 0x0FFF_FFF8;
        vol.fat[1] = END_OF_CHAIN;

        // The root directory always starts at cluster 2.
        let root_cluster = vol.write_dir(&self.root, None, Some(&self.label))?;
        assert_eq!(root_cluster, 2);

        let used = vol.next_free - 2;
        let free = vol.g.cluster_count - used;
        vol.write_boot_sectors(hidden_sectors, free, &self.label);
        vol.write_fats();
        Ok(vol.pieces)
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    total_sectors: u32,
    sectors_per_cluster: u32,
    fat_sectors: u32,
    cluster_count: u32,
}

impl Geometry {
    /// Picks the largest cluster size (up to 4 KiB) that still yields a valid
    /// FAT32 cluster count, following the computation in Microsoft's FAT spec.
    fn choose(total_sectors: u32) -> Result<Self, String> {
        let too_small = || format!("volume of {total_sectors} sectors is too small for FAT32");
        let tmp1 = total_sectors.checked_sub(RESERVED_SECTORS).ok_or_else(too_small)?;
        for spc in [8u32, 4, 2, 1] {
            let tmp2 = (256 * spc + NUM_FATS) / 2;
            let fat_sectors = tmp1.div_ceil(tmp2);
            let Some(data_sectors) = tmp1.checked_sub(NUM_FATS * fat_sectors) else { continue };
            let cluster_count = data_sectors / spc;
            if cluster_count >= MIN_FAT32_CLUSTERS + 16 {
                return Ok(Geometry { total_sectors, sectors_per_cluster: spc, fat_sectors, cluster_count });
            }
        }
        Err(too_small())
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    fn data_start_sector(&self) -> u32 {
        RESERVED_SECTORS + NUM_FATS * self.fat_sectors
    }
}

struct Volume<'a> {
    pieces: Pieces<'a>,
    fat: Vec<u32>,
    next_free: u32,
    g: Geometry,
}

impl<'a> Volume<'a> {
    /// Allocates a contiguous cluster chain big enough for `bytes` (at least
    /// one cluster) and returns its first cluster.
    fn alloc_chain(&mut self, bytes: usize) -> Result<u32, String> {
        let n = bytes.div_ceil(self.g.cluster_bytes()).max(1) as u32;
        let first = self.next_free;
        if first + n > self.g.cluster_count + 2 {
            return Err("FAT32 volume is full; increase the ESP size".into());
        }
        for c in first..first + n {
            self.fat[c as usize] = if c == first + n - 1 { END_OF_CHAIN } else { c + 1 };
        }
        self.next_free += n;
        Ok(first)
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        let sector = self.g.data_start_sector() as u64 + (cluster as u64 - 2) * self.g.sectors_per_cluster as u64;
        sector * SECTOR as u64
    }

    /// Writes `dir` (recursively) and returns its first cluster.
    fn write_dir(&mut self, dir: &Dir<'a>, parent: Option<u32>, label: Option<&str>) -> Result<u32, String> {
        let is_root = parent.is_none();
        let entries = dir.dirs.len() + dir.files.len() + if is_root { 1 } else { 2 };
        let first = self.alloc_chain(entries * 32)?;

        let mut buf: Vec<u8> = Vec::with_capacity(entries * 32);
        if let Some(label) = label {
            let mut name = [b' '; 11];
            for (i, b) in label.bytes().take(11).enumerate() {
                name[i] = b;
            }
            push_entry(&mut buf, &name, ATTR_VOLUME_ID, 0, 0);
        }
        if let Some(parent) = parent {
            push_entry(&mut buf, b".          ", ATTR_DIRECTORY, first, 0);
            // A ".." entry pointing at the root uses cluster 0.
            let p = if parent == 2 { 0 } else { parent };
            push_entry(&mut buf, b"..         ", ATTR_DIRECTORY, p, 0);
        }
        for (name, sub) in &dir.dirs {
            let cluster = self.write_dir(sub, Some(first), None)?;
            push_entry(&mut buf, &short_name(name)?, ATTR_DIRECTORY, cluster, 0);
        }
        for (name, data) in &dir.files {
            let cluster = if data.is_empty() { 0 } else { self.alloc_chain(data.len())? };
            if cluster != 0 {
                let at = self.cluster_offset(cluster);
                self.pieces.put(at, Cow::Borrowed(*data));
            }
            push_entry(&mut buf, &short_name(name)?, ATTR_ARCHIVE, cluster, data.len() as u32);
        }
        let at = self.cluster_offset(first);
        self.pieces.put(at, buf);
        Ok(first)
    }

    fn write_boot_sectors(&mut self, hidden: u32, free_clusters: u32, label: &str) {
        let g = self.g;
        let mut bs = [0u8; SECTOR];
        bs[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        bs[3..11].copy_from_slice(b"VEDA    ");
        bs[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        bs[13] = g.sectors_per_cluster as u8;
        bs[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
        bs[16] = NUM_FATS as u8;
        bs[21] = 0xF8; // fixed media
        bs[24..26].copy_from_slice(&63u16.to_le_bytes());
        bs[26..28].copy_from_slice(&255u16.to_le_bytes());
        bs[28..32].copy_from_slice(&hidden.to_le_bytes());
        bs[32..36].copy_from_slice(&g.total_sectors.to_le_bytes());
        bs[36..40].copy_from_slice(&g.fat_sectors.to_le_bytes());
        bs[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
        bs[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
        bs[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
        bs[64] = 0x80;
        bs[66] = 0x29;
        bs[67..71].copy_from_slice(&0x5645_4441u32.to_le_bytes()); // volume id "VEDA"
        let mut lab = [b' '; 11];
        for (i, b) in label.bytes().take(11).enumerate() {
            lab[i] = b;
        }
        bs[71..82].copy_from_slice(&lab);
        bs[82..90].copy_from_slice(b"FAT32   ");
        bs[510] = 0x55;
        bs[511] = 0xAA;

        let mut fsinfo = [0u8; SECTOR];
        fsinfo[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        fsinfo[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        fsinfo[488..492].copy_from_slice(&free_clusters.to_le_bytes());
        fsinfo[492..496].copy_from_slice(&self.next_free.to_le_bytes());
        fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

        for (sector, data) in [(0u64, &bs), (1, &fsinfo), (6, &bs), (7, &fsinfo)] {
            self.pieces.put(sector * SECTOR as u64, data.to_vec());
        }
    }

    /// The two FATs, as far as clusters are used (free ones are zeros).
    fn write_fats(&mut self) {
        let used = self.next_free as usize;
        let fat: Vec<u8> = self.fat[..used].iter().flat_map(|e| e.to_le_bytes()).collect();
        let fat_bytes = self.g.fat_sectors as u64 * SECTOR as u64;
        for n in 0..NUM_FATS as u64 {
            self.pieces.put(RESERVED_SECTORS as u64 * SECTOR as u64 + n * fat_bytes, fat.clone());
        }
    }
}

/// Where a file is on a FAT32 volume: its directory entry and its data.
pub struct Located {
    /// The directory entry (its 32 bytes), from the volume's start.
    pub entry: u64,
    /// The file's clusters, in order: where each one starts.
    pub clusters: Vec<u64>,
    /// Bytes in a cluster.
    pub cluster_bytes: usize,
    /// The file's size, as its entry gives it.
    pub size: usize,
}

/// The FAT32 volume of `volume`'s bytes, read a piece at a time.
struct Reader<'r, R: ReadAt + ?Sized> {
    volume: &'r R,
    cluster_bytes: usize,
    fat_at: u64,
    data_at: u64,
    root: u32,
}

impl<'r, R: ReadAt + ?Sized> Reader<'r, R> {
    fn new(volume: &'r R) -> Result<Self, String> {
        let not_fat32 = || String::from("not a FAT32 file system");
        let boot = volume.bytes(0, SECTOR).map_err(|_| not_fat32())?;
        if boot[82..90] != *b"FAT32   " {
            return Err(not_fat32());
        }
        let u16_at = |at: usize| u16::from_le_bytes([boot[at], boot[at + 1]]) as u64;
        let u32_at = |at: usize| u32::from_le_bytes([boot[at], boot[at + 1], boot[at + 2], boot[at + 3]]);
        let sector = u16_at(11);
        let cluster_bytes = (sector * boot[13] as u64) as usize;
        if sector == 0 || cluster_bytes == 0 {
            return Err(not_fat32());
        }
        let fat_at = sector * u16_at(14);
        let data_at = fat_at + boot[16] as u64 * sector * u32_at(36) as u64;
        Ok(Reader { volume, cluster_bytes, fat_at, data_at, root: u32_at(44) })
    }

    /// Where the clusters of the chain from `first` start, in order.
    fn chain(&self, first: u32) -> Result<Vec<u64>, String> {
        let length = self.volume.length()?;
        let mut clusters = Vec::new();
        let mut cluster = first;
        while (2..0x0FFF_FFF8).contains(&cluster) {
            let at = self.data_at + (cluster as u64 - 2) * self.cluster_bytes as u64;
            if at + self.cluster_bytes as u64 > length {
                return Err("a cluster beyond the volume".into());
            }
            clusters.push(at);
            if clusters.len() as u64 * self.cluster_bytes as u64 > length {
                return Err("a cluster chain that loops".into());
            }
            let next =
                self.volume.bytes(self.fat_at + cluster as u64 * 4, 4).map_err(|_| "a cluster beyond the FAT")?;
            cluster = u32::from_le_bytes([next[0], next[1], next[2], next[3]]) & 0x0FFF_FFFF;
        }
        Ok(clusters)
    }

    /// The bytes of the clusters at `clusters`.
    fn read(&self, clusters: &[u64]) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::with_capacity(clusters.len() * self.cluster_bytes);
        for &at in clusters {
            bytes.extend(self.volume.bytes(at, self.cluster_bytes)?);
        }
        Ok(bytes)
    }

    /// Finds the file at `path`.
    fn locate(&self, path: &str) -> Result<Located, String> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let mut dir = self.chain(self.root)?;
        for (i, part) in parts.iter().enumerate() {
            let name = short_name(part)?;
            let entries = self.read(&dir)?;
            let index = entries
                .as_chunks::<32>()
                .0
                .iter()
                .take_while(|e| e[0] != 0)
                .position(|e| e[0] != 0xE5 && e[11] != 0x0F && e[11] & ATTR_VOLUME_ID == 0 && e[..11] == name)
                .ok_or_else(|| format!("{path}: no {part}"))?;
            let entry = &entries[index * 32..index * 32 + 32];
            let first = (u16::from_le_bytes([entry[20], entry[21]]) as u32) << 16
                | u16::from_le_bytes([entry[26], entry[27]]) as u32;
            let size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;
            let last = i + 1 == parts.len();
            match (entry[11] & ATTR_DIRECTORY != 0, last) {
                (false, true) => {
                    let clusters = if first == 0 { Vec::new() } else { self.chain(first)? };
                    if clusters.len() * self.cluster_bytes < size {
                        return Err(format!("{path}: shorter than its size"));
                    }
                    // The entry's place: its cluster of the directory, and
                    // where in it.
                    let per_cluster = self.cluster_bytes / 32;
                    let entry = dir[index / per_cluster] + (index % per_cluster * 32) as u64;
                    return Ok(Located { entry, clusters, cluster_bytes: self.cluster_bytes, size });
                }
                (true, false) => dir = self.chain(first)?,
                (true, true) => return Err(format!("{path}: a directory")),
                (false, false) => return Err(format!("{path}: {part} is a file")),
            }
        }
        Err(format!("{path}: no file named"))
    }
}

/// Where the file at `path` (8.3 names, any case) is on the FAT32 volume
/// `volume`, as the volume has it: written by [`Fat32Builder`] or by anyone.
pub fn locate(volume: &(impl ReadAt + ?Sized), path: &str) -> Result<Located, String> {
    Reader::new(volume)?.locate(path)
}

/// The file at `path` on the FAT32 volume `volume`.
pub fn read_file(volume: &(impl ReadAt + ?Sized), path: &str) -> Result<Vec<u8>, String> {
    let reader = Reader::new(volume)?;
    let located = reader.locate(path)?;
    let mut data = reader.read(&located.clusters)?;
    data.truncate(located.size);
    Ok(data)
}

fn push_entry(buf: &mut Vec<u8>, name: &[u8; 11], attr: u8, cluster: u32, size: u32) {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(name);
    e[11] = attr;
    e[14..16].copy_from_slice(&FAT_TIME.to_le_bytes());
    e[16..18].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[18..20].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&FAT_TIME.to_le_bytes());
    e[24..26].copy_from_slice(&FAT_DATE.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&e);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        assert_eq!(&short_name("loader1.efi").unwrap(), b"LOADER1 EFI");
        assert_eq!(&short_name("EFI").unwrap(), b"EFI        ");
        assert!(short_name("waytoolongname.txt").is_err());
        assert!(short_name("a.toolong").is_err());
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let mut b = Fat32Builder::new("VEDA");
        let big: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        b.add_file("VEDA/BOOT.CFG", b"run=editor").unwrap();
        b.add_file("VEDA/BIG.BIN", &big).unwrap();
        b.add_file("VEDA/EMPTY.TXT", b"").unwrap();
        b.add_dir("VEDA/LOGS").unwrap();
        let img = b.build(64 * 1024 * 1024 / SECTOR as u32, 0).unwrap().to_vec();
        assert_eq!(read_file(&img, "veda/boot.cfg").unwrap(), b"run=editor");
        assert_eq!(read_file(&img, "VEDA/BIG.BIN").unwrap(), big);
        assert_eq!(read_file(&img, "VEDA/EMPTY.TXT").unwrap(), b"");
        assert!(read_file(&img, "VEDA/LOGS").unwrap_err().contains("a directory"));
        assert!(read_file(&img, "VEDA/LOGS/0001/VEDA.TXT").unwrap_err().contains("no 0001"));
        assert!(read_file(&img, "VEDA/NONE.TXT").is_err());
        assert!(read_file(&vec![0u8; 4096], "VEDA/BOOT.CFG").is_err());
        // Where the file is: its entry holds its size, its cluster its data.
        let located = locate(&img, "VEDA/BOOT.CFG").unwrap();
        let entry = located.entry as usize;
        assert_eq!(&img[entry..entry + 11], b"BOOT    CFG");
        assert_eq!(u32::from_le_bytes(img[entry + 28..entry + 32].try_into().unwrap()), 10);
        let at = located.clusters[0] as usize;
        assert_eq!(&img[at..at + 10], b"run=editor");
    }

    #[test]
    fn builds_volume_with_files() {
        let mut b = Fat32Builder::new("VEDA");
        let loader = vec![0xAB; 10_000];
        b.add_file("EFI/BOOT/LOADER.EFI", &loader).unwrap();
        b.add_file("VEDA/BOOT.CFG", b"hello").unwrap();
        let sectors = 128 * 1024 * 1024 / SECTOR as u32;
        let img = b.build(sectors, 2048).unwrap().to_vec();
        assert_eq!(img.len(), sectors as usize * SECTOR);
        assert_eq!(&img[510..512], &[0x55, 0xAA]);
        assert_eq!(&img[82..90], b"FAT32   ");
        let g = Geometry::choose(sectors).unwrap();
        assert!(g.cluster_count >= MIN_FAT32_CLUSTERS);
        // The first FAT entry carries the media descriptor, in both FATs.
        let fat0 = RESERVED_SECTORS as usize * SECTOR;
        assert_eq!(&img[fat0..fat0 + 4], &0x0FFF_FFF8u32.to_le_bytes());
        let fat1 = fat0 + g.fat_sectors as usize * SECTOR;
        assert_eq!(&img[fat1..fat1 + 4], &0x0FFF_FFF8u32.to_le_bytes());
        assert_eq!(read_file(&img, "EFI/BOOT/LOADER.EFI").unwrap(), loader);
        // Too small a volume is refused, not a panic.
        assert!(Geometry::choose(16).is_err());
        assert!(Fat32Builder::new("VEDA").build(1000, 0).is_err());
    }
}
