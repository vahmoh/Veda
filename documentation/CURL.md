<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# curl on Veda

Veda's system image has curl 8.23: the `curl` command, which transfers
data by URL, and libcurl, the library C programs do it with. Its TLS is
OpenSSL's, trusting the certificate authorities the system trusts; it
speaks HTTP/1.1 and HTTP/2, takes every compression of the web's, keeps
cookies to the domains the Public Suffix List allows, and knows
international domain names. This document covers using curl and libcurl,
how they work on Veda, what keeps them safe, their limits, how they are
built, the port and its tests.

**Contents:** [Using curl](#using-curl) · [libcurl in C programs](#libcurl-in-c-programs) · [How it works](#how-it-works) · [Security](#security) · [Limits](#limits) · [Building it](#building-it) · [The port](#the-port) · [Tests](#tests)

## Using curl

In the Terminal, `curl` is the program `/system/bin/curl`, as on any other
system:

```text
user@veda:~$ curl https://example.com/
user@veda:~$ curl -O https://example.com/archive.tar.gz
user@veda:~$ curl -L --compressed -o page.html http://example.com/
user@veda:~$ curl -fsS https://example.com/missing; echo $?
curl: (22) The requested URL returned error: 404
22
```

`curl --help all` lists its options, and `curl --manual` is its manual,
built in (Veda has no `man`). `curl --version` says what it has:

```text
curl 8.23.0 (x86_64-pc-veda) libcurl/8.23.0 OpenSSL/3.5.9 zlib/1.3.2 brotli/1.2.0 zstd/1.5.7 libidn2/2.3.8 libpsl/0.23.3 nghttp2/1.70.0
Release-Date: 2026-10-14
Protocols: dict file ftp ftps gopher gophers http https imap imaps ipfs ipns mqtt mqtts pop3 pop3s rtsp smtp smtps telnet tftp ws wss
Features: alt-svc AsynchDNS brotli HSTS HTTP2 HTTPS-proxy IDN IPv6 Largefile libz PSL SSL threadsafe UnixSockets zstd
```

| Area | What it has |
|------|-------------|
| TLS | OpenSSL 3.5, its long-term support release: TLS 1.2 and 1.3, the server's certificate checked against `/system/etc/ssl/cert.pem` (Mozilla's list), and its name; client certificates (`--cert`), public keys pinned (`--pinnedpubkey`), OCSP stapling (`--cert-status`), HTTPS proxies |
| HTTP | HTTP/1.1 and HTTP/2 (nghttp2), chosen by ALPN over TLS, or by prior knowledge (`--http2-prior-knowledge`); streams of one connection at once (`-Z`), proxies and their tunnels, Basic, Digest and Bearer authentication and AWS's signatures, WebSockets (`ws://`, `wss://`) |
| Compression | gzip and deflate (zlib), Brotli and Zstandard, as `--compressed` asks for them |
| Cookies | Kept in files (`-c`, `-b`), never for a public suffix (`co.uk`), as libpsl says from the Public Suffix List |
| Names | Looked up by the system's resolver, in a thread of curl's, so that a transfer times out while one waits (`AsynchDNS`); international names (`bücher.example`) as DNS knows them (IDNA2008, by libidn2); IPv4 and IPv6 |
| What it remembers | HSTS (`--hsts FILE`): hosts that asked to be reached over HTTPS alone; Alt-Svc (`--alt-svc FILE`) |
| Elsewhere than TCP | Unix domain sockets (`--unix-socket`), the system's files (`file://`) |

**In the Terminal**, curl shows its progress meter (`-#` a bar), sized to
the window, and asks for a password it is not given (`-u NAME`) without
showing it. Ctrl+C ends it (`$?` is 130), and `$?` is otherwise its exit
status: 6 when a name has no address, 7 when nothing answers, 22 for an
HTTP error with `-f`, 28 for a time-out, 35 when TLS fails to begin, 60
for a certificate not trusted. curl reads `~/.curlrc` first (`-q` before
the other options leaves it out) and `~/.netrc` when asked (`-n`).

**The agent** runs a command line that runs curl, as any program's, only
with the user's consent each time: curl writes files (`-o`, `-O`, `-c`),
sends them (`-T`, `-d @FILE`) and does what `~/.curlrc` says, so it is no
command that only looks ([`terminal_core::classify`](../implementation/applications/terminal/core/source/classify.rs)).
The Terminal had a `curl` of its own, a GET in Rust; the program has its
name now.

## libcurl in C programs

libcurl is a shared library, `/system/lib/libcurl.so.4`, with its
interface in `/system/include/curl`. A program that uses it is linked
dynamically (`-pie`, its code `-fPIE`), as programs that use shared
libraries are on Veda ([Linking and loading](LINKING.md)):

```c
#include <curl/curl.h>
#include <stdio.h>

int main(void)
{
	CURL *curl = curl_easy_init();
	if (!curl)
		return 1;
	curl_easy_setopt(curl, CURLOPT_URL, "https://example.com/");
	curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
	CURLcode rc = curl_easy_perform(curl);
	if (rc != CURLE_OK)
		fprintf(stderr, "fetch: %s\n", curl_easy_strerror(rc));
	curl_easy_cleanup(curl);
	return rc != CURLE_OK;
}
```

```text
user@veda:~$ gcc -O2 -fPIE -pie -o fetch fetch.c -lcurl
user@veda:~$ ./fetch
```

Linked statically, as GCC links by default, it fails (`cannot find
-lcurl`): the image has libcurl shared alone. On the development machine,
the cross compiler links with the build's copy:

```bash
target/toolchain/cross/bin/x86_64-veda-gcc -O2 -fPIE -pie -I target/toolchain/curl/curl/system/include \
  -o fetch fetch.c -L target/toolchain/curl/curl/system/lib -lcurl
```

- **As on Linux**: libcurl's interface is the same, its options and
  their defaults upstream's, and `curl_global_init` sets it up, from any
  thread. Its certificate authorities are `/system/etc/ssl/cert.pem`
  (`CURLINFO_CAINFO`), and `CURLOPT_CAINFO` or `CURLOPT_CAINFO_BLOB` give
  others.
- **International names** are taken in the locale's encoding, as on
  Linux: a program sets the locale (`setlocale(LC_ALL, "")`, which is
  `C.UTF-8` on Veda) before it asks for `https://bücher.example/`; the
  curl program does.
- **Its libraries are its own.** libcurl holds OpenSSL, nghttp2 and the
  rest, and exports its interface alone: a program that uses OpenSSL
  itself links its own, which libcurl's cannot be mistaken for. So the
  OpenSSL objects libcurl hands out (`CURLOPT_SSL_CTX_FUNCTION`'s, and
  `CURLINFO_TLS_SSL_PTR`'s) are its own copy's, for no other copy's
  functions.

## How it works

**The program** is linked dynamically, with libcurl and the C library's
shared object, both from `/system/lib`, found without a search path of
its own; **libcurl** is linked with the C library's shared object alone.
Both are linked as Veda's GCC links what is dynamic: no relocations of
code, what is relocated read-only before anything runs (full RELRO, every
symbol bound at start), relative relocations packed. Their pages are the
system image's, shared by every process that runs curl or loads libcurl,
and they stand on the C library as the image has it: a new POSIX layer
needs no new build of them.

| curl uses | On Veda it is |
|-----------|---------------|
| Name lookups | `getaddrinfo`, in a thread of libcurl's, which asks the network service's resolver (its cache, `localhost`, `/system/etc/hosts`) |
| Connections | The network service's TCP sockets, connected without waiting (`EINPROGRESS`, then `poll` and `SO_ERROR`); Unix domain sockets of the VFS |
| Waiting | `poll`; the multi interface's wake-ups (`curl_multi_wakeup`) an `eventfd` |
| Files | The VFS: a cookie jar, HSTS or Alt-Svc cache is written whole under another name, then put in place (`rename`); `-R` sets a file's time |
| The terminal | Its size (`TIOCGWINSZ`), for the progress meter; `termios`, to read a password without showing it |
| Signals | `SIGPIPE` ignored (a connection's end is an error, not a signal); `SIGINT` ends it |
| Random numbers | OpenSSL's, seeded by `getrandom` |

## Security

- **Verified by default.** A server's certificate must chain to an
  authority of Mozilla's list (`/system/etc/ssl/cert.pem`, the same the
  web engine and Python trust) and name the server; TLS is 1.2 or newer.
  Only `-k` (`CURLOPT_SSL_VERIFYPEER` 0) turns that off, for the
  transfers it is given to.
- **No code loaded.** OpenSSL is built without engines, modules or
  loading of any kind; libcurl loads nothing and exports its interface
  alone (the build checks).
- **Cookies stay with their sites.** No cookie is kept for a public
  suffix, nor sent where its domain does not match; HSTS, when asked for,
  keeps hosts on HTTPS.
- **What the agent may do.** curl is a program: the agent runs it only
  with the user's consent, every time.
- **The firewall** applies to curl as to the Terminal, whose programs
  speak for it ([Networking](NETWORKING.md#the-firewall)).
- **Kept current.** curl is a port (`implementation/ports/curl`): a
  security release is a new pin and a new build
  ([Updating a port](../implementation/ports/README.md#updating-a-port)).
  Its patch changes curl's build alone, not its code.

## Limits

| Area | On Veda |
|------|---------|
| HTTP/3 | Not built: it needs QUIC's libraries (ngtcp2, nghttp3), and OpenSSL is built without its QUIC |
| SCP and SFTP, LDAP, Kerberos and SPNEGO | Not built: they need libraries Veda has not (libssh2 or libssh, OpenLDAP, a GSS-API) |
| NTLM and SMB | Left out, as curl 8.23 leaves them by default |
| Interfaces by name (`--interface eth0`) | There is no list of the interfaces (`getifaddrs` fails: no netlink): an address works (`--interface 10.0.2.15`) |
| `--xattr` | Veda's files keep no extended attributes: curl says it could not set them, and writes the file |
| `wcurl`, `curl-config` | Shell scripts, and there is no `/bin/sh` yet: not in the image (`wcurl URL` is `curl -fLO URL`) |
| A static libcurl | None: programs link `libcurl.so.4`, dynamically |
| A file | 2 GiB at most, kept in memory, as every file on Veda ([C on Veda](C.md#limits)) |

## Building it

`cargo automation toolchain` builds curl last, once the C toolchain is
built ([C on Veda](C.md#the-toolchain)), in a few minutes, with
`implementation/ports/curl/build.sh`, in `target/toolchain/curl`; every
image built afterwards has it. `cargo automation doctor` says whether it
is built. The script builds in stages, each done again only when what it
is made from changed:

1. **pkgconf**, for this machine, which finds the libraries below.
2. **The libraries libcurl holds**, for Veda, static and
   position-independent (`-fPIC`, for a shared library): zlib, Zstandard,
   Brotli, nghttp2, libpsl (the Public Suffix List built in), libidn2
   (with the parts of libunistring it carries) and OpenSSL, without what
   loads code or what curl does not use (its QUIC, its legacy ciphers).
   The same ports as the web engine's and Python's, built apart.
3. **curl**, by its own build (Autoconf and libtool): libcurl shared, the
   program linked with it. The build fails if configure did not find a
   library curl must have, if libtool cannot make shared libraries for
   Veda, or if the manual is left out.
4. **What the image has**: the program and the library, stripped (their
   unstripped files stay in the build, for `addr2line`), checked: each
   needs what `/system/lib` has and no search path, and libcurl exports
   its interface alone; libcurl by its soname, `libcurl.so.4`, and a
   linker script of its unversioned name, which `-lcurl` links (Veda has
   no symbolic links); its headers; and `libcurl.pc`, without what a
   static link would need.
5. **The licence texts** of curl and of the libraries in libcurl, which
   the image has in `/system/licences/curl`.

It needs Perl beside the toolchain's build tools: it configures OpenSSL
and makes curl's manual.

## The port

`implementation/ports/curl` is curl's port, as
[the ports' README](../implementation/ports/README.md#curl) describes
ports: `port.toml` pins the release (`curl-8.23.0.tar.xz`, by its
SHA-256, its signature checked against Daniel Stenberg's key when it was
pinned), `veda.patch` changes its build, and `build.sh` builds it. The
patch teaches it Veda, in its build alone:

- **`config.sub`** knows `veda`, as the toolchain's ports' do.
- **libtool makes Veda's shared libraries** (`configure`, and
  `m4/libtool.m4`, which it is generated from), as GCC's port teaches its
  own: a library holds the static libraries it is linked with (their
  dependencies are kept, `pass_all`), and is named and found as on
  GNU/Linux (`libcurl.so.4`, `LD_LIBRARY_PATH`, `/system/lib` and
  `/home/packages/lib` searched without a `RUNPATH`). Without it libtool
  knows no dynamic linker on Veda, and makes no shared library.

`libidn2`'s port has `config.sub` know `veda` too. curl's code is as
released: Veda's C library is musl's, which curl knows.

## Tests

- **`verification/curl/checks.c`** checks curl and libcurl inside Veda,
  against servers of its own, threads on the loopback interface (no
  Internet): HTTP/1.1 over IPv4, IPv6 and a Unix domain socket; redirects,
  authentication, uploads (in chunks too, and forms), responses in chunks,
  each compression, 16 MiB downloaded and resumed, time-outs, ports that
  refuse and names that have no address; a proxy and its tunnel; TLS 1.3
  and 1.2, HTTP/2 by ALPN and by prior knowledge, streams at once on one
  connection, flow control each way; certificates for other names,
  expired or of an authority not trusted, pinned keys, the chain, client
  certificates; HSTS, cookies and the Public Suffix List, a share between
  handles; the multi interface, its wake-ups from another thread, eight
  threads at once; WebSockets, plain and over TLS; `file://`; URLs and
  international names. Then the curl program, in processes it starts:
  each of those as its options ask, its exit statuses, files it writes,
  resumes and dates, cookies and HSTS kept between runs, a proxy from the
  environment, `SIGINT`, and its manual. The versions it expects are the
  ports'. `systest` runs it (`/system/tests/curl/checks`), with the host's
  web server when there is one, and continuous integration too.
- **`verification/ui/curl.vts`** runs curl in the Terminal: the host's web
  server to the terminal, through a pipe and into a file, an HTTP error's
  status, a transfer's progress bar and Ctrl+C, and the manual. It says
  `requires curl`.
- **`verification/real/curl.vts`** reaches the Internet, by hand: HTTPS to
  public sites over HTTP/2, their certificates checked against the
  system's authorities, and one that has expired refused.
- **Unit tests**: the Terminal's classifier (`cargo test -p
  terminal-core`), which takes curl for a program.
