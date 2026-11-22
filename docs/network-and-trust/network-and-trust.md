# Network and trust

How two desks find each other, prove who they are, and decide whether one may connect to the
other. A **desk** is a machine running Illumination. The desk that shows its screen is the
**host**; the desk that looks at it is the **controller** (its window is the viewer).

There is no server in between. A controller connects straight to the host's IP address,
normally over a [Tailscale](https://tailscale.com) network (a **tailnet**).

Code: `crates/illum-net` (listening, TLS, handshake, the UDP path), `crates/illum-core`
(`protocol.rs`, `store.rs`), `crates/illum/src/host.rs` (the session on the host).

The section numbers are fixed: code comments cite them as "architecture §4.1" and so on.

## 4. Network and trust

### 4.1 Listening

- **One port number, default 28326, used twice.** TCP carries the TLS control connection. UDP
  carries media. To the system these are two ports that do not collide; to the person it is
  one number and one firewall rule. 28326 is unassigned at IANA and outside Windows' range
  for outgoing ports. It is `DEFAULT_PORT` in `illum_core::protocol`.
- **`--port`** (with `--service`) moves both together.
- **The host listens on the machine's tailnet addresses only**: those in `100.64.0.0/10` and
  `fd7a:115c:a1e0::/48` (`illum_net::bind`). It never listens on `0.0.0.0` by default, and it
  does not use UPnP.
- **`--bind <IP>`** (with `--service`, repeatable) replaces the tailnet addresses with the
  ones given: a LAN address, or `127.0.0.1` for testing.
- With no tailnet address and no `--bind`, the host does not listen and says so. The host of
  an installed desk tries again every 5 s, and listens anew when the machine's addresses
  change.
- One TCP listener and one UDP socket are opened per address. Media for a session leaves on
  the address its control connection came in on.
- **The controller opens no listening port.** It sends UDP to the host's port from a port the
  system picks, and the host answers to where those datagrams came from.
- **Who listens.** The app listens while it is open. On an installed desk the service's host
  listens instead. `illum --service` listens from a console.
- **Addresses only.** The app connects to an IP address. A name is refused, not looked up.
  A target without a port gets the default port.

### 4.2 Identity and TLS

- **Each desk has one key.** On first start it makes an Ed25519 key and a self-signed
  certificate (`CN=Illumination Device`, valid until 2075) and keeps them in its home folder
  as `device-key.der` and `device-cert.der`. The home folder is `%ProgramData%\Illumination`
  on Windows and `~/.illumination` elsewhere; `--home` names another.
- **The fingerprint** is the SHA-256 of the certificate, written as 64 hex characters and
  shown in groups of four. The app shows it as the desk's key; so does `illum fingerprint`.
  A desk is identified by its fingerprint, nothing else.
- **Every connection is TLS 1.3 with a certificate on both sides** (`rustls`). TLS 1.2 is
  refused. The ALPN name is `illum/1`; a peer that speaks anything else fails the handshake.
- **TLS proves possession of the key, not who the peer is.** No authority signs the
  certificates, so the TLS layer accepts any certificate and checks only the handshake
  signatures. Whether the peer is welcome is decided right after, from the trust store.

**The trust store** is `trust.json` in the home folder (`illum_core::store::TrustStore`). It
is versioned JSON; a file with another version number is refused. It holds three maps:

| Map | Key | Meaning |
|---|---|---|
| `known_hosts` | address with port, as connected to | The host key this desk met at that address |
| `paired_peers` | fingerprint | Desks that may always connect here |
| `desks` | fingerprint | What is known about each desk (§9) |

Desks pin each other's keys on first meeting. To **pin** a key is to remember it and expect
the same one next time.

- **Controller side.** The first time a desk connects to an address, it remembers the host's
  key without asking, once the host has accepted it. From then on a different key at that
  address is an error (`HostKeyChanged`) and the connection stops before the hello.
  Forgetting the desk in the app, or `illum forget <address>`, clears the pin.
- **Host side.** A controller is known by its key. An unknown key is a new desk, and §9
  decides what happens to it. A known desk with a new key is also a new desk.

On a tailnet the address already says which machine answers. With `--bind` on an ordinary
network there is no such guarantee, and the first key met is still pinned without a
question.

### 4.3 Session handshake

See [`session.d2`](session.d2). Code: `illum_net::session::connect` (controller) and
`accept` (host).

1. **TCP connect**, then **TLS 1.3** with both certificates.
2. Both sides **export the media keys** from the TLS session (§4.4).
3. **The controller checks the host's key** against its pin for this address (§4.2). A
   mismatch ends here.
4. **Hello, both ways.** The controller sends first. A hello carries the protocol version,
   the app version, the desk's name, its system ("Windows 11") and its **capabilities**. The
   protocol versions must be equal, or the host answers `denied`.
5. **The host decides** whether this desk may connect (§9). This may put a question to the
   person at the host.
6. The host sends **`accepted`**, or **`denied`** with a reason and closes.
7. On `accepted` the controller pins a new host's key and notes the connection in its list
   of desks.

The app connects twice: once to check the way and the key, ended with `bye`, and once from
the session window it then opens.

The screen starts afterwards: the controller sends `screen_start`, the host answers
`media_endpoint` with its UDP port, and media begins (§4.4, [video.md](../session/video.md)).

Every step has a deadline, so nothing hangs:

| Step | Limit |
|---|---|
| TCP connect | 5 s |
| TLS handshake | 10 s |
| Each hello | 5 s |
| The person's answer at the host | 60 s, then it counts as no |
| The controller's wait for `accepted` | 65 s |

**Capabilities** are short names in the hello. A feature is used only if both sides list it.
This is also how the host says what a connected desk may do: what is switched off is left
out of its hello (§9).

| Capability | Meaning |
|---|---|
| `ping` | Round-trip probes on the control channel |
| `screen` | The host can show its screen |
| `input` | The host accepts keyboard and mouse |
| `udp-media` | Media on the UDP path |
| `audio` | The host can send its sound |
| `clipboard` | Clipboard text is shared both ways |
| `input-notice` | The controller can show "input is not arriving" |
| `displays` | The controller can show and switch the host's displays |
| `sas` | The host can show the Ctrl+Alt+Del screen (an installed host) |
| `cursor-kind` | The controller can draw its own pointers from the host's pointer kind |
| `idle-warning` | The controller can show the idle warning (§10) |
| `lock` | The host can be locked from the controller |
| `enc:…`, `dec:…` | Video codecs a side can encode or decode ([video.md](../session/video.md)) |

The control messages that belong to the connection itself:

| Message | Direction | Meaning |
|---|---|---|
| `hello` | both | First message after TLS |
| `accepted` | host → controller | The session may begin |
| `denied { reason }` | host → controller | Refused; the connection closes |
| `ping`, `pong` | both | Round-trip probe |
| `media_endpoint { port }` | host → controller | Send media datagrams to this UDP port |
| `idle_warning { seconds }` | host → controller | The session ends in `seconds`; 0 takes it back (§10) |
| `bye` | both | Deliberate end. The other side does not reconnect |

The other messages of `ControlMsg` are described with their subject: screen and quality in
[video.md](../session/video.md) and [adaptation.md](../adaptation/adaptation.md), input in
[input.md](../session/input.md).

### 4.4 Two paths: reliable control, unreliable media

See [`media-transport.d2`](media-transport.d2). A session uses two paths. The reliable one
sets things up and carries what must arrive. The unreliable one carries what must arrive
*now* or not at all.

| Path | Carries | Properties |
|---|---|---|
| **TLS 1.3 over TCP** | hello, control messages, keys, buttons, wheel, text, the host's cursor, clipboard, feedback and clock probes | Everything arrives, in order |
| **UDP**, same port number | video and audio to the controller; mouse moves and punch datagrams to the host | Never resent. Each datagram is encrypted and authenticated |

**Why two.** A reliable stream under load queues: latency rises instead of data being
dropped, and one lost packet holds back everything behind it. For video a late frame is
worthless, so a lost datagram costs at most one frame, and parity usually repairs it
([adaptation.md](../adaptation/adaptation.md)). A mouse move is the one input event where the newest
replaces the rest, so it travels on UDP too; everything else about input must arrive in
order and stays on TCP ([input.md](../session/input.md)).

#### The TCP path

The TLS stream is split into frames: one byte for the channel, a 4-byte length, then the
payload (at most 8 MB). A send queue (`illum_net::outbox`) always sends the most urgent
frame first, so input never waits behind bulk data.

| Channel | Priority | Use |
|---|---|---|
| `Input`, `Cursor` | 0, first | Input events to the host; cursor position and shape to the controller |
| `Control` | 1 | `ControlMsg`, as JSON |
| `Video` | 2 | Reserved; video travels on UDP |
| `Audio` | 3 | Reserved; audio travels on UDP |
| `Clipboard`, `Files` | 4, last | Clipboard text. `Files` is reserved |

A connection on which nothing arrives for 30 s is closed as dead. A live session never comes
near this: the controller sends clock probes several times a second.

#### The UDP path

Code: `illum_net::media`.

- **Keys come from the TLS session.** Both sides ask TLS for 72 bytes of exporter output
  (RFC 5705, label `illumination media v1`): a host-to-controller key, a controller-to-host
  key, and a 64-bit session id. There is no second handshake.
- **Encryption** is AES-256-GCM, one key per direction. The nonce is the stream byte followed
  by the sequence number, which is never reused under one key.
- **Replay protection:** each stream accepts a sequence number once, within 256 of the
  highest seen. A datagram that fails any check is dropped silently.
- **Datagrams are at most 1200 bytes**, to fit a tailnet's 1280-byte MTU.

| Bytes | Field |
|---|---|
| 0 | Magic `0x49` |
| 1 | Stream: 0 punch, 1 video, 2 audio, 3 input |
| 2–9 | Session id, in clear, so the host finds the session before decrypting |
| 10–17 | Sequence number, per stream and direction |
| 18– | Ciphertext, then a 16-byte authentication tag |

Bytes 0–17 are authenticated with the payload.

**Finding the controller.** The host does not know the controller's UDP address in advance.
After `media_endpoint` the controller sends an empty **punch** datagram every 200 ms until
video arrives, then every 2 s to keep the path open. The host sends media to the address of
the latest authentic punch, so a controller whose address changes is followed.

**A blocked path says so.** If no punch reaches the host within 5 s, the host reports a
`screen_error` naming the UDP port. If no video reaches the controller within 5 s, its
window says the same.

What is inside video and audio datagrams, and how the sending rate is chosen, is in
[video.md](../session/video.md), [adaptation.md](../adaptation/adaptation.md) and
[clipboard-audio.md](../session/clipboard-audio.md).

## 9. Permissions and profiles

The person at a desk decides two things about the desks that reach it: **which may connect**,
and **what a connected desk may do**.

### Which desks may connect

**A desk is one entry**, kept by its key in the trust store (`DeskInfo`), whichever way the two
desks met. The entry holds what the desk calls itself, its system, the address it was last seen
at, the person's own name for it, whether it is a favourite, and three times: this desk's last
connection to it, its last connection here, and its last refused attempt.

**Permission** says whether a desk may connect here (`illum_core::store::Permission`):

| Permission | In the app | What the host does |
|---|---|---|
| `Always` | Always allow | Lets the desk in. Nobody is asked |
| `Ask` | Ask | Asks the person at the host each time |
| `Never` | Never allow | Refuses the desk. Nobody is asked |

- **A desk has a permission of its own, or none.** It is set on the desk's card in the app,
  or by an answer to the question below.
- **With none, the default for new desks applies.** It has the same three values, is set in
  Settings, and is `Ask` unless changed (`new_desks`).
- **A desk's own permission always wins** over the default.
- **A paired desk is one whose own permission is `Always`.** It is stored as an entry in
  `paired_peers`; `Ask` and `Never` are stored in the desk's entry.
- **A desk let in by a default of `Always` is not paired.** Nothing is stored about its
  permission. If the default changes later, the desk follows the new default.
- **Who may connect is locked while a desk is connected here.** A desk that controls this
  one could otherwise open the app here and let itself, or any desk, in for good. Until
  every connected desk is gone, the app here does not change the default for new desks,
  what a connected desk may do, the idle time, or any desk's own permission, and does not
  forget a desk. Names, favourites and the desks this one connects to can still change.
  On an installed desk the host refuses these changes itself (the app may not write its
  files), so the lock holds whatever program asks. AnyDesk and RustDesk ask for an
  administrator's rights for their security settings instead; this asks for nothing, and
  ends when the session does.

The host decides in this order (the "door" in `illum_net::session::accept`):

| # | Case | Result |
|---|---|---|
| 1 | The desk's own permission is `Always` | In |
| 2 | The desk's own permission is `Never` | Refused |
| 3 | The desk was let in "this time" and is still a guest | In |
| 4 | The desk's own permission is `Ask` | Asked |
| 5 | The desk has none: the default for new desks | In, asked or refused |

A refused desk gets `denied` with "not accepted at the host". Every attempt is noted in the
desk's entry, let in or not.

**The question** shows the desk's name, system, address and key, and has four answers:

| Answer | Effect |
|---|---|
| No | Refused this time. Nothing is remembered |
| This time | In as a **guest**. Nothing is written |
| Always | In, and paired: its permission becomes `Always` |
| Never for this desk | Refused, and its permission becomes `Never` |

No answer within 60 s is a no.

**A guest** is kept in memory only. It may connect without a question while it has a session
here and for two minutes after its last one ended. A dropped link therefore does not ask
again, and neither does the session window that follows the app's first connection.
Restarting the host forgets all guests.

**Where the question appears:**

| Host | Question |
|---|---|
| The app, listening itself | In the app's window |
| The host of an installed desk | A small window of its own (`illum ask`), run as the person signed in at the screen. Nobody signed in, or the window closed: no |
| `illum --service` in a console | A `y/N` line. `y` means Always |

**A key is not a wall.** A desk is known by its key, and a desk that makes itself a new key
is a new desk. `Never` stops a known machine from asking again. It does not keep anyone out
under a default of `Always` or `Ask`.

### What a connected desk may do

Three switches in Settings apply to all connected desks alike. The screen is always seen.

| Setting | In the app | Default | Capability left out when off |
|---|---|---|---|
| `let_control` | Use keyboard and mouse | on | `input`, `sas`, `lock` |
| `let_sound` | Hear sound | on | `audio` |
| `let_clipboard` | Share clipboard | on | `clipboard` |

The host leaves what is switched off out of its hello, so the controller treats the feature
as absent: a host without `input` opens as view-only.

A change to these switches makes the host listen anew with the new offer. A desk connected
at that moment keeps what it had until it connects again. A change to the default for new
desks takes effect at once.

The settings are in `settings.json` in the home folder. A host started
as `illum --service` does not read it: it asks about every new desk and takes `--no-audio`
and `--no-clipboard` from its command line.

## 10. Session policy

- **Several desks may be connected at once.** Each has its own session, with what the host
  offers. The app's home screen lists the connected desks; a desk with several windows open
  is listed once.
- **Sending a desk away.** Each connected desk has a "Disconnect" button. The host sends
  `bye`, so the controller's window closes and does not reconnect. A guest sent away is asked
  about again at once. A paired desk can simply connect again; forget it or set it to
  `Never` to stop that.
- **Idle sessions end.** A connected desk that sends no keyboard or mouse input for the idle
  time is disconnected by the host. The idle time is set on the host: 30 minutes by default;
  10, 60 or 120 minutes, or never.
  - One minute before, the host sends `idle_warning` to a controller that lists
    `idle-warning`, and the controller's window says so. Input takes the warning back.
  - The host then sends `bye`, so the controller does not reconnect.
  - Input is counted per desk, not per window: a desk with several windows open stays
    connected while it uses any of them.
  - A view-only session sends no input and so ends by the same rule.
  - A host started as `illum --service` has no idle time.
- **The host stays awake while its screen is streamed.** On Windows the capture thread holds
  a system and display power request (`illum_win::power`), so the machine does not sleep or
  blank the screen under a session.
- **Closing the app ends every session**, each with a `bye`.
