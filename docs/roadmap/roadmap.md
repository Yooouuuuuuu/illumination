# Roadmap

Every milestone ends with a **pass/fail check** run on the reference setup: two Windows 11 PCs on
Tailscale, about 12 ms direct (see [`../lessons/lessons.md`](../lessons/lessons.md) §4). A
milestone is done when its check passes, not when its code is written.

Dependencies: [`milestones.d2`](milestones.d2). Design: [`../architecture/architecture.md`](../architecture/architecture.md).

| | Milestone | Depends on |
|---|---|---|
| M0 | Repo and docs | — |
| M1 | Connect and trust | M0 |
| M2 | First pixels | M1 |
| M3 | Input | M2 |
| M4 | Service and secure desktop | M3 |
| M5 | Desktop features | M4 |
| M6 | Quality and latency | M2 (runs alongside M3–M5) |
| M7 | v0.1 release | M5, M6 |

---

## M0: Repo and docs ✅ (v0.1.0)
- Workspace, GPL-3.0-only license, `illum` role dispatch, architecture, lessons and roadmap docs.

**Check:** `cargo build` and `cargo test` pass. Docs reviewed.

## M1: Connect and trust
Build `illum-net`:
- TCP listener bound to Tailscale interfaces only, default port 7171
- self-signed Ed25519 device certificate
- TLS 1.3 with certificates on both sides
- pairing (show fingerprints, pin them)
- `hello` with version and capabilities
- channel framing with priorities
- ping/RTT

Uses console `illum` commands; no GUI yet.

**Status (v0.2.0):** implemented. The checks pass on one machine, both in tests and over its
own Tailscale address. The cross-device run from the laptop is still to do.

Stand-ins until later milestones:
- The pairing question is asked in the `--service` console. It moves to the backend's accept
  panel in M5.
- The key and trust store live in one device-wide folder (`%ProgramData%\Illumination`) that
  both roles use directly. In M4 the app reaches them through the service instead.

**Check:**
- [ ] Connect from the laptop to the host by Tailscale IP. Both sides show the same fingerprints.
  *(Same-machine run over the Tailscale address: ✅. Laptop: to do.)*
- [x] After pairing, reconnecting needs no prompt. A changed host key is refused with a clear
  error that names `illum forget`.
- [x] The host is **not** reachable on a LAN or public interface unless enabled. It binds only
  Tailscale addresses by default; `192.168.50.123:7171` refuses.
- [ ] RTT is shown and is within 2 ms of `tailscale ping`. *(RTT is shown. Comparing with
  `tailscale ping` needs the laptop.)*
- [x] No connection step can hang: every one has a deadline. Tests cover a silent host, a
  silent controller, an unanswered prompt and an idle session.

## M2: First pixels
Build `illum-media` plus capture and presentation in `illum-win`:
- DDA capture into BGRA8
- one colour-conversion module
- hardware encode (4:4:4 if supported)
- decode, and present in a viewer window
- separate, locally drawn cursor
- latest-frame-wins pacing and a small send buffer
- per-stage timestamps

**Status (v0.3.0):** implemented.

Changes from the plan above:
- The first codec is **lossless tiles** (LZ4), not hardware encoding. It follows the AnyDesk rule
  (DeskRT is a segmented screen codec) and makes colours bit-exact. Hardware H.264/HEVC moves to
  M6 for fast-moving regions.
- Pacing uses frame acknowledgements (at most 2 frames in flight) instead of tuning the socket
  buffer.
- Presentation uses `softbuffer`; the flip-model swap chain moves to M6.

Not yet checked: the run from the laptop and the cursor check by eye.

**Check:**
- [x] Host desktop visible and live at 1080p, at least 60 fps on a static desktop or light use.
  *Measured on this PC over its Tailscale address: 59 fps with the 60 fps test pattern
  (0.3 Mb/s). The real desktop streams live. When the viewer shows its own screen, the whole
  frame changes every time and it reaches 15–36 fps at about 18 Mb/s. Laptop run: to do.*
- [x] **Colour test pattern correct:** bit-exact (ΔE 0), stricter than the ΔE 2 target.
  Automated in `crates/illum/tests/m2.rs` (end to end over TLS) and in the codec tests.
- [x] Median latency ≤ 50 ms. *Measured about 5 ms from capture to presentation, with the clocks
  synced over the control channel. That excludes the wait for the next screen update and the
  monitor's own scan-out. A true screen-to-screen measurement from the laptop is still to do.*
- [ ] Cursor moves with no visible lag compared with the video. *It is sent separately and drawn
  by the viewer. Still to be checked by eye.*

## M3: Input
- Keyboard modes Remote/Local/Auto, with the host reporting its layout
- IME text path
- host-side repeat only, sequence numbers
- release everything on disconnect, stall or focus loss
- absolute mouse per monitor with DPI
- reserved `Ctrl+Alt+Shift` hotkeys

**Check:**
- [ ] Typing English and Chinese (IME) into Notepad on the host comes out exactly as typed.
- [ ] Hold Enter, drop the laptop's Wi-Fi for 3 s, release: **no stuck key and no extra Enters** on the host.
- [ ] Mouse lands exactly on target on both monitors (2560×1440 and 1920×1080).
- [ ] Shortcuts (Ctrl+C/V, Alt+Tab while the viewer is focused) reach the host.

## M4: Service and secure desktop
- Windows service (`--service`, SYSTEM)
- backend started per session as SYSTEM in the console session
- `--control` per user
- desktop switcher (suspend, switch, restart)
- Ctrl+Alt+Del via `SendSAS`
- wake lock
- restart on session change

**Check:**
- [ ] Connect when nobody is logged in (login screen). Log in remotely.
- [ ] A UAC prompt appears and can be answered remotely. The session never drops.
- [ ] Ctrl+Alt+Shift+Del opens the Ctrl+Alt+Del screen on the host.
- [ ] The host does not sleep during an hour-long idle session. After the session, normal sleep works again.

## M5: Desktop features
- Accept panel and permissions (three states)
- interactive and unattended profiles
- clipboard (text, then files) with loop prevention
- audio (Opus, 20 ms)
- one-controller policy with view-only extras
- privacy mode, block input, lock on session end

**Check:**
- [ ] Copy/paste text both ways 100 times: no echo, no lost copy, no hang when another app holds the clipboard.
- [ ] A second controller connects view-only. The host shows both.
- [ ] Privacy mode: the person at the host sees black; the controller still sees the desktop.
- [ ] Unattended: correct password works; 5 wrong ones lock out for the configured time.

## M6: Quality and latency
- Hardware H.264/HEVC (4:4:4 where supported) for regions that change every frame, next to the lossless tiles
- Use the capture API's dirty rectangles instead of comparing whole frames; keep frames on the GPU
- Flip-model swap chain for presentation
- Segmented encoding with skipped unchanged segments
- full-quality refinement of static regions
- bandwidth measurement and preset bands
- HDR tone-mapping
- optional reduction of host animations
- diagnostics bundle

**Check:**
- [ ] Median screen-to-screen latency **≤ 30 ms** on the reference setup.
- [ ] Text sharpness: a small-font text screenshot matches the host pixel for pixel once the screen is static.
- [ ] Under a simulated 3% loss with 80 ms extra delay (as when Tailscale falls back to DERP), it stays usable with no long freezes.
- [ ] Side by side with AnyDesk on the same link, it is judged at least as smooth (blind comparison by the user).

## M7: v0.1 release
- Installer (service registration, firewall rule bound to the Tailscale interface, `SoftwareSASGeneration`)
- uninstaller
- signed build if possible
- user docs
- tagged release with source (GPL)

**Check:**
- [ ] Clean install and uninstall on a fresh Windows 11 machine leaves nothing behind.
- [ ] All M1–M6 checks pass on the release build.

---

## Always-on CI
- `cargo build`, `cargo test`, `clippy`, `rustfmt`
- From M2: colour golden-image test, latency regression benchmark
- From M3: input correctness tests (stuck keys, duplicate events)
- From M1: failure-path tests (peer killed mid-handshake, network stall)
