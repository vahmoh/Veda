#!/bin/bash
# Builds Veda's C toolchain from the ports in this directory. `cargo xtask
# toolchain` runs it after downloading and
# verifying the sources; see xtask/src/toolchain.rs and docs/C.md.
#
# Environment:
#   VEDA_PORTS       this directory
#   VEDA_TOOLCHAIN   where to build; downloads/ holds the verified archives
#   VEDA_POSIX_LIB   the POSIX layer (lib/posix) as a static library
#   VEDA_POSIX_EXPORTS  the symbols it gives the C library
#   JOBS             parallel jobs (default: the number of processors)
#
# Results, under $VEDA_TOOLCHAIN:
#   cross/           the cross toolchain (x86_64-veda-gcc, -g++ ...) for this
#                    machine; cross/sysroot is laid out like Veda's /system
#   native/system/   the toolchain built for Veda, as /system has it
#
# Every stage keeps a fingerprint of what went into it (its commands, the
# patches, the stages before it) and is skipped while that is unchanged.
# Build trees keep absolute paths, so the place itself is part of it: a
# toolchain directory that has moved is built again.

set -euo pipefail

TARGET=x86_64-veda
PORTS=${VEDA_PORTS:?}
ROOT=${VEDA_TOOLCHAIN:?}
POSIX_LIB=${VEDA_POSIX_LIB:?}
POSIX_EXPORTS=${VEDA_POSIX_EXPORTS:?}
JOBS=${JOBS:-$(nproc)}

DOWNLOADS=$ROOT/downloads
SRC=$ROOT/src
BUILD=$ROOT/build
STAMPS=$BUILD/stamps
CROSS=$ROOT/cross
SYSROOT=$CROSS/sysroot
NATIVE=$ROOT/native
DEPS=$BUILD/host-deps

mkdir -p "$SRC" "$BUILD" "$STAMPS" "$CROSS"
export PATH="$CROSS/bin:$PATH"

BUILD_TRIPLE=$(gcc -dumpmachine)

# log, port, stage, quiet and unpack.
source "$PORTS/common.sh"

for p in binutils gcc gmp mpfr mpc musl; do
	eval "stage_src_$p() { unpack $p; }"
	stage "src_$p" "$PORTS/$p/port.toml" "$PORTS/$p/veda.patch"
done

# --- the cross toolchain -----------------------------------------------------

BINUTILS_OPTIONS=(
	--disable-nls --disable-werror --disable-gdb --disable-gdbserver --disable-sim
	--disable-readline --disable-libdecnumber --disable-gprofng --disable-plugins
	--disable-libctf --disable-multilib --enable-deterministic-archives
)

stage_binutils_cross() {
	local b=$BUILD/binutils-cross
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/binutils/configure" --target=$TARGET --prefix="$CROSS" \
		--with-sysroot="$SYSROOT" "${BINUTILS_OPTIONS[@]}" MAKEINFO=true
	quiet make.log make -j"$JOBS" MAKEINFO=true
	quiet install.log make install MAKEINFO=true
}
stage binutils_cross src_binutils @BINUTILS_OPTIONS @ROOT

# GCC's limits.h wraps the C library's only if it sees it: the headers
# come first.
stage_musl_headers() {
	local b=$BUILD/musl-headers
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet install.log make -f "$SRC/musl/Makefile" srcdir="$SRC/musl" ARCH=x86_64 prefix=/system \
		DESTDIR="$SYSROOT" install-headers
}
stage musl_headers src_musl

GCC_OPTIONS=(
	--disable-nls --disable-multilib --disable-shared
	--enable-threads=posix --enable-tls --enable-initfini-array
	--disable-libssp --disable-libquadmath --disable-libgomp --disable-libatomic
	--disable-libsanitizer --disable-libvtv --disable-libitm --disable-lto
	--disable-plugin --disable-libstdcxx-pch --without-isl --without-zstd --disable-bootstrap
	gcc_cv_libc_provides_ssp=yes
	# Position-independent libraries (libgcc, libstdc++), as the C library
	# is, so that -static-pie programs can use them too.
	"CFLAGS_FOR_TARGET=-g -O2 -fPIE" "CXXFLAGS_FOR_TARGET=-g -O2 -fPIE"
)

# C and C++: GCC is written in C++, so building the native GCC takes a C++
# compiler for Veda, and its library.
stage_gcc_cross() {
	local b=$BUILD/gcc-cross
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/gcc/configure" --target=$TARGET --prefix="$CROSS" \
		--with-sysroot="$SYSROOT" --enable-languages=c,c++ "${GCC_OPTIONS[@]}" MAKEINFO=true
	quiet make-gcc.log make -j"$JOBS" all-gcc MAKEINFO=true
	quiet install-gcc.log make install-gcc MAKEINFO=true
	quiet make-libgcc.log make -j"$JOBS" all-target-libgcc MAKEINFO=true
	quiet install-libgcc.log make install-target-libgcc MAKEINFO=true
}
stage gcc_cross src_gcc binutils_cross musl_headers @GCC_OPTIONS @ROOT

# Position-independent code, as systems built on musl have it, so that the
# one libc.a links both fixed-address and position-independent
# (-static-pie) programs.
stage_musl() {
	local b=$BUILD/musl
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log env CC=$TARGET-gcc "$SRC/musl/configure" --target=$TARGET --prefix=/system \
		--syslibdir=/system/lib --disable-shared --enable-static CFLAGS=-fPIE
	quiet make.log make -j"$JOBS"
	quiet install.log make DESTDIR="$SYSROOT" install
}
stage musl src_musl gcc_cross

# The POSIX layer changes more often than the rest: it is added last, as
# one object that exports its interface and nothing else (no Rust or
# compiler-builtins symbol can clash with the C library's). `cargo xtask`
# refreshes it in the same way when it changes.
stage_posix_layer() {
	local b=$BUILD/posix-layer
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	local keep=() undefined=()
	for s in $POSIX_EXPORTS; do
		undefined+=(-u "$s")
		keep+=("--keep-global-symbol=$s")
	done
	$TARGET-ld -r --gc-sections "${undefined[@]}" -o veda-all.o "$POSIX_LIB"
	$TARGET-objcopy --strip-debug --remove-section=.llvmbc --remove-section=.llvmcmd "${keep[@]}" \
		veda-all.o "$BUILD/veda.o"
	$TARGET-ar rcs "$SYSROOT/system/lib/libc.a" "$BUILD/veda.o"
	# And its header for Veda's own interfaces, <veda/ipc.h>.
	mkdir -p "$SYSROOT/system/include/veda"
	cp "$PORTS"/../lib/posix/include/veda/*.h "$SYSROOT/system/include/veda/"
}
stage posix_layer musl "$POSIX_LIB" @POSIX_EXPORTS

# The C++ library checks the C library by linking programs with it, so it
# comes once the C library is complete (the POSIX layer's content does not
# matter to it).
stage_libstdcxx_cross() {
	cd "$BUILD/gcc-cross"
	quiet make-libstdcxx.log make -j"$JOBS" all-target-libstdc++-v3 MAKEINFO=true
	quiet install-libstdcxx.log make install-target-libstdc++-v3 MAKEINFO=true
}
stage libstdcxx_cross gcc_cross musl

# --- the media libraries, for mediadec ---------------------------------------

# dav1d and FFmpeg, static and position independent, for the sandboxed
# decoder of Videos (services/mediadec, which `cargo xtask` links). They go
# to a prefix of their own: the sysroot is copied into the image, these are
# not. They are compiled with the C library as built (they link nothing but
# their checkasm programs), so a new POSIX layer only relinks those.
MEDIA=$ROOT/media

stage_src_dav1d() { unpack dav1d; }
stage src_dav1d "$PORTS/dav1d/port.toml"
stage_src_ffmpeg() { unpack ffmpeg; }
stage src_ffmpeg "$PORTS/ffmpeg/port.toml" "$PORTS/ffmpeg/veda.patch"

# `grep -q` for each `#define` a configured build must have: a check that
# fails says which (a configure test that failed without a word, under GCC's
# C23 default, say).
expect_defines() {
	local file=$1 line
	shift
	for line in "$@"; do
		grep -qxF "#define $line" "$file" || { echo "$file: expected '#define $line'" >&2; exit 1; }
	done
}

# Both bit depths (10-bit video is common), the assembly (nasm, ELF64), and
# the tests, for checkasm, which compares each assembly function with the C
# one: a release build would leave those out (trim_dsp).
DAV1D_OPTIONS=(
	--prefix=/system --libdir=lib --buildtype=release --default-library=static -Db_pie=true
	-Denable_asm=true -Denable_tests=true -Denable_tools=false -Dbitdepths=8,16 -Dtrim_dsp=false
)

stage_dav1d() {
	local b=$BUILD/dav1d
	rm -rf "$b" "$MEDIA/dav1d" && mkdir -p "$b"
	cat > "$BUILD/dav1d.cross" <<-EOF
	[binaries]
	c = '$CROSS/bin/$TARGET-gcc'
	ar = '$CROSS/bin/$TARGET-ar'
	strip = '$CROSS/bin/$TARGET-strip'
	nasm = 'nasm'
	pkg-config = 'false'
	[host_machine]
	system = 'veda'
	cpu_family = 'x86_64'
	cpu = 'x86_64'
	endian = 'little'
	[built-in options]
	# dav1d defines _GNU_SOURCE itself only on the systems it knows (Linux):
	# without it, musl's headers under -std=c99 hide clock_gettime and the
	# rest of POSIX.
	c_args = ['-D_GNU_SOURCE']
	EOF
	quiet "$b-setup.log" meson setup "$b" "$SRC/dav1d" --cross-file "$BUILD/dav1d.cross" "${DAV1D_OPTIONS[@]}"
	expect_defines "$b/config.h" "HAVE_ASM 1" "CONFIG_8BPC 1" "CONFIG_16BPC 1"
	quiet "$b-build.log" ninja -C "$b" -j"$JOBS"
	quiet "$b-install.log" env DESTDIR="$MEDIA/dav1d" meson install -C "$b" --no-rebuild
}
stage dav1d src_dav1d gcc_cross musl @DAV1D_OPTIONS @ROOT

# LGPL (no --enable-gpl, nothing non-free: checked below), only what
# mediadec decodes: the demuxers, decoders (AV1 through dav1d), parsers and
# the scaler and resampler; no programs, devices, filters, network or
# libraries found by themselves.
FFMPEG_OPTIONS=(
	--enable-cross-compile --cross-prefix=$TARGET- --arch=x86_64 --target-os=veda
	--prefix=/system --enable-static --disable-shared --enable-pic --enable-pthreads
	--disable-debug --disable-autodetect --disable-everything --disable-programs --disable-doc
	--disable-network --disable-avdevice --disable-avfilter --x86asmexe=nasm
	--enable-libdav1d
	--enable-demuxer=mov,matroska,ogg,avi,mpegts,mpegps
	--enable-decoder=libdav1d,vp9,vp8,h264,hevc,mpeg4,mpeg1video,mpeg2video
	--enable-decoder=aac,aac_latm,opus,vorbis,mp3float,mp2float,flac,ac3,eac3,'pcm_*'
	--enable-parser=av1,vp9,vp8,h264,hevc,mpeg4video,mpegvideo,aac,aac_latm,opus,vorbis,mpegaudio,flac,ac3
	--enable-bsf=extract_extradata
)

stage_ffmpeg() {
	local b=$BUILD/ffmpeg
	rm -rf "$b" "$MEDIA/ffmpeg" && mkdir -p "$b" "$BUILD/ffmpeg-tools"
	# A pkg-config that knows only dav1d, as built above.
	cat > "$BUILD/ffmpeg-tools/pkg-config" <<-EOF
	#!/bin/sh
	pkg=
	for a; do case "\$a" in -*) ;; *) pkg=\${pkg:-\$a};; esac; done
	case "\$pkg" in ""|dav1d*) ;; *) exit 1;; esac
	for a; do case "\$a" in
	--version) echo 0.29.2; exit 0;;
	--modversion) echo $(port dav1d version); exit 0;;
	--cflags) echo "-I$MEDIA/dav1d/system/include"; exit 0;;
	--libs) echo "-L$MEDIA/dav1d/system/lib -ldav1d -pthread"; exit 0;;
	esac; done
	[ -n "\$pkg" ]
	EOF
	chmod +x "$BUILD/ffmpeg-tools/pkg-config"
	cd "$b"
	quiet configure.log "$SRC/ffmpeg/configure" "${FFMPEG_OPTIONS[@]}" \
		--pkg-config="$BUILD/ffmpeg-tools/pkg-config"
	expect_defines config.h "HAVE_PTHREADS 1" "HAVE_X86ASM 1" "HAVE_AVX2_EXTERNAL 1" \
		"HAVE_SCHED_GETAFFINITY 1" "HAVE_POSIX_MEMALIGN 1" "CONFIG_GPL 0" "CONFIG_NONFREE 0" \
		"CONFIG_SWSCALE 1" "CONFIG_SWRESAMPLE 1"
	expect_defines config_components.h "CONFIG_LIBDAV1D_DECODER 1" "CONFIG_H264_DECODER 1" \
		"CONFIG_MATROSKA_DEMUXER 1"
	quiet make.log make -j"$JOBS"
	quiet install.log make install DESTDIR="$MEDIA/ffmpeg"
}
stage ffmpeg src_ffmpeg dav1d gcc_cross musl @FFMPEG_OPTIONS @ROOT

# Both projects' checkasm, which checks every assembly function against its
# C version: run inside Veda by systest (it checks the processor's state as
# Veda keeps it). Linked with the C library as it is now.
stage_media_tests() {
	rm -rf "$MEDIA/tests" && mkdir -p "$MEDIA/tests"
	rm -f "$BUILD/dav1d/tests/checkasm" "$BUILD/ffmpeg/tests/checkasm/checkasm"
	quiet "$BUILD/dav1d-checkasm.log" ninja -C "$BUILD/dav1d" tests/checkasm
	quiet "$BUILD/ffmpeg-checkasm.log" make -C "$BUILD/ffmpeg" -j"$JOBS" checkasm
	$TARGET-strip --strip-all -o "$MEDIA/tests/checkasm-dav1d" "$BUILD/dav1d/tests/checkasm"
	$TARGET-strip --strip-all -o "$MEDIA/tests/checkasm-ffmpeg" "$BUILD/ffmpeg/tests/checkasm/checkasm"
}
stage media_tests dav1d ffmpeg posix_layer

# --- the native toolchain ----------------------------------------------------

# The native programs are compiled once; when only the POSIX layer changed,
# they are linked again (the `native` stage), not rebuilt.

# GMP, MPFR and MPC for Veda (GCC is linked with them).
stage_host_deps() {
	rm -rf "$DEPS"
	local b
	for p in gmp mpfr mpc; do
		b=$BUILD/$p-host
		rm -rf "$b" && mkdir -p "$b" && cd "$b"
		local extra=()
		[ $p = gmp ] || extra+=(--with-gmp="$DEPS")
		[ $p = mpc ] && extra+=(--with-mpfr="$DEPS")
		# GMP 6.3's configure checks are not C23, GCC's default since 15
		# (`void g(){}` called with arguments): C17 for GMP.
		[ $p = gmp ] && extra+=(CC="$TARGET-gcc -std=gnu17" CC_FOR_BUILD="gcc -std=gnu17")
		quiet configure.log "$SRC/$p/configure" --host=$TARGET --build="$BUILD_TRIPLE" --prefix="$DEPS" \
			--disable-shared --enable-static "${extra[@]}" MAKEINFO=true
		quiet make.log make -j"$JOBS" MAKEINFO=true
		quiet install.log make install MAKEINFO=true
	done
}
stage host_deps src_gmp src_mpfr src_mpc musl @ROOT

stage_binutils_native() {
	local b=$BUILD/binutils-native
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/binutils/configure" --build="$BUILD_TRIPLE" --host=$TARGET --target=$TARGET \
		--prefix=/system --with-lib-path=/system/lib --disable-install-libbfd "${BINUTILS_OPTIONS[@]}" \
		MAKEINFO=true
	quiet make.log make -j"$JOBS" MAKEINFO=true
}
stage binutils_native src_binutils binutils_cross musl @BINUTILS_OPTIONS

# C inside Veda, for now. It finds `as` and `ld` in /system/bin
# (MD_EXEC_PREFIX in gcc/config/veda.h: --with-as and --with-ld would want
# them on this machine). GCC raises its own stack limit to 64 MiB where it
# can (deeply nested code needs it); a Veda program's stack size comes from
# its file instead.
stage_gcc_native() {
	local b=$BUILD/gcc-native
	rm -rf "$b" && mkdir -p "$b" && cd "$b"
	quiet configure.log "$SRC/gcc/configure" --build="$BUILD_TRIPLE" --host=$TARGET --target=$TARGET \
		--prefix=/system --with-build-sysroot="$SYSROOT" \
		--with-gmp="$DEPS" --with-mpfr="$DEPS" --with-mpc="$DEPS" --enable-languages=c "${GCC_OPTIONS[@]}" \
		MAKEINFO=true LDFLAGS="-Wl,-z,stack-size=$((64 << 20))"
	quiet make-gcc.log make -j"$JOBS" all-gcc MAKEINFO=true
	quiet make-libgcc.log make -j"$JOBS" all-target-libgcc MAKEINFO=true
}
stage gcc_native src_gcc host_deps binutils_cross gcc_cross libstdcxx_cross musl @GCC_OPTIONS @ROOT

# /system as the image installs it: the programs (stripped), GCC's own
# files, the C library and its headers; no documentation, no duplicates.
# The native programs are linked here, with the current C library and
# POSIX layer.
stage_native() {
	local b=$BUILD/binutils-native
	rm -f "$b"/binutils/{addr2line,ar,cxxfilt,nm-new,objcopy,objdump,ranlib,readelf,size,strings,strip-new} \
		"$b"/gas/as-new "$b"/ld/ld-new
	quiet "$b/relink.log" make -C "$b" -j"$JOBS" MAKEINFO=true
	rm -rf "$BUILD/binutils-native-image"
	quiet "$b/install.log" make -C "$b" install MAKEINFO=true DESTDIR="$BUILD/binutils-native-image"
	local g=$BUILD/gcc-native
	rm -f "$g"/gcc/{xgcc,cpp,cc1,collect2,lto-wrapper,gcov,gcov-dump,gcov-tool,gcc-ar,gcc-nm,gcc-ranlib}
	quiet "$g/relink.log" make -C "$g" -j"$JOBS" all-gcc MAKEINFO=true
	rm -rf "$BUILD/gcc-native-image"
	quiet "$g/install-gcc.log" make -C "$g" install-gcc MAKEINFO=true DESTDIR="$BUILD/gcc-native-image"
	quiet "$g/install-libgcc.log" make -C "$g" install-target-libgcc MAKEINFO=true \
		DESTDIR="$BUILD/gcc-native-image"
	rm -rf "$NATIVE"
	mkdir -p "$NATIVE/system/bin"
	local bu=$BUILD/binutils-native-image/system
	for tool in as ld ar nm objcopy objdump ranlib readelf size strings strip addr2line c++filt; do
		local from=$bu/bin/$tool
		[ "$tool" = ld ] && from=$bu/bin/ld.bfd
		cp "$from" "$NATIVE/system/bin/$tool"
	done
	local gi=$BUILD/gcc-native-image/system
	cp "$gi/bin/gcc" "$NATIVE/system/bin/gcc"
	cp "$gi/bin/gcc" "$NATIVE/system/bin/cc"
	cp "$gi/bin/cpp" "$NATIVE/system/bin/cpp"
	mkdir -p "$NATIVE/system/libexec/gcc/$TARGET" "$NATIVE/system/lib/gcc/$TARGET"
	local version
	version=$(port gcc version)
	cp -r "$gi/libexec/gcc/$TARGET/$version" "$NATIVE/system/libexec/gcc/$TARGET/"
	cp -r "$gi/lib/gcc/$TARGET/$version" "$NATIVE/system/lib/gcc/$TARGET/"
	rm -rf "$NATIVE/system/libexec/gcc/$TARGET/$version/install-tools" \
		"$NATIVE/system/lib/gcc/$TARGET/$version/install-tools" \
		"$NATIVE/system/lib/gcc/$TARGET/$version/plugin"
	rm -f "$NATIVE/system/libexec/gcc/$TARGET/$version/lto-wrapper"
	find "$NATIVE/system" -name '*.la' -delete
	cp -r "$SYSROOT/system/include" "$NATIVE/system/include"
	cp -r "$SYSROOT/system/lib/." "$NATIVE/system/lib/"
	# Programs only: libraries keep their symbols for the linker.
	find "$NATIVE/system/bin" "$NATIVE/system/libexec" -type f | while read -r f; do
		if head -c 4 "$f" | grep -q ELF; then $TARGET-strip --strip-all "$f"; fi
	done
}
stage native binutils_native gcc_native posix_layer

log Finished "the C toolchain ($ROOT)"
