# Video

How the host's screen gets to the viewer's window: capture, the pixel format, the codec,
presentation, the pointer, and the settings a person can change.

Related documents: [architecture.md](../architecture/architecture.md) (roles and crates),
[network-and-trust.md](../network-and-trust/network-and-trust.md) (the connection and the UDP
media path), [adaptation.md](../adaptation/adaptation.md) (bitrate, frame rate, parity,
keyframes), [input.md](input.md).

## 5. Video

Diagram: [`video-pipeline.d2`](video-pipeline.d2).

The path of one frame:

1. The host captures the display as a BGRA8 frame in memory.
2. A one-frame slot holds the newest captured frame. A newer frame replaces one that was not
   sent yet.
3. The encoder turns it into AV1, H.265 or lossless tiles.
4. The frame is split into encrypted UDP datagrams with parity
   ([adaptation.md](../adaptation/adaptation.md),
   [network-and-trust.md](../network-and-trust/network-and-trust.md)).
5. The viewer reassembles and decodes it, on the GPU where it can.
6. The window draws it on the GPU and shows it at once.

The pointer does not travel in the picture. Its shape and position go over the reliable
connection on a channel of their own (§5.4).

| Part | Where |
|---|---|
| Pixel format, codec interface, tile codec, scaling, cursor messages, test pattern | `crates/illum-media` |
| AV1 and H.265 encoders and decoders (FFmpeg) | `crates/illum-ffmpeg` |
| Windows capture and pointer kind | `crates/illum-win` (`dda.rs`, `gdi.rs`, `pointer_kind.rs`) |
| Viewer window, GPU presenter, display tiles | `crates/illum-ui` (`lib.rs`, `gpu.rs`, `tiles.rs`) |
| Host session, viewer session, command line | `crates/illum/src` (`host.rs`, `viewer.rs`, `main.rs`) |

### 5.1 Capture

Screen capture exists on Windows only. On other systems the host can stream the colour test
pattern (`illum --service --test-pattern`) and nothing else.

`illum_win::ScreenCapture` uses one of two methods:

| Method | When | Notes |
|---|---|---|
| DXGI Desktop Duplication (DDA) | First choice | Windows hands over a frame whenever the screen changed. The frame is copied to memory once. |
| GDI (`BitBlt`) | When DDA is not available from this process | Copies the whole screen at most about 60 times a second and drops a frame identical to the one before. Slower, and it cannot see the secure desktop. |

The switch to GDI happens once, at start or at the first frame, when every graphics adapter
that lists the display refuses to duplicate it (`DXGI_ERROR_UNSUPPORTED`). This is the case on
a laptop with two GPUs when Windows runs `illum` on the GPU that does not drive the panel. The
user does not have to set anything.

Facts that follow from this:

- A still screen produces no frames, so no video is sent.
- The pointer is not in the captured image. Moving the mouse over a still screen sends pointer
  messages only.
- Only 8-bit BGRA (`DXGI_FORMAT_B8G8R8A8_UNORM`) is accepted; any other desktop format is a
  capture error.
- When Windows changes desktops (lock screen, a permission prompt), capture is refused for a
  moment. The capture follows the screen to the new desktop and retries for up to 15 seconds
  before it reports an error. See [input.md](input.md) for the secure desktop.
- While a capture exists, the host is kept awake and its display on.

**Several displays.** One stream carries one display. `ScreenStart` names it by index; 0 is
the primary one. `illum displays` on the host lists the indexes, and `illum view --screen N`
asks for one.

When the screen starts, the host tells a viewer that lists the `displays` capability what
displays it has (each as its size) and which one it is streaming (`ControlMsg::Displays`). The
session window offers them by number in its top bar. Choosing another sends a new
`ScreenStart`: the host stops the stream, releases any input held on the display it leaves,
and starts capture, encoder and input for the other. The viewer keeps the choice
(`MediaFlags::display`), so a reconnection asks for the chosen display.

The window can also show up to four displays together as tiles ("All"), or pop one out into a
window of its own. Each extra display is a separate session with its own stream, without sound
or clipboard. A tile's display is asked for at about the size the tile shows it. Tiles need
the GPU presenter. The arrangement is saved per host (§5.8).

### 5.2 One pixel format

Inside the program a frame is always **BGRA8** (`illum_media::Frame`): bytes in memory are
`B, G, R, A`, rows are tightly packed, alpha is 255. Windows calls this `X8R8G8B8`.

Lossy video does not store BGRA. It stores YUV 4:2:0: brightness per pixel and colour per 2×2
pixels. The conversions are in few places and each states its colour matrix and range:

| Step | Where | What |
|---|---|---|
| BGRA → 4:2:0 on the host | The NVENC encoder itself, or `swscale` in `illum-ffmpeg` for the others | BT.601, limited range. The encoder writes this into the stream. |
| 4:2:0 → RGB on the viewer, GPU | The shader in `illum-ui/src/gpu.rs` | Matrix (BT.601 or BT.709) and range are read from each decoded picture. |
| 4:2:0 → RGB on the viewer, CPU | `YuvPicture::to_bgra` in `illum-media/src/picture.rs` | The same arithmetic as the shader. |
| BGRA → window pixel on the CPU path | `illum_media::scale::bgra_to_0rgb` | The only place that produces `0x00RRGGBB`. |

A colour test pattern (`illum_media::pattern`: eight colour patches, a grey ramp, a moving
bar) guards against swapped channels and shifted colour. Tests send it through the tile codec
(bit-exact), through each FFmpeg encoder and decoder, through the GPU shader with both plane
layouts, both matrices and both ranges, and through a whole host-to-viewer session over UDP.

### 5.3 Codec

The host chooses the codec when the screen starts (`illum_media::codec::choose_codec`):

1. **AV1**, if the host can encode it and the viewer can decode it.
2. Otherwise **H.265** (HEVC), on the same condition.
3. Otherwise **lossless tiles**, which every build has.

Each side lists what it can do in its hello as capability strings: `enc:av1`, `enc:hevc`,
`dec:av1`, `dec:hevc`. The choice is made per direction, so two machines may use AV1 one way
and H.265 the other. H.264 and VP9 are not used.

`illum codecs` prints what this machine can encode and decode, with FFmpeg's own reason for
each encoder that would not open.

**Library.** AV1 and H.265 go through FFmpeg 8.1 (`crates/illum-ffmpeg`, bindings
`ffmpeg-the-third`). On Windows a prebuilt FFmpeg is fetched by `scripts/get-ffmpeg.ps1`. Its
NVIDIA encoder needs driver 570 or newer.

**What the host offers to encode.** At start the host opens each encoder once at a small size.
One that does not open is dropped. The first that opens for a codec is used.

| Codec | Encoders tried, in order |
|---|---|
| AV1 | `av1_nvenc`, `av1_amf`, `av1_qsv` (hardware only) |
| H.265 | `hevc_nvenc`, `hevc_amf`, `hevc_qsv`, then `libx265` on the CPU |

There is no software AV1 encoder on purpose: real-time AV1 on a CPU is too slow.

**What the viewer offers to decode.**

| Codec | Offered when |
|---|---|
| AV1 | Only when this machine's GPU decodes it. Software AV1 decoding costs too much time per frame. |
| H.265 | Always. The GPU decodes it where it can, FFmpeg's software decoder otherwise. |

The viewer finds out what its GPU decodes by decoding one small embedded keyframe per codec
(`crates/illum-ffmpeg/probe/`). The answer is kept for a week in `gpu-decoders.txt` in the
viewer's own folder, because the check can take seconds on a two-GPU laptop.

`--software-decode` turns GPU decoding off. AV1 is then not offered. `--codec av1|hevc|tiles`
narrows the offer and exists for measurement runs.

**Encoding.** All settings aim at low delay:

- No B-frames, no lookahead, one thread, `LOW_DELAY`, a 100 ms rate-control buffer.
- Keyframes only when asked for.
- NVENC: preset `p4`, tune `ll`, constant bitrate. AV1 on NVENC uses 4 tile columns.
- NVENC takes BGRA and converts on the GPU. Every other encoder gets NV12 or planar 4:2:0 from
  one `swscale` conversion on the host's CPU.
- Chroma is 4:2:0. Coloured edges on small text are slightly softer than on the host.

The encoder is opened at the size of the first frame and reopened when the resolution, the
frame rate or the scale changes. The bitrate it gets is decided by the adaptation
([adaptation.md](../adaptation/adaptation.md)).

If a lossy encoder fails to open or to encode, the host switches to tiles and the session goes
on. Every packet header names the codec of its frame, so the viewer opens the matching decoder
by itself.

**Decoding.** GPU decoding uses FFmpeg's hardware paths. The list of APIs is the only place
where the operating system shows:

| System | APIs tried, in order |
|---|---|
| Windows | D3D11VA, DXVA2 |
| Linux | VAAPI, CUDA, VDPAU |
| macOS | VideoToolbox |

On a machine with several GPUs the second and third adapter are tried too. A GPU that cannot
decode the stream fails openly; it never falls back to the CPU silently. If the GPU decoder
does not open for a session, the viewer uses the software decoder (`libdav1d` for AV1,
FFmpeg's `hevc`) and logs it.

Decoders hold no frames back: the GPU decoders use one thread, the software ones slice threading
(never frame threading), and `libdav1d` runs with `max_frame_delay=1`. A decoder hands the
window its own planes (`illum_media::picture::Picture`): NV12 from the GPU, I420 from software,
BGRA from the tile codec. A stream that is not 8-bit 4:2:0 is converted to it once per frame.

**Lossless tiles** (`illum_media::tiles`). The frame is cut into 64×64 tiles. Only tiles that
changed since the last sent frame are sent, each compressed with LZ4. Colours arrive bit-exact.
It is the fallback when the two sides share no lossy codec, and the codec of the bit-exact
tests. It needs far too much data for motion to be the main codec.

### 5.4 Presentation and the cursor

**No added delay.** The viewer shows each frame as soon as it is decoded. A newer frame
replaces an older one that was not shown yet. Nothing is queued. Sound is the exception; see
[clipboard-audio.md](clipboard-audio.md).

**GPU presenter** (`illum-ui/src/gpu.rs`). The window is a `winit` window drawn with `wgpu`,
so the same code runs on Direct3D 12, Vulkan and Metal.

- The decoder's planes are uploaded as they are (R8, RG8 or BGRA8 textures).
- One shader converts YUV to RGB, scales with bilinear filtering and blends the pointer.
- The swap chain asks for the first present mode the system offers of `Mailbox`, `Immediate`,
  `Fifo`. With the first two the newest frame wins and presenting does not block.
- Two frames may be in flight (`desired_maximum_frame_latency: 2`). With one, a frame that
  arrived before the display took the previous one blocked for up to a refresh.
- A picture is uploaded once. Redraws for a moved mouse reuse it.

**CPU fallback.** A machine with no usable GPU API, or `--software-render`, draws with
`softbuffer`: it converts, scales and blends on the CPU with the same arithmetic. This path
has no session bars and no display tiles.

**View modes.**

| Mode | Behaviour |
|---|---|
| Fit (default) | 1:1 when the picture fits the window, otherwise scaled down. Never scaled up. On the CPU each window pixel is the average of the host pixels it covers. |
| Original | Always 1:1. When the picture is larger than the window, the visible part follows the mouse. |

The top bar switches between them, and so does **Ctrl+Alt+Shift+F2**.

**The pointer.** The host sends pointer shape and position on the `Cursor` channel of the
reliable connection, apart from the video. Shapes are BGRA images with a hotspot, compressed
with LZ4.

While you control the host, the pointer you see is always your own system's pointer. Your
system moves it at input time, so it never waits for a video frame, a draw or the swap chain.
Nothing is drawn into the picture. Only what the pointer looks like is a choice
(`PointerMode`, `--pointer`):

| Mode | What the pointer looks like |
|---|---|
| `remote` (default) | The host's pointer image for every pointer, set as a custom cursor through the window library. |
| `local` | Your own system's pointer of the same kind as the host's. A program's own pointer (a brush, a game crosshair) has no kind and still comes as the host's image. |

For `local` the host says which standard pointer it shows (`CursorKind`: arrow, text, hand,
wait, progress, crosshair, four resize directions, move, not allowed, help). On Windows the
host knows the kind from the cursor handle: a program that asks for a standard pointer gets
the system's own handle, so the pointer on screen is standard exactly when its handle is one
of those (`illum-win/src/pointer_kind.rs`). The kind travels with the image in cursor
message 3, sent only to a viewer that lists `cursor-kind`.

Other cases:

- When the host hides its pointer or has sent none yet, yours shows as the plain arrow. You
  never lose the mouse.
- In a view-only session, and while your mouse is outside the picture, the host's pointer is
  drawn into the picture where the host reports it.
- Windows draws some pointers, such as the text pointer, by inverting the pixels under them. A
  cursor image cannot do that. Such pixels are sent as white with a black edge
  (`edge_inverting` in `illum-media/src/cursor.rs`), which is visible on any background.
- Old names for the modes are still read from a command line or a saved choice: `desk` and
  `native` mean `local`, `host` and `embedded` mean `remote`.

**Pausing while minimized.** When the viewer window is minimized it sends
`ScreenPause { paused: true }`. The host stops capturing, encoding and sending the screen and
resumes with a keyframe when the window is restored. The connection, input and sound keep
going. Being covered by another window does not pause.

**A still host is not a dead path.** A host whose screen does not change sends no video. The
host reports each second how much it sent (`HostStats`). The viewer says "video stalled;
reconnecting…" only when the host says it did send and nothing arrived.

### 5.5–5.7

The latency controller, parity and how the numbers are tested are in
[adaptation.md](../adaptation/adaptation.md).

### 5.8 User settings

The settings are flags of `illum view`. The app's settings window sets the common ones (bitrate,
frame rate, latency limit, scaling, sound, reconnect).

**Sent to the host in `ScreenStart`.** Changing one restarts the stream, not the connection.
A value of 0 means "host default".

| Flag | Default | Meaning |
|---|---|---|
| `--screen N` | 0 | Which display. |
| `--max-bitrate MBPS` | 20 | Top of the bitrate ladder. Up to 200. A higher top gets more bitrate rungs, so the ladder still reaches ordinary rates. |
| `--max-fps N` | 30 | Top frame rate. The host clamps it to 15–120. |
| `--latency-limit MS` | 50 | Frames arriving later than this after capture count as late. |
| `--allow-scale` | off | Lets the host lower the resolution under pressure. The host scales on the CPU, which costs time. |
| `--mute` | off | Do not ask for the host's sound. |

The adaptation moves bitrate, frame rate and parity inside these limits by itself
([adaptation.md](../adaptation/adaptation.md)).

**Viewer only.** Nothing travels to the host, except that the decode flags change what the
viewer offers.

| Flag | Default | Meaning |
|---|---|---|
| `--pointer remote\|local` | `remote` | Which pointer you see (§5.4). Can be changed in the window at any time. |
| `--software-decode` | off | Decode on the CPU only. AV1 is then not offered. |
| `--software-render` | off | Draw on the CPU. A debugging switch. |
| `--codec av1\|hevc\|tiles` | not set | Narrow the codec offer. For measurement runs. |
| `--no-reconnect` | off | A dropped connection ends the session instead of re-dialling. |
| `--view-only` | off | Send no keyboard or mouse input. |

`--keyboard` is described in [input.md](input.md). `--fixed`, `--scale`, `--fec-min`,
`--fec-max`, `--duration`, `--report` and `--label` are for measurement runs.

**Remembered per host.** `desk-choices.json` in the viewer's own folder keeps, for each host
key (its fingerprint):

| Field | What |
|---|---|
| `keyboard` | The keyboard mode last chosen in the window. |
| `pointer` | The pointer mode last chosen in the window. |
| `layout` | How the host's displays are arranged as tiles: their order and where the dividing lines are. |

A flag on the command line wins over the saved choice. With neither, keyboard and pointer are
both `remote`. A saved layout is used only while it still matches the host's displays.
