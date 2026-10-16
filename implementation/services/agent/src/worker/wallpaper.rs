// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `wallpaper`: the desktop's picture, through the desktop shell.

use serde_json::{Value, json};

use super::{Ctx, desktop, show};

impl Ctx {
    /// `wallpaper`: the pictures, and setting one.
    pub(super) fn wallpaper(&self, args: &Value) -> Result<Value, String> {
        let s = desktop().ok_or("the desktop is not available")?;
        let list = s.wallpapers().map_err(|_| "the desktop did not answer")?;
        let current = s.wallpaper().unwrap_or_default();
        let name_of = |p: &str| vfiles::path::file_stem(p).to_string();
        match args["operation"].as_str().unwrap_or("list") {
            "list" => Ok(json!({
                "pictures": list.iter().map(|p| Value::from(name_of(p))).collect::<Vec<_>>(),
                "current": name_of(&current),
            })),
            op @ ("set" | "next") => {
                let path = if op == "next" {
                    let i = list.iter().position(|p| *p == current).map(|i| (i + 1) % list.len().max(1)).unwrap_or(0);
                    list.get(i).cloned().ok_or("there are no wallpapers")?
                } else {
                    let want = args["picture"].as_str().ok_or("which picture?")?;
                    let w = want.to_lowercase();
                    match list.iter().find(|p| name_of(p).to_lowercase().contains(&w)) {
                        Some(p) => p.clone(),
                        None => vagent::files::resolve(want)?,
                    }
                };
                match s.set_wallpaper(path.clone()) {
                    Ok(Ok(())) => Ok(json!({ "wallpaper": name_of(&path) })),
                    _ => Err(format!("the desktop could not use {}", show(&path))),
                }
            }
            other => Err(format!("unknown wallpaper operation {other}")),
        }
    }
}
