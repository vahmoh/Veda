# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# The ports on AMD64 (x86-64): what the build scripts (build.sh and
# linux/build.sh) take from the architecture they build for. The ports'
# own changes for it are in <port>/architectures/amd64/.

# Veda's C target, which the binutils, GCC and musl ports add.
TARGET=x86_64-veda
# The driver VM's: Linux's own, on musl.
GUEST=x86_64-linux-musl

# Veda's Rust target, which the rust port adds, and LLVM's name for the
# architecture's code generator, the only one Rust's compiler is built
# with.
RUST_TARGET=x86_64-unknown-veda
RUST_LLVM_TARGETS=X86

# musl's name for the architecture (its arch/ directory).
MUSL_ARCH=x86_64

# Meson's names for the processor, in cross files.
MESON_CPU_FAMILY=x86_64
MESON_CPU=x86_64

# The assembler of dav1d's x86 code, in its cross file.
DAV1D_BINARIES="nasm = 'nasm'"

# FFmpeg's options for the architecture (its x86 code, which nasm
# assembles), and what its configured build must have of it.
FFMPEG_ARCH_OPTIONS=(--arch=x86_64 --x86asmexe=nasm)
FFMPEG_ARCH_DEFINES=("HAVE_X86ASM 1" "HAVE_AVX2_EXTERNAL 1")

# Linux's name for the architecture, and the kernel image its build makes
# (the make target, and where it leaves it).
LINUX_ARCH=x86_64
LINUX_IMAGE=bzImage
LINUX_IMAGE_PATH=arch/x86/boot/bzImage
