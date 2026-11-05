#!/bin/bash
# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Builds curl for Veda from the ports: the curl program and libcurl, a
# shared library that holds the libraries it stands on (OpenSSL, for TLS;
# nghttp2, for HTTP/2; zlib, Brotli and Zstandard, for compressed
# responses; libpsl, for the domains a cookie may be set for; libidn2, for
# international domain names); and pkgconf, which its build runs on this
# machine. `cargo automation toolchain` runs it after build.sh, the sources
# downloaded and verified; see automation/source/toolchain.rs and
# documentation/CURL.md.
#
# Environment:
#   VEDA_PORTS       implementation/ports
#   VEDA_ARCH        the architecture built for
#                    (architectures/$VEDA_ARCH/settings.sh)
#   VEDA_CURL        where to build
#   VEDA_TOOLCHAIN   Veda's toolchain: its cross/ compilers and sysroot, and
#                    downloads/, which holds the verified archives
#   JOBS             parallel jobs (default: the number of processors)
#
# Results, under $VEDA_CURL:
#   host/            pkgconf for this machine (and $TARGET-pkg-config, which
#                    finds the libraries below)
#   system/          the libraries for Veda, static and position-independent,
#                    as /system would have them: include/ and lib/ (with
#                    their pkg-config files), which libcurl holds, and the
#                    tests' servers link
#   curl/system/     curl as the image installs it in /system: bin/curl;
#                    lib/libcurl.so.4, and lib/libcurl.so, which links it;
#                    include/curl, libcurl's interface; and
#                    lib/pkgconfig/libcurl.pc
#   licences/        the licence texts of curl and of the libraries libcurl
#                    holds, which the image installs in /system/licences/curl
#
# Every stage keeps a fingerprint of what went into it and is skipped while
# that is unchanged, as in build.sh.

set -euo pipefail

PORTS=${VEDA_PORTS:?}
VEDA_ARCH=${VEDA_ARCH:?}
ROOT=${VEDA_CURL:?}
TOOLCHAIN=${VEDA_TOOLCHAIN:?}
JOBS=${JOBS:-$(nproc)}

DOWNLOADS=$TOOLCHAIN/downloads
SRC=$ROOT/src
BUILD=$ROOT/build
STAMPS=$BUILD/stamps
HOST=$ROOT/host
SYSTEM=$ROOT/system
CROSS=$TOOLCHAIN/cross

mkdir -p "$SRC" "$BUILD" "$STAMPS" "$HOST/bin" "$SYSTEM"
export PATH="$HOST/bin:$CROSS/bin:$PATH"
# The build's own settings, whatever this machine's are.
unset PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR CFLAGS CXXFLAGS LDFLAGS CPPFLAGS

# log, port, archive, patches, stage, quiet and unpack.
source "$PORTS/common.sh"
# TARGET and what the ports take from the architecture.
source "$PORTS/architectures/$VEDA_ARCH/settings.sh"
# VEDA_CFLAGS, the compilers, pkgconf (host_pkgconf), CMake's toolchain
# file, cmake_veda and autoconf_veda: how libraries are built for Veda.
source "$PORTS/libraries.sh"

# libcurl is a shared library, and holds the libraries below: their code is
# position-independent for one (-fPIC, which programs can hold too), where
# libraries.sh's is for programs (-fPIE).
VEDA_CFLAGS=${VEDA_CFLAGS/-fPIE/-fPIC}

# `grep -q` for each `#define` a configured build must have: a check that
# fails says which (a library that configure did not find, say).
expect_defines() {
	local file=$1 line
	shift
	for line in "$@"; do
		grep -qxF "#define $line" "$file" || { echo "$file: expected '#define $line'" >&2; exit 1; }
	done
}

# --- the libraries, for Veda ----------------------------------------------------

# Meson, for libpsl: Veda's compilers, the pkg-config for Veda, the flags
# above.
MESON_CROSS=$BUILD/veda.cross
cat > "$MESON_CROSS" <<EOF
[binaries]
c = '$CC_VEDA'
ar = '$CROSS/bin/$TARGET-ar'
strip = '$CROSS/bin/$TARGET-strip'
pkg-config = '$HOST/bin/$TARGET-pkg-config'

[host_machine]
system = 'veda'
cpu_family = '$MESON_CPU_FAMILY'
cpu = '$MESON_CPU'
endian = 'little'

[properties]
needs_exe_wrapper = true

[built-in options]
c_args = [$(printf "'%s', " $VEDA_CFLAGS)]
default_library = 'static'
prefer_static = true
wrap_mode = 'nodownload'
EOF

# How the libraries are built for Veda, which every library's build
# depends on: the cross file, the toolchain file, CMake's description of
# the system, libraries.sh's helpers, and the flags.
declare -f cmake_veda autoconf_veda > "$BUILD/helpers.sh"
CONFIG_FILES=("$MESON_CROSS" "$CMAKE_TOOLCHAIN" "$CMAKE_MODULES/Platform/Veda.cmake" "$BUILD/helpers.sh")

# What the libraries and curl are compiled by, which their builds depend
# on as the toolchain's media libraries do: the toolchain's ports of the
# compiler, the binary tools and the C library.
COMPILED_BY=(+gcc +binutils +musl)

# The stages of library PORT: its source as Veda patches it (`src_NAME`),
# then its build (`NAME`, with the stage function stage_NAME), which also
# depends on the stages given, on how libraries are built for Veda and on
# what compiles them. NAME is PORT with dashes as underscores. LIBRARIES
# lists them, for their licences.
LIBRARIES=()
library() {
	local port=$1 name=${1//-/_}
	shift
	LIBRARIES+=("$port")
	eval "stage_src_$name() { unpack $port; }"
	stage "src_$name" "+$port"
	stage "$name" "src_$name" host_pkgconf "$@" "${CONFIG_FILES[@]}" "${COMPILED_BY[@]}" @VEDA_CFLAGS @ROOT \
		@TOOLCHAIN @TARGET
}

# zlib: gzip and deflate (Content-Encoding), and the built-in manual,
# which the curl program carries compressed.
stage_zlib() {
	local b=$BUILD/zlib
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CHOST=$TARGET CC="$CC_VEDA" CFLAGS="$VEDA_CFLAGS" "$SRC/zlib/configure" \
		--prefix=/system --static
	quiet make.log make -j"$JOBS" libz.a
	quiet install.log make install DESTDIR="$ROOT"
}
library zlib

# Zstandard's library (Content-Encoding: zstd), which its Makefile builds in
# its source's directory, from a copy. Decompressing needs none of its
# compressor's threads.
stage_zstd() {
	local b=$BUILD/zstd
	rm -rf "$b" && cp -r "$SRC/zstd" "$b" && cd "$b/lib"
	local make=(make CC="$CC_VEDA" AR="$TARGET-ar" CFLAGS="$VEDA_CFLAGS" PREFIX=/system LIBDIR=/system/lib
		DESTDIR="$ROOT")
	quiet make.log "${make[@]}" -j"$JOBS" libzstd.a
	quiet install.log "${make[@]}" install-pc install-includes
	install -m 644 libzstd.a "$SYSTEM/lib/"
}
library zstd

# Brotli's libraries (Content-Encoding: br), without its program.
stage_brotli() {
	cmake_veda brotli -DCMAKE_C_FLAGS="$VEDA_CFLAGS" -DBROTLI_BUILD_TOOLS=OFF -DBROTLI_DISABLE_TESTS=ON
}
library brotli

# nghttp2's library alone: HTTP/2's framing, its streams and their flow,
# and HPACK.
stage_nghttp2() {
	cmake_veda nghttp2 -DCMAKE_C_FLAGS="$VEDA_CFLAGS" -DENABLE_LIB_ONLY=ON -DBUILD_STATIC_LIBS=ON \
		-DBUILD_SHARED_LIBS=OFF -DENABLE_DOC=OFF -DBUILD_TESTING=OFF
}
library nghttp2

# libpsl, with the Public Suffix List its release carries built in, as the
# web engine's is; the names curl asks about are ASCII already (libidn2's,
# below), so it needs no IDNA library of its own.
stage_libpsl() {
	local b=$BUILD/libpsl
	rm -rf "$b"
	quiet "$b-setup.log" meson setup "$b" "$SRC/libpsl" --cross-file "$MESON_CROSS" --prefix=/system \
		--libdir=lib --buildtype=release -Druntime=no -Dbuiltin=true -Dtests=false -Ddocs=false
	quiet "$b-build.log" ninja -C "$b" -j"$JOBS"
	quiet "$b-install.log" env DESTDIR="$ROOT" meson install -C "$b" --no-rebuild
}
library libpsl

# libidn2, with the parts of libunistring it carries, without its
# translations or documents. Its archive is all libcurl takes: libtool's
# description of it (libidn2.la) would name the directories of this build.
stage_libidn2() {
	autoconf_veda libidn2 configure --with-included-libunistring --disable-nls --disable-doc \
		--disable-valgrind-tests --disable-rpath
	rm -f "$SYSTEM/lib/libidn2.la"
}
library libidn2

# Veda's target of OpenSSL's (Configurations/50-veda.conf, in the port's
# patch): its assembly for the architecture, static, without what loads
# code (engines, providers in modules), the ciphers of its legacy provider,
# or what curl has no use for: QUIC's stack among them, curl having no
# HTTP/3. Its certificates are the system's (/system/etc/ssl).
OPENSSL_OPTIONS=(--prefix=/system --openssldir=/system/etc/ssl --libdir=lib no-shared no-dso no-engine no-module
	no-tests no-docs no-apps no-afalgeng no-legacy no-ui-console no-quic)
stage_openssl() {
	local b=$BUILD/openssl
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC="$CC_VEDA" AR="$TARGET-ar" RANLIB="$TARGET-ranlib" \
		perl "$SRC/openssl/Configure" "veda-$OPENSSL_ARCH" "${OPENSSL_OPTIONS[@]}" $VEDA_CFLAGS
	quiet make.log make -j"$JOBS" build_libs
	quiet install.log make install_dev DESTDIR="$ROOT"
}
library openssl @OPENSSL_OPTIONS @OPENSSL_ARCH

# --- curl, for Veda -------------------------------------------------------------

# curl as /system has it: TLS by OpenSSL, trusting the system's certificate
# authorities (/system/etc/ssl/cert.pem) and nothing else unless told;
# HTTP/2 by nghttp2; every compression of HTTP's; cookies checked against
# the Public Suffix List; international domain names; names looked up in a
# thread of their own, so that a transfer can time out while one is;
# IPv6, Unix domain sockets and the built-in manual (`curl --manual`).
# Without what needs libraries Veda has not (LDAP, SSH's SCP and SFTP,
# Kerberos) and HTTP/3, which needs QUIC's libraries.
CURL_OPTIONS=(
	--host=$TARGET --build="$BUILD_TRIPLE" --prefix=/system --libdir=/system/lib
	--enable-shared --disable-static --disable-dependency-tracking
	--with-openssl --with-nghttp2 --with-zlib --with-brotli --with-zstd --with-libpsl --with-libidn2
	--with-ca-bundle=/system/etc/ssl/cert.pem --without-ca-path --without-ca-fallback
	--enable-threaded-resolver --enable-ipv6 --enable-unix-sockets --enable-manual
	--disable-ldap --disable-ldaps --without-libssh2 --without-libssh --without-gssapi
	--without-ngtcp2 --without-nghttp3 --without-quiche --without-libgsasl
	--without-zsh-functions-dir --without-fish-functions-dir
)

# How libcurl and the program are linked, beside GCC's own hardening for
# what is linked dynamically: what nothing uses dropped; libcurl exporting
# its interface alone (curl_*, which -fvisibility=hidden leaves it), none of
# the libraries it holds, whose code is its own, and naming every symbol it
# needs (-z defs); the program dynamically linked, with libcurl.so.4 and the
# C library's shared object (-pie: on Veda, programs are static otherwise).
CURL_LDFLAGS="-Wl,--gc-sections"
CURL_MAKE=(CURL_LDFLAGS_LIB="-Wl,--exclude-libs,ALL -Wl,-z,defs" CURL_LDFLAGS_BIN=-pie)

stage_src_curl() { unpack curl; }
stage src_curl +curl

stage_curl() {
	local b=$BUILD/curl image=$BUILD/curl-image
	rm -rf "$b" "$image" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC="$CC_VEDA" AR="$TARGET-ar" RANLIB="$TARGET-ranlib" CFLAGS="$VEDA_CFLAGS" \
		LDFLAGS="$CURL_LDFLAGS" PKG_CONFIG="$TARGET-pkg-config" "$SRC/curl/configure" "${CURL_OPTIONS[@]}"
	expect_defines lib/curl_config.h "USE_OPENSSL 1" "USE_NGHTTP2 1" "HAVE_LIBZ 1" "HAVE_BROTLI 1" \
		"HAVE_ZSTD 1" "USE_LIBPSL 1" "HAVE_LIBIDN2 1" "HAVE_IDN2_H 1" "USE_RESOLV_THREADED 1" "USE_IPV6 1" \
		"USE_UNIX_SOCKETS 1" "CURL_CA_BUNDLE \"/system/etc/ssl/cert.pem\""
	if ! grep -qx 'build_libtool_libs=yes' libtool; then
		echo "curl's libtool cannot make shared libraries for Veda (see $b/config.log)" >&2
		exit 1
	fi
	if ! grep -qxF 'S["USE_MANUAL_TRUE"]=""' config.status; then
		echo "curl's configure left out the built-in manual (see $b/config.log)" >&2
		exit 1
	fi
	quiet make.log make -j"$JOBS" "${CURL_MAKE[@]}"
	quiet install.log make install DESTDIR="$image" "${CURL_MAKE[@]}"
}
stage curl src_curl zlib zstd brotli nghttp2 libpsl libidn2 openssl "${COMPILED_BY[@]}" @CURL_OPTIONS \
	@CURL_LDFLAGS @CURL_MAKE @VEDA_CFLAGS @ROOT @TOOLCHAIN @TARGET

# The names of the dynamic entries of type TYPE (NEEDED, RUNPATH...) in
# the ELF file FILE, one a line.
dynamic() {
	"$TARGET-readelf" -d "$2" | sed -n "s/.*($1) *[^\[]*\[\(.*\)\]$/\1/p"
}

# curl as the image installs it, checked: the program and the library,
# stripped (the unstripped ones stay in the build, for addr2line), each
# needing only what /system/lib has and found without a RUNPATH; the
# library by its soname, and a linker script of its unversioned name that
# links it (`-lcurl`), there being no symbolic links; libcurl's interface,
# and its pkg-config file, without what linking libcurl statically would
# need: the image has it shared alone.
stage_curl_image() {
	local image=$BUILD/curl-image/system out=$ROOT/curl/system soname
	soname=$(dynamic SONAME "$image/lib/libcurl.so")
	if [ "$soname" != libcurl.so.4 ]; then
		echo "libcurl's soname is '$soname', not libcurl.so.4" >&2
		exit 1
	fi
	rm -rf "$ROOT/curl"
	mkdir -p "$out/bin" "$out/lib/pkgconfig" "$out/include"
	"$TARGET-strip" --strip-all -o "$out/bin/curl" "$image/bin/curl"
	"$TARGET-strip" --strip-unneeded -o "$out/lib/$soname" "$(readlink -f "$image/lib/$soname")"
	echo "INPUT($soname)" > "$out/lib/libcurl.so"
	cp -r "$image/include/curl" "$out/include/"
	sed '/^Requires\.private:/d; /^Libs\.private:/d; /^Cflags\.private:/d' "$image/lib/pkgconfig/libcurl.pc" \
		> "$out/lib/pkgconfig/libcurl.pc"
	local file expected needed
	for file in bin/curl "lib/$soname"; do
		expected="libc.so"
		[ "$file" = bin/curl ] && expected="$soname libc.so"
		needed=$(dynamic NEEDED "$out/$file" | sort | tr '\n' ' ')
		if [ "$needed" != "$(tr ' ' '\n' <<< "$expected" | sort | tr '\n' ' ')" ]; then
			echo "$file needs $needed, where it should need $expected alone" >&2
			exit 1
		fi
		if [ -n "$(dynamic RUNPATH "$out/$file")$(dynamic RPATH "$out/$file")" ]; then
			echo "$file has a search path of its own: it should find its libraries in /system/lib" >&2
			exit 1
		fi
	done
	# Its interface, and what every shared library has (_init, _fini).
	local other
	other=$("$TARGET-nm" -D --defined-only --format=just-symbols "$out/lib/$soname" |
		grep -vx 'curl_.*\|_init\|_fini' || true)
	if [ -n "$other" ]; then
		echo "libcurl exports more than its interface:" $other >&2
		exit 1
	fi
}
stage curl_image curl @ROOT @TARGET

# --- the licences -------------------------------------------------------------------

# The licence texts of curl and of the libraries libcurl holds, as their
# releases have them, under licences/PORT/ (the image's
# /system/licences/curl/PORT): libidn2's library is under the LGPL 3, which
# is the GPL 3 with permissions added, beside the GPL 2, and has Unicode's
# data; Zstandard's are both of those it is offered under, as Python's. The
# Public Suffix List that libpsl builds in is under the MPL 2.0, as its
# SOURCE note says (automation/source/curl.rs).
LICENCE_FILES=(
	curl:COPYING openssl:LICENSE.txt nghttp2:COPYING zlib:LICENSE brotli:LICENSE zstd:LICENSE zstd:COPYING
	libpsl:COPYING libidn2:COPYING.LESSERv3 libidn2:COPYING libidn2:COPYINGv2 libidn2:COPYING.unicode
)

stage_licences() {
	local dir=$ROOT/licences entry port file
	rm -rf "$dir"
	for port in curl "${LIBRARIES[@]}"; do
		if [[ " ${LICENCE_FILES[*]} " != *" $port:"* ]]; then
			echo "$port has no licence texts in LICENCE_FILES" >&2
			exit 1
		fi
	done
	for entry in "${LICENCE_FILES[@]}"; do
		port=${entry%%:*} file=${entry#*:}
		mkdir -p "$dir/$port/$(dirname "$file")"
		cp -r "$SRC/$port/$file" "$dir/$port/$file"
	done
}
LICENCE_SOURCES=(src_curl)
for port in "${LIBRARIES[@]}"; do
	LICENCE_SOURCES+=("src_${port//-/_}")
done
stage licences "${LICENCE_SOURCES[@]}" @LICENCE_FILES @ROOT

log Finished "curl $(port curl version) for Veda ($ROOT)"
