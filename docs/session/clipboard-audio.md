# Clipboard, audio, files

What a session carries besides the picture and the input: the text clipboard and the host's
sound.

The **host** is the machine being controlled, the **viewer** the machine you sit at.

Related documents: [network-and-trust.md](../network-and-trust/network-and-trust.md) for
channels, capabilities and permissions, [video.md](video.md) and
[adaptation.md](../adaptation/adaptation.md) for the UDP media path and the video budget.

## 8. Clipboard, audio, files

### Clipboard

Text copied on one machine can be pasted on the other, both ways, for as long as the session
is connected.

**Switching it off.** Each side lists the `clipboard` capability when it is willing. The
clipboard is shared only when both do.

| Side | Command line | App |
|---|---|---|
| Host | `illum --service --no-clipboard` | A host setting, applied to desks that connect from then on |
| Viewer | `illum view <host> --no-clipboard` | A viewer setting |

A host that streams the test pattern does not share its clipboard. When a host shows several
displays, only the main session shares the clipboard; the extra sessions for the other
displays do not.

**How it works** (`illum::clipboard::ClipboardSync`, the same code on both sides):

- A dedicated thread owns the system clipboard through the `arboard` crate, one code path for
  Windows, macOS, X11 and Wayland.
- It **polls** the clipboard four times a second to notice a copy made here. A poll is the
  one mechanism that is the same on every system, and the host has no window to receive
  Windows' clipboard notifications.
- New text is sent on the `Clipboard` channel of the TCP path as
  `illum_core::clipboard::ClipboardMsg::Text`: one byte for the kind (1), then the UTF-8
  bytes. The other side puts it on its clipboard.
- **A content hash prevents echo loops.** The thread remembers the hash of what it last saw
  or set. The text the peer just applied is therefore not sent back, and the same copy is
  not sent twice.
- What is already on the clipboard when the session starts is noted, not sent.
- On the viewer the clipboard thread lives across reconnections.

**Limits:**

- Text only. An image or a copied file is not text; the read fails and is skipped until the
  next poll.
- Text over 1 MiB (`MAX_TEXT_BYTES`) is not sent.
- Empty text is not sent.
- If another program holds the clipboard, the read is skipped until the next poll. A failed
  write is logged and left for the next change.
- A machine with no usable clipboard (headless) runs the session without the feature.

### Audio

The viewer plays what the host plays: the sound going to the host's default output device.
There is no microphone path and no sound from the viewer to the host.

**Switching it off.**

| Side | Command line | App |
|---|---|---|
| Host | `illum --service --no-audio`: nothing is captured or sent, and `audio` is not listed | A host setting, applied to desks that connect from then on |
| Viewer | `illum view <host> --mute`: the viewer does not ask for sound | A viewer setting |

The viewer's wish travels in `ScreenStart`. The host sends audio only when it has a source
and the viewer asked for it. Extra sessions for other displays of the same host are always
muted, so the sound is heard once.

**The pipeline:**

| Stage | What | Where |
|---|---|---|
| Capture | WASAPI loopback of the default output, shared mode. Windows converts to 48 kHz stereo 16-bit (`AUTOCONVERTPCM`), so there is no resampler of our own | `illum_win::LoopbackCapture` |
| Encode | Opus through FFmpeg's `libopus`, `application=lowdelay`, constant 80 kb/s, **5 ms frames** | `illum_ffmpeg::opus` |
| Protect | Groups of **4 data + 2 parity** packets (one group is 20 ms). Any 4 of the 6 rebuild the rest | `illum_media::audio::Grouper` |
| Send | Its own stream on the UDP media path, with its own sequence numbers, from its own thread | `crates/illum/src/host.rs` |
| Receive | Rebuilds lost frames from parity, then a fixed **40 ms** buffer | `illum_media::audio::Reassembler`, `JitterBuffer` |
| Play | Opus decode, then WASAPI shared mode on the default output | `illum::audio::AudioOut`, `illum_win::Playback` |

Terms: **Opus** is the audio codec. **Parity** packets are extra packets computed from the
data packets (Reed–Solomon, the same code as for video) so that lost ones can be rebuilt
without asking again. A **jitter buffer** holds a little audio before playing, so packets
that arrive unevenly still play evenly.

Why these numbers:

- 5 ms frames keep a parity group short. Opus packets decode independently, so parity for
  them is cheap.
- 40 ms of buffer is long enough to rebuild a lost packet from its group and short enough
  not to be noticed.
- Nothing here adapts. The bitrate, the parity and the buffer are fixed. Audio is about
  0.1 Mb/s with parity, next to 2–20 Mb/s of video.

Behaviour:

- A data packet is sent the moment its frame is encoded. The two parity packets follow the
  fourth frame of the group.
- The audio thread on the host is separate from the video pacer and never waits behind it.
- A missing frame is played as 5 ms of silence.
- 100 ms with nothing means the host is quiet. The buffer then fills to 40 ms again when
  sound returns, instead of playing catch-up.
- A backlog is skipped, not played late, on both sides. The host keeps at most 100 ms of
  captured sound waiting; the viewer drops down to 40 ms when more than 80 ms has piled up.
- There is no synchronisation between sound and picture. Both are played as soon as they
  can be.
- A viewer without an output device, or without an Opus decoder, runs the session without
  sound.
- With `--test-pattern` the host sends a 440 Hz tone instead of captured sound.

Packet format (`illum_media::audio`, little-endian, 8-byte header):

| Bytes | Field |
|---|---|
| 0..4 | `u32` frame sequence. A parity packet carries the first sequence of its group |
| 4 | `u8` index in the group: 0..4 data, 4..6 parity |
| 5 | `u8` `k \| m << 4` (4 and 2) |
| 6..8 | `u16` shard length the parity was computed over; 0 on data packets |
| 8.. | The Opus frame, or one parity shard |

Capture and playback are built for Windows. A host on another system does not list `audio`;
a viewer on another system decodes the sound and drops it.

### Files

The protocol reserves a `Files` channel on the TCP path, at the lowest send priority. Nothing
uses it.
