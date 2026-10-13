#!/bin/bash
# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Builds the Linux kernel of Veda's driver VM from the port in this
# directory. `cargo xtask linux` runs it after downloading and verifying the
# sources (see xtask/src/linux.rs and documentation/DRIVERVM.md).
#
# Environment:
#   VEDA_PORTS   the ports directory
#   VEDA_ARCH    the architecture built for (../arch/$VEDA_ARCH/settings.sh)
#   VEDA_LINUX   where to build; downloads/ holds the verified archives
#   VEDA_RENDERER      the renderer (implementation/guest/renderer), whose
#                decoder and libdrm Mesa's build compiles
#   VEDA_RENDERER_LIB  its Rust half, which Mesa's build links the program
#                around
#   JOBS         parallel jobs (default: the number of processors)
#
# Results, under $VEDA_LINUX:
#   kernel           the kernel (the architecture's image of it)
#   toolchain/       a cross toolchain for the guest's programs in C and C++
#                    ($GUEST-gcc, -g++ ...), with their sysroot
#   build/mesa/      Mesa built with it for the guest's renderer
#                    (src/gallium/targets/veda/libvrenderer.a)
#
# The kernel's build tool objtool needs libelf, which few machines have the
# headers of: zlib and elfutils' libelf are built for it first, static,
# into build/host-deps.

set -euo pipefail

PORTS=${VEDA_PORTS:?}
VEDA_ARCH=${VEDA_ARCH:?}
ROOT=${VEDA_LINUX:?}
RENDERER=${VEDA_RENDERER:?}
RENDERER_LIB=${VEDA_RENDERER_LIB:?}
JOBS=${JOBS:-$(nproc)}

DOWNLOADS=$ROOT/downloads
SRC=$ROOT/src
BUILD=$ROOT/build
STAMPS=$BUILD/stamps
DEPS=$BUILD/host-deps

mkdir -p "$SRC" "$BUILD" "$STAMPS"

# log, port, patches, stage, quiet and unpack.
source "$PORTS/common.sh"
# GUEST and what the ports take from the architecture.
source "$PORTS/arch/$VEDA_ARCH/settings.sh"

stage_src_zlib() { unpack zlib; }
stage src_zlib +zlib
stage_src_elfutils() { unpack elfutils; }
stage src_elfutils +elfutils
stage_src_linux() { unpack linux; }
stage src_linux +linux

stage_zlib() {
	local b=$BUILD/zlib
	rm -rf "$b" && cp -r "$SRC/zlib" "$b" && cd "$b"
	quiet configure.log ./configure --static --prefix="$DEPS"
	quiet make.log make -j"$JOBS"
	quiet install.log make install
}
stage zlib src_zlib @DEPS

# A pkg-config that knows only libelf, as built here: elfutils' configure
# insists on finding one, and the kernel asks it for libelf's flags.
pkgconfig() {
	mkdir -p "$DEPS/bin"
	cat > "$DEPS/bin/pkg-config" <<-END
	#!/bin/sh
	for a; do case "\$a" in
	--version) echo 0.29.2; exit 0;;
	--atleast-pkgconfig-version) exit 0;;
	--cflags) echo "-I$DEPS/include"; exit 0;;
	--libs) echo "-L$DEPS/lib -lelf -lz"; exit 0;;
	esac; done
	exit 1
	END
	chmod +x "$DEPS/bin/pkg-config"
}

stage_libelf() {
	local b=$BUILD/elfutils
	pkgconfig
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env PATH="$DEPS/bin:$PATH" "$SRC/elfutils/configure" --prefix="$DEPS" \
		--disable-debuginfod --disable-libdebuginfod --disable-nls --disable-demangler \
		--without-bzlib --without-lzma --without-zstd \
		CPPFLAGS="-I$DEPS/include" LDFLAGS="-L$DEPS/lib"
	quiet make-lib.log make -j"$JOBS" -C lib
	quiet make-libelf.log make -j"$JOBS" -C libelf libelf.a
	cp libelf/libelf.a "$DEPS/lib/"
	cp "$SRC/elfutils/libelf/libelf.h" "$SRC/elfutils/libelf/gelf.h" "$SRC/elfutils/libelf/nlist.h" "$DEPS/include/"
}
stage libelf src_elfutils zlib @DEPS

# Everything off but what the driver VM needs (veda.config, and the
# architecture's: arch/$VEDA_ARCH/veda.config), all of which must take:
# Kconfig leaves out an option it does not know, or whose dependencies are
# off, without a word.
stage_config() {
	cat "$PORTS/linux/veda.config" "$PORTS/linux/arch/$VEDA_ARCH/veda.config" > "$BUILD/veda.config"
	quiet "$BUILD/config.log" make -C "$SRC/linux" O="$BUILD/linux" ARCH="$LINUX_ARCH" \
		KCONFIG_ALLCONFIG="$BUILD/veda.config" allnoconfig
	local line missing=""
	while IFS= read -r line; do
		case $line in
		CONFIG_*=n) if grep -q "^${line%=n}=" "$BUILD/linux/.config"; then missing="$missing $line"; fi ;;
		CONFIG_*=*) if ! grep -qxF "$line" "$BUILD/linux/.config"; then missing="$missing $line"; fi ;;
		esac
	done < "$BUILD/veda.config"
	if [ -n "$missing" ]; then
		echo "veda.config: options that did not take (unknown, or their dependencies off):$missing" >&2
		exit 1
	fi
}
stage config src_linux "$PORTS/linux/veda.config" "$PORTS/linux/arch/$VEDA_ARCH/veda.config" @LINUX_ARCH

# --- the guest's C and C++ toolchain -------------------------------------------

# The guest's programs in C and C++ (Mesa, for the renderer) are built by a
# cross toolchain for Linux on musl, from the releases Veda's own is built
# from (ports/binutils, gcc and musl) as their authors released them: Veda's
# patches add Veda's target, and the guest's (GUEST) is upstream's own.
# Programs link statically, position independent (-static-pie, as Rust
# links them).
TOOLS=$ROOT/toolchain
SYSROOT=$TOOLS/sysroot
export PATH="$TOOLS/bin:$PATH"

for p in binutils gcc musl; do
	eval "stage_src_$p() { unpack $p upstream; }"
	stage "src_$p" "$PORTS/$p/port.toml"
done

stage_binutils() {
	local b=$BUILD/binutils
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/binutils/configure" --target=$GUEST --prefix="$TOOLS" --with-sysroot="$SYSROOT" \
		--disable-nls --disable-werror --disable-gdb --disable-gdbserver --disable-sim --disable-readline \
		--disable-libdecnumber --disable-gprofng --disable-plugins --disable-libctf --disable-multilib \
		--enable-deterministic-archives MAKEINFO=true
	quiet make.log make -j"$JOBS" MAKEINFO=true
	quiet install.log make install MAKEINFO=true
}
stage binutils src_binutils @TOOLS @GUEST

# GCC's limits.h wraps the C library's only if it sees it: the headers
# come first.
stage_musl_headers() {
	local b=$BUILD/musl-headers
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet install.log make -f "$SRC/musl/Makefile" srcdir="$SRC/musl" ARCH="$MUSL_ARCH" prefix=/usr \
		DESTDIR="$SYSROOT" install-headers
}
stage musl_headers src_musl @TOOLS @MUSL_ARCH

GCC_OPTIONS=(
	--disable-nls --disable-multilib --disable-shared
	--enable-threads=posix --enable-tls --enable-initfini-array --enable-default-pie
	--disable-libssp --disable-libquadmath --disable-libgomp --disable-libatomic
	--disable-libsanitizer --disable-libvtv --disable-libitm --disable-lto
	--disable-plugin --disable-libstdcxx-pch --without-isl --without-zstd --disable-bootstrap
	"CFLAGS_FOR_TARGET=-g -O2 -fPIE" "CXXFLAGS_FOR_TARGET=-g -O2 -fPIE"
)

stage_gcc() {
	local b=$BUILD/gcc
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/gcc/configure" --target=$GUEST --prefix="$TOOLS" --with-sysroot="$SYSROOT" \
		--enable-languages=c,c++ "${GCC_OPTIONS[@]}" MAKEINFO=true
	quiet make-gcc.log make -j"$JOBS" all-gcc MAKEINFO=true
	quiet install-gcc.log make install-gcc MAKEINFO=true
	quiet make-libgcc.log make -j"$JOBS" all-target-libgcc MAKEINFO=true
	quiet install-libgcc.log make install-target-libgcc MAKEINFO=true
}
stage gcc src_gcc binutils musl_headers @GCC_OPTIONS @TOOLS

stage_musl() {
	local b=$BUILD/musl
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC=$GUEST-gcc "$SRC/musl/configure" --target=$GUEST --prefix=/usr \
		--disable-shared --enable-static CFLAGS=-fPIE
	quiet make.log make -j"$JOBS"
	quiet install.log make DESTDIR="$SYSROOT" install
}
stage musl src_musl gcc

# Linux's interface for programs (its UAPI headers: linux/types.h, the
# DRM's, ...), from the kernel the guest runs.
stage_linux_headers() {
	quiet "$BUILD/headers.log" make -C "$SRC/linux" O="$BUILD/linux-headers" ARCH="$LINUX_ARCH" \
		INSTALL_HDR_PATH="$SYSROOT/usr" headers_install
}
stage linux_headers src_linux musl_headers @TOOLS @LINUX_ARCH

# The C++ library checks the C library by linking programs with it, so it
# comes once that is complete. Rust's programs ask for an unwinder library
# of their own (libunwind, LLVM's); GCC's unwinder is in libgcc.a, which
# every program links, so an empty library stands for it: a program with
# C++ in it has one unwinder.
stage_libstdcxx() {
	cd "$BUILD/gcc"
	quiet make-libstdcxx.log make -j"$JOBS" all-target-libstdc++-v3 MAKEINFO=true
	quiet install-libstdcxx.log make install-target-libstdc++-v3 MAKEINFO=true
	rm -f "$SYSROOT/usr/lib/libunwind.a"
	$GUEST-ar rc "$SYSROOT/usr/lib/libunwind.a"
}
stage libstdcxx gcc musl

# --- Mesa, for the renderer -------------------------------------------------------

# The guest's renderer (implementation/guest/renderer) carries out the
# OpenGL ES command streams of Veda's applications on Mesa's Gallium
# drivers, over the GPU's Linux driver: softpipe (no GPU), virgl (QEMU's
# virtio-gpu), iris (Intel's GPUs). The stage configures Mesa's build for
# the guest, and for this machine the same decoder as a library
# (vgallium.so) on softpipe, on virgl over virglrenderer's test server, and
# on iris (this machine's Intel GPU, if it has one), which the OpenGL ES
# tests use. `cargo xtask` builds them (ninja) when it needs them.
stage_src_mesa() { unpack mesa; }
stage src_mesa +mesa

MESA_OPTIONS=(
	--buildtype=release -Ddefault_library=static -Dvulkan-drivers=
	-Dplatforms= -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Degl=disabled -Dglx=disabled
	-Dgbm=disabled -Dllvm=disabled -Dmesa-clc=auto -Dzlib=disabled -Dzstd=disabled -Dexpat=disabled
	-Dxmlconfig=disabled -Dshader-cache=disabled -Dbuild-tests=false -Dvalgrind=disabled
	-Dlibunwind=disabled -Dgallium-va=disabled -Dmicrosoft-clc=disabled -Dvideo-codecs=
	-Dintel-elk=false
)

stage_mesa() {
	cat > "$BUILD/mesa.cross" <<-EOF
	[binaries]
	c = '$TOOLS/bin/$GUEST-gcc'
	cpp = '$TOOLS/bin/$GUEST-g++'
	ar = '$TOOLS/bin/$GUEST-ar'
	strip = '$TOOLS/bin/$GUEST-strip'
	pkg-config = 'false'
	[host_machine]
	system = 'linux'
	cpu_family = '$MESON_CPU_FAMILY'
	cpu = '$MESON_CPU'
	endian = 'little'
	EOF
	rm -rf "$BUILD/mesa" "$BUILD/mesa-host"
	quiet "$BUILD/mesa-setup.log" meson setup "$BUILD/mesa" "$SRC/mesa" --cross-file "$BUILD/mesa.cross" \
		"${MESA_OPTIONS[@]}" -Dveda-renderer="$RENDERER" -Dveda-renderer-lib="$RENDERER_LIB" \
		-Dgallium-drivers=softpipe,virgl,iris
	quiet "$BUILD/mesa-host-setup.log" meson setup "$BUILD/mesa-host" "$SRC/mesa" \
		"${MESA_OPTIONS[@]}" -Dveda-renderer="$RENDERER" -Dgallium-drivers=softpipe,virgl,iris
}
stage mesa src_mesa libstdcxx linux_headers @MESA_OPTIONS @RENDERER @RENDERER_LIB @MESON_CPU_FAMILY @MESON_CPU

# make knows what changed.
log Building linux
quiet "$BUILD/linux.log" make -C "$SRC/linux" O="$BUILD/linux" ARCH="$LINUX_ARCH" \
	HOSTPKG_CONFIG="$DEPS/bin/pkg-config" -j"$JOBS" "$LINUX_IMAGE"
cp "$BUILD/linux/$LINUX_IMAGE_PATH" "$ROOT/kernel"
log Finished "Linux $(port linux version) for the driver VM ($ROOT/kernel)"
