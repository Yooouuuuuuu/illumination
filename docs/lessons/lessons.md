# Lessons and evidence

The evidence behind [`../architecture/architecture.md`](../architecture/architecture.md):
- **AnyDesk 9.7.14** and **RustDesk 1.4.9**, both installed on our own Windows 11 machines. The
  evidence was collected on 2026-10-16, 2026-10-19 and 2026-10-22 from logs, live sockets, config
  key names and two real AnyDesk sessions over Tailscale.
- The source code of **RustDesk**, **Sunshine** and **Moonlight**, and public documentation and
  GitHub discussions.
- Our own first two-machine test of Illumination (v0.4.0, 2026-10-19).

Personal values (device IDs, public IPs, keys) are redacted.

**Rule:** when sources disagree, AnyDesk's observed behaviour wins, except for relay/ID (out of
scope), UPnP (refused) and the media transport (UDP, never resent; decided after our own test).
See architecture §2.

---

## 1. AnyDesk: what it does

### 1.1 Idle: identity, relay link, listener
From `%APPDATA%\AnyDesk\ad.trace` and `C:\ProgramData\AnyDesk\ad_svc.trace`:
```
anynet.connection_mgr - Generating a new certificate and key.          (first run)
anynet.connection_mgr - 3 CA certificate(s) loaded.
anynet.relay_connector - Connecting to relay boot.net.anydesk.com (1/1)
anynet.relay_connector - Skipping connect method connect_proxy_443 (1/6) (no proxy found)
anynet.relay_connector - Cipher: TLS_AES_256_GCM_SHA384 TLSv1.3
anynet.relay_connector - Received a new server list (10 servers)
anynet.relay_conn      - External address: <public-ip>:<port>.
anynet.tcp_acceptor    - Listening on TCP port 7070 with IPv4. / with IPv6.
anynet.tcp_acceptor    - Attempting UPnP forwarding to port 7070. … UPnP forwarding failed.
anynet.udp_socket      - … multicast group … / Multicast loop option set to 0.
app.discovery_manager  - repeat interval (1000), repeat limit (10), buffer interval (5000), port range (3)
```

Live sockets while idle:
- TCP listening on `7070` (IPv4 and IPv6)
- one TCP connection to a relay on `:443`
- UDP `50001` (discovery, multicast)

Port 7070 checked with `openssl s_client`:
- TLS 1.3, `TLS_AES_256_GCM_SHA384`
- self-signed certificate: `CN=AnyDesk Client`, RSA-2048, valid 2026-08-13 to 2076
- the server asks the client for a certificate too (`Requested Signature Algorithms`), so both
  sides authenticate

Processes:
- `AnyDesk.exe --service` (LocalSystem, session 0)
- `AnyDesk.exe --control` (user, session 1)
- the window, with no flag

`HKLM\…\Policies\System\SoftwareSASGeneration = 1`. This allows Ctrl+Alt+Del to be sent by
software. RustDesk is also installed and could be the one that set it.

### 1.2 Two direct sessions over Tailscale (2026-10-19)
The laptop dialled `100.100.101.1` (first session) and later dialled by ID (second session); the
host accepted both. Service log, first session:
```
anynet.connection_mgr - [100.100.101.2:54389] Incoming connection.
anynet.connection_mgr - [100.100.101.2:54389] Direct connection.
anynet.any_socket     - Connect request accepted, direct route created.
anynet.any_socket     - Local vport: 10, Remote vport: 10
anynet.any_socket     - Connection switch started. / completed. / acknowledged.
app.service           - Using protocol version 4.
app.session           - Session features: deskrt, clipboard, chat, audio, input, tcp_tunnel.
app.session           - 1 - Connecting to console session 1 (current 0).
win_app.process_starter - Spawning backend (elevated 1, session 1).
win_app.process_starter - Backend spawned as system user.
app.session           - 3: Authenticated by local user.
anynet.client_packetizer - Adjusted send buffer: 8192   (later 16384)
app.session           - Updating local clipboard forwarding (1). … (3).
```
Live sockets during the session:
- `100.100.101.1:7070 ← 100.100.101.2` (TCP, direct), **no UDP socket carried the session**
- the relay connection to `:443` stayed open, but nothing in the session log went through it

The second session, dialled by ID, raced 8 connection candidates (LAN, Tailscale and public
addresses) through `anynet.punch_connector` and selected the Tailscale one after about 1.8 s.
That is TCP hole punching, not UDP media.

Backend log (`--backend`, SYSTEM in session 1):
```
win_app.desktop_switcher - Desktop change: --> Default / Suspending capture. / Restarting capture.
capture.fallback_capture - Switching to dda. / Selecting DDA capture.
capture.privacy_feature_dwm1 - Using surrogate.
app.backend_session - Remote caps: sound_src, sound_sink, keyboard_sink, keyboard2, switch_sides,
                      request_elevation, request_elevation_pw, clip_paste_files, clip_src_files,
                      … lossless_codec, … quality_adaptive, … video_mode, encoder_reset,
                      encoder_retransmit, request_vp_dimensions, reset_vp_resolution, …
app.backend_session - Sending SAS (9) permissions (allowed supported)
app.backend_session - Sending clipboard files (11) permissions (enabled allowed supported)
app.backend_session - Sending privacy (12) permissions (disabled forbidden supported)
desk_rt.auto_adjust - Route type: direct (internet)
desk_rt.encoder     - 8 segments.
desk_rt.encoder     - Format changed to 1920x1080 (X8R8G8B8).
desk_rt.system_event_source - Enabled mouse cursor
desk_rt.capture_component - Keyboard layout changed to: zh_cl
audio.core_audio_capture - Target frame length 20 ms
audio.opus_encoder  - Bitrate set to 81690 bits/s.
taskbar_animation_modifier - Couldn't set value (5).
```
Control process: `clipbrd.capture - Registered for clipboard notifications`,
`ad_app.control - Session count: (1, 1)`.

`powercfg /requests` during the session: AnyDesk holds both the **DISPLAY** and **SYSTEM**
requests.

**Timeline from connection to picture** (first session): backend spawn and desktop switch
0.1–3.6 s, waiting for the Accept click 3.6–5.5 s, capture and first frame 5.5–6.0 s, bandwidth
estimate at 6.7 s. AnyDesk's slower connect comes from starting a SYSTEM process per session and
(by ID) racing candidates, not from measuring the link before streaming.

### 1.3 AnyDesk's bandwidth behaviour
What its log shows, both sessions:
```
app.backend_session - Received new auto-quality preset (3).   (sent by the viewer)
app.backend_session - Received new video quality (20). / image quality (7).
desk_rt.auto_adjust - New preset: 2.
desk_rt.auto_adjust - Adjusting for a direct connection (997.84 kb/s).   (2nd session: 874.18 kb/s)
desk_rt.auto_adjust - Adjusting for 500-1.500 kb/s
```
- One estimate about **0.7 s** after the first frame, then a band, then **nothing more** in a
  15-minute session. Whether it adjusts inside the band without logging is unknown.
- The estimate is almost certainly what AnyDesk happened to send during start-up, not the link's
  capacity: the same link later carried 13 Mb/s from us.
- Published measurements: about 1.5 Mb/s during video playback and bursts up to about 8 Mb/s
  during heavy scrolling and window moving
  ([Windows Report](https://windowsreport.com/anydesk-vs-teamviewer-bandwidth/)); AnyDesk claims
  usability at 100 kb/s ([AnyDesk](https://anydesk.com/en/performance)).
- Its quality setting is Best quality / Balanced / Best performance
  ([display settings](https://support.anydesk.com/docs/display.md)).
- **The algorithm itself is unknown.** DeskRT is closed and the binary is packed. AnyDesk is a
  benchmark to measure against here, not a design to copy.

### 1.4 What AnyDesk's documentation adds
- "Allow direct connections": when off, sessions are routed through AnyDesk servers. Default port
  7070, can be changed. Sessions are end-to-end encrypted either way.
  ([settings](https://support.anydesk.com/docs/settings))
- Keyboard modes: Remote (physical key position), Local (local layout), Auto. Hotkeys use
  `Ctrl+Alt+Shift+<key>`, for example Del sends Ctrl+Alt+Del.
  ([keyboard](https://support.anydesk.com/docs/keyboard-hotkeys))
- 14 permissions in permission profiles (input, clipboard, files, lock on session end, privacy
  mode, …) ([permission profiles](https://support.anydesk.com/docs/permission-profiles))

### 1.5 What we take from AnyDesk
| Observed | Decision |
|---|---|
| One binary, roles by flag | `illum`, `--service`, `--backend`, `--control` |
| Per-session backend as SYSTEM in the console session | Same |
| Clipboard in the user-level control process | Same |
| Direct TCP, TLS 1.3, certificates on both sides | Same for control and input; **media moves to UDP** (override, §2 of the architecture) |
| Capability exchange, three-state permissions | Same |
| Desktop switcher: suspend, switch, restart DDA | Same |
| Explicit pixel format, separate cursor | Same |
| One early bandwidth estimate, then a band | **Improved on:** continuous adaptation in the user's own terms (latency limit) |
| Opus at ~80 kb/s | Same bitrate; 5 ms frames instead of 20 (for parity groups) |
| Wake lock during a session | Same |
| Relay link, connection switch, ID | **Not taken** (out of scope) |
| UPnP on the listening port | **Not taken** (refused) |

### 1.6 Still unverified
- How AnyDesk handles stuck keys and IME text during a live session.
- Its policy when several controllers connect at once.
- Its codec internals, its real maximum bandwidth on our link, and whether it adapts mid-session
  without logging it.

These don't block the design.

---

## 2. RustDesk: what went wrong, and what its adaptation does

Each lesson below has its evidence and a status for Illumination.

| # | Lesson | Evidence | Status |
|---|---|---|---|
| 1 | Video over a reliable stream suffers head-of-line blocking; rate control was an afterthought | Maintainers: "uses TCP", its rate control is "far away from real ABR/congestion control" ([#792](https://github.com/rustdesk/rustdesk/discussions/792)) | **Adopted in full (2026-10-22):** media over UDP, never resent; our own test reproduced the queueing problem under TCP (§4) |
| 2 | No real congestion control | Bitrate stuck near 600 KB/s, under 5 fps on a <10 ms link ([#6888](https://github.com/rustdesk/rustdesk/discussions/6888)) | **Adopted:** the latency controller (architecture §5.5) |
| 3 | Transport sprawl with untested failure paths | TCP/UDP punching, IPv6, relay, WebSocket, then WebRTC ([PR #15684](https://github.com/rustdesk/rustdesk/pull/15684)); UDP punching by default broke direct connections ([#12511](https://github.com/rustdesk/rustdesk/discussions/12511)) and hung on failure ([#12606](https://github.com/rustdesk/rustdesk/discussions/12606)); on our machine, 8–19 s from a failed punch to the relay | **Avoided by scope:** two paths on one port number, no punching, no relay. Every step still gets a deadline |
| 4 | Camera-video defaults used for screen content | Our log: `encoder: H265` and `i444: false` on every encoder, although the viewer asked for `prefer_chroma: I444`; peer saved at `custom-fps = '30'` | **Partly open:** 60 fps by default and sharp text are goals; chroma (4:2:0 vs 4:4:4) is an open question (architecture §13) |
| 5 | Too many colour-conversion paths, each with its own bugs | Chroma offset at odd heights ([#6964](https://github.com/rustdesk/rustdesk/issues/6964)), HDR overexposure ([#5368](https://github.com/rustdesk/rustdesk/issues/5368)), washed out ([#6448](https://github.com/rustdesk/rustdesk/issues/6448)), dark colours ([#14785](https://github.com/rustdesk/rustdesk/issues/14785)), blue-orange text ([#13005](https://github.com/rustdesk/rustdesk/discussions/13005)); our own red↔blue swap, which no setting fixed | **Adopted:** one pixel format, one conversion module, golden-image CI (§5.2) |
| 6 | Capture and presentation chosen as workarounds | "switched to GDI as a workaround" after a DXGI leak; 120 ms screen-to-screen on LAN ([#3979](https://github.com/rustdesk/rustdesk/discussions/3979)); video drawn through Flutter (`use texture render: true`) | **Adopted:** DDA first with fallbacks; flip-model presentation (§5.1, §5.4) |
| 7 | Latency never measured end to end | Users compare by feel | **Adopted:** per-stage timestamps, per-second grades in the log, CI benchmark (§11) |
| 8 | Bugs that can't be diagnosed | Issues closed as "I somehow fixed it" or converted to discussions (#13005, #14785) | **Adopted:** diagnostics bundle (§11) |

**RustDesk's adaptation, from `src/server/video_qos.rs`** (read 2026-10-22; the method is
documented at the top of the file):
- A probe (`TestDelay`) about once a second travels **with the video**, so it measures the queue
  in front of it: *"it measures the queue in front of it rather than the path RTT"*. It subtracts
  the lowest delay seen, so only the build-up counts.
- Excess delay **≥ 150 ms** is a bad reply. One bad reply does nothing; **2 in a row** lower the
  bitrate, **3 in a row** lower the frame rate by at most a fifth. An excess of a full second
  halves the frame rate immediately.
- Drop size scales with severity: bitrate × 0.95 (< 200 ms excess) down to × 0.5 (> 500 ms).
- It raises every 3 s if delay is good, **only if the screen is actually changing**; after a
  dip, two good replies return it straight to the level before the trouble.
- It starts **low** (15 fps) and doubles while replies are good. Bitrate is reduced before frame
  rate because *"bitrate-targeted encoders do not send fewer bytes at fewer frames"*.
- The file has no notion of packet loss: it runs over TCP or KCP, which resend everything.

What we took from it: bitrate before frame rate, "raise only with evidence" (our ⚪ seconds),
and the idea that a queue is the earliest sign of overload. What we didn't: its fixed 150 ms
threshold (ours is the user's own limit), its low start, and its severity-scaled multi-step
drops (we move one rung at a time).

What RustDesk got right and we keep:
- keys bound to device identity
- direct-by-IP mode (which works well with Tailscale)
- an open protocol
- reconnecting

---

## 3. Sunshine / Moonlight: what we took and what we didn't

From Sunshine's [configuration docs](https://docs.lizardbyte.dev/projects/sunshine/master/md_docs_2configuration.html),
its source (`src/stream.cpp`), moonlight-common-c (`ControlStream.c`, `VideoDepacketizer.c`,
`RtpAudioQueue.h`, `SdpGenerator.c`) and moonlight-qt (`streamingpreferences.cpp`,
`SettingsView.qml`, `pacer.cpp`).

**Input**
- **Key repeat generated on the host:** `key_repeat_delay` 500 ms, `key_repeat_frequency` 24.9 /s.
  Adopted (§6).
- `always_send_scancodes`, which Sunshine says "may result in incorrect keyboard input from …
  clients that aren't using a US English keyboard layout". This is why we add AnyDesk's
  Local/Auto modes and an IME text path.

**Transport and loss** (adopted, architecture §4.4 and §5.6)
- Video and audio over **UDP**, never resent; control on a separate connection. Ports: a family
  offset from a base (47984–48010 TCP/UDP). We use one port number for TCP and UDP.
- **FEC:** `fec_percentage`, default **20%**, fixed for the session. Moonlight notes the price:
  effective bitrate *"about 20% lower when using the standard 20% FEC configuration"*. Ours
  adapts between 5 and 30%.
- **Incomplete frames are dropped, never decoded:** the depacketizer calls `dropFrameState()`,
  logs `"Network dropped N frames"`, then `LiRequestIdrFrame()` or reference invalidation
  (`connectionDetectedFrameLoss`). This is why a frame is all-or-nothing in our design.
- **Audio:** 5 ms Opus packets (10 ms on some links), fixed parity groups of
  `RTPA_DATA_SHARDS 4` + `RTPA_FEC_SHARDS 2`. Adopted as is.
- **Video encryption is off by default on LAN** (`lan_encryption_mode` 0). We encrypt always.

**Adaptation** (not adopted: there is none)
- Moonlight's default bitrate comes from a table (1080p60 = 20 Mb/s, 1440p60 = 40, 4K60 = 80;
  half at 30 fps; ×2 for 4:4:4). `autoAdjustBitrate` only resets the slider when resolution or
  frame rate change in the settings screen; nothing changes during a stream.
- Connection status: every **3 s**, the share of **lost frames**; "poor" at ≥ 30% in one
  interval or ≥ 15% in two consecutive ones; "okay" again at ≤ 5%
  (`CONN_IMMEDIATE_POOR_LOSS_RATE`, `CONN_CONSECUTIVE_POOR_LOSS_RATE`, `CONN_OKAY_LOSS_RATE`).
  The only action is a warning: *"Slow connection to PC — Reduce your bitrate"*.
- We took the shape (a share over a window, different thresholds for getting worse and better)
  and the bitrate table for our top rung; the warning-only part is the gap we fill.

**Presentation**
- Moonlight's pacer keeps at most **1** queued frame (briefly 3) and drops the rest; frame pacing
  to V-sync is optional and **off** by default. Adopted: no added video delay.

**Licence:** `GPL-3.0-only` (Sunshine `package.json`). The same licence as ours, so code can be
ported if a file says so.

**Where Sunshine and AnyDesk differ, and who won:**
- transport for media: UDP + FEC (Sunshine) vs one TCP connection (AnyDesk) → **Sunshine**, after
  our own test (§4)
- session model: "launch an app" vs "join the live desktop" → AnyDesk
- cursor: drawn into the video vs sent separately → AnyDesk
- audio protection: fixed parity groups vs none visible → Sunshine
- bitrate: chosen by the user vs chosen by the app → neither; ours adapts

---

## 4. Our own environment and tests

**The reference setup**
- Two Windows 11 PCs on Tailscale. Direct path: `pong … via <public-ip>:41641 in 12ms`, ping
  11–13 ms, 0% loss when idle.
- Host (this PC): NVIDIA RTX 5070 Ti (driver 32.0.15.9597), two monitors (2560×1440 and
  1920×1080).
- Laptop: NVIDIA RTX 3050 Ti Laptop GPU plus Intel UHD Graphics.
- Keyboard layout: Chinese (Traditional), with an IME.

**Hardware codec support, which fixed the codec rule (architecture §5.3):**

| Codec | This PC encode | This PC decode | Laptop encode | Laptop decode |
|---|---|---|---|---|
| AV1 | ✅ | ✅ | ❌ (RTX 30 series has no AV1 encoder) | ✅ |
| H.265 | ✅ | ✅ | ✅ | ✅ |
| H.264 | ✅ | ✅ | ✅ | ✅ |
| VP9 | ❌ | ✅ | ❌ | ✅ |

**The first two-machine test (v0.4.0, 2026-10-19), laptop → this PC:** pairing worked on the
first try, typing in English and Chinese worked, Alt+Tab stayed on the laptop (fixed in v0.4.2),
small text looked broken (fixed in v0.4.2). The stream ran at **11 fps, about 13 Mb/s, about
100 ms frame latency**; `tailscale ping` rose from 12 ms idle to **48 ms** during the stream and
the in-app RTT swung between 10 and 75 ms. Nothing was lost; the data queued. This is the
measurement behind the UDP decision and the re-planned roadmap.

**Default port 28326** was checked on 2026-10-22: unassigned in the IANA registry (inside the
unassigned block 28241–28588), outside Windows' outgoing-port range (49152+), in no reserved
range on this PC, and used by no well-known program.

Milestone targets in [`../roadmap/roadmap.md`](../roadmap/roadmap.md) are measured on this setup.
