<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Developing Veda

How to build, run and test Veda, how its system image is laid out, and how
to write programs for it. [Coding conventions](CODING.md) has the rules the
code follows, [Architecture](ARCHITECTURE.md) the design, and
[Contributing](../CONTRIBUTING.md) how changes are proposed, tested and
reviewed.

**Contents:** [Commands](#commands) · [Running in QEMU](#running-in-qemu) · [The live system](#the-live-system) · [Scripts and tests](#scripts-and-tests) · [Reading the logs](#reading-the-logs) · [Builds](#builds) · [The system image](#the-system-image) · [Writing an application](#writing-an-application) · [Performance notes](#performance-notes)

## Commands

Each is `cargo orchestration COMMAND`, run in the repository;
`cargo orchestration help` has the details.

| Command | What it does |
|---------|--------------|
| `doctor` | Checks the tools, and says what is built |
| `toolchain` | Builds GCC, binutils, musl, the media libraries and Rust for Veda into `target/toolchain`: once, before the first build |
| `linux` | Rebuilds the driver VM's Linux, its toolchain and Mesa in `target/linux` (the first build builds them, given the tools) |
| `build` | Builds `target/veda/veda.img` |
| `run` | Builds, then boots Veda in a QEMU window |
| `iso` | Writes [the live system](#the-live-system), `target/veda/veda.iso` |
| `shot` | Boots headless and saves `target/veda/screen.png` |
| `script FILE` | Boots headless and runs a [script](#scripts-and-tests) |
| `scripts FILE...` | Builds once, then runs the scripts side by side (logs in `target/veda/test-logs`) |
| `test` | Runs the [tests](#scripts-and-tests); `--ui` adds the scripts |
| `clippy` | Clippy over every crate as it is built, warnings as errors |
| `acpi DIR` | What devmgr makes of a PC's ACPI tables (see [Reading the logs](#reading-the-logs)) |
| `clean` | Removes build outputs |

Their options (of values separated by `|`, the first is the default):

* **Building:** `--debug` (no optimisations), `--resolution WxH`,
  `--cmdline "..."` (kernel arguments, 256 bytes at most with the
  build's), `--no-generate` (reuse the generated media), `--skip PROGRAM`
  (leave a program out; repeatable).
* **QEMU:** `--smp N` (default 4), `--memory MiB` (default 1024),
  `--headless`, `--serial FILE`, `--gdb` (a debugger on localhost:1234),
  `--qemu-arg ARG` (repeatable), `--gpu` or `--no-gpu` (default: a 3D GPU
  if QEMU has one), `--no-iommu`, `--sound virtio|hda`, `--no-audio`,
  `--net ethernet|none`, `--nic virtio-net-pci|e1000e|igb`,
  `--input standard|usb`, `--fresh-home`, `--live`.
* **Scripts:** `--jobs N`, how many scripts run at once (default: one per
  five processors); for `toolchain` and `linux`, how many build jobs.
* **`shot` and `iso`:** `--wait SECS` (default 10) or `--until TEXT` (in
  the serial log), and `--out FILE`.

## Running in QEMU

`run`'s screen is 1920x1080; the scripts', `shot`'s and `build`'s image's
is 1280x800, which the scripts' clicks and the README's pictures are laid
out for. QEMU's display offers the screen's size as its EDID's preferred
mode, so the firmware starts in it and Linux drives it at 75 Hz. A
`--resolution` is at least 640x480 and at most 4095x4095, its width a
multiple of 8; above about six
million pixels the rate drops (56 Hz at 3840x2160).

* **Display and GPU.** QEMU's VGA flips once Linux drives it. Where QEMU
  has a 3D GPU (`virtio-gpu-gl-pci`; `qemu-system-modules-opengl` on
  Debian and Ubuntu), the driver VM gets it for Veda's renderer; without
  it, OpenGL ES renders in software. The GPU renders on the host's:
  through `-display gtk,gl=on` in the window, and headless through
  `-display egl-headless`, on the GPU the firmware showed its screen on or
  the render node `VEDA_RENDERNODE` names.
* **Sound.** `--sound hda` is the ICH9's HD Audio controller, which Linux
  drives in the driver VM, as on a PC. Scripts that reset the machine use
  it, as QEMU's virtio sound card, before its commit 75d28444c7 (September
  2026), writes buffers held across a reset into the next start's memory.
* **USB input.** `--input usb` replaces the PS/2 controller and the virtio
  tablet with a hub (a keyboard and a tablet on it) and a mouse on an xHCI
  controller. Scripts say `input usb`, and plug devices in and out with
  `qmp device_add '{"driver":"usb-kbd","bus":"xhci.0","port":"2.3","id":"kbd2"}'`
  and `qmp device_del '{"id":"kbd2"}'`.
* **Home directory.** It is kept across builds and runs on its own disk,
  `target/veda/home.img`, until `--fresh-home`; `shot`, `script` and
  `test` use a fresh one (`test-home.img` in `$VEDA_OUT`).
* **Kernel command line.** `run=NAME` has `init` start `/system/bin/NAME`
  after the system services, the quickest way to try an application
  (`run=NAME:ARG1,ARG2` passes arguments). `run=nettest` checks the
  Internet from inside Veda (`run=nettest:wifi,SSID,PASSWORD` joins a
  Wi-Fi network first); `drivervm=off` starts no
  [driver VM](DRIVERVM.md); `startup-sound=off` (also in an image's
  `BOOT.CFG`) leaves out the startup sound, as the scripts that record the
  sound output or feed it to the microphone do.

## The live system

`cargo orchestration iso` writes a hybrid ISO image
(`orchestration/src/image/iso9660.rs`) that boots with UEFI from a disc,
or from a stick written by Rufus (in ISO mode) or as it is. It boots with
`live` on the kernel command line (no disk driver; `/home` in memory), at
1920x1080 or the screen's largest mode below it, and with no time zone, as
a PC's clock keeps local time under Windows. `run --live` boots it in QEMU
from a virtual USB stick (`target/veda/stick.iso`), and so do scripts with
a `live` line, on a machine with a disk for it to leave alone
(`verification/ui/live-usb.vts`).

The stick keeps the logs of the last eight starts, synced every second, so
they hold what happened even when the screen showed nothing:
`VEDA/LOGS/NNNN/VEDA.TXT` (Veda's log, with Linux's console) and
`LINUX.TXT` (the driver VM's Linux's whole log, its display drivers'
debugging messages too). Its EFI system partition mounts as `VEDA` on any
computer. Scripts boot from a writable stick with `live writable` and
check its logs with `expect-stick` (`verification/ui/live-logs.vts`).

## Scripts and tests

`cargo orchestration test` runs the unit tests on the host, then `systest`
inside Veda; `--ui` adds the scripts of `verification/ui/` (the desktop,
the applications and the devices; `architectures/amd64/`: a PC's machine
only) and `verification/agent/` (with a stand-in for Deepgram, and a
microphone fed from the host by `testmic`). Those in `verification/real/`,
run by hand, use the real Internet and Deepgram, with the key in
`DEEPGRAM_API_KEY` ([The agent](AGENT.md#testing)).

A script (`.vts`) drives QEMU and checks the log and the screen, as its
opening comment says. `orchestration/src/script.rs` documents the
language, such as how a `wait-serial` matches lines after a `checkpoint`
and how `shot` waits (three seconds at most) for the screen to settle.
Scripts are checked whole before anything is built: a wrong line fails
the run, with its number.

* **Resets.** `reset` restarts the machine as a PC does, its memory kept,
  and fails if Veda's loader does not start again, saying what the
  firmware printed and where each processor is. QEMU's loader blanks, at
  every reset, the page from which OVMF before edk2's e4f99a8554 reads a
  processor count and a memory map (`blank_at_reset`,
  `orchestration/src/architectures/amd64/qemu.rs`).
* **Outputs.** `shot`, `script` and `test` save the serial console (the
  kernel's log and every program's `println!`) to
  `target/veda/serial.log`, and headless runs' sound to
  `target/veda/audio.wav`, where `expect-audio-gapless` fails on digital
  silence over 20 ms, as when a driver falls behind. A script's paths are
  relative to the repository; those under `target/veda/` follow
  `$VEDA_OUT`. `scripts` and `test` write the system's image once
  (`target/veda/system/veda.img`), and each machine boots a qcow2 overlay
  of it (`boot.qcow2` in its `$VEDA_OUT`).
* **Deadlines.** A script of `test --ui` is killed, with its QEMU, past
  four times its last run (at least five minutes).
* **The network.** The network cards' scripts check it against a server on
  the host (`nettest local`, `orchestration/src/hostweb.rs`), without the
  Internet.

```bash
cargo orchestration script documentation/screenshots.vts                         # the README's screenshots
cargo orchestration script documentation/interceptor.vts --resolution 1920x1200  # Interceptor's: scale it to 1280x800
```

Where QEMU has the 3D GPU, `test` also runs the OpenGL ES tests on the
host's GPU through the system's virglrenderer (`libvirglrenderer1` on
Debian and Ubuntu); once the driver VM's Linux is built, also through
Veda's renderer, on softpipe, on Mesa's virgl over `virgl_test_server`
(`virgl-server`), and on the machine's Intel GPU if it has one. By
hand:

```bash
GL_TEST_BACKEND=virgl cargo test -p gl                      # on the host's GPU (desktop OpenGL)
GL_TEST_HOST=gles GL_TEST_BACKEND=virgl cargo test -p gl   # ... on OpenGL ES
GL_TEST_RENDERNODE=/dev/dri/renderD129 GL_TEST_BACKEND=virgl cargo test -p gl   # ... on another GPU
GL_TEST_BACKEND=gallium cargo test -p gl                    # through Veda's renderer on softpipe
VR_DEPTH_LOW=1 GL_TEST_BACKEND=gallium cargo test -p gl     # ... with depth kept as on iris
GL_TEST_BACKEND=gallium GL_GALLIUM_DEVICE=iris cargo test -p gl -p interceptor-game   # ... on an Intel GPU
GL_GALLIUM_DEVICE=iris cargo test -p interceptor-game        # Interceptor's game: world, drivers, sounds, pictures
```

Interceptor's game tests save pictures and sounds to the directory
`INTERCEPTOR_SHOTS` or another variable of
`implementation/applications/interceptor/game/src/tests/` names.

## Reading the logs

In Veda's Terminal, `dmesg TEXT` shows the log's lines that contain TEXT.
Displays, GPUs, input, USB, networks and a PC's sound are Linux's, in the
driver VM; its log comes in Veda's (`drivervm: linux:`).

| Startup line | Says |
|--------------|------|
| `boot: the loader cleared the screen in N ms; ...` | How long it took, with the framebuffer's memory type (uncached by the firmware, write-combining for the loader) |
| `boot: the firmware's screen modes: ..., 1920x1200 (chosen), ...` | The mode chosen, of the display's shape where the firmware gives its EDID |
| `devmgr: coming: the display's driver: ...; sound: ...` | What the window system and the audio service wait for, or `none` |
| `compositor: the splash fades in (...)` | The display driver's first picture, or `... did not show its first picture in 20 s` |
| `compositor: the desktop appears (...)` | After `the shell did not say the desktop may appear` if the startup sound was late |
| `compositor: desktop shown after N ms (...)` | From the window system's start: time black, the splash (at least 2.1 s), frame rate, and frames: in the firmware's framebuffer or a driver's flips (as many as vertical blanks: none missed), by the GPU or the processor |
| `compositor: frames are drawn by the GPU (...)` | The GPU took over, or `frames stay with the processor:` why not |
| `lock: ready (...)` | Whether a password is set; then `lock: unlocked (no password is set)` as the window system starts locked, or `lock: the screen is locked` and the lock screen asks for it |
| `lock: the screen locks after N minutes without input` | The idle timeout the window system was told (or `... does not lock for want of input`); `compositor: no input for N s: the screen locks` when it does |
| `shell: startup sound: playing` | The sound started, or why not |

Linux's first commit kept the firmware's setup if it logs
`keeping the firmware's pipe bpp 24` and no
`PPS 0 turn panel power off`. The startup's look is in
`implementation/services/compositor/src/startup.rs`, its sound in
`generation/music/src/songs/startup.rs`.

| `dmesg` | Shows |
|---------|-------|
| `drivervm` | Linux and Veda's drivers for Linux (the display `kms` drives, its mode, whether it copies or scales the compositor's pictures; the GPU `gpu` serves; what `input` and `net` attach), and what the monitor gives the guest: GPIO pins, the firmware's variables and tables (`... comes with the firmware's NHLT`), an Intel GPU's OpRegion and host bridge |
| `compositor` | The display attaching |
| `snd_hda`, `cs35l41`, `alsa` | Linux's sound controllers and codecs; a laptop's speaker amplifiers; Veda's sound driver for Linux (the card, underruns, the device it records) |
| `devmgr` | The ACPI tables (`devmgr: acpi: N tables, ...`, and the conditional definitions left out), each driver's device's devices in them (`acpi: 00:1e.3 is \_SB.PC00.SPI1, ...`), and `gpio:` a controller's register windows, the pads set up for drivers and the pins that interrupt |

`cargo orchestration acpi DIR` runs devmgr's interpreter on the host over
a PC's ACPI tables (`sudo cp -r /sys/firmware/acpi/tables DIR`): the PCI
root bridges' windows (where devmgr places BARs the firmware left
unplaced), what the motherboard reserves, and each root bus function's
ACPI device, INTx route and devices below it. The firmware's variables
read as zeros unless set with `--set NAME=VALUE`, so devices they enable
may be missing; PCI configuration space comes from `DIR/pci/BB:DD.F`
files (copies of `/sys/bus/pci/devices/*/config`, whole as root), else
from this machine.

## Builds

> [!IMPORTANT]
> In a new checkout, run a `cargo orchestration` command (such as
> `doctor`) before Cargo itself (`cargo test -p ...`, rust-analyzer).
> Every command but `help` and `clean` has Cargo build the crates among the
> ports (smoltcp, zune-jpeg, tokio and others: "Crates" in
> `implementation/ports/README.md`) as patched; until then, Cargo builds
> their releases.

* **Generated media.** A generator in `generation/` runs when its
  executable (with every library it uses) has changed. It writes into
  `target/generated/<generator>/` (in `$CARGO_TARGET_DIR`), lists what it
  wrote in its `.manifest`, which the image is packed from, and removes
  anything else there.
* **Builds side by side.** Give each its own `CARGO_TARGET_DIR` (Cargo's
  output, generated media) and `VEDA_OUT` (image, serial log,
  screenshots):
  `export CARGO_TARGET_DIR=target/agent-x VEDA_OUT=target/agent-x/veda`.
  A build waits, saying so, for what they share: the toolchain
  (`target/toolchain`, which image builds add to), the driver VM's Linux
  and Mesa (`target/linux`), and a target directory's generated media.
* **No paths of the build machine.** Programs name the checkout, Cargo's
  home and the target directory `/veda`, `/cargo` and `/veda/target`
  (`--remap-path-prefix` through Cargo's configuration, so a build by hand
  compiles again; `-ffile-prefix-map` for C and in
  `implementation/ports/build.sh`).
  `strings target/veda/veda.img | grep -F "$HOME"` finds nothing; a line
  it finds is a leak to fix.

## The system image

`cargo orchestration build` puts these into the initrd, mounted at
`/system`:

| Source | Installed as |
|--------|--------------|
| The programs in `PROGRAMS` (`orchestration/src/components.rs`), without the symbols that `target/x86_64-unknown-linux-musl/` (on `rt` alone) and `target/std/x86_64-unknown-veda/` (on the standard library) keep | `/system/bin/<name>` |
| `LICENSE` (the GPL, version 3) | `/system/LICENSE.txt` |
| The fonts and their licence, `implementation/ports/fonts/` | `/system/fonts/` |
| The manifests, `implementation/applications/<name>/<name>.app` | `/system/apps/<name>.app` |
| What the generators' manifests list | `/system/<same relative path>` |
| `verification/c/*.c` and `*.cc`, by the cross toolchain (also as `<name>-pie`), for `systest` and the scripts | `/system/tests/c/<name>` |
| `verification/rust/*.rs`, by the toolchain's Rust (also as `<name>-fixed`), the same | `/system/tests/rust/<name>` |
| The native C toolchain, `target/toolchain/native/system` | `/system/bin/gcc` ..., `/system/include`, `/system/lib`, `/system/libexec` |
| `mediadec`, the video decoder (its Rust half, the C shim, FFmpeg, dav1d), and the libraries' `checkasm` programs | `/system/bin/mediadec`, `/system/tests/media/` |
| `verification/fakedec/clips/*`, for the stand-in decoder | `/system/tests/videos/fake/` |
| The driver VM's Linux, once built: its kernel (a bzImage on AMD64) and the programs of `implementation/guests/linux/` | `/system/linux/kernel`, `/system/linux/initramfs.cpio` |

At boot, the image's `samples/` is copied into the home directory
(`/system/samples/Pictures/x.png` → `/home/user/Pictures/x.png`). Images
leave out their free space (sparse files); the boot partition holds its
files and the 64 MiB a stick's logs may take.

## Writing an application

Applications are programs on Rust's standard library, with Veda's
libraries ([Rust on Veda](RUST.md)).

1. Create a crate in `implementation/applications/<name>`, and add it to
   the workspace's `members` in `Cargo.toml` and to `PROGRAMS` in
   `orchestration/src/components.rs`, as `Program::std("<name>")`.
2. Its `Cargo.toml` depends on `rt`, `abi`, `ui` and others as needed.
3. Write `src/main.rs`:

   ```rust
   struct MyApp { /* state */ }

   impl ui::App for MyApp {
       fn update(&mut self, ui: &mut ui::Ui) {
           // Draw and handle input for one frame. ui.rect() is the client area.
           let r = ui.rect().centered(200, 40);
           if ui.primary_button(r, "Hello") { /* ... */ }
       }
   }

   fn main() {
       let mut spec = ui::WindowSpec::new("My App", 800, 560);
       spec.app_id = "myapp".into();
       std::process::exit(ui::run(spec, MyApp { }));
   }
   ```

4. Add a manifest for the start menu, `<name>.app` beside `Cargo.toml`.
   `icon` names a vector icon (`ui::Icon::by_name`); `pinned=true` puts
   the application in the start menu's pinned grid, and `taskbar=true`
   keeps it on the taskbar when it is not running:

   ```text
   name=My App
   exe=/system/bin/myapp
   icon=document
   category=Accessories
   description=What it does
   pinned=false
   taskbar=false
   ```

5. Make it agent-compatible with `App::agent_info` (a summary, and the
   actions with their risk), `App::agent_state` (what the window shows, as
   JSON) and `App::agent_invoke` (runs an action through the keyboard's and
   mouse's code); `ui::run` registers it with the agent. See
   [The agent](AGENT.md#making-an-application-agent-compatible).

### The `ui` toolkit

* **Immediate mode.** Widgets take explicit `Rect`s and return their
  interaction (`button` → `bool`, `text_input` → `TextInputResponse`,
  `list` → `ListResponse`, `menu_bar` → the chosen item, ...);
  `gfx::Rect` has `split_top/left/right/bottom`, `inset` and `centered`.
* **Drawing and input.** `ui.canvas` is a `gfx::Canvas` for custom
  drawing, `ui.ctx.text` draws text, and `ui.input` holds the frame's
  pointer, buttons, keys (`proto::input::keys`), typed text and scroll.
* **Frames** are drawn when events arrive; `ui.repaint_at(deadline_ns)` or
  `ui.repaint()` asks for more.
* **The window.** `ui.set_title`, `ui.window_state()`,
  `ui.set_window_state` (maximise, minimise, full screen), `ui.activate()`
  (to the front), `ui.close_window()`.
* **Long-running work** must not block `update`: give it to a
  `std::thread` (which sets its priority with `rt::thread::set_priority`)
  through a `std::sync::mpsc` channel, and have it signal an `Event` the
  window waits on (`App::wait_handles`), as
  `implementation/applications/paint/src/files.rs` does.
* **Files.** Small ones of the application's own (settings, state) go
  through `std::fs` and `apps_common::store` (reads within a limit,
  replacement in one step). Pictures, music and other large ones go
  through `files::Fs::connect()` (whole files in shared memory, bounded
  or mapped; listings of any size; trees copied, moved and removed with
  their progress; a deadline on every call), or the protocol,
  `proto::vfs::Client` on `proto::connect(proto::vfs::NAME)`
  (`read_file` returns a VMO and a length).
* **Other programs** start through `proto::launcher::Client` (`launch`,
  `launch_app`).
* **Without the toolkit,** games and other full-screen renderers use
  `ui::window::Window`: `begin_frame()` returns a `Canvas` for the back
  buffer, `present()` shows it, `poll_events()` returns `WindowEvent`s,
  and `can_draw()` says when the compositor is ready for the next frame;
  after `triple_buffer()`, a frame may be drawn while the last is still on
  its way, and presented once `can_draw()` says so.

### Programs on `rt` alone

Services and drivers that the standard library stands on, that drive
hardware or that must answer in time are `no_std` programs on `rt`
(`Program::native`). Their `Cargo.toml` has `build` as a build
dependency, `build.rs` is `fn main() { build::user_program(); }`, and
`src/main.rs` starts with `#![no_std]`, `#![no_main]`,
`extern crate alloc;` and `rt::entry!(main);` (a `main` returning `()`,
an `i32` or a `Result`). They log with `rt::println!`.

### Programs in C and on Rust's standard library

Both are static ELF executables on the C library (musl) that run from the
Terminal like any command. GCC compiles C inside Veda, or on the
development machine as the cross compiler (`<target>-gcc` for the
architecture's C target); the
toolchain's Rust compiles for Veda's target (`x86_64-unknown-veda` on
AMD64), and the cross compiler links ([C on Veda](C.md),
[Rust on Veda](RUST.md), which covers Cargo too).

```bash
target/toolchain/cross/bin/x86_64-veda-gcc -O2 -Wall -o hello hello.c
target/toolchain/rust/bin/rustc --edition 2024 --target x86_64-unknown-veda -C linker=target/toolchain/cross/bin/x86_64-veda-gcc -O -o hello hello.rs
```

### Videos and its decoder

Videos (`implementation/applications/videos`) is a `ui` application;
`videos/core`, tested on the host, has its playback, clock, pacing,
choice of pictures, skipping, layout and memory, and the audio output that
outlives the audio service, which Music shares. It speaks `proto::media`
with its decoder (a heartbeat, a frame pool, a sound ring).

The decoder, `implementation/services/mediadec`, is a C program: its Rust
half, built for the bare target (`x86_64-unknown-none` on AMD64), is
linked with its C shim, FFmpeg and dav1d by the cross compiler
(`mediadec()` in `orchestration/src/toolchain.rs`), once `toolchain` has
built the media libraries (`doctor` says whether it has). Scripts that
need it say `requires media`. Without it, `verification/fakedec` stands
in: `videos --decoder /system/bin/fakedec FILE` plays a text file of
`/system/tests/videos/fake` (`verification/fakedec/src/clip.rs`) as a
timecode and a tone, crashing, hanging or falling behind when the clip
asks (`verification/ui/videos-fake.vts`).

`generation/videos` makes the sample videos (`samples/Videos`, copied to
`~/Videos`) with rav1e (AV1, on the build machine only) and a FLAC
encoder of its own, and writes `/system/tests/videos/Countdown.md5`, every
picture's MD5, for `systest` to check the decoder against, with a
truncated copy for the error scripts. `VIDEOGEN_PREVIEW=DIR` adds PNGs of
what was drawn and decoded.

## Performance notes

QEMU runs Veda on KVM where the host has it, and otherwise on its TCG
emulator, several times slower than native code. Keep per-frame work
proportional to what changed, avoid per-pixel floating point in hot loops,
and render 3D scenes at a modest internal resolution. The rules for hot
loops are in [Coding conventions](CODING.md#performance).
