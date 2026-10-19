# Lessons and evidence

The evidence behind [`../architecture/architecture.md`](../architecture/architecture.md):
- **AnyDesk 9.7.14** and **RustDesk 1.4.9**, both installed on our own Windows 11 machines. The
  evidence was collected on 2026-10-16 and 2026-10-19 from logs, live sockets, config key names
  and one real AnyDesk session over Tailscale.
- Public documentation and GitHub discussions.

Personal values (device IDs, public IPs, keys) are redacted.

**Rule:** when sources disagree, AnyDesk's observed behaviour wins, except for relay/ID (out of
scope) and UPnP (refused). See architecture §2.

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

### 1.2 A direct session over Tailscale (2026-10-19)
The laptop dialled `100.100.101.1` and the host accepted. Service log:
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
- `100.100.101.1:7070 ← 100.100.101.2` (TCP, direct)
- the relay connection to `:443` stayed open, but nothing in the session log went through it

Backend log (`--backend`, SYSTEM in session 1):
```
win_app.desktop_switcher - Desktop change: --> Default / Suspending capture. / Restarting capture.
capture.fallback_capture - Switching to dda. / Selecting DDA capture.
capture.privacy_feature_dwm1 - Using surrogate.
app.backend_session - Remote caps: sound_src, sound_sink, keyboard_sink, keyboard2, switch_sides,
                      request_elevation, request_elevation_pw, clip_paste_files, clip_src_files, …
app.backend_session - Sending SAS (9) permissions (allowed supported)
app.backend_session - Sending clipboard files (11) permissions (enabled allowed supported)
app.backend_session - Sending privacy (12) permissions (disabled forbidden supported)
desk_rt.auto_adjust - Route type: direct (internet)
desk_rt.encoder     - 8 segments.
desk_rt.encoder     - Format changed to 1920x1080 (X8R8G8B8).
desk_rt.auto_adjust - Adjusting for a direct connection (997.84 kb/s). / Adjusting for 500-1.500 kb/s
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

### 1.3 What AnyDesk's documentation adds
- "Allow direct connections": when off, sessions are routed through AnyDesk servers. Default port
  7070, can be changed. Sessions are end-to-end encrypted either way.
  ([settings](https://support.anydesk.com/docs/settings))
- Keyboard modes: Remote (physical key position), Local (local layout), Auto. Hotkeys use
  `Ctrl+Alt+Shift+<key>`, for example Del sends Ctrl+Alt+Del.
  ([keyboard](https://support.anydesk.com/docs/keyboard-hotkeys))
- 14 permissions in permission profiles (input, clipboard, files, lock on session end, privacy
  mode, …) ([permission profiles](https://support.anydesk.com/docs/permission-profiles))

### 1.4 What we take from AnyDesk
| Observed | Decision |
|---|---|
| One binary, roles by flag | `illum`, `--service`, `--backend`, `--control` |
| Per-session backend as SYSTEM in the console session | Same |
| Clipboard in the user-level control process | Same |
| Direct TCP, TLS 1.3, certificates on both sides | Same, on port 7171 |
| Capability exchange, three-state permissions | Same |
| Desktop switcher: suspend, switch, restart DDA | Same |
| Segmented encoding, explicit pixel format, bandwidth bands, separate cursor | Same behaviour, our own implementation |
| Small send buffer, tuned at runtime | Same |
| Opus with 20 ms frames | Same |
| Wake lock during a session | Same |
| Relay link, connection switch, ID | **Not taken** (out of scope) |
| UPnP on the listening port | **Not taken** (refused) |

### 1.5 Still unverified
- How AnyDesk handles stuck keys and IME text during a live session.
- Its policy when several controllers connect at once.
- Its codec internals (DeskRT is closed).

These don't block the design. Our own rules are in architecture §6 and §10.

---

## 2. RustDesk: what went wrong

Each lesson below has its evidence and a status for Illumination.

| # | Lesson | Evidence | Status |
|---|---|---|---|
| 1 | Video over a reliable stream suffers head-of-line blocking; rate control was an afterthought | Maintainers: "uses TCP", its rate control is "far away from real ABR/congestion control" ([#792](https://github.com/rustdesk/rustdesk/discussions/792)) | **Partly superseded.** We use TCP, as AnyDesk does, but adopt the fix: small send buffer, latest frame wins, bandwidth bands (architecture §4.4, §5.4) |
| 2 | No real congestion control | Bitrate stuck near 600 KB/s, under 5 fps on a <10 ms link ([#6888](https://github.com/rustdesk/rustdesk/discussions/6888)) | **Adopted:** adaptation from day one (§5.4) |
| 3 | Transport sprawl with untested failure paths | TCP/UDP punching, IPv6, relay, WebSocket, then WebRTC ([PR #15684](https://github.com/rustdesk/rustdesk/pull/15684)); UDP punching by default broke direct connections ([#12511](https://github.com/rustdesk/rustdesk/discussions/12511)) and hung on failure ([#12606](https://github.com/rustdesk/rustdesk/discussions/12606)); on our machine, 8–19 s from a failed punch to the relay | **Avoided by scope:** one transport, no punching, no relay. Every step still gets a deadline |
| 4 | Camera-video defaults used for screen content | Our log: `encoder: H265` and `i444: false` on every encoder, although the viewer asked for `prefer_chroma: I444`; peer saved at `custom-fps = '30'` | **Adopted:** 4:4:4 when supported, refine static regions, honour the chroma request or say so (§5.3) |
| 5 | Too many colour-conversion paths, each with its own bugs | Chroma offset at odd heights ([#6964](https://github.com/rustdesk/rustdesk/issues/6964)), HDR overexposure ([#5368](https://github.com/rustdesk/rustdesk/issues/5368)), washed out ([#6448](https://github.com/rustdesk/rustdesk/issues/6448)), dark colours ([#14785](https://github.com/rustdesk/rustdesk/issues/14785)), blue-orange text ([#13005](https://github.com/rustdesk/rustdesk/discussions/13005)); our own red↔blue swap, which no setting fixed | **Adopted:** one pixel format, one conversion module, golden-image CI (§5.2) |
| 6 | Capture and presentation chosen as workarounds | "switched to GDI as a workaround" after a DXGI leak; 120 ms screen-to-screen on LAN ([#3979](https://github.com/rustdesk/rustdesk/discussions/3979)); video drawn through Flutter (`use texture render: true`) | **Adopted:** DDA first with fallbacks; flip-model presentation (§5.1, §5.5) |
| 7 | Latency never measured end to end | Users compare by feel | **Adopted:** per-stage timestamps, CI latency benchmark (§11) |
| 8 | Bugs that can't be diagnosed | Issues closed as "I somehow fixed it" or converted to discussions (#13005, #14785) | **Adopted:** diagnostics bundle (§11) |

What RustDesk got right and we keep:
- keys bound to device identity
- direct-by-IP mode (which works well with Tailscale)
- an open protocol
- reconnecting

---

## 3. Sunshine / Moonlight: useful parts

From Sunshine's [configuration docs](https://docs.lizardbyte.dev/projects/sunshine/master/md_docs_2configuration.html)
and [security docs](https://mintlify.com/LizardByte/Sunshine/deployment/security):
- **Key repeat generated on the host:** `key_repeat_delay` 500 ms, `key_repeat_frequency` 24.9 /s.
  Adopted (§6).
- `always_send_scancodes`, which Sunshine says "may result in incorrect keyboard input from …
  clients that aren't using a US English keyboard layout". This is why we add AnyDesk's
  Local/Auto modes and an IME text path.
- **Video encryption is off by default on LAN** (`lan_encryption_mode` 0). We encrypt always.
- Licence: `GPL-3.0-only` (Sunshine `package.json`). The same licence as ours, so code can be
  ported if a file says so.

**Where Sunshine and AnyDesk differ, AnyDesk wins:**
- transport: UDP + FEC (Sunshine) vs one TCP+TLS port (AnyDesk) → TCP
- session model: "launch an app" vs "join the live desktop" → live desktop
- cursor: drawn into the video vs sent separately → separate

---

## 4. Our own environment (the reference setup)

- Two Windows 11 PCs on Tailscale. Direct path: `pong … via <public-ip>:41641 in 12ms`, ping
  11–13 ms, 0% loss.
- Host GPU: NVIDIA RTX 5070 Ti (driver 32.0.15.9597), two monitors (2560×1440 and 1920×1080).
- Keyboard layout: Chinese (Traditional), with an IME.

Milestone targets in [`../roadmap/roadmap.md`](../roadmap/roadmap.md) are measured on this setup.
