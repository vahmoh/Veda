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
