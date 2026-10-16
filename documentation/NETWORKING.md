<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Networking and Wi-Fi

Veda reaches the Internet over Ethernet and Wi-Fi. As everywhere else
in the system, the work is split into isolated user-space processes that
talk over kernel channels; the kernel knows nothing about networks.

* **Drivers** move frames and nothing else, and they are Linux's, in
  [the driver VM](DRIVERVM.md): `net` offers each of Linux's network
  cards to `netd` as an Ethernet device, and `wifi` each of its Wi-Fi
  radios to `wlan` as a *managed* radio, which joins access points on
  `wlan`'s commands (the cards of PCs keep the MAC in their firmware) and
  moves Ethernet frames (see below). Neither sees passwords or addresses;
  a radio gets the session keys only.
* **`wlan`** is the Wi-Fi service. Everything above the radio happens here:
  scanning, authentication, association, the key handshakes, encryption,
  saved networks and the decisions of when and where to connect. Each
  radio appears to `netd` as the Ethernet-like interface `wlan0`, whose
  link goes up when a network is joined.
* **`netd`** is the network service: IPv4 and IPv6, DHCP, IPv6 address
  autoconfiguration, routing between interfaces, a caching DNS resolver,
  and the sockets applications use. The protocol engine is
  [smoltcp](https://github.com/smoltcp-rs/smoltcp), a port
  (`implementation/ports/smoltcp`): its release with documented patches
  (see Dependencies below).
* **Applications** use `vnet` (`implementation/libraries/net`): blocking,
  `std::net`-like `TcpStream`, `TcpListener`, `UdpSocket`, name resolution,
  ping, and `vnet::wifi` for Wi-Fi; `vtls`
  (`implementation/libraries/tls`) for TLS over a `TcpStream`, inside the
  application's own process; and `vweb` (`implementation/libraries/web`)
  for HTTP and WebSocket clients over either (HTTPS, secure WebSockets).
  C programs, and programs on Rust's standard library (`std::net`), use
  the POSIX layer's sockets (`implementation/libraries/posix/src/inet.rs`),
  which are `netd`'s sockets too, and look names up with its resolver
  (see [C on Veda](C.md)).

Frames travel between processes through shared-memory rings
(`vproto::netring`): a pair of single-producer/single-consumer rings per
link with wake-up events, so a frame costs one copy into the ring and one
out, and a system call only when the other side is asleep. In `netd`,
smoltcp's device is the ring itself: a frame is copied from the ring into
the interface's buffer, where smoltcp parses it, and smoltcp builds each
frame it sends in a buffer that is copied into the ring; when the ring is
full, `netd` waits for room rather than dropping frames. Neither side
trusts the other: positions are compared modulo 2^64 and clamped to the
ring, lengths are checked, and shared memory is only read and written
through volatile accesses.

## Who may do what

Both services know who opened each connection (the registry's client
identity, which a process cannot forge), and check it:

* Network devices attach (`netdev`) only from the device manager, whose
  drivers, the driver VM's programs included, speak as it, and from the
  Wi-Fi service; radios are offered (`wlanphy`) only by the device
  manager. Others' connections are closed.
* Configuring interfaces, renewing leases, setting the DNS servers and
  flushing the DNS cache are for Settings and the Terminal; so are listing
  every socket (others list their own) and the interfaces' hardware
  addresses (zeros for others). The DNS cache is not listed at all: it is
  a record of every name any program looked up.
* Changing Wi-Fi (scanning, joining, leaving, the radio switch, saved
  networks) is for the desktop shell, the agent, Settings and the
  Terminal. What tells where the machine is or
  has been, or identifies it — the access points' addresses (BSSIDs), the
  saved networks, diagnostics, the adapter's address — is for Settings and
  the Terminal; statuses go to others without those addresses.
* Limits apply per process, however many connections it opens: `net`
  connections, sockets (with the connections its listeners accept), lookups
  in progress, status watchers; and of each, a part is kept for system
  services, so that applications together cannot use it all. The Wi-Fi
  service limits connections and watchers the same way.

## Sockets in `netd`

* Flow control is credit-based both ways (see `vproto::net`), and nothing
  a client is owed is lost when its channel is full (a channel holds a
  bounded number of messages, whatever their size): received data leaves
  the stack only once its message is queued to the client, so a client
  that does not read leaves it in the stack, whose TCP window then closes;
  an accepted connection, the end of a stream or an error waits until the
  client's channel has room, which `netd` waits for.
* The POSIX layer's sockets go to other processes, which share their
  channels, and credit would go to whichever process read it. So a TCP
  connection's bytes go on a kernel socket between the program and
  `netd`, whose buffers are the flow control (what does not fit stays in
  the stack, whose window closes), and its channel brings only the
  connection and its failure; a UDP socket's datagrams go without credit,
  `netd` reading no more of them from its channel while 256 KiB of them
  wait to be sent, and sending those a program sent before it closed the
  socket (for three seconds at most, as long as Linux waits for a neighbor
  that does not answer).
* A TCP client may send only what fits the stack's send buffer, so what it
  sent is there the moment it arrives: closing the socket delivers it,
  then a FIN. The connection then finishes on its own, within bounds: it
  is reset if the peer sends more (nobody will read it), and dropped after
  a minute without progress, or when more than 256 such connections wait.
* The end of a stream is the peer's FIN, whatever is still buffered; what
  a connection received before it closed stays readable after its smoltcp
  socket is gone (TIME-WAIT included).
* An established connection is never given up for being quiet: the
  timeout (two minutes) applies only while data or a FIN of ours waits for
  the peer's acknowledgement, or keep-alive probes are sent (at least a
  second apart). A connection whose peer stopped answering ends timed out,
  and one the peer reset ends reset, after the client finished sending
  too (not with the end of the stream).
* A listener holds at most its backlog of connections not accepted yet;
  further attempts are refused.
* Each wake-up attends to what changed: the sockets the stack reports
  ready (data, room to send, a state change, an accepted connection, a
  ping result; data after what `netd` took, or room after what it queued,
  however soon it comes) and those whose client wrote or made room. Lookups' reply
  channels are waited on like sockets, so a lookup whose client left is
  dropped at once. The status is worked out again only when something it
  depends on changed.
* What crosses the loopback interface is taken in at once: a poll that
  leaves frames on their way back makes the next poll due now, and
  sockets are told of what a poll's last round brought them, so a
  connection to the machine itself waits for no timer.
* Ping reports why a request failed: no answer in time, or what a router
  said (destination or host unreachable, time to live exceeded, with the
  router's address), so pings with a small TTL find the routers on the
  way.
* The resolver asks from small sockets of its own, on the interface the
  server is reached through (not counted against, nor listed among,
  applications' sockets), and passes over at once a server it cannot send
  to.
* The DHCP lease shown is the one the client accepted from its server's
  acknowledgement (renewals included), not just any acknowledgement on
  the network.
* IPv6 addresses do not embed the hardware address, which would follow
  the machine from network to network: their interface identifiers, the
  link-local one included, are stable privacy identifiers (RFC 7217) from
  a secret `netd` draws when it starts.

## Components

| Path | What it is |
|------|------------|
| `implementation/services/netd` | the network service (`net` and `netdev` protocols) |
| `implementation/libraries/netstack` | `vnetstack`: interfaces, DHCP, SLAAC, routes, DNS, sockets on smoltcp |
| `implementation/services/wlan` | the Wi-Fi service (`wlan` and `wlanphy` protocols) |
| `implementation/libraries/wlan` | `vwlan`: IEEE 802.11 frames, RSN, CCMP/BIP, EAPOL, handshakes, SAE, station and access point state machines, saved-network format, connection policy |
| `implementation/guest/net`, `implementation/guest/wifi` | in the driver VM: Linux's Ethernet cards as `netdev` devices; Linux's Wi-Fi radios as managed radios |
| `implementation/libraries/net` | `vnet`: the application API, including `vnet::wifi` |
| `implementation/libraries/tls` | `vtls`: TLS 1.3 and 1.2 client (rustls with a pure-Rust cryptography provider) |
| `implementation/libraries/web` | `vweb`: URLs, the HTTP/1.1 client (redirects followed) and the WebSocket client, over TCP or TLS |
| `implementation/libraries/proto/src/{net,wlan,netring}.rs` | the service protocols and frame rings |
| `implementation/libraries/entropy`, `implementation/kernel/src/random.rs` | the kernel's random number generator |
| `verification/nettest` | in-system network checks |
| `verification/ui/drivervm-net.vts`, `e1000e.vts`, `drivervm-usb.vts`, `network-cable.vts` | GUI scripts: Linux's drivers of QEMU's network cards, and a cable pulled |

## Wi-Fi

### Joining a network

1. **Scanning.** The service has the radio scan every channel (probe
   requests where transmitting is allowed, and directed ones for hidden
   networks) and records every beacon and probe response it reports.
   Networks are grouped by name and security for the network list.
2. **Authentication.** Open system authentication for WPA2 and open
   networks; Simultaneous Authentication of Equals (SAE, WPA3), whose
   commit and confirm the service computes, with both the
   hunting-and-pecking and the hash-to-element password element, and the
   radio sends.
3. **Association**, by the radio, with the service's RSN element, matching
   the access point's ciphers (CCMP-128) and management frame protection
   (required for WPA3, used whenever the access point is capable).
4. **The 4-way handshake**, the service's, derives the session keys from
   the PMK (the PBKDF2 pre-shared key for WPA2, the SAE key for WPA3) and
   gives the radio the pairwise and group keys; the group key handshake
   renews group keys.
5. The link to `netd` goes up; `netd` runs DHCP (and SLAAC) on `wlan0`, and
   the default route moves to it unless a wired connection is up (Ethernet
   has the lower route metric).

### Staying connected

* **Saved networks** (`/home/user/.config/wlan/networks`) are joined
  automatically, preferring the most recently used, then the strongest
  access point (5 GHz gets a small bonus).
* **Lost connections** (the access point stops answering, disconnects
  the station, or the signal fades) are re-established after a fresh scan,
  on any access point of the network. An access point that stopped
  answering is not tried again until it is heard again.
* **Failed attempts** are retried with growing delays (2, 4, 8, 16, 32,
  then every 60 seconds).
* **Weak connections** (below -72 dBm) look for an access point of the
  same network that is at least 10 dB stronger and move to it, keeping the
  IP configuration.
* **A password the access point does not seem to accept** is retried
  after longer pauses (1, 5 and 15 minutes, then every hour), since
  repeated attempts could get the device blocked; connecting by hand tries
  again at once. Nothing proves a password wrong (the frames that suggest
  it are not authenticated, and anyone in range could send them), so the
  network is never given up for good, and the user is told only that the
  password may be incorrect.
* **The radio** may go away and come back (the driver reports it, the
  service rejoins); if the driver stops responding to control calls for two
  seconds, the service drops the radio rather than hanging. If the Wi-Fi
  service exits, `init` restarts it and the driver offers the radio again.

### Security

* All key material comes from the kernel's ChaCha20 random number
  generator, seeded from the firmware's `EFI_RNG_PROTOCOL`, RDSEED, RDRAND
  and timing jitter.
* Every received frame is untrusted: parsers are bounds-checked and never
  panic, MICs and authentication tags are compared in constant time, and
  received key material is used only after the standard's checks pass.
  CCMP and BIP enforce replay counters; duplicate frames are dropped.
* Unprotected deauthentication and disassociation frames are ignored once
  management frame protection is in use; an unprotected one starts an SA
  Query to check whether the access point really lost the association.
* **No downgrades:** a network saved with security is never joined through
  an open access point; a network saved as WPA3, or one where WPA3 was used
  once, is never joined through an access point offering only WPA2; WEP,
  TKIP-only WPA, Enterprise and OWE networks are listed but never joined.
  A password is never used with an open access point either (an open "evil
  twin" of a protected network is neither joined nor saved as open): the
  desktop and Settings say which security the network the user picked has,
  and a request with a password takes only access points with security.
* The RSN element the access point sends in the handshake must match the
  one it advertised, and so must the RSN Extension element: in both and
  the same, or in neither, whatever the AKM.
* An SAE frame that does not verify is discarded, as 802.11 says, and the
  access point's repeated commits and anti-clogging requests count against
  the attempt's tries, so forged frames can neither end an attempt nor keep
  it going for ever.
* Passphrases are checked (8-63 printable characters or 64 hex digits),
  never logged and never returned by the API. The Terminal does not keep
  them in its history. Session keys are zeroed when a connection ends
  (with the copies the handshake keeps of what it installed), and so is
  the decrypted key data of the handshake's messages; a new pairwise key
  keeps the group keys.
* The saved-networks file holds the passphrases (needed to rejoin); the
  file system has no access control yet, so any program can read it.

### Soft MAC radios

The service can drive a radio that moves raw 802.11 frames and does
nothing else (a *soft MAC* radio, `wlanphy_ctl`), being its MLME itself:
scanning channel by channel, sending the authentication and association
frames, encrypting with CCMP. Veda's own driver of a virtual radio was
one; it went with Veda's other network drivers, and no radio of that kind
is left.

### Managed radios: Linux's Wi-Fi

The cards of PCs (Intel's, MediaTek's, Realtek's, Qualcomm's) keep their
MAC in firmware: their drivers do not move raw frames, and Linux's 802.11
stack (cfg80211 and mac80211) builds on that. Veda uses them through
[the driver VM](DRIVERVM.md), where `wifi` (`implementation/guest/wifi`)
offers each of Linux's radios to `wlan` as a *managed* radio
(`phy_caps::MANAGED`): its control channel speaks `wlanmlme_ctl` instead of
`wlanphy_ctl`, and the station of `vwlan` drives it with commands rather
than frames:

| The station | `wlanmlme_ctl` | Linux (nl80211) |
|-------------|----------------|-----------------|
| scans | `scan(ssids)`; results as beacons and probe responses on the link, then `ScanDone` | `TRIGGER_SCAN`, `GET_SCAN` |
| authenticates (open; SAE's commit and confirm, computed by the station) | `authenticate(bssid, channel, ssid, body)` | `AUTHENTICATE` with `AUTH_DATA`; the answer as a frame |
| associates (its RSN and RSNX elements, PMF) | `associate(...)` | `ASSOCIATE`, the control port over nl80211, owned by `wifi`'s socket |
| runs the 4-way and group handshakes | `send_eapol(peer, frame, encrypt)`; EAPOL from the AP as Ethernet frames | `CONTROL_PORT_FRAME` (message 4 in clear, whatever key follows it) |
| installs the session keys only | `install_key(kind, index, key, rsc, peer)` | `NEW_KEY` (CCMP-128, BIP-CMAC-128) |
| opens the port | `authorize(peer)` | `SET_STATION` (authorized) |
| checks an unprotected deauthentication (SA Query) | `send_management(frame)`; `UnprotectedDeauth` | `FRAME`; `UNPROT_DEAUTHENTICATE` |
| leaves | `deauthenticate(bssid, reason)` | `DEAUTHENTICATE` |

Linux retries, times out (`Timeout`), encrypts and decrypts, answers the
access point's SA Queries, and watches the link (`LinkLost` when it gives
up on the access point; `Signal` as it changes); the station decides, and
authenticates, as with a soft-MAC radio. Linux joins only access points it
heard lately: `wifi` listens on the access point's channel first when it
must. The Wi-Fi service is the same for both kinds of radio, and so are
the user's networks and the connection policy.

## Using the network

* **The taskbar's network icon** shows the Wi-Fi signal (its arcs light
  up in turn while it joins a network), the wired connection when Wi-Fi
  is not connected, a faint fan when Wi-Fi is on but not connected, or the
  fan crossed out when Wi-Fi is off. It opens the network flyout: Wi-Fi on
  and off, networks in range (a padlock on the secured ones), joining one
  (with its password, optionally automatically in future), hidden
  networks, disconnecting, and a link to Settings.
* **Settings → Network & Internet**: the Wi-Fi connection in detail,
  networks in range, saved networks (automatic connection, forget), every
  interface with its addresses, gateway, DNS servers, DHCP lease and
  traffic, and the Wi-Fi diagnostics log.
* **Terminal**: `wifi` (`status`, `scan`, `list`, `connect SSID
  [PASSWORD]`, `disconnect`, `saved`, `forget`, `auto`, `on`, `off`,
  `aps`, `log`), `ifconfig`, `ping`, `nslookup`, `netstat`, `route`,
  `curl`.

### For applications

```rust
use vnet::{Duration, TcpStream};

let mut s = TcpStream::connect_host("example.com", 80, Duration::from_secs(20))?;
// HTTP or HTTPS, following redirects; the body at most 1 MiB.
let page = vweb::http::get("https://example.com/", Duration::from_secs(20), 1 << 20)?;

vnet::wifi::scan()?;
for n in vnet::wifi::networks()? {
    // n.name, n.security, n.signal_dbm, n.bars, n.saved, n.connected
}
vnet::wifi::connect("Veda Home", Some("password"), true)?;
let events = vnet::wifi::Watcher::new()?; // status changes, scans, failures

let config = vtls::ClientConfig::with_alpn(&[b"http/1.1"]); // share it
let mut tls = vtls::connect("example.com", 443, Duration::from_secs(20), &config)?;
tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")?;
let n = tls.read(&mut buf)?; // Ok(0): the server closed the connection (close_notify)
```

## TLS

`vtls` (`implementation/libraries/tls`) is the TLS client of Veda
applications: HTTPS, secure WebSockets, anything else over TLS 1.3 or TLS
1.2. It runs in the application's process, on top of a `vnet::TcpStream`.

* **Protocol.** [rustls](https://github.com/rustls/rustls) 0.23, through
  its `no_std` "unbuffered" API: `TlsStream` keeps the received TLS bytes,
  the decrypted data and the records to send, lets rustls process what
  arrived, sends what rustls produces (before anything later) and reads
  from the socket only when rustls needs more. Partial records, several
  records per read, key updates, session tickets, alerts and close_notify
  are handled inside. The end of the stream is the server's close_notify:
  a connection that just ends may have been cut short by anyone on the
  path, and reading reports it (`TlsError::Truncated`) once the data
  before it is read.
* **Cryptography** is vtls' own rustls provider on pure-Rust crates (no C,
  no assembly): AES-128-GCM, AES-256-GCM and ChaCha20-Poly1305 records;
  SHA-256/384 and HMAC, with rustls' HKDF and TLS 1.2 PRF over them; X25519,
  secp256r1 and secp384r1 key exchange; ECDSA (P-256, P-384), Ed25519, RSA
  PKCS#1 v1.5 and RSA-PSS signatures. The cipher suites are the three TLS
  1.3 ones and ECDHE-ECDSA/ECDHE-RSA with AES-GCM or ChaCha20-Poly1305 for
  TLS 1.2. RSA verification is implemented in vtls on `crypto-bigint`,
  with *ring*'s rules (2048 to 8192-bit moduli, PSS salts as long as the
  hash). Ephemeral secrets come from the kernel's ChaCha20 generator and
  are zeroed after use.
* **Certificates** are verified by rustls-webpki against the Mozilla root
  program (webpki-roots, 121 roots compiled in), at the time of the system
  clock (the firmware's clock, advanced by the kernel). A clock that reads
  earlier than 2025 is treated as unset, and the handshake fails with a
  message saying so rather than with misleading expiry errors.
* **Errors** are `TlsError`s with messages for users: "the server's
  certificate is not trusted: it expired on 2025-01-01 (the system clock
  reads 2026-06-01)", "secure connection failed: the server refused the
  connection: no security parameters in common". After a protocol error
  the alert rustls produces is sent to the server; every later call
  reports the same error. Nothing panics on what the network sends.
* **Event loops** wait for `tls.transport().handle()` to become readable
  and call `try_read` until it returns `Ok(None)`: it decrypts whatever
  the socket holds without waiting, and sends what it owes (a key update's
  answer) as far as the socket takes it at once. `try_write` sends small
  messages the same way, the rest queued in order, so a server that stops
  reading cannot stall the loop.

Under QEMU's TCG emulation a full TLS 1.3 handshake with example.com
(X25519, an ECDSA P-256/P-384 chain) took 115 ms, the network round trip
included. Every connection is a full handshake (there is no session cache
yet).

`run=nettest:https` checks it from inside Veda (see Testing).

## Dependencies

The protocols are implemented with mature Rust crates where they exist,
built `no_std`:

* **smoltcp 0.14** (TCP/IP), a port (`implementation/ports/smoltcp`; see
  `implementation/ports/README.md`). Its patch draws the random numbers
  (TCP initial sequence numbers, DHCP and DNS ids, source ports) from a
  ChaCha20 stream keyed from the kernel's generator instead of the
  predictable upstream sPCG32; applies a TCP connection's timeout only
  while something waits for the peer's acknowledgement (upstream reset
  every connection quiet for longer than it); tells the lease the DHCP
  client accepted; forms IPv6 addresses with stable privacy interface
  identifiers given a secret; lets ping sockets hear the errors about
  their requests, ICMPv4 errors that quote only part of a packet
  included; and limits the rate of ARP requests and neighbor
  solicitations per address (upstream asked for no neighbor for a second
  after asking for any, so the first connection after a DNS lookup waited
  a second for the gateway's address); and takes what comes up to the
  furthest edge of the receive window it advertised (a window rounded to
  the scale's units can end short of an earlier one, and what the sender
  sent past that end waited a second to be sent again).
* **RustCrypto**: `sha1`, `sha2`, `hmac`, `pbkdf2`, `aes`, `ccm`, `cmac`,
  `aes-kw`, `p256` (SAE), `base16ct` (keys typed in hexadecimal), plus
  `subtle` (constant-time comparisons) and `zeroize`; `chacha20` and
  `blake2` for the kernel's random number generator, which every key comes
  from.
* **httparse** for the HTTP client (`vweb`): response heads and the sizes
  of chunked bodies; **percent-encoding** (the `url` crate's) for URL
  components and **base64ct** (RustCrypto's) for WebSocket keys.
* **TLS** (`vtls`):
  * **rustls 0.23.45** (`tls12`, `custom-provider`; no default features):
    the most widely used Rust TLS implementation, memory safe and
    `no_std`-capable through its unbuffered API. It brings
    **rustls-webpki 0.103** (certificate path validation) and
    **rustls-pki-types 1**. The 0.24 line is still a pre-release.
  * **webpki-roots 1.0.9**: the Mozilla root certificates, compiled in.
  * **aes-gcm 0.11**, **chacha20poly1305 0.11**, **p384 0.14** and the
    `ecdsa`/`ecdh` features of **p256 0.14** (pulling **ecdsa 0.17**),
    **crypto-bigint 0.7**: the RustCrypto generation the Wi-Fi code
    already uses (`aes` 0.9, `sha2` 0.11, `hmac` 0.13, `p256` 0.14), so no
    crate family is built twice.
  * **x25519-dalek 3** and **ed25519-dalek 3** (on curve25519-dalek 5):
    the standard X25519 and Ed25519 implementations, same generation
    (`sha2` 0.11, `rand_core` 0.10, unused here: secrets are made from
    the kernel's random bytes).
  * Not used: *ring* and aws-lc-rs (rustls' usual providers) contain C and
    assembly; the `rsa` crate's current line (0.10) is only a release
    candidate, and verification, all a TLS client needs, is short and
    involves no secrets, so vtls implements it on `crypto-bigint`. The
    lock file lists `ring` because of a weak optional feature of
    rustls-webpki; it is never built.

  LLVM cannot build the SIMD code (AES-NI, SSE2, AVX2) of the RustCrypto
  and dalek crates for the soft-float UEFI target, which only the `no_std`
  check of the libraries uses: Cargo's settings of the target
  (`.cargo/arch/amd64/config.toml`) select their portable code for it
  (Veda's user space keeps the fast code, chosen at run time from the
  processor's features).

Everything else (the 802.11 protocol, the handshakes, SAE's protocol
logic, the TLS stream and provider, the services and drivers) is in this
repository. The cryptography is checked against published test vectors:
RFC 8439 (the smoltcp port's ChaCha20 generator), RFC 3394 (AES key
wrap), RFC 4493 (AES-CMAC), the IEEE 802.11 PSK and PRF vectors, the IEEE
802.11i CCMP test MPDUs, the IEEE 802.11-2020 Annex J.10 SAE vectors, and
for TLS the RFC 8448 handshake trace (X25519, protected records, RSA-PSS
and PKCS#1 signatures), RFC 4231 (HMAC), RFC 8032 (Ed25519), the TLS 1.2
PRF vector and RSA signatures made by OpenSSL; the kernel's generator
(RustCrypto's ChaCha20 and BLAKE2s) against its known answers.

## Testing

* **Host tests** (`cargo orchestration test` runs them all): `vwlan`
  (frames, RSN, crypto vectors, handshakes, SAE, and
  station-against-access-point runs: every security type, wrong passwords,
  hidden networks, replays, deauthentication, silent access points,
  reconnection, rekeying, and frames forged or altered on the air),
  `vnetstack` (two stacks over a simulated cable: DHCP including lease
  renewal, server outages and other clients' acknowledgements, TCP
  including quiet connections, half closes, closing with data in flight
  and connections whose client left, UDP, DNS including unreachable
  servers, ICMP and its errors, IPv6, a full link), the profile and policy
  modules, smoltcp's own tests with Veda's patch (see
  `implementation/ports/README.md`), and `vtls`: the provider against the
  vectors above, handshakes with a rustls server in memory (every cipher
  suite, key exchange group and certificate type, data both ways, closing
  from either side, key updates, records split down to single bytes,
  expired, future, misnamed, untrusted and client-only certificates, broken
  servers, every bit of the server's first flight damaged in turn), and the
  certificate chains of api.deepgram.com, example.com and www.google.com
  captured on 2026-10-03, verified against the built-in roots.
  `cargo test -p vtls -- --ignored --nocapture` connects to those servers
  for real with each cipher suite and group.
* **In-system**: `nettest` (`run=nettest`): DNS, UDP, TCP, HTTP and ping
  over the default route, Ethernet or Wi-Fi; `run=nettest:https`: HTTPS
  requests to example.com (200 and the page) and api.deepgram.com (401
  without a key) with the TLS version, cipher suite and handshake time.
  These checks, and the scripts below, reach real Internet hosts through
  QEMU's NAT, so the host must be online. `run=nettest:wifi,SSID,PASSWORD`
  joins a network before its checks: the Wi-Fi service lets `nettest`
  change Wi-Fi only when the kernel command line started it (the
  registry's `ClientIdentity::requested`), never when an application did.
* **GUI and recovery** (`cargo orchestration test --ui`):
  `verification/ui/network-cable.vts` unplugs and replugs the wired card's
  cable (QMP `set_link`) and checks that the default route goes and comes
  back; `verification/ui/drivervm-restart.vts` crashes the driver VM and
  checks that the network comes back with it;
  `verification/ui/drivervm-net.vts` and `e1000e.vts` run Linux's drivers
  of QEMU's Intel 82576 and 82574L, and `drivervm-usb.vts` of its USB
  network adapter.

## Limitations and next steps

* Wi-Fi hardware is Linux's, through the driver VM: Intel's AX211 has its
  firmware so far (`implementation/ports/linux/firmware.txt`); other cards
  need their driver and firmware added. 6 GHz channels, and telling Linux
  the country (it keeps to the world's rules, listening first on channels
  that need it), are next. QEMU cannot emulate a Wi-Fi adapter, so beyond
  `vwlan`'s host tests Wi-Fi is tested on PCs.
* WPA2/WPA3-Enterprise (802.1X/EAP), Enhanced Open (OWE), fast roaming
  (802.11r), power saving, 802.11n/ac rate control.
* TLS: no session resumption (each connection is a full handshake), no
  client certificates, no revocation checking (OCSP, CRLs), no Encrypted
  Client Hello, and no post-quantum key exchange yet (X25519MLKEM768, which
  RustCrypto's `ml-kem` 0.3 could provide as a hybrid group). The trusted
  roots are compiled in: updating them needs a new build, and there is no
  system certificate store to add one to (`ClientConfigBuilder` can, per
  application). Certificate checks depend on the firmware clock being
  right.
* HTTP: one request per connection (`Connection: close`), no compressed
  bodies, cookies or caching (`vweb`'s client serves API calls and
  downloads).
* IPv6: no temporary addresses (RFC 8981) yet; the stable privacy
  addresses change when `netd` starts again (its secret is not kept on
  disk).
* Encrypting the saved passphrases at rest.
