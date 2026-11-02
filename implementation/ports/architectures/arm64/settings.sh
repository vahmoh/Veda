# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# The ports on ARM64 (AArch64): what the build scripts (build.sh and
# linux/build.sh) take from the architecture they build for. The ports'
# own changes for it are in <port>/architectures/arm64/.

# Veda's C target, which the binutils, GCC and musl ports add.
TARGET=aarch64-veda
# The driver VM's: Linux's own, on musl.
GUEST=aarch64-linux-musl

# Veda's Rust target, which the rust port adds, and LLVM's names for the
# architecture's code generator and for the build machine's, as the
# compiler also builds what runs while Veda is built (build scripts,
# procedural macros).
RUST_TARGET=aarch64-unknown-veda
RUST_LLVM_TARGETS="AArch64;X86"

# musl's name for the architecture (its arch/ directory).
MUSL_ARCH=aarch64

# Meson's names for the processor, in cross files.
MESON_CPU_FAMILY=aarch64
MESON_CPU=aarch64

# The assembler of dav1d's Arm code, in its cross file: GCC's, which it
# finds itself.
DAV1D_BINARIES=""

# FFmpeg's options for the architecture (its Arm code, which GCC's
# assembler assembles), and what its configured build must have of it.
FFMPEG_ARCH_OPTIONS=(--arch=aarch64)
FFMPEG_ARCH_DEFINES=("HAVE_NEON 1")

# libjpeg-turbo's options for the architecture: none, as its Arm code is C
# with Neon's intrinsics.
LIBJPEG_TURBO_ARCH_OPTIONS=()

# Mesa's drivers: softpipe for tests, virgl for QEMU's virtio-gpu. Its
# drivers of the GPUs ARM64 machines have (Mali's, Adreno's) go in with
# their Linux drivers and firmware.
MESA_GALLIUM_DRIVERS=softpipe,virgl

# OpenSSL's name for the architecture, in its target for Veda
# (veda-$OPENSSL_ARCH, which the openssl port adds): its assembly for it.
OPENSSL_ARCH=aarch64

# Linux's name for the architecture, and the kernel image its build makes
# (the make target, and where it leaves it).
LINUX_ARCH=arm64
LINUX_IMAGE=Image
LINUX_IMAGE_PATH=arch/arm64/boot/Image
# What comes before its compiler's name: the guest's toolchain compiles it.
LINUX_CROSS_COMPILE=$GUEST-
