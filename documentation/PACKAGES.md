<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Packages

Veda installs software beyond its system image as *packages*, from
*repositories* whose keys sign what they offer. A system service,
`packages`, alone changes what is installed; the `pkg` command asks it to,
in the Terminal. Every change is planned, shown, and then made whole or
not at all, even when the power goes in the middle of it. Every Veda,
the live system on a USB stick too, has a repository of its own in its
system image, which it finds at every start without a network.

This document describes how packages are used and made, their formats,
who may do what, how a change is made and recovered from, what the system
image holds, and why it is so. The code is `implementation/libraries/packages`
(what is decided, tested on the host), `implementation/services/packages`
(the service) and `implementation/applications/pkg` (the command); the
launcher in `init` lists and starts the applications packages install
(`implementation/services/init`, its rules in `init-core`).

**Contents:** [Using packages](#using-packages) · [How it fits together](#how-it-fits-together) · [Packages](#packages-1) · [Repositories](#repositories) · [The system image's repository](#the-system-images-repository) · [Working out a change](#working-out-a-change) · [Making a change, and recovering](#making-a-change-and-recovering) · [Security](#security) · [Limits](#limits) · [Decisions](#decisions) · [Testing](#testing) · [What comes next](#what-comes-next)

## Using packages

In the Terminal:

| Command | What it does |
|---------|--------------|
| `pkg update` | Fetches every enabled repository's index anew, and checks it |
| `pkg search [TEXT]` | Lists the packages offered, the newest version of each (those whose name or summary holds `TEXT`) |
| `pkg list [--upgradable]` | Lists the packages installed (those installed only for others say so), or those a newer version of is offered |
| `pkg info NAME` | What is known of a package: the version installed, the one offered, what they depend on |
| `pkg files NAME` | The files of a package installed |
| `pkg install [-y] PACKAGE...` | Installs packages (`hello`, `hello=1.0`, `'hello (>= 2)'`), with what they depend on |
| `pkg remove [-y] NAME...` | Removes packages, with what was installed only for them |
| `pkg upgrade [-y] [NAME...]` | Upgrades packages (every one, if none is named) |
| `pkg verify [NAME]` | Checks the files installed against what was installed, and that nothing else is there |
| `pkg clean` | Removes the downloaded package files kept (a change that failed keeps its downloads, for the next try) |
| `pkg status` | What the service is doing |
| `pkg repo list` | The repositories, in the order they are preferred, with their state |
| `pkg repo add NAME URL KEY... [--title TITLE]` | Adds a repository, trusted with the public keys given (`ed25519:...`, or files holding them) |
| `pkg repo remove NAME`, `pkg repo enable NAME`, `pkg repo disable NAME` | Removes a repository added, or enables or disables one |

A change is shown before it is made, and made once agreed to (`-y` agrees
at once):

```text
user@veda:~$ pkg install hello
To install:
  hello 1.0 (test)
  libgreet 1.0 (test)
295 KiB to download; 641 KiB more installed.
Go ahead? [y/N] y
Fetching hello (295 KiB)
Fetching libgreet (447 B)
Unpacking hello
Unpacking libgreet
Putting the files in place
Done: 2 installed.
user@veda:~$ hello
Hello from C on Veda! argc=1 argv[0]=hello
pid=10932 cwd=/home/user
```

Packages install their files under `/home/packages`, which every program
reads and only the packages service changes. Their commands are in
`/home/packages/bin`, on the Terminal's `PATH` after `/system/bin` (a
package cannot stand in for one of the system's commands), and their
applications in the start menu beside the system's
([Applications](#applications)).

## How it fits together

| Part | What it is |
|------|------------|
| `packages` (`implementation/libraries/packages`) | Everything decided: names and versions, the package and index formats, keys and signatures, the repositories' configuration, the database of what is installed, the resolver, plans, and the transactions that make changes and recover them; `no_std`, tested on the host, with a file system of the standard library's (`std` feature) |
| The `packages` service (`implementation/services/packages`, program `packages`) | A system service on the standard library, which `init` starts last and restarts if it ends: serves `proto::packages`, keeps its state in `/home/.private/packages`, fetches indexes and packages, alone writes `/home/packages`, and tells the launcher when what is installed changed |
| `pkg` (`implementation/applications/pkg`) | The command: asks the service, shows plans and progress; and makes keys, packages and repositories without it, in Veda or on the build machine |
| The VFS (`vfs-core`'s `access`) | Lets none but the packages service change `/home/packages`, which it makes at every start and nothing removes or moves |
| `init` (`boot.rs`) | Starts the service after the desktop shell, and again if it ends |
| The launcher in `init` (`apps.rs`; `init-core`'s `apps`) | Lists the installed packages' applications beside the system's, reading them through the VFS when the service says what is installed changed, and starts them |
| The desktop shell | Shows them in the start menu, taking the list again when the launcher says it changed |
| The Terminal | Has `/home/packages/bin` on its `PATH`, and completes its commands |
| `automation` (`automation/source/packages.rs`) | Puts the system's repository, its configuration and the tests' repositories into the image |

```text
  pkg (in the Terminal)                    the packages service
  ---------------------                    --------------------
  plan(request)  -----------------------> resolve the world, against the
                 <----------------------- indexes taken: a plan, its digest
  apply(request, digest, progress) -----> same plan? -> worker thread:
                 <- - - progress - - - -    download (checked) -> cache
                 <----------------------    stage -> journal -> move ->
                                            commit (the database, in one step)
                                                    |
                         /home/.private/packages   |   /home/packages
                         installed.json, journal,  +-> bin/, lib/, share/...
                         repositories.json,            (read by all, written
                         indexes/, cache/              by the service alone)
```

## Packages

### What a package says of itself

A package's *manifest*:

| Field | What it is |
|-------|------------|
| `name` | 1 to 64 bytes of lower-case letters, digits and `+`, `-`, `.`, beginning with a letter or digit (`libgreet`, `gtk+`, `python3.15`) |
| `version` | `[EPOCH:]UPSTREAM[-REVISION]` ([below](#versions)) |
| `architecture` | `amd64`, or `any` for a package of no programs (data, fonts, documents, scripts) |
| `summary` | One line |
| `description` | Text, which may be empty |
| `licence` | An SPDX expression (`GPL-3.0-only`, `MIT OR Apache-2.0`) |
| `homepage` | A web address, or nothing |
| `depends` | Requirements that must be met for it to be installed: `libgreet (>= 1.2, < 2)` |
| `conflicts` | Requirements that must not be met beside it |
| `provides` | Names it answers to besides its own, with a version or without: `editor (= 1.0)` |

A requirement is met by a package of its name whose version meets every
constraint (`<`, `<=`, `=`, `>=`, `>`, at most four), or by one that
provides its name: with a version that meets them, or, for a requirement
without constraints, with none (a provision without a version tells
nothing a constraint could check, as in Debian). The system image
provides `veda` at its version, so a package says which systems it needs
with `depends=veda (>= 0.2)`.

### Versions

Versions are ordered as Debian orders its packages' versions (Debian
Policy 5.6.12), an order that has carried every kind of version upstream
projects give for thirty years: the epoch first, as a number; then the
upstream part and the revision, each in turns of non-digits, compared
character by character (letters before other characters, `~` before
everything, even the end), and of digits, compared as numbers. So
`1.0~rc1 < 1.0 < 1.0-1 < 1.0a < 1.0+b1 < 1.0.1 < 1.10 < 1:0.1`.
Unlike Debian's, a version has at most one `:` and one `-`, so that where
its parts begin is never in doubt; versions that differ only in leading
zeros (`1.01`, `1.1`) are equal, and a repository may not offer both.

### The package file

A package is one file, `NAME-VERSION.ARCHITECTURE.vpkg`
(`packages::archive`), integers little-endian:

| Offset | Bytes | What |
|--------|-------|------|
| 0 | 8 | `VEDAPKG\0` |
| 8 | 4 | The format: 1 |
| 12 | 4 | How the payload is stored: 0 as it is, 1 deflated (RFC 1951) |
| 16 | 8 | The metadata's length |
| 24 | 8 | The payload's length |
| 32 | 8 | The files' sizes together |
| 40 | 24 | Zeros |
| 64 | | The metadata: JSON, the manifest and the list of files (path, size, SHA-256), sorted by path |
| | | The payload: the files' contents one after the other, in the list's order, as one stream |

It holds regular files alone, at paths under `/home/packages` that begin
with `apps`, `bin`, `etc`, `fonts`, `include`, `lib`, `libexec`,
`licences` or `share`: no links, devices, owners or permissions, which
Veda has none of. A file is a program by what it holds (an ELF
executable, a `#!` script), as everywhere on Veda. Nothing is read of a
package beyond its header and metadata until all of it has been checked:
the lengths add up to the file's own exactly, every path is plain
(relative, no `.`, `..` or empty component, no control character), no file
stands where another's directory is, and the sizes add up. Unpacking then
streams each file out a piece at a time and checks its size and SHA-256
as it ends; a stream that would make more than the sizes say is cut off
where it does. The same files and manifest make the same bytes, so
packages build reproducibly.

A package runs nothing as it installs: no scripts before or after, which
other package managers run with all the rights of whatever installs them.
What a program needs at its first start, it does itself, in the user's
home.

### Making one

A package's directory holds its manifest as `KEY=VALUE` lines, as an
application's `.app` manifest is written (`depends`, `conflicts`,
`provides` and `description` may be given more than once), and in
`files/` what it installs:

```text
hello/manifest
hello/files/bin/hello
hello/files/share/hello/README
```

```text
name=hello
version=1.0-1
architecture=amd64
summary=Says hello
licence=GPL-3.0-only
homepage=https://www.vedaos.org
depends=libgreet (>= 1.0)
depends=veda (>= 0.1)
```

`pkg build hello` makes `hello-1.0-1.amd64.vpkg` of it, in Veda or on the
build machine (`cargo run -p pkg -- build hello`).

### Applications

A package puts an application in the start menu with a manifest in
`apps/`, as the system image's applications have, beside its program:

```text
abacus/files/apps/abacus.app
abacus/files/bin/abacus
```

```text
name=Abacus
icon=calculator
category=Accessories
description=Sums
```

The application's id is the manifest's name without `.app` (`abacus`):
the name its program runs under, which services know it by. Its program
is `exe`, by default `/home/packages/bin/<id>`, and `icon` names one of
the system's glyphs. The launcher in `init` lists the installed packages'
applications beside the system's (`launcher::apps`), and starts one as it
starts the system's (`launcher::launch_app`): under its id, at the same
priority and of the same memory class, from its program read whole (an
upgrade meanwhile does not change what runs). It leaves out, saying so in
the log, a manifest:

| Left out | Why |
|----------|-----|
| Whose id is not 1 to 64 bytes of lower-case letters, digits and `+`, `-`, `.`, beginning with a letter or digit (a package's name's rule) | An id names a process and a window, and is shown |
| Whose id is the system's: a system application's id, or the name of a program of the system image (`terminal`, `shell`, `pkg`) | Services decide what a program may do by that name: a package's `terminal.app` would have the Terminal's rights |
| Whose program is not a plain path (no `.`, `..` or empty part) in `/home/packages` or `/system` | Nothing runs as an application but what is installed or the system's: another program could write anywhere else |
| Beyond the 64th, or larger than 4 KiB | The list of every application fits one message |

`pinned` and `taskbar` count for nothing: a package's application is in
the start menu's list and its search, never in the pinned grid or on the
taskbar, which hold what the system image puts there. Its name, icon and
category are cut to 64 bytes, its description to 256, all of them rid of
control characters.

The launcher reads the manifests through a connection of its own to the
VFS, which reaches nothing but `/home/packages`, to read, and does not
wait for its answers: `init`, the launcher's process, is every process's
registry, which a busy VFS must not hold up. It reads them once the VFS is
there, and again whenever the packages service has made a change, or
started (which may have finished or rolled one back), and the shell takes
the list again when it changed.

## Repositories

A repository is a directory of package files and its index,
`index.vidx`, on this system (`file:///home/user/repo`) or on a web
server (`https://packages.example.org/veda/amd64`; `http://` too, the
packages being checked against the signed index, though others on the
network then see which ones are fetched). An address has no user name,
password, query or fragment, and no repository is in a service's private
directory.

### The index

An index (`packages::index`), integers little-endian:

| Offset | Bytes | What |
|--------|-------|------|
| 0 | 8 | `VEDAIDX\0` |
| 8 | 4 | The format: 1 |
| 12 | 4 | How the contents are stored: 0 as they are, 1 deflated |
| 16 | 4 | The signatures: 1 to 8 |
| 20 | 4 | Zero |
| 24 | 8 | The contents' length |
| 32 | 96 each | Each signature: the key's id (the SHA-256 of the public key), the Ed25519 signature |
| | | The contents: JSON, the repository's name, a serial number, when it was made and when it expires, and each package offered: its manifest, file, size, SHA-256 and the size of its files together |

Each signature is of `Veda package repository index\0`, the format, the
storage, the contents' length and the contents as stored: no signature
made for anything else checks as an index's, and nothing of the file but
the signatures themselves goes unsigned. A system takes an index only
when:

1. one of its signatures checks against a key the repository is trusted
   with (Ed25519, checked strictly: no signature another could be forged
   from, no key of small order), before anything else of it is read or
   decompressed;
2. it names the repository it was fetched for, so that the index of one
   repository, signed by a key it shares with another, cannot pass for the
   other's;
3. it has not expired; and one fetched over the network says when it
   does;
4. its serial is not lower than that of the index taken last from the
   repository, and an index of the same serial is the same index.

Every package file is then bound to it by its size and SHA-256: a file
that is larger is not read past its size, and one that differs is not
installed.

### Making one

```text
pkg key repo.key                                   # repo.key (secret), repo.key.pub
pkg build hello -o repo/hello-1.0-1.amd64.vpkg
pkg index repo --name mine --key repo.key          # repo/index.vidx, serial 1, expires in 30 days
pkg repo add mine file:///home/user/repo repo.key.pub
pkg update
```

`pkg index` signs an index of every package file in the directory; its
serial is the one before's plus one (`--serial` sets it) and it expires
in 30 days (`--expires DAYS`, or `never` for a repository on this system
alone). A repository on the network is signed again before it expires,
whether its packages changed or not. The secret key stays with whoever
publishes the repository; a repository moves to a new key by signing its
indexes with both while systems are told of the new one.

### The repositories a system uses

In two layers (`packages::config`):

* **The system image's**, one file each in
  `/system/etc/packages/repositories/NAME.repo` (`title=`, `url=`,
  `key=`, which may be given more than once, and `enabled=false` for one
  the user is to enable), read at every start. Nothing of them is kept
  elsewhere: the image a system starts from, newer or older, is the one
  whose repositories, addresses and keys it uses.
* **The user's**, which the service keeps (`repositories.json` in its
  private directory): the repositories the user added, and which of the
  image's they disabled or enabled. A system repository is disabled, never
  removed.

The system's come first, by name, then the user's, in the order they were
added. A user's repository whose name a newer image has given one of its
own is left out, and the log says so: the image's name is the one packages
installed from it were recorded by.

## The system image's repository

Every system image has (`automation/source/packages.rs`):

| In the image | What |
|--------------|------|
| `/system/packages/index.vidx` | The repository `system`: valid, signed, and offering no packages yet. What a release puts there installs from the image itself, without a network |
| `/system/etc/packages/repositories/system.repo` | Its configuration: `url=file:///system/packages`, and the public key its index is checked against |
| `/system/etc/packages/provides` | What the system provides: `veda (= VERSION)` |
| `/system/tests/packages/` | The tests' repositories, `one` and `two` (newer versions), both `test`, with the C test programs alone (`cargo automation toolchain`); `apps`, whose `abacus` installs an application (the Calculator's program) and `impostor` applications the launcher leaves out; and their public key, `test.pub` |

**The key** that signs the system's repository is a release's, whose
secret key file `VEDA_REPOSITORY_KEY` names when the release is built
(it stays off every other machine), or else the build machine's own, made
at its first build in the target directory (`target/packages/repository.key`)
and kept there. `cargo automation doctor` says which. Ed25519 signs the
same index the same every time, and the image's indexes say they were made
on 2026-01-01, as the image's other dates do: the same key makes the same
image. The tests' repositories are signed by a key whose secret is no
secret (it is made from a fixed seed): no system trusts it unless a test
adds the repository with its public key.

A repository in the system image is part of what the system trusts as it
trusts its own programs (whoever can change the image can change those):
its index need not expire, and it is read from the image at every start
and checked against the image's key, never against what a system took
before. So each start uses the repository of the image it started from:

| Where Veda starts | The repositories | What is installed |
|-------------------|------------------|-------------------|
| The live system, from a USB stick (`cargo automation iso`) | The image's, from the image in memory, found and checked at start; those added during the session, until it ends | In `/home/packages`, in memory: gone at the next start, as everything of a live session is |
| An installed system, its home on a disk (`veda-home`) | The image's, read anew; those the user added, kept with their last indexes on the home disk | Kept on the home disk, with the database, from one start to the next |
| An installed system started from a newer or older image | The new image's own (its addresses and key); those the user added, as they were | As it was. A package that needs a newer system than the image (`depends=veda (>= 2)`) stays, but what it needs is not there: a change says so (`hello 1.0 needs veda (>= 2), but the system has veda (= 0.1.0)`) until the package is removed or the newer image is back; no version that needs more than the image has is installed |
| Any, after a change was cut short | As they were | Rolled back or finished at the service's start, before anything else ([below](#making-a-change-and-recovering)) |

## Working out a change

What is installed is worked out anew from the *world*, the packages the
user asked for by name (with the constraints they asked with:
`pkg install 'hello (< 2)'` keeps hello below 2), as Alpine's `apk` does:
`install` adds to the world, `remove` takes from it, and a package
installed only for others goes once nothing needs it. The resolver
(`packages::resolve`) chooses one version of each package so that the
world, and every dependency of every package chosen, is met (by the
system, a package of its name, or one that provides it), and none chosen
conflicts with another. It searches depth first, trying candidates in
order of preference:

* an installed package keeps its version, unless it is upgraded or no
  longer meets what is asked of it; otherwise the newest version first;
* an installed package moves only to versions the repository it came from
  offers, so that a repository added later cannot take a package of
  another over by offering a higher version; one whose repository is gone
  keeps its version;
* of one version offered by two repositories, the first's;
* a package of the name asked for before one providing it; among
  providers, installed ones first, then by name.

The same request against the same indexes resolves the same way every
time. The search is bounded (200 000 choices): a problem that needs more
fails with a message rather than keeps the service busy. When nothing
satisfies a request, the message says why the most preferred attempt
failed, as people say it: `hello 1.0 needs libgreet (>= 2), but there is
libgreet 1.4 alone`; `clash is asked for, but clash 1.0 conflicts with
veda (= 0.1.0), which the system has`.

A plan says what a request installs, upgrades, downgrades and removes,
what is downloaded and how the installed size changes. It has a digest,
the SHA-256 of the database's generation, of every step with the SHA-256
of the package it installs, and of the world after: `pkg` hands it back
when the user agrees, and the service makes the change only if the plan it
works out then has the same digest, so that what is done is what was
shown, even if a repository was updated in between.

## Making a change, and recovering

The installed files are in `/home/packages`; the service keeps its
database (`installed.json`), a journal and the change under way in its
private directory, on the same file system, where a file moves in one
step (the VFS's `replace`). A change (`packages::transaction`):

1. **Downloads** what the cache lacks, each package checked against its
   size and SHA-256 as the index gives them, into `cache/`.
2. **Stages** every package it installs, into `transaction/new`, each
   file checked as it ends; first it checks that each package is what the
   index says (its manifest too), and that none of its files would take a
   place that is not its own or its earlier version's (another package's,
   or a file of no package's). Nothing installed has changed yet: what
   fails here (a damaged package, no room) leaves the system as it was.
3. **Journals** what it will do: the files it moves out of
   `/home/packages` and in, the directories it removes and makes, and the
   database's generation before and after; written whole under another
   name, then put in place.
4. **Moves** the files of the packages that go into `transaction/old`,
   makes and removes directories, and moves the new files into place.
5. **Commits**: puts the new database in place, in one step, and has the
   VFS save `/home` to the home disk. The change is made, whole, at that
   moment.
6. **Finishes**: removes `transaction`, the directories emptied and the
   journal, and the downloaded files it installed.

Should it be cut short anywhere (the power lost, the service ended), the
service recovers at its next start, before it answers anyone:

| Found | Done |
|-------|------|
| No journal, nothing staged | Nothing |
| No journal, something staged | What was staged goes (nothing installed had changed) |
| A journal, the database at its generation before | Rolled back: the files moved in go, the directories made go and those removed come back, the files moved out go back where they were |
| A journal, the database at its generation after | Finished |
| A journal that does not read, or fits neither generation | Nothing is changed until it is looked into; what is installed still lists |

Each step of recovery looks before it acts, so a recovery cut short is
recovered from too. The VFS saves `/home` whole, as it was after some
change of its, so what a computer finds after losing its power is the
state after one step or another: the tests cut a change, and then its
recovery, at every one of their steps, and find the system as it was
before the change or as it is after, never between.

## Security

| Threat | Answer |
|--------|--------|
| A repository's server, a mirror or the network sends another index, or other packages | Indexes are taken only signed by a key the repository is trusted with; packages only as the index gives their size and SHA-256 |
| An old index is replayed, to keep packages with known holes | Serials never go down; indexes fetched over the network expire, and are refused once they have |
| One repository's index stands in for another's (they share a key) | An index names its repository, which must be the one it was fetched for |
| A response that never ends, or a package larger than said | Every read is bounded: an index by a limit, a package by the size its index gives; compressed data is not read before its signature checks, nor unpacked past its sizes |
| A package writes outside its place (`../`, links, devices, another package's files) | Paths are plain and under `/home/packages`'s directories; a package holds files alone; a file of another package's, or of none, is never replaced |
| A package runs code as it installs | Packages run nothing as they install: the service only writes files |
| A program changes what another runs, or what the user installed | Only the packages service changes `/home/packages` (the VFS's rule, by the identity the registry gives each connection) |
| A program installs or removes software unasked | Only the Terminal (whose programs speak for it), Settings and the programs the kernel command line asks for may change packages or repositories |
| A package's application passes for one of the system's, whose rights services give by name | The launcher lists none whose id is a system application's or a system program's name |
| A package's application runs a program that another program wrote | Its program is in `/home/packages` or `/system`, which only the packages service and the image change; the launcher reads it through a connection that reaches nothing else |
| A package crowds the start menu or the taskbar, or makes the list of applications too large to send | Its applications are in the start menu's list alone, at most 64, each manifest's values bounded |
| A repository added later takes a package over with a higher version | An installed package is upgraded only from the repository it came from |
| What the user agreed to is not what is done | Changes are made by the digest of the plan shown |
| A change cut short leaves a broken system | Changes are journalled, and rolled back or finished at the next start |
| The resolver kept busy by an index made to be hard | Resolution is bounded |
| A key is lost or stolen | A repository may be trusted with several keys, and moves to new ones by signing with both; the user removes a repository they no longer trust |

What the service trusts as it trusts its own programs is the system image,
including the repositories in it: whoever can change the image can change
the system anyway.

## Limits

| What | At most |
|------|---------|
| A package's name, and a version | 64 bytes |
| A package file | 2 GiB; its files 4 GiB together, 65 536 of them, each path 1024 bytes |
| A package's dependencies, conflicts and provisions | 256 each |
| An index | 64 MiB as stored, 256 MiB of contents, 200 000 packages, 8 signatures |
| Repositories | 32, each trusted with 8 keys at most, its address 512 bytes |
| A resolution | 200 000 choices |
| A plan, a check of the files | 200 steps or problems listed (the rest counted) |
| Connections to the service | 32, 8 of one process |
| Applications of packages in the start menu | 64; a manifest 4 KiB, its program's path 256 bytes, its name, icon and category 64, its description 256 |

`/home` is kept in memory, and on the home disk where there is one: what
packages install takes memory, and room on the disk.

## Decisions

| Decision | Weighed, and why |
|----------|------------------|
| A system service owns the installed software, and the VFS lets no one else change it | A command that writes the files itself (dpkg, pacman, apk run by root): any application could then change what another runs, as nothing else in Veda stops it; macOS's `installd`, Windows' TrustedInstaller and Android's package manager are services for the same reason |
| One tree of installed files, `/home/packages`, under `/home` | A directory per package with links into `bin` (Homebrew, Nix): Veda has no links. A file system of its own: only `/home` is kept on the home disk |
| Packages carry files alone, and run no scripts | Debian's maintainer scripts and RPM's scriptlets run with every right of the installer, and make changes impossible to roll back exactly |
| A package format of Veda's own (`.vpkg`) | tar (Arch, Alpine, Debian's data): carries links, hard links, devices, owners and long-name extensions, each a way out of where an archive is unpacked, and the `tar` crate needs `std`; Veda's holds files, their SHA-256s and a manifest, and is checked whole before anything is unpacked |
| Deflate (`miniz_oxide`) | Zstandard: better and quicker, but no mature encoder without `std`; the format names its compression, so another can come |
| Signed indexes, packages bound by their SHA-256 | Signed packages (RPM, Arch): the index binds them already, and one signature to check per update is cheaper; TUF's roles: a repository of Veda's has one publisher, and the index carries TUF's protections for one role (rollback, freeze, mix-and-match, endless data) |
| Ed25519 | RSA, ECDSA, OpenPGP: larger, slower, with parameters to choose wrong; Ed25519's signatures are deterministic, so images build reproducibly. `ed25519-dalek`, already Veda's for TLS |
| JSON (`serde_json`) for metadata, signed as bytes | A canonical form to sign (TUF's): the signature is of the bytes as they are, which are read only once it checks |
| Debian's order of versions | Semantic Versioning: upstream projects' versions are not all semantic (`9.5p1`, `2024.1`); Debian's order has carried them all for decades |
| The world (asked-for packages) as the state, worked out anew each time | Marking packages installed by hand or not (apt): the world makes the result of a change depend on what was asked, not on its history, and removes what nothing needs |
| A resolver of Veda's, depth first, bounded | pubgrub 0.3: needs `std`, and has neither conflicts nor provisions, which packages declare, nor a bound on its work; a SAT solver: large, and its failures hard to explain |
| Installed packages upgrade only from their own repository | apt's pinning by priority: a repository added for one package can otherwise take over any other by offering a higher version |
| Staging, a journal and a commit by the database's replacement | Undoing from a log of operations (dpkg's half-installed states): a change staged whole first fails before anything installed is touched, and the journal makes the rest exact both ways |
| The system image's repository read from the image at every start | Copying its configuration into the user's: a newer image's address or key would not reach a system that had started from an older one |
| A repository in the system image, read in place | On the ISO's own file system: Veda reads nothing of the stick but the files its loader loads |
| The launcher reads the packages' applications through the VFS itself, sending its requests and taking the answers as they come | Waiting for each answer: `init` is every process's registry, so a VFS busy saving `/home` would hold up every connection in the system. The packages service handing `init` the list: the launcher would show what another process says is installed, not what is there |
| A package's application takes no name of the system's, and is pinned nowhere | Letting a package replace a system application (a Terminal of its own): it would have the system application's rights, which services give by name. Pinning itself: the pinned grid and the taskbar would fill with what packages chose, and a script's clicks would miss |

## Testing

| What | Where |
|------|-------|
| Versions, requirements, manifests, the package and index formats (damaged, false and endless input), keys, the configuration, the database, the resolver, plans | Unit tests in `implementation/libraries/packages` (`cargo test -p packages --features std`) |
| Changes: install, upgrade, removal, conflicts, no room, checks of the files; every change cut short at every step, and every recovery at every step of its own | `packages`' `tests.rs`, on a file system in memory that loses its power on cue, and on the build machine's |
| The image's repositories: valid, the same from build to build | `automation`'s tests (`packages.rs`) |
| Applications' manifests: what they say, the ids and programs left out, the values cut short, the list of every application fitting a message | `init-core`'s unit tests (`cargo test -p init-core`) |
| The protocol's messages, and that the largest answers fit one | `proto`'s tests |
| Only the packages service changes `/home/packages` | `vfs-core`'s tests |
| In Veda: the system's repository at start; a repository added, a package installed with its dependency and run, the VFS refusing others' writes there, a conflict and a newer system refused, the files checked, the packages removed | `systest`'s `packages` (`verification/systest/source/packages.rs`), which `cargo automation test` runs, in the continuous integration too |
| In Veda: a package's application listed once installed, started from `/home/packages` as an application of its own, gone with its package; another package's applications that take the Terminal's id or run programs from elsewhere left out | `systest`'s `packages' applications`, from the image's repository `apps` |
| `pkg` in the Terminal, through a restart and an upgrade | `verification/ui/packages.vts` |
| A package's application installed with `pkg`, found and opened from the start menu, gone once removed | `verification/ui/packages-apps.vts` |
| The live system from a USB stick: the image's repository found and used, a package installed into memory | `verification/ui/packages-live.vts` |
| Repositories on a web server (the host's, `automation/source/hostweb.rs`): an index that never expires refused, one that does taken, a package downloaded, installed and run | `verification/ui/packages-web.vts` |

## What comes next

* **Applications' own icons, and the files they open.** A package's
  application shows one of the system's glyphs (`icon=`), and opens no
  file by its type: `launcher::launch`, which the Files application opens
  files with, starts programs of the system image alone.
* **Downloads in pieces.** `web` reads a response whole: a package is
  held in memory once as it downloads.
* **Settings and the agent.** A page of Settings for software, and the
  agent's actions, asking the user's consent as it does.
* **Delta updates and mirrors.**
