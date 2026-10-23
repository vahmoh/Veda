// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A generator's output: the files it writes into its directory, each
//! whole or not at all (under a temporary name, then renamed), and listed
//! once all are written in the directory's manifest ([`MANIFEST`]), one
//! path a line. The system image takes the files the manifest lists, and
//! nothing else; what the directory holds besides (what an earlier
//! version wrote) is removed.

use std::path::{Path, PathBuf};

/// The manifest's name in a generator's directory.
pub const MANIFEST: &str = ".manifest";

/// A generator's output directory, being written.
pub struct Output {
    dir: PathBuf,
    written: Vec<String>,
}

impl Output {
    /// The output directory `dir`, made if it is not there.
    pub fn new(dir: &Path) -> Result<Output, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        Ok(Output { dir: dir.to_path_buf(), written: Vec::new() })
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `data` at `path` (relative, components separated by `/`),
    /// whole or not at all.
    pub fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        if path.split('/').any(|p| p.is_empty() || p == "." || p == "..") || path == MANIFEST {
            return Err(format!("{path}: not a path in an output directory"));
        }
        write_whole(&self.dir.join(path), data)?;
        self.written.push(path.to_string());
        Ok(())
    }

    /// Lists what has been written in the manifest, and removes every other
    /// file of the directory.
    pub fn finish(mut self) -> Result<(), String> {
        self.written.sort();
        self.written.dedup();
        let mut text = String::new();
        for path in &self.written {
            text.push_str(path);
            text.push('\n');
        }
        write_whole(&self.dir.join(MANIFEST), text.as_bytes())?;
        for file in files_under(&self.dir)? {
            let relative = file.strip_prefix(&self.dir).unwrap_or(&file).to_string_lossy().replace('\\', "/");
            if relative != MANIFEST && self.written.binary_search(&relative).is_err() {
                std::fs::remove_file(&file).map_err(|e| format!("removing {}: {e}", file.display()))?;
            }
        }
        Ok(())
    }
}

/// The paths the manifest of the output directory `dir` lists; `None` if
/// it has none (its generator has not finished a run).
pub fn manifest(dir: &Path) -> Result<Option<Vec<String>>, String> {
    match std::fs::read_to_string(dir.join(MANIFEST)) {
        Ok(text) => Ok(Some(text.lines().filter(|l| !l.is_empty()).map(String::from).collect())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", dir.join(MANIFEST).display())),
    }
}

/// Writes `data` at `path` under a temporary name of this process's beside
/// it, then renames it.
fn write_whole(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| format!("{}: no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let partial = dir.join(format!(".{name}.{}.part", std::process::id()));
    std::fs::write(&partial, data).and_then(|()| std::fs::rename(&partial, path)).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        format!("writing {}: {e}", path.display())
    })
}

/// Every file under `dir`.
fn files_under(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("reading {}: {e}", d.display()))? {
            let path = entry.map_err(|e| format!("reading {}: {e}", d.display()))?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_lists_what_was_written_and_nothing_else_stays() {
        let dir = std::env::temp_dir().join(format!("gencommon-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // What an earlier version wrote.
        std::fs::create_dir_all(dir.join("games/old")).unwrap();
        std::fs::write(dir.join("games/old/theme.qoa"), b"old").unwrap();
        std::fs::write(dir.join("preview.wav"), b"old").unwrap();
        assert_eq!(manifest(&dir), Ok(None));
        let mut out = Output::new(&dir).unwrap();
        out.write("samples/Music/Song.qoa", b"song").unwrap();
        out.write("sounds/startup.wav", b"sound").unwrap();
        out.write("samples/Music/Song.qoa", b"song again").unwrap();
        // Files appear whole: nothing but them beside them.
        let names: Vec<_> =
            std::fs::read_dir(dir.join("samples/Music")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["Song.qoa"]);
        assert!(out.write("../escape", b"x").is_err());
        assert!(out.write(MANIFEST, b"x").is_err());
        out.finish().unwrap();
        assert_eq!(
            manifest(&dir).unwrap().unwrap(),
            ["samples/Music/Song.qoa".to_string(), "sounds/startup.wav".into()]
        );
        assert_eq!(std::fs::read(dir.join("samples/Music/Song.qoa")).unwrap(), b"song again");
        let mut left: Vec<String> = files_under(&dir)
            .unwrap()
            .iter()
            .map(|f| f.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".manifest", "samples/Music/Song.qoa", "sounds/startup.wav"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
