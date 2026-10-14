<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Veda: The Agentic-Native Operating System

Veda is an operating system with a voice agent built in. Say "Hey Veda,
switch my wallpaper to the dunes" and it does the work in any application,
through the same code as your keyboard and mouse. It asks you first before
anything that is hard to undo.

The system is written in Rust and lives in this repository: the UEFI
bootloader, a capability-based microkernel, the system services, the
window system, the toolkit and the applications. Where a proven, widely
used library exists, Veda uses it rather than its own: TCP/IP, TLS and
cryptography (smoltcp, rustls, RustCrypto), image codecs (zune, qoi,
fast_image_resize) and math (libm, glam, rand). Veda runs on x86-64 PCs
and in QEMU.

![The Veda desktop, with the agent's window open and listening](documentation/images/desktop.png)

## Why agentic-native

Operating systems were designed for a person at a keyboard. An assistant
added later has to read the screen, imitate clicks or use the few
applications that have an API, and the system cannot hold it to anything.
In Veda, the agent is a first-class user of the system:

* **Part of the system.** The agent is a system service, started at boot
  and restarted if it fails. It sleeps until you say its name, click the
  ring on the taskbar or press **Super+Space**.
* **Native in every application.** Applications declare their actions
  (typed parameters and a risk level) and report what they show as JSON.
  The agent calls those actions, which run the same code as the keyboard
  and mouse: no screen scraping, no simulated clicks. The built-in
  applications offer more than 70 actions.
* **Consent enforced by the system.** Every action is *routine*,
  *sensitive* or *destructive*. The agent service, not the language model,
  holds the last two until you allow them. Only the desktop shell can
  answer (`init` tells every service which program is calling), so
  nothing the agent reads in a document can approve anything.
* **Real context.** Each conversation starts with the time, the open
  windows, every application's actions, what the agent knows about you
  and how your last conversation ended. Before acting, it reads the
  applications' current state.
* **Memory you control.** It remembers only what you tell it plainly (your
  name, the people in your life, how you like things done), never its own
  guesses. The memory stays on your computer, and Settings shows and
  forgets any part of it.
* **Voice first.** You can interrupt it at any time. Echo cancellation and
  an echo gate keep it from hearing itself, and other sound drops 20 dB
  during a conversation. Asleep, it listens with a voice detector that
  runs locally, so no audio leaves the computer while nobody speaks.
* **The intelligence is a setting.** Speech recognition, the language
  model and the voice come from Deepgram's Voice Agent API, with the audio
  opted out of Deepgram's model improvement. The model (OpenAI's
  gpt-4.1-mini by default), the voice and even the agent's name are
  chosen in Settings.

## How the agent works

1. **Listening.** Asleep, the agent runs a voice detector on the computer.
   Speech it hears goes to Deepgram's recogniser, with the agent's name as
   a key term. A sentence that starts or ends with the name ("Hey Veda, …",
   "…, Veda?") wakes the agent, and the rest of the sentence is its first
   request.
2. **Conversation.** Waking opens a Deepgram Voice Agent session with the
   agent's instructions, context and functions. Deepgram handles
   turn-taking, the language model and the voice; Veda's audio service
   carries the sound both ways.
3. **Action.** The model calls the agent's 17 functions (windows, files,
   volume, Wi-Fi, wallpaper, notifications, timers and reminders, memory,
   the system and its programs) and, through `use_app` and `read_app`,
   every application's actions and state. Each call returns a structured
   result.
4. **Consent.** A call that needs your OK waits in the agent's window, or
   in a notification, saying exactly what will happen and to what. It runs
   only if you allow it within three minutes. Sensitive actions can be
   allowed for good; destructive ones are asked about every time.

[The agent](documentation/AGENT.md) covers the design in full: the
protocols, the voice pipeline, consent, memory and testing.

## The operating system

* **Microkernel.** `vkernel` handles only isolation, scheduling, memory
  and IPC: capability handles with rights, channels that carry messages
  and handles, VMOs, events, futexes and interrupt objects. It supports
  SMP with x2APIC, tickless timers and XSAVE.
* **Supervised services.** Drivers, the file system, the window system,
  audio, networking and the agent are user-space processes. `init`
  restarts the window system, the shell, audio, networking and the agent
  if they crash, so the desktop comes back on its own.
* **Linux's drivers in a VM.** Veda drives only its disks (virtio-blk,
  AHCI, NVMe). Linux drives everything else in the *driver VM*, on Veda's
  own hypervisor (VMX with EPT, no emulated hardware). Devices go to it
  whole, their DMA confined and their interrupts remapped by Veda's IOMMU
  driver. A crashed driver takes down only the VM, which Veda resets and
  restarts. See [the driver VM](documentation/DRIVERVM.md).
* **Desktop.** A compositing window manager with server-side decorations,
  shadows, animations, snapping and an Alt+Tab switcher with live
  thumbnails. It composes on the GPU with Intel graphics and on the
  processor elsewhere; once Linux drives the display, every frame is
  flipped whole at the vertical blank. The shell has a taskbar, a
  searchable start menu, desktop icons, a calendar and notifications.
* **Toolkit.** `vui` is an immediate-mode toolkit: widgets, menus,
  dialogs, vector icons and anti-aliased text in Inter and JetBrains Mono.
  Every `vui` application is agent-compatible.
* **3D graphics.** OpenGL ES 3.0 with GLSL ES 1.00 and 3.00, in Rust
  (`vgl`, `vglsl`), every call checked as the specification requires. It
  renders on the GPU through Mesa in the driver VM (Intel's iris; virgl on
  the host's GPU under QEMU), or on a multi-threaded software renderer.
  *Interceptor* shows it off, full screen: a bay city with its traffic,
  physically based lighting, shadows, bloom and 4x multisampling.
* **Networking.** A network service with IPv4 and IPv6, DHCP, DNS, TCP,
  UDP and ICMP, and a Wi-Fi service that joins WPA2, WPA3 and open
  networks with management frame protection, remembers them, reconnects
  and roams. Linux's drivers only move frames and never see a password.
  Applications get TLS 1.3 and 1.2 from `vtls`, with certificates checked
  against Mozilla's roots. See [Networking](documentation/NETWORKING.md).
* **Audio.** A mixing service with echo-cancelled recording and ducking,
  built for the agent's conversations as much as for music.
* **Video.** FFmpeg and dav1d, ported to Veda, decode the common formats
  (AV1, VP9, H.264, HEVC and more) in a sandboxed process that reaches
  nothing but the file it is given: a damaged file that crashes or hangs
  it costs a moment, not the video. Pictures come through shared memory
  and are shown in time with the sound, timed to the screen's refresh.
  See [Architecture](documentation/ARCHITECTURE.md#video).
* **Storage.** The home directory has its own disk. It is saved half a
  second after changes stop, alternating between two checksummed slots,
  so an interrupted save never damages the previous one. The agent's
  memory and key live in a private directory that only the agent can
  open.
* **C and GCC.** The image includes GCC 16, binutils and musl: write a C
  program in the Terminal, compile it and run it, in QEMU or on a PC. C
  programs are static executables whose system calls go to `vposix`, a
  POSIX layer in Rust. A cross compiler also builds C and C++ programs
  for Veda on the development machine. See [C on Veda](documentation/C.md).
* **Rust's standard library.** The toolchain builds Rust from its release,
  with Veda among the compiler's targets: programs that use the standard
  library (threads, files, processes, panics with backtraces) run on Veda,
  where it stands on the C library. See
  [Rust on Veda](documentation/RUST.md).

## Applications

Every application is agent-compatible: what you can do with the mouse and
keyboard, the agent can do too.

| Application | What it does | What the agent can do |
|-------------|--------------|-----------------------|
| **Text Editor** | Tabs, syntax highlighting (Rust, C, TOML, Markdown), find and replace, word wrap, zoom, unlimited undo. | Create, open, read and write documents; find and replace, undo, save; save as or discard changes (with your OK). |
| **Photos** | A thumbnail library of `~/Pictures` and a viewer with zoom, rotation, full screen and slideshows; PNG, JPEG, BMP and QOI. | Show, zoom and rotate a picture, go full screen, run a slideshow, set a picture as the wallpaper. |
| **Music** | A library of `~/Music` (QOA and WAV), cover art, a live spectrum visualiser, seeking, shuffle and repeat. | Play a song by name, pause, stop, skip, seek, shuffle, repeat, set the volume. |
| **Videos** | A library of `~/Videos` with thumbnails and "Continue watching", and a player with seek previews, full screen, picture stepping and resuming where you left off; AV1, VP9, VP8, H.264, HEVC, MPEG-4 and MPEG-1/2 with AAC, Opus, Vorbis, MP3, FLAC and AC-3, in MP4, Matroska, WebM, Ogg, AVI and MPEG-TS. | Play a video by name, from where it was left or the start; pause, seek, set the volume, go full screen, list the videos. |
| **Files** | Places, breadcrumbs, list and icon views with thumbnails, search, copy and paste, properties, and a Trash that restores items where they were. | Open, search, make folders; copy, move, rename and delete to the Trash (with your OK); restore; empty the Trash (asked every time). |
| **Terminal** | A shell with about fifty built-in commands (files, processes, `wifi`, `ping`, `curl`, ...), history and tab completion; runs C programs and GCC with pipes, redirections and Ctrl+C. | Run a command (with your OK), read its output, interrupt it. |
| **Task Manager** | Processes with CPU and memory use, "end task" and live performance graphs. | List processes, read the graphs, end a program (asked every time). |
| **Settings** | Wallpapers, the agent (key, name, voice, models, listening, memory, permissions), networks and Wi-Fi, the display and the system. | Open a page, change the agent's voice and speaking rate; rename it or change its model (with your OK). Never the key. |
| **Interceptor** | Police pursuits through a bay city in OpenGL ES 3.0: eight missions, quick chases and patrols, by day, at sunset, at night and in the rain, with synthesised engines, sirens and music. | Start a mission, a chase or a patrol; pause, resume, restart or quit; change the sound, units, camera and display. |

## Screenshots

<table>
  <tr>
    <td width="50%"><img src="documentation/images/editor.png" width="100%" align="top" alt="The Text Editor showing a Rust program with syntax highlighting"></td>
    <td width="50%"><img src="documentation/images/photos.png" width="100%" align="top" alt="Photos showing a library of eight pictures"></td>
  </tr>
  <tr>
    <td align="center">Text Editor, highlighting Rust</td>
    <td align="center">Photos, the picture library</td>
  </tr>
  <tr>
    <td width="50%"><img src="documentation/images/music.png" width="100%" align="top" alt="Music playing a song, with cover art and a spectrum visualiser"></td>
    <td width="50%"><img src="documentation/images/files.png" width="100%" align="top" alt="Files showing pictures as thumbnails, with the Trash in its sidebar"></td>
  </tr>
  <tr>
    <td align="center">Music, with its visualiser</td>
    <td align="center">Files, with thumbnails</td>
  </tr>
  <tr>
    <td width="50%"><img src="documentation/images/videos.png" width="100%" align="top" alt="Videos playing an aurora timelapse, a picture from further on above its seek bar"></td>
    <td width="50%"><img src="documentation/images/agent-settings.png" width="100%" align="top" alt="Settings on the agent's page: its Deepgram key, its name, voice and speaking rate, and its language model"></td>
  </tr>
  <tr>
    <td align="center">Videos, AV1 decoded by dav1d</td>
    <td align="center">Agent Settings</td>
  </tr>
  <tr>
    <td width="50%"><img src="documentation/images/gcc.png" width="100%" align="top" alt="The Terminal, where GCC names an undeclared variable in a C program"></td>
    <td width="50%"><img src="documentation/images/interceptor.png" width="100%" align="top" alt="Interceptor: a chase at sunset on a road through the trees of the hills"></td>
  </tr>
  <tr>
    <td align="center">GCC 16 in the Terminal</td>
    <td align="center"><i>Interceptor</i>, in OpenGL ES 3.0</td>
  </tr>
</table>

Every screenshot is 1280x800. They are taken in QEMU by
`cargo orchestration script documentation/screenshots.vts`, with the agent
talking to a stand-in for Deepgram. Interceptor's is taken at 1920x1200 by
`cargo orchestration script documentation/interceptor.vts --resolution 1920x1200`
and scaled down.

## Quick start

### Requirements

Veda is built on an x86-64 PC with Linux.

* [Rust](https://rustup.rs), stable. `rust-toolchain.toml` makes rustup
  install the extra targets on first use, and Rust's own linker
  (`rust-lld`) links everything.
* [QEMU](https://www.qemu.org/download/) and the OVMF UEFI firmware (on
  Debian and Ubuntu: `qemu-system-x86`, `qemu-system-gui`,
  `qemu-system-modules-opengl` and `ovmf`), with access to `/dev/kvm`.
* For the driver VM: the build tools for Linux, a cross compiler and Mesa
  (on Debian and Ubuntu: `build-essential`, `bc`, `curl`, `rsync`, `m4`,
  `flex`, `bison`, `meson`, `ninja-build`, `libgmp-dev`, `libmpfr-dev`,
  `libmpc-dev`, `python3-mako` and `python3-packaging`), and KVM's nested
  virtualization (`kvm_intel nested=1`). See
  [the driver VM](documentation/DRIVERVM.md#testing).
* Optional: a [Deepgram](https://deepgram.com) API key for the agent, and
  your distribution's build tools for the toolchain, which
  `cargo orchestration toolchain` builds once: C (see
  [C on Veda](documentation/C.md#the-toolchain)), with `nasm` for the video
  decoders it builds too, and Rust, with CMake (see
  [Rust on Veda](documentation/RUST.md#the-toolchain)).

Check the environment:

```bash
cargo orchestration doctor
```

### Build and run

First, build the driver VM's Linux (its kernel, a cross compiler and
Mesa), which takes a while. Without it, Veda boots with no keyboard,
mouse, sound or network.

```bash
cargo orchestration linux
```

Then build and run Veda:

```bash
cargo orchestration run
```

This builds every component, writes `target/veda/veda.img` and boots it
in a QEMU window at 1920x1080. The first build also renders the sample
pictures, music and videos. Later builds are incremental.

Videos plays videos with FFmpeg and dav1d, which the toolchain builds
(it needs `nasm`), with GCC for the image and Rust for Veda's target (with
CMake). Build it once, in about half an hour, and the images built after
it include the decoder:

```bash
cargo orchestration toolchain
```

```bash
cargo orchestration run --resolution 1280x800 --smp 4 --memory 2048
cargo orchestration run --cmdline "run=editor"
```

`run=NAME` starts `/system/bin/NAME.exe` after boot. The machine reaches
the Internet through a wired card on QEMU's NAT (`--net none` leaves it
out). `cargo orchestration help` lists every command and option.

### A real PC, from a USB stick

```bash
cargo orchestration iso
```

This writes `target/veda/veda.iso`, a live system. To start it:

1. Write it to a USB stick with [Rufus](https://rufus.ie) (partition
   scheme GPT, target system UEFI, ISO mode) or with any image writer.
2. In the PC's firmware settings, turn on VT-x and VT-d and turn off
   Secure Boot (Veda's loader is not signed).
3. Start the PC from the stick in UEFI mode, usually from the boot menu
   (F12, F11 or Esc at power-on).

Veda runs from memory and starts no disk driver, so it never reads or
writes the PC's disks. Its home directory lasts until the PC is turned
off. To try the stick in QEMU first, run `cargo orchestration run --live`.

The stick keeps a log of every start in `VEDA/LOGS`, one numbered folder
per start (the newest has the highest number): `VEDA.TXT` from Veda and
`LINUX.TXT` from the driver VM. They are synced every second, so even a
start that shows nothing can be diagnosed on another computer.

Linux's drivers in the driver VM support:

| Devices | Support |
|---------|---------|
| Display | The firmware's screen mode near 1920x1080, in the panel's own shape where the firmware reports it; every frame flipped whole at the vertical blank |
| GPU | Intel's integrated graphics (i915, with Mesa's iris) |
| Input | USB keyboards, mice and hubs on USB 3 (xHCI) controllers; PS/2 keyboards, mice and touchpads |
| Network | Intel's and Realtek's Ethernet, Intel's Wi-Fi, USB network adapters |
| Sound | HD Audio (speakers, headphones, line outputs, the codec's microphone), laptop amplifiers (Cirrus Logic CS35L41), HDMI and DisplayPort audio |
| Built-in microphones on an audio DSP | Intel's Tiger Lake to Raptor Lake (11th to 13th generation Core), through Sound Open Firmware; later ones are silent for now |

Without VT-x and VT-d, Veda drives only the disks (with VT-x alone, PS/2
keyboards and mice too). USB disks, the stick included, are left alone.
[Status](documentation/DRIVERVM.md#status) shows how far each device class
has come. To try USB input or HD Audio in QEMU:

```bash
cargo orchestration run --input usb
cargo orchestration run --sound hda
```

## Talking to Veda

1. Open **Settings → Agent** and add your Deepgram API key. The agent's
   name, voice, language model, listening and memory are on the same page.
2. Say **"Hey Veda"**, click the ring at the right of the taskbar or press
   **Super+Space**.
3. Talk naturally and interrupt whenever you like. Say goodbye when you
   are done, and it goes back to sleep.

Veda uses the PC's microphone and speakers, under QEMU too. It speaks
English only for now. Deepgram bills every minute of conversation; asleep,
only the short recognitions that check for its name are billed, at most
ten minutes an hour.

Things to ask:

* "Switch my wallpaper to the dunes."
* "Play Neon Horizon."
* "Write a shopping list with eggs, milk and coffee."
* "Show me the lighthouse photo, full screen."
* "Remind me to stretch in twenty minutes."
* "What's using the most memory?"
* "Start a quick chase at sunset in Interceptor."
* "Delete the City Lights photo." (It asks you first.)
* "What do you know about me?"

## Using the desktop

* Click the ring in the middle of the taskbar, or tap **Super**, for the
  start menu, and type to search.
* Click a taskbar button to open, focus or minimise an application;
  middle-click opens another window. Click the clock for the calendar.
* Double-click a desktop icon to open it. Right-click the desktop to
  change the wallpaper.
* Drag a window to the top of the screen to maximise it, or to a side to
  fill that half (or press **Super+Left/Right/Up/Down**). **Super+D**
  shows the desktop, **Alt+Tab** switches windows, **Alt+F4** closes one.
* Deleted items go to the **Trash**, the first desktop icon: drag desktop
  items onto it, or press **Delete** in Files (**Ctrl+Z** brings them
  back, **Shift+Delete** deletes for good). Open it to restore items
  where they were, or to empty it.
* The speaker icon on the taskbar sets the volume (scroll over it); the
  network icon next to it lists the Wi-Fi networks to join.

Your files live in `/home/user`, kept on `target/veda/home.img` across
restarts and rebuilds (`--fresh-home` starts over).

## Building agent-compatible applications

Agent support is part of the `vui` application platform. An application
describes its actions, reports its state and performs actions; `vui::run`
registers it with the agent service and answers the agent between frames:

```rust
use vui::agent::{self, Action, AppAgentInfo, Risk, Value, arg_path, arg_str, json, show_path};

impl vui::App for Notes {
    fn update(&mut self, ui: &mut vui::Ui) { /* ... */ }

    fn agent_info(&self) -> Option<AppAgentInfo> {
        Some(agent::info(
            "A notes app: one note per file in ~/Notes.",
            vec![
                Action::new("add_line", "Adds a line at the end of the note shown")
                    .param("text", "string", "The line", true)
                    .build(),
                Action::new("delete_note", "Deletes a note for good")
                    .param("path", "string", "The note's file, such as ~/Notes/Ideas.txt", true)
                    .risk(Risk::Destructive)
                    .build(),
            ],
        ))
    }

    fn agent_state(&self) -> Value {
        json!({ "note": self.title.as_str(), "lines": self.lines.len() })
    }

    fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        match action {
            "add_line" => { self.append(arg_str(args, "text")?); Ok(json!({ "lines": self.lines.len() })) }
            "delete_note" => {
                let path = arg_path(args, "path")?;
                self.delete(&path).map_err(|e| format!("{} could not be deleted: {e}", show_path(&path)))?;
                Ok(json!({ "deleted": show_path(&path) }))
            }
            other => Err(format!("Notes has no action called {other}")),
        }
    }
}
```

Write action descriptions for a language model: short, imperative, with
units and defaults spelled out. Declare each action's risk, name its
target explicitly (an approved action may run minutes later), and run it
through the same code as the keyboard and mouse. Consent, memory and the
conversation are the system's job, not the application's. See
[Making an application agent-compatible](documentation/AGENT.md#making-an-application-agent-compatible).

## Testing

```bash
cargo orchestration test
```

This runs the libraries' unit tests on the host, then boots Veda headless
with the `systest` integration tests (IPC, memory, threads, the file
system, crash recovery of the window system, C and C++ programs). Any
panic fails the run.

```bash
cargo orchestration test --ui
```

This adds the GUI scripts in `verification/ui/` (with
`verification/ui/arch/amd64/`, those that hold only on QEMU's PC) and the
agent's scripts in `verification/agent/`. They click through the desktop
and the applications, check logs and screenshots, and break things on
purpose (Linux in the driver VM crashes, a cable is pulled, services and
drivers are killed) to check that Veda recovers by itself. Scripts that
need GCC or the video decoder are skipped without the C toolchain.
Videos is also tested without it, with a stand-in decoder
(`verification/fakedec`) that draws its pictures and crashes, hangs or
falls behind when asked.

The agent's scripts run against a stand-in for Deepgram on the host, so
they are deterministic, offline and free. They cover conversations,
function calls, interruptions, echo, consent, waking by name over music
and every application's actions. `verification/real/` talks to the real
Deepgram (with the key in `DEEPGRAM_API_KEY`), using a synthetic voice in a
virtual microphone.

Networking has host tests of its own: 802.11 and its cryptography against
published test vectors, TCP/IP over a simulated cable, and TLS against a
rustls server and real certificate chains. Inside Veda, `nettest` checks
DNS, TCP, HTTP, HTTPS and ping.

To run one script, or to boot headless and save `target/veda/screen.png`:

```bash
cargo orchestration script verification/agent/wake-name.vts
cargo orchestration shot --wait 20
```

Every run saves the serial console (the kernel log and every program's
output) to `target/veda/serial.log`.

## Repository layout

| Path | Contents |
|------|----------|
| `implementation/services/agent/`, `implementation/libraries/agent/` | the agent: conversations, the name listener, functions, consent and memory; its logic (Deepgram's protocol, instructions, wake word, echo gate) is in `implementation/libraries/agent` |
| `implementation/boot/` | `vboot`, the UEFI bootloader |
| `implementation/kernel/` | `vkernel`, the microkernel, with the hypervisor and the IOMMU driver |
| `implementation/services/` | `init` (service registry, launcher, process identity), `vfs`, `devmgr` (PCI), `compositor`, `audio`, `agent`, `netd` (network), `wlan` (Wi-Fi), `drivervm` (the driver VM's monitor), `mediadec` (the sandboxed video decoder) |
| `implementation/drivers/` | Veda's own drivers: `virtio-blk`, `ahci` and `nvme` |
| `implementation/libraries/` | shared libraries: the system call ABI and runtime, IPC and service protocols, the POSIX layer, 2D graphics, fonts and images, the `vui` toolkit, OpenGL ES and GLSL, networking and TLS, audio, the agent's logic, and more |
| `implementation/applications/` | the desktop `shell` and the applications, each with its manifest for the start menu (`<name>.app`); what is testable on the host of Interceptor (its game), Videos and the Terminal is in `interceptor/game`, `videos/core` and `terminal/core` |
| `implementation/guest/` | the driver VM's programs: its `init`, Veda's drivers for Linux (`input`, `alsa`, `net`, `wifi`, `kms`), the OpenGL ES `renderer`, and `logkeeper` (the stick's logs) |
| `implementation/ports/` | upstream releases with Veda's patches: GCC, binutils, GMP, MPFR, MPC and musl for the C toolchain, Rust, FFmpeg and dav1d for videos, Linux for the driver VM, and the crates smoltcp (TCP/IP) and zune-jpeg (JPEG); and the fonts (Inter, Lato, JetBrains Mono), unmodified |
| `verification/` | `systest` and `nettest` (in-system tests), C and C++ test programs, `fakedec` (a stand-in video decoder), GUI and agent scripts |
| `generation/` | host programs that generate wallpapers, pictures, music and videos at build time |
| `orchestration/` | the build system: cross-compilation, disk images, QEMU, automation, the stand-in for Deepgram |
| `documentation/` | documentation, the README's screenshots, the icon and social preview (`documentation/icon/`), and [www.vedaos.org](https://www.vedaos.org), a static page (`documentation/website/`) |

## Documentation

* [The agent](documentation/AGENT.md): the voice agent, its functions,
  consent and memory, and how applications become agent-compatible.
* [Architecture](documentation/ARCHITECTURE.md): how the system fits
  together.
* [Networking](documentation/NETWORKING.md): the network and Wi-Fi
  services, security and tests.
* [C on Veda](documentation/C.md): writing, compiling and running C
  programs, the POSIX layer, and how the toolchain is built.
* [Rust on Veda](documentation/RUST.md): programs on Rust's standard
  library, how it works on Veda, and how Rust is built.
* [The driver VM](documentation/DRIVERVM.md): the hypervisor, the platform,
  the bridge, the IOMMU and the devices.
* [Developing](documentation/DEVELOPING.md): build commands, the system
  image, writing applications, performance notes.
* [Libraries](documentation/DEPENDENCIES.md): the libraries Veda is built
  on, and what it keeps of its own and why.
* [Coding conventions](documentation/CODING.md).

## License

Copyright © 2026 Vahid Mohammadi.

Veda is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License, version 3, as published by the
Free Software Foundation (see [LICENSE](LICENSE)). It is distributed in
the hope that it will be useful, but WITHOUT ANY WARRANTY; without even
the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
PURPOSE.

Every file of Veda's own names its copyright holders and its licence in
its first lines, as SPDX tags (see
[Licence headers](documentation/CODING.md#licence-headers)).

Other parts keep their own licences:

* **The driver VM's renderer**: its C code
  (`implementation/guest/renderer/decoder`,
  `implementation/guest/renderer/drm`), which Mesa's build compiles with
  Mesa, is under the MIT licence, as Mesa is.
* **Fonts** (Inter, Lato, JetBrains Mono): the SIL Open Font License 1.1;
  see `implementation/ports/fonts/`.
* **Device firmware** that the driver VM's Linux loads into devices
  (`implementation/ports/linux/firmware.txt`) runs on the devices, not in
  Veda. It comes unmodified from the Linux firmware collection, under the
  terms its `WHENCE` gives each file's maker (who allow its redistribution),
  and from Sound Open Firmware's releases (sof-bin: Intel's signed builds of
  the audio DSP's firmware, under the BSD licence and Intel's patent
  licence in its `LICENCE.Intel`).
* **The crates Veda changes**: smoltcp under the 0BSD licence, and
  zune-jpeg under the MIT, Apache 2.0 or zlib licence, at the choice of
  whoever uses it. Their source is the releases that
  `implementation/ports/smoltcp` and `implementation/ports/zune-jpeg` name,
  with the patches beside them.
* **Rust's standard library**, in the programs an image may contain that
  use it (its tests): the MIT or Apache 2.0 licence, at the choice of
  whoever uses it. Its source is the release that
  `implementation/ports/rust` names, with the patches beside it.
* **The C toolchain** an image may contain: GCC and binutils under the GPL
  version 3 or later (the GCC runtime library with its Runtime Library
  Exception, so programs GCC builds keep their own licences), GMP, MPFR
  and MPC under the LGPL version 3 or later, and musl under the MIT
  licence. Their source is the upstream releases that
  `implementation/ports/*/port.toml` names, with the patches beside them.
* **The video decoder** an image may contain: FFmpeg under the LGPL
  version 2.1 or later (built without its GPL and non-free parts, which
  the build checks) and dav1d under the BSD 2-clause licence, from the
  releases `implementation/ports/ffmpeg` and `implementation/ports/dav1d`
  name. Some formats it decodes (H.264, HEVC, AAC) are covered by patents in
  some countries; whoever distributes Veda there is responsible for any
  licences they need.
