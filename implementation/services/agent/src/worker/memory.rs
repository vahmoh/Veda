// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! `memory`: what the agent remembers about the user. A request to forget
//! arrives naming its facts by id (made definite before it was classified:
//! `vagent::memory::Memory::pin_forget`), and removes exactly those.

use serde_json::{Value, json};
use vagent::tools;

use super::Ctx;

impl Ctx {
    /// `memory`: remember, search, forget.
    pub(super) fn memory(&self, args: &Value) -> Result<Value, String> {
        let now = vrt::time::unix_time_ns() / 1_000_000_000;
        let mut sh = self.shared.lock();
        let r = match args["operation"].as_str().unwrap_or("search") {
            "remember" => {
                let text = args["text"].as_str().filter(|t| !t.trim().is_empty()).ok_or("what should I remember?")?;
                let id = sh.memory.remember(text, args["kind"].as_str().unwrap_or("fact"), now);
                Ok(json!({ "remembered": id }))
            }
            "search" => Ok(Value::Array(
                sh.memory
                    .search(args["text"].as_str().unwrap_or(""), 15)
                    .iter()
                    .map(|f| json!({ "id": f.id, "text": f.text.as_str() }))
                    .collect(),
            )),
            "forget" => {
                let gone = sh.memory.forget_ids(&tools::forget_ids(args));
                if gone.is_empty() {
                    Err("nothing with that id".into())
                } else {
                    Ok(json!({ "forgot": gone.iter().map(|f| Value::from(f.text.as_str())).collect::<Vec<_>>() }))
                }
            }
            "forget_all" => {
                sh.memory.forget_all();
                Ok(json!({ "forgot": "everything" }))
            }
            other => Err(format!("unknown memory operation {other}")),
        };
        sh.save_memory();
        r
    }
}
