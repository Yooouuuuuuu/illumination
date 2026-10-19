# Illumination

Direct, IP-only remote desktop. You connect straight to your own machines by IP address, for
example over a Tailscale tailnet. There is no ID server, no relay and no account.

- **One app for both sides:** every install can control other machines and be controlled.
- **Direct only:** dial an IP address or hostname. Getting through NAT is left to your network
  (a LAN, Tailscale, a VPN).
- **Written in Rust.**
- **Free:** GPL-3.0-only.

> Status: early design. Nothing works yet. See [`docs/`](docs/).

## Command

```
illum              # the app window
illum --service    # background service (installed, runs as SYSTEM)
illum --backend    # per-session worker (started by the service)
illum --control    # per-user helper: clipboard, tray (started by the service)
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
