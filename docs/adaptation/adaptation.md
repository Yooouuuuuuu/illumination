# Adaptation and loss

How the host chooses the quality it streams at, and what happens when packets are lost.
The section numbers are kept because code comments cite them (`§5.5`, `§5.6`, `§5.7`).

The code is `illum_media::adapt` (`crates/illum-media/src/adapt.rs`): pure logic, fed once a
second. The viewer measures, the host decides and applies. The diagram of one pass is
[`adaptation.d2`](adaptation.d2).

Related documents: [video.md](../session/video.md) for capture, codecs and the user settings,
[network-and-trust.md](../network-and-trust/network-and-trust.md) for the UDP media path,
[clipboard-audio.md](../session/clipboard-audio.md) for audio.

Terms used below:

| Term | Meaning |
|---|---|
| Rung | One quality setting: a bitrate, a frame rate and a resolution scale. |
| Ladder | The ordered list of rungs for a session, best first. |
| Delay limit (L) | The viewer's setting: a frame shown later than this after capture is late. Default 50 ms. |
| Round trip | The time for a small probe to go from the viewer to the host and back. |
| Parity | Extra packets computed from a frame's packets, so lost packets can be rebuilt. |
| FEC | Forward error correction: repairing loss with parity instead of asking again. |
| Keyframe | A frame that needs no earlier frame to decode. |

## 5.5 Adaptation: the latency controller

The controller keeps frames within the delay limit while using as much quality as the link
carries. It starts at the top of the ladder and steps down.

### The viewer's limits

The viewer sends its limits when the stream starts. The host clamps them.

| Limit | Default | Range the host accepts | `illum view` flag |
|---|---|---|---|
| Max bitrate | 20 Mb/s | 1 to 200 Mb/s | `--max-bitrate` |
| Max frame rate | 30 fps | 15–120 fps | `--max-fps` |
| Delay limit | 50 ms | any | `--latency-limit` |
| Resolution rungs | off | on or off | `--allow-scale` |

The bitrate is a ceiling for the encoder's rate control. A still desktop uses far less.
It is the limit for the whole session window, not for each display it shows (below).

### The ladder

`Ladder::new` builds the rungs in the order bitrate, then frame rate, then resolution.
Bitrate and frame rate are cheap to change. For screen content, readable text matters more
than motion, so bitrate goes first and frame rate second. A resolution change costs a keyframe
and scaling costs time, so it comes last and only if allowed.

1. Bitrate rungs at the top frame rate, each 0.8 times the one before: five of them (down
   to 0.41 of the cap). A cap above 20 Mb/s gets more, for as long as the next rung is still
   at least 8.19 Mb/s (`BITRATE_RUNGS_REACH_BPS`, where the default ladder's bitrate rungs
   end). A cap of 200 Mb/s therefore has 15 bitrate rungs and steps down to 8.8 Mb/s before
   the frame rate is touched.
2. The frame-rate rungs below the cap, from the list 60, 45, 30, 20, 15. The bitrate shrinks
   in proportion to the frame rate.
3. Only with `--allow-scale`: 75 % and 50 % of the native size, at the lowest frame rate.
   The bitrate shrinks with the pixel count.

No rung goes below 1 Mb/s (`MIN_BITRATE_BPS`).

The default ladder (20 Mb/s, 30 fps) has 7 rungs, 9 with `--allow-scale`:

| Rung | Bitrate (Mb/s) | Frame rate | Scale |
|---|---|---|---|
| 0 | 20.0 | 30 | 100 % |
| 1 | 16.0 | 30 | 100 % |
| 2 | 12.8 | 30 | 100 % |
| 3 | 10.2 | 30 | 100 % |
| 4 | 8.2 | 30 | 100 % |
| 5 | 5.5 | 20 | 100 % |
| 6 | 4.1 | 15 | 100 % |
| 7 (`--allow-scale`) | 2.3 | 15 | 75 % |
| 8 (`--allow-scale`) | 1.0 | 15 | 50 % |

A 60 fps cap gives 9 rungs (11 with scale), a 120 fps cap gives 10 (12 with scale).

Resolution rungs are off by default because the host scales on the CPU. That measured as
10–15 ms of added latency and a frame rate held to 45–53.

### What the viewer measures

For every frame it presents, the viewer compares the host's capture time (in the packet header)
with its estimate of the host clock. The estimate comes from clock probes sent four times a
second; the fastest probe of the last minute sets the offset, because two machines' clocks
drift apart over a longer time. A frame presented before any probe has answered counts as
within the limit.

Once a second the viewer sends a `MediaReport`:

| Field | Meaning |
|---|---|
| `shown` | Frames presented. |
| `within_limit` | Of those, frames presented within L. |
| `decoded` | Frames decoded, presented or not. A hidden window decodes everything and shows nothing. |
| `received`, `lost` | Datagrams received and lost, before any repair. |
| `repaired` | Frames rebuilt from parity. |
| `frames_lost` | Frames that could not be rebuilt. |
| `rtt_ms` | The 75th percentile of the probe round trips of the last 3 s. |
| `rtt_floor_ms` | The smallest round trip of the last minute: the path with no queue. Probes sent before the stream starts already set it. |

### Congestion or random loss

Loss has two causes and they need opposite answers. Congestion means the host sends more than
the path carries: a queue builds, so the round trip rises, and sending less cures it. Random
loss (Wi-Fi, for example) leaves the round trip flat, and sending less changes nothing; parity
is the cure (§5.6).

The round trip counts as **risen** when `rtt_ms` is at least 1.5 times the floor and at least
10 ms above it. The 75th percentile is used because an overloaded path delays most probes,
while one probe stuck behind a keyframe burst should not count.

### Grading a second

The host gives each second a grade. N is a quarter of the current rung's frame rate, at least 5.

| Condition | Grade | Effect |
|---|---|---|
| `decoded` < N, `frames_lost` ≥ 2 and the round trip risen | red 🔴 | The picture froze from overload. |
| otherwise `shown` < N | blank ⚪ | No evidence: a still screen, a hidden window, or random loss. Not counted. |
| `within_limit` ÷ `shown` ≥ 90 % | green 🟢 | Counts toward a raise. |
| 50–90 % | yellow 🟡 | Counts toward neither. |
| < 50 % | red 🔴 | Counts toward a drop. |

Lost frames are not late frames. Apart from the first row, loss is the parity controller's
business.

### When the rung moves

Every move is one rung. After any move the count of graded seconds starts again, from the first
red or green second.

| Trigger | Condition | Move |
|---|---|---|
| Fast drop | In one second: loss ≥ 2 % and the round trip risen. | Down one rung at once, at most one per second. |
| Drop | Red in at least 30 % of the last 10 graded seconds, all within the last 30 s, and the current second not green. | Down one rung. |
| Raise | Green in at least 80 % of the last 15 graded seconds, and loss over the last 30 s below 0.5 %. | Up one rung. |
| Backstop | Asked for by the parity controller (§5.6). | Down one rung. |

Details of each:

- **Fast drop repeats only while it works.** Within 10 s of a fast drop, the next one needs
  the count of packets received to have held up: at least half-way between "fell with the
  rate" and "unchanged". A saturated link keeps delivering the same amount when the host sends
  less. A loss storm delivers a fixed share, so the count falls with the rate, and dropping
  further would be for nothing. A second with nothing received is no proof, so it allows no
  further fast drop either.
- **Evidence must be recent.** A hidden window grades almost nothing, so old red seconds must
  not add up across minutes. The seconds of a raise must lie within four times the raise
  window.
- **The last second of a drop must be bad.** A few red seconds followed by green ones are a
  path that has recovered.
- **A failed raise backs off.** A raise undone by a drop within 60 s counts as a failure of
  that rung. Each failure doubles the green run needed before the next try: 15, 30, 60, then
  120 s. A raise that holds 60 s clears the count. Without this the controller would hunt
  between the rung that fits and the one above it.
- **Raises wait for clean loss numbers** over the same 30 s the parity controller uses, so a
  backstop drop is not undone 15 s later.

### Applying a rung

| Part | What changes |
|---|---|
| Pacer | A token bucket sends at the rung's bitrate. Its burst allowance is 50 ms or two frames, whichever is longer, so a frame at a low frame rate is not spread over its own interval. |
| Encoder | Its bitrate is `rung × 100 ÷ (100 + parity %) × 90 %` (`encoder_bitrate`). Parity comes out of the budget; 10 % is left for headers and for the bucket to refill. Hardware encoders (NVENC, AMF, QSV) take a new bitrate live; software encoders are reopened. |
| Frame rate, scale | The encoder is reopened, which costs one keyframe. Frames above the rate are skipped; the newest frame wins. |
| Viewer | The host sends `Quality` (rung, bitrate, frame rate, scale, parity, reason). The viewer shows it in its status line. |

When the viewer shows a display small (one of several in its window), the host sends it at
that size.

The displays of one session window share one ladder and one max bitrate: the tiles of
"All", or the window's own display and those popped out of it. "All" and pop-outs do not mix;
turning "All" on closes the popped-out windows. The viewer numbers the window
(`ScreenStart.group`), and the host keeps one ladder per desk and window. A second is judged
from the frames of all the displays added up. Each rung's bitrate is split by the pixels each
display sends (`split`): a display sent at a quarter of its width needs a sixteenth of the
cap, together never more than the cap and never under 1 Mb/s. All the displays step down and
up together.

### Constants

| Constant (`adapt.rs`) | Value |
|---|---|
| Green / red share of frames within L | 90 % / 50 % |
| N, frames needed to grade | ¼ of the rung's frame rate, at least 5 |
| `FREEZE_LOST_FRAMES` | 2 |
| `DROP_WINDOW`, `DROP_SHARE`, `DROP_RECENT_SECONDS` | 10 s, 30 %, 30 s |
| `RAISE_WINDOW`, `RAISE_SHARE`, `RAISE_RECENT_FACTOR` | 15 s, 80 %, 4 |
| `RAISE_HOLD_SECONDS`, `MAX_BACKOFF_DOUBLINGS` | 60 s, 3 |
| `RAISE_LOSS_SECONDS` | 30 s |
| `FAST_DROP_LOSS_PPM`, `FAST_DROP_MEMORY_SECONDS` | 2 %, 10 s |
| `RTT_RISE_FACTOR`, `RTT_RISE_MIN_MS`, `RTT_FLOOR_SECONDS` | 1.5, 10 ms, 60 s |
| Bitrate step, bitrate rungs | × 0.8, 5 (more for a cap above 20 Mb/s) |
| `MIN_BITRATE_BPS` | 1 Mb/s |

## 5.6 Loss: parity FEC, never resend

Video travels over UDP and is never resent. A resent packet arrives a round trip late, which
is too late for a frame that should already be on screen. Loss is repaired on arrival with
parity, and a frame that cannot be repaired is replaced by a keyframe.

### Parity

The code is `illum_media::fec` and `illum_media::packets`.

- A frame is cut into packets. Packets are grouped into **blocks** of at most 128 data packets
  (`MAX_DATA_SHARDS`).
- Each block of k data packets gets m parity packets, computed with Reed–Solomon over GF(2⁸)
  (the `reed-solomon-erasure` crate). Any m packets of the block may be lost; the viewer
  rebuilds the block as soon as any k of its k + m packets are in. There is no waiting and no
  round trip.
- m is `ceil(k × percent ÷ 100)`, and at least 1 when the percentage is not zero. A 2-packet
  frame at 5 % still gets one parity packet.
- A parity packet is as large as the largest data packet of its block, so the overhead is
  about m ÷ k.
- A frame is all-or-nothing. A decoder cannot use part of a frame, so every packet matters
  equally and parity protects them all alike. A frame is handed over only when every block is
  complete, received or rebuilt.
- Every packet starts with a 30-byte header (codec, keyframe flag, frame number, block index
  and count, packet index, k, m, padded length, capture time, encode time), so any single
  packet identifies its frame and block. The layout is in `packets.rs`.

**Keyframes get at least 30 % parity** (`KEYFRAME_FEC_PERCENT`), whatever the controller has
set. A keyframe is large and is the one frame that must arrive.

### The FEC controller

`FecController` sets the parity percentage. It is separate from the latency controller because
the goals differ: one keeps frames arriving, the other keeps them on time. It looks at packet
loss **before repair**, so parity doing its job does not hide the loss, over windows of 30 s.

| Loss before repair in the window | Action |
|---|---|
| ≥ 0.5 % | Parity + 5 points, up to the ceiling. |
| 0.1–0.5 % | Hold. |
| < 0.1 % | After two such windows in a row, parity − 5 points per window, down to the floor. |

Parity starts at the floor. The floor is 5 % and the ceiling 30 %. It rises fast and falls
slowly. The range is not a user setting; `--fec-min` and `--fec-max` exist for measurement
runs (the host accepts up to 50).

### How the two controllers coexist

1. **Parity is budget-neutral.** Its bytes come out of the rung's bitrate, never on top.
   Raising parity cannot add latency; the picture gets slightly softer instead.
2. **Parity never waits for the latency controller.** A frame that arrives late beats one that
   never arrives. The latency controller never raises while loss is at or above 0.5 %.
3. **Backstop.** When parity is at the ceiling and a window still has loss ≥ 0.5 % *and*
   frames that could not be rebuilt, the loss may be overload that never showed up as late
   frames. The ladder steps down one rung if both hold:
   - the round trip was risen for at least half of the window's seconds (a standing queue);
   - for any rung after the first, the window's loss is at most half of what it was when the
     backstop last fired (the previous rung down helped).

   Otherwise the loss is the path's own. Parity stays at the ceiling, the rung holds, and the
   host logs `Adaptation holds: … path loss, holding`. Loss that parity fully repairs never
   triggers the backstop.

### When a frame is lost anyway

The viewer keeps showing the last good frame and sends `FrameLost`. The host makes its next
frame a keyframe.

| Rule | Value | Where |
|---|---|---|
| Keyframe requests honoured | at most one per 500 ms; a request inside the interval stays pending | `KEYFRAME_MIN_INTERVAL`, `host.rs` |
| Static screen | The capture emits frames only on change. If a keyframe is owed and nothing new was captured, the host re-sends the last frame as a keyframe. | `host.rs` |
| Lost reference | Five frames in a row fed to the decoder with no picture out mean the frame they build on is gone. The viewer asks for a keyframe. | `NO_PICTURE_LIMIT`, `viewer.rs` |
| Reminder | While waiting for a keyframe and other frames keep arriving, the viewer asks again every second. | `KEYFRAME_REMINDER` |
| Notice | After 2 s without the keyframe the session window says "waiting for a keyframe from the host…". | `KEYFRAME_TIMEOUT` |
| Stall | The host reports each second what it sent (`HostStats`). No video for 3 s while the host says it is sending: the viewer asks for a keyframe and the session window says "video stalled; reconnecting…". After 8 s it sends `ScreenStart` again, which rebuilds the media path. | `SILENCE_AFTER`, `RECONNECT_AFTER` |

A host with a still screen sends nothing. That is not a stall, and the viewer stays quiet.

Two keyframes a second is a measured middle: four a second fed the collapse on an overloaded
link, one a second left a bursty link showing under half its frames.

### Audio

Audio uses the same Reed–Solomon code with a fixed shape: every 4 Opus frames (20 ms) are
followed by 2 parity packets, and any 4 of the 6 rebuild the rest. Nothing about audio adapts.
See [clipboard-audio.md](../session/clipboard-audio.md).

## 5.7 Testing the numbers

Every number above is a tunable. Two tools test them; one parameter changes per run.

**The simulated link** (`illum_media::sim`, `crates/illum-media/tests/harness.rs`) runs the
real `Adapter` against a model of host, path and viewer in model time. The host model makes
frames of bitrate ÷ fps bytes, packetises them with the current parity and paces them like the
real host. The path is a queue with a bandwidth, a base delay, a queue limit after which
packets drop, random loss and scripted outages. The viewer model completes frames from k of
k + m packets, waits for keyframes after a loss and reports once a second.

```
cargo test -p illum-media --test harness -- --nocapture
```

Each scenario prints its table and asserts behaviour, not exact numbers:

| Scenario | Asserts |
|---|---|
| Clean LAN | The rung never moves; no frame lost. |
| 20 Mb/s top on a 12 Mb/s path | Under the link within 8 s by the fast drop, then stays there. |
| 20 Mb/s top on a 10 Mb/s path | Under the link within 15 s; settles green; at most 2 rung changes in the last minute. |
| 20 → 4 Mb/s and back | Recovers after the drop and climbs back after the restore. |
| 15 s at 75 % loss | Costs at most 3 rungs; back at the top within two minutes. |
| 2 % random loss | Parity rises; the rung never moves. |
| 100 ms outage every 4 s | The picture keeps moving; the rung never moves. |
| 6 Mb/s cap | Never exceeded. |

The scores are utilisation (sent bitrate ÷ bandwidth over the last 60 s), flaps (rung changes
in that time), recovery time per event, false drops, seconds over the limit and frames lost.
The model has no jitter and no variation in frame size, so real links are noisier.

**The real link**: fixed-quality sessions against a known screen, recorded second by second.
