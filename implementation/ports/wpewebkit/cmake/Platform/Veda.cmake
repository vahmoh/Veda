# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

# Veda, for CMake (CMAKE_SYSTEM_NAME Veda): a Unix system whose programs
# are linked statically, by GCC and GNU ld, with musl. CMake finds this
# file on CMAKE_MODULE_PATH, which the toolchain file of
# implementation/ports/wpewebkit/build.sh sets.

# No shared libraries and no dlopen: what would link them links the static
# archives instead.
set(CMAKE_DL_LIBS "")
set_property(GLOBAL PROPERTY TARGET_SUPPORTS_SHARED_LIBS FALSE)
set(CMAKE_FIND_LIBRARY_SUFFIXES ".a")
set(CMAKE_EXE_EXPORTS_C_FLAG "-Wl,--export-dynamic")

# Linking a group of static libraries until nothing is left undefined.
set(CMAKE_LINK_GROUP_USING_RESCAN "LINKER:--start-group" "LINKER:--end-group")
set(CMAKE_LINK_GROUP_USING_RESCAN_SUPPORTED TRUE)

include(Platform/UnixPaths)
