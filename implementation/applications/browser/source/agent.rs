// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The browser for the voice agent: what it shows (its tabs, the page's
//! address and title, downloads) and what it does (going to addresses,
//! searching, the history's steps, tabs, finding, scrolling, zooming,
//! bookmarking, and reading the page's text), through the same commands
//! as the toolbar and the keyboard, so that the window shows what the
//! agent did as it does it.

use ui::agent::{self, Action, AppAgentInfo, Risk, Value, arg_bool, arg_opt_int, arg_opt_str, arg_str, json};

use crate::browser::{Browser, Focus, Script};

/// The most characters of a page's text the agent reads.
const MAX_TEXT: usize = 20_000;
/// How long the agent waits for a page to answer (ns).
const SCRIPT_TIMEOUT_NS: u64 = 3_000_000_000;
/// How long it waits for the shown page to load before reading it (ns).
const LOAD_TIMEOUT_NS: u64 = 15_000_000_000;

pub fn info() -> AppAgentInfo {
    agent::info(
        "Browser: shows web pages in tabs. It goes to addresses, searches the web, goes back and forward, opens, \
         switches and closes tabs, finds text in the page, scrolls, zooms, bookmarks, and reads the page's text.",
        vec![
            Action::new("open", "Goes to a web address, or searches the web for words")
                .param("address", "string", "An address (example.com) or words to search for", true)
                .param("new_tab", "boolean", "In a new tab (the shown one by default)", false)
                .build(),
            Action::new("search", "Searches the web for words, with the chosen search engine")
                .param("words", "string", "What to search for", true)
                .build(),
            Action::new("back", "Goes back a page").build(),
            Action::new("forward", "Goes forward a page").build(),
            Action::new("reload", "Loads the page again").build(),
            Action::new("stop", "Stops loading the page").build(),
            Action::new("new_tab", "Opens a new tab")
                .param("private", "boolean", "A private one, which keeps no history or cookies", false)
                .build(),
            Action::new("switch_tab", "Shows a tab")
                .param("index", "integer", "Its number, from 1 at the left", true)
                .build(),
            Action::new("close_tab", "Closes a tab (Ctrl+Shift+T opens it again)")
                .param("index", "integer", "Its number, from 1 (the shown one by default)", false)
                .build(),
            Action::new("read_page", "Reads the shown page's text (its title, address and words), once it has loaded")
                .build(),
            Action::new("find", "Finds text in the page and marks it, and says how many times it is there")
                .param("text", "string", "What to find (empty: stop finding)", true)
                .build(),
            Action::new("scroll", "Scrolls the page")
                .choice("direction", "Which way", true, &["up", "down", "top", "bottom"])
                .build(),
            Action::new("zoom", "Zooms the page")
                .param("percent", "integer", "From 30 to 300 (100 is its size)", true)
                .build(),
            Action::new("bookmark", "Bookmarks the shown page, or removes its bookmark").build(),
            Action::new("clear_history", "Forgets every page visited").risk(Risk::Destructive).build(),
        ],
    )
}

pub fn state(b: &Browser) -> Value {
    let tabs: Vec<Value> = b
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({
                "index": i + 1,
                "title": t.name(),
                "address": if t.is_new() { String::new() } else { t.url.clone() },
                "loading": t.loading,
                "private": t.private,
                "shown": i == b.tabs.shown(),
            })
        })
        .collect();
    let downloads: Vec<Value> = b
        .downloads
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "done": d.end.is_some(),
                "fraction": d.fraction,
            })
        })
        .collect();
    json!({ "tabs": tabs, "downloads": downloads })
}

/// Tab `index` (from 1), or the shown one.
fn tab_index(b: &Browser, index: Option<i64>) -> Result<usize, String> {
    match index {
        None => Ok(b.tabs.shown()),
        Some(n) if n >= 1 && (n as usize) <= b.tabs.len() => Ok(n as usize - 1),
        Some(n) => Err(format!("there is no tab {n}: there are {}", b.tabs.len())),
    }
}

/// Runs `script` in the shown page and waits for its value (as JSON).
fn script(b: &mut Browser, script: &str) -> Result<String, String> {
    let n = b.run_script(script).ok_or("the shown tab has no page")?;
    let deadline = runtime::time::now_ns() + SCRIPT_TIMEOUT_NS;
    loop {
        b.engine_events();
        if let Some(Script::Done(result)) = b.scripts.get(&n) {
            let result = result.clone();
            b.scripts.remove(&n);
            return result;
        }
        if !wait_engine(b, deadline) {
            b.scripts.remove(&n);
            return Err("the page did not answer".into());
        }
    }
}

/// Waits until the shown page has loaded, or for `timeout_ns`.
fn wait_loaded(b: &mut Browser, timeout_ns: u64) {
    let deadline = runtime::time::now_ns() + timeout_ns;
    b.engine_events();
    while b.current().is_some_and(|t| t.loading) && wait_engine(b, deadline) {
        b.engine_events();
    }
}

/// Waits a little for the engine's events (until `deadline` at the
/// latest); false once the deadline has passed.
fn wait_engine(b: &Browser, deadline: u64) -> bool {
    let now = runtime::time::now_ns();
    if now >= deadline {
        return false;
    }
    let mut item =
        [abi::WaitItem { handle: b.engine.wake_handle(), signals: abi::signals::SIGNALED, ..Default::default() }];
    let _ = runtime::object::wait_many(&mut item, deadline.min(now + 100_000_000));
    true
}

pub fn invoke(b: &mut Browser, action: &str, args: &Value) -> Result<Value, String> {
    match action {
        "open" => {
            let address = arg_str(args, "address")?;
            if arg_bool(args, "new_tab").unwrap_or(false) {
                b.open(Some(address), false, false, true);
            } else {
                b.navigate(address);
            }
            b.focus = Focus::Page;
            Ok(json!({ "going_to": b.current().map(|t| t.url.clone()) }))
        }
        "search" => {
            let words = arg_str(args, "words")?;
            let target = b.store.settings.engine().results(words);
            let i = b.tabs.shown();
            b.go(i, &target);
            Ok(json!({ "going_to": target }))
        }
        "back" => {
            b.back();
            Ok(Value::Null)
        }
        "forward" => {
            b.forward();
            Ok(Value::Null)
        }
        "reload" => {
            b.reload(false);
            Ok(Value::Null)
        }
        "stop" => {
            b.stop();
            Ok(Value::Null)
        }
        "new_tab" => {
            b.new_tab(arg_bool(args, "private").unwrap_or(false));
            Ok(json!({ "tabs": b.tabs.len() }))
        }
        "switch_tab" => {
            let i = tab_index(b, arg_opt_int(args, "index")?)?;
            b.show_tab(i);
            Ok(json!({ "shown": b.current().map(|t| t.name()) }))
        }
        "close_tab" => {
            let i = tab_index(b, arg_opt_int(args, "index")?)?;
            b.close_tab(i);
            Ok(json!({ "tabs": b.tabs.len() }))
        }
        "read_page" => {
            wait_loaded(b, LOAD_TIMEOUT_NS);
            let (name, address) = b.current().map(|t| (t.name(), t.url.clone())).unwrap_or_default();
            // The page's own title, which the engine tells a moment after
            // the page has loaded.
            let page = script(
                b,
                &format!(
                    "(() => {{ const t = document.body ? document.body.innerText : ''; \
                     return {{ title: document.title, text: t.length > {MAX_TEXT} ? t.slice(0, {MAX_TEXT}) : t }}; }})()"
                ),
            )?;
            let page: Value = serde_json::from_str(&page).unwrap_or(Value::Null);
            let title = page["title"].as_str().filter(|t| !t.trim().is_empty()).map_or(name, str::to_string);
            Ok(json!({ "title": title, "address": address, "text": page["text"] }))
        }
        "find" => {
            let text = arg_opt_str(args, "text").unwrap_or("");
            if text.is_empty() {
                b.close_find();
                return Ok(Value::Null);
            }
            // How many matches, which the engine tells a moment later (in
            // every frame: a PDF's text is in its viewer's).
            if let Some(t) = b.tabs.current_mut() {
                t.found = None;
            }
            b.find(text, false);
            let deadline = runtime::time::now_ns() + SCRIPT_TIMEOUT_NS;
            b.engine_events();
            while b.current().is_some_and(|t| t.found.is_none()) && wait_engine(b, deadline) {
                b.engine_events();
            }
            Ok(json!({ "found": b.current().and_then(|t| t.found) }))
        }
        "scroll" => {
            let how = match arg_str(args, "direction")? {
                "up" => "window.scrollBy({ top: -window.innerHeight * 0.8, behavior: 'smooth' })",
                "down" => "window.scrollBy({ top: window.innerHeight * 0.8, behavior: 'smooth' })",
                "top" => "window.scrollTo({ top: 0, behavior: 'smooth' })",
                "bottom" => "window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' })",
                other => return Err(format!("cannot scroll {other}")),
            };
            script(b, &format!("{how}; true"))?;
            Ok(Value::Null)
        }
        "zoom" => {
            let percent = arg_opt_int(args, "percent")?.ok_or("percent is needed")?;
            let z = browser_core::settings::ZOOMS
                .iter()
                .copied()
                .min_by_key(|z| (i64::from(*z) - percent).abs())
                .unwrap_or(100);
            if let Some(tab) = b.tabs.current_mut() {
                tab.zoom = z;
                if let Some(v) = tab.view {
                    b.engine.zoom(v, f64::from(z) / 100.0);
                }
            }
            Ok(json!({ "zoom": z }))
        }
        "bookmark" => {
            b.toggle_bookmark();
            let kept = b.current().is_some_and(|t| b.store.bookmarks.contains(&t.url));
            Ok(json!({ "bookmarked": kept }))
        }
        "clear_history" => {
            b.clear(true, 0);
            Ok(Value::Null)
        }
        _ => Err(format!("the browser does not {action}")),
    }
}
