# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Entries record *what shipped*, kept simple. The *why* behind a design lives in
the docs: [docs/architecture/architecture.md](docs/architecture/architecture.md)
and the evidence in [docs/lessons/lessons.md](docs/lessons/lessons.md).

## [Unreleased]

## [0.5.6] - 2026-10-28

### Fixed

- The viewer's "no video within 5 s" message no longer replaces an error the
  host already reported (on the laptop it hid the real capture failure behind
  a firewall guess), and it is worded as the guess it is.

## [0.5.5] - 2026-10-28

### Fixed

- **Hosting from a two-GPU laptop.** On the laptop, `DuplicateOutput` failed
  with `DXGI_ERROR_UNSUPPORTED` and the host retried forever behind a black
  viewer: the NVIDIA layer lists the panel under the discrete GPU when the
  program runs on it, but only the GPU that drives the panel can duplicate
  it. Capture now keeps every adapter that lists the display and moves to the
  next one when one refuses; if all refuse, the viewer gets the reason and
  the Windows graphics-settings fix instead of a black window.

## [0.5.4] - 2026-10-28

### Fixed

- `scripts/get-libclang.ps1` falls back to 7-Zip when Windows' `tar.exe`
  exists but cannot unpack `.xz` (older builds shell out to a missing `xz`
  program), and `-Use7Zip` forces that path. A finished download is reused
  across attempts.

## [0.5.3] - 2026-10-28

### Fixed

- `scripts/get-libclang.ps1` fetches LLVM's `.tar.xz` instead of `.tar.zst`
  and unpacks it with Windows' own `tar.exe` (7-Zip only as a fallback): the
  `.zst` asset needs 7-Zip 23.01+, which the laptop didn't have. Both fetch
  scripts also turn off PowerShell's progress bar, which made the downloads
  several times slower, and the libclang script reuses a finished download.

## [0.5.2] - 2026-10-28

### Removed

- Software AV1 (`libsvtav1`) is no longer an encoder candidate. The rule is
  AV1 in hardware, otherwise H.265; a machine without a hardware AV1 encoder
  goes straight to H.265 (hardware, or `libx265` as the last resort).

## [0.5.1] - 2026-10-22

Milestone M4, second slice: the video codec and parity FEC. Lossless tiles
remain only as the fallback.

### Added

- **AV1 / H.265 video through FFmpeg** (new crate `illum-ffmpeg`, FFmpeg 8.1
  prebuilt, fetched once by `scripts/get-ffmpeg.ps1`; `scripts/get-libclang.ps1`
  fetches what the bindings need to build). At start each side probes which
  encoders actually open here (NVENC, AMF, QSV, then software) and lists them
  in its hello as `enc:av1`, `enc:hevc`, `dec:av1`, `dec:hevc`. The host picks
  **AV1 if it can encode it and the viewer decode it, otherwise H.265,
  otherwise tiles**; the choice is per direction, 4:2:0, and shown in the
  title bar and the log. Encoders run with no B-frames, no lookahead and a
  100 ms rate buffer; decoders with slice threading only, so a packet yields
  its picture at once.
- **Parity FEC.** Every frame's packets carry Reed–Solomon parity (blocks of
  ≤ 128 packets, **5 %** of each block, at least one packet), so the viewer
  rebuilds lost packets without waiting or a round trip. Repaired frames are
  counted (`MediaReport.repaired`, "N repaired" in the title). The 5–30 %
  controller is M5.
- `illum codecs`: what this machine can encode and decode, with FFmpeg's
  reason for each hardware encoder that would not open.

### Changed

- Protocol version 5: a new packet header (codec id, block and shard fields,
  30 bytes) and the `repaired` field in the loss report.
- Reference invalidation is served with a **keyframe** for now: FFmpeg gives
  no per-frame reference control for the hardware encoders (architecture §13).
- FFmpeg 8.1 rather than 9.0, whose NVENC needs NVIDIA driver 610+ and would
  otherwise fall back to software unnoticed; 8.1 needs 570+.
- `illum-ffmpeg`'s build copies the FFmpeg DLLs next to every executable that
  links it (so tests run too).

### Verified

- `cargo test`: 104 tests pass. New: Reed–Solomon parity and rebuild;
  block-based packetisation with repair, out-of-order and stalled frames;
  the tile codec behind the common codec interface; AV1 and H.265 round trips
  of the colour pattern through the real encoders (`av1_nvenc`, `hevc_nvenc`
  on this PC) and decoders, every patch within a colour distance of 12;
  end to end over loopback UDP: the host picks AV1 for a viewer that
  advertises `dec:av1`, colours survive; a viewer without decoders gets
  tiles, bit-exact; through a 1 % lossy relay frames are **repaired by
  parity** (more repaired than lost) and the pattern stays intact.
  `cargo clippy` is clean.
- Not yet run live between this PC and the laptop.

## [0.5.0] - 2026-10-22

Milestone M4, first slice: the media transport. Video now travels over UDP
and is never resent; control and input stay on TLS over TCP.

### Added

- **UDP media path.** The host binds UDP on the same port number as TCP
  (default now **28326** for both; `--port` moves both). The viewer sends
  small "punch" datagrams so the host learns its address, then video flows
  host → viewer. Every datagram is encrypted and authenticated with keys
  **exported from the TLS session** (RFC 5705), so there is no second
  handshake; the host finds the session by an id in the datagram header and
  rejects replays and forgeries.
- **Packetisation and reassembly.** Frames are split into ≤ 1200-byte
  datagrams (fits Tailscale's MTU). A frame is shown only when every packet
  has arrived; a frame whose packets stop, or that a newer frame overtakes,
  is reported lost and the viewer asks for a keyframe (the host honours that
  at most twice a second). Until the keyframe arrives the viewer keeps showing
  the last good picture.
- **Pacing by bitrate.** A token bucket paces datagrams to a cap
  (`view --max-bitrate <Mb/s>`, default 20, capped by the host at 20). The host
  encodes only the newest captured frame once the previous one has left, so
  nothing queues up. The old "two frames in flight" window is gone;
  acknowledgements are feedback only.
- **Loss feedback.** The viewer reports datagrams received and lost (counted
  by sequence gaps, before any repair) and frames lost, once a second; the
  title bar shows `loss x%`.
- **Blocked-path detection.** If no punch reaches the host within 5 s, or no
  video reaches the viewer, each side says so in plain words instead of
  hanging.

### Changed

- Protocol version 4; a `udp-media` capability. Hosts and viewers from
  v0.4.x can't connect to v0.5.0 (the hello refuses the mismatch cleanly).
- Video no longer travels on the TCP channel at all. Cursor updates still do.

### Verified

- `cargo test`: 89 tests pass, including: datagram seal/open/replay/forgery
  tests; both sides derive the same media keys from a real TLS handshake;
  packetisation with out-of-order and stalled frames; the pacer's average
  rate; and end-to-end runs over loopback UDP (bit-exact colour pattern,
  recovery through a 1% lossy relay, a blocked path reported within 5 s).
  `cargo clippy` is clean.
- Live on this PC over its Tailscale address: test pattern at **59 fps,
  9 ms capture-to-screen, 0.0% loss, RTT 0.3 ms**. Real screen capped at
  6 Mb/s: the pacer held ~6 Mb/s with **RTT flat at 0.2 ms** (no queue);
  the picture ran at 2–4 fps because lossless tiles of a full-screen change
  are about 2 MB per frame, which is what the hardware codec (v0.5.1) is for.
- Not yet run from the laptop.

## [0.4.4] - 2026-10-22

Documentation only. Two decisions that close the open questions before M4.

### Changed

- **Codec library: FFmpeg**, for all platforms from the start, with a trimmed
  build bundled in the installer.
- **Chroma: 4:2:0 in v1** for both AV1 and H.265. A 4:4:4 mode on H.265, for
  sharper coloured text at roughly 1.5–2× the bitrate, is noted as future work
  to try once 4:2:0 is stable (roadmap M8).

## [0.4.3] - 2026-10-22

Documentation only: the design decisions from the bandwidth discussion, after
the first two-machine test. No code changes.

### Changed

- **Architecture:** media (video and audio) moves to **UDP and is never
  resent**; control and input stay on TLS over TCP; one port number for both,
  default **28326** (code still uses 7171 until the media path lands). Codec:
  **AV1 if both sides can, otherwise H.265**, chosen per direction. Loss:
  **parity FEC over the whole frame** (5–30%, adaptive, budget-neutral) plus
  reference invalidation. A full **adaptation design**: a latency controller
  on a user-set limit (50 ms) with per-second grades, interval rules and a
  one-rung-at-a-time ladder, and a separate slow FEC controller, with three
  coordination rules. Audio the Sunshine way: Opus 5 ms frames, fixed 4+2
  parity groups, ~40 ms buffer. User settings, the tunable numbers and the
  simulated-link test harness are written down. Ideas discussed and not
  adopted are listed so they aren't re-proposed.
- **Roadmap:** re-planned. M4 is now the media transport and codec, M5 the
  adaptation and harness; the service, desktop features, quality and release
  milestones became M6–M9. M1–M3 checks updated with the laptop results.
- **Lessons:** AnyDesk's bandwidth behaviour and connect timeline from the
  two sessions; RustDesk's adaptation as documented in its `video_qos.rs`;
  Sunshine/Moonlight's FEC, audio, connection-status thresholds, bitrate
  table and pacer, from their source; the two-machine test numbers; the
  laptop's codec support; the port check.
- Two new diagrams: `media-transport.d2` and `adaptation.d2`;
  `milestones.d2` updated.

## [0.4.2] - 2026-10-19

Fixes from the first two-machine test.

### Fixed

- **Small text looked broken in the viewer.** Shrinking the host screen to fit
  the window dropped whole pixel rows and columns, so thin strokes vanished in
  places. Each window pixel now averages the host pixels it covers.
- **Alt+Tab and the Win key stayed on the controlling machine.** The keyboard
  hook was registered without a module handle, which Windows can refuse. It now
  passes the program's module, switches on even if the window already has focus
  when it opens, and prints a clear note if it still fails.
- **Closing the viewer panicked** ("there is no reactor running"). The
  shutdown timer was created outside the async runtime.
- **The laptop console filled with stats and colour codes.** The once-a-second
  stats line is now only shown with `-v` (the window title still has it), and
  colour codes are off on Windows consoles, which printed them raw.

### Added

- **Original-size view:** 1:1 pixels, and the visible part follows the mouse
  when the host screen is larger than the window. **Ctrl+Alt+Shift+F2**
  switches between fit and original size; the hotkey is never sent to the host.

### Changed

- `Cargo.toml` names the real repository.

### Verified

- `cargo test`: 74 tests pass. New tests check that averaging keeps a
  one-pixel line instead of dropping it, that the original-size view pans and
  maps the mouse back correctly, and (with `-- --ignored`) that the keyboard
  hook installs. `cargo clippy` is clean.
- Live: when the host goes away, the viewer now exits with a plain error
  instead of a panic, and the console shows no stats spam or colour codes.

## [0.4.1] - 2026-10-19

### Fixed

- README: the "Try it" steps now run in PowerShell. PowerShell needs `.\illum.exe`
  to start a program from the current folder; plain `illum` fails. The steps
  also show how to clone and build on each machine.

## [0.4.0] - 2026-10-19

Milestone M3, input: you can control the host's keyboard and mouse from the
viewer.

### Added

- **Keyboard:**
  - keys go by position (scancode), so the host's layout and IME apply
  - three modes, as in AnyDesk, with `--keyboard remote|local|auto`; Auto
    compares the two machines' keyboard layouts, which the host reports
    whenever its layout changes
  - in Local mode, characters and this machine's IME result are sent as text
    and typed on the host as Unicode
- **No stuck keys and no extra Enters:**
  - only the host repeats keys (after 500 ms, every 40 ms); the controller
    drops its own repeats
  - while keys are held, the controller sends heartbeats
  - the host stops repeating after 300 ms of silence and releases everything
    after 1 s, when the viewer loses focus, or when the session ends
  - duplicate presses and old sequence numbers are ignored
- **Mouse:** absolute positions on the shown display, buttons (including
  back/forward) and wheel. The host is DPI-aware, so positions match the
  captured pixels.
- **Cursor:** while controlling, the host's cursor shape is drawn at your own
  mouse position, so it never lags.
- **System keys:** Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Alt+F4 go to the host
  while the viewer has focus (low-level keyboard hook).
- **View-only:** `--view-only`. The viewer also opens view-only on its own if
  the host doesn't offer input.

### Changed

- Protocol version 3: input channel messages, a keyboard-layout message, and an
  `input` capability.
- Hosts advertise only the capabilities they have. A test-pattern host offers
  no input.

### Verified

- `cargo test`: 71 tests pass, including:
  - the input rules replayed with a fake clock (a 3 s network drop while
    holding Enter gives no extra Enters and no stuck key)
  - an end-to-end test where a real connection drops while Enter is held and
    the host releases it
- `cargo clippy` is clean.
- Live on this PC: the viewer opens view-only against a test-pattern host, and
  real-screen streaming still works.
- Real keyboard and mouse injection is not yet tested by hand. It needs two
  machines, because controlling your own PC loops input back into the viewer.

## [0.3.0] - 2026-10-19

Milestone M2, first pixels: you can see another machine's screen live in a
window. Input comes in M3.

### Added

- **`illum view <host>`:** opens a window with the host's screen, shown 1:1 or
  scaled down to fit. The title bar shows frame rate, bandwidth, latency,
  encode time and round-trip time. `--screen N` picks another display.
- **Screen capture on Windows** with DXGI Desktop Duplication. Only the BGRA8
  desktop format is accepted, and capture is recreated after a mode change.
  `illum displays` lists monitors, primary first.
- **Lossless tile codec:**
  - 64×64 tiles; only tiles that changed are sent, each LZ4-compressed
  - colours arrive bit-exact
  - about 3 ms encode at 1080p
  - like AnyDesk's segmented DeskRT, a screen codec rather than a video codec
- **One pixel format** (BGRA8) end to end, translated for display in exactly
  one function. Tests pin the channel order, so a red↔blue swap fails the build.
- **Cursor sent separately** from the video (position and shape, including
  colour, masked-colour and monochrome cursors) and drawn by the viewer.
- **Frame pacing:** the viewer acknowledges each frame it shows, and the host
  keeps at most two frames in flight, always sending the newest screen, so
  delays never pile up.
- **Clock sync** over the control channel to measure capture-to-screen latency.
  The probes also keep a session on a still screen from timing out.
- **Colour test pattern:** `illum --service --test-pattern` streams eight
  colour patches, a grey ramp and a moving bar instead of the screen.
- New crates: `illum-media` (portable media), `illum-win` (Windows capture) and
  `illum-ui` (viewer window).

### Changed

- Protocol version 2: screen start/stop, screen errors, frame
  acknowledgements, clock probes, and a `screen` capability.
- Host logs show the full cause of an error, not just "network error".

### Verified

- `cargo test`: 53 tests pass, including an end-to-end test over TLS that
  checks the colour pattern arrives bit-exact and that the host stops after two
  unacknowledged frames. The real-capture test (`-- --ignored`) passes on this
  PC. `cargo clippy` is clean.
- On this PC over its Tailscale address: the test pattern ran at 59 fps,
  0.3 Mb/s, about 5 ms capture-to-screen. The real desktop streamed live (15–36
  fps in the worst case, where the viewer shows itself and every frame changes
  completely).
- Not yet run from the laptop, and the cursor is not yet checked by eye.

## [0.2.0] - 2026-10-19

Milestone M1, connect and trust: two machines connect directly, check each
other's keys and measure round-trip time. There is no screen yet.

### Added

- **Hosting:** `illum --service` listens on this machine's **Tailscale addresses
  only**, port 7171. Other addresses are opt-in with `--bind <IP>`. There is no
  UPnP.
- **Connecting:** `illum connect <host>` dials an IP address or MagicDNS name
  (port optional) and prints round-trip times with a min/avg/max summary.
- **Device identity:** a self-signed Ed25519 certificate is made on first start
  and kept in `%ProgramData%\Illumination`. `illum fingerprint` shows it.
- **Encryption:** TLS 1.3 only, with certificates on **both** sides. Every
  handshake signature is checked.
- **Trust by pairing.** On a first connection the controller shows the host's
  fingerprint and asks whether to trust it. The host shows the device's
  fingerprint and asks whether to pair. After that, connections need no
  prompts. A host whose key changed is refused, and the error names the fix
  (`illum forget <host>`).
- `illum list` shows pinned hosts and paired devices. The trust store is
  versioned JSON.
- **Protocol:**
  - `hello` exchange with protocol version and a capability list
  - channel framing (`[channel][length][payload]`)
  - a send queue where input and cursor go before control, then video, then
    audio, then bulk, and where video can replace stale frames
- **Deadlines on every step:** name lookup, TCP connect, TLS, hello, prompts
  (unanswered counts as "no"), and idle sessions (closed after 30 s).

### Changed

- Clearer errors: an unreachable host says so in plain words instead of
  repeating the operating system's message. Logs have no colour codes when
  written to a file or pipe.

### Verified

- `cargo test`: 33 tests pass (unit tests, plus 9 loopback integration tests
  for pairing, refusals, a changed key and deadlines). `cargo clippy` is clean.
- On this PC over its own Tailscale address: the first connection asked once
  on each side, the reconnect needed no prompt, and the LAN address refused.
- Not yet run from a second machine (the laptop). See the M1 check in
  `docs/roadmap/roadmap.md`.

## [0.1.0] - 2026-10-19

First milestone, M0: the repository, the design and the plan. Nothing connects
yet.

### Added

- Rust workspace (edition 2024) under GPL-3.0-only, with two crates:
  `illum` (the single binary) and `illum-core` (shared code).
- `illum` picks its role from the command line, the same way AnyDesk's single
  binary does: the app window (no flag), `--service`, `--backend` and
  `--control`. Each role reports that it isn't implemented yet. Unknown flags
  exit with a usage message.
- Design docs:
  - `docs/architecture/`: roles and processes, one TLS 1.3 connection with
    prioritised channels, pairing by fingerprint, the video pipeline, input
    correctness rules, secure-desktop handling, permissions and session policy,
    with D2 diagrams.
  - `docs/lessons/`: evidence from AnyDesk (idle and a live direct session over
    Tailscale), RustDesk and Sunshine behind every decision.
  - `docs/roadmap/`: milestones M0–M7, each with a pass/fail check.
- Verified with `cargo build` and `cargo test` (3 tests passing).
