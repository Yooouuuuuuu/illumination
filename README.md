# Illumination

Direct, IP-only remote desktop. You connect straight to your own machines by IP address, for
example over a Tailscale tailnet. There is no ID server, no relay and no account.

- **One app for both sides:** every install can control other machines and be controlled.
- **Direct only:** dial an IP address or hostname. Getting through NAT is left to your network
  (a LAN, Tailscale, a VPN).
- **Written in Rust.**
- **Free:** GPL-3.0-only.

> Status: milestone M2. You can see another machine's screen live, but you can't control it
> yet; input is M3. See [`docs/roadmap/roadmap.md`](docs/roadmap/roadmap.md).

## Try it (console)

On the machine to be reached (the host):
```
illum --service                  # listens on this machine's Tailscale address, port 7171
```
On the other machine:
```
illum view 100.x.y.z             # or a MagicDNS name; opens a window with the host's screen
```
The first time, each side shows the other's key fingerprint and asks you to confirm. After
that you connect without prompts, and a changed key is refused. The window title shows frame
rate, bandwidth and latency.

You may need to allow `illum.exe` through Windows Firewall on the host.

To check colours, run the host with `--test-pattern`. The top half shows eight patches, left to
right: **red, green, blue, cyan, magenta, yellow, white, black**.

## Commands

```
illum                  # the app window (not built yet)
illum --service        # host (--port, --bind <IP> to opt in to other addresses, --test-pattern)
illum --backend        # per-session worker, started by the service (later milestone)
illum --control        # per-user helper for clipboard and tray (later milestone)
illum view <host>      # show a host's screen (--screen N for another display)
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
