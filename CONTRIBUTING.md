<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Contributing to Veda

Thank you for helping build Veda. This guide covers how to propose a
change, the standards every change meets, and how changes are tested and
reviewed. [Developing](documentation/DEVELOPING.md) has the commands in
full, [Coding conventions](documentation/CODING.md) the rules the code
follows, and [Architecture](documentation/ARCHITECTURE.md) the design.

**Contents:** [Before you start](#before-you-start) · [Making a change](#making-a-change) · [Engineering standards](#engineering-standards) · [Testing](#testing) · [Documentation](#documentation) · [Commits](#commits) · [Pull requests](#pull-requests) · [Reporting a vulnerability](#reporting-a-vulnerability) · [Licence](#licence)

## Before you start

- **Found a bug?** Open an issue with the steps, what happened and what
  you expected, the commit (`git rev-parse --short HEAD`) and what
  `cargo orchestration doctor` says. Attach the log:
  `target/veda/serial.log` after `script`, `shot` or `test`, the file
  `cargo orchestration run --serial FILE` writes, or `VEDA/LOGS/` on a live
  system's stick.
- **Planning more than a fix?** Open an issue first for a new service,
  protocol or application, a change to who may do what, or a new
  dependency or port. Agreeing on the approach saves rework.
- **Have a question?** Open an issue.
- **Found a vulnerability?** Report it privately, never in an issue; see
  [Reporting a vulnerability](#reporting-a-vulnerability).

To set up, follow the [quick start](README.md#quick-start): Veda builds on
an x86-64 Linux machine with KVM, and the toolchain and the driver VM's
Linux are built once.

## Making a change

1. **Branch** from an up-to-date `main` of your fork.
2. **Make one change**, with its tests and documentation. In a new
   checkout, run a `cargo orchestration` command (such as `doctor`) before
   Cargo itself, so that Cargo builds the crates Veda patches
   ([Builds](documentation/DEVELOPING.md#builds)).
3. **Iterate** on what you changed:

   ```bash
   cargo test -p netstack                                           # one crate's unit tests, on the host
   cargo orchestration script verification/ui/firewall-question.vts  # one GUI script, headless
   cargo orchestration run --cmdline run=settings                    # boot with an application started
   ```

4. **Check everything** before you open a pull request:

   ```bash
   cargo fmt --all --check         # formatting, as rustfmt.toml sets it
   cargo orchestration clippy      # Clippy over every crate as Veda builds it, warnings as errors
   cargo orchestration test --ui   # unit tests, systest inside Veda, then every GUI and agent script
   ```

## Engineering standards

Veda is an operating system: its code handles other programs' data and
people's files, and has to keep working when something around it fails.
Every change meets these standards; [Coding conventions](documentation/CODING.md)
has the detailed rules.

### Security

- **Trust nothing from outside the process.** Check IPC messages, files,
  network data and what people type before using them. Malformed input
  returns an error, never a panic.
- **Decide by identity.** A service gives each caller only what it needs,
  judged by the identity the registry gives each connection, which a
  process cannot forge ([Who may do what](documentation/NETWORKING.md#who-may-do-what)
  is an example).
- **Bound what others can grow.** Every queue, table and buffer that
  another process can fill has a limit, so that one process cannot starve
  the rest.
- **Fail closed.** Defaults are the safe choice (the firewall starts in
  stealth mode), and what is stored is checked again when it is read back.
- **Keep secrets secret.** Keys and passwords stay out of logs and the
  agent's state, and are never shown or returned once saved.
- **Proven cryptography.** Use RustCrypto and rustls, not code of your
  own; [Libraries](documentation/DEPENDENCIES.md) records each exception
  and why.
- **Leave consent to the system.** An application's agent actions declare
  their real risk and run the code the keyboard and mouse run
  ([Consent and safety](documentation/AGENT.md#consent-and-safety)).
- **`unsafe` only where nothing else will do,** each block with a
  `// SAFETY:` comment saying why it is sound.

### Reliability

- **Expect failure.** Services restart when they crash, and the driver VM
  when its Linux does: a client reconnects and restores what the restart
  lost, as `ui` registers an application with the agent again.
- **Answer in time.** An application's frame never waits on slow work,
  which goes to a thread ([The `ui` toolkit](documentation/DEVELOPING.md#the-ui-toolkit)),
  and a call to a peer that may hang, such as a driver, has a deadline
  (`set_timeout`).
- **Say what failed.** Errors are a crate's own enum implementing
  `Display`, in words a person can act on: what failed, and on what.

### Code

- **Proven libraries first.** Use a mature crate where one fits
  ([What a library must fit](documentation/DEPENDENCIES.md#what-a-library-must-fit)).
  A new dependency goes in `[workspace.dependencies]` with its reason; a
  library that is wrong becomes a port, and the fix goes upstream.
- **Write like the code around it**: its naming, comment density and
  idioms. What depends on the processor architecture stays in
  `architectures/` ([Architectures](documentation/CODING.md#architectures)).
- **Explain why.** Public items have doc comments, every module a `//!`
  overview, and comments say why the code is as it is.
- **Keep a change whole and small.** No unrelated renames or reformatting
  beside a fix, and nothing left that the change made unused.
- **Mind the emulator.** Without KVM, QEMU runs Veda 5 to 20 times slower
  than native code: keep hot loops lean
  ([Performance](documentation/CODING.md#performance)), and measure what a
  `perf` change claims, before and after.

## Testing

A change is finished when it is tested where it could break:

| What changed | Test it with | Run it with |
|--------------|--------------|-------------|
| Logic: parsing, protocols, state, maths | Unit tests beside the code | `cargo test -p <crate>` |
| The kernel, services, the POSIX layer, the standard library | `systest`, and the C and Rust test programs | `cargo orchestration test` |
| Applications, the shell, devices, recovery | A GUI script in `verification/ui` | `cargo orchestration script FILE` |
| What the agent does | An agent script in `verification/agent` | `cargo orchestration script FILE` |

- **A fix comes with the test that catches it,** checked to fail without
  the fix.
- **Deterministic and offline.** Tests talk to stand-ins (a server on the
  host, one for Deepgram, a video decoder, a microphone) and wait on log
  lines rather than fixed delays. What needs the real Internet or Deepgram
  goes in `verification/real/`, run by hand.
- **No flaky tests.** A test that fails now and then has a bug, in it or
  in the code: find it rather than retrying.
- **Break things on purpose.** Crashes, restarts and pulled cables have
  scripts, so that recovery is tested like any other feature.
- **A new crate with host tests** goes in `HOST_TESTED`
  (`orchestration/src/runner.rs`); a test fails until it is there.

[Scripts and tests](documentation/DEVELOPING.md#scripts-and-tests) covers
the scripts, whose language `orchestration/src/script.rs` documents.

## Documentation

- **Update what you change.** A change to behaviour, a command, a protocol
  or a limit updates the document that describes it, in the same pull
  request.
- **Write as the documents do**: plain, precise British English in short
  sentences; tables for reference; commands, code and paths in backticks;
  a link rather than a repetition; no emojis.
- **Retake pictures with their scripts.** The README's screenshots come
  from `documentation/screenshots.vts`, Interceptor's from
  `documentation/interceptor.vts`.

## Commits

Each commit message is one line:

```text
type(scope): description
```

- **type**: one of the table below.
- **scope**: what changed, as one lowercase word: a component (`kernel`,
  `firewall`, `shell`, `drivervm`), or `repo` for the whole repository.
- **description**: imperative and lowercase ("add", not "added" or
  "adds"), without a full stop, the whole line 72 characters at most.
- **Nothing else**: no body or footer. The pull request explains.

| Type | For |
|------|-----|
| `feat` | A new capability |
| `fix` | A bug fix |
| `perf` | Faster or smaller, behaving the same |
| `refactor` | Restructured, behaving the same |
| `test` | Tests alone |
| `docs` | Documentation alone |
| `build` | The build, the toolchain or the ports |
| `chore` | Upkeep: removals, lint fixes, licence headers |
| `style` | Formatting alone |

```text
feat(firewall): add a firewall that asks when an application listens
fix(videos): handle decoder restarts and lengthen vgl's fence wait
perf(glsl): preprocess shaders in linear time
```

Every commit builds and passes the tests, so that `git bisect` works, and
the history stays linear: rebase on `main`, never merge it into your
branch.

## Pull requests

Open a pull request against `main` from a branch of your fork, and say:

- **What and why**: the problem, and how the change solves it.
- **How you tested it**: the commands and scripts you ran, and what you
  could not test (hardware you do not have, say).
- **What it looks like**, for a visible change: a screenshot
  (`cargo orchestration shot`, or a script's `shot`).

Before asking for a review:

- [ ] `cargo fmt --all --check` and `cargo orchestration clippy` pass
- [ ] `cargo orchestration test --ui` passes
- [ ] New behaviour has tests, and a fix the test that catches it
- [ ] The documents that describe the change are up to date
- [ ] New files start with their licence header
- [ ] Commits follow the format, on an up-to-date `main`

Reviews look at correctness and security first, then tests, clarity and
fit. Answer a question in the code when the next reader would ask it too
(a comment, a better name), and fold changes into the commits they belong
to rather than adding commits that fix earlier ones. Reviews are about the
code: be specific, and be kind.

## Reporting a vulnerability

> [!CAUTION]
> Never report a vulnerability in a public issue.

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/vahmoh25/Veda/security/advisories/new)
(the **Security** tab, **Report a vulnerability**): what you found, how to
reproduce it, and what it exposes. Where that button is missing, open an
issue that asks for a private contact, without details.

A vulnerability is anything that crosses a boundary Veda draws: a process
reaching what its capabilities do not give it, a caller doing what its
identity does not allow, the agent acting without consent, traffic the
firewall should stop, or the driver VM reaching memory the IOMMU should
keep from it.

## Licence

Veda is under the [GNU General Public License, version 3](LICENSE) only.
By contributing, you license your contribution under it (or under the MIT
licence, for the driver VM's renderer, as [Licensing](documentation/LICENSING.md)
says).

- **Every new file** starts with its SPDX header, with your name and the
  year the file is first published
  ([Licence headers](documentation/CODING.md#licence-headers) has the form
  for each kind of file):

  ```rust
  // SPDX-FileCopyrightText: © 2026 Your Name
  // SPDX-License-Identifier: GPL-3.0-only
  ```

  To a file you change substantially, you may add a line of your own
  below those there. `cargo orchestration test` checks every file.
- **Code from elsewhere is not copied in.** Third-party software is a port
  (`implementation/ports/`): its release, pinned by its SHA-256, with a
  small, explained patch
  ([Updating a port](implementation/ports/README.md#updating-a-port)), and
  its licence in [Licensing](documentation/LICENSING.md).
