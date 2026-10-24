#!/bin/bash
# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Builds Python for Veda from the ports: CPython's interpreter with its
# standard library's modules built in, and the C libraries they stand on,
# static and position-independent; and the tools for this machine its
# build runs (pkgconf, and the same release of Python, which a cross build
# of CPython runs its own scripts with). `cargo automation python` runs it
# after downloading and verifying the sources; see
# automation/source/python.rs and documentation/PYTHON.md.
#
# Environment:
#   VEDA_PORTS       implementation/ports
#   VEDA_ARCH        the architecture built for
#                    (architectures/$VEDA_ARCH/settings.sh)
#   VEDA_PYTHON      where to build; downloads/ holds the verified archives
#   VEDA_TOOLCHAIN   Veda's toolchain (its cross/ compilers and sysroot),
#                    which `cargo automation toolchain` builds
#   JOBS             parallel jobs (default: the number of processors)
#
# Results, under $VEDA_PYTHON:
#   host/            the tools for this machine: pkgconf (and
#                    $TARGET-pkg-config, which finds the libraries below),
#                    zlib, and Python
#   system/          the libraries for Veda, as /system would have them:
#                    include/ and lib/ (static archives and their pkg-config
#                    files)
#   python/system/   Python for Veda as the image installs it in /system:
#                    bin/python3, and lib/python3.15, the standard library
#                    with its bytecode, but for its tests
#   tests/system/    the standard library's tests (lib/python3.15/test),
#                    which images for testing add
#   licences/        the licence texts of Python and of the libraries it
#                    has built in, which the image installs in
#                    /system/licences
#
# Every stage keeps a fingerprint of what went into it and is skipped while
# that is unchanged, as in build.sh.

set -euo pipefail

PORTS=${VEDA_PORTS:?}
VEDA_ARCH=${VEDA_ARCH:?}
ROOT=${VEDA_PYTHON:?}
TOOLCHAIN=${VEDA_TOOLCHAIN:?}
JOBS=${JOBS:-$(nproc)}

DOWNLOADS=$ROOT/downloads
SRC=$ROOT/src
BUILD=$ROOT/build
STAMPS=$BUILD/stamps
HOST=$ROOT/host
SYSTEM=$ROOT/system
CROSS=$TOOLCHAIN/cross

mkdir -p "$SRC" "$BUILD" "$STAMPS" "$HOST/bin" "$SYSTEM"
export PATH="$HOST/bin:$CROSS/bin:$PATH"
# The build's own languages and tools, whatever this machine's are set to;
# no Python of this machine's settings reaches the Pythons built here.
export LC_ALL=C.UTF-8
unset PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR CFLAGS CXXFLAGS LDFLAGS CPPFLAGS
for variable in $(compgen -e); do
	case "$variable" in PYTHON* | _PYTHON*) unset "$variable" ;; esac
done

# log, port, archive, patches, stage, quiet and unpack.
source "$PORTS/common.sh"
# TARGET and what the ports take from the architecture.
source "$PORTS/architectures/$VEDA_ARCH/settings.sh"
# VEDA_CFLAGS, the compilers, pkgconf (host_pkgconf), CMake's toolchain
# file, cmake_veda and autoconf_veda: how libraries are built for Veda.
source "$PORTS/libraries.sh"

# Python's version as its library's directory names it (3.15).
PYTHON_VERSION=$(port python version)
PYTHON_VERSION=${PYTHON_VERSION%.*}

# --- tools for this machine ---------------------------------------------------

# Python, the release Veda's is: a cross build of CPython runs its scripts
# (its code generators, freezing modules, compiling the library) with the
# same release. With zlib, which the bundled pip's wheel is compressed with.
stage_host_python() {
	local SRC=$HOST_SRC
	local b
	unpack zlib upstream
	b=$BUILD/zlib-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/zlib/configure" --prefix="$HOST" --static
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	unpack python upstream
	b=$BUILD/python-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/python/configure" --prefix="$HOST" --without-ensurepip --disable-test-modules \
		ZLIB_CFLAGS="-I$HOST/include" ZLIB_LIBS="-L$HOST/lib -lz"
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	"$HOST/bin/python$PYTHON_VERSION" -c 'import zlib'
}
stage host_python "$PORTS/zlib/port.toml" "$PORTS/python/port.toml" @ROOT @PYTHON_VERSION

# --- the libraries, for Veda ----------------------------------------------------

# How the libraries are built for Veda, which every library's build
# depends on: the toolchain file, CMake's description of the system,
# the helpers of libraries.sh, and the flags.
declare -f cmake_veda autoconf_veda > "$BUILD/helpers.sh"
CONFIG_FILES=("$CMAKE_TOOLCHAIN" "$CMAKE_MODULES/Platform/Veda.cmake" "$BUILD/helpers.sh")

# The stages of library PORT: its source as Veda patches it (`src_NAME`),
# then its build (`NAME`, with the stage function stage_NAME), which also
# depends on the stages given and on how libraries are built for Veda. NAME
# is PORT with dashes as underscores. LIBRARIES lists them, for their
# licences.
LIBRARIES=()
library() {
	local port=$1 name=${1//-/_}
	shift
	LIBRARIES+=("$port")
	eval "stage_src_$name() { unpack $port; }"
	stage "src_$name" "+$port"
	stage "$name" "src_$name" host_pkgconf "$@" "${CONFIG_FILES[@]}" @VEDA_CFLAGS @ROOT @TOOLCHAIN @TARGET
}

# zlib (the `zlib` module, and binascii's CRCs).
stage_zlib() {
	local b=$BUILD/zlib
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CHOST=$TARGET CC="$CC_VEDA" CFLAGS="$VEDA_CFLAGS" "$SRC/zlib/configure" \
		--prefix=/system --static
	quiet make.log make -j"$JOBS" libz.a
	quiet install.log make install DESTDIR="$ROOT"
}
library zlib

# bzip2's library (`bz2`), which its Makefile builds in its source's
# directory, from a copy; it has no pkg-config file of its own.
stage_bzip2() {
	local b=$BUILD/bzip2
	rm -rf "$b" && cp -r "$SRC/bzip2" "$b" && cd "$b"
	quiet make.log make -j"$JOBS" libbz2.a CC="$CC_VEDA" AR="$TARGET-ar" RANLIB="$TARGET-ranlib" \
		CFLAGS="$VEDA_CFLAGS"
	mkdir -p "$SYSTEM/include" "$SYSTEM/lib/pkgconfig"
	install -m 644 bzlib.h "$SYSTEM/include/"
	install -m 644 libbz2.a "$SYSTEM/lib/"
	cat > "$SYSTEM/lib/pkgconfig/bzip2.pc" <<-EOF
	prefix=/system
	libdir=\${prefix}/lib
	includedir=\${prefix}/include

	Name: bzip2
	Description: The bzip2 compression library
	Version: $(port bzip2 version)
	Libs: -L\${libdir} -lbz2
	Cflags: -I\${includedir}
	EOF
}
library bzip2

# liblzma alone (`lzma`): none of XZ Utils' programs, scripts, translations
# or documents.
stage_xz() {
	cmake_veda xz -DXZ_TOOL_XZ=OFF -DXZ_TOOL_XZDEC=OFF -DXZ_TOOL_LZMADEC=OFF -DXZ_TOOL_LZMAINFO=OFF \
		-DXZ_TOOL_SCRIPTS=OFF -DXZ_NLS=OFF -DXZ_DOC=OFF -DBUILD_TESTING=OFF
}
library xz

# Zstandard's library (`compression.zstd`), with its compressor's worker
# threads, which its Makefile builds in its source's directory, from a copy.
stage_zstd() {
	local b=$BUILD/zstd
	rm -rf "$b" && cp -r "$SRC/zstd" "$b" && cd "$b/lib"
	local make=(make CC="$CC_VEDA" AR="$TARGET-ar" CFLAGS="$VEDA_CFLAGS" PREFIX=/system LIBDIR=/system/lib
		DESTDIR="$ROOT")
	quiet make.log "${make[@]}" -j"$JOBS" libzstd.a-mt
	quiet install.log "${make[@]}" install-pc install-includes
	install -m 644 libzstd.a "$SYSTEM/lib/"
}
library zstd

# libmpdec (`decimal`), without its C++ library.
stage_mpdecimal() {
	autoconf_veda mpdecimal configure --disable-cxx --disable-doc
}
library mpdecimal

stage_expat() {
	cmake_veda expat -DEXPAT_BUILD_TOOLS=OFF -DEXPAT_BUILD_EXAMPLES=OFF -DEXPAT_BUILD_TESTS=OFF -DEXPAT_BUILD_DOCS=OFF \
		-DEXPAT_SHARED_LIBS=OFF
}
library expat

# The amalgamation (`sqlite3`), compiled as one file: thread-safe, without
# loadable extensions (Veda links statically), with full-text search
# (FTS3, FTS4 and FTS5), R*Tree indexes and the mathematical functions, as
# python.org's builds have them. It locks no files (its unix-none VFS):
# Veda's file system cannot arbitrate POSIX locks between processes yet
# (fcntl refuses them), so a database is one connection's to write at a
# time.
SQLITE_OPTIONS=(
	-DSQLITE_THREADSAFE=1 -DSQLITE_OMIT_LOAD_EXTENSION=1 -DSQLITE_ENABLE_FTS3=1 -DSQLITE_ENABLE_FTS3_PARENTHESIS=1
	-DSQLITE_ENABLE_FTS4=1 -DSQLITE_ENABLE_FTS5=1 -DSQLITE_ENABLE_RTREE=1 -DSQLITE_ENABLE_MATH_FUNCTIONS=1
	-DHAVE_USLEEP=1 -DHAVE_FDATASYNC=1 -DSQLITE_DEFAULT_UNIX_VFS=\"unix-none\"
)
stage_sqlite() {
	local b=$BUILD/sqlite
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet compile.log "$CC_VEDA" $VEDA_CFLAGS "${SQLITE_OPTIONS[@]}" -c -o sqlite3.o "$SRC/sqlite/sqlite3.c"
	mkdir -p "$SYSTEM/lib/pkgconfig" "$SYSTEM/include"
	rm -f "$SYSTEM/lib/libsqlite3.a"
	"$TARGET-ar" rcs "$SYSTEM/lib/libsqlite3.a" sqlite3.o
	cp "$SRC/sqlite/sqlite3.h" "$SRC/sqlite/sqlite3ext.h" "$SYSTEM/include/"
	cat > "$SYSTEM/lib/pkgconfig/sqlite3.pc" <<-EOF
	prefix=/system
	libdir=\${prefix}/lib
	includedir=\${prefix}/include

	Name: SQLite
	Description: SQL database engine
	Version: $(port sqlite version)
	Libs: -L\${libdir} -lsqlite3
	Libs.private: -lm -pthread
	Cflags: -I\${includedir}
	EOF
}
library sqlite @SQLITE_OPTIONS

# Veda's target of OpenSSL's (Configurations/50-veda.conf, in the port's
# patch), for `ssl` and `hashlib`: its assembly for the architecture,
# static, without what loads code (engines, providers in modules) or what
# Veda has no use for. Its certificates are the system's
# (/system/etc/ssl/cert.pem).
OPENSSL_OPTIONS=(--prefix=/system --openssldir=/system/etc/ssl --libdir=lib no-shared no-dso no-engine no-module
	no-tests no-docs no-apps no-afalgeng no-legacy no-ui-console)
stage_openssl() {
	local b=$BUILD/openssl
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC="$CC_VEDA" AR="$TARGET-ar" RANLIB="$TARGET-ranlib" \
		perl "$SRC/openssl/Configure" "veda-$OPENSSL_ARCH" "${OPENSSL_OPTIONS[@]}" $VEDA_CFLAGS
	quiet make.log make -j"$JOBS" build_libs
	quiet install.log make install_dev DESTDIR="$ROOT"
}
library openssl @OPENSSL_OPTIONS @OPENSSL_ARCH

# --- Python, for Veda -----------------------------------------------------------

# The modules of the standard library that Python on Veda must have built
# in, beside those every build has: configure says in the Makefile it
# writes whether each can be built, and the build fails if one of these
# cannot.
PYTHON_MODULES=(
	_bz2 _decimal _elementtree _hashlib _lzma _multiprocessing _posixshmem _posixsubprocess _socket _sqlite3 _ssl
	_zstd mmap pyexpat resource select termios zlib
)

# Why a module of the standard library is missing on Veda, which a
# traceback says for the ModuleNotFoundError of its import (JSON, written
# into the build: CPython's --with-missing-stdlib-config). The image leaves
# out tkinter and IDLE, which stand on Tk, and pty, whose every function
# needs a pseudo-terminal or fork().
MISSING_MODULES='{
  "_ctypes": "ctypes is not available on Veda yet: it needs dynamic loading (dlopen)",
  "_curses": "curses is not available on Veda: its Terminal cannot move the cursor yet",
  "_curses_panel": "curses is not available on Veda: its Terminal cannot move the cursor yet",
  "_dbm": "dbm.ndbm is not available on Veda: dbm.sqlite3 and dbm.dumb are",
  "_gdbm": "dbm.gnu is not available on Veda: dbm.sqlite3 and dbm.dumb are",
  "_remote_debugging": "remote debugging is not available on Veda: it reads the memory of other processes",
  "_tkinter": "tkinter is not available on Veda: it has no Tk",
  "idlelib": "IDLE is not available on Veda: it needs tkinter, and Veda has no Tk",
  "pty": "pty is not available on Veda: it has no pseudo-terminals",
  "readline": "readline is not available on Veda: its Terminal edits each line itself",
  "syslog": "syslog is not available on Veda: what a program writes to its standard error goes to the system log",
  "tkinter": "tkinter is not available on Veda: it has no Tk",
  "turtledemo": "turtledemo is not available on Veda: it needs tkinter, and Veda has no Tk"
}'

# CPython configured for Veda (its patch), as /system has it: the modules
# built into the interpreter, its libraries found by pkg-config, the
# answers of the run-time checks a cross build cannot make (config.site,
# then the architecture's), no pip installed (ensurepip has it, for
# environments), and the time zones in /system/share/zoneinfo, where Veda
# has none yet. Compiled with CPython's own optimisations, position-
# independent with stack protection as the libraries are; linked
# -static-pie, the sections of the libraries' code that nothing uses
# dropped.
PYTHON_CFLAGS="-fPIE -fstack-protector-strong -fstack-clash-protection $PREFIX_MAP"
PYTHON_LDFLAGS="-static-pie -Wl,--gc-sections"
PYTHON_OPTIONS=(
	--host=$TARGET --build="$BUILD_TRIPLE" --prefix=/system --disable-shared --without-ensurepip --with-system-expat
	--with-pkg-config=yes --with-tzpath=/system/share/zoneinfo --enable-ipv6
)

stage_src_python() { unpack python; }
stage src_python +python

# Veda's C library: musl, whose release platform.libc_ver() gives and whose
# headers and patch CPython is configured and built for (+musl below), and
# its archive, which the interpreter is linked with.
LIBC_VERSION=$(port musl version)
LIBC=$CROSS/sysroot/system/lib/libc.a

stage_python() {
	local b=$BUILD/python image=$BUILD/python-image
	rm -rf "$b" "$image" && mkdir -p "$b" && cd "$b"
	printf '%s\n' "$MISSING_MODULES" > missing-modules.json
	# Expat, which configure looks for without pkg-config.
	quiet configure.log env \
		CONFIG_SITE="$PORTS/python/config.site $PORTS/python/architectures/$VEDA_ARCH/config.site" \
		CC="$CC_VEDA" AR="$TARGET-ar" READELF="$TARGET-readelf" PKG_CONFIG="$TARGET-pkg-config" \
		CFLAGS="$PYTHON_CFLAGS" LDFLAGS="$PYTHON_LDFLAGS" VEDA_LIBC_VERSION="$LIBC_VERSION" \
		LIBEXPAT_CFLAGS="$("$TARGET-pkg-config" --cflags expat)" LIBEXPAT_LDFLAGS="$("$TARGET-pkg-config" --libs expat)" \
		"$SRC/python/configure" "${PYTHON_OPTIONS[@]}" --with-build-python="$HOST/bin/python$PYTHON_VERSION" \
		--with-missing-stdlib-config="$b/missing-modules.json"
	local module
	for module in "${PYTHON_MODULES[@]}"; do
		if ! grep -qx "MODULE_${module^^}_STATE=yes" Makefile; then
			echo "Python for Veda cannot build its module $module (see $b/config.log)" >&2
			exit 1
		fi
	done
	quiet make.log make -j"$JOBS"
}
stage python src_python host_python zlib bzip2 xz zstd mpdecimal expat sqlite openssl +musl \
	"$PORTS/python/config.site" "$PORTS/python/architectures/$VEDA_ARCH/config.site" @MISSING_MODULES \
	@PYTHON_CFLAGS @PYTHON_LDFLAGS @PYTHON_OPTIONS @PYTHON_MODULES @PYTHON_VERSION @LIBC_VERSION @VEDA_CFLAGS @ROOT \
	@TOOLCHAIN @TARGET

# The interpreter linked again with the C library as it is now (the POSIX
# layer in it changes more often than Python), then installed: the rest of
# the build stands.
stage_python_link() {
	local b=$BUILD/python image=$BUILD/python-image
	rm -rf "$b/python" "$image"
	quiet "$b/link.log" make -C "$b" python
	quiet "$b/install.log" make -C "$b" install DESTDIR="$image"
}
stage python_link python "$LIBC"

# Python as the image installs it: the interpreter, stripped, as python3
# (the unstripped one stays in the build, for addr2line); the standard
# library but for what Veda cannot use (IDLE and turtle's examples, on Tk;
# pty) and its tests, which go apart; no headers, static library or
# configuration for building C extensions, which Veda cannot load. The
# library's bytecode is compiled once, at the default optimisation level,
# and checked against no source (the system image does not change); the
# names the image shows are /system's.
STDLIB_LEFT_OUT=(idlelib tkinter turtledemo pty.py test)
stage_python_image() {
	local image=$BUILD/python-image/system out=$ROOT/python/system tests=$ROOT/tests/system
	local lib=lib/python$PYTHON_VERSION
	rm -rf "$ROOT/python" "$ROOT/tests"
	mkdir -p "$out/bin" "$out/$lib" "$tests/$lib"
	"$TARGET-strip" --strip-all -o "$out/bin/python3" "$image/bin/python$PYTHON_VERSION"
	cp -r "$image/$lib/." "$out/$lib/"
	rm -rf "$out/$lib/config-$PYTHON_VERSION-"*
	# Where extension modules would be loaded from: Python finds its
	# platform's files (sys.exec_prefix) by it, and the image keeps no empty
	# directory.
	cat > "$out/$lib/lib-dynload/README.txt" <<-EOF
	Python on Veda has its extension modules built into the interpreter
	(/system/bin/python3): Veda links its programs statically, and loads no
	shared libraries. This directory, where they would be, has none.
	EOF
	for dir in "${STDLIB_LEFT_OUT[@]}"; do
		rm -rf "${out:?}/$lib/$dir"
	done
	# What the image has of the installation, as its build details say
	# (PEP 739; the build's describe the build machine's extension modules):
	# the interpreter as /system/bin/python3, no extension modules, and no
	# C API or library to build with, which the image leaves out.
	"$HOST/bin/python$PYTHON_VERSION" -I - "$out/$lib/build-details.json" <<-'EOF'
	import json, sys
	path = sys.argv[1]
	with open(path, encoding='utf-8') as f:
	    details = json.load(f)
	details['base_interpreter'] = '/system/bin/python3'
	details['suffixes']['extensions'] = []
	details['abi'].pop('stable_abi_suffix', None)
	for left_out in ('libpython', 'c_api'):
	    details.pop(left_out, None)
	with open(path, 'w', encoding='utf-8') as f:
	    f.write(json.dumps(details, indent=2) + '\n')
	EOF
	cp -r "$image/$lib/test" "$tests/$lib/test"
	find "$out" "$tests" -name __pycache__ -type d -prune -exec rm -rf {} +
	local dir prefix
	for dir in "$out" "$tests"; do
		quiet "$BUILD/python-compile.log" "$HOST/bin/python$PYTHON_VERSION" -m compileall -q -j "$JOBS" -o 0 \
			--invalidation-mode unchecked-hash -s "$dir" -p /system -x 'bad_coding|badsyntax' "$dir/$lib"
	done
}
stage python_image python_link @STDLIB_LEFT_OUT @PYTHON_VERSION @ROOT

# --- the licences -------------------------------------------------------------------

# The licence texts of Python and of the libraries it has built in, as
# their releases have them, under licences/PORT/ (the image's
# /system/licences/PORT): CPython's licence, and the terms of the code of
# others it builds into the interpreter (Doc/license.rst, which python.org
# ships beside its programs); the licences of pip and the packages it
# vendors, from the wheel ensurepip installs. Each library above has its
# texts here, but SQLite, which is in the public domain.
LICENCE_FILES=(
	python:LICENSE python:Doc/license.rst zlib:LICENSE bzip2:LICENSE xz:COPYING xz:COPYING.0BSD zstd:LICENSE
	zstd:COPYING mpdecimal:COPYRIGHT.txt expat:COPYING openssl:LICENSE.txt
)
LICENCE_FREE=(sqlite)

stage_licences() {
	local dir=$ROOT/licences entry port file
	rm -rf "$dir"
	for port in python "${LIBRARIES[@]}"; do
		if [[ " ${LICENCE_FREE[*]} " != *" $port "* && " ${LICENCE_FILES[*]} " != *" $port:"* ]]; then
			echo "$port has no licence texts in LICENCE_FILES" >&2
			exit 1
		fi
	done
	for entry in "${LICENCE_FILES[@]}"; do
		port=${entry%%:*} file=${entry#*:}
		mkdir -p "$dir/$port/$(dirname "$file")"
		cp -r "$SRC/$port/$file" "$dir/$port/$file"
	done
	local wheel
	for wheel in "$SRC/python/Lib/ensurepip/_bundled/"*.whl; do
		"$HOST/bin/python$PYTHON_VERSION" - "$wheel" "$dir/python/ensurepip" <<-'EOF'
		import pathlib, sys, zipfile
		wheel, out = sys.argv[1], pathlib.Path(sys.argv[2])
		with zipfile.ZipFile(wheel) as z:
		    texts = [n for n in z.namelist() if '.dist-info/licenses/' in n and not n.endswith('/')]
		    if not texts:
		        sys.exit(f'{wheel}: no licence texts')
		    for name in texts:
		        target = out / name
		        target.parent.mkdir(parents=True, exist_ok=True)
		        target.write_bytes(z.read(name))
		EOF
	done
}
LICENCE_SOURCES=(src_python host_python)
for port in "${LIBRARIES[@]}"; do
	LICENCE_SOURCES+=("src_${port//-/_}")
done
stage licences "${LICENCE_SOURCES[@]}" @LICENCE_FILES @LICENCE_FREE @PYTHON_VERSION @ROOT

log Finished "Python $(port python version) for Veda ($ROOT)"
