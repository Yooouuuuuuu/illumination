# Illumination architecture

Status: agreed design, nothing implemented yet. Last updated 2026-10-19.

Diagrams in this folder (render with `d2 <file>.d2`):
- [`processes.d2`](processes.d2): roles and processes on one machine
- [`session.d2`](session.d2): connection and session start
- [`video-pipeline.d2`](video-pipeline.d2): the media path from host to controller

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

## 4. Network and trust

### 4.1 Listening
- **One TCP port**, default **7171**. It can be changed. The default differs from AnyDesk's 7070
  so both can run on the same machine.
- Default bind: **Tailscale interfaces only** (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`). LAN
  interfaces are opt-in. Never `0.0.0.0` by default, and no UPnP.
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
2. **hello** in both directions: protocol version, **capability list**, host keyboard layout,
   displays. AnyDesk logs `Remote caps: sound_src, sound_sink, keyboard_sink, …`. A feature is
   used only if both sides list it.
3. The service starts a backend in the console session.
4. **Authorisation.** Interactive profile: the backend shows the accept panel to the person at
   the host. Unattended profile: a paired device, optionally with a password.
5. **Session accepted.** The host sends the granted permissions. From then on, all traffic runs
   over the same TLS connection as separate channels.

### 4.4 Channels on one connection
One TCP+TLS connection carries every channel. Each message has a small header (channel, length).
The sender gives some channels priority over others:

| Priority | Channel | Notes |
|---|---|---|
| 1 (highest) | input, cursor | Small and latency-critical; never queued behind video |
| 2 | control | Session commands, permissions, pings, feedback |
| 3 | video | **Latest frame wins.** Frames still waiting to be sent are replaced, never queued |
| 4 | audio | Opus, 20 ms frames |
| 5 (lowest) | clipboard, files | Bulk; paused while the video queue is under pressure |

**Why TCP:** AnyDesk's direct path is TCP (port 7070, observed), and it feels smooth on your
tailnet. TCP's weak spot is that one lost packet holds up everything behind it. We follow
AnyDesk's mitigations:
- **Small socket send buffer, tuned at runtime.** AnyDesk logs `Adjusted send buffer: 8192` and
  then `16384`.
- Never let video pile up in the socket: pace frames and drop stale ones before sending.
- Adapt to measured bandwidth (§5.4).

If Tailscale falling back to its DERP relays turns out to hurt in practice, revisit this with
measurements. Don't change it in advance.

## 5. Video

See [`video-pipeline.d2`](video-pipeline.d2).

### 5.1 Capture
- Primary: **DXGI Desktop Duplication (DDA)**, with a fallback chain (Windows.Graphics.Capture,
  then GDI). AnyDesk: `capture.fallback_capture - Selecting DDA capture.`
- Use the dirty rectangles that DDA reports, and keep frames on the GPU until encoding.

### 5.2 One pixel format
- Internally every frame is **BGRA8**, the format Windows calls `X8R8G8B8`. AnyDesk logs
  `Format changed to 1920x1080 (X8R8G8B8)`.
- All colour conversions (to YUV for the encoder, and back on the controller) live in one module.
  Colour space, range and channel order are always stated explicitly.
- HDR and wide-gamut desktops are tone-mapped to SDR on purpose.
- CI test: a colour test pattern goes through capture → encode → decode → present and is compared
  with a reference image. A red↔blue swap or a chroma shift fails the build.

### 5.3 Encoding
- The frame is split into **segments**, and segments that didn't change are skipped. AnyDesk logs
  `desk_rt.encoder - 8 segments.`
- Codec: hardware H.264/HEVC through the GPU vendor's encoder (NVENC, AMF, QSV), with **4:4:4
  when the GPU supports it** so text stays sharp. Software fallback. The encoder sits behind an
  interface so it can be swapped.
- When the screen stops changing, static regions are refined to full quality.

### 5.4 Adaptation
- Measure RTT and throughput continuously, and pick a quality/frame-rate preset from bands, as
  AnyDesk does: `Adjusting for a direct connection (997.84 kb/s)` and
  `Adjusting for 500-1.500 kb/s`.
- The controller sends feedback (RTT, throughput, frame acknowledgements) on the control channel.
- Optional, as AnyDesk does: reduce host animations during a session to save bandwidth
  (`taskbar_animation_modifier`).

### 5.5 Cursor and presentation
- The cursor shape and position are sent **separately** from the video and drawn locally by the
  controller, so the cursor never waits for a video frame. AnyDesk:
  `desk_rt.system_event_source - Enabled mouse cursor`.
- The controller decodes in hardware and presents through a flip-model swap chain, not through a
  UI toolkit's image widget.

## 6. Input

**Keyboard modes** (as AnyDesk):
- **Remote:** sends physical key positions (scancodes).
- **Local:** translates using the controller's layout.
- **Auto:** Remote if both layouts match, otherwise Local.

The host reports its layout at session start and whenever it changes. AnyDesk logs
`Keyboard layout changed to: zh_cl`.

**Text and IME:** finished IME text (for example Chinese) is sent as text and typed on the host as
Unicode (`SendInput` with `KEYEVENTF_UNICODE`).

**Correctness rules** (the "multi-enter" problem):
1. **Only the host generates key repeats.** Auto-repeat events from the controller are dropped.
2. **Every input event has a sequence number.** Duplicates are ignored.
3. **Release every held key and mouse button** on disconnect, a network stall (no input-channel
   traffic for longer than a threshold), or when the viewer loses focus.

**Mouse:** absolute coordinates mapped per monitor, using each monitor's DPI.

**Special keys:**
- Ctrl+Alt+Del is sent by the service with `SendSAS`. The installer sets
  `SoftwareSASGeneration` so this is allowed.
- Reserved hotkeys use the `Ctrl+Alt+Shift+<key>` pattern, as AnyDesk does: Del = Ctrl+Alt+Del,
  arrows = switch display, Return = fullscreen.

## 7. Secure desktop (UAC, lock screen)

The backend follows desktop switches as AnyDesk's `desktop_switcher` does:
1. Desktop change detected.
2. **Suspend capture.**
3. Switch the capture and input thread to the new desktop.
4. **Restart capture.** DDA reports "access lost" on a switch, and that is expected, not an error.

The session stays connected throughout.

## 8. Clipboard, audio, files

- **Clipboard** (`--control`):
  - Text and files are separate formats with separate permissions.
  - Watch for changes with `AddClipboardFormatListener`.
  - Each update carries its origin and a content hash, and an update that just came from the peer
    is never sent back. That prevents echo loops.
  - Retry with backoff when another program holds the clipboard.
- **Audio:** WASAPI loopback capture, resampled to 48 kHz, **Opus with 20 ms frames** at about
  80 kb/s. AnyDesk logs `Target frame length 20 ms` and `Bitrate set to 81690 bits/s`.
- **Files:** a later milestone, on the lowest-priority channel.

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
  show where the time goes.
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
| `illum` | The binary: role dispatch (exists) |
| `illum-core` | Roles, protocol messages, capabilities, permissions, config (exists) |
| `illum-net` | TCP + TLS 1.3, pairing and fingerprints, channel multiplexing and priorities (exists, M1) |
| `illum-media` | Pixel-format module, segmenter, encoder/decoder interfaces, pacing and adaptation |
| `illum-win` | Windows: service, process spawning into sessions, DDA capture, input, desktop switching, clipboard, power |
| `illum-ui` | The app window: device list, viewer, accept panel |

Crates are added only when a milestone needs them.

## 13. Open questions

- UI toolkit for the window and the viewer surface.
- Which encoder library to bind (vendor SDKs directly, or FFmpeg).
- How the controller asks for a full-quality refresh after it drops frames.
- Exact threshold for "network stall → release all keys".
