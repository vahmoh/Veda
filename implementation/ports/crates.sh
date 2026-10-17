#!/bin/bash
# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Prepares the crates among the ports for Cargo: unpacks each one's
# release and applies its patches, as Cargo then builds it in place of the
# release. `cargo orchestration` runs it, once it has downloaded and verified
# the releases, whenever one of these ports has changed; see
# orchestration/src/crates.rs and "Crates" in README.md.
#
# Arguments: the crates (the names of their ports).
#
# Environment:
#   VEDA_PORTS   this directory
#   VEDA_ARCH    the architecture built for
#                (architectures/$VEDA_ARCH/veda.patch)
#   VEDA_CRATES  where to prepare them; downloads/ holds the verified archives
#
# Results, under $VEDA_CRATES: src/<crate>, each crate as Veda builds it.
# Like the stages of build.sh, each keeps a fingerprint of its release and
# patches, and is unpacked again only when they change.

set -euo pipefail

PORTS=${VEDA_PORTS:?}
VEDA_ARCH=${VEDA_ARCH:?}
ROOT=${VEDA_CRATES:?}

DOWNLOADS=$ROOT/downloads
SRC=$ROOT/src
STAMPS=$ROOT/stamps

mkdir -p "$SRC" "$STAMPS"

# log, port, patches, stage and unpack.
source "$PORTS/common.sh"

for p in "$@"; do
	eval "stage_src_$p() { unpack $p; }"
	stage "src_$p" "+$p"
done
