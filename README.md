# Illumination

Direct, IP-only remote desktop. You connect straight to your own machines by IP address, for
example over a Tailscale tailnet. There is no ID server, no relay and no account.

- **One app for both sides:** every install can control other machines and be controlled.
- **Direct only:** dial an IP address or hostname. Getting through NAT is left to your network
  (a LAN, Tailscale, a VPN).
- **Written in Rust.**
- **Free:** GPL-3.0-only.

> Status: milestone M1. Two machines can connect, trust each other and measure round-trip
> time. There is no screen yet. See [`docs/roadmap/roadmap.md`](docs/roadmap/roadmap.md).

## Try it (M1, console)

On the machine to be reached (the host):
```
illum --service                  # listens on this machine's Tailscale address, port 7171
```
On the other machine:
```
illum connect 100.x.y.z          # or a MagicDNS name; prints the round-trip time
```
The first time, each side shows the other's key fingerprint and asks you to confirm. After
that you connect without prompts, and a changed key is refused.

You may need to allow `illum.exe` through Windows Firewall on the host.

## Commands

```
illum                  # the app window (not built yet)
illum --service        # host: listen for connections (--port, --bind to opt in to other addresses)
illum --backend        # per-session worker, started by the service (later milestone)
illum --control        # per-user helper for clipboard and tray (later milestone)
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
