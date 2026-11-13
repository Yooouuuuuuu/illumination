# Illumination architecture

Status: agreed design. M1–M3 are implemented (v0.4.x); the media transport, codec and adaptation
sections describe what comes next. Last updated 2026-10-22.

Diagrams in this folder (render with `d2 <file>.d2`):
- [`processes.d2`](processes.d2): roles and processes on one machine
- [`session.d2`](session.d2): connection and session start
- [`video-pipeline.d2`](video-pipeline.d2): the media path from host to controller
- [`media-transport.d2`](media-transport.d2): the two paths, reliable and unreliable
- [`adaptation.d2`](adaptation.d2): the adaptation cycle, once a second — grade, latency controller, FEC controller, what is applied

## 1. Goals and non-goals

**Goals**
- Remote desktop between your **own machines, by IP address or hostname**. The main case is a
  Tailscale tailnet; a LAN or other VPN works the same way.
- It should feel as good as AnyDesk on the same link: low latency, sharp text, correct colours.
- **One app for both sides.** Every install can control other machines and be controlled.
- Free and open: GPL-3.0-only, written in Rust.

**Non-goals**
- No ID server, no rendezvous, no relay, no accounts. Getting through NAT is the network's job.
  Tailscale already picks a direct path or its own DERP relay.
- No UPnP and no exposure to the public internet.
- No compatibility with Moonlight, Sunshine, RustDesk or AnyDesk clients.

**First target platform:** Windows 10/11 x64, for both roles. The AnyDesk behaviour we copy was
observed on Windows. Other platforms come later and get their own capture and input modules.

## 2. The design rule

When sources disagree, **follow AnyDesk's observed behaviour.** The sources are AnyDesk, Sunshine,
RustDesk and general knowledge. AnyDesk has been stable for many users for a long time. "Observed"
means what its logs, sockets and documentation show. It is closed source, so we rebuild behaviour
and copy no code.

Exceptions, decided on purpose:
1. **No relay and no ID.** That is the product's scope, so AnyDesk's relay link and its
   relay↔direct "connection switch" layer are left out.
2. **No UPnP.** AnyDesk tries to forward its port on the router. We bind to chosen interfaces only.
3. **Pixels and audio travel over UDP and are never resent** (decided 2026-10-22). AnyDesk sends
   everything over one TCP connection, and TCP resends lost data, which holds up everything behind
   it. Our first two-machine test showed the cost of a reliable stream under load (§4.4). Control
   and input stay on TLS over TCP, as AnyDesk does.

The evidence behind each decision is in [`../lessons/lessons.md`](../lessons/lessons.md).

## 3. Process model

One binary, `illum`, runs in four roles chosen by command-line flag. This is the same split
AnyDesk uses (`AnyDesk.exe --service`, `--backend` and `--control`, plus the window, which takes
no flag). See [`processes.d2`](processes.d2).

| Role | Runs as | Lifetime | Responsibilities |
|---|---|---|---|
| `illum --service` | SYSTEM, session 0 | Always on (Windows service, auto-start) | Listens for connections; TLS; pairing and fingerprints; permissions and config; starts the other roles; Ctrl+Alt+Del (`SendSAS`) |
| `illum --backend` | **SYSTEM inside the console session** | One per incoming session | Screen capture, encoding, input injection, desktop switching (secure desktop), audio capture, the accept/permissions panel, wake lock |
| `illum --control` | Logged-in user | One per user session | Clipboard (text and files), tray icon |
| `illum` | Logged-in user | While the window is open | Device list, outgoing connections, viewer (decode and present), settings |

Rules:
- The backend runs as SYSTEM *inside* the console session, so it can follow the switch to the
  secure desktop (UAC prompts, lock screen, Ctrl+Alt+Del screen). AnyDesk logs this:
  `Spawning backend (elevated 1, session 1)`, `Backend spawned as system user`.
- The clipboard belongs to the user's desktop, so it lives in `--control`. AnyDesk does the same:
  its `ctrl` process logs `clipbrd.capture - Registered for clipboard notifications`.
- Local IPC uses named pipes that only SYSTEM and the session's user can open. The service checks
  which process and session is on the other end before trusting it.
- The service restarts `--control` and `--backend` when the console session changes (login,
  logout, fast user switching).

*v0.4.x:* the session logic runs inside `--service` itself; the per-session backend and the
control process arrive with the service milestone.

## 4. Network and trust

### 4.1 Listening
- **One port number**, default **28326**, used twice: **TCP** for control, input and clipboard,
  **UDP** for video and audio. To the operating system these are two separate ports that don't
  collide; to the user it is one number to set and one firewall rule. 28326 is unassigned in the
  IANA registry, outside Windows' outgoing-port range, and used by no well-known program.
  *(In the code since v0.5.0.)*
- `--port` moves both together, the way Sunshine moves its whole port family.
- Default bind: **Tailscale interfaces only** (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`). LAN
  interfaces are opt-in. Never `0.0.0.0` by default, and no UPnP.
- The viewer opens no listening port. It sends to the host's UDP port from an ephemeral local
  port, and replies come back through that; Windows Firewall lets them through on its own.
- Hosting can be switched off, for machines that only control others (AnyDesk's "Allow direct
  connections" setting).

### 4.2 Identity and TLS
- Each device makes a **self-signed certificate** on first start and keeps it. AnyDesk does the
  same: `CN=AnyDesk Client`, valid for 50 years, created when it logged "Generating a new
  certificate and key".
- Connections use **TLS 1.3 with certificates on both sides** (mutual TLS). AnyDesk's port 7070
  asks the connecting side for a certificate too. Key type: Ed25519 (via `rustls`).
- There is no ID server to vouch for keys, so trust comes from **pairing**:
  - **First connection:** both sides show the other's fingerprint. The host owner accepts, and
    each side stores (pins) the other's fingerprint.
  - **Later connections:** a fingerprint that doesn't match is a hard error.
  - Optional later: ask Tailscale who is connecting (its local `WhoIs` API) and show the tailnet
    user and device name.

### 4.3 Session handshake
See [`session.d2`](session.d2).
1. TCP connect, then TLS 1.3 (mutual), then the fingerprint check.
2. **hello** in both directions: protocol version, **capability list** (including which video
   codecs each side can encode and decode in hardware), host keyboard layout, displays. AnyDesk
   logs `Remote caps: sound_src, sound_sink, keyboard_sink, …`. A feature is used only if both
   sides list it.
3. The service starts a backend in the console session.
4. **Authorisation.** Interactive profile: the backend shows the accept panel to the person at
   the host. Unattended profile: a paired device, optionally with a password.
5. **Session accepted.** The host sends the granted permissions. Control, input and clipboard
   continue on the TLS connection; video and audio start on the UDP path (§4.4).

### 4.4 Two paths: reliable control, unreliable media
See [`media-transport.d2`](media-transport.d2). This is the SIP/WebRTC/Sunshine shape: a
reliable channel to set things up and carry what must arrive, and an unreliable channel for
media that must arrive *now* or not at all.

| Path | Carries | Properties |
|---|---|---|
| **TLS 1.3 over TCP** (exists since M1) | pairing, hello, permissions, **input** (keys, buttons, wheel, text), cursor, clipboard, files, feedback and probes | Everything arrives, in order. Priorities inside this path: input and cursor first, then control, then clipboard and files. |
| **UDP**, same port number | **video and audio**, and **mouse moves** the other way (v0.11.2) | Never resent. Encrypted with keys **exported from the TLS session** (RFC 5705 keying material), so no second handshake is invented. Each packet is authenticated. A move is the one input event where the newest replaces the rest, so it belongs here: on TCP a 1000 Hz mouse queued clicks behind itself and one lost segment froze the pointer for a retransmit. |

**Why media left TCP** (the override in §2):
- The first two-machine test (v0.4.0, laptop → this PC over Tailscale) sent about 13 Mb/s of
  lossless tiles into a link that could not carry it. Nothing was lost (Tailscale reported 0%),
  but the data **queued**: the round trip rose from 12 ms idle to 48–75 ms, frame latency to
  about 100 ms, and the frame rate fell to 11. That is what a reliable stream does under load.
- On TCP, one lost packet also stops every frame behind it until the resend arrives. Over UDP a
  lost packet costs one frame at most, and parity data usually repairs it (§5.6).
- Sunshine, the one open-source peer that handles games well, works this way. AnyDesk stays on
  TCP but sends roughly a tenth of the data, so its queue stays short.

**What stays from the TCP design:** input never waits behind video (it travels the other way
and on the other path). The per-frame acknowledgement from v0.3.0 remains, but as **feedback**
(latency and loss per frame) rather than flow control; pacing is now the bitrate target (§5.5).

**Implemented (v0.5.0)**, `illum_net::media`, `illum_media::packets`, `illum_media::pacer`:
- The viewer sends a **punch** datagram every 200 ms until video arrives, then every 2 s as a
  keepalive; the host learns the viewer's address from the first authenticated punch. Either
  side reports a blocked path after **5 s** instead of hanging.
- Keys: 72 bytes of TLS exporter output (label `illumination media v1`): host→viewer key,
  viewer→host key, and a 64-bit session id carried in clear so the host can route the datagram
  before decrypting. AES-256-GCM, nonce = stream byte + per-stream sequence number, a 256-entry
  replay window per stream.
- Datagrams are ≤ 1200 bytes. Frames are split into packets with a 22-byte header (codec, flags,
  frame number, index/count, capture and encode times) and are **all-or-nothing** on the viewer.
- The host encodes only the newest captured frame once the previous one has been sent, and a
  token bucket paces the packets to the cap (`--max-bitrate`, default 20 Mb/s, burst 50 ms).
- A frame that never completes makes the viewer request a keyframe (honoured at most every
  500 ms) and keep showing the last good frame meanwhile. This is the tile codec's stand-in for
  reference invalidation (§5.6).
- Once a second the viewer reports datagrams received, datagrams lost (sequence gaps, before
  any repair) and frames lost.

**Considered and set aside:** QUIC with datagrams would give the same split on one port with one
handshake, and remains an option. We chose the plain UDP path because it keeps all of M1
unchanged and reuses the TLS handshake we already trust for the keys. A fully custom handshake
was rejected: that is where security mistakes happen.

## 5. Video

See [`video-pipeline.d2`](video-pipeline.d2).

### 5.1 Capture
- Primary: **DXGI Desktop Duplication (DDA)**, with a fallback chain (Windows.Graphics.Capture,
  then GDI). AnyDesk: `capture.fallback_capture - Selecting DDA capture.`
- Use the dirty rectangles that DDA reports, and keep frames on the GPU until encoding.
- The cursor is **not** in the captured image (DDA leaves the pointer out), so moving the mouse
  over a still screen produces no video frames at all.

**Implemented (v0.5.5–v0.5.7)**, `illum_win::ScreenCapture`: DDA first, **GDI** when DDA is not
available, the way RustDesk does it (`libs/scrap/src/dxgi/mod.rs`: "Fallback to GDI" when the
device or `DuplicateOutput` fails). The case that forced this, seen on the laptop
(2026-10-28): on a two-GPU laptop Windows may bind a process to the discrete GPU and then
DXGI *reparents* the panel onto that adapter; `DuplicateOutput` from it answers
`DXGI_ERROR_UNSUPPORTED`, and the integrated GPU, which really drives the panel, lists no
outputs at all inside that process. The DDA code tries every adapter that lists the display
(enough when both list it); when all refuse, capture switches to GDI: `BitBlt` into a top-down
32-bit DIB at most ~60 times a second, identical frames dropped by comparison, cursor from
`GetCursorInfo` / `GetIconInfo`. Slower (a CPU copy of the screen per frame) and blind to the
secure desktop, but it streams. Not chosen: asking the user to set a per-app GPU preference in
Windows settings (AnyDesk and RustDesk need no such entry), and Sunshine's approach of patching
the GPU-preference lookup inside its own process (undocumented, fragile).
**Windows.Graphics.Capture** stays the intended middle step of the chain: it is independent of
GPU binding and works from a service; it comes with the capture work of M7.

### 5.2 One pixel format
- Internally every frame is **BGRA8**, the format Windows calls `X8R8G8B8`. AnyDesk logs
  `Format changed to 1920x1080 (X8R8G8B8)`.
- All colour conversions (to YUV for the encoder, and back on the controller) live in one module.
  Colour space, range and channel order are always stated explicitly.
- HDR and wide-gamut desktops are tone-mapped to SDR on purpose.
- CI test: a colour test pattern goes through capture → encode → decode → present and is compared
  with a reference image. A red↔blue swap or a chroma shift fails the build.

### 5.3 Codec
**Decided 2026-10-20:** the stream uses a hardware video codec, chosen **per direction** from
what the two machines can do:

> **AV1 if both sides can use it in hardware, otherwise H.265 (HEVC).**

- Each side lists its hardware encoders and decoders in the hello. The host encodes with the
  best codec the viewer can decode.
- AV1 is royalty-free and gives the best picture per bit. H.265 is the fallback every recent GPU
  handles in hardware; using the GPU's built-in encoder and decoder carries no licensing cost for
  us. H.264 is left out for now.
- On the reference setup: this PC (RTX 5070 Ti) → laptop (RTX 3050 Ti, which decodes but can't
  encode AV1) uses **AV1**; laptop → this PC uses **H.265**.
- Decoding may be done in software where that avoids trouble (for example dav1d for AV1 on a
  laptop whose windows run on the integrated GPU); the choice is an implementation detail.

**Chroma: 4:2:0 in v1** (decided 2026-10-22). Hardware AV1 encoders are 4:2:0 only, and H.265 is
used at 4:2:0 as well so both directions behave the same. 4:2:0 stores colour at a quarter of the
resolution (one colour value per 2×2 pixels; brightness stays per pixel), which is invisible in
video and games but softens coloured edges on text, such as ClearType fringes or red text on
blue. A **4:4:4 mode** (H.265 on GPUs that support it, roughly 1.5–2× the bitrate at the same
quality) is **future work**: once the 4:2:0 path is stable, try it from the laptop on a page of
small text and decide whether it becomes a ladder rung or a user preference.

**Library: FFmpeg** (decided 2026-10-22). One API for NVENC, AMF and QSV on Windows, and for
VideoToolbox and VAAPI when other platforms come; it also wraps dav1d for software AV1 decoding.
The cost is on our side: a trimmed FFmpeg build (hardware codecs, Opus, dav1d; roughly 10–30 MB)
bundled with the installer, Rust bindings (`ffmpeg-next`) that need the libraries at compile
time, and a heavier CI. Media Foundation (Windows only, nothing to ship) and the NVENC SDK (NVIDIA
only) were the alternatives; writing one of them now and FFmpeg later would mean two
implementations of the same thing. The encoder and decoder still sit behind a small interface in
our code.

**Two codecs we looked at and why they aren't the base:** VP9 (NVIDIA has never encoded it in
hardware) and lossless tiles for motion (far too much data; see §4.4).

**The lossless tile codec from v0.3.0** (`illum_media::tiles`: 64×64 tiles, LZ4, bit-exact) is
the fallback when the two sides share no lossy codec, and the codec of the bit-exact tests.
Whether it keeps any other role is an open question (§13). Ideas that were discussed and **not**
adopted, so nobody re-proposes them by accident: switching between tiles and video per frame,
"refinement" passes after motion, ordering or protecting parts of a frame by a neighbourhood
score, and sending the smallest parts of a frame first. A high-quality still frame after the
screen stops changing is parked as a future idea.

**Implemented in v0.5.1** (`illum-ffmpeg`, bindings `ffmpeg-the-third`, prebuilt FFmpeg from
BtbN fetched by `scripts/get-ffmpeg.ps1`):
- At start the host **probes** FFmpeg's encoders by opening each once at a small size, so a
  hardware encoder for a GPU that isn't there drops out; candidates in order: `av1_nvenc`,
  `av1_amf`, `av1_qsv`; `hevc_nvenc`, `hevc_amf`, `hevc_qsv`, `libx265`. Software AV1
  (`libsvtav1`) is deliberately **not** a candidate (decided 2026-10-28): the rule is AV1 in
  hardware or else H.265, and real-time 1080p60 AV1 on a CPU would show up as encode delay;
  `libx265` stays only as the last resort for a machine with no GPU encoder. Software decoders:
  `libdav1d` for AV1, FFmpeg's `hevc`. The result becomes hello capabilities `enc:av1`,
  `enc:hevc`, `dec:av1`, `dec:hevc`; the host picks per the rule above (`choose_codec`).
  `illum codecs` prints the probe with FFmpeg's reasons for each encoder that would not open.
  **A viewer offers `dec:av1` only when it decodes AV1 in hardware** (v0.7.2): from v0.5.1 to
  v0.7.1 the software decoder counted, the laptop received AV1 and spent ~20 ms a frame on it —
  half the lag of the first live runs. The user's **hardware decode** setting (default on;
  `--software-decode`) turns the GPU decoders off, which also removes AV1 from the offer. NVENC
  AV1 is encoded in 4 tile columns so a software decoder can use several cores without a frame
  of delay.
- **GPU decoding (v0.8.0)** goes through FFmpeg's hardware paths, the same code on every OS:
  the decoder is FFmpeg's native `av1` / `hevc` with a hardware device attached (`D3D11VA`,
  then `DXVA2`, on Windows; `VAAPI`, `CUDA`, `VDPAU` on Linux; `VideoToolbox` on macOS — one
  list, the only place the OS shows), `get_format` accepts only the device's surface format
  (a GPU that cannot decode the stream fails where we can see it instead of quietly decoding on
  the CPU), and each surface is copied down once as NV12 (`av_hwframe_transfer_data`). On
  two-GPU machines the second and third adapter get a try. **What the GPU can decode** is found
  by decoding one embedded 256×144 keyframe per codec (`crates/illum-ffmpeg/probe/`) through
  that path — the one check that gives the same answer on every vendor and OS; it takes
  ~0.3 s, runs once, and only on the viewer side. `illum codecs` lists the result as
  "Decoders (GPU)". If the probed decoder will not open for a session after all, the viewer
  falls back to the software decoder and logs it.
- Encoder settings are all about delay: no B-frames, no lookahead, one thread, a 100 ms
  rate-control buffer, `LOW_DELAY`, keyframes only on request (`forced-idr`), NVENC preset p4 /
  tune ll / CBR. The encoder gets 85 % of the bitrate cap; packet headers and parity take the
  rest. It is opened at the first frame's size and reopened on a resolution change.
- Decoders run with slice threading only (frame threading would hold pictures back) and
  dav1d's `max_frame_delay=1`, so every packet yields its picture at once. A decoder hands the
  window its **own planes** (`Picture::Yuv`: I420 from the software decoders, NV12 from the
  GPU, with the stream's matrix and range; the tile codec gives `Picture::Bgra`); nothing on
  the viewer's CPU rewrites pixels (v0.8.0; before that swscale converted every frame to BGRA).
  The encoder declares what its BGRA → 4:2:0 conversion uses, BT.601 limited range, so the
  viewer converts back the same way (`frame.color_space()` / `color_range()` on the way out).
- The packet header names the codec per frame, so the viewer opens the matching decoder when
  the first frame arrives and would follow a mid-session fallback to tiles (an encoder that
  fails to open or to encode is replaced by tiles rather than ending the session).
- **FFmpeg 8.1, not 9.0:** 9.0's NVENC demands NVIDIA driver 610+ (API 13.1) and silently falls
  back to software on anything older; 8.1 needs 570+.
- A reference-invalidation request (§5.6) is served with a **keyframe** for now: FFmpeg exposes
  no "predict from frame N" control for NVENC/AMF/QSV; that needs the vendor SDKs (§13).

### 5.4 Presentation and the cursor
- The cursor shape and position are sent **separately** from the video, so the cursor never
  waits for a video frame. AnyDesk: `desk_rt.system_event_source - Enabled mouse cursor`.
- **Two pointer modes** while controlling (decided 2026-11-03, v0.11.0; `--pointer`):
  - **Native** (default): the controller's **own OS pointer** stays visible and **wears the
    host's cursor shape** (a custom cursor made from the host's bitmap and hotspot, through the
    window library, so the same code serves Windows, macOS, X11 and Wayland). The OS moves it
    on its cursor plane at input time, so it never waits for our draw or the swap chain, never
    stutters with the frame rate, and never disappears over the dark window or the letterbox
    bars. When the host hides its own pointer (typing, a full-screen video) ours shows as the
    plain arrow: the user never loses the mouse. This is what RDP, AnyDesk, RustDesk, Parsec
    and Chrome Remote Desktop do.
  - **Embedded**: the host's cursor **drawn inside the picture** at the host's size, scaled
    with it, at the controller's own mouse position (never at the host-reported one, which
    arrives once per captured frame and would stutter): the picture looks as a screenshot of
    the host would, pointer included. This was the only mode until v0.11.0 and is what Moonlight
    does; it inherits the picture's latency (event loop, draw, one or two refreshes of swap
    chain) and vanishes wherever nothing is drawn, which felt like VMware without its tools
    or a full-screen game. Kept as a choice, and used for **view-only** in both modes, where the
    pointer belongs to the host and is drawn where the host reports it.
  - Later: a marker for the host's own pointer while controlling (someone sitting at the host,
    an app warping it), and a **relative mouse mode** for games and other mouse-capturing apps
    (local pointer captured and hidden, deltas on the wire), which is the real reason Moonlight
    paints the pointer: such apps read raw motion and have no pointer position at all.
- **No added video delay.** Each frame is shown as soon as it is decoded; a newer frame replaces
  an older one that hasn't been shown yet. Moonlight does the same (its pacer keeps at most one
  queued frame). Audio is the exception (§8).
- The controller decodes in hardware and presents through a flip-model swap chain, not through a
  UI toolkit's image widget.
  - **GPU presentation (v0.8.0, `illum-ui::gpu`):** `wgpu` on top of the `winit` window, so
    the same code runs on Direct3D 12, Vulkan and Metal. The decoder's planes are uploaded as
    they are (R8 / RG8 / BGRA8 textures, any row stride) and **one shader** converts
    (BT.601 or BT.709, limited or full range, from the picture's own description), scales
    with bilinear filtering and blends the cursor with its alpha. The swap chain asks for the
    **newest-frame-wins** mode (`Mailbox`, else `Immediate`, else `Fifo`) with a frame latency
    of 1, so presenting never blocks the event loop and a late frame never queues behind an
    older one. The picture is uploaded once per decoded frame; redraws for a moved mouse reuse
    it. Offscreen tests render the colour pattern from I420 and NV12 planes in both matrices
    and ranges and read the pixels back.
  - **CPU fallback:** `softbuffer` with the v0.3.0 blit stays for a machine with no usable
    GPU API (and behind `--software-render`); it uses the same YUV → RGB arithmetic on the
    CPU (`YuvPicture::to_bgra`), so both paths show the same colours.
  - **View modes (v0.4.2):**
    - **Fit:** 1:1 when the window is big enough, otherwise scaled down. On the CPU each
      window pixel **averages** the host pixels it covers; on the GPU bilinear filtering does
      the same down to half size (a box or mip filter for smaller windows is a follow-up).
      Nearest neighbour, used until v0.4.1, dropped pixel rows and columns and broke thin text
      strokes.
    - **Original:** 1:1, and the visible part follows the mouse when the host screen is larger
      than the window.
    - **Ctrl+Alt+Shift+F2** switches between them.
  - On the CPU path BGRA is translated to the window's `0x00RRGGBB` in exactly one function
    (`illum_media::scale::bgra_to_0rgb`), and a test pins the channel order.
  - Frame pacing to the display refresh (Moonlight's optional feature) is a v2 idea.
- **Pausing while minimized (v0.11.6).** When the viewer window is minimized it sends
  `ScreenPause { paused: true }`; the host stops capturing, encoding and sending the **screen**
  only, and resumes with a keyframe on `ScreenPause { paused: false }` when the window is
  restored. The TLS connection, the input channel and the **audio stream keep going** — they
  cost almost nothing, so the remote's sound still plays with the window down. Nothing
  reconnects; it only looks like a quick reconnect because the resume is a keyframe. This is
  what RDP does by default (it suppresses graphics when minimized) and what the industry
  patents describe. The trigger is a real minimize (a zero-sized window), not merely being
  covered by another window. The surface-size fix of v0.11.5 and this share the aim of a window
  that is never streamed to when nobody is watching it.
- **Telling a static host from a dead path (v0.11.6).** The viewer used to cry "no video is
  arriving; re-establishing" whenever video stopped — but a host whose screen is not changing
  sends nothing, which looks the same as a dead path. The host already reports, in `HostStats`,
  how much it sent each second; the viewer now only says **"video stalled; reconnecting…"** when
  the host says it *did* send and nothing arrived. A quiet host stays quiet in the title.

### 5.5 Adaptation: the latency controller
See [`adaptation.d2`](adaptation.d2). Decided 2026-10-22. The design is ours; the comparison with
AnyDesk, RustDesk and Moonlight is in the lessons doc, §5.

**The user sets a latency limit L** (default **50 ms**, from capture on the host to shown on the
viewer). The controller's job is to keep frames within L using as much quality as the link allows.
It starts at the **top** and steps **down**.

**Measure, per second, from frames that were shown:**
- share of frames whose latency was within L
- number of frames shown

Lost frames are **not** counted here; they belong to the FEC controller (§5.6).

**Grade each second:**

| Share of shown frames within L | Grade |
|---|---|
| ≥ 90% | 🟢 may raise |
| 50–90% | 🟡 hold |
| < 50% | 🔴 may drop |
| fewer than **N** frames shown | ⚪ no evidence: never raises, never drops |

N = one quarter of the current target frame rate, minimum 5. A still screen produces ⚪ seconds,
so it never changes anything.

**Act only when a grade persists.** Counting starts at the first 🔴 (or 🟢) second:
- **Drop one rung** if, within **10 s**, at least **30%** of the seconds were 🔴.
- **Raise one rung** if, within **15 s**, at least **80%** of the seconds were 🟢 (⚪ seconds
  don't count), and loss is not above the FEC threshold.
- After any action the count starts fresh.
- One rung per action, never several at once: a one-second Wi-Fi accident must not cost a drop
  that then takes a full raise window to undo. (The plan had no emergency path; the measurement
  runs added one for congestion only, rule 7 below.)

**The ladder.** A rung is a triple (resolution, frame rate, bitrate cap). Rungs are generated in
the order **bitrate → frame rate → resolution**, because bitrate and frame rate are free to change
while a resolution change costs a keyframe, and because for screen content readable text matters
more than motion (WebRTC's screen-sharing default and RustDesk agree on bitrate before frame
rate).
1. From the cap, each rung multiplies bitrate by **0.8** until the bitrate floor for that
   resolution (about 3 Mb/s at 1080p, 1.5 at 720p).
2. Then frame rate: 60 → 45 → 30 → 20 → **15**.
3. Then resolution drops one level, and the bitrate and frame-rate rungs repeat there.

**Top rung:** native resolution, the user's frame-rate cap (default 30 since v0.10.0), and a
bitrate cap (default 20 Mb/s). The cap is a ceiling for the encoder's rate control; a still
desktop uses far less.
**Bottom rung:** 15 fps, about 1 Mb/s; half the native resolution only if the user allows
resolution rungs.

**If L is below what the link can do** (half the round trip plus encode and decode is already
higher), the controller uses that floor plus a margin and the app shows "best possible on this
link: X ms" instead of stepping down forever.

**Why not the others' designs:** AnyDesk estimates once, about 0.7 s into the session, picks a
band and (as far as its log shows) never moves again. Moonlight never adapts and tells the user to
lower the bitrate. RustDesk adapts on queue delay above a 150 ms threshold and starts low. Ours
works in the user's own terms (a latency number), keeps adapting, and starts high.

**Implemented in v0.6.0** (`illum_media::adapt`; the host runs it, the viewer measures). The
viewer counts, per second, frames presented and frames presented within L (its clock estimate
against the capture time in the packet header) and reports both with the loss counts; the host
grades and decides, applies the rung to the pacer, the encoder (bitrate live on hardware
encoders; frame rate and resolution by reopening, which costs a keyframe) and the packetiser's
parity, and tells the viewer (`Quality`, shown in the title). The ladder as built: five bitrate
rungs (×0.8 each, so down to 0.41 × cap), then the frame-rate rungs below the user's cap
(60/45/30/20/15) with bitrate scaled with the frame rate, then, only if the user allows it,
75 % and 50 % of the native size, never below 1 Mb/s — 7 rungs from a 20 Mb/s, 30 fps cap
(11 at 60 fps with resolution rungs, the v0.6.0 shape). The resolution floor per bitrate of the
plan above was dropped in favour of this fixed shape; the harness can revisit it.

Three rules were **added from the harness** (§5.7) on 2026-10-30, each because a scenario failed
without it:
1. **A freeze caused by loss is 🔴, not ⚪.** When fewer than N frames are shown *and* at least
   **2** frames were lost, the second is 🔴. Without this an oversubscribed path (queue
   overflow drops 50–80 % of packets, nearly no frame completes) read as "still screen" and the
   latency controller never moved; the only way down was the FEC backstop, 150 s to the ceiling
   plus 30 s per rung. A still screen with one lost frame is still ⚪. Lost frames still do not
   count as late frames; the FEC controller is unchanged. (The threshold was N ÷ 2 until the
   first Wi-Fi log, v0.7.1: during a real stall only the two keyframe attempts per second die,
   the small frames in between arrive, so dozens of lost frames never happen.)

**The clock behind "within L"** (v0.7.1): the viewer's estimate of the host clock comes from
the fastest probe of the **last minute**, not of the whole session. Two machines' clocks drift
apart by 10–50 µs per second; a two-hour session with a one-shot offset read "latency −37 ms",
graded everything 🟢 and never moved the ladder. Within a minute the drift stays under 3 ms,
and the asymmetry of a 10 ms round trip bounds the rest.
2. **A failed raise backs off.** A raise undone by a drop within 60 s counts as a failure of that
   rung; each failure doubles the green run it needs before the next try (15 → 30 → 60 → 120 s),
   and a raise that holds 60 s clears the count. Without this the controller hunted for ever
   between the rung that fits and the one just above it (10.24 vs 8.19 Mb/s on a 10 Mb/s path).
3. **The backstop needs frames still dying**, not merely loss above the threshold: with parity
   at the ceiling repairing every frame, persistent random loss is path loss, fully handled, and
   walking the ladder down would only lower the picture for nothing. **And it needs the same
   evidence as every other drop** (2026-11-03): the round trip risen for at least half the
   window (a standing queue, the mark of overload), and, for a further rung, the loss at most
   half of what it was when the backstop last fired (an overload of 2 % loss vanishes after a
   20 % cut). Bursty Wi-Fi loss of 2–3 % kills the odd frame through 30 % parity with the
   round trip flat between the bursts; the backstop took a live session from 10.2 Mb/s to
   4.1 Mb/s and 15 fps for it, one rung per window, with the loss unchanged at every rung.
   Loss that does not answer to the rate is held at the parity ceiling and logged as such.

Two more came from the first long live run (2026-10-30, viewer window hidden for an hour):
4. **Evidence must be recent.** The 10 graded seconds of a drop must lie within the last 30 s,
   the graded seconds of a raise within four times the raise window. A hidden window grades
   almost nothing, and the laptop's Wi-Fi hiccups for 3 s every five minutes; those 🔴 seconds
   added up to "3 of the last 10" across twenty minutes and cost a rung per quarter hour.
   And the second that completes the count must be bad itself (2026-11-03): four 🔴 seconds
   followed by six 🟢 ones are a path that has recovered, and the rule was dropping a rung
   into six seconds of green evidence five seconds after a loss storm ended.
5. **"Frozen by loss" counts frames *decoded*, not shown.** A minimised window (or one whose
   display is off) decodes every frame and presents none: that is ⚪, no evidence about the
   link, whatever was lost meanwhile. The viewer reports `decoded` next to `shown`.

Four more came from the measurement runs of 2026-11-02 (`docs/testing/results-2026-11-02.md`),
built in v0.10.0:
6. **Congestion is loss *with a rising round trip*; random loss is loss with a flat one.** The
   viewer probes four times a second and reports two numbers: the **75th percentile** of the
   round trips of the last 3 s (a path above its capacity delays most probes; our own bursts
   and keyframes, which delay single probes by tens of ms, do not move it — and neither the
   minimum, tried first: on Wi-Fi it stayed at the floor through 70 s of 12 % loss because some
   probes always slipped through, 2026-11-03) and the smallest of the last minute, the floor, which
   the probes before the stream started already set, so a session that starts above the link
   still knows the path's clean round trip. "Risen" means at least 1.5× the floor and 10 ms
   above it. A rung above the link showed loss of 2–12 % with the round trip going
   12 → 35 ms inside the first second; Wi-Fi loss at a rung that fitted never moved it.
   Rule 1 therefore needs the rise: lost frames with flat timing are ⚪ (parity's business),
   not 🔴. Loss-only 🔴 seconds had walked the ladder down for nothing. (The harness caught
   both refinements: with the last probe instead of the 3 s minimum, keyframe bursts on a
   fast link read as congestion; without the pre-stream floor, a session starting above the
   link never saw a rise.)
7. **The fast drop.** A second with loss ≥ 2 % and a risen round trip drops one rung at once,
   at most one per second, before the window rules. This is the emergency path the plan left
   out; the data made it unambiguous. Starting high stays the default because of it: three
   rungs took thirty seconds through the 10 s window, three seconds through this.
   **It repeats only while it is working** (2026-11-03): a probe lost on the path comes back
   by TCP retransmission, so during a loss storm the round trip reads as risen too, and
   unbounded, a 15 s storm walked the ladder to the bottom in seven seconds (harness). The
   tell is what gets through when the host sends less. A saturated link keeps delivering the
   same amount, so the count of packets received holds up; a storm delivers a fixed share, so
   the count falls with the rate. Within 10 s of a fast drop the next one needs the received
   count to have held up (at least half-way between "fell with the rate" and "unchanged"; a
   blackout delivers nothing either way and proves nothing). The loss ratio is not the test:
   the first second of an overload fills the path's queue, so its loss reads low. With this a
   15 s storm costs three rungs (one fast drop per 10 s plus the "frozen" 🔴 window rule) and
   the top is back within 90 s, while a 20 Mb/s start still gets under a 12 Mb/s link in
   three seconds.
8. **Raises are gated on loss over the last 30 s**, the FEC controller's window, not 10 s; a
   backstop drop was being undone by a raise 15 s later.
9. **Keyframes at most two per second** however many frames die (was four); four a second fed
   the collapse above the link, and one a second, tried first, left the harness's bursty link
   showing under half its frames. With that, the viewer says what it is waiting for: a
   keyframe unanswered for 2 s, or no media at all for 3 s with the control channel alive (then
   it punches fast again and asks for a keyframe), both as a notice in the title.

The ladder's **resolution rungs are off unless the user allows them** (§5.8): the host scales on
the CPU, which measured as 10–15 ms of latency and a frame-rate cap of 45–53. The frame-rate
rungs are those below the user's cap, which defaults to 30 (60/45/30/20/15 are all available
under a 120 cap).

Two facts about the pacer came out of the same runs and apply to the host: the encoder's share
is `rung × 100 / (100 + parity %) × 90 %` (parity truly budget-neutral, 10 % left for headers
and for the bucket to refill), and the burst allowance is at least **two frames** (was 50 ms),
otherwise a frame at the low frame-rate rungs is spread over its own interval by our own pacer
and can never be within a 50 ms limit.

### 5.6 Loss: parity FEC, never resend
Pixels are never resent (§2). Loss is handled in two layers.

**Parity over the whole frame.** Each frame's packets get **m** extra parity packets computed from
them (Reed–Solomon). Any **m** of the frame's packets can be lost and the viewer rebuilds them,
with no waiting and no round trip. A parity packet is the same size as a data packet, so m ÷ k
is the overhead. Facts that shaped this:
- A frame is **all-or-nothing** to a hardware decoder, so every packet is equally necessary.
  Protecting "important" packets by duplicating them, or scoring parts of a frame, buys almost
  nothing; parity at the same cost protects any packet. Moonlight drops incomplete frames too.
- Overhead is budgeted in **bytes**, not percent: a 2-packet frame with "10%" rounds to one parity
  packet, which is 50% of that frame but one small packet in absolute terms.

**The FEC controller** is separate from the latency controller: availability and latency are
different goals. It measures **packet loss before repair** (gaps in sequence numbers, so FEC doing
its job doesn't hide the loss) over **30 s windows**:

| Loss before repair | Zone | Action |
|---|---|---|
| ≥ 0.5% | 🔴 | step up **+5** points, up to the ceiling |
| 0.1–0.5% | 🟡 | hold |
| < 0.1% | 🟢 | after **two** consecutive 🟢 windows, step down **−5**, then −5 per further 🟢 window, down to the floor |

Defaults: **floor 5%, ceiling 30%**, both user-changeable. Fast up, slow down.

**How the two controllers coexist** (three rules, nothing more):
1. **FEC is budget-neutral.** Its bytes come out of the video bitrate target, never on top. So
   stepping FEC up cannot add latency; the picture gets slightly softer instead.
2. **FEC never waits for the latency controller.** Availability comes first: a frame that arrives
   late beats one that never arrives. The time horizons already keep them apart: the latency
   controller fixes overload within about 10 s; if loss is still there after FEC's 30 s window,
   it is path loss and parity is the right fix. The latency controller never raises while loss is
   above the FEC threshold.
3. **Backstop.** If FEC sits at the ceiling, loss persists for one more window with frames still
   dying, **and the round trip was risen for most of that window**, the loss is overload that
   never showed up as late frames enough to move the ladder. FEC then asks the latency
   controller for one rung down, and again each window while the loss keeps answering (each
   further rung needs the window's loss at most half of the last backstop's). Without the risen
   round trip, or when a rung down changed nothing, the loss is the path's: parity stays at the
   ceiling and the rung holds (§5.5 rule 3).

**Reference invalidation** is the second layer, for the frame that FEC could not save. The
viewer reports "frame N broken"; the host encodes its *next* frame as a difference from an older
frame the viewer does have. No old data is resent, the viewer never waits (it keeps showing the
last good frame), and the picture heals within one round trip. AV1 and H.265 both support this,
and it is what Sunshine does. **Extra parity for keyframes** (v0.7.1): a keyframe's blocks get at
least 30 % parity whatever the controller set, and a request is honoured at most every 500 ms (two keyframes a second; 250 ms until
v0.10.0, §5.5 rule 9). A
keyframe is ~180 packets and the one frame that must arrive; at 5–10 % parity on a Wi-Fi burst
it mostly died, the retry died too, and the picture stalled for seconds (the first live run).

**A keyframe request is always answerable (v0.11.7).** Keyframe-on-loss stands in for reference
invalidation until the vendor SDKs are worth it, but it had a hole: the capture emits a frame
only when the screen changes, so a keyframe asked for while the host sat **static** had no frame
to attach to and was never sent. A viewer that lost its reference during a loss spell then waited
for ever — the video died permanently while input (on TCP) kept working (2026-11-09). The host
now **re-sends the last frame as a keyframe**, dated now, when one is owed and nothing new has
been captured. As a backstop the viewer **rebuilds the whole media path** (it re-sends
`ScreenStart`, and the control loop reconnects the socket) after eight seconds of a real stall —
the host reports in `HostStats` that it is sending, yet nothing arrives, which is a stuck path
(a route flip, or keyframes lost for good), not something a keyframe request can cure.

**A lost reference is inferred from empty decodes (v0.11.8).** When a frame is lost, every
delta after it references the gap; the decoder cannot build a picture and FFmpeg prints
"Could not find ref with POC …". But it reports that **the same way as a normally buffered
frame** — a packet accepted, no frame out — so the reassembler being happy (the delta's own
packets all arrived) is not enough to notice. Our encoders are one-in-one-out, so a **run of
empty decodes** (five, which still allows for a decoder that buffers a frame or two when it
opens) means the reference is gone. The viewer then asks for a keyframe, the only thing that
re-anchors the decoder, instead of feeding it more undecodable deltas. This catches a reference
loss the reassembler did not flag (a frame whose every packet was dropped leaves no partial to
expire), whatever its cause.

**Implemented in v0.5.1** (`illum_media::fec`, `illum_media::packets`): Reed–Solomon over
GF(2⁸) (`reed-solomon-erasure`). A frame's packets are grouped into **blocks of at most 128
data packets**, each with its own parity (the field allows 255 shards per block; a 2 MB tile
keyframe is ~1800 packets, so one block per frame was not an option). Parity per block is
`ceil(k × percent / 100)`, at least 1 when the percentage is non-zero, so a 2-packet frame at 5 %
still gets one parity packet. Data packets stay variable-length on the wire; parity is computed
over the padded, length-prefixed form, whose length the header carries. The packet header (30
bytes) names codec, keyframe flag, frame sequence, block index and count, shard index, k, m,
shard length, capture time and encode time, so any single packet identifies its frame and block.
The viewer rebuilds a block as soon as any k of its k + m shards are in and reports frames it
had to repair (`MediaReport.repaired`, "N repaired" in the title). Parity is fixed at the
**5 % floor**; the controller above is M5. Reference invalidation is a keyframe for now (§5.3).

### 5.7 Testing the numbers
Every number above is a starting value. We control the UDP path, so the test suite inserts a
**simulated link** between host and viewer, bandwidth cap, delay, jitter, random and burst loss,
scripted over time, and runs fixed scenarios in CI:

| Scenario | Scored by |
|---|---|
| steady link at X Mb/s | utilisation (bitrate reached vs. available); flaps per minute at steady state (≈ 0) |
| bandwidth suddenly halves | recovery time until 🟢/🟡 |
| 1–2 s outage, then normal | false drops (must be none) |
| slow degradation over a minute | share of frames over L |
| random loss 1 / 3 / 10 %, burst loss | frames lost after repair (→ 0); FEC bytes spent; time to converge |

**On the real link (v0.9.0):** the same discipline without the model. `illum view --fixed`
asks the host for one rung and one parity for the whole session (no controller moves), the
host shows `illum workload` (still / fine detail / full motion, 20 s each on the wall clock),
`--report` keeps every second and every per-frame latency, decode and draw sample, and
`illum report` makes one table per run, grouped by phase. A plan (`scripts/measure-plan.csv`,
run by `scripts/measure.ps1`) sweeps bitrate, codec, decode and render path, frame rate,
parity and scale; the runs in both directions are what the tunables below are decided from.
See `docs/testing/measure.md`.

One parameter changes per run; everything else stays fixed. The tunables, with their defaults:

| Parameter | Default |
|---|---|
| 🟢 / 🔴 thresholds | 90% / 50% |
| drop window / share | 10 s / 30% |
| raise window / share | 15 s / 80% |
| rung ratio | ×0.8 |
| bitrate floor per resolution | 3 Mb/s @1080p, 1.5 @720p |
| N | ¼ of target fps, min 5 |
| FEC thresholds | 0.5% / 0.1% |
| FEC step | +5 / −5 |
| FEC window | 30 s |
| FEC decay wait | 2 clean windows |
| FEC backstop wait | 1 window at ceiling |

**Implemented in v0.6.0** (`illum_media::sim`, `crates/illum-media/tests/harness.rs`). A model
of host, path and viewer runs the real `Adapter` in model time: frames of bitrate ÷ fps bytes
(a keyframe about 100 ms of bitrate), packetised with the current parity and paced by the same
token bucket as the host; a FIFO path with service rate, base delay, a queue limit after which
packets drop, random per-packet loss and scripted outages; a viewer that completes frames from
k of k + m, waits for keyframes after loss exactly like the real one, and reports once a second.
Six scenarios, each printing its table and asserting the promised behaviour: clean LAN (no
move, 9 ms), Wi-Fi at 10 Mb/s with a 20 Mb/s top (under the link within 90 s, ≤ 2 flaps once
settled), 20 → 4 Mb/s and back (recovers, climbs back), 2 % random loss (parity up, ladder
still), 100 ms outages every 4 s (picture keeps moving), a 6 Mb/s cap (never exceeded). Run with
`cargo test -p illum-media --test harness -- --nocapture` to see the tables. Scores: utilisation
over the last 60 s, flaps in that window, recovery time per event (first of five 🟢 seconds),
false drops (a drop with ≥ 40 % headroom and no loss), seconds over the limit, frames lost.
Numbers at v0.6.0 are in the roadmap's M5 status; the model's biggest simplification is that
it has no jitter and no encoder-size variance, so real links will be a little noisier.

### 5.8 User settings
Decided 2026-11-03 from the measurement runs (`docs/testing/results-2026-11-02.md`). Two
kinds: choices **fixed at connect**, which never move during a session (changing one restarts
the stream, a short blackout, not a new connection), and **limits for the adaptation**, inside
which the controllers move bitrate, frame rate and parity on their own. Everything else in
§5.5–5.6 is internal and not shown.

Fixed at connect:

| Setting | Default | Choices |
|---|---|---|
| Codec | auto: AV1 if both sides do it in hardware, else H.265 | AV1 / H.265 / lossless as overrides |
| Hardware decode (viewer) | on | off = CPU decoders only, which also removes AV1 from the offer |
| GPU for decode and display (viewer) | auto: the discrete GPU if there is one | auto + each GPU the machine lists (one entry on a Mac) |
| Encoder (host) | auto: the first that opens of NVENC, AMF, QSV, VideoToolbox, then CPU | auto + each GPU's encoder + CPU |
| Display, send audio, keyboard mode | as today | |

Limits for the adaptation:

| Setting | Default | Notes |
|---|---|---|
| **Latency limit** L | 50 ms | capture on the host → handed to the viewer's swap chain, decode included (not the display's scan-out, not the input path) |
| **Max bandwidth** | 20 Mb/s | the top of the ladder; on a Wi-Fi link the fast drop finds the ceiling within seconds |
| Max frame rate | 30, up to 120 | 30 costs 3–5 ms of latency against 60 and half the bits; the frame-rate rungs are those below the cap |
| Lower resolution under pressure | off | the host scales on the CPU today (10–15 ms, 45–53 fps cap); on once it scales on the GPU |

Not exposed: the parity range (the controller keeps it between 5 and 30 %; the runs showed 30 %
costing 23 % of the picture bits for nothing on a clean link), the grading thresholds, the
windows, the back-off, the keyframe policy.

The viewer's own, switchable at any time (nothing travels to the host):

| Setting | Default | Choices |
|---|---|---|
| **Pointer** (§5.4) | Native: your own pointer, wearing the host's cursor shape; it never lags behind the picture and never disappears | Embedded: the host's cursor drawn inside the picture, at the host's size, as a screenshot of the host would show it |
| **Reconnect** (§5.5, v0.11.9) | On: an unexpected drop keeps the window and re-dials the host until it answers, then resumes | Off (`--no-reconnect`): a drop ends the session |

The setting's help text says exactly that; the choice is about feel, and the words have to let
someone pick without reading this document.

Plus one message the app shows rather than asks: "best possible on this link: X ms" when L is out
of reach.

**Implemented** as viewer flags, sent to the host in `ScreenStart` (0 = host default, and the
host clamps: bitrate to its own ceiling, frame rate to 15–120): `--latency-limit 50`,
`--max-bitrate 20`, `--max-fps 30`, `--allow-scale` (v0.10.0), `--mute` (v0.7.0),
`--software-decode` (v0.7.2; the host side has `--no-audio`), `--software-render` (v0.8.0, a
debugging switch rather than a setting: draw on the CPU), `--pointer native|embedded`
(v0.11.0, viewer-side only). `--fec-min/--fec-max` remain for measurement runs only. The GPU and encoder choices are the UI milestone's; the "best possible
on this link" message is still to do.

## 6. Input

**Keyboard modes** (as AnyDesk; **Remote is the default** since v0.11.10):
- **Remote:** sends physical key positions (USB HID usages, see "Key codes on the wire") **and
  grabs the keyboard** (see "Keyboard grab" below): the host's layout and IME apply, and nothing
  acts on the controller. This is RustDesk's "Map" mode and the right default.
- **Local:** translates using the controller's layout and IME, sending the composed characters —
  for typing into a host that lacks your language's IME. The keyboard is **not** grabbed (the
  local IME must see the keys), so only the system keys below go to the host.
- **Auto:** Remote if both layouts match, otherwise Local. Like Local, it does not full-grab;
  pick Remote explicitly for the grab when the layouts differ but you want the host to handle them.

The host reports its layout at session start and whenever it changes. AnyDesk logs
`Keyboard layout changed to: zh_cl`.

**Text and IME:** finished IME text (for example Chinese) is sent as text and typed on the host as
Unicode (`SendInput` with `KEYEVENTF_UNICODE`).

**Correctness rules** (the "multi-enter" problem):
1. **Only the host generates key repeats.** Auto-repeat events from the controller are dropped.
2. **Every input event has a sequence number.** Duplicates are ignored.
3. **Release every held key and mouse button** on disconnect, a network stall (no input-channel
   traffic for longer than a threshold), or when the viewer loses focus.

**Implemented (v0.4.0):**
- **Host rules** (`crates/illum/src/input.rs`, unit-tested with a fake clock and sink):
  - repeat the most recent key after **500 ms**, every **40 ms**
  - pause repeating after **300 ms** of input silence
  - release everything after **1 s** of silence
  - ignore a second key-down for a held key and any old sequence number
  - release everything when the session ends
- **Heartbeats:** while keys or buttons are held, the controller sends a heartbeat every
  **200 ms**, so silence really means trouble.
- **Local mode and repeats:** the host can't repeat text, so in Local mode the controller's own
  repeats are passed on for character keys only.
- **IME:**
  - Local mode enables this machine's IME and sends its result as text.
  - Remote mode disables it, so the host's IME composes from key positions.
  - Keys pressed while composing are not sent.
- **Wire format:** `illum_core::input`, binary, on the input channel.
- **Key codes on the wire (v0.11.1, step 1 of the control review):** a key travels as its
  **USB HID usage** (`page << 16 | id`; `illum_core::keymap`), not as a PC Set-1 scancode as
  before. The scancode was the viewer's platform code (winit's), which only works while both
  ends are Windows; macOS has no Set-1 scancodes at all. The HID usage is nobody's operating
  system's: the W3C `code` names (which the window library reports on every OS), evdev codes,
  macOS key codes and Set-1 scancodes are all tabulated against it in Chromium's keycode
  converter, and Chrome Remote Desktop sends exactly this. The viewer maps the window library's
  physical key to the usage (`illum_ui::input::KEY_USAGES`); the host maps the usage to what its
  injector takes (`keymap::windows_scancode` today; evdev and macOS tables join with those
  hosts). A test on Windows checks our table against winit's own, key by key. Two Windows
  quirks live in the host table: Pause is E1 1D 45 on the wire, which `SendInput` cannot take as
  a scancode, so it goes as its virtual key; NumLock is reported as E0 45 in window messages (to
  tell it from Pause) but injected as plain 45. Keys with no usage (F25 and up, Fn) are not sent.
- **Capabilities:** the host advertises `input` only when it can inject, and the viewer then
  opens view-only.
- **Checked by hand (2026-10-19, laptop → this PC):** English and Chinese typing both correct.

**Mouse:** absolute coordinates mapped per monitor, using each monitor's DPI.
- *v0.4.0:* positions are in pixels of the shown display. The host is per-monitor DPI-aware and
  converts to normalised virtual-desktop coordinates for `SendInput`.
- The pointer the user sees is the OS's own while controlling (Native, §5.4), or the host's
  shape drawn at the local mouse position (Embedded); either way it never waits for the host.
- **Moves travel on the UDP path, newest wins (v0.11.2, step 2 of the control review).** The
  viewer sends at most one move per **4 ms** (the latest position; a 1000 Hz mouse is not a
  thousand events) on the media path's `Input` stream, with its own sequence numbers; the host
  drops a move overtaken by a newer one and never a key behind it (keys stay on TCP with the
  reliable numbering). Before the media path is up, moves go on TCP as before. **Buttons and
  wheels carry the pointer's position**, and the host moves there before pressing, so a click
  lands where the mouse was even when the move before it is still in flight or lost; the viewer
  also flushes a pending move before a click so the host's hover state matches. A press off the
  picture (the letterbox bars) is nobody's; a release there still lets go, at the last place the
  pointer was on the picture.

**Input round trip (v0.11.3, step 3 of the control review):** the host acknowledges every
input event as it injects it (`InputAck`, with the event's sequence number and which space it
came from; moves from the UDP path at most ten times a second), and the viewer's title shows
**"input N ms"**: the median round trip of the last second, or the last one known while nothing
new was sent. It is the input half of the picture's "latency": from the key or move leaving the
viewer to the host having injected it (the host's own injection latency is inside it; the
picture's way back is not). Alongside `rtt` (the clock probes' round trip) it shows at once
whether input delay is the network or the host.

**Special keys:**
- Ctrl+Alt+Del is sent by the service with `SendSAS`. The installer sets
  `SoftwareSASGeneration` so this is allowed.
- Reserved hotkeys use the `Ctrl+Alt+Shift+<key>` pattern, as AnyDesk does, and are never sent
  to the host.
  - **F2:** switch between fit and original size *(v0.4.2)*
  - **Del:** Ctrl+Alt+Del *(service milestone)*
  - **arrows:** switch display *(later)*
  - **Return:** fullscreen *(later)*
- **System keys (v0.4.0, fixed in v0.4.2):** a low-level keyboard hook on the controller sends
  Win, Apps, Alt+Tab, Alt+Esc, Ctrl+Esc and Alt+F4 to the host while the viewer has focus.
  Injected keys are ignored by the hook. The hook is registered with the program's module handle;
  without it Windows may refuse a global hook, which is why v0.4.0 kept Alt+Tab local.
- **Keyboard grab (v0.11.10, Remote mode).** The same hook, in Remote mode, captures **every**
  key while the viewer is focused — not only the system ones — forwards it as a physical key and
  swallows it locally, so the controller's own Windows and its IME never see the keystroke. This
  is how RustDesk, AnyDesk and RDP keep local Alt+Tab, local shortcuts and a local Chinese IME
  from fighting the remote session (the control review of 2026-11-11 found both: Alt+Tab acted
  locally, and a local IME doubled/stuck a letter). The host does everything: its layout and its
  IME compose. **Release is by unfocusing** the window (the mouse is never grabbed, so click
  another window); a dedicated release hotkey is a follow-up for fullscreen. **Ctrl+Alt+Del** is
  reserved by Windows and cannot be grabbed; it is sent from the host side (`SendSAS`, §6, later).
  The grab is Windows-only today (the hook lives in `illum-win`); the other OSes get their own
  when they land, and the wire (physical usages) is already the same everywhere.
- **The grab is built as RustDesk builds it (v0.11.11).** Read from its source (`start_grab_loop`
  in `src/keyboard.rs`, and `windows/grab.rs` of its `rdev` fork) after the v0.11.10 hook was
  found not to hold: the Windows key still opened the controller's own Start menu.
  - **The hook has a thread of its own** that only pumps its messages. Windows calls a low-level
    hook on the thread that installed it and silently removes one whose thread answers late;
    ours sat on the window's thread, which also sets up the GPU and draws. Once removed, keys
    reached the local system first and the host only through the window.
  - **A captured press is swallowed; a captured release still reaches the local system**, so a
    key that was down when the grab began does not stay stuck here. **CapsLock and NumLock**
    always reach the local system too (and the host), so the lock lights stay right.
  - **The local IME is not switched off.** No key press reaches it, so it has nothing to
    compose; its language switch and indicator are left as they are. (Without a grab — Remote
    mode where the hook could not be installed — the window still turns its IME off.)
  - The hook and the window can both report a key, and the hook reports this machine's
    auto-repeat as more presses: `InputOut::key` drops a press of a held key and a release of
    one that is not, so the host sees each once and does its own repeating.
  - The window sees no press under a grab, so the hook's side picks out the viewer's own
    `Ctrl+Alt+Shift+F2` (`GrabHotkeys`) and hands it to the window; it is not sent to the host.
  - **Only our own injection is left alone (v0.11.12).** The hook used to ignore every injected
    key, so a key that a remapper, an on-screen keyboard or a vendor hotkey tool re-injects on
    the controller was never grabbed. RustDesk marks what it injects (`dwExtraInfo`, its
    `ENIGO_INPUT_EXTRA_VALUE`) and skips only that; the Windows injector now marks its input the
    same way (`OWN_INPUT`) and the hook takes everything else.
  - **Checked against the real system:** two ignored tests in `illum-win` drive the installed
    hook with real key events and read back what Windows saw — a grabbed press never reaches it,
    and a grabbed Windows key (press swallowed, release passed) does not open the Start menu.
  - **Told, not guessed:** the viewer prints its version and what it does with the keyboard at
    start, and logs, each time focus leaves, how many key presses went to the host and how many
    stayed on this machine (counts only, never the keys). The host logs the viewer's version.
  - Differences kept: the grab follows window focus (RustDesk also needs the pointer inside the
    picture), and the lock keys are forwarded as keys (RustDesk sends lock *state* with each key).

## 7. Secure desktop (UAC, lock screen)

The backend follows desktop switches as AnyDesk's `desktop_switcher` does:
1. Desktop change detected.
2. **Suspend capture.**
3. Switch the capture and input thread to the new desktop.
4. **Restart capture.** DDA reports "access lost" on a switch, and that is expected, not an error.

The session stays connected throughout.

## 8. Clipboard, audio, files

- **Clipboard** (§8; eventually `--control`):
  - Text and files are separate formats with separate permissions. Text first.
  - **Implemented in v0.11.4 (step 4 of the control review), both ways, text only.** A dedicated
    thread owns the OS clipboard through the `arboard` crate — one code path for Windows, macOS,
    X11 and Wayland — on the `Clipboard` channel (`illum_core::clipboard::ClipboardMsg::Text`).
    Each side runs the watcher when **both** advertised the `clipboard` capability
    (`illum::clipboard::ClipboardSync`); the host side also has `--no-clipboard`, the viewer
    `--no-clipboard`.
  - **Change detection is a poll** (four times a second), not Windows'
    `AddClipboardFormatListener`: the host has no window to receive `WM_CLIPBOARDUPDATE`, and a
    poll is the one mechanism that is identical on every OS, which the cross-platform rule
    prefers over a Windows-only listener. The listener may return later as a per-OS optimisation.
  - **A content hash prevents echo loops**: the thread remembers the hash of what it last saw or
    set, so the value the peer just applied is not sent back, and the same copy is not sent
    twice. The content already on the clipboard at connect is seeded, not pushed.
  - A read that fails (another program holds the clipboard, or the content is not text such as an
    image) is skipped until the next poll; a failed set is logged and left for the next change.
  - Text over `MAX_TEXT_BYTES` (1 MiB) is not sent; the frame limit is 8 MiB and files are a
    later milestone.
- **Audio** (decided 2026-10-22, the Sunshine way):
  - WASAPI loopback capture, resampled to 48 kHz, **Opus at about 80 kb/s** (AnyDesk logs
    `Bitrate set to 81690 bits/s`).
  - **5 ms frames**, so that a parity group stays short.
  - Over the UDP path, in **fixed parity groups of 4 data + 2 parity packets** (a 20 ms group).
    Audio packets decode independently, so this is cheap and not adaptive.
  - A fixed **~40 ms jitter buffer** on the viewer: long enough to rebuild a lost packet from its
    group, short enough not to be noticed. Every real-time audio path has one; video has none.
  - Fixed bitrate, not part of the adaptive budget: 0.08 Mb/s next to 2–20 Mb/s of video isn't
    worth adapting. Subtracted from the budget once as a constant.
  - **"Send audio: off"** captures and sends nothing.
  - **Implemented in v0.7.0** as designed: `illum_win::LoopbackCapture` / `Playback` (WASAPI
    shared mode, asking the engine for 48 kHz stereo 16-bit via `AUTOCONVERTPCM` on both
    ends, so no resampler of our own); `illum_ffmpeg::opus` (`libopus`, `application=lowdelay`,
    constant ~80 kb/s, 5 ms frames); `illum_media::audio` (8-byte packet header, groups of
    4 + 2 through the same Reed–Solomon code as video, data packets sent as soon as encoded,
    parity after the fourth; a 40 ms jitter buffer that conceals a missing frame with 5 ms of
    silence, treats 100 ms of nothing as the host being quiet and re-primes when sound returns,
    and skips a backlog rather than playing it late). Audio has its own stream and sequence
    numbers on the UDP path, its own sender thread on the host (never behind the video pacer)
    and its own thread on the viewer. Host flag `--no-audio`, viewer flag `--mute`; the
    viewer's wish travels in `ScreenStart`. Audio is not yet subtracted from the video budget
    (0.1 Mb/s with parity; noted for M5's follow-ups) and there is no A/V sync by design.
- **Files:** a later milestone, on the lowest-priority channel of the TCP path.

## 9. Permissions and profiles

Each permission has **three states**, as AnyDesk does:
- **supported:** this build or OS can do it
- **allowed:** policy permits it
- **enabled:** it is on in this session

AnyDesk logs, for example, `Sending clipboard files (11) permissions (enabled allowed supported)`.

Initial permissions:

| Permission | Default |
|---|---|
| view screen | on |
| control keyboard and mouse | on |
| clipboard (text) | on |
| clipboard (files) | on |
| audio | on |
| Ctrl+Alt+Del | allowed, off |
| block host input | allowed, off |
| privacy mode | allowed, off |
| lock account on session end | allowed, off |
| file manager | later |

**Profiles:**
- **interactive:** the accept panel decides
- **unattended:** paired device, optional password stored as a salted hash, lockout after
  repeated failures

Settings are stored per paired device. The config format is **versioned** from day one (AnyDesk
logs `Migrating V0 -> V1`).

## 10. Session policy

- **One controller at a time.** Further connections are view-only, and the host always shows who
  is connected.
- **Wake lock:** during a session the backend holds display and system power requests. AnyDesk
  does this (seen in `powercfg /requests`).
- **Session end:** optional lock (`LockWorkStation`) and an idle disconnect timer.
- **Privacy mode:** a full-screen black window on the host, marked `WDA_EXCLUDEFROMCAPTURE`, plus
  `BlockInput`. The person at the host sees black; the controller still sees the desktop.

## 11. Diagnostics

- Every frame carries timestamps (capture, encode, send, receive, decode, present). The viewer can
  show where the time goes. *(v0.3.0: the title bar shows fps, bandwidth, latency, encode time
  and RTT; `-v` logs it once a second.)*
- The per-second grades, rung changes and FEC steps are logged, so a bad session can be read back
  second by second. *(v0.9.0: `view --report` writes the per-second records as JSON, with the
  host's own per-second `HostStats`; `illum report` tabulates a folder of them, §5.7.)*
- The viewer's title says what it is waiting for rather than freezing silently: a keyframe
  unanswered for 2 s, or no media for 3 s while the control channel is alive (v0.10.0, §5.5
  rule 9).
- Each role writes a trace log with a component prefix, as AnyDesk's `ad.trace` does.
- One-click diagnostics bundle containing:
  - connection details
  - codec, chroma and pixel formats
  - GPU and driver
  - HDR state
  - per-stage timings
  - a test-pattern capture

  Personal data is redacted by default.

## 12. Planned crate layout

| Crate | Contents |
|---|---|
| `illum` | The binary: role dispatch, session glue, host input rules (exists) |
| `illum-core` | Roles, protocol messages, capabilities, input messages, trust store (exists) |
| `illum-net` | TCP + TLS 1.3, pairing and fingerprints, channel framing (exists, M1); next: the UDP media path with TLS-exported keys |
| `illum-media` | Pixel format, codec interfaces, tile codec, packetisation and parity FEC, pacer, colour test pattern, cursor shapes, scaling, clock sync (exists, M2–M4); next: the two controllers, the simulated link |
| `illum-ffmpeg` | AV1 / H.265 encode and decode through FFmpeg: the probe, low-delay encoder settings, GPU decoding through FFmpeg's hardware paths with the embedded-keyframe probe, Opus (exists, v0.5.1–v0.8.0) |
| `illum-win` | Windows: DDA and GDI capture, input injection, system-key hook, WASAPI audio, keep-awake (exists); later service, process spawning into sessions, desktop switching, clipboard |
| `illum-ui` | Viewer window, `wgpu` presenter with the CPU fallback, controller-side input (exists); later device list and accept panel |

Crates are added only when a milestone needs them.

## 13. Open questions

- **Lossless tiles:** whether the v0.3.0 tile codec keeps any role beyond fallback and tests.
- **True reference invalidation:** FFmpeg gives no per-frame reference control for the hardware
  encoders, so a broken frame costs a keyframe (§5.3). Worth the vendor SDKs (NVENC's
  `NV_ENC_PIC_PARAMS` invalidation list, AMF, QSV) only if keyframes on loss show up as visible
  stalls in the M5 harness.
- ~~UI toolkit~~ decided in v0.12.0: `egui` (§14).
- Exact threshold for "network stall → release all keys".
- Parked ideas (not planned, kept so they aren't lost): a **4:4:4 mode** on H.265 for sharper
  coloured text, to try once 4:2:0 is stable (§5.3); a high-quality still frame after the
  screen stops changing; per-area encoder quality from a neighbourhood score; frame pacing to
  the display refresh (v2); an adaptive audio buffer; an emergency drop path if the harness shows
  one is needed.

## 14. The app (v0.12.0)

`illum` with no command opens the app: one window with **This desk** (the address to give out,
its key, "Allow incoming connections"), **Remote desk** (an address and Connect), **Your desks**
(every host this desk has connected to) and **Settings**. The layout follows AnyDesk's home
window with addresses where AnyDesk has IDs; the two questions about keys are dialogs over it.
The look comes from the icon (`assets/icon`): soft blue ground, navy outlines two pixels wide,
the strong yellow for the main action, the pale yellow for what is yours. The design canvas it
was drawn on first is linked from the roadmap.

**Toolkit: `egui`**, drawn with `egui-wgpu` in a `winit` window — the window and GPU stack the
viewer already uses, on every OS, with no second renderer in the binary. It brought `wgpu` from
26 to 30 (the viewer's presenter was ported; its offscreen picture tests pass unchanged). The
alternatives looked at: Slint (its own renderer next to ours, and its own windowing) and iced
(a different `wgpu` than ours at the time). Fonts are bundled (`assets/fonts`, Open Font
License): Inter, Space Grotesk, JetBrains Mono, Latin only, with a system font for Chinese,
Japanese and Korean behind them when the machine has one.

**Two halves, one direction each.**
- `illum_ui::app` draws [`AppState`] and returns [`Command`]s: what the person asked for. It
  knows nothing of networking. `illum_ui::app_window` is the window: it redraws on input, when
  egui asks (an animation), or when woken after the state changed — never on a timer.
- `illum::app` (`Backend`) keeps the state true and carries the commands out. It is given its
  window-waker, its listening address and its way of opening a session as values, so the whole
  of it runs in a test without a window: two desks on loopback go through a first connection
  with both key questions, a second without, a dead address, a forgotten and then declined
  desk (`two_desks_connect_through_the_app`).

**This desk** listens inside the app process while "Allow incoming connections" is on: the same
`open_listener` + `accept_loop` as `illum --service`, with the pairing question shown as the
*Incoming connection* dialog instead of asked in a console. Accepting pairs the desk, as it
always has: there is no accept-once and there are no per-connection permissions yet (the
dialog says in words what accepting allows). Both arrive with the granted-permissions message
of the backend freeze (D1). When the port is taken — a console `--service` is already running —
the window says so in plain words. This is the stand-in until the service of M6: the app then
talks to the service instead of listening itself.

**Connect** dials from the app first. A wrong address, a refusal or a changed key is then said
in the window, and a first connection gets its key check there (the *First connection* dialog).
That connection says goodbye at once; the session itself opens in **its own process**
(`illum view <host>` with the settings as flags). One window per session, and a session that
crashes takes nothing else with it; AnyDesk and RustDesk do the same. The cost is a second
handshake (a few round trips). A session that ends with an error in its first seconds is
reported in the window, pointing at `viewer.log`.

**Settings** are `settings.json` in the home folder (`illum_ui::app::Settings`): the ladder's
top (bitrate, frame rate, delay limit, scaling), reconnecting, keyboard and pointer mode,
sound, clipboard, and whether to listen. Unknown fields are ignored and missing ones take
their defaults, so the file survives versions in both directions; an unreadable file means the
defaults, never a window that will not open. They apply to the next connection.

**Looking at it without a desktop.** `illum_ui::app_window::snapshot` draws any state to
pixels with no window; the test `screens_draw` writes each screen to `target/app-pictures`.
That is how the interface was checked while it was built.

**The session window's bars (v0.12.1, `illum_ui::session_bar`).** A bar on top — who you are
connected to, Fit / Original, the keyboard mode, the pointer mode, full screen, Disconnect —
and a status strip at the bottom with the codec, the rung and the last second's numbers (they
left the title bar), or the notice when there is one ("connection lost; reconnecting…").
- **The picture gets the room between them:** the placement is computed for that area and
  moved down by the top bar, so the mapping from window to host pixels, panning in Original
  size and the embedded cursor all follow without knowing about bars.
- **Same frame, same GPU:** egui's output is handed to the presenter
  (`WindowPresenter::set_overlay`) and drawn over every picture in a second pass. It is laid
  out again only when something about the bars changed (a mouse event over them, a new status
  line, a mode), not once per video frame.
- **Mouse only.** No key ever goes to the bars: every key is the host's. A pointer over a bar
  is the window's own (plain arrow, nothing sent to the host); a click that began on a bar
  ends there. egui's wishes for the window's cursor and input method are ignored, because the
  session manages both.
- **Changing modes while connected.** Keyboard mode moves the grab with it
  (`ViewerOptions::on_keyboard`; everything held on the host is released first); pointer mode
  re-dresses the pointer on the next draw.
- With the CPU fallback (no usable GPU) there are no bars and the title bar carries the
  numbers, as before.

**Not yet:** switching the host's display, sound and clipboard toggles and Ctrl+Alt+Del in the
top bar (each needs something the session cannot do while running, or a protocol addition);
the incoming session cannot be ended from the app other than by closing it; desks show no
online state (nothing tells the app); the console window is released only when the app was
started by a double-click.
