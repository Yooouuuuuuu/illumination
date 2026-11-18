# Illumination docs

The map to everything in `docs/`, and the ledger of *why* the design changed over time. Each
document below is **present tense: what it is now**. How it got there is in the ledger at the
bottom, so the design documents do not grow revision logs and the root `CHANGELOG.md` can
stay about what shipped.

## Where things live

- **README** (repo root): what the program does and how to install and use it.
- **CHANGELOG.md** (repo root): what shipped, per version. No design reasoning.
- **This file**: the map and the design-decision ledger.

### Reference (kept true to the running program)

| Doc | What it is |
|---|---|
| [architecture.md](architecture/architecture.md) | The system as it exists: goals, the design rule, the processes and how they share state, diagnostics, the crates. Start here. |
| [future-work.md](future-work/future-work.md) | What is planned: a measured comparison with AnyDesk and RustDesk, then macOS and Linux. |

### Design records (one per subsystem, present tense)

| Doc | Sections | What it is |
|---|---|---|
| [network-and-trust.md](network-and-trust/network-and-trust.md) | §4, §9, §10 | Listening, keys and TLS, the handshake, the two paths (TLS over TCP for control, UDP for media), which desks may connect and what they may do, session policy. |
| [video.md](session/video.md) | §5.1–5.4, §5.8 | Capture, the pixel format, the codec choice, drawing the picture and the pointer, the settings. |
| [adaptation.md](adaptation/adaptation.md) | §5.5–5.7 | The latency controller and its quality ladder, parity instead of resending, how the numbers are tested. |
| [input.md](session/input.md) | §6, §7 | Keyboard modes and the keyboard grab, the mouse, the wire format; the lock screen and administrator prompts. |
| [clipboard-audio.md](session/clipboard-audio.md) | §8 | The shared text clipboard and the host's sound. |

**Section numbers.** The design was once one document with numbered sections, and code
comments cite them ("architecture §5.5"). The numbers are kept in the headings of the
documents above so those citations can still be followed; the table says where each lives.

### Diagrams

[D2](https://d2lang.com) files beside the documents: [processes.d2](architecture/processes.d2),
[session.d2](network-and-trust/session.d2),
[media-transport.d2](network-and-trust/media-transport.d2),
[video-pipeline.d2](session/video-pipeline.d2), [adaptation.d2](adaptation/adaptation.d2).
Render one with `d2 <file>.d2 <file>.svg`.

---

## Design-decision ledger

Brief, dated notes on the load-bearing decisions and the places the approach **changed**:
the archaeology the design documents leave out. Newest first.

- **2026-11 · Trust, simplified** ([network-and-trust.md](network-and-trust/network-and-trust.md),
  0.13.2–0.14.6). The planned AnyDesk-style model (supported / allowed / enabled states,
  interactive and unattended profiles, a password) was replaced by something smaller: a
  desk is one entry with a permission of always, ask or never, three switches say what a
  connected desk may do, and a setting says what a new desk meets. The question on the
  connecting side ("is this the right host key?") was removed: it was being clicked
  through, and on a tailnet the address already names the machine. The key is remembered
  silently and a later change is refused. The host's question grew to four answers: no,
  this time, always, never.

- **2026-11 · The service, as built** ([architecture.md](architecture/architecture.md),
  0.13.0–0.13.1). The plan had four roles (service, backend, control, frontend) talking over
  pipes. What was built is a service that keeps one host process alive in the console session,
  with state shared through files in the home folder. When that folder was given to the system
  on install, one pipe was added for the app's changes. A new desk's question is asked by `illum
  ask`, started as the signed-in person, so no window with system rights sits on a user's
  desktop.

- **2026-11 · The lock screen and administrator windows** ([input.md](session/input.md),
  0.12.6–0.14.3). Input that cannot arrive is named instead of silently lost; an
  administrator window in front is found by looking at the front process, because
  `SendInput` reports success either way. The host follows desktop switches and rides out
  a refused capture instead of freezing. Ctrl+Alt+Del was planned as a hotkey and built as
  a menu entry (the host raises an event, the service sends the sequence); Lock was added
  because Win+L, too, is taken by the viewer's own Windows.

- **2026-11 · The keyboard grab** ([input.md](session/input.md), 0.11.10–0.12.7). Remote mode
  with a full grab became the default after a review found Alt+Tab acting locally and the
  local IME doubling letters. Making the grab hold took four fixes: the hook on its own
  thread (Windows silently removes a hook that answers late), skipping only our own
  injected keys, asking Windows who is in front instead of trusting focus events, and the
  one that mattered most: the viewer must never ask for raw device events, which silences
  the hook. Key codes on the wire went from PC scancodes to neutral physical key codes,
  for macOS and Linux.

- **2026-11 · The pointer** ([video.md](session/video.md), 0.11.0–0.14.6). First drawn into the
  picture, then the viewer's own system pointer wearing the host's shape, then always the
  system pointer with a choice between this desk's pointers and the host's pictures. It
  never lags the picture because the local system moves it. The names settled on
  Local / Remote, Remote being the default.

- **2026-11 · Adaptation, tuned on real links** ([adaptation.md](adaptation/adaptation.md),
  0.6.0–0.11.8). The controller was decided on 2026-10-22 as a delay limit in the person's
  own terms, starting high and stepping down (AnyDesk estimates once, Moonlight never
  adapts, RustDesk starts low). Live runs then changed most of its rules: loss alone no
  longer walks the ladder down, only loss with a risen round trip counts as congestion; a
  failed raise backs off; evidence must be recent; a fast drop exists and is bounded; the
  clock offset follows the fastest probe of the last minute instead of one reading for the
  session. The default frame rate went from 60 to 30 and resolution steps became opt-in,
  because CPU scaling cost more than it saved.

- **2026-11 · Decode and draw on the GPU** ([video.md](session/video.md), 0.8.0). Decoders hand
  over their own planes and one shader converts; before, every frame was converted on the
  CPU and drawn by `softbuffer`, which remains as the fallback. AV1 is offered only when
  the viewer's GPU decodes it, after a laptop spent 20 ms a frame decoding it in software.

- **2026-10 · Media over UDP, never resent**
  ([network-and-trust.md](network-and-trust/network-and-trust.md),
  [adaptation.md](adaptation/adaptation.md), 0.5.0). Everything first went over one TLS
  connection, as AnyDesk does. The first two-machine test queued: 11 frames a second and about
  100 ms over a 12 ms link. Video and sound moved to UDP on the same port number, encrypted with
  keys exported from the TLS session, with parity to rebuild lost packets. QUIC was considered
  and set aside; a handshake of our own was rejected. Mouse moves followed in 0.11.2, after a
  1000 Hz mouse queued clicks behind its own moves.

- **2026-10 · Codecs through FFmpeg** ([video.md](session/video.md), 0.5.1). AV1 when both sides
  can, else H.265, else the lossless tiles that were the first codec. H.264 was left out
  and VP9 rejected (NVIDIA never encoded it in hardware). FFmpeg was chosen over Media
  Foundation and the NVENC SDK so the work is not written once per vendor and once per
  system. Chroma is 4:2:0 because hardware AV1 offers nothing else. No software AV1
  encoder.

- **2026-10 · Sound, the Sunshine way** ([clipboard-audio.md](session/clipboard-audio.md), 0.7.0).
  Opus at a fixed bitrate in 5 ms frames with parity and a fixed 40 ms buffer; no
  adaptation and no audio/video synchronisation. The clipboard (0.11.4) polls through a
  cross-platform library instead of a Windows listener, since the host has no window and
  the code should not be Windows-only.

- **Recurring · Direct only.** No ID server, no relay, no account, no UPnP. A desk is an
  IP address, and getting between networks is the network's job. Where AnyDesk's observed
  behaviour and a tidier idea disagree, AnyDesk's behaviour wins, except for these.

- **Recurring · Not only Windows.** The first system is Windows, and the choices are made
  so the others stay possible: FFmpeg's hardware acceleration, `wgpu`, `winit`, neutral
  key codes, a cross-platform clipboard and tray. Windows-only code is kept in its own
  crate.
