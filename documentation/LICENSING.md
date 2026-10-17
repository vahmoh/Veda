<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Licensing

Copyright © 2026 Vahid Mohammadi.

Veda is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License, version 3, as published by the
Free Software Foundation (see [LICENSE](../LICENSE)). It is distributed in
the hope that it will be useful, but WITHOUT ANY WARRANTY; without even
the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
PURPOSE.

Every file of Veda's own names its copyright holders and its licence in
its first lines, as SPDX tags (see
[Licence headers](CODING.md#licence-headers)).

## Other parts

Other parts keep their own licences:

| Part | Licence | Source |
|------|---------|--------|
| The driver VM's renderer: its C code, which Mesa's build compiles with Mesa | The MIT licence, as Mesa is | `implementation/guests/linux/renderer/decoder` and `implementation/guests/linux/renderer/drm` |
| Fonts (Inter, Lato, JetBrains Mono) | The SIL Open Font License 1.1 | `implementation/ports/fonts/` |
| Device firmware that the driver VM's Linux loads into devices (`implementation/ports/linux/firmware.txt` and `implementation/ports/linux/arch/amd64/firmware.txt`); it runs on the devices, not in Veda | From the Linux firmware collection, the terms its `WHENCE` gives each file's maker (who allow its redistribution); from Sound Open Firmware's releases (sof-bin: Intel's signed builds of the audio DSP's firmware), the BSD licence and Intel's patent licence in its `LICENCE.Intel` | The Linux firmware collection and Sound Open Firmware's releases, unmodified |
| The crates Veda changes | smoltcp: the 0BSD licence; zune-jpeg: the MIT, Apache 2.0 or zlib licence, at the choice of whoever uses it | The releases that `implementation/ports/smoltcp` and `implementation/ports/zune-jpeg` name, with the patches beside them |
| Rust's standard library, in the programs built on it (the applications, the voice agent and tests) | The MIT or Apache 2.0 licence, at the choice of whoever uses it | The release that `implementation/ports/rust` names, with the patches beside it |
| The C toolchain an image may contain | GCC and binutils: the GPL version 3 or later (the GCC runtime library with its Runtime Library Exception, so programs GCC builds keep their own licences); GMP, MPFR and MPC: the LGPL version 3 or later; musl: the MIT licence | The upstream releases that `implementation/ports/*/port.toml` names, with the patches beside them |
| The video decoder an image may contain | FFmpeg: the LGPL version 2.1 or later (built without its GPL and non-free parts, which the build checks); dav1d: the BSD 2-clause licence | The releases `implementation/ports/ffmpeg` and `implementation/ports/dav1d` name, with FFmpeg's patch beside it |
| The other ports: the driver VM's Linux kernel, Mesa, and the crates ported to Veda's target | Their own licences, which [the ports' README](../implementation/ports/README.md) lists for each (the Linux kernel: the GPL version 2 only; Mesa: the MIT licence, for the parts the renderer uses) | The releases their `port.toml` files name, with the patches beside them |

Some formats the video decoder decodes (H.264, HEVC, AAC) are covered by
patents in some countries; whoever distributes Veda there is responsible
for any licences they need.
