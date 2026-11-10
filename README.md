# Illumination

Direct, IP-only remote desktop. You connect straight to your own machines by IP address, for
example over a Tailscale tailnet. There is no ID server, no relay and no account.

- **One app for both sides:** every install can control other machines and be controlled.
- **Direct only:** dial an IP address or hostname. Getting through NAT is left to your network
  (a LAN, Tailscale, a VPN).
- **Written in Rust.**
- **Free:** GPL-3.0-only.

> Status: milestone M4 in progress. You can see and control another machine (keyboard, mouse,
> IME text) with AV1 / H.265 video over UDP. It still runs from a console, not yet as a Windows
> service. See
> [`docs/roadmap/roadmap.md`](docs/roadmap/roadmap.md).

## Try it (console)

Build on each machine (needs [Rust](https://rustup.rs) and, once, two downloads: a prebuilt
FFmpeg for the AV1 / H.265 codecs and libclang for generating its bindings; nothing else to
install on Windows 10/11):
```powershell
git clone https://github.com/Yooouuuuuuu/illumination
cd illumination
powershell -ExecutionPolicy Bypass -File scripts\get-ffmpeg.ps1
powershell -ExecutionPolicy Bypass -File scripts\get-libclang.ps1
cargo build --release
cd target\release
```
The build copies FFmpeg's DLLs next to `illum.exe`. PowerShell only runs programs from the
current folder with `.\` in front, hence `.\illum.exe`.

On the machine to be reached (the host):
```powershell
.\illum.exe --service            # listens on this machine's Tailscale address, port 28326 (TCP and UDP)
```
On the other machine:
```powershell
.\illum.exe view 100.x.y.z       # or a MagicDNS name; opens a window with the host's screen
```
The first time, each side shows the other's key fingerprint and asks you to confirm. After
that you connect without prompts, and a changed key is refused. The window title shows frame
rate, bandwidth, latency and packet loss.

Control and input travel over TLS on TCP; video travels over **UDP on the same port number**
and is never resent; parity lets the viewer rebuild lost packets. The host **adapts**: it starts
at the top of a quality ladder (`--max-bitrate 20` Mb/s, `--max-fps 30`, up to 120) and steps
down one rung at a time while frames arrive later than your limit (`--latency-limit 50` ms), at
once when loss comes with a rising round trip (congestion), and back up when things are clean;
parity moves between 5 and 30 percent with the packet loss it sees. Resolution only goes down if
you allow it (`--allow-scale`). The title bar shows the rung in use; the host log says why it changed. A viewer offers AV1
only when its GPU decodes it (`--software-decode` turns the GPU decoders off and keeps it on
H.265). The viewer decodes and draws on the GPU; `--software-render` draws on the CPU instead,
which is also what happens on a machine with no usable GPU.

**Sound:** the host's audio output is sent too (Opus, 5 ms frames with parity, a fixed 40 ms
buffer). `illum --service --no-audio` never captures or sends it; `illum view --mute` doesn't
ask for it.

**Clipboard:** text copied on either machine appears on the other, both ways, while you are
connected. Turn it off with `illum --service --no-clipboard` on the host or
`illum view <host> --no-clipboard` on the viewer. Files come later.

**Reconnecting:** if the connection drops (a flaky link, a route change), the viewer keeps its
window open, shows "reconnecting…", and re-dials the host on its own until it comes back.
`illum view <host> --no-reconnect` turns that off so a drop just ends the session.

**Codec:** AV1 if the host can encode it and the viewer decode it, otherwise H.265, otherwise
lossless tiles (sharp, but heavy when the whole screen changes). The title bar names the one in
use. `illum codecs` shows what this machine can do and why a hardware encoder was skipped; the
NVIDIA encoders need driver **570 or newer**.

You may need to allow `illum.exe` through Windows Firewall on the host; one answer covers TCP
and UDP. (Over Tailscale this is usually not needed: its Windows client permits traffic on its
own interface.)

Each role also writes a debug-level log file: `%ProgramData%\Illumination\logs\service.log` on
the host and `…\viewer.log` on the viewer. Send those when something looks wrong.

**Measuring instead of guessing:** `illum view <host> --fixed …` streams at one quality with no
adaptation, `--duration N --report file.json` records every second, `illum workload` on the
host shows a known screen (still, fine detail, full motion), and `illum report <folder>` turns a
run into a table. `scripts/measure.ps1` runs a whole plan; see `docs/testing/measure.md`.

To check colours, run the host with `--test-pattern`. The top half shows eight patches, left to
right: **red, green, blue, cyan, magenta, yellow, white, black**.

**Keyboard modes** (`illum view <host> --keyboard …`):
- `remote`: sends key positions, so the host's layout and IME apply
- `local`: sends characters as you type them here, including your IME's result
- `auto` (default): remote when both machines use the same layout

`--view-only` sends no input. While the viewer has focus, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and
Alt+F4 go to the host.

**Pointer** (`illum view <host> --pointer …`):
- `native` (default): your own pointer, wearing the host's cursor shape. The OS moves it, so it
  never lags behind the picture and never disappears.
- `embedded`: the host's cursor drawn inside the picture, at the host's size, scaled with it, as
  a screenshot of the host would show it.

Don't control the machine you're sitting at: its own injected keys would loop back into the
viewer.

## Commands

```
illum                  # the app window (not built yet)
illum --service        # host (--port, --bind <IP> to opt in to other addresses, --test-pattern,
                       #   --no-audio, --no-clipboard)
illum --backend        # per-session worker, started by the service (later milestone)
illum --control        # per-user helper for clipboard and tray (later milestone)
illum view <host>      # see and control a host (--screen N, --keyboard remote|local|auto, --view-only,
                       #   --pointer native|embedded, --no-clipboard, --no-reconnect,
                       #   --max-bitrate Mb/s, --max-fps, --allow-scale, --latency-limit ms, --mute,
                       #   --software-decode, --software-render; measurement: --fixed, --scale,
                       #   --codec, --duration, --report, --label)
illum workload         # the measurement screen, full size (host side); Esc closes it
illum report <folder>  # table from a folder of --report files
illum displays         # list this machine's displays
illum codecs           # which video codecs this machine can encode and decode
illum connect <host>   # dial a host and measure round-trip time
illum fingerprint      # show this device's key fingerprint
illum list             # pinned hosts and paired devices
illum forget <host>    # drop a pinned host key (after the host was reinstalled)
```

## Acknowledgements

Illumination is built on what we learned from other remote-desktop and streaming projects.
None of their code is included unless a file says otherwise.

- **AnyDesk:** the reference for behaviour (studied as a user, closed source)
- **Sunshine / Moonlight** (and the forks Apollo / Artemis): low-latency capture, encoding and
  streaming ideas
- **RustDesk:** lessons on what to avoid, and UX ideas

## License

GPL-3.0-only. See [LICENSE](LICENSE).
