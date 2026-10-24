#!/bin/bash
# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Builds Veda's web engine from the ports: WPE WebKit and the libraries it
# stands on, static and position-independent, for the browser
# (implementation/applications/browser), which `cargo automation` links;
# and the tools for this machine that their builds run. `cargo automation
# web` runs it after downloading and verifying the sources; see
# automation/source/web.rs and documentation/BROWSER.md.
#
# Environment:
#   VEDA_PORTS       implementation/ports
#   VEDA_ARCH        the architecture built for
#                    (architectures/$VEDA_ARCH/settings.sh)
#   VEDA_WEB         where to build; downloads/ holds the verified archives
#   VEDA_TOOLCHAIN   Veda's toolchain (its cross/ compilers and sysroot),
#                    which `cargo automation toolchain` builds
#   JOBS             parallel jobs (default: the number of processors);
#                    WebKit's C++ takes some 2 GB a job, so it gets fewer
#                    on a machine without the memory
#
# Results, under $VEDA_WEB:
#   host/            the tools for this machine: Meson, pkgconf (and
#                    $TARGET-pkg-config, which finds the libraries below),
#                    gperf, Ruby, and GLib's code generators
#   system/          the libraries for Veda, as /system would have them:
#                    include/, lib/ (static archives and their pkg-config
#                    files) and share/ (their data)
#   webkit/          WPE WebKit's build, whose static libraries the browser
#                    links (webkit/lib)
#
# Every stage keeps a fingerprint of what went into it and is skipped while
# that is unchanged, as in build.sh. The libraries are compiled for Veda
# with the same flags (VEDA_CFLAGS), which a program that links them can
# rely on: position-independent (for -static-pie), with stack protection.

set -euo pipefail

PORTS=${VEDA_PORTS:?}
VEDA_ARCH=${VEDA_ARCH:?}
ROOT=${VEDA_WEB:?}
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
# The build's own languages and tools, whatever this machine's are set to.
export LC_ALL=C.UTF-8
unset PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR CFLAGS CXXFLAGS LDFLAGS CPPFLAGS

# log, port, archive, patches, stage, quiet and unpack.
source "$PORTS/common.sh"
# TARGET and what the ports take from the architecture.
source "$PORTS/architectures/$VEDA_ARCH/settings.sh"
# VEDA_CFLAGS, the compilers, pkgconf (host_pkgconf), CMake's toolchain
# file, cmake_veda and autoconf_veda: how libraries are built for Veda.
source "$PORTS/libraries.sh"

# --- tools for this machine ---------------------------------------------------

# Meson, which runs from its source (Python): the release the libraries'
# builds need, whatever this machine has.
stage_host_meson() {
	local SRC=$HOST_SRC
	unpack meson upstream
	cat > "$HOST/bin/meson" <<-EOF
	#!/bin/sh
	exec python3 '$HOST_SRC/meson/meson.py' "\$@"
	EOF
	chmod +x "$HOST/bin/meson"
}
stage host_meson "$PORTS/meson/port.toml" @ROOT

stage_host_gperf() {
	local SRC=$HOST_SRC
	unpack gperf upstream
	local b=$BUILD/gperf-host
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/gperf/configure" --prefix="$HOST"
	quiet make.log make -j"$JOBS"
	quiet install.log make install
}
stage host_gperf "$PORTS/gperf/port.toml" @ROOT

# Ruby for JavaScriptCore's offlineasm and WebKit's generators: the
# interpreter and its standard library, no documentation, no extensions
# that need libraries this machine may lack.
stage_host_ruby() {
	local SRC=$HOST_SRC
	local b
	# WebKit's preferences are YAML, which Ruby reads with libyaml.
	unpack libyaml upstream
	b=$BUILD/libyaml-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/libyaml/configure" --prefix="$HOST" --disable-shared --enable-static --with-pic
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	unpack ruby upstream
	b=$BUILD/ruby-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/ruby/configure" --prefix="$HOST" --disable-install-doc --disable-install-rdoc \
		--disable-shared --without-gmp --with-libyaml-dir="$HOST" \
		--with-out-ext=openssl,readline,fiddle,zlib,dbm,gdbm
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	"$HOST/bin/ruby" -e 'require "yaml"; exit YAML.load("a: 1")["a"] == 1'
}
stage host_ruby "$PORTS/libyaml/port.toml" "$PORTS/ruby/port.toml" @ROOT

# GLib's code generators (glib-compile-resources, glib-mkenums,
# glib-genmarshal, gdbus-codegen), which the GLib libraries' builds and
# WebKit's run: GLib for this machine, with the zlib, libffi and PCRE2 it
# needs, all static, in host/.
stage_host_glib() {
	local SRC=$HOST_SRC
	local b
	unpack zlib upstream
	b=$BUILD/zlib-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/zlib/configure" --prefix="$HOST" --static
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	unpack libffi upstream
	b=$BUILD/libffi-host && rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/libffi/configure" --prefix="$HOST" --disable-shared --enable-static \
		--disable-docs --disable-multi-os-directory --with-pic
	quiet make.log make -j"$JOBS"
	quiet install.log make install
	unpack pcre2 upstream
	b=$BUILD/pcre2-host && rm -rf "$b" && mkdir -p "$b"
	quiet "$b-configure.log" cmake -S "$SRC/pcre2" -B "$b" -G Ninja -DCMAKE_BUILD_TYPE=Release \
		-DCMAKE_INSTALL_PREFIX="$HOST" -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=OFF -DBUILD_STATIC_LIBS=ON \
		-DPCRE2_STATIC_PIC=ON -DPCRE2_BUILD_PCRE2GREP=OFF -DPCRE2_BUILD_TESTS=OFF
	quiet "$b-build.log" ninja -C "$b" install
	unpack glib upstream
	b=$BUILD/glib-host && rm -rf "$b"
	PKG_CONFIG_PATH="$HOST/lib/pkgconfig" quiet "$b-setup.log" meson setup "$b" "$SRC/glib" \
		--prefix="$HOST" --libdir=lib --buildtype=release --default-library=static --wrap-mode=nodownload \
		-Dtests=false -Dinstalled_tests=false -Dnls=disabled -Dlibmount=disabled -Dselinux=disabled \
		-Dxattr=false -Dintrospection=disabled -Dman-pages=disabled -Ddocumentation=false -Dsysprof=disabled \
		-Ddtrace=disabled -Dsystemtap=disabled -Dlibelf=disabled -Dglib_debug=disabled
	quiet "$b-build.log" ninja -C "$b" install
}
stage host_glib "$PORTS/zlib/port.toml" "$PORTS/libffi/port.toml" "$PORTS/pcre2/port.toml" \
	"$PORTS/glib/port.toml" host_meson host_pkgconf @ROOT

# --- how the libraries are built for Veda --------------------------------------

# Meson's description of Veda (a cross file), for the libraries built with
# Meson: Veda's compilers, the pkg-config for Veda (libraries.sh's), and
# the code generators of GLib for this machine.
MESON_CROSS=$BUILD/veda.cross
cat > "$MESON_CROSS" <<EOF
[binaries]
c = '$CC_VEDA'
cpp = '$CXX_VEDA'
ar = '$CROSS/bin/$TARGET-ar'
nm = '$CROSS/bin/$TARGET-nm'
ranlib = '$CROSS/bin/$TARGET-ranlib'
strip = '$CROSS/bin/$TARGET-strip'
pkg-config = '$HOST/bin/$TARGET-pkg-config'
glib-compile-resources = '$HOST/bin/glib-compile-resources'
glib-compile-schemas = '$HOST/bin/glib-compile-schemas'
glib-genmarshal = '$HOST/bin/glib-genmarshal'
glib-mkenums = '$HOST/bin/glib-mkenums'
gdbus-codegen = '$HOST/bin/gdbus-codegen'

[host_machine]
system = 'veda'
cpu_family = '$MESON_CPU_FAMILY'
cpu = '$MESON_CPU'
endian = 'little'

[properties]
needs_exe_wrapper = true

[built-in options]
c_args = [$(printf "'%s', " $VEDA_CFLAGS)]
cpp_args = [$(printf "'%s', " $VEDA_CFLAGS)]
default_library = 'static'
prefer_static = true
wrap_mode = 'nodownload'
EOF

# Meson for Veda: library NAME, configured with OPTIONS and installed into
# system/.
meson_veda() {
	local name=$1 b=$BUILD/$1
	shift
	rm -rf "$b"
	quiet "$b-setup.log" meson setup "$b" "$SRC/$name" --cross-file "$MESON_CROSS" --prefix=/system \
		--libdir=lib --buildtype=release "$@"
	quiet "$b-build.log" ninja -C "$b" -j"$JOBS"
	quiet "$b-install.log" env DESTDIR="$ROOT" meson install -C "$b" --no-rebuild
	relocate_cmake_packages
}

# The CMake packages Meson writes (libxml2's) name their files by the
# prefix, /system, in full: they are made to find them from where they are,
# as CMake's own packages do, so that CMake finds them under system/
# (lib/cmake/PACKAGE/*.cmake, four directories below the root).
relocate_cmake_packages() {
	local f
	while IFS= read -r f; do
		grep -q 'VEDA_SYSROOT' "$f" && continue
		grep -q '/system/' "$f" || continue
		sed -i -e '1i get_filename_component(VEDA_SYSROOT "${CMAKE_CURRENT_LIST_DIR}/../../../.." ABSOLUTE)' \
			-e 's|\([ "(]\)/system/|\1${VEDA_SYSROOT}/system/|g' "$f"
	done < <(find "$SYSTEM/lib/cmake" -mindepth 2 -maxdepth 2 -name '*.cmake' 2>/dev/null)
}

# How the libraries are built for Veda, which every library's build
# depends on: the cross file, the toolchain file, CMake's description of
# the system, the helpers (above, and libraries.sh's), and the flags.
declare -f meson_veda relocate_cmake_packages cmake_veda autoconf_veda > "$BUILD/helpers.sh"
CONFIG_FILES=("$MESON_CROSS" "$CMAKE_TOOLCHAIN" "$CMAKE_MODULES/Platform/Veda.cmake" "$BUILD/helpers.sh")

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
	stage "$name" "src_$name" host_meson host_pkgconf "$@" "${CONFIG_FILES[@]}" @VEDA_CFLAGS @ROOT @TOOLCHAIN @TARGET
}

# --- the libraries ------------------------------------------------------------

stage_zlib() {
	local b=$BUILD/zlib
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CHOST=$TARGET CC="$CC_VEDA" CFLAGS="$VEDA_CFLAGS" "$SRC/zlib/configure" \
		--prefix=/system --static
	quiet make.log make -j"$JOBS" libz.a
	quiet install.log make install DESTDIR="$ROOT"
}
library zlib

# GLib's calls through GClosure (ffi_call); nothing builds closures at run
# time (GObject introspection would), so no memory is made executable.
stage_libffi() {
	autoconf_veda libffi configure --disable-docs --disable-multi-os-directory --disable-exec-static-tramp
}
library libffi

# 8-bit only (GLib's), without the JIT, whose code would need executable
# memory: GRegex is not on any path a page can make hot.
stage_pcre2() {
	cmake_veda pcre2 -DBUILD_STATIC_LIBS=ON -DPCRE2_BUILD_PCRE2_8=ON -DPCRE2_BUILD_PCRE2_16=OFF \
		-DPCRE2_BUILD_PCRE2_32=OFF -DPCRE2_SUPPORT_JIT=OFF -DPCRE2_BUILD_PCRE2GREP=OFF -DPCRE2_BUILD_TESTS=OFF \
		-DPCRE2_STATIC_PIC=ON
}
library pcre2

stage_glib() {
	meson_veda glib -Dtests=false -Dinstalled_tests=false -Dnls=disabled -Dlibmount=disabled -Dselinux=disabled \
		-Dxattr=false -Dintrospection=disabled -Dman-pages=disabled -Ddocumentation=false -Dsysprof=disabled \
		-Ddtrace=disabled -Dsystemtap=disabled -Dlibelf=disabled -Dglib_debug=disabled
}
library glib zlib libffi pcre2 host_glib

# The Public Suffix List as the release has it, built into the library.
stage_libpsl() {
	meson_veda libpsl -Druntime=no -Dbuiltin=true -Dtests=false -Ddocs=false
}
library libpsl

stage_nghttp2() {
	cmake_veda nghttp2 -DENABLE_LIB_ONLY=ON -DBUILD_STATIC_LIBS=ON -DENABLE_DOC=OFF -DBUILD_TESTING=OFF
}
library nghttp2

stage_brotli() {
	cmake_veda brotli -DBROTLI_BUILD_TOOLS=OFF -DBROTLI_DISABLE_TESTS=ON
}
library brotli

# The amalgamation, compiled as one file: thread-safe, without loadable
# extensions (Veda links statically), with the options WebKit's databases
# use. It locks no files (its unix-none VFS): Veda's file system cannot
# arbitrate POSIX locks between processes yet (fcntl refuses them), and
# each of the engine's databases is one process's (the network process
# keeps the cookies, the storage and the like, the browser's own process
# the icons), in a browser that runs once.
SQLITE_OPTIONS=(
	-DSQLITE_THREADSAFE=1 -DSQLITE_OMIT_LOAD_EXTENSION=1 -DSQLITE_DEFAULT_MEMSTATUS=0 -DSQLITE_ENABLE_FTS3=1
	-DSQLITE_ENABLE_FTS3_PARENTHESIS=1 -DSQLITE_ENABLE_UNLOCK_NOTIFY=1 -DSQLITE_SECURE_DELETE=1
	-DSQLITE_ENABLE_COLUMN_METADATA=1 -DSQLITE_DQS=0 -DHAVE_USLEEP=1 -DHAVE_FDATASYNC=1
	-DSQLITE_DEFAULT_UNIX_VFS=\"unix-none\"
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

# The parser WebKit's XML documents and fontconfig's configuration use;
# no network access of its own, no Python.
stage_libxml2() {
	meson_veda libxml2 -Dpython=disabled -Dicu=disabled -Dhttp=disabled -Dreadline=disabled -Dhistory=disabled \
		-Dmodules=disabled -Dlegacy=disabled -Dzlib=enabled -Ddocs=disabled
}
library libxml2 zlib

stage_libxslt() {
	cmake_veda libxslt -DLIBXSLT_WITH_PYTHON=OFF -DLIBXSLT_WITH_CRYPTO=OFF -DLIBXSLT_WITH_TESTS=OFF \
		-DLIBXSLT_WITH_PROGRAMS=OFF -DLIBXSLT_WITH_MODULES=OFF -DLIBXSLT_WITH_DEBUGGER=OFF
}
library libxslt libxml2

# ICU is cross-compiled with a build of itself for this machine, whose
# tools make its data; the data is linked into the library (the program
# carries it, as it does everything else).
ICU_OPTIONS=(--disable-shared --enable-static --disable-tests --disable-samples --disable-extras
	--disable-layoutex --disable-icuio --disable-dyload --with-data-packaging=static)
stage_icu() {
	local h=$BUILD/icu-host b=$BUILD/icu
	rm -rf "$h" && mkdir -p "$h" && cd "$h"
	quiet configure.log "$SRC/icu/source/configure" "${ICU_OPTIONS[@]}"
	quiet make.log make -j"$JOBS"
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	# Static ICU is C++: what links it as C needs C++'s library, which its
	# pkg-config files then name (LIBS goes into them).
	quiet configure.log env CC="$CC_VEDA" CXX="$CXX_VEDA" CFLAGS="$VEDA_CFLAGS" CXXFLAGS="$VEDA_CFLAGS" LIBS=-lstdc++ \
		"$SRC/icu/source/configure" --host=$TARGET --build="$BUILD_TRIPLE" --prefix=/system \
		--with-cross-build="$h" "${ICU_OPTIONS[@]}" --disable-tools
	quiet make.log make -j"$JOBS"
	quiet install.log make install DESTDIR="$ROOT"
}
library icu @ICU_OPTIONS

stage_expat() {
	cmake_veda expat -DEXPAT_BUILD_TOOLS=OFF -DEXPAT_BUILD_EXAMPLES=OFF -DEXPAT_BUILD_TESTS=OFF -DEXPAT_BUILD_DOCS=OFF \
		-DEXPAT_SHARED_LIBS=OFF
}
library expat

stage_libpng() {
	cmake_veda libpng -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_TESTS=OFF -DPNG_TOOLS=OFF -DPNG_FRAMEWORK=OFF
}
library libpng zlib

stage_libjpeg_turbo() {
	cmake_veda libjpeg-turbo -DENABLE_SHARED=OFF -DENABLE_STATIC=ON -DWITH_TURBOJPEG=OFF -DWITH_JAVA=OFF \
		-DREQUIRE_SIMD=ON -DCMAKE_ASM_NASM_COMPILER=nasm
}
library libjpeg-turbo

stage_libwebp() {
	cmake_veda libwebp -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
		-DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF \
		-DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=ON
}
library libwebp

# With Brotli, which gives it WOFF2 (WebKit then needs no library of its
# own for them); without HarfBuzz, which is built on it.
stage_freetype() {
	meson_veda freetype -Dbrotli=enabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=enabled -Dzlib=system \
		-Dtests=disabled
}
library freetype zlib libpng brotli

stage_harfbuzz() {
	meson_veda harfbuzz -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dchafa=disabled -Dicu=enabled \
		-Dfreetype=enabled -Dgraphite2=disabled -Dfontations=disabled -Dtests=disabled -Dintrospection=disabled \
		-Ddocs=disabled -Dutilities=disabled -Dbenchmark=disabled -Draster=disabled -Dvector=disabled \
		-Dgpu=disabled -Dsubset=disabled -Dwasm=disabled
}
library harfbuzz icu freetype

# Fonts in /system/fonts, configured in /system/etc/fonts; each program
# keeps the cache of what it found in the user's cache directory.
stage_fontconfig() {
	meson_veda fontconfig -Dxml-backend=libxml2 -Ddoc=disabled -Dnls=disabled -Dtests=disabled -Dtools=disabled \
		-Dcache-build=disabled -Diconv=disabled -Dfontations=disabled -Ddefault-fonts-dirs=/system/fonts \
		-Dbaseconfig-dir=/system/etc/fonts -Dconfig-dir=/system/etc/fonts/conf.d \
		-Dtemplate-dir=/system/share/fontconfig/conf.avail -Dxml-dir=/system/share/xml/fontconfig \
		-Dcache-dir=/tmp/fontconfig
}
library fontconfig freetype libxml2

stage_libgpg_error() {
	autoconf_veda libgpg-error configure --disable-nls --disable-doc --disable-tests --disable-languages \
		--enable-install-gpg-error-config
}
library libgpg-error

# Its random numbers from the kernel's generator (`getentropy`), its
# assembly for the architecture. It finds libgpg-error with the
# gpgrt-config libgpg-error installs, which knows the sysroot.
stage_libgcrypt() {
	GPGRT_CONFIG="$SYSTEM/bin/gpgrt-config" PKG_CONFIG_SYSROOT_DIR="$ROOT" \
		autoconf_veda libgcrypt configure --disable-doc --enable-random=getentropy --disable-jent-support
}
library libgcrypt libgpg_error

stage_libtasn1() {
	autoconf_veda libtasn1 configure --disable-doc --disable-valgrind-tests
}
library libtasn1

# Veda's target of OpenSSL's (Configurations/50-veda.conf, in the port's
# patch): its assembly for the architecture, static, without what loads
# code (engines, providers in modules) or what Veda has no use for. Its
# certificates are the system's (/system/etc/ssl/cert.pem).
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

# GIO's TLS on OpenSSL, as a static module that the program registers
# itself (`g_io_openssl_load`), and proxies from the environment.
stage_glib_networking() {
	# Its modules go where GIO's file says (giomoduledir), which pkgconf
	# would give with the sysroot before it, to install under it again: as
	# freedesktop's pkg-config has it, without.
	PKG_CONFIG_FDO_SYSROOT_RULES=1 meson_veda glib-networking -Dgnutls=disabled -Dopenssl=enabled \
		-Dlibproxy=disabled -Dgnome_proxy=disabled -Denvironment_proxy=enabled -Dtests=false -Dinstalled_tests=false
}
library glib-networking glib openssl

stage_libsoup() {
	meson_veda libsoup -Dgssapi=disabled -Dntlm=disabled -Dbrotli=enabled -Dzstd=disabled -Dtls_check=false \
		-Dintrospection=disabled -Dvapi=disabled -Ddocs=disabled -Ddoc_tests=false -Dtests=false \
		-Dautobahn=disabled -Dinstalled_tests=false -Dsysprof=disabled -Dfuzzing=disabled -Dpkcs11_tests=disabled
}
library libsoup glib libpsl nghttp2 brotli sqlite zlib

# The Khronos Group's EGL headers, as released, for libepoxy and WebKit.
stage_egl_registry() {
	mkdir -p "$SYSTEM/include"
	rm -rf "$SYSTEM/include/EGL" "$SYSTEM/include/KHR"
	cp -r "$SRC/egl-registry/api/EGL" "$SRC/egl-registry/api/KHR" "$SYSTEM/include/"
}
library egl-registry

# EGL and OpenGL ES's entry points, which it finds through EGL's
# eglGetProcAddress, linked in (the port's patch): Veda has no dlopen.
stage_libepoxy() {
	# The EGL headers are the system's, not a library's that pkg-config
	# would name.
	meson_veda libepoxy -Degl=yes -Dglx=no -Dx11=false -Dtests=false -Ddocs=false \
		"-Dc_args=$VEDA_CFLAGS -idirafter $SYSTEM/include"
}
library libepoxy egl_registry

stage_libxkbcommon() {
	meson_veda libxkbcommon -Denable-x11=false -Denable-wayland=false -Denable-docs=false -Denable-tools=false \
		-Denable-xkbregistry=false -Denable-bash-completion=false -Dxkb-config-root=/system/share/xkb
}
library libxkbcommon

stage_lcms2() {
	meson_veda lcms2 -Dutils=false -Dtests=disabled -Djpeg=disabled -Dtiff=disabled
}
library lcms2

# --- media: GStreamer and its codecs -------------------------------------------

# What the media's builds must have configured: `grep -q` for each `#define`
# a configured build must have, failing with the one it lacks (a configure
# test that failed without a word, an option that did not take).
expect_defines() {
	local file=$1 line
	shift
	for line in "$@"; do
		grep -qxF "#define $line" "$file" || { echo "$file: expected '#define $line'" >&2; exit 1; }
	done
}

# The Ogg container, for GStreamer's Ogg demuxer.
stage_libogg() {
	cmake_veda libogg -DBUILD_TESTING=OFF -DINSTALL_DOCS=OFF
}
library libogg

# Opus, for GStreamer's Opus parser (Opus in WebM, for Media Source
# Extensions); FFmpeg decodes it. Without the neural network's extensions,
# which only an encoder or a lossy network would use.
stage_opus() {
	meson_veda opus -Dtests=disabled -Ddocs=disabled -Dextra-programs=disabled -Ddeep-plc=disabled -Ddred=disabled \
		-Dosce=disabled
}
library opus

# dav1d, which FFmpeg decodes AV1 with, as the toolchain builds it for
# Videos' decoder (both bit depths, the architecture's assembly with its
# assembler, DAV1D_BINARIES), without its tests. It defines _GNU_SOURCE
# itself only on the systems it knows (Linux): without it, musl's headers
# hide clock_gettime and the rest of POSIX from its C99.
stage_dav1d() {
	printf '[binaries]\n%s\n' "$DAV1D_BINARIES" > "$BUILD/dav1d.cross"
	meson_veda dav1d --cross-file "$BUILD/dav1d.cross" -Denable_asm=true -Denable_tests=false -Denable_tools=false \
		-Dbitdepths=8,16 -Dtrim_dsp=true "-Dc_args=$VEDA_CFLAGS -D_GNU_SOURCE"
	expect_defines "$BUILD/dav1d/config.h" "HAVE_ASM 1" "CONFIG_8BPC 1" "CONFIG_16BPC 1"
}
library dav1d @DAV1D_BINARIES

# FFmpeg's decoders of the web's codecs, for gst-libav: LGPL (no
# --enable-gpl, nothing non-free: checked below), with the libraries
# gst-libav links (libavformat and libavfilter, with none of their
# demuxers or filters) and the resampler Opus's decoder uses, the
# architecture's assembly (FFMPEG_ARCH_OPTIONS), AV1 through dav1d; no
# programs, devices, network or scaler (GStreamer converts), nor libraries
# found by themselves.
FFMPEG_OPTIONS=(
	--enable-cross-compile --cross-prefix=$TARGET- --target-os=veda "${FFMPEG_ARCH_OPTIONS[@]}"
	--prefix=/system --enable-static --disable-shared --enable-pic --enable-pthreads
	--disable-debug --disable-autodetect --disable-everything --disable-programs --disable-doc
	--disable-network --disable-avdevice --disable-swscale
	--enable-libdav1d
	--enable-decoder=libdav1d,vp8,vp9,h264,aac,opus,vorbis,mp3float,flac
	--enable-parser=av1,vp8,vp9,h264,aac,opus,vorbis,mpegaudio,flac
)
stage_ffmpeg() {
	local b=$BUILD/ffmpeg tools=$BUILD/ffmpeg-tools
	rm -rf "$b" "$tools" && mkdir -p "$b" "$tools" && cd "$b"
	# The compiler with the libraries' flags, found on the path, so that the
	# configuration FFmpeg records (and the program carries) names no
	# directory of this machine's.
	printf '#!/bin/sh\nexec %q %s "$@"\n' "$CC_VEDA" "$VEDA_CFLAGS" > "$tools/$TARGET-gcc"
	chmod +x "$tools/$TARGET-gcc"
	PATH="$tools:$PATH" quiet configure.log "$SRC/ffmpeg/configure" "${FFMPEG_OPTIONS[@]}" \
		--pkg-config="$TARGET-pkg-config"
	expect_defines config.h "HAVE_PTHREADS 1" "${FFMPEG_ARCH_DEFINES[@]}" "CONFIG_GPL 0" "CONFIG_NONFREE 0" \
		"CONFIG_AVFORMAT 1" "CONFIG_AVFILTER 1"
	expect_defines config_components.h "CONFIG_LIBDAV1D_DECODER 1" "CONFIG_VP9_DECODER 1" "CONFIG_H264_DECODER 1" \
		"CONFIG_OPUS_DECODER 1"
	PATH="$tools:$PATH" quiet make.log make -j"$JOBS"
	quiet install.log make install DESTDIR="$ROOT"
	# dav1d's directory as pkg-config gave it, under this machine's sysroot:
	# its pkg-config file finds it as it finds the others.
	sed -i "s| -L$SYSTEM/lib | |" "$SYSTEM"/lib/pkgconfig/libav*.pc
}
library ffmpeg dav1d @FFMPEG_OPTIONS @FFMPEG_ARCH_DEFINES

# GStreamer's core, for static programs (gstreamer-static-full): gst_init
# registers the plugins linked in by calling the program's
# gst_init_static_plugins. No registry of plugins on disk (there are none
# to scan, nor the helper that would), no tools, no tests.
stage_gstreamer() {
	meson_veda gstreamer -Dgstreamer-static-full=true -Dregistry=false -Dptp-helper=disabled -Dcheck=disabled \
		-Dlibunwind=disabled -Dlibdw=disabled -Ddbghelp=disabled -Dbash-completion=disabled -Dcoretracers=disabled \
		-Dexamples=disabled -Dtests=disabled -Dbenchmarks=disabled -Dtools=disabled -Dintrospection=disabled \
		-Dnls=disabled -Ddoc=disabled
}
library gstreamer glib

# Each module's libraries, and only the plugins the web engine uses (every
# other feature off): playback and what it plugs in (finding types, Ogg,
# converting and resampling sound and pictures, volume, the application's
# sources and sinks), and GIO's streams (Web Audio decodes sound files from
# one); without OpenGL, X11 or ORC's run-time code.
stage_gst_plugins_base() {
	meson_veda gst-plugins-base -Dauto_features=disabled -Dapp=enabled -Daudioconvert=enabled \
		-Daudioresample=enabled -Dplayback=enabled -Dtypefind=enabled -Dvideoconvertscale=enabled -Dvolume=enabled \
		-Dogg=enabled -Dgio=enabled
}
library gst-plugins-base gstreamer libogg zlib

# MP4 and Matroska (WebM), the audio parsers, WAV and ID3 tags, the
# automatic audio sink (Veda's, the browser's), splitting channels (Web
# Audio's sources) and changing tempo without pitch (playback rates).
stage_gst_plugins_good() {
	meson_veda gst-plugins-good -Dauto_features=disabled -Disomp4=enabled -Dmatroska=enabled \
		-Daudioparsers=enabled -Dwavparse=enabled -Did3demux=enabled -Dautodetect=enabled -Dinterleave=enabled \
		-Daudiofx=enabled
}
library gst-plugins-good gst_plugins_base zlib

# The video parsers and Opus's, which Media Source Extensions put after
# their demuxers, WebVTT's encoder, which WebKit shows a video's own
# subtitles with, and the video sink that drops pictures, which a player of
# sound alone has (debugutils); nothing under the GPL.
stage_gst_plugins_bad() {
	meson_veda gst-plugins-bad -Dauto_features=disabled -Dgpl=disabled -Dvideoparsers=enabled -Dopus=enabled \
		-Dsubenc=enabled -Ddebugutils=enabled
}
library gst-plugins-bad gst_plugins_base opus

# FFmpeg's decoders as GStreamer's (avdec_*), dav1d's among them (the
# port's patch).
stage_gst_libav() {
	meson_veda gst-libav -Dtests=disabled -Ddoc=disabled
}
library gst-libav gst_plugins_base ffmpeg

# GStreamer for static programs, as GStreamer's own build makes it
# (gstreamer-full): gstreamer-full-1.0.pc, which WebKit is built with
# (USE_GSTREAMER_FULL) and the browser links, names GStreamer's libraries
# and the plugins above; libgstreamer-full-1.0.a holds
# gst_full_register_plugins(), which registers those plugins. GStreamer's
# core calls the program's gst_init_static_plugins() as it starts, which
# calls it, and registers the program's own elements (the browser's sink of
# Veda's audio service).
GST_PLUGINS=(
	coreelements
	app audioconvert audioresample gio ogg playback typefindfunctions videoconvertscale volume
	audiofx audioparsers autodetect id3demux interleave isomp4 matroska wavparse
	debugutilsbad opusparse subenc videoparsersbad
	libav
)
# The libraries WebKit uses itself, beside what the plugins need.
GST_LIBRARIES=(gstreamer-1.0 gstreamer-base-1.0 gstreamer-allocators-1.0 gstreamer-app-1.0 gstreamer-audio-1.0
	gstreamer-fft-1.0 gstreamer-pbutils-1.0 gstreamer-tag-1.0 gstreamer-video-1.0)
stage_gstreamer_full() {
	local b=$BUILD/gstreamer-full p libs="" requires
	rm -rf "$b" && mkdir -p "$b"
	{
		echo '/* The web engine'"'"'s GStreamer plugins, linked in (wpewebkit/build.sh). */'
		echo '#include <gst/gst.h>'
		for p in "${GST_PLUGINS[@]}"; do echo "GST_PLUGIN_STATIC_DECLARE($p);"; done
		echo 'void gst_full_register_plugins(void);'
		echo 'void gst_full_register_plugins(void)'
		echo '{'
		for p in "${GST_PLUGINS[@]}"; do printf '\tGST_PLUGIN_STATIC_REGISTER(%s);\n' "$p"; done
		echo '}'
	} > "$b/plugins.c"
	quiet "$b-compile.log" "$CC_VEDA" $VEDA_CFLAGS -Wall -Werror $("$TARGET-pkg-config" --cflags gstreamer-1.0) \
		-c -o "$b/plugins.o" "$b/plugins.c"
	rm -f "$SYSTEM/lib/libgstreamer-full-1.0.a"
	"$TARGET-ar" rcs "$SYSTEM/lib/libgstreamer-full-1.0.a" "$b/plugins.o"
	# What the plugins need, as their own files say (they are no packages
	# pkg-config looks for by name), and their archives.
	requires=$({
		printf '%s\n' "${GST_LIBRARIES[@]}"
		for p in "${GST_PLUGINS[@]}"; do
			"$TARGET-pkg-config" --print-requires "$SYSTEM/lib/gstreamer-1.0/pkgconfig/gst$p.pc"
		done
	} | cut -d' ' -f1 | sort -u | tr '\n' ' ')
	for p in "${GST_PLUGINS[@]}"; do
		libs+=" -lgst$p"
	done
	cat > "$SYSTEM/lib/pkgconfig/gstreamer-full-1.0.pc" <<-PC
	prefix=/system
	libdir=\${prefix}/lib
	includedir=\${prefix}/include

	Name: gstreamer-full-1.0
	Description: GStreamer, its libraries and the web engine's plugins, to link statically
	Version: $(port gstreamer version)
	Requires: $requires
	Libs: -L\${libdir} -lgstreamer-full-1.0 -L\${libdir}/gstreamer-1.0$libs -lm
	Cflags: -I\${includedir}/gstreamer-1.0
	PC
}
stage gstreamer_full gstreamer gst_plugins_base gst_plugins_good gst_plugins_bad gst_libav host_pkgconf \
	"${CONFIG_FILES[@]}" @GST_PLUGINS @GST_LIBRARIES @VEDA_CFLAGS @ROOT @TOOLCHAIN @TARGET

# What WebKit is compiled against of GStreamer: its libraries' headers and
# pkg-config files, which the plugins (and the patches of their ports) leave
# as they are. WebKit is built again for a change of these, not of those.
GST_HEADERS=$(cd "$SYSTEM" && find include/gstreamer-1.0 lib/pkgconfig -type f \( -name '*.h' -o -name 'gstreamer-*.pc' \) \
	! -name gstreamer-full-1.0.pc | LC_ALL=C sort | xargs sha256sum | sha256sum | cut -d' ' -f1)

# --- WPE WebKit -------------------------------------------------------------------

# WebKit's static libraries (libWPEWebKit-2.0.a, with the WPE platform
# library in it, and those of WebCore, JavaScriptCore, WTF, bmalloc and the
# third-party code it builds), which the browser links and whose processes
# it is (Source/WebKit/Shared/veda in the port's patch); its API's headers;
# and how to link it statically, wpe-webkit-static-2.0.pc (each library's
# own file says what it needs in turn). Skia paints pages on the CPU; the
# browser's settings choose whether WebKit composites them with OpenGL ES,
# through EGL's surfaceless platform (libepoxy finds both in the program,
# the egl library's), or paints each straight into shared memory. Media
# (<video>, <audio>, Media Source Extensions, Web Audio) is GStreamer's,
# static (gstreamer-full), its video painted on the CPU or composited
# without GStreamer's OpenGL. WebGL is ANGLE's, built into the engine
# (libANGLE, libGLESv2), in the web process: its OpenGL back end draws with
# the program's OpenGL ES, which it finds through the program's
# eglGetProcAddress, sharing objects with the compositor's. What it leaves
# out for now: WebRTC, WebCodecs, encrypted media and capture, the GPU
# process, and what is Linux's (bubblewrap, journald, DRM, Wayland).
WEBKIT_OPTIONS=(
	-DPORT=WPE -DCMAKE_BUILD_TYPE=Release
	-DENABLE_WPE_PLATFORM=ON -DENABLE_WPE_PLATFORM_HEADLESS=ON -DENABLE_WPE_PLATFORM_DRM=OFF
	-DENABLE_WPE_PLATFORM_WAYLAND=OFF -DENABLE_WPE_LEGACY_API=OFF -DENABLE_WPE_QT_API=OFF
	-DENABLE_DOCUMENTATION=OFF -DENABLE_INTROSPECTION=OFF -DENABLE_JOURNALD_LOG=OFF -DENABLE_BUBBLEWRAP_SANDBOX=OFF
	-DUSE_ATK=OFF -DUSE_FLITE=OFF -DENABLE_SPEECH_SYNTHESIS=OFF -DUSE_GBM=OFF -DUSE_LIBDRM=OFF -DUSE_LIBBACKTRACE=OFF
	-DUSE_LIBHYPHEN=OFF -DUSE_SYSPROF_CAPTURE=OFF -DUSE_SYSTEM_SYSPROF_CAPTURE=OFF -DUSE_SYSTEM_UNIFDEF=ON
	-DENABLE_SPELLCHECK=OFF -DENABLE_WEBDRIVER=OFF -DENABLE_MINIBROWSER=OFF -DENABLE_API_TESTS=OFF -DENABLE_COG=OFF
	-DUSE_GSTREAMER=ON -DUSE_GSTREAMER_FULL=ON -DUSE_GSTREAMER_GL=OFF -DUSE_GSTREAMER_WEBRTC=OFF -DENABLE_VIDEO=ON
	-DENABLE_WEB_AUDIO=ON -DENABLE_MEDIA_SOURCE=ON -DENABLE_MEDIA_STREAM=OFF -DENABLE_MEDIA_RECORDER=OFF
	-DENABLE_WEB_CODECS=OFF -DENABLE_ENCRYPTED_MEDIA=OFF -DENABLE_THUNDER=OFF -DENABLE_GAMEPAD=OFF -DENABLE_WEBGL=ON
	-DENABLE_WEBXR=OFF -DENABLE_MEDIA_SESSION=OFF -DENABLE_GPU_PROCESS=OFF -DUSE_AVIF=OFF -DUSE_JPEGXL=OFF
	-DUSE_LCMS=ON -DUSE_WOFF2=ON -DUSE_VULKAN=OFF -DENABLE_XSLT=ON -DENABLE_PDFJS=ON -DUSE_THIN_ARCHIVES=OFF
)
# Its parts' libraries, in the order they link: each needs the ones after it.
WEBKIT_LIBRARIES=(WPEWebKit-2.0 WebCore GLESv2 ANGLE PAL JavaScriptCore WTF bmalloc Skia xdgmime)
# What they need of the libraries above, beyond what WebKit's own file says
# (it has nothing to say of a static build), and GLib's modules for TLS and
# proxies, which the program registers (they are no shared modules here).
WEBKIT_REQUIRES=(
	wpe-webkit-2.0 wpe-platform-2.0 gio-unix-2.0 libsoup-3.0 icu-uc icu-i18n harfbuzz harfbuzz-icu
	freetype2 fontconfig libxml-2.0 libxslt sqlite3 libpng16 libjpeg libwebp libwebpdemux libwebpmux lcms2 epoxy
	xkbcommon libgcrypt libtasn1 libssl libcrypto libpsl libnghttp2 libbrotlidec libbrotlienc expat zlib
	gstreamer-full-1.0
)
# WebKit's C++ takes up to some 2.5 GB a compiler: no more of them than the
# memory holds.
WEBKIT_JOBS=$(awk -v jobs="$JOBS" '/^MemTotal:/ { n = int($2 / 2600000); if (n < 1) n = 1; print (n < jobs ? n : jobs) }' /proc/meminfo)

stage_webkit() {
	local b=$BUILD/webkit
	# WebKit's unifdef, for this machine: its build makes WPE's API headers
	# with it. It is C17 (`constexpr` is a name of its own in it).
	gcc -std=gnu17 -O2 -o "$HOST/bin/unifdef" "$SRC/wpewebkit/Source/ThirdParty/unifdef/unifdef.c"
	rm -rf "$b"
	quiet "$b-configure.log" cmake -S "$SRC/wpewebkit" -B "$b" -G Ninja --toolchain "$CMAKE_TOOLCHAIN" \
		-DCMAKE_INSTALL_PREFIX=/system -DCMAKE_INSTALL_LIBDIR=lib "${WEBKIT_OPTIONS[@]}"
	quiet "$b-build.log" ninja -C "$b" -j"$WEBKIT_JOBS" WebKit
	DESTDIR="$ROOT" quiet "$b-install.log" cmake --install "$b" --component Development
	local lib libs=""
	for lib in "${WEBKIT_LIBRARIES[@]}"; do
		install -m 644 "$b/lib/lib$lib.a" "$SYSTEM/lib/"
		libs+=" -l$lib"
	done
	cat > "$SYSTEM/lib/pkgconfig/wpe-webkit-static-2.0.pc" <<-EOF
	prefix=/system
	libdir=\${prefix}/lib

	Name: WPE WebKit (static)
	Description: WPE WebKit and what it needs, to link statically
	Version: $(port wpewebkit version)
	Requires: ${WEBKIT_REQUIRES[*]}
	Libs: -L\${libdir}$libs -L\${libdir}/gio/modules -lgioopenssl -lgioenvironmentproxy -lstdc++ -latomic -lm
	EOF
}
eval "stage_src_wpewebkit() { unpack wpewebkit; }"
stage src_wpewebkit +wpewebkit
stage webkit src_wpewebkit host_meson host_pkgconf host_gperf host_ruby host_glib glib libsoup glib_networking \
	icu harfbuzz freetype fontconfig expat libxml2 libxslt sqlite libpng libjpeg_turbo libwebp libgcrypt libtasn1 \
	libepoxy libxkbcommon lcms2 "${CONFIG_FILES[@]}" @GST_HEADERS @WEBKIT_OPTIONS @WEBKIT_LIBRARIES @WEBKIT_REQUIRES \
	@VEDA_CFLAGS @ROOT @TOOLCHAIN @TARGET

# --- the licences -------------------------------------------------------------------

# The libraries' licence texts, as their releases have them, under
# system/share/licences/PORT/ (the image's /system/licences/web): what
# their licences ask of whoever passes their binaries on. WebKit's are its
# parts' (WebCore's, JavaScriptCore's and WTF's LGPL and BSD licences) and
# those of the code of others it builds into the program (Skia; ANGLE, its
# xxHash and its borrowings from Chromium, whose terms are ANGLE's and
# whose notices are their sources' headers; PDF.js and what its viewer
# brings: Adobe's character maps, Foxit's and Liberation's fonts, and
# OpenJPEG and PDFium's JBIG2 decoder, built into JavaScript; WTF's
# borrowings, and xdgmime, whose terms are its sources' header). Each
# library above has its texts here, but for those whose licences ask for
# none: SQLite is in the public domain, and EGL's headers only name types.
LICENCE_FILES=(
	brotli:LICENSE dav1d:COPYING dav1d:doc/PATENTS expat:COPYING ffmpeg:LICENSE.md ffmpeg:COPYING.LGPLv2.1
	fontconfig:COPYING freetype:LICENSE.TXT freetype:docs/FTL.TXT glib:COPYING glib:LICENSES
	glib-networking:COPYING glib-networking:LICENSE_EXCEPTION gst-libav:COPYING gst-plugins-bad:COPYING
	gst-plugins-base:COPYING gst-plugins-good:COPYING gstreamer:COPYING harfbuzz:COPYING icu:LICENSE
	lcms2:LICENSE libepoxy:COPYING libffi:LICENSE libgcrypt:COPYING.LIB libgcrypt:LICENSES
	libgpg-error:COPYING.LIB libjpeg-turbo:LICENSE.md libjpeg-turbo:README.ijg libogg:COPYING libpng:LICENSE
	libpsl:COPYING libsoup:COPYING libtasn1:COPYING.LESSERv2 libwebp:COPYING libwebp:PATENTS libxkbcommon:LICENSE
	libxml2:Copyright libxslt:Copyright nghttp2:COPYING openssl:LICENSE.txt opus:COPYING pcre2:LICENCE.md
	zlib:LICENSE
	wpewebkit:Source/WebCore/LICENSE-LGPL-2 wpewebkit:Source/WebCore/LICENSE-LGPL-2.1
	wpewebkit:Source/WebCore/LICENSE-APPLE wpewebkit:Source/ThirdParty/skia/LICENSE
	wpewebkit:Source/ThirdParty/ANGLE/LICENSE wpewebkit:Source/ThirdParty/ANGLE/src/common/third_party/xxhash/LICENSE
	wpewebkit:Source/ThirdParty/pdfjs/LICENSE wpewebkit:Source/ThirdParty/pdfjs/web/cmaps/LICENSE
	wpewebkit:Source/ThirdParty/pdfjs/web/standard_fonts/LICENSE_FOXIT
	wpewebkit:Source/ThirdParty/pdfjs/web/standard_fonts/LICENSE_LIBERATION
	wpewebkit:Source/ThirdParty/pdfjs/web/wasm/LICENSE_OPENJPEG
	wpewebkit:Source/ThirdParty/pdfjs/web/wasm/LICENSE_PDFJS_OPENJPEG
	wpewebkit:Source/ThirdParty/pdfjs/web/wasm/LICENSE_JBIG2
	wpewebkit:Source/ThirdParty/pdfjs/web/wasm/LICENSE_PDFJS_JBIG2 wpewebkit:Source/WTF/LICENSE-dragonbox.txt
	wpewebkit:Source/WTF/LICENSE-libc++.txt wpewebkit:Source/WTF/LICENSE-LLVM.txt
	wpewebkit:Source/WTF/LICENSE-simde.txt
)
LICENCE_FREE=(sqlite egl-registry)

stage_licences() {
	local dir=$SYSTEM/share/licences entry port file
	rm -rf "$dir"
	for port in "${LIBRARIES[@]}"; do
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
	# xdgmime's terms, from its sources' header.
	mkdir -p "$dir/wpewebkit/Source/ThirdParty/xdgmime"
	sed -n '1,/\*\//p' "$SRC/wpewebkit/Source/ThirdParty/xdgmime/src/xdgmime.c" \
		> "$dir/wpewebkit/Source/ThirdParty/xdgmime/COPYING"
	# The notices of the Chromium code ANGLE builds in (its base library's
	# SHA-1, and zlib's wrapper), from their sources' headers: their terms
	# are the BSD licence of ANGLE's LICENSE.
	local angle=$SRC/wpewebkit/Source/ThirdParty/ANGLE
	{
		sed -n '1,3p' "$angle/src/common/base/anglebase/sha1.cc"
		echo
		sed -n '1,/\*\//p' "$angle/third_party/zlib/google/compression_utils_portable.cc"
	} > "$dir/wpewebkit/Source/ThirdParty/ANGLE/CHROMIUM"
}
LICENCE_SOURCES=(src_wpewebkit)
for port in "${LIBRARIES[@]}"; do
	LICENCE_SOURCES+=("src_${port//-/_}")
done
stage licences "${LICENCE_SOURCES[@]}" @LICENCE_FILES @LICENCE_FREE @ROOT

log Finished "the web engine ($ROOT)"
