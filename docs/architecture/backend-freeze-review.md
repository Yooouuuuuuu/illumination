# Backend freeze review (before v0.12)

Written 2026-11-03, after the control review (v0.11.0–v0.11.4). This is a decision document:
it inventories what the two ends of a connection actually exchange, says what is ready to
freeze, and lists the few things we must decide first. We work through section 6 together, fold
the answers into [architecture.md](architecture.md), and cut the freeze as v0.12.

## 1. What "freeze the backend" really means

The backend is two different things, and only one of them is expensive to change later:

- **The wire** — everything two `illum` builds send each other: the protocol-version gate, the
  TLS and keying scheme, the framing, the channels, the set of control messages and what they
  mean, and the datagram formats for video, audio, input, cursor and clipboard. Once other
  people (and other operating systems) run the app, changing any of this means every end must
  update together. **This is what a freeze pins down.**
- **The inside** — how many processes there are, which thread does what, which OS module is
  called, what the window looks like. A remote peer never sees this. It can change freely after
  the freeze, as long as the wire already carries the concepts it needs.

So the review's real question is not "is every feature built" — it is **"does the wire already
carry everything v1 needs, so that the big unbuilt pieces (the service split, the secure
desktop, the installer, the other-OS layers) are all inside and additive?"**

One sharp consequence decides most of this. The protocol version is checked **strict-equal**
(`hello.protocol == PROTOCOL_VERSION`), and `ControlMsg` is a tagged enum whose decoder **errors
on a message type it does not know**. So today, within one protocol version the message set is
fixed, and any new message is a breaking version bump. That is fine for our two machines but not
for a released product. See **D0**.

## 2. Inventory: the wire as it stands, and its freeze status

| Surface | Where | Status |
|---|---|---|
| TLS 1.3 mutual + key-fingerprint pinning + pairing (trust store, console prompt) | `illum-net::session`, `illum-core::store` | **Freeze.** Works since M1–M3. The prompt UI changes; the scheme does not. |
| Media keying: RFC 5705 exporter, 72 bytes → two AES-256-GCM keys + session id, per-stream nonce, 256-packet replay window | `illum-net::media` | **Freeze.** |
| Framing: `[channel u8][len u32 BE][payload]`, 8 MiB cap | `illum-net::frame` | **Freeze.** |
| Channels: Control, Input, Cursor, Video, Audio, Clipboard, Files (+ send priorities) | `illum-core::protocol::Channel` | **Freeze.** Files is reserved and unused; that is fine. |
| Media datagram header: magic, stream (0 punch / 1 video / 2 audio / 3 input), session id, per-stream seq | `illum-net::media` | **Freeze.** |
| `Hello { protocol, app_version, device_name, capabilities }` | `illum-core::protocol` | **Freeze the shape.** One possible addition: the host's display list (**D4**). |
| Capability negotiation (string list, a feature is used only if both offer it; codec caps added at runtime) | `Hello::common_capabilities` | **Freeze.** Growing the *list* is safe and additive; both-must-offer already gives forward-compat for features. |
| Input sub-protocol: USB HID usages, text, move, button+pos, wheel+pos, heartbeat, release-all | `illum-core::input` | **Freeze.** Just rebuilt in v0.11.1–v0.11.2. |
| Cursor sub-protocol: position, shape (LZ4 BGRA) | `illum-media::cursor` | **Freeze.** |
| Clipboard sub-protocol: text (kind byte 1; files add a kind later, additively) | `illum-core::clipboard` | **Freeze.** |
| Feedback messages: `MediaReport`, `HostStats`, `Quality`, `VideoAck`, `FrameLost`, `TimeRequest/Reply`, `InputAck`, `KeyboardLayout` | `illum-core::protocol` | **Freeze the shapes.** These are telemetry/feedback and already tolerate added fields via serde defaults. |
| `ScreenPause { paused }` — pause the screen while the viewer is minimized (v0.11.6, §5.4) | `illum-core::protocol` | **Freeze.** Added pre-freeze; audio/input/connection keep going while paused. |

Everything in this table is either done and stable, or (Hello/capabilities) stable in shape. The
media kernel behind it — transport, FEC, pacer, the two adaptation controllers and the harness,
the codec layer, GPU decode and the wgpu present path, audio — is the most exercised part of the
system and does not need to move for anything downstream.

## 3. The one wire-level gap: authorization and permissions

This is the only place where the wire is **not** ready, and it is the reason we cannot honestly
freeze today.

**What happens now:** authorization is pairing only. A device is paired once (fingerprint
compared, `y/N` at the console), and from then on it connects and receives **everything** —
screen, keyboard and mouse, clipboard, audio — every session, with no further gate. The
handshake's only authorization messages are `Accepted` / `Denied` (one bit). There is **no
"granted permissions" message**, although §4.3 step 5 and §9 both call for one. `--view-only` is
honoured only because the viewer chooses not to open an input sender; **the host does not enforce
it** — it starts the input injector for any paired peer and injects any `Input` frame it
receives. There is no unattended password, no per-permission grant, no interactive accept panel,
and no mid-session change.

Under the current threat model (direct IP over a private Tailscale tailnet, explicit pairing)
this is not a security hole so much as a missing product surface. But the **shape of the
permission exchange is a wire decision**, and §9 already specifies the model we want (three
states — supported / allowed / enabled; a permission table; interactive vs unattended profiles).
If we freeze without it, adding it later is a breaking protocol bump for everyone.

## 3b. Multiple displays (added to the v0.12 scope, 2026-11-03)

We built single-display streaming and skipped the multi-monitor case. What exists: the host
enumerates every display (`illum-win::DisplayInfo { index, x, y, width, height, primary }`,
`illum displays`), and `ScreenStart { display }` picks one. Re-sending `ScreenStart` with a
different index already switches the host to another monitor — it tears the stream down and
rebuilds it (a brief black), which matches the "fixed at connect, a change restarts the stream"
rule (§5.8).

What is missing, and which of it is **wire**:

- **The viewer is blind.** The host's display list never reaches the viewer, so `--screen N` is a
  guess and no picker can exist. The list must go **on the wire** (host → viewer): a new
  `Displays` control message carrying, per display, the index, geometry (x, y, width, height in
  the virtual desktop), the scale/DPI, whether it is primary, and a name. This is the wire piece
  of multi-screen and the one part that must land before the freeze. The OS-independent struct
  lives in `illum-core`; `illum-win::DisplayInfo` maps onto it (and gains scale + name).
- **Switching** is already a re-sent `ScreenStart`; no new message. The hotkey
  (Ctrl+Alt+Shift+arrows, already reserved in §6) and the picker are v0.13 UI that just send it.
- **An all-monitors / spanning view** (one frame across every monitor, as RDP and AnyDesk offer)
  is not supported — DDA duplicates one output at a time, so it needs capture-side compositing.
  Whether v1 wants it is **D8**. If deferred, reserve a sentinel `display` value now
  (`u32::MAX` = the whole virtual desktop) so adding it later is capture work with no wire change.

So multi-screen adds exactly one message to the freeze (the display list) plus one reserved
value; the rest is the restart-based switch we already have, plus later UI.

## 4. Deferred, and confirmed not to reopen the wire

These are the big unbuilt pieces. Each was checked against the wire; none forces a wire change,
given the decisions in section 6.

- **The process/privilege split (M6): `--service` as SYSTEM, `--backend` in the console session,
  `--control` as the user.** Today everything runs in one user process; `--backend` and
  `--control` `bail!("not implemented")`. This is the heaviest remaining backend work, but it is
  **internal**: the remote peer still sees one TLS connection with the same channels. The seams
  are already clean — the clipboard watcher is self-contained (moves to `--control`), the input
  sink and the capture source are traits (move to `--backend`). The only genuinely new plumbing
  is local IPC (named pipes) and which process owns the socket. **Does not reopen the wire.**
- **Secure desktop / UAC / lock screen / Ctrl+Alt+Del / run-before-login.** All inside the
  service and backend. Ctrl+Alt+Del is a permission flag plus `SendSAS`, not a new message.
  **Does not reopen the wire** (Ctrl+Alt+Del and the rest are reserved in the permission set,
  **D1**). The user-visible gap today: you cannot reach a locked or just-rebooted machine, or a
  UAC prompt, because a non-elevated process cannot inject there.
- **Reference invalidation.** Still the keyframe-on-loss stand-in. True invalidation would need
  to know which frames the viewer has — and `VideoAck(seq)` already tells the host that. So a
  future version can build the invalidation list from acks it already receives. **No wire change
  needed; keep deferred** (**D6**).
- **Files** (clipboard files, file transfer). The `Files` channel is reserved and clipboard
  files are an added kind byte. Additive. Deferred.
- **One-controller policy, privacy mode, block input, lock-on-end, wake lock (§10).** Host-side
  policy and permission flags; the wire already allows multiple connections. Reserved in the
  permission set. Deferred.

## 5. Smaller things to confirm

- **Reconnect after a network blip.** Not built; a timed-out session just ends. Resuming media
  after a TCP reconnect would want a *resume token* on the wire — the one deferral here with a
  possible wire implication. Recommend consciously excluding it from v1 (**D5**) so we are not
  surprised later.
- **Multiple displays** — now in scope; see §3b. The display list is the only new wire piece.
- **Config versioning.** §9 wants per-device settings versioned from day one; the trust store is
  already versioned (`store::wrong_version_is_refused`). Extend the same discipline to any
  per-device permission settings we add.

## 6. Decisions to make (the discussion agenda)

Answer these, I fold them into architecture.md, and we build what must be on the wire. The rest
becomes v0.13+.

- **D0 — Make the control decoder forward-tolerant.** Ignore an unknown `ControlMsg` type instead
  of erroring, so the protocol can grow additively after the freeze and a later message is not a
  breaking bump. *Recommend: yes.* This is what makes a freeze meaningful rather than final.
- **D1 — Granted-permissions message, and enforce it host-side.** Add a host→viewer message that
  states the permissions in force, define the set now (view screen, control, clipboard-text,
  clipboard-files, audio, Ctrl+Alt+Del, block-input, privacy, lock-on-end), and at minimum
  **enforce view-only on the host** (do not inject input unless control is granted). The accept
  *panel* and the fuller states are UI/policy for later; the *message* must exist at the freeze.
  *Recommend: yes, define the set now.*
- **D2 — v1 authorization model.** Pairing counts as unattended-allow for v1 (fits the tailnet +
  pairing threat model); the interactive accept panel is v0.13 UI, with its messages reserved
  now. *Recommend: yes.*
- **D3 — Unattended password.** Decide the shape now (salted hash, lockout after repeated
  failures, carried in the handshake), implement later. *Recommend: reserve the shape, defer the
  code.*
- **D4 — Host display list on the wire (required for multi-screen, §3b).** A `Displays` message,
  host → viewer, with per-display index, geometry, scale, primary flag and name, so the viewer
  can map coordinates and a picker can exist. Switching monitors stays a re-sent `ScreenStart`
  (a brief restart); the hotkey and picker are v0.13 UI on top. *Recommend: build for v0.12.*
- **D8 — All-monitors / spanning view (§3b).** Defer from v1; reserve `display = u32::MAX` as
  "whole virtual desktop" so capture-side compositing can be added later with no wire change.
  *Recommend: confirm deferred and reserved.*
- **D5 — Reconnect/resume.** Exclude from v1; no resume token on the wire. *Recommend: confirm
  excluded.*
- **D6 — Reference invalidation.** Keep keyframe-on-loss; no wire change; a future version reuses
  `VideoAck`. *Recommend: confirm.*
- **D7 — Mid-session permission change.** With D0 in, this is a later additive message and needs
  no reservation now. *Recommend: defer, rely on D0.*

## 7. What freezes at v0.12

With **D0**, **D1** and **D4** built (D4 is now required, for multi-screen), the wire is complete
for v1: the version gate, TLS and keying, framing, channels, the datagram header, `Hello` and
capability negotiation, the display list, the control-message set and its meanings, and the
input / cursor / clipboard / media formats. From there:

- **v0.12** — the freeze: D0 + D1 + D4 (the display list, with monitor switching via re-sent
  `ScreenStart` and `u32::MAX` reserved for spanning), view-only enforced, architecture.md
  updated to mark the wire frozen and list what is deferred.
- **v0.13** — frontend window, icon, and the installer, which is also where the service is
  registered (the exe and the service are one milestone).
- The **process/privilege split**, the **secure desktop**, and the **other-OS platform layers**
  then land as internal or additive work that never reopens the frozen wire.
