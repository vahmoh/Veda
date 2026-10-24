# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# How the ports' C and C++ libraries are built for Veda, static and
# position-independent, for the programs that link them: what the web
# engine's build (wpewebkit/build.sh) and Python's (python/build.sh) share.
# They source it after common.sh and the architecture's settings, having
# set ROOT (the build's directory), SRC, BUILD, STAMPS, HOST (the tools for
# this machine), SYSTEM (the libraries for Veda, as /system would have
# them: include/, lib/ and its pkg-config files, share/), CROSS (the
# toolchain's cross compilers and C library) and JOBS. It gives them:
#
#   VEDA_CFLAGS      what every library for Veda is compiled with
#   CC_VEDA, CXX_VEDA
#                    Veda's C and C++ compilers
#   BUILD_TRIPLE     this machine, as Autoconf names it
#   HOST_SRC         where the tools for this machine are unpacked
#   host_pkgconf     a stage: pkgconf for this machine, and
#                    $TARGET-pkg-config, which finds the libraries in SYSTEM
#   CMAKE_TOOLCHAIN  CMake's toolchain file for Veda, and CMAKE_MODULES,
#                    the copy of cmake/ (CMake's description of Veda) it uses
#   cmake_veda, autoconf_veda
#                    build a library with CMake or Autoconf into SYSTEM

if [ ! -x "$CROSS/bin/$TARGET-gcc" ]; then
	echo "no cross toolchain in $CROSS: build it with \`cargo automation toolchain\`" >&2
	exit 1
fi

BUILD_TRIPLE=$(gcc -dumpmachine)

# What Veda gets of these libraries names their sources from the build's
# directory, never where that is on this machine (assertions' file names).
PREFIX_MAP="-ffile-prefix-map=$ROOT=."
# Position-independent, for programs linked -static-pie; with stack
# protection and stack clash protection, as befits code that parses what
# web sites and files bring; each function and datum in a section of its
# own, so that a program's link drops what it does not use.
VEDA_CFLAGS="-O2 -g0 -fPIE -fstack-protector-strong -fstack-clash-protection -ffunction-sections -fdata-sections $PREFIX_MAP"

CC_VEDA=$CROSS/bin/$TARGET-gcc
CXX_VEDA=$CROSS/bin/$TARGET-g++

# --- tools for this machine ---------------------------------------------------

# Each built from its port as released (so it depends on the release
# alone, not on Veda's patches), into host/: what the builds run, whatever
# this machine has installed. Their sources are unpacked apart (src/host),
# as some of the same ports are built for Veda with Veda's patches.
HOST_SRC=$SRC/host
mkdir -p "$HOST_SRC"

stage_host_pkgconf() {
	local SRC=$HOST_SRC
	unpack pkgconf upstream
	local b=$BUILD/pkgconf-host
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	# What it leaves out of flags as the compiler's own directories: this
	# machine's, not its own prefix (where the host's libraries below are).
	quiet configure.log "$SRC/pkgconf/configure" --prefix="$HOST" --disable-shared --enable-static \
		--with-system-includedir=/usr/include --with-system-libdir=/usr/lib:/lib
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	# pkg-config for Veda: the libraries below, under system/ (their
	# files name /system, which the sysroot moves here), with what each
	# needs to link statically.
	cat > "$HOST/bin/$TARGET-pkg-config" <<-EOF
	#!/bin/sh
	PKG_CONFIG_SYSROOT_DIR='$ROOT' \\
	PKG_CONFIG_LIBDIR='$SYSTEM/lib/pkgconfig:$SYSTEM/share/pkgconfig' \\
	PKG_CONFIG_SYSTEM_INCLUDE_PATH=/system/include \\
	PKG_CONFIG_SYSTEM_LIBRARY_PATH=/system/lib \\
	exec '$HOST/bin/pkgconf' --static "\$@"
	EOF
	chmod +x "$HOST/bin/$TARGET-pkg-config"
	ln -sf pkgconf "$HOST/bin/pkg-config"
}
stage host_pkgconf "$PORTS/pkgconf/port.toml" @ROOT @TARGET

# --- how the libraries are built for Veda --------------------------------------

# CMake's description of Veda (a toolchain file), for the libraries built
# with CMake: the system is Veda (cmake/Platform/Veda.cmake, beside this
# script), what it finds is under system/ and the C library's sysroot.
# CMake reads the system's description from a copy in the build, so that
# the toolchain file names no checkout: every library's fingerprint has
# it, and their build trees outlast the checkout that configured them.
CMAKE_MODULES=$BUILD/cmake
rm -rf "$CMAKE_MODULES"
cp -r "$PORTS/cmake" "$CMAKE_MODULES"
CMAKE_TOOLCHAIN=$BUILD/veda.cmake
cat > "$CMAKE_TOOLCHAIN" <<EOF
set(CMAKE_SYSTEM_NAME Veda)
set(CMAKE_SYSTEM_PROCESSOR $MESON_CPU)
list(APPEND CMAKE_MODULE_PATH "$CMAKE_MODULES")
set(CMAKE_C_COMPILER "$CC_VEDA")
set(CMAKE_CXX_COMPILER "$CXX_VEDA")
set(CMAKE_AR "$CROSS/bin/$TARGET-ar" CACHE FILEPATH "")
set(CMAKE_RANLIB "$CROSS/bin/$TARGET-ranlib" CACHE FILEPATH "")
set(CMAKE_NM "$CROSS/bin/$TARGET-nm" CACHE FILEPATH "")
set(CMAKE_STRIP "$CROSS/bin/$TARGET-strip" CACHE FILEPATH "")
set(CMAKE_C_FLAGS_INIT "$VEDA_CFLAGS")
set(CMAKE_CXX_FLAGS_INIT "$VEDA_CFLAGS")
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_FIND_ROOT_PATH "$ROOT" "$CROSS/sysroot")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(PKG_CONFIG_EXECUTABLE "$HOST/bin/$TARGET-pkg-config" CACHE FILEPATH "")
set(BUILD_SHARED_LIBS OFF CACHE BOOL "")
EOF

# CMake for Veda: library NAME, configured with OPTIONS and installed into
# system/.
cmake_veda() {
	local name=$1 b=$BUILD/$1
	shift
	rm -rf "$b"
	quiet "$b-configure.log" cmake -S "$SRC/$name" -B "$b" -G Ninja --toolchain "$CMAKE_TOOLCHAIN" \
		-DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/system -DCMAKE_INSTALL_LIBDIR=lib "$@"
	quiet "$b-build.log" ninja -C "$b" -j"$JOBS"
	quiet "$b-install.log" env DESTDIR="$ROOT" ninja -C "$b" install
}

# Autoconf for Veda: library NAME (its configure script SCRIPT, relative to
# its source), configured with OPTIONS and installed into system/. The
# library's config.sub knows Veda (its port's patch).
autoconf_veda() {
	local name=$1 script=$2 b=$BUILD/$1
	shift 2
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC="$CC_VEDA" CXX="$CXX_VEDA" AR="$TARGET-ar" RANLIB="$TARGET-ranlib" \
		CFLAGS="$VEDA_CFLAGS" CXXFLAGS="$VEDA_CFLAGS" PKG_CONFIG="$TARGET-pkg-config" \
		"$SRC/$name/$script" --host=$TARGET --build="$BUILD_TRIPLE" --prefix=/system --libdir=/system/lib \
		--disable-shared --enable-static "$@"
	quiet make.log make -j"$JOBS"
	quiet install.log make install DESTDIR="$ROOT"
}
