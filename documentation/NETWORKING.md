<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Networking and Wi-Fi

How Veda reaches the network: the network service (`netd`) and its
firewall, the Wi-Fi service (`wlan`), TLS (`tls`), the API applications
use, and the tests.

**Contents:** [Components](#components) · [Who may do what](#who-may-do-what) · [Sockets in `netd`](#sockets-in-netd) · [The firewall](#the-firewall) · [Wi-Fi](#wi-fi) · [Using the network](#using-the-network) · [TLS](#tls) · [Testing](#testing) · [Limitations and next steps](#limitations-and-next-steps)

## Components

The kernel knows nothing about networks: isolated processes do the work
and talk over kernel channels. Drivers only move frames and see no
passwords or addresses (a radio gets the session keys only), and `wlan`
presents each radio to `netd` as the Ethernet-like interface `wlan0`, whose
link goes up when a network is joined. Mature `no_std` crates (smoltcp,
rustls, RustCrypto; see [Libraries](DEPENDENCIES.md)) do the protocol work
where they exist; the 802.11 protocol, the handshakes, SAE's protocol
logic, the TLS stream and provider, the services and the drivers are in
this repository.

| Path | What it is |
|------|------------|
| `implementation/guests/linux/net`, `implementation/guests/linux/wifi` | In [the driver VM](DRIVERVM.md): `net` offers Linux's network cards to `netd` (`netdev`), `wifi` Linux's Wi-Fi radios to `wlan` as *managed* radios, which join access points on `wlan`'s commands |
| `implementation/services/wlan` | `wlan`, the Wi-Fi service (`wlan` and `wlanphy` protocols): what to join and when, authentication, the key handshakes, saved networks |
| `implementation/libraries/wlan` | `wlan`: 802.11 frames, RSN, CCMP/BIP, EAPOL, handshakes, SAE, station and access point state machines, the saved-network format, the connection policy |
| `implementation/services/netd` | `netd`, the network service (`net` and `netdev` protocols), and the firewall's settings store (`store.rs`) |
| `implementation/libraries/netstack` | `netstack`: interfaces, IPv4 and IPv6, DHCP, SLAAC, routing between interfaces, a caching DNS resolver, sockets on smoltcp, the firewall (`firewall.rs`) |
| `implementation/ports/smoltcp` | [smoltcp](https://github.com/smoltcp-rs/smoltcp)'s release and Veda's patch (`implementation/ports/README.md` explains each change) |
| `implementation/libraries/proto/src/{net,wlan,netring}.rs` | The service protocols and frame rings |
| `implementation/libraries/net` | `net`: blocking, `std::net`-like `TcpStream`, `TcpListener` and `UdpSocket`, name resolution, ping, `net::wifi` |
| `implementation/libraries/tls` | `tls`: the TLS 1.3 and 1.2 client |
| `implementation/libraries/web` | `web`: URLs, the HTTP/1.1 client (redirects followed) and the WebSocket client, over TCP or TLS |
| `implementation/libraries/posix/src/inet.rs` | The POSIX layer's sockets, `netd`'s too, for C programs and programs on Rust's standard library (see [C on Veda](C.md)) |
| `implementation/shell/desktop/src/firewall.rs` | The firewall's question, as the desktop shows it |
| `implementation/libraries/entropy`, `implementation/kernel/src/random.rs` | The kernel's random number generator |
| `verification/nettest` | In-system network checks |

Frames travel through shared-memory rings (`proto::netring`), a
single-producer, single-consumer ring each way per link with wake-up
events: one copy in and one out per frame, a system call only when the
other side is asleep, and a full ring makes `netd` wait rather than drop
frames. Neither side trusts the other: positions are compared modulo 2^64
and clamped to the ring, lengths are checked, and shared memory is accessed
only through volatile reads and writes.

## Who may do what

Both services check the identity the registry gives each connection, which
a process cannot forge:

| What | Who may |
|------|---------|
| Attach a network device (`netdev`) | The device manager (its drivers, the driver VM's included, speak as it) and `wlan` |
| Offer a radio (`wlanphy`) | The device manager |
| Configure interfaces, renew leases, set the DNS servers, change the firewall | Settings |
| Flush the DNS cache; read the firewall's settings, log, counts and questions; list every socket; see hardware addresses | Settings and the Terminal (others list their own sockets and see hardware addresses as zeros) |
| Hear and answer the firewall's questions | The desktop shell, about the applications asked about only |
| Change Wi-Fi: scan, join, leave, the radio switch, saved networks | The desktop shell, the agent, Settings and the Terminal |
| See what tells where the machine is or has been, or identifies it: BSSIDs, saved networks, diagnostics, the adapter's address | Settings and the Terminal (others get statuses without those addresses) |

Other `netdev` and `wlanphy` connections are closed. The Terminal only
reads, as the programs it runs have its identity. Nobody can list the DNS
cache, a record of every name any program looked up. Each process has
limits, however many connections it opens: on its `netd` connections,
sockets (with those its listeners accept), lookups in progress and status
watchers, and on its `wlan` connections and watchers. A share of each is
kept for system services, so that applications together cannot use it all.

## Sockets in `netd`

* **Flow control** is credit-based both ways (`proto::net`). Received data
  leaves the stack only once its message is queued to the client, so data
  a client does not read stays there and the TCP window closes; an accepted
  connection, the end of a stream or an error waits for room in the
  channel. Nothing a client is owed is lost.
* **POSIX sockets** go to other processes, which share their channels, so
  credit would go to whichever read it. A TCP connection's bytes go instead
  on a kernel socket between the program and `netd`, whose buffers are the
  flow control, and its channel brings only the connection and its
  failure. UDP datagrams go without credit: `netd` reads no more while
  256 KiB wait to be sent, and still sends, for three seconds at most,
  those sent before the socket closed.
* **Closing** a TCP socket delivers all the client sent (it may send only
  what fits the stack's send buffer), then a FIN. The connection finishes
  on its own: it is reset if the peer sends more, and dropped after a
  minute without progress or when more than 256 such connections wait.
* **The end of a stream** is the peer's FIN, whatever is still buffered;
  what a connection received stays readable after its smoltcp socket is
  gone (TIME-WAIT included).
* **Timeouts**: a quiet established connection is never given up. The
  two-minute timeout applies only while data or a FIN of ours waits for
  acknowledgement, or keep-alive probes (at least a second apart) are
  sent. A connection ends timed out if its peer stopped answering, and
  reset if the peer reset it, once the client finished sending too (not
  with the end of the stream).
* **A listener** holds at most its backlog of connections not accepted yet;
  further attempts are refused.
* **Ping** reports why a request failed: no answer in time, or what a
  router said (destination or host unreachable, time to live exceeded) and
  its address, so pings with a small TTL find the routers on the way.
* **IPv6 addresses** do not embed the hardware address: their interface
  identifiers, link-local included, are stable privacy identifiers (RFC
  7217) from a secret `netd` draws when it starts.
* **The DHCP lease** shown is the one the client accepted from its server
  (renewals included).
* **Random protocol numbers** (TCP initial sequence numbers, DHCP and DNS
  ids, source ports) come from a ChaCha20 stream keyed from the kernel's
  generator, instead of upstream smoltcp's predictable sPCG32.

## The firewall

The firewall (`netstack`'s `firewall.rs`) filters what comes from other
machines before smoltcp sees it, on every interface but loopback. `netd`
applies its settings (`proto::net::FirewallConfig`), which Settings
changes.

* **Defaults**: on; nothing comes in that this machine did not ask for;
  stealth; pings unanswered; a log. They hold from `netd`'s start until it
  has read the saved settings.
* **What always comes in**: replies (a connection's segments, which smoltcp
  takes if it has the connection; UDP datagrams from where this machine
  sent one in the last three minutes), DHCP's answers, the ICMP errors that
  tell TCP and ping what became of their packets (unreachable, time
  exceeded, IPv6's packet too big), and IPv6's router and neighbour
  discovery from the link itself (a hop limit of 255, which no router
  passes on).
* **Rules**: a new connection (a SYN alone) or a datagram for a port comes
  in when a port rule opens the port to its source, or the application
  whose socket is on the port accepts connections from there. A rule's
  source is anywhere, the local network (the subnets of the interface it
  comes in on, and link-local addresses) or a prefix. A port rule that
  closes a port wins over both, and "block all incoming connections" sets
  aside every rule that lets something in.
* **Stealth**: smoltcp answers what comes for a port nothing uses (a reset,
  "port unreachable"), which shows the machine is there. Stealth mode drops
  it instead, and such answers on their way out (a reset for a connection
  this machine sent nothing on, "port unreachable", "protocol
  unreachable"), so a scan finds nothing. Without it, closed ports answer
  as closed; ports a socket uses but no rule opens stay silent either way.
* **Pings** are answered from nobody (the default), the local network or
  anywhere.
* **Rate limits**, a burst then so many a second: new connections let in,
  20 then 10 from one address and 200 then 100 from all together; pings
  answered, 5 then 1 to one address and 50 then 20 to all together.
* **Applications kept off the network** may not use it at all while the
  firewall is on: their
  connections, pings and name lookups (but for `localhost` and address
  literals: a lookup tells the DNS servers the name) are refused with
  `PermissionDenied` (`EACCES` in C), their datagrams to other machines are
  dropped, and their connections are reset when the rule is set. Nothing
  from other machines reaches their sockets, whatever port rule opens the
  port. They still reach the machine itself over loopback.
* **What no host sends** is dropped, the firewall on or off: a source
  address that is the machine's own, loopback, multicast or broadcast (the
  IPv6 unspecified address is taken only for duplicate address detection
  and listener reports); a loopback destination; impossible TCP flags (a
  SYN with a FIN or a reset, a segment with neither SYN, ACK nor reset: a
  scan's "null", "FIN" and "Xmas" probes); port 0; IPv6's type 0 routing
  header; ICMP redirects, router solicitations and timestamps; protocols
  other than TCP, UDP and ICMP.
* **Fragments**: smoltcp reassembles IPv4 datagrams, whose ports are in the
  first piece only. The first piece is judged as the whole datagram would
  be and must hold its whole transport header; the others come in for a
  minute after their datagram's first piece did, unless they would rewrite
  its TCP header (RFC 1858). IPv6 fragments, which smoltcp does not
  reassemble, are dropped.
* **The log** keeps the last 256 things turned away, with the reason,
  interface, other machine, port and application (repeats within a second
  count as one entry, so a scan from one address is one), and counts since
  the system started. Settings shows the latest, with "Let in" for an
  application that was not reached; the Terminal's `firewall log` lists
  them. `netd` writes each change of the settings, and who made it, to the
  system log.
* **Where the settings are kept**: `/home/.private/netd/firewall`, in
  `netd`'s private directory, which the file system opens to no other
  program, so no application can read the rules or change them to let
  itself in. A thread of `netd`'s own reads and writes it, so the service
  never waits on the file system. A new file is written beside the old and
  takes its place; one that does not read back whole and valid is not used
  (the defaults hold).
* **Names**: rules name applications by their ids (`music`, `terminal`).
  The programs run in the Terminal speak for it: a rule about `terminal` is
  about them all.

### When an application listens

`netd` asks the user whether an application may accept connections when
it starts taking what other machines start (a listener, or a datagram
socket on a port of its choosing; one on a port the system picked is a
client's, whose replies come in anyway), not on loopback alone, and the
settings say nothing about it (no rule for it either way, and not kept off
the network). Nobody is asked while the firewall is off or blocks
everything incoming, which no answer would change.

Until the user answers, nothing from other machines reaches the
application. The desktop shell asks with a notification that stays until
it is answered: "Don't allow", or "Allow" from the local network (from
anywhere, ticked). Its buttons take clicks only a second after it appears,
so a click aimed at what was there just before, or a window timed to put
one there, does not answer it.

`netd` takes answers from the shell alone (`answer_firewall`), only about
the applications it asked about; an answer sets that application's rule
and nothing else, kept as Settings' changes are. "Don't allow" is a rule
too, so the application is not asked again. Settings lists the questions
waiting at the top of its applications, where a choice answers them as
well; every rule there is No, Local network or Anywhere, and removing one
has the user asked again the next time. One question waits per
application, sixteen at most, and none is asked before the saved settings
are read (they may answer it), nor about system services.

## Wi-Fi

### Joining a network

1. **Scan**: the radio scans every channel (probe requests where
   transmitting is allowed, directed ones for hidden networks); the service
   records every beacon and probe response, and lists networks by name and
   security.
2. **Authenticate**: open system authentication for WPA2 and open networks;
   SAE for WPA3, with both the hunting-and-pecking and the hash-to-element
   password element, its commit and confirm computed by the service and
   sent by the radio.
3. **Associate**: the radio associates with the service's RSN element,
   matching the access point's ciphers (CCMP-128) and management frame
   protection (required for WPA3, used whenever the access point is
   capable).
4. **Handshake**: the service's 4-way handshake derives the session keys
   from the PMK (the PBKDF2 pre-shared key for WPA2, the SAE key for WPA3)
   and gives the radio the pairwise and group keys; the group key
   handshake renews group keys.
5. **Link up**: `netd` runs DHCP and SLAAC on `wlan0`, and the default
   route moves to it unless a wired connection is up (Ethernet has the
   lower route metric).

### Staying connected

* **Saved networks** (`/home/user/.config/wlan/networks`) are joined
  automatically: the most recently used first, then the strongest access
  point (5 GHz gets a small bonus).
* **A lost connection** (the access point stops answering, disconnects the
  station, or the signal fades) is re-established after a fresh scan, on
  any access point of the network; one that stopped answering is not tried
  again until it is heard again.
* **Failed attempts** are retried after 2, 4, 8, 16 and 32 seconds, then
  every 60 seconds.
* **A weak connection** (below -72 dBm) moves to an access point of the
  same network at least 10 dB stronger, keeping the IP configuration.
* **A password the access point does not seem to accept** is retried after
  1, 5 and 15 minutes, then every hour, as repeated attempts could get the
  device blocked; connecting by hand tries again at once. Nothing proves a
  password wrong (anyone in range could send the unauthenticated frames
  that suggest it), so the network is never given up for good, and the
  user is told only that the password may be incorrect.
* **The radio** may go away and come back; the service rejoins. A driver
  that stops answering control calls for two seconds has its radio dropped
  rather than hanging the service, and if `wlan` exits, `init` restarts it
  and the driver offers the radio again.

### Security

* **All key material** comes from the kernel's ChaCha20 random number
  generator, seeded from the firmware's `EFI_RNG_PROTOCOL`, RDSEED, RDRAND
  and timing jitter.
* **Every received frame is untrusted**: parsers are bounds-checked and
  never panic, MICs and authentication tags are compared in constant time,
  received key material is used only after the standard's checks pass, and
  CCMP and BIP enforce replay counters (duplicate frames are dropped).
* **Management frame protection**: once it is in use, unprotected
  deauthentication and disassociation frames are ignored, and one starts
  an SA Query to check whether the access point really lost the
  association.
* **No downgrades**: a network saved with security is never joined through
  an open access point, nor one saved as WPA3 (or where WPA3 was used once)
  through an access point offering only WPA2; WEP, TKIP-only WPA,
  Enterprise and OWE networks are listed but never joined. A password is
  never used with an open access point (an open "evil twin" of a protected
  network is neither joined nor saved as open): the desktop and Settings
  say which security the network the user picked has, and a request with a
  password takes only access points with security.
* **RSN elements**: the one the access point sends in the handshake must
  match the one it advertised, and so must the RSN Extension element (in
  both and the same, or in neither), whatever the AKM.
* **SAE**: a frame that does not verify is discarded, as 802.11 says, and
  the access point's repeated commits and anti-clogging requests count
  against the attempt's tries, so forged frames can neither end an attempt
  nor keep it going for ever.
* **Passphrases** are checked (8 to 63 printable characters, or 64 hex
  digits), never logged, never returned by the API, and kept out of the
  Terminal's history. Session keys are zeroed when a connection ends (with
  the handshake's copies of what it installed), and so is the decrypted
  key data of the handshake's messages; a new pairwise key keeps the group
  keys.

> [!IMPORTANT]
> The saved-networks file holds the passphrases, which rejoining needs, and
> the file system has no access control yet: any program can read it.

### Radios

Linux's radios are *managed* radios: Linux's 802.11 stack is their MLME on
`wlan`'s commands (`wlanmlme_ctl`, in `proto::wlan`), which `wifi` carries
out through nl80211 (see [the driver VM](DRIVERVM.md)). Linux retries,
times out, encrypts and decrypts, answers the access point's SA Queries
and watches the link; the station in `wlan` decides and authenticates.
EAPOL comes and goes over nl80211's control port, owned by `wifi`'s socket
(message 4 in clear, whatever key follows it). Linux joins only access
points it heard lately, so `wifi` listens on the access point's channel
first when it must. A *soft MAC* radio, which only moves
raw 802.11 frames (`wlanphy_ctl`), has `wlan` as its MLME too: scanning
channel by channel, sending the authentication and association frames,
encrypting with CCMP. No driver offers one now.

## Using the network

The taskbar's network icon shows the Wi-Fi signal or the wired connection
and opens a flyout for Wi-Fi; Settings → Network & Internet shows the
connections, saved networks, interfaces, the firewall and the Wi-Fi
diagnostics log. The Terminal has `wifi` (`status`, `scan`, `list`,
`connect SSID [PASSWORD]`, `disconnect`, `saved`, `forget`, `auto`, `on`,
`off`, `aps`, `log`), `ifconfig`, `ping`, `nslookup`, `netstat`, `route`,
`curl`, and `firewall` (`log`, `stats` and the questions waiting; it only
reads).

### For applications

```rust
use net::{Duration, TcpStream};

let mut s = TcpStream::connect_host("example.com", 80, Duration::from_secs(20))?;
// HTTP or HTTPS, following redirects; the body at most 1 MiB.
let page = web::http::get("https://example.com/", Duration::from_secs(20), 1 << 20)?;

net::wifi::scan()?;
for n in net::wifi::networks()? {
    // n.name, n.security, n.signal_dbm, n.bars, n.saved, n.connected
}
net::wifi::connect("Veda Home", Some("password"), true)?;
let events = net::wifi::Watcher::new()?; // status changes, scans, failures

let config = tls::ClientConfig::with_alpn(&[b"http/1.1"]); // share it
let mut tls = tls::connect("example.com", 443, Duration::from_secs(20), &config)?;
tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")?;
let n = tls.read(&mut buf)?; // Ok(0): the server closed the connection (close_notify)
```

## TLS

`tls` is the TLS 1.3 and 1.2 client of Veda applications (HTTPS, secure
WebSockets, anything else over TLS), running in the application's process
over a `net::TcpStream`. The web browser's engine has its own, as WebKit
expects: OpenSSL, through GIO's glib-networking, trusting Mozilla's list
of certificate authorities ([The web browser](BROWSER.md#webkit-on-veda),
and [why not rustls](DEPENDENCIES.md#c-and-c-libraries)).

* **Protocol**: [rustls](https://github.com/rustls/rustls) 0.23, through
  its `no_std` unbuffered API; `TlsStream` handles partial records, several
  records per read, key updates, session tickets, alerts and close_notify.
  The end of the stream is the server's close_notify: a connection that
  just ends may have been cut short by anyone on the path, and reading
  reports `TlsError::Truncated` once the data before it is read.
* **Cryptography** is `tls`'s own rustls provider on pure-Rust crates (no
  C, no assembly): AES-128-GCM, AES-256-GCM and ChaCha20-Poly1305 records;
  SHA-256/384 and HMAC, with rustls' HKDF and TLS 1.2 PRF over them;
  X25519, secp256r1 and secp384r1 key exchange; ECDSA (P-256, P-384),
  Ed25519, RSA PKCS#1 v1.5 and RSA-PSS signatures. The cipher suites are
  the three of TLS 1.3, and for TLS 1.2 ECDHE-ECDSA and ECDHE-RSA with
  AES-GCM or ChaCha20-Poly1305. RSA verification is `tls`'s own, on
  `crypto-bigint`, with *ring*'s rules (2048 to 8192-bit moduli, PSS salts
  as long as the hash). Ephemeral secrets come from the kernel's ChaCha20
  generator and are zeroed after use. `Cargo.lock` lists *ring* because of
  a weak optional feature of rustls-webpki; it is never built.
* **Certificates** are verified by rustls-webpki against the Mozilla root
  program (webpki-roots, 121 roots compiled in), at the system clock's time
  (the firmware's clock, advanced by the kernel). A clock that reads
  earlier than 2025 is treated as unset: the handshake fails saying so,
  rather than with misleading expiry errors.
* **Errors** are `TlsError`s with messages for users. After a protocol
  error, the alert rustls produces is sent to the server and every later
  call reports the same error. Nothing panics on what the network sends.
* **Event loops** wait for `tls.transport().handle()` to become readable
  and call `try_read` until it returns `Ok(None)`: it decrypts what the
  socket holds without waiting, and sends what it owes (a key update's
  answer) as far as the socket takes it at once. `try_write` sends small
  messages the same way, queuing the rest in order, so a server that stops
  reading cannot stall the loop.

## Testing

**On the host**, `cargo automation test` runs:

| Tests | Cover |
|-------|-------|
| `wlan` | Frames, RSN, crypto vectors, handshakes, SAE, the profile and policy modules; station-against-access-point runs with every security type, wrong passwords, hidden networks, replays, deauthentication, silent access points, reconnection, rekeying, and frames forged or altered on the air |
| `netstack` | Two stacks over a simulated cable: DHCP (renewals, server outages, other clients' acknowledgements), TCP (quiet connections, half closes, closing with data in flight, clients that left), UDP, DNS (unreachable servers included), ICMP and its errors, IPv6, a full link; the firewall, packet by packet and between the stacks (what it lets in and turns away, stealth, pings, replies, fragments, floods, forged packets, the log) |
| smoltcp | Its own tests, with Veda's patch |
| `tls` | Handshakes with an in-memory rustls server: every cipher suite, key exchange group and certificate type, data both ways, closing from either side, key updates, records split down to single bytes, expired, future, misnamed, untrusted and client-only certificates, broken servers, every bit of the server's first flight damaged in turn; the chains of api.deepgram.com, example.com and www.google.com (captured on 2026-10-03) against the built-in roots |
| Test vectors | RFC 8439 (the smoltcp port's ChaCha20), RFC 3394, RFC 4493, IEEE 802.11's PSK, PRF, CCMP and SAE vectors; for TLS, RFC 8448's handshake trace, RFC 4231, RFC 8032, the TLS 1.2 PRF and RSA signatures made by OpenSSL; the kernel's generator against its known answers |

`cargo test -p tls -- --ignored --nocapture` connects to those three
servers for real with each cipher suite and group.

**In Veda**, `nettest` runs from the kernel command line:

| Command line | Checks |
|--------------|--------|
| `run=nettest` | DNS, UDP, TCP, HTTP and ping over the default route, Ethernet or Wi-Fi |
| `run=nettest:https` | HTTPS requests to example.com (200 and the page) and api.deepgram.com (401 without a key), with the TLS version, cipher suite and handshake time |
| `run=nettest:wifi,SSID,PASSWORD` | Joins a network before its checks |

The first two reach real Internet hosts through QEMU's NAT, so the host
must be online. `wlan` lets `nettest` change Wi-Fi only when the kernel
command line started it (the registry's `ClientIdentity::requested`),
never when an application did.

**GUI and recovery** scripts (`cargo automation test --ui`), in
`verification/ui/`:

| Script | Checks |
|--------|--------|
| `network-cable.vts` | Plugs the wired card's cable in as the driver VM attaches the card, then unplugs and replugs it (QMP `set_link`): the default route comes, goes and comes back |
| `drivervm-restart.vts` | Crashes the driver VM: the network comes back with it |
| `drivervm-net.vts`, `e1000e.vts`, `drivervm-usb.vts` | Linux's drivers of QEMU's Intel 82576 and 82574L, and of its USB network adapter |
| `settings-firewall.vts` | Changes the firewall in Settings: `netd` applies the changes and reads them back after a restart |
| `firewall-question.vts` | Starts a program that listens (`run=nettest:listen,8080`), answers the question on the desktop, checks that nobody is asked again after a restart, and changes the answer in Settings |

The network cards' scripts check against a server on the host
(`nettest local`) and need no Internet. `systest` checks that a program
other than Settings may not change the network's configuration or read the
firewall, and that one other than the desktop shell may not hear or answer
its questions.

## Limitations and next steps

* **The firewall** has no profiles per network (home, public), and no
  rules for what goes out but per application. Rules cannot tell apart the
  programs run in the Terminal, and its question is about them all.
* **Wi-Fi hardware** is Linux's, through the driver VM: Intel's AX211 has
  its firmware so far (`implementation/ports/linux/firmware.txt`); other
  cards need their driver and firmware added. Next are 6 GHz channels, and
  telling Linux the country (it keeps to the world's rules, listening first
  on channels that need it). QEMU cannot emulate a Wi-Fi adapter, so beyond
  `wlan`'s host tests Wi-Fi is tested on PCs.
* **Wi-Fi features** not there yet: WPA2/WPA3-Enterprise (802.1X/EAP),
  Enhanced Open (OWE), fast roaming (802.11r), power saving, 802.11n/ac
  rate control.
* **TLS** has no session resumption (each connection is a full handshake),
  client certificates, revocation checking (OCSP, CRLs), Encrypted Client
  Hello or post-quantum key exchange (X25519MLKEM768) yet. The trusted
  roots are compiled in: updating them needs a new build, and there is no
  system certificate store to add one to (`ClientConfigBuilder` can, per
  application). Certificate checks depend on the firmware clock being
  right.
* **HTTP**: one request per connection (`Connection: close`); no
  compressed bodies, cookies or caching.
* **IPv6** has no temporary addresses (RFC 8981) yet; the stable privacy
  addresses change when `netd` starts again (its secret is not kept on
  disk).
* **Saved passphrases** are not encrypted at rest yet.
