// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Share groups, contexts and surfaces, and the context each thread has
//! current.
//!
//! The contexts that share objects are a [`Group`]: one [`gl::Context`],
//! which holds the shared objects and the renderer, and a [`gl::Local`] for
//! each context, of which the one in use is in the `gl::Context` and the
//! others wait beside it. A call loads its context's own part first (a
//! swap, when another context of the group was in use), under the group's
//! lock: the contexts of a group take turns, whatever threads they are
//! current in.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;

/// Contexts that share their objects.
pub struct Group {
    pub gl: gl::Context,
    /// The context whose own part is in `gl`.
    loaded: u64,
    /// The others' own parts.
    parked: HashMap<u64, gl::Local>,
    /// `glGetString`'s strings, which callers keep pointers to, for as long
    /// as the group lives.
    pub strings: HashMap<u32, CString>,
    /// `glGetStringi`'s.
    pub extensions: HashMap<u32, CString>,
}

impl Group {
    /// A group of one context, `id`, as `config` describes its default
    /// framebuffer; or why it has no renderer.
    fn new(id: u64, config: gl::Config) -> Result<Group, String> {
        Ok(Group {
            gl: renderer(config)?,
            loaded: id,
            parked: HashMap::new(),
            strings: HashMap::new(),
            extensions: HashMap::new(),
        })
    }

    /// Puts context `id`'s own part in use.
    fn load(&mut self, id: u64) {
        if self.loaded == id {
            return;
        }
        let Some(mut local) = self.parked.remove(&id) else { return };
        self.gl.swap_local(&mut local);
        self.parked.insert(self.loaded, local);
        self.loaded = id;
    }

    /// Context `id` goes: its own part with it (the group, with its last
    /// context).
    fn remove(&mut self, id: u64) {
        if self.loaded != id {
            if let Some(local) = self.parked.remove(&id) {
                self.gl.destroy_local(local);
            }
            return;
        }
        // Another context's part in its place, if there is one.
        let Some(&other) = self.parked.keys().next() else { return };
        let mut local = self.parked.remove(&other).expect("a parked context");
        self.gl.swap_local(&mut local);
        self.loaded = other;
        self.gl.destroy_local(local);
    }
}

/// A share group's renderer, or why there is none (for the log). In Veda
/// it is the GPU or nothing: OpenGL ES in software would take a worker
/// thread on every processor, and the program that composites with EGL,
/// the browser, paints its pages on the CPU instead. Elsewhere (the host's
/// tests) it is the processors, threads of the standard library.
#[cfg(target_os = "veda")]
fn renderer(config: gl::Config) -> Result<gl::Context, String> {
    gl::veda::gpu_context(config).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "veda"))]
fn renderer(config: gl::Config) -> Result<gl::Context, String> {
    #[cfg(test)]
    if NO_RENDERER.get() {
        return Err(String::from("the test's renderer is away"));
    }
    Ok(gl::Context::new(Box::new(gl::soft::SoftBackend::new(Box::new(StdWorkers::default()))), config))
}

#[cfg(test)]
thread_local! {
    /// The calling thread's new share groups have no renderer, as in Veda
    /// without a GPU (for the tests).
    pub static NO_RENDERER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The software renderer's workers, as threads of the standard library.
#[cfg(not(target_os = "veda"))]
struct StdWorkers(usize);

#[cfg(not(target_os = "veda"))]
impl Default for StdWorkers {
    fn default() -> StdWorkers {
        StdWorkers(std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
    }
}

#[cfg(not(target_os = "veda"))]
impl gl::soft::Workers for StdWorkers {
    fn threads(&self) -> usize {
        self.0
    }

    fn run(&self, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|s| {
            for i in 1..self.0 {
                s.spawn(move || f(i));
            }
            f(0);
        });
    }
}

/// Locks a group (whose state no panic leaves half changed: a panic in a
/// program on Veda ends it).
pub fn lock(group: &Mutex<Group>) -> MutexGuard<'_, Group> {
    group.lock().unwrap_or_else(|e| e.into_inner())
}

/// An EGL context.
pub struct Context {
    pub id: u64,
    /// Its configuration's index (in `egl::CONFIGS`).
    pub config: usize,
    pub group: Arc<Mutex<Group>>,
    /// The thread it is current in, if any.
    pub thread: Mutex<Option<ThreadId>>,
    /// The surface it draws into, while current (none: surfaceless).
    pub surface: Mutex<Option<Arc<Surface>>>,
    /// It has been current with a surface: its viewport and scissor box
    /// were set to the surface's size (EGL 1.5, section 3.7.3).
    pub framed: AtomicBool,
}

impl Context {
    /// A context of a group of its own, or of `share`'s; or why a group of
    /// its own has no renderer.
    pub fn new(id: u64, config: usize, gl_config: gl::Config, share: Option<&Context>) -> Result<Context, String> {
        let group = match share {
            Some(s) => {
                let mut g = lock(&s.group);
                let local = g.gl.new_local(gl_config);
                g.parked.insert(id, local);
                drop(g);
                s.group.clone()
            }
            None => Arc::new(Mutex::new(Group::new(id, gl_config)?)),
        };
        Ok(Context {
            id,
            config,
            group,
            thread: Mutex::new(None),
            surface: Mutex::new(None),
            framed: AtomicBool::new(false),
        })
    }

    /// The thread it is current in.
    pub fn current_thread(&self) -> Option<ThreadId> {
        *self.thread.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        lock(&self.group).remove(self.id);
    }
}

/// An EGL pbuffer: the size of the default framebuffer of the contexts
/// made current with it (each context draws into buffers of its own).
pub struct Surface {
    pub config: usize,
    pub width: u32,
    pub height: u32,
    /// The context it is current with, if any.
    pub context: Mutex<Option<u64>>,
}

/// What a thread has current: the context, which it lets go of when the
/// thread ends.
pub struct Current {
    pub context: Arc<Context>,
}

impl Drop for Current {
    fn drop(&mut self) {
        *self.context.thread.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(s) = self.context.surface.lock().unwrap_or_else(|e| e.into_inner()).take() {
            *s.context.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

/// The calling thread's current context.
pub fn current() -> Option<Arc<Context>> {
    CURRENT.with(|c| c.borrow().as_ref().map(|c| c.context.clone()))
}

/// Makes `context` the calling thread's current context (none: lets go of
/// the one it has), with `surface`. The caller has checked that neither is
/// another thread's.
pub fn make_current(context: Option<Arc<Context>>, surface: Option<Arc<Surface>>) {
    // The one it had goes first (its marks with it).
    CURRENT.with(|c| c.borrow_mut().take());
    let Some(context) = context else { return };
    *context.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
    if let Some(s) = &surface {
        *s.context.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.id);
    }
    {
        let mut g = lock(&context.group);
        g.load(context.id);
        g.gl.set_surface(surface.as_ref().map(|s| (s.width, s.height)));
        if let Some(s) = &surface
            && !context.framed.swap(true, Ordering::Relaxed)
        {
            let (w, h) = (s.width as i32, s.height as i32);
            g.gl.viewport(0, 0, w, h);
            g.gl.scissor(0, 0, w, h);
        }
    }
    *context.surface.lock().unwrap_or_else(|e| e.into_inner()) = surface;
    CURRENT.with(|c| *c.borrow_mut() = Some(Current { context }));
}

/// Runs `f` on the current context's group, its own part in use: what `f`
/// returns, or `none` without a current context.
pub fn with_group_or<R>(none: R, f: impl FnOnce(&mut Group) -> R) -> R {
    let Some(context) = current() else { return none };
    let mut g = lock(&context.group);
    g.load(context.id);
    f(&mut g)
}

/// [`with_group_or`], `R`'s default without a current context.
pub fn with_group<R: Default>(f: impl FnOnce(&mut Group) -> R) -> R {
    with_group_or(R::default(), f)
}

/// Runs `f` on the current context: what it returns, or `none` without
/// one.
pub fn with_gl_or<R>(none: R, f: impl FnOnce(&mut gl::Context) -> R) -> R {
    with_group_or(none, |g| f(&mut g.gl))
}

/// [`with_gl_or`], `R`'s default without a current context.
pub fn with_gl<R: Default>(f: impl FnOnce(&mut gl::Context) -> R) -> R {
    with_gl_or(R::default(), f)
}
