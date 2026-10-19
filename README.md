# Illumination

Direct, IP-only remote desktop. You connect straight to your own machines by IP address, for
example over a Tailscale tailnet. There is no ID server, no relay and no account.

- **One app for both sides:** every install can control other machines and be controlled.
- **Direct only:** dial an IP address or hostname. Getting through NAT is left to your network
  (a LAN, Tailscale, a VPN).
- **Written in Rust.**
- **Free:** GPL-3.0-only.

> Status: milestone M3. You can see and control another machine (keyboard, mouse, IME text).
> It still runs from a console, not yet as a Windows service. See
> [`docs/roadmap/roadmap.md`](docs/roadmap/roadmap.md).

## Try it (console)

Build on each machine (needs [Rust](https://rustup.rs)):
```powershell
git clone https://github.com/Yooouuuuuuu/illumination
cd illumination
cargo build --release
cd target\release
```
PowerShell only runs programs from the current folder with `.\` in front, hence `.\illum.exe`.

On the machine to be reached (the host):
```powershell
.\illum.exe --service            # listens on this machine's Tailscale address, port 7171
```
On the other machine:
```powershell
.\illum.exe view 100.x.y.z       # or a MagicDNS name; opens a window with the host's screen
```
The first time, each side shows the other's key fingerprint and asks you to confirm. After
that you connect without prompts, and a changed key is refused. The window title shows frame
rate, bandwidth and latency.

You may need to allow `illum.exe` through Windows Firewall on the host.

To check colours, run the host with `--test-pattern`. The top half shows eight patches, left to
right: **red, green, blue, cyan, magenta, yellow, white, black**.

**Keyboard modes** (`illum view <host> --keyboard …`):
- `remote`: sends key positions, so the host's layout and IME apply
- `local`: sends characters as you type them here, including your IME's result
- `auto` (default): remote when both machines use the same layout

`--view-only` sends no input. While the viewer has focus, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and
Alt+F4 go to the host.

Don't control the machine you're sitting at: its own injected keys would loop back into the
viewer.

## Commands

```
illum                  # the app window (not built yet)
illum --service        # host (--port, --bind <IP> to opt in to other addresses, --test-pattern)
illum --backend        # per-session worker, started by the service (later milestone)
illum --control        # per-user helper for clipboard and tray (later milestone)
illum view <host>      # see and control a host (--screen N, --keyboard remote|local|auto, --view-only)
illum displays         # list this machine's displays
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
