# Future work

## A measured comparison

How Illumination compares with AnyDesk and RustDesk has not been measured. The test: the same
two machines, the same link and the same screen content for each program, with the host
showing a millisecond clock (`illum workload`) and a slow-motion camera filming the host's
screen and the viewer's window together. The difference between the two clocks is the delay
from screen to screen. Frame rate and behaviour under packet loss are compared the same way.

Until this is done, no claim about being faster is made.

## macOS and Linux

Illumination runs on Windows only. macOS and Linux are the plan.

Most of the program already carries over, because a portable way was chosen wherever one
exists:

| Already portable | Through |
|---|---|
| Protocol, trust, connection, media path | plain Rust, `rustls`, `tokio` |
| Encoding and decoding, hardware included | FFmpeg's hardware paths (VideoToolbox, VAAPI) |
| Drawing the picture | `wgpu` |
| Windows, and input on the controller | `winit`, `egui` |
| Clipboard text | `arboard` |
| Keys on the wire | neutral key codes (`illum_core::keymap`) |

What each system still needs, as a crate beside `illum-win`:

- **Screen capture** and the pointer's shape (ScreenCaptureKit on macOS; PipeWire or X11 on
  Linux).
- **Input injection**, with the key-code table for that system.
- **The keyboard grab** in the session window, so shortcuts reach the host.
- **Sound** capture and playback.
- **Running without the app:** a launchd or systemd service in place of the Windows service.
- **Packaging**, and on macOS the screen-recording and accessibility permissions.

None of this is started. The non-Windows branches in the code are stubs, and a build on those
systems is not tested.
