# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Entries record *what shipped*, kept simple. The *why* behind a design lives in
the docs: [docs/architecture/architecture.md](docs/architecture/architecture.md)
and the evidence in [docs/lessons/lessons.md](docs/lessons/lessons.md).

## [Unreleased]

## [0.9.3] - 2026-11-02

What the reverse measurement run (desktop viewing the laptop) showed: seven
of ten steps recorded nothing, because the laptop's `hevc_qsv` was fed BGRA
and encoded **4:4:4** H.265, which the desktop's D3D11VA refused and the
software decoder returned as YUV444P, which the viewer rejected. Until 0.8.0
swscale hid this by converting every decoded frame to BGRA.

### Fixed

- **Encoders other than NVENC are fed NV12 (or 4:2:0 planar), never BGRA.**
  NVENC converts BGRA to 4:2:0 on the GPU; QSV given BGRA encodes 4:4:4.
- **A decoder that returns something other than 8-bit 4:2:0** (4:4:4, 4:2:2,
  10-bit) is converted to 4:2:0 once on the CPU, with one warning, instead of
  failing every frame.
- **A GPU decoder that fails on a stream hands over to the CPU decoder** once
  and asks for a keyframe, instead of asking for a keyframe for ever.

## [0.9.2] - 2026-11-02

What the first measurement run (laptop viewing the desktop, 24 steps) showed
about the measuring itself.

### Fixed

- **The viewer no longer holds the state lock while drawing.** The window took
  the lock, converted, scaled and presented, and only then let go; the receive
  thread waited behind every draw. On the GPU path that was a few ms, on the
  CPU path 70–90 ms per frame, so the receive thread fell further and further
  behind until "latency" read 30 s. The window now copies what it needs,
  draws without the lock, and takes it again only to record the result. The
  CPU-path rows of the first run measured this bug, not the path.
- **The CPU YUV → BGRA conversion is integer fixed-point** instead of
  floating point per pixel; the colours are the same to within a unit.
- **The still phase is still.** The workload window re-presented an unchanged
  picture 60 times a second, and the compositor handed each one to the
  capture as a new frame, so the host sent 58 fps of identical pixels. It now
  presents only when the picture changed: once a second while still.

### Added

- `scripts/measure-plan-short.csv`: the 8 steps at 6 Mb/s (both codecs, all
  four decode/render combinations), for re-checking a viewer machine.

## [0.9.1] - 2026-11-02

### Added

- `illum workload --screen N` fills monitor N instead of the first, so the
  host's own desktop stays usable during a run; `scripts/measure.ps1 -Screen N`
  makes the viewer stream that display.

## [0.9.0] - 2026-11-02

Measurement runs: the simulated-link harness, on the real link. The ladder
and parity numbers get tuned from these tables rather than from how a
session felt (docs/testing/measure.md).

### Added

- **`illum view --fixed`**: the host streams at exactly `--max-bitrate`,
  `--max-fps`, `--fec-min` and the new `--scale` percent for the whole
  session; the controllers are not consulted (`ScreenStart.fixed`,
  `scale_percent`; protocol 9). `--codec av1|hevc|tiles` narrows what the
  viewer offers, `--duration N` closes the window after N seconds, and
  `--report file.json --label name` writes every second of the session:
  frames, bytes, keyframes, per-frame latency / decode / draw / encode
  samples, longest stall, rtt, loss, repairs, frames lost, plus the host's own
  per-second numbers.
- **`HostStats`** (host → viewer, once a second): frames out of capture,
  skipped by the frame-rate cap, sent (and bytes), keyframes (and bytes),
  encoder time. The report can tell a still screen from a stalled link.
- **`illum workload`**: a full-screen window for the host side of a run, on
  the wall clock: 20 s still (one change per second), 20 s scrolling fine
  text, 20 s full motion, with a seven-segment clock in the corner (seconds
  while still, milliseconds otherwise, for slow-motion camera measurements).
- **`illum report <folder>`**: one Markdown table from a folder of report
  files, one row per step and phase (the first two seconds of each phase
  left out): fps, Mb/s, latency p50 / p95 / max, stall, decode, draw, encode,
  rtt, loss, frames lost, keyframes, host taken / capped / sent.
- **`scripts/measure.ps1`** with `scripts/measure-plan.csv`: 24 steps of
  60 s (bitrate sweeps per codec, decode and render variants, frame rate,
  parity and scale variants, tiles), one fresh session per step, then the
  table. Run it from each machine for the two directions.
- Tests: the workload phases and content, the recorder, the report
  aggregation and JSON round trip, and an in-process fixed session that holds
  its settings, scales to 50 % and records seconds with host stats.

### Fixed

- The loopback tests no longer fail now and then with WSAEACCES (10013) when
  binding the media port. With port 0, Windows picked the TCP port
  sequentially, and once it was inside a UDP-reserved block (Hyper-V, WSL)
  every retry landed in the same block. The listener now draws the shared
  port at random from the dynamic range and keeps a refused port's TCP
  listener until a pair works (45 binds × 15 runs without a failure).

## [0.8.0] - 2026-11-02

M8, the viewer half: the picture no longer touches the viewer's CPU. Built
with the three operating systems in mind — FFmpeg's hardware paths and `wgpu`
carry the OS differences; nothing here is written against a Windows API.

### Added

- **GPU decoding** (`illum-ffmpeg`): AV1 and H.265 through FFmpeg's hardware
  decode paths — D3D11VA (then DXVA2) on Windows, VAAPI / CUDA / VDPAU on
  Linux, VideoToolbox on macOS, one list — with each surface copied down once
  as NV12. On a two-GPU machine the second and third adapter get a try. What
  the GPU can decode is found by decoding one embedded 256×144 keyframe per
  codec (`crates/illum-ffmpeg/probe/`), the same check on every vendor and OS;
  `illum codecs` prints it under "Decoders (GPU)". A viewer with
  `--software-decode`, or whose GPU has no path for the codec, decodes on the
  CPU as before; a probed decoder that will not open for a session falls back
  to the CPU and says so in the log.
- **GPU presentation** (`illum-ui::gpu`, `wgpu`): the decoder's planes are
  uploaded as they are and one shader converts (BT.601 / BT.709, limited /
  full range, from the picture's own description), scales and blends the
  cursor. The swap chain uses the newest-frame-wins mode (Mailbox, else
  Immediate, else Fifo) with a frame latency of 1, so presenting never blocks
  and a late frame never queues behind an older one. Fit and Original modes
  and the locally drawn cursor behave as before. `--software-render` forces
  the CPU path, which also remains the fallback on a machine with no usable
  GPU API.
- Tests: offscreen renders of the colour pattern from I420 and NV12 planes in
  both matrices and both ranges with unaligned strides, half-size scaling,
  cursor blending at the hotspot; GPU decode round trips and lag checks for
  both codecs (they skip with a note where there is no GPU).

### Changed

- Decoders hand the window their native 4:2:0 planes (`Picture::Yuv`) instead
  of converting to BGRA with swscale; the tile codec returns `Picture::Bgra`.
  The encoder now declares BT.601 limited range in the stream, which is what
  its BGRA → 4:2:0 conversion produces, so the viewer converts back the same
  way.
- AV1 is offered by a viewer whose GPU decodes it; the rule from 0.7.2 now has
  something to find, so this PC ↔ laptop goes back to AV1 when the laptop's
  GPU decodes it.

## [0.7.3] - 2026-11-02

What an hour of nobody watching taught.

### Fixed

- **The ladder no longer ratchets down behind a hidden window.** With the
  viewer minimised (or its display off) nothing is presented, so almost no
  second is graded; the laptop's Wi-Fi hiccups for 3 s every five minutes,
  those seconds graded 🔴, and "3 of the last 10 graded seconds" was met with
  evidence spread over twenty minutes — one rung lost per quarter hour. Two
  rules: the 10 graded seconds of a drop must lie within 30 s (a raise's
  within four times its window), and "frozen by loss" now looks at frames
  **decoded**, not shown — a hidden window decodes everything and presents
  nothing, which is no evidence about the link at all.
- **The host stays awake while it streams** (system and display): an idle
  hour put the host to sleep, which took the capture and the connection with
  it. Roadmap M6's "wake lock", pulled forward.
- An unexpected `AcquireNextFrame` error (display turned off, device state)
  recreates the duplication instead of ending capture; capture gives up only
  after 30 s of continuous failure.
- The host logs once when the viewer decodes but does not present.

### Changed

- Protocol version 8: `MediaReport` carries `decoded`.

## [0.7.2] - 2026-10-30

The codec rule enforced as written, and the numbers to judge the viewer by.

### Changed

- **A viewer offers AV1 only when it decodes AV1 in hardware.** Since v0.5.1
  the viewer advertised `dec:av1` on the strength of the software decoder
  (`dav1d`), so the laptop received AV1 and spent ~20 ms per 1080p frame
  decoding it on the CPU — the main part of the lag next to RustDesk. The
  rule (§5.3) was always "AV1 if both sides can use it in hardware"; until the
  GPU decode path lands (M8) every viewer therefore gets H.265, whose software
  decoder costs half as much, and AV1 comes back the day the hardware path
  exists.
- New viewer setting **hardware decode**, default on: `illum view
  --software-decode` turns it off (CPU decoders only, so never AV1).
- NVENC encodes AV1 in 4 tile columns, so a software decoder can use several
  cores without adding a frame of delay (~2 % more bits).
- The title bar and viewer log now show `decode x ms` and `draw y ms` next to
  `encode`, so the viewer's own cost is visible.

### Verified

- `cargo test`: 136 tests pass (the end-to-end codec test now expects what
  the rule yields on the test machine); `cargo clippy` is clean.

## [0.7.1] - 2026-10-30

What the first live run of the adaptation taught (this PC → laptop over
Wi-Fi, a two-hour session).

### Fixed

- **Clock drift blinded the latency controller.** The viewer's estimate of
  the host clock came from the single fastest probe of the whole session; the
  two machines' clocks drift apart by tens of microseconds per second, and
  after two hours the estimate was ~80 ms stale, so the title showed
  `latency −37 ms`, every frame counted as "within 50 ms" and the ladder never
  moved. The offset now comes from the fastest probe of the **last minute**.
- **Keyframes get at least 30 % parity.** A keyframe is ~180 packets; with
  5–10 % parity on a Wi-Fi burst it mostly died, the viewer waited, the retry
  died too — stalls of 1–20 s (the "laggy" feeling). Keyframe requests are
  also honoured every 250 ms instead of 500 ms.
- The "frozen by loss" grade now triggers at **2** lost frames in a second
  with almost nothing shown (was 8): during a stall only the two keyframe
  attempts per second die while the small frames in between arrive.
- The frame-rate cap skipped frames captured a microsecond early; at 60 fps
  that cost up to a third of the frames.
- Capture times now travel through the encoder as `pts` and come back with
  each packet, so a codec that returned frames late could not mislabel them
  (NVENC and the decoders were tested and do not lag; the guard stays).

### Verified

- `cargo test`: 136 tests pass, including the new ones: clock sync follows
  a 12 ppm drift over a simulated two hours; AV1 and H.265 encoders and
  decoders return the frame they were given (checked by the test pattern's
  moving bar at 1080p). `cargo clippy` is clean.

## [0.7.0] - 2026-10-30

Audio, the Sunshine way (architecture §8). The host's sound reaches the
viewer; nothing adapts, nothing waits.

### Added

- **Audio capture and playback** (`illum-win`): WASAPI loopback of the
  default output on the host, WASAPI playback on the viewer, both asked for
  48 kHz stereo 16-bit with the engine converting.
- **Opus** through FFmpeg's `libopus` (`illum-ffmpeg::opus`): 5 ms frames,
  ~80 kb/s, low-delay application, one packet per frame.
- **Audio over the UDP media path** (`illum_media::audio`): an 8-byte header,
  every 4 frames protected by 2 Reed–Solomon parity packets (a 20 ms group;
  any 4 of 6 rebuild the rest), data packets sent the moment they are
  encoded. Separate stream, separate sequence numbers, same keys.
- **Fixed 40 ms jitter buffer** on the viewer; a missing frame is 5 ms of
  silence; 100 ms of nothing means the host is quiet and the buffer
  re-primes when sound returns; a backlog is skipped, never played late.
- `illum --service --no-audio` ("send audio: off": nothing is captured or
  sent); `illum view --mute` (don't ask for sound). The test pattern host
  hums a 440 Hz tone. The title bar shows `audio` while frames arrive.

### Changed

- Protocol version 7: `ScreenStart` carries `audio`; capability `audio`.

### Verified

- `cargo test`: 131 tests pass — audio packets and parity groups (two lost
  of six rebuilt, three lost handled), the jitter buffer (priming, holes,
  pauses, backlog), an Opus round trip through the real encoder and decoder
  (level within 2 dB, ~80 kb/s), and end to end over loopback UDP: a viewer
  that asks gets ≥ 100 frames in half a second, a muted one gets none.
  `cargo clippy` is clean.
- Not yet heard live between the two machines.

## [0.6.1] - 2026-10-30

Documentation only.

### Changed

- `docs/architecture/adaptation.d2` redrawn as the per-second adaptation
  cycle as built in v0.6.0: the viewer's measurement, the report, the grade
  (with the freeze-by-loss rule), the latency controller with back-off, the
  FEC controller with its zones and backstop, what gets applied, and the
  `Quality` message back to the viewer.

## [0.6.0] - 2026-10-30

Milestone M5: adaptation. The host now finds the quality the link can carry
instead of sending the cap and hoping.

### Added

- **The ladder and the latency controller** (architecture §5.5). Every
  second the viewer reports how many frames it showed and how many arrived
  within the latency limit (default 50 ms). The host grades the second
  🟢/🟡/🔴/⚪ and walks a ladder — bitrate × 0.8 steps, then 60/45/30/20/15 fps,
  then 75 % and 50 % resolution, bottom ~1 Mb/s — one rung down when 🔴 fills
  30 % of the last 10 graded seconds, one rung up when 🟢 fills 80 % of the
  last 15, never while the path is losing packets. Starts at the top.
- **The FEC controller** (§5.6): packet loss before repair over 30 s windows
  moves parity +5 points per 🔴 window (≥ 0.5 %) up to the ceiling, −5 per 🟢
  window (< 0.1 %) after two in a row down to the floor; defaults 5–30 %.
  Parity comes out of the rung's budget. At the ceiling with frames still
  dying, it asks the ladder for one rung down.
- **Simulated link** (`illum_media::sim`, §5.7) and six scored scenarios in
  `crates/illum-media/tests/harness.rs`: utilisation, flaps, recovery time,
  false drops, seconds over the limit.
- Viewer settings as flags: `--latency-limit <ms>`, `--max-fps`,
  `--fec-min`, `--fec-max` (with the existing `--max-bitrate`).
- The host tells the viewer its rung and parity (`Quality`); the title bar
  shows `rung 3/11: 12.8 Mb/s 60 fps, parity 10 %`, the host log says why
  each change happened.

### Changed

- Three rules that the harness forced, all recorded in §5.5: a second with
  almost nothing shown is ⚪ only if nothing was lost — if at least half the
  grading threshold of frames were *lost*, it is 🔴 (an oversubscribed path
  otherwise left the controller blind for minutes); a raise that is undone
  within 60 s backs off (the green run needed doubles, 15 → 30 → 60 → 120 s),
  which stops the hunting between the rung that fits and the one above; the
  backstop fires only while frames are still being lost.
- The pacer's burst now holds at least two frames (was a fixed 50 ms), and the
  encoder gets `rung × 100 / (100 + parity) × 90 %` instead of a fixed 85 %:
  frames leave at link speed, and parity really is budget-neutral.
- Encoders are reopened when the rung changes frame rate or resolution
  (hardware encoders take bitrate changes live; software ones are reopened).
- Protocol version 6: `ScreenStart` carries the settings, `MediaReport` the
  shown/within counts, and `Quality` is new.

### Verified

- `cargo test`: 122 tests pass, including the six harness scenarios and the
  end-to-end run that checks the host's first `Quality` (top rung, 5 %
  parity). `cargo clippy` is clean.
- Not yet measured live between the two machines.

## [0.5.8] - 2026-10-28

### Fixed

- **Stream dying for good after a lost frame.** The viewer asks for a
  keyframe when a frame is lost; the host honours that at most twice a
  second — and a request arriving inside that window was *dropped*, leaving
  the viewer skipping every frame until the end of time. The request now stays
  pending until it is served, and the viewer repeats it once a second while
  non-keyframes keep arriving (which also covers a keyframe lost in full).
- Media sockets ask for 4 MB kernel buffers instead of Windows' 64 KB
  default, so a keyframe burst no longer overflows the receive buffer.

### Added

- **Log files.** Each role writes `<home>\logs\<role>.log` at debug level
  (`service.log`, `viewer.log`; home is `%ProgramData%\Illumination`), next
  to the console output. The path is printed at start.

## [0.5.7] - 2026-10-28

### Added

- **GDI capture fallback.** When Desktop Duplication is not available from
  the host process (on the laptop, Windows had bound illum to the discrete
  GPU, which cannot duplicate the panel), capture switches to GDI the way
  RustDesk does: `BitBlt` at up to ~60 fps, unchanged frames dropped, cursor
  from `GetCursorInfo`. Slower than duplication, but it streams without any
  Windows setting. The host log says which method is in use.

### Changed

- The "set Power saving in Windows graphics settings" advice is gone from the
  error path; it is a log hint only.

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
