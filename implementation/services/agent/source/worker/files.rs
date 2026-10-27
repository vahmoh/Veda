// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `files`: the user's files and the Trash.
//!
//! The paths come from the language model, and the places items are
//! restored to from the Trash's records, which any program can write: each
//! one an action acts on — its source, its destination, where a restored
//! item goes — is resolved by [`agent::files`] first, so that nothing
//! reaches a private directory, and what is created goes in the home folder
//! or `/tmp`. Copies, moves and renames never replace what is there: the
//! destination is worked out as it is used, and one that exists is refused
//! before anything is done.

use agent::files::{self as paths, HOME};
use files::path::{file_name, parent};
use files::trash;
use serde_json::{Value, json};

use super::{Ctx, show};

/// Largest text the agent reads from a file at once.
const MAX_READ: usize = 24 * 1024;

impl Ctx {
    /// `files` (see the module documentation).
    pub(super) fn files(&mut self, args: &Value) -> Result<Value, String> {
        let op = args["operation"].as_str().unwrap_or("");
        let path = paths::resolve(args["path"].as_str().unwrap_or("~"))?;
        let err = |e: files::Error| e.to_string();
        match op {
            "list" => {
                let entries = self.fs.read_dir(&path).map_err(err)?;
                let list: Vec<Value> = entries
                    .iter()
                    .filter(|e| !e.name.starts_with('.'))
                    .take(150)
                    .map(|e| json!({ "name": e.name.as_str(), "folder": e.is_dir, "size": files::format::human_size(e.size) }))
                    .collect();
                Ok(json!({ "folder": show(&path), "entries": list, "total": entries.len() }))
            }
            "find" => {
                let what =
                    args["text"].as_str().or(args["to"].as_str()).ok_or("what should I look for?")?.to_lowercase();
                let mut found = Vec::new();
                self.find(&path, &what, 0, &mut found);
                Ok(json!({ "matches": found.into_iter().map(Value::from).collect::<Vec<_>>() }))
            }
            "info" => {
                let st = self.fs.stat(&path).map_err(err)?;
                Ok(json!({
                    "path": show(&path),
                    "folder": st.is_dir,
                    "size": files::format::human_size(st.size),
                    "modified": files::format::format_time(st.modified),
                    "kind": files::kind::kind_name(file_name(&path), st.is_dir, st.executable),
                }))
            }
            "read" => {
                let bytes = self.fs.read(&path).map_err(err)?;
                match core::str::from_utf8(&bytes) {
                    Ok(text) => {
                        let mut end = text.len().min(MAX_READ);
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        Ok(json!({ "path": show(&path), "text": &text[..end], "truncated": end < text.len() }))
                    }
                    Err(_) => Ok(json!({
                        "path": show(&path),
                        "kind": files::kind::kind_name(
                            file_name(&path),
                            false,
                            self.fs.stat(&path).is_ok_and(|st| st.executable),
                        ),
                        "size": files::format::human_size(bytes.len() as u64),
                        "note": "not a text file; open it in its application instead",
                    })),
                }
            }
            "write" | "append" => {
                let path = paths::writable(&path)?;
                let text = args["text"].as_str().unwrap_or("");
                let _ = self.fs.mkdir_all(parent(&path));
                if op == "write" {
                    self.fs.write(&path, text.as_bytes()).map_err(err)?;
                } else {
                    self.fs.append(&path, text.as_bytes()).map_err(err)?;
                }
                Ok(json!({ "saved": show(&path) }))
            }
            "create_folder" => {
                let path = paths::writable(&path)?;
                self.fs.mkdir_all(&path).map_err(err)?;
                Ok(json!({ "created": show(&path) }))
            }
            "copy" | "move" | "rename" => {
                let to = args["to"].as_str().ok_or("where to?")?;
                let fs = &self.fs;
                let dest = paths::destination(op, &path, to, &|p| fs.is_dir(p))?;
                if self.fs.exists(&dest) {
                    return Err(format!("{} already exists; nothing is replaced", show(&dest)));
                }
                if op == "copy" {
                    self.fs.copy(&path, &dest).map_err(err)?;
                } else {
                    self.fs.move_to(&path, &dest).map_err(err)?;
                }
                Ok(json!({ "done": op, "from": show(&path), "to": show(&dest) }))
            }
            "delete" => {
                if path == HOME || !path.starts_with("/home/user/") {
                    return Err("only files and folders inside the home folder can be deleted".into());
                }
                if trash::contains(&path) {
                    return Err(
                        "that is in the Trash already; empty_trash deletes what is in the Trash for good".into()
                    );
                }
                trash::put(&self.fs, &path).map_err(err)?;
                Ok(json!({
                    "moved_to_trash": show(&path),
                    "note": "restore puts it back until the Trash is emptied",
                }))
            }
            "list_trash" => {
                let items = trash::items(&self.fs).map_err(err)?;
                let list: Vec<Value> = items
                    .iter()
                    .take(150)
                    .map(|i| {
                        let from = i.info.as_ref().map(|info| show(parent(&info.origin)));
                        json!({
                            "name": i.original_name(),
                            "folder": i.is_dir,
                            "deleted_from": from.unwrap_or_else(|| "not known".into()),
                            "deleted": files::format::format_time(i.deleted()),
                        })
                    })
                    .collect();
                Ok(json!({ "trash": list, "total": items.len() }))
            }
            "restore" => {
                let wanted =
                    args["path"].as_str().ok_or("which item? give where it was deleted from, or its name")?.trim();
                let name = file_name(wanted).to_lowercase();
                // By where it was deleted from, else by name: the latest
                // deleted first.
                let items = trash::items(&self.fs).map_err(err)?;
                let item = items
                    .iter()
                    .find(|i| i.info.as_ref().is_some_and(|info| info.origin == path))
                    .or_else(|| items.iter().find(|i| i.name == wanted || i.original_name().to_lowercase() == name))
                    .ok_or_else(|| format!("nothing in the Trash is called {wanted}"))?;
                // Where its record says it was, if the agent may put it
                // there: under a free name like its own, in its folder made
                // again if that is gone.
                let origin = item.info.as_ref().map_or("", |info| info.origin.as_str());
                let target = paths::restore_target(origin)?;
                let to = trash::restore_to(&self.fs, &item.name, parent(&target), file_name(&target)).map_err(err)?;
                Ok(json!({ "restored": show(&to) }))
            }
            "empty_trash" => {
                let n = trash::empty(&self.fs).map_err(err)?;
                let what = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
                Ok(json!({ "emptied": format!("{what} deleted for good") }))
            }
            other => Err(format!("unknown file operation {other}")),
        }
    }

    /// Paths under `dir` whose names contain `what` (bounded search).
    fn find(&self, dir: &str, what: &str, depth: u32, out: &mut Vec<String>) {
        if depth > 6 || out.len() >= 40 {
            return;
        }
        let Ok(entries) = self.fs.read_dir(dir) else { return };
        for e in entries {
            if e.name.starts_with('.') {
                continue;
            }
            let p = files::path::join(dir, &e.name);
            if e.name.to_lowercase().contains(what) {
                out.push(show(&p));
            }
            if e.is_dir {
                self.find(&p, what, depth + 1, out);
            }
        }
    }
}
