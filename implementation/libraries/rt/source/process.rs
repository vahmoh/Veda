// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Creating processes from ELF64 executables.
//!
//! Process creation is done entirely from user space with kernel primitives:
//! create an empty process, map the program's image into it from a VMO,
//! give it a stack, create its first thread and start it with a bootstrap
//! channel carrying the startup message (`abi::startup`).
//!
//! Every program is an ELF64 executable for the System V ABI, checked and
//! laid out by `elf`, and placed at a random address if it is
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
//!
//! A POSIX program may be linked dynamically: it names the system's dynamic
//! linker (`elf::INTERPRETER`, the C library's shared object), which is
//! loaded beside it, at a random address of its own, and started in its
//! place, the auxiliary vector saying where both are; the dynamic linker
//! links the program with its libraries and starts it. Who starts such a
//! program says how the dynamic linker's file is had
//! ([`Spawn::dynamic_linker`]); no other is ever started.

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

/// Moves `process` (`None`: this one) to a memory class
/// ([`abi::memory_class`]): what the system ends first when memory runs
/// out. Only ever towards the most expendable.
pub fn set_memory_class(process: Option<&Process>, class: usize) -> Result<(), Error> {
    let h = process.map_or(abi::INVALID_HANDLE, |p| p.raw());
    call(nr::PROCESS_SET_MEMORY_CLASS, [h as usize, class, 0, 0, 0, 0]).map(|_| ())
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
    /// A dynamically linked program that names another dynamic linker than
    /// the system's (`elf::INTERPRETER`).
    OtherDynamicLinker,
    /// A dynamically linked program whose dynamic linker could not be had
    /// ([`Spawn::dynamic_linker`]), or is not one.
    NoDynamicLinker,
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
            SpawnError::OtherDynamicLinker => {
                write!(f, "names a dynamic linker other than the system's ({})", elf::INTERPRETER)
            }
            SpawnError::NoDynamicLinker => {
                write!(f, "linked dynamically, but the dynamic linker ({}) cannot be had", elf::INTERPRETER)
            }
            SpawnError::Kernel(e) => write!(f, "{e}"),
        }
    }
}

/// Whether `image` is a program [`Spawn::start`] runs, judged by its first
/// bytes (those of an ELF file).
pub fn is_executable(image: &[u8]) -> bool {
    elf::is_elf(image)
}

/// How much of a script's first line [`interpreter`] reads, as Linux reads
/// it (`BINPRM_BUF_SIZE`): its `#!`, the interpreter, an argument and the
/// end of the line.
pub const SCRIPT_LINE_MAX: usize = 256;

/// How deep a script's interpreter may be a script itself, and its own
/// interpreter, and so on, as Linux allows (`BINPRM_MAX_RECURSION`).
pub const SCRIPT_DEPTH: usize = 4;

/// The program a script names on its first line to run it, and the one
/// argument it may give that program: what [`interpreter`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interpreter<'a> {
    pub path: &'a [u8],
    pub argument: Option<&'a [u8]>,
}

/// The interpreter of a script (`#!PATH [ARGUMENT]` on its first line), as
/// Linux reads it: blanks around the path and the argument left out, the
/// rest of the line after the path one argument whatever it holds, a
/// carriage return kept. `start` is the file's first bytes (at least
/// [`SCRIPT_LINE_MAX`] of them, or all of a shorter file). None if it is no
/// script, or its line names no interpreter or does not end within
/// [`SCRIPT_LINE_MAX`] bytes (the program that runs a script with its
/// interpreter reports that as `ENOEXEC`).
pub fn interpreter(start: &[u8]) -> Option<Interpreter<'_>> {
    let start = &start[..start.len().min(SCRIPT_LINE_MAX)];
    let rest = start.strip_prefix(b"#!")?;
    let line = match rest.iter().position(|&b| b == b'\n') {
        Some(end) => &rest[..end],
        // A file of one line, without its end.
        None if start.len() < SCRIPT_LINE_MAX => rest,
        None => return None,
    };
    let blank = |b: &u8| *b == b' ' || *b == b'\t';
    let line = &line[line.iter().position(|b| !blank(b))?..];
    let end = line.iter().position(blank).unwrap_or(line.len());
    let (path, rest) = line.split_at(end);
    let rest = match rest.iter().position(|b| !blank(b)) {
        Some(first) => &rest[first..],
        None => &[],
    };
    let last = rest.iter().rposition(|b| !blank(b));
    let argument = last.map(|last| &rest[..=last]);
    Some(Interpreter { path, argument })
}

/// Where a position-independent image of `len` bytes goes: a random page
/// of the span programs are placed in (`abi::program_base`), clear of the
/// addresses `avoid` (the program's, as its dynamic linker is placed).
fn random_base(len: usize, avoid: Option<(u64, u64)>) -> Result<u64, Error> {
    /// The places drawn at most: two images of a few hundred MiB meet in
    /// the span (a TiB) once in thousands of draws.
    const DRAWS: usize = 16;
    for _ in 0..DRAWS {
        let mut r = [0u8; 8];
        crate::object::try_random_bytes(&mut r)?;
        let base = abi::program_base(len, u64::from_le_bytes(r)) as u64;
        if avoid.is_none_or(|(start, end)| base.saturating_add(len as u64) <= start || end <= base) {
            return Ok(base);
        }
    }
    Err(Error::NoMemory)
}

/// Maps `elf`'s image into `process`, at its address or, if it is
/// position-independent, at a random one clear of the addresses `avoid`;
/// relocated for it if `relocate` (POSIX programs relocate themselves, as
/// the dynamic linker does). The pages nothing writes are the pages of
/// `file`, if it is given (the VMO `elf` was parsed from, from offset 0),
/// where it has them ([`Elf::file_pages`]): every process running the
/// program shares them. The others are memory of the process's own, a copy
/// of what the segments put there. Returns the load bias, which moves an
/// address of the file to where it is (downwards too: the arithmetic
/// wraps).
fn load_image(
    process: &Process,
    elf: &Elf,
    relocate: bool,
    file: Option<&Vmo>,
    avoid: Option<(u64, u64)>,
) -> Result<u64, SpawnError> {
    let (lo, hi) = elf.span();
    let len = (hi - lo) as usize;
    let base = if elf.is_position_independent() { random_base(len, avoid)? } else { lo };
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

/// How the system's dynamic linker is had ([`Spawn::dynamic_linker`]):
/// given its path (`elf::INTERPRETER`), its file as a VMO and the length of
/// its bytes, or `None` if it cannot be had.
pub type DynamicLinker<'a> = dyn Fn(&str) -> Option<(Vmo, usize)> + 'a;

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
    /// The highest priority its threads may run at (`abi::priority`),
    /// never more than this process's (`None`: this process's).
    pub max_priority: Option<usize>,
    /// Its memory class (`abi::memory_class`), never less expendable than
    /// this process's (`None`: this process's).
    pub memory_class: Option<usize>,
    /// The POSIX signals it inherits as ignored and as blocked, bit `n - 1`
    /// for signal `n`, which it has so until it takes signals itself
    /// (`abi::nr::PROCESS_SIGNALS_INHERITED`): for a program a POSIX one
    /// starts.
    pub signals_inherited: (u64, u64),
    /// How the system's dynamic linker is had, if the program is linked
    /// dynamically ([`DynamicLinker`]): its bytes must not change while the
    /// program runs (as the system image's files cannot). `None` refuses
    /// programs linked dynamically ([`SpawnError::NoDynamicLinker`]).
    pub dynamic_linker: Option<&'a DynamicLinker<'a>>,
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
            max_priority: None,
            memory_class: None,
            signals_inherited: (0, 0),
            dynamic_linker: None,
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

    /// The highest priority the process's threads may run at.
    pub fn max_priority(mut self, priority: usize) -> Self {
        self.max_priority = Some(priority);
        self
    }

    /// What the system ends first when memory runs out
    /// ([`abi::memory_class`]).
    pub fn memory_class(mut self, class: usize) -> Self {
        self.memory_class = Some(class);
        self
    }

    /// How the system's dynamic linker is had, for a program linked
    /// dynamically ([`Spawn::dynamic_linker`](Self#structfield.dynamic_linker)).
    pub fn dynamic_linker(mut self, get: &'a DynamicLinker<'a>) -> Self {
        self.dynamic_linker = Some(get);
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
        let max_priority = self.max_priority.unwrap_or(0);
        for _ in 0..TRIES {
            let h = call(nr::PROCESS_CREATE, [self.name.as_ptr() as usize, self.name.len(), max_priority, 0, 0, 0])?;
            // SAFETY: fresh handle from the kernel.
            let process = Process(unsafe { Handle::from_raw(h as u32) });
            if let Some(accept) = self.accept
                && !accept(process.0.koid())
            {
                // Never started: it goes with its handle.
                continue;
            }
            if let Some(class) = self.memory_class {
                set_memory_class(Some(&process), class)?;
            }
            if self.signals_inherited != (0, 0) {
                let (ignored, blocked) = self.signals_inherited;
                call(
                    nr::PROCESS_SIGNALS_INHERITED,
                    [process.raw() as usize, ignored as usize, blocked as usize, 0, 0, 0],
                )?;
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
    /// and environment in its startup message. It is static: only POSIX
    /// programs are linked dynamically.
    fn start_veda(self, elf: &Elf, file: Option<&Vmo>) -> Result<Process, SpawnError> {
        if elf.interpreter().is_some() {
            return Err(SpawnError::BadElf(elf::ElfError::Dynamic));
        }
        let (process, thread) = self.create_process()?;
        let bias = load_image(&process, elf, true, file, None)?;

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

    /// The system's dynamic linker, for a program that names it
    /// (`elf::INTERPRETER`), as [`Spawn::dynamic_linker`] has it: its file
    /// and the length of its bytes. `None` for a program linked statically.
    fn dynamic_linker_of(&self, elf: &Elf) -> Result<Option<(Vmo, usize)>, SpawnError> {
        match elf.interpreter() {
            None => Ok(None),
            Some(path) if path != elf::INTERPRETER.as_bytes() => Err(SpawnError::OtherDynamicLinker),
            Some(_) => {
                let get = self.dynamic_linker.ok_or(SpawnError::NoDynamicLinker)?;
                get(elf::INTERPRETER).map(Some).ok_or(SpawnError::NoDynamicLinker)
            }
        }
    }

    /// Starts a POSIX program: its arguments, environment and auxiliary
    /// vector on its stack, and what its POSIX layer inherits in its
    /// startup message. A dynamically linked one starts at its dynamic
    /// linker's entry, which links it.
    fn start_posix(mut self, elf: &Elf, file: Option<&Vmo>) -> Result<Process, SpawnError> {
        let linker_file = self.dynamic_linker_of(elf)?;
        let (process, thread) = self.create_process()?;
        let bias = load_image(&process, elf, false, file, None)?;
        // Where the main thread starts, and where the dynamic linker is
        // (`AT_BASE`): the program itself, and none, unless it has one.
        let (mut start, mut linker_bias) = (elf.entry().wrapping_add(bias), 0);
        if let Some((linker_file, len)) = &linker_file {
            let map = vm::Mapping::new(Vmo(linker_file.0.duplicate(None)?), (*len).max(1), map_flags::READ)?;
            // SAFETY: a read-only mapping of the dynamic linker's file, `len`
            // bytes, which lives until it is loaded; nothing changes the file.
            let image = unsafe { &map.as_slice()[..*len] };
            let linker = Elf::parse(image).map_err(|_| SpawnError::NoDynamicLinker)?;
            // A static position-independent POSIX program, which relocates
            // itself as it starts, then the program.
            if linker.interpreter().is_some() || linker.kind() != Kind::Posix || !linker.is_position_independent() {
                return Err(SpawnError::NoDynamicLinker);
            }
            let (lo, hi) = elf.span();
            let program = (lo.wrapping_add(bias), hi.wrapping_add(bias));
            linker_bias = load_image(&process, &linker, false, Some(linker_file), Some(program))?;
            start = linker.entry().wrapping_add(linker_bias);
        }

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
            base: linker_bias,
            hwcap: crate::architecture::hwcap(),
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

        // Where its main thread's stack is, which only the loader knows
        // (its top and size, without the guard pages).
        let stack_entry = alloc::format!("{}={top:x}:{size:x}", startup::MAIN_STACK);
        let mut inherited = core::mem::take(&mut self.inherited);
        inherited.push(&stack_entry);
        self.launch(process, thread, start as usize, sp as usize, &[], &inherited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
        interpreter(text).map(|i| (i.path, i.argument))
    }

    #[test]
    fn scripts_name_their_interpreter_as_linux_reads_them() {
        assert_eq!(read(b"#!/system/bin/python3\nprint(1)\n"), Some((&b"/system/bin/python3"[..], None)));
        // Blanks around the path and the argument go; the rest is one
        // argument, its blanks inside kept.
        assert_eq!(
            read(b"#! \t/system/bin/python3 -I -u \t\nx"),
            Some((&b"/system/bin/python3"[..], Some(&b"-I -u"[..])))
        );
        // One line without its end; a carriage return is kept, as Linux
        // keeps it.
        assert_eq!(read(b"#!/system/bin/tool"), Some((&b"/system/bin/tool"[..], None)));
        assert_eq!(read(b"#!/system/bin/tool\r\n"), Some((&b"/system/bin/tool\r"[..], None)));
        // Not scripts, or no interpreter.
        for text in [&b"\x7fELF"[..], b"# comment\n", b"#!\n", b"#!   \n", b""] {
            assert_eq!(read(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_first_line_ends_within_its_limit() {
        let mut long = b"#!/".to_vec();
        long.resize(SCRIPT_LINE_MAX, b'a');
        long.extend_from_slice(b"\nprint(1)\n");
        assert_eq!(read(&long), None);
        let mut fits = b"#!/".to_vec();
        fits.resize(SCRIPT_LINE_MAX - 1, b'a');
        fits.push(b'\n');
        assert_eq!(read(&fits).map(|(path, _)| path.len()), Some(SCRIPT_LINE_MAX - 3));
    }
}
