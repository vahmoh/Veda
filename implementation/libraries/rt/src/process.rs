// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Creating processes from ELF64 executables.
//!
//! Process creation is done entirely from user space with kernel primitives:
//! create an empty process, map the program's image into it from a VMO,
//! give it a stack, create its first thread and start it with a bootstrap
//! channel carrying the startup message (`abi::startup`).
//!
//! Every program is a static ELF64 executable for the System V ABI, checked
//! and laid out by `elf`, and placed at a random address if it is
//! position-independent. Its note (`elf::veda`) tells which of two kinds
//! it is:
//!
//! * **Veda programs** (Rust, on this runtime): position-independent and
//!   relocated here, so that their RELRO is read-only from the start. Their
//!   arguments and environment travel in the startup message.
//! * **POSIX programs** (C, built by Veda's GCC; Rust on its standard
//!   library), which relocate themselves if they are position-independent.
//!   Their arguments, environment and auxiliary vector are laid out on
//!   their stack as on Linux (`elf::initial_stack`); the startup message
//!   carries the working directory and the handles (registry, file
//!   descriptors, terminal), and, where a Veda program's environment goes,
//!   what their POSIX layer inherits from the one that starts them
//!   ([`Spawn::inherited`]) and where their main thread's stack is
//!   ([`startup::MAIN_STACK`]).

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use abi::startup;
use abi::{Error, map_flags, nr};
use elf::{Elf, Kind, PageRun, Relro};

use crate::object::{Channel, Handle, Process, Thread, Vmo};
use crate::sys::call;
use crate::vm;

/// Stack size of a Veda program's main thread.
pub const MAIN_STACK: usize = 1 << 20;
/// Stack size of a POSIX program's main thread (as Linux's default limit),
/// unless the program asks for more.
pub const POSIX_STACK: usize = 8 << 20;
/// The largest main-thread stack a POSIX program may ask for.
pub const POSIX_STACK_MAX: usize = 512 << 20;
/// Inaccessible pages below a program's stack, and below each of its
/// threads', so that an overflow faults instead of running into other
/// memory.
pub const STACK_GUARD: usize = 64 << 10;

/// Takes abilities from this process, for good ([`abi::restrict`]).
pub fn restrict(flags: usize) -> Result<(), Error> {
    call(nr::PROCESS_RESTRICT, [flags, 0, 0, 0, 0, 0]).map(|_| ())
}

/// The user and group POSIX programs run as.
pub const UID: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    /// Not an executable (an ELF file).
    NotExecutable,
    BadElf(elf::ElfError),
    /// A Veda program of another version of Veda's conventions.
    Incompatible,
    /// The arguments and environment do not fit (in the startup message of
    /// a Veda program, on the stack of a POSIX one).
    TooLarge,
    /// No process the caller accepts ([`Spawn::accept`]) could be made.
    Refused,
    Kernel(Error),
}

impl From<Error> for SpawnError {
    fn from(e: Error) -> Self {
        SpawnError::Kernel(e)
    }
}

impl core::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpawnError::NotExecutable => f.write_str("not an executable"),
            SpawnError::BadElf(e) => write!(f, "invalid executable: {e}"),
            SpawnError::Incompatible => f.write_str("built for another version of Veda"),
            SpawnError::TooLarge => f.write_str("the argument list is too long"),
            SpawnError::Refused => f.write_str("no process the caller accepts could be made"),
            SpawnError::Kernel(e) => write!(f, "{e}"),
        }
    }
}

/// Whether `image` is a program [`Spawn::start`] runs, judged by its first
/// bytes (those of an ELF file).
pub fn is_executable(image: &[u8]) -> bool {
    elf::is_elf(image)
}

/// Where a position-independent image of `len` bytes goes: a random page
/// of the span programs are placed in (`abi::program_base`).
fn random_base(len: usize) -> Result<u64, Error> {
    let mut r = [0u8; 8];
    crate::object::try_random_bytes(&mut r)?;
    Ok(abi::program_base(len, u64::from_le_bytes(r)) as u64)
}

/// Maps `elf`'s image into `process`, at its address or, if it is
/// position-independent, at a random one; relocated for it if `relocate`
/// (POSIX programs relocate themselves). The pages nothing writes are the
/// pages of `file`, if it is given (the VMO `elf` was parsed from, from
/// offset 0), where it has them ([`Elf::file_pages`]): every process
/// running the program shares them. The others are memory of the
/// process's own, a copy of what the segments put there. Returns the load
/// bias, which moves an address of the file to where it is (downwards too:
/// the arithmetic wraps).
fn load_image(process: &Process, elf: &Elf, relocate: bool, file: Option<&Vmo>) -> Result<u64, SpawnError> {
    let (lo, hi) = elf.span();
    let len = (hi - lo) as usize;
    let base = if elf.is_position_independent() { random_base(len)? } else { lo };
    let bias = base.wrapping_sub(lo);
    let end = base.checked_add(hi - lo);
    if base < abi::USER_SPACE_START as u64 || end.is_none_or(|e| e > abi::USER_SPACE_END as u64) {
        return Err(SpawnError::BadElf(elf::ElfError::BadSegment));
    }

    let relro = if relocate { Relro::ReadOnly } else { Relro::Writable };
    // Each run of pages, and where the file has its pages if they can be
    // the file's own.
    let runs: Vec<(PageRun, Option<u64>)> =
        elf::page_runs(elf, relro).map(|run| (run, file.and_then(|_| elf.file_pages(&run)))).collect();
    // One VMO for the rest, holding the segments those pages show at their
    // offsets in the span (zero elsewhere, which is the bss).
    let vmo = Vmo::create(len)?;
    let page = abi::PAGE_SIZE as u64;
    for s in elf.segments() {
        let (first, end) = (s.vaddr / page * page, s.end().checked_next_multiple_of(page).unwrap_or(u64::MAX));
        if runs.iter().any(|(run, in_file)| in_file.is_none() && run.start < end && first < run.start + run.len) {
            vmo.write((s.vaddr - lo) as usize, s.data)?;
        }
    }
    if relocate {
        // Through a mapping of the VMO here, which faults pages in only
        // where relocations are (in the writable segments).
        let image = vm::Mapping::new(Vmo(vmo.0.duplicate(None)?), len, map_flags::READ | map_flags::WRITE)?;
        // SAFETY: a mapping of a VMO made above, which nothing else maps yet.
        elf.relocate(unsafe { image.as_mut_slice() }, bias).map_err(SpawnError::BadElf)?;
    }
    for (run, in_file) in &runs {
        let mut flags = map_flags::FIXED;
        for (on, flag) in [(run.perms.read, map_flags::READ), (run.perms.write, map_flags::WRITE)] {
            if on {
                flags |= flag;
            }
        }
        if run.perms.exec {
            flags |= map_flags::EXECUTE | map_flags::READ;
        }
        let at = run.start.wrapping_add(bias) as usize;
        match (file, in_file) {
            (Some(file), Some(offset)) => vm::map(Some(process), file, *offset as usize, run.len as usize, at, flags)?,
            _ => vm::map(Some(process), &vmo, (run.start - lo) as usize, run.len as usize, at, flags)?,
        };
    }
    Ok(bias)
}

/// What to give a new process.
pub struct Spawn<'a> {
    /// Process name (shown by the task manager).
    pub name: &'a str,
    /// Arguments; the first is the program name.
    pub args: Vec<&'a [u8]>,
    /// `KEY=VALUE` strings.
    pub env: Vec<&'a [u8]>,
    /// Absolute working directory (empty: `/`).
    pub cwd: &'a str,
    /// The path the program was loaded from (`AT_EXECFN`), if known.
    pub path: &'a str,
    /// Handles to pass, tagged with roles (`abi::startup::role`).
    pub handles: Vec<(u32, Handle)>,
    /// For a POSIX program: what its POSIX layer inherits from the one
    /// starting it, `KEY=VALUE` strings the startup message carries in place
    /// of the environment (which is on the program's stack). Left out for a
    /// Veda program.
    pub inherited: Vec<&'a str>,
    /// Whether the process of this kernel object id may be the one started
    /// (`None`: any). One refused is dropped before it runs and another
    /// made in its place, a few times at most: for ids the caller derives
    /// from kernel object ids, which can repeat (POSIX process ids, cut
    /// to 30 bits).
    pub accept: Option<&'a dyn Fn(u64) -> bool>,
}

impl<'a> Spawn<'a> {
    pub fn new(name: &'a str) -> Spawn<'a> {
        Spawn {
            name,
            args: alloc::vec![name.as_bytes()],
            env: Vec::new(),
            cwd: "",
            path: "",
            handles: Vec::new(),
            inherited: Vec::new(),
            accept: None,
        }
    }

    pub fn arg(self, a: &'a str) -> Self {
        self.arg_bytes(a.as_bytes())
    }

    /// An argument that need not be UTF-8 (POSIX programs take any bytes but
    /// NUL; Veda programs get invalid UTF-8 replaced).
    pub fn arg_bytes(mut self, a: &'a [u8]) -> Self {
        self.args.push(a);
        self
    }

    pub fn env(self, kv: &'a str) -> Self {
        self.env_bytes(kv.as_bytes())
    }

    pub fn env_bytes(mut self, kv: &'a [u8]) -> Self {
        self.env.push(kv);
        self
    }

    pub fn cwd(mut self, dir: &'a str) -> Self {
        self.cwd = dir;
        self
    }

    pub fn path(mut self, path: &'a str) -> Self {
        self.path = path;
        self
    }

    pub fn handle(mut self, role: u32, h: Handle) -> Self {
        self.handles.push((role, h));
        self
    }

    /// Loads `image` (an ELF64 executable: a Veda program or a POSIX one)
    /// into a new process and starts it.
    pub fn start(self, image: &[u8]) -> Result<Process, SpawnError> {
        self.start_image(image, None)
    }

    /// Loads the executable that is the first `len` bytes of `file` into a
    /// new process and starts it, as [`start`](Self::start) does, with the
    /// program's pages that nothing writes being the file's own where it
    /// has them a whole number of pages from where they go (as a file of
    /// the system image does): every process running the program shares
    /// them, and loading copies only what is written. The file must not
    /// change while the program runs, which the caller vouches for (the
    /// system image cannot).
    pub fn start_file(self, file: &Vmo, len: usize) -> Result<Process, SpawnError> {
        let map = vm::Mapping::new(Vmo(file.0.duplicate(None)?), len.max(1), map_flags::READ)?;
        // SAFETY: a read-only mapping of the file, `len` bytes, which lives
        // until the program is loaded; nothing changes the file meanwhile.
        let image = unsafe { &map.as_slice()[..len] };
        self.start_image(image, Some(file))
    }

    /// Starts the executable `image`, which is the bytes of `file` if it is
    /// given.
    fn start_image(self, image: &[u8], file: Option<&Vmo>) -> Result<Process, SpawnError> {
        if !is_executable(image) {
            return Err(SpawnError::NotExecutable);
        }
        let elf = Elf::parse(image).map_err(SpawnError::BadElf)?;
        match elf.kind() {
            Kind::Veda => self.start_veda(&elf, file),
            Kind::OtherVeda => Err(SpawnError::Incompatible),
            Kind::Posix => self.start_posix(&elf, file),
        }
    }

    fn create_process(&self) -> Result<(Process, Thread), SpawnError> {
        /// How many processes are made, at most, for one the caller accepts.
        const TRIES: usize = 4;
        for _ in 0..TRIES {
            let h = call(nr::PROCESS_CREATE, [self.name.as_ptr() as usize, self.name.len(), 0, 0, 0, 0])?;
            // SAFETY: fresh handle from the kernel.
            let process = Process(unsafe { Handle::from_raw(h as u32) });
            if let Some(accept) = self.accept
                && !accept(process.0.koid())
            {
                // Never started: it goes with its handle.
                continue;
            }
            let t = call(nr::THREAD_CREATE, [process.raw() as usize, "main".as_ptr() as usize, 4, 0, 0, 0])?;
            // SAFETY: fresh handle from the kernel.
            let thread = Thread(unsafe { Handle::from_raw(t as u32) });
            return Ok((process, thread));
        }
        Err(SpawnError::Refused)
    }

    /// Sends the startup message and starts the main thread at `entry`, its
    /// stack pointer `sp`.
    fn launch(
        self,
        process: Process,
        thread: Thread,
        entry: usize,
        sp: usize,
        args: &[&str],
        env: &[&str],
    ) -> Result<Process, SpawnError> {
        let roles: Vec<u32> = self.handles.iter().map(|(r, _)| *r).collect();
        let msg = startup::Startup { args, env, cwd: self.cwd, roles: &roles };
        if msg.encoded_len() > abi::CHANNEL_MAX_BYTES || roles.len() > abi::CHANNEL_MAX_HANDLES {
            return Err(SpawnError::TooLarge);
        }
        let mut bytes = Vec::with_capacity(msg.encoded_len());
        msg.encode(&mut |b| bytes.extend_from_slice(b));
        let (ours, theirs) = Channel::create()?;
        ours.write(&bytes, self.handles.into_iter().map(|(_, h)| h).collect())?;
        drop(ours);
        call(
            nr::PROCESS_START,
            [process.raw() as usize, thread.raw() as usize, entry, sp, theirs.into_handle().into_raw() as usize, 0],
        )?;
        Ok(process)
    }

    /// Starts a Veda program: relocated where it is placed, its arguments
    /// and environment in its startup message.
    fn start_veda(self, elf: &Elf, file: Option<&Vmo>) -> Result<Process, SpawnError> {
        let (process, thread) = self.create_process()?;
        let bias = load_image(&process, elf, true, file)?;

        // Stack: guard pages, then the stack proper.
        let stack = Vmo::create(STACK_GUARD + MAIN_STACK)?;
        let base = vm::map(Some(&process), &stack, 0, STACK_GUARD + MAIN_STACK, 0, map_flags::READ | map_flags::WRITE)?;
        vm::protect(Some(&process), base, STACK_GUARD, 0)?;
        let sp = abi::entry_stack_pointer(base + STACK_GUARD + MAIN_STACK);

        // The startup message carries text.
        fn text<'b>(v: &[&'b [u8]]) -> Vec<Cow<'b, str>> {
            v.iter().map(|b| String::from_utf8_lossy(b)).collect()
        }
        let (args, env) = (text(&self.args), text(&self.env));
        let args: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
        let env: Vec<&str> = env.iter().map(|s| s.as_ref()).collect();
        let entry = elf.entry().wrapping_add(bias) as usize;
        self.launch(process, thread, entry, sp, &args, &env)
    }

    /// Starts a POSIX program: its arguments, environment and auxiliary
    /// vector on its stack, and what its POSIX layer inherits in its
    /// startup message.
    fn start_posix(mut self, elf: &Elf, file: Option<&Vmo>) -> Result<Process, SpawnError> {
        let (process, thread) = self.create_process()?;
        let bias = load_image(&process, elf, false, file)?;

        // Stack: guard pages, then the stack proper, its top holding the
        // arguments, environment and auxiliary vector.
        let size = elf.stack_size().map_or(POSIX_STACK, |s| (s as usize).clamp(POSIX_STACK, POSIX_STACK_MAX));
        let size = size.next_multiple_of(abi::PAGE_SIZE);
        let stack = Vmo::create(STACK_GUARD + size)?;
        let base = vm::map(Some(&process), &stack, 0, STACK_GUARD + size, 0, map_flags::READ | map_flags::WRITE)?;
        vm::protect(Some(&process), base, STACK_GUARD, 0)?;
        let top = (base + STACK_GUARD + size) as u64;
        let mut random = [0u8; 16];
        crate::object::try_random_bytes(&mut random)?;
        let aux = elf::AuxInfo {
            phdr: elf.phdr_vaddr().wrapping_add(bias),
            phnum: elf.phnum() as u64,
            entry: elf.entry().wrapping_add(bias),
            hwcap: crate::arch::hwcap(),
            platform: elf::NATIVE.platform,
            uid: UID,
            gid: UID,
            random,
            execfn: self.path.as_bytes(),
        };
        // Like Linux, a quarter of the stack at most for the arguments.
        let len = elf::initial_stack_size(&self.args, &self.env, &aux);
        if len > size / 4 {
            return Err(SpawnError::TooLarge);
        }
        let mut img = alloc::vec![0u8; len];
        let sp = elf::initial_stack(top, &self.args, &self.env, &aux, &mut img).map_err(SpawnError::BadElf)?;
        stack.write(STACK_GUARD + size - len, &img)?;

        let entry = elf.entry().wrapping_add(bias) as usize;
        // Where its main thread's stack is, which only the loader knows
        // (its top and size, without the guard pages).
        let stack_entry = alloc::format!("{}={top:x}:{size:x}", startup::MAIN_STACK);
        let mut inherited = core::mem::take(&mut self.inherited);
        inherited.push(&stack_entry);
        self.launch(process, thread, entry, sp as usize, &[], &inherited)
    }
}
