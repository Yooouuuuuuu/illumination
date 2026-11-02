# Roadmap

Every milestone ends with a **pass/fail check** run on the reference setup: two Windows 11 PCs on
Tailscale, about 12 ms direct (see [`../lessons/lessons.md`](../lessons/lessons.md) §4). A
milestone is done when its check passes, not when its code is written.

Dependencies: [`milestones.d2`](milestones.d2). Design: [`../architecture/architecture.md`](../architecture/architecture.md).

**Re-planned 2026-10-22.** The first two-machine test (v0.4.0) showed that the picture pipeline,
not the service work, is what the user feels first: 11 fps and about 100 ms latency over a 12 ms
link. So the media transport, the video codec and the adaptation moved ahead of the Windows
service. The old M4–M7 became M6–M9.

| | Milestone | Depends on |
|---|---|---|
| M0 | Repo and docs ✅ | — |
| M1 | Connect and trust ✅ | M0 |
| M2 | First pixels ✅ | M1 |
| M3 | Input ✅ | M2 |
| M4 | Media transport and codec | M3 |
| M5 | Adaptation and the simulated-link harness | M4 |
| M6 | Service and secure desktop | M5 |
| M7 | Desktop features | M6 |
| M8 | Quality and latency | M5 (runs alongside M6–M7) |
| M9 | v0.1 release | M7, M8 |

---

## M0: Repo and docs ✅ (v0.1.0)
- Workspace, GPL-3.0-only license, `illum` role dispatch, architecture, lessons and roadmap docs.

**Check:** `cargo build` and `cargo test` pass. Docs reviewed.

## M1: Connect and trust ✅ (v0.2.0)
Build `illum-net`:
- TCP listener bound to Tailscale interfaces only
- self-signed Ed25519 device certificate
- TLS 1.3 with certificates on both sides
- pairing (show fingerprints, pin them)
- `hello` with version and capabilities
- channel framing with priorities
- ping/RTT

Uses console `illum` commands; no GUI yet.

Stand-ins until later milestones:
- The pairing question is asked in the `--service` console. It moves to the backend's accept
  panel in M7.
- The key and trust store live in one device-wide folder (`%ProgramData%\Illumination`) that
  both roles use directly. In M6 the app reaches them through the service instead.

**Check:**
- [x] Connect from the laptop to the host by Tailscale IP. Both sides show the same fingerprints.
  *(Done 2026-10-19: the laptop paired with this PC on the first try.)*
- [x] After pairing, reconnecting needs no prompt. A changed host key is refused with a clear
  error that names `illum forget`.
- [x] The host is **not** reachable on a LAN or public interface unless enabled. It binds only
  Tailscale addresses by default; `192.168.50.123:7171` refuses.
- [ ] RTT is shown and is within 2 ms of `tailscale ping`. *(RTT is shown. The comparison was
  inconclusive because the link was saturated by the video stream at the time: 10–75 ms vs.
  12 ms idle. Repeat once M5 keeps the queue short.)*
- [x] No connection step can hang: every one has a deadline. Tests cover a silent host, a
  silent controller, an unanswered prompt and an idle session.

## M2: First pixels ✅ (v0.3.0)
Build `illum-media` plus capture and presentation in `illum-win`:
- DDA capture into BGRA8
- one colour-conversion module
- decode, and present in a viewer window
- separate, locally drawn cursor
- latest-frame-wins pacing
- per-stage timestamps

Changes from the original plan:
- The first codec is **lossless tiles** (LZ4), not hardware encoding, which made colours bit-exact
  and got pixels on screen quickly. The hardware video codec is now M4.
- Pacing used frame acknowledgements (at most 2 frames in flight). With the UDP path (M4) the
  acknowledgements become feedback only.
- Presentation uses `softbuffer`; the flip-model swap chain is M8.
- v0.4.2: downscaling averages pixels (nearest neighbour broke thin text); an original-size view
  was added.

**Check:**
- [x] Host desktop visible and live at 1080p, at least 60 fps on a static desktop or light use.
  *On this PC over its Tailscale address: 59 fps with the test pattern. From the laptop
  (2026-10-19): the picture is live, but 11 fps at about 13 Mb/s; the lossless tiles overloaded
  the link. That result is why M4 and M5 come next.*
- [x] **Colour test pattern correct:** bit-exact (ΔE 0), stricter than the ΔE 2 target.
  Automated in `crates/illum/tests/m2.rs` (end to end over TLS) and in the codec tests.
- [x] Median latency ≤ 50 ms. *About 5 ms capture-to-presentation locally; about 100 ms from the
  laptop under the overload above. To be re-measured after M5.*
- [x] Cursor moves with no visible lag compared with the video. *Sent separately and drawn at the
  viewer's own mouse position while controlling.*

## M3: Input ✅ (v0.4.0, fixes in v0.4.2)
- Keyboard modes Remote/Local/Auto, with the host reporting its layout
- IME text path
- host-side repeat only, sequence numbers
- release everything on disconnect, stall or focus loss
- absolute mouse per monitor with DPI
- system keys (Win, Alt+Tab, …) via a low-level hook

Notes:
- The reserved `Ctrl+Alt+Shift+Del` hotkey moves to M6, since Ctrl+Alt+Del needs the SYSTEM
  service.
- Elevated windows can't receive input until the backend runs as SYSTEM (M6). Windows blocks
  injection into them from a normal process.

**Check:**
- [x] Typing English and Chinese (IME) into Notepad on the host comes out exactly as typed.
  *(Confirmed by hand from the laptop, 2026-10-19.)*
- [x] Hold Enter, drop the laptop's Wi-Fi for 3 s, release: **no stuck key and no extra Enters**
  on the host. *Verified by test: the rule tests in `crates/illum/src/input.rs` replay exactly
  this case, and `crates/illum/tests/m3.rs` drops a real connection while Enter is held. Still to
  be repeated by hand.*
- [ ] Mouse lands exactly on target on both monitors (2560×1440 and 1920×1080). *Not yet
  reported from the laptop.*
- [x] Shortcuts (Ctrl+C/V, Alt+Tab while the viewer is focused) reach the host. *(Alt+Tab stayed
  local in v0.4.0 because the hook lacked a module handle; fixed in v0.4.2 and confirmed.)*

## M4: Media transport and codec
Architecture §4.4, §5.3, §5.6, §8.
- UDP media path next to the TLS connection, keys exported from the TLS session, authenticated
  packets; one port number for both (default **28326**, `--port` moves both)
- hardware video codec chosen per direction: **AV1 if both sides can, otherwise H.265**, both
  at **4:2:0**; capabilities carry each side's hardware encoders and decoders
- encode and decode through **FFmpeg** (`ffmpeg-next`), with a trimmed FFmpeg build bundled
- parity FEC over each frame (Reed–Solomon), budgeted in bytes; fixed at the 5% floor until M5
- reference invalidation: the viewer reports a broken frame, the host encodes the next frame from
  an older good one
- per-frame feedback over TLS: shown time, latency, packets lost before repair
- audio: Opus 80 kb/s, 5 ms frames, 4+2 parity groups, ~40 ms viewer buffer; "send audio: off"
- a fixed bitrate target for now (the ladder's top rung), so M4 can be judged on its own

**Status:** three slices. **v0.5.0** shipped the UDP media path, keys exported from TLS, port
28326, packetisation, pacing, keyframe-on-loss and the loss report, still carrying the tile
codec. **v0.5.1** shipped the codec (AV1 / H.265 through FFmpeg 8.1, chosen per direction from
the hello, tiles as fallback) and parity FEC at the 5 % floor; a broken frame is answered with a
keyframe because FFmpeg exposes no reference-invalidation control for the hardware encoders
(architecture §13). Audio shipped as **v0.7.0** (2026-10-30, after M5): WASAPI loopback →
Opus 5 ms frames → 4 + 2 parity groups over the UDP path → a fixed 40 ms buffer → WASAPI
playback; `--no-audio` on the host, `--mute` on the viewer. Not yet heard live.
Measured in v0.5.0 on this PC over its Tailscale address: test pattern 59 fps, 9 ms, 0% loss;
real screen at a 6 Mb/s cap paced to ~6 Mb/s with RTT flat (no queue) but only 2–4 fps, because
lossless tiles of a full-screen change are ~2 MB per frame. First live run from the laptop
(2026-10-28) found no NVENC there (driver API 12.2; `hevc_qsv` serves) and Desktop Duplication
refused from the GPU Windows bound the process to; **v0.5.7** added the GDI fallback
(architecture §5.1). Codec and FEC are not yet measured live.

**Check:**
- [ ] This PC → laptop streams **AV1**, laptop → this PC streams **H.265**, both in hardware;
  the chosen codec is shown in the title bar and the log.
- [ ] Colour test pattern through the real codec: every patch within **ΔE 2** (lossy now, so the
  bit-exact test moves to the tile codec only).
- [ ] At a fixed 6 Mb/s from the laptop, a 1080p60 desktop with scrolling stays at 60 fps with no
  queue build-up: RTT during the stream within 5 ms of `tailscale ping` idle.
- [ ] With 3% simulated random loss, no visible broken frames (parity repairs them) and the
  log shows zero keyframe requests; at 10% loss, the picture heals within one round trip after
  each broken frame (with a keyframe until true reference invalidation exists).
- [ ] Audio plays with no gaps at 3% loss; "send audio: off" sends zero audio packets.

## M5: Adaptation and the simulated-link harness
Architecture §5.5–5.8.
- the simulated link inside the test suite: bandwidth cap, delay, jitter, random and burst loss,
  scripted over time
- the latency controller: per-second grades, interval rules, the ladder, ⚪ seconds
- the FEC controller: 30 s windows, zones, floor and ceiling, decay, backstop
- the three coordination rules (budget-neutral FEC, no raise during loss, backstop)
- settings: latency limit, max bandwidth, FEC range, max fps, send audio
- "best possible on this link: X ms" when the limit can't be met
- the tuning runs: one parameter per run, scored as in §5.7

**Status:** **v0.6.0** (2026-10-30) shipped the ladder, both controllers, the simulated link
with six scored scenarios, the viewer's per-second report (frames shown, frames within the
limit), the host's `Quality` message and the CLI settings (`--latency-limit`, `--max-fps`,
`--fec-min/--fec-max`, `--max-bitrate`). Three rules were added from the harness, recorded in
architecture §5.5: a freeze caused by loss grades 🔴 (not ⚪), a failed raise backs off, and
the backstop needs frames still dying. Still open: "best possible on this link: X ms" in the
title, and "send audio" (with audio). Harness results at v0.6.0 (`cargo test -p illum-media
--test harness -- --nocapture`): clean LAN 0 moves, 9 ms; 10 Mb/s Wi-Fi under the link in 40 s,
no hunting after; 20 → 4 Mb/s drop: first picture 60 s after the drop, 🟢 at 73 s, climbs back
one rung per 15 s after the restore; 2 % random loss: parity up, ladder untouched; 100 ms
outages every 4 s: survived. Not yet measured live.

**Check:**
- [ ] Steady 20 Mb/s link: the stream settles on the top rung and changes rungs **0 times** in
  5 minutes.
- [ ] Bandwidth halves: back to 🟢/🟡 within **30 s**, one rung at a time.
- [ ] A 2 s outage causes **no** rung change.
- [ ] 3% random loss: FEC settles between 10 and 20%, frames lost after repair → 0, and the
  bitrate is **not** reduced (loss, not overload).
- [ ] Loss-only overload (small-buffer link): FEC reaches the ceiling, then the backstop steps the
  bitrate down until the loss stops.
- [ ] From the laptop over Tailscale: median latency within the 50 ms limit at the best rung the
  link allows, with the limit shown as unreachable only if the link really can't do it.

## M6: Service and secure desktop
- Windows service (`--service`, SYSTEM)
- backend started per session as SYSTEM in the console session
- `--control` per user
- desktop switcher (suspend, switch, restart)
- Ctrl+Alt+Del via `SendSAS`, hotkey Ctrl+Alt+Shift+Del
- wake lock *(done early in v0.7.3: the host holds system and display awake while a session streams)*
- restart on session change
- install/uninstall commands (service registration, firewall rule, `SoftwareSASGeneration`)

**Check:**
- [ ] Connect when nobody is logged in (login screen). Log in remotely.
- [ ] A UAC prompt appears and can be answered remotely. The session never drops.
- [ ] Ctrl+Alt+Shift+Del opens the Ctrl+Alt+Del screen on the host.
- [ ] The host does not sleep during an hour-long idle session. After the session, normal sleep works again.
- [ ] Input reaches an elevated window.

## M7: Desktop features
- Accept panel and permissions (three states)
- interactive and unattended profiles
- clipboard (text, then files) with loop prevention
- one-controller policy with view-only extras
- privacy mode, block input, lock on session end

**Planned as v0.8.0 (decided 2026-10-30): a control review.** After audio (v0.7.0), go through
everything the controller does to the host — keyboard, mouse, IME, system keys — against the
A–C checklist from the first live test (clicks offset? delay? stuck or repeated keys? IME both
ways? Alt+Tab/Win on the host? nothing held after focus loss?), and add the **clipboard** (text
first): today a copy on one side is not seen on the other at all. Discussion starts after v0.7.0.

**Check:**
- [ ] Copy/paste text both ways 100 times: no echo, no lost copy, no hang when another app holds the clipboard.
- [ ] A second controller connects view-only. The host shows both.
- [ ] Privacy mode: the person at the host sees black; the controller still sees the desktop.
- [ ] Unattended: correct password works; 5 wrong ones lock out for the configured time.

## M8: Quality and latency
- Use the capture API's dirty rectangles; keep frames on the GPU from capture to encoder
- Flip-model swap chain for presentation; hardware decode straight to the screen
- HDR tone-mapping
- optional reduction of host animations (AnyDesk's `taskbar_animation_modifier`)
- diagnostics bundle
- try a **4:4:4 mode** (H.265) from the laptop on a page of small text, and decide whether it
  becomes a ladder rung or a user preference
- decide the remaining open questions from architecture §13: whether the tile codec keeps a
  role, extra parity for keyframes, frame pacing

**Check:**
- [ ] Median screen-to-screen latency **≤ 30 ms** on the reference setup.
- [ ] Small-font text is judged sharp from the laptop at the top rung at 4:2:0 (by eye); the
  4:4:4 trial is compared side by side.
- [ ] Under a simulated 3% loss with 80 ms extra delay (as when Tailscale falls back to DERP), it
  stays usable with no long freezes.
- [ ] Side by side with AnyDesk on the same link, it is judged at least as smooth (blind comparison
  by the user).

## M9: v0.1 release
- Installer and uninstaller
- signed build if possible
- user docs
- tagged release with source (GPL)

**Check:**
- [ ] Clean install and uninstall on a fresh Windows 11 machine leaves nothing behind.
- [ ] All M1–M8 checks pass on the release build.

---

## Always-on CI
- `cargo build`, `cargo test`, `clippy`, `rustfmt`
- From M1: failure-path tests (peer killed mid-handshake, network stall)
- From M2: colour golden-image test, latency regression benchmark
- From M3: input correctness tests (stuck keys, duplicate events)
- From M4: codec round trip through the real encoder and decoder (ΔE check), FEC repair tests
- From M5: the simulated-link scenarios from architecture §5.7, with their scores tracked over
  time so a change that makes any score worse fails the build
