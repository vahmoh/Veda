# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# The helpers the build scripts of the ports share (`build.sh`,
# `linux/build.sh` and `crates.sh`). They expect PORTS (the ports
# directory), VEDA_ARCH (the architecture built for), DOWNLOADS (the
# verified archives), SRC (where ports are unpacked) and STAMPS (the
# stages' fingerprints).

log() { printf '\033[1;32m%12s\033[0m %s\n' "$1" "$2"; }

# The value of KEY in implementation/ports/NAME/port.toml.
port() {
	sed -n "s/^$2 *= *\"\(.*\)\"/\1/p" "$PORTS/$1/port.toml"
}

# Veda's patches of port NAME, those it has: its own (NAME/veda.patch),
# then the architecture's (NAME/arch/$VEDA_ARCH/veda.patch).
patches() {
	local p
	for p in "$PORTS/$1/veda.patch" "$PORTS/$1/arch/$VEDA_ARCH/veda.patch"; do
		if [ -f "$p" ]; then echo "$p"; fi
	done
}

# Runs stage NAME (a shell function stage_NAME) unless its fingerprint is
# unchanged. Other arguments are what it depends on: files, stages, ports
# as Veda builds them (+NAME: the release and Veda's patches) and
# variables it uses (@NAME).
stage() {
	local name=$1
	shift
	local print
	print=$({
		declare -f "stage_$name"
		for dep in "$@"; do
			case "$dep" in
			@*) declare -p "${dep#@}" ;;
			+*)
				sha256sum "$PORTS/${dep#+}/port.toml"
				patches "${dep#+}" | while IFS= read -r p; do sha256sum "$p"; done
				;;
			*) if [ -f "$dep" ]; then sha256sum "$dep"; else cat "$STAMPS/$dep"; fi ;;
			esac
		done
	} | sha256sum | cut -d' ' -f1)
	if [ -f "$STAMPS/$name" ] && [ "$(cat "$STAMPS/$name")" = "$print" ]; then
		log Fresh "$name"
		return
	fi
	log Building "$name"
	rm -f "$STAMPS/$name"
	"stage_$name"
	echo "$print" > "$STAMPS/$name"
}

# Runs a command with its output in LOG, showing the end of it on failure.
quiet() {
	local logfile=$1
	shift
	if ! "$@" > "$logfile" 2>&1; then
		tail -n 40 "$logfile"
		echo "failed: $* (log: $logfile)" >&2
		exit 1
	fi
}

# Unpacks port NAME into $SRC/NAME (its archive's one top directory: most
# are NAME-VERSION, Rust's is rustc-VERSION-src) and applies its patches
# (`patches`); with `upstream`, as its authors released it (the patches
# are for Veda).
unpack() {
	local name=$1 archive dir top p
	archive=$DOWNLOADS/$(basename "$(port "$name" url)")
	dir=$SRC/$name
	rm -rf "$dir" "$dir.tmp"
	mkdir -p "$dir.tmp"
	tar -xf "$archive" -C "$dir.tmp"
	top=("$dir.tmp"/*)
	if [ ${#top[@]} -ne 1 ] || [ ! -d "${top[0]}" ]; then
		echo "$archive: not one top directory" >&2
		exit 1
	fi
	mv "${top[0]}" "$dir"
	rmdir "$dir.tmp"
	if [ "${2:-}" != upstream ]; then
		while IFS= read -r p; do
			(cd "$dir" && patch -p1 --batch --forward --quiet < "$p")
		done < <(patches "$name")
	fi
}
