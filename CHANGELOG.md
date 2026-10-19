# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Entries record *what shipped*, kept simple. The *why* behind a design lives in
the docs: [docs/architecture/architecture.md](docs/architecture/architecture.md)
and the evidence in [docs/lessons/lessons.md](docs/lessons/lessons.md).

## [Unreleased]

## [0.2.0] - 2026-10-19

Milestone M1, connect and trust: two machines connect directly, check each
other's keys and measure round-trip time. There is no screen yet.

### Added

- **Hosting:** `illum --service` listens on this machine's **Tailscale addresses
  only**, port 7171. Other addresses are opt-in with `--bind <IP>`. There is no
  UPnP.
- **Connecting:** `illum connect <host>` dials an IP address or MagicDNS name
  (port optional) and prints round-trip times with a min/avg/max summary.
- **Device identity:** a self-signed Ed25519 certificate is made on first start
  and kept in `%ProgramData%\Illumination`. `illum fingerprint` shows it.
- **Encryption:** TLS 1.3 only, with certificates on **both** sides. Every
  handshake signature is checked.
- **Trust by pairing.** On a first connection the controller shows the host's
  fingerprint and asks whether to trust it. The host shows the device's
  fingerprint and asks whether to pair. After that, connections need no
  prompts. A host whose key changed is refused, and the error names the fix
  (`illum forget <host>`).
- `illum list` shows pinned hosts and paired devices. The trust store is
  versioned JSON.
- **Protocol:**
  - `hello` exchange with protocol version and a capability list
  - channel framing (`[channel][length][payload]`)
  - a send queue where input and cursor go before control, then video, then
    audio, then bulk, and where video can replace stale frames
- **Deadlines on every step:** name lookup, TCP connect, TLS, hello, prompts
  (unanswered counts as "no"), and idle sessions (closed after 30 s).

### Changed

- Clearer errors: an unreachable host says so in plain words instead of
  repeating the operating system's message. Logs have no colour codes when
  written to a file or pipe.

### Verified

- `cargo test`: 33 tests pass (unit tests, plus 9 loopback integration tests
  for pairing, refusals, a changed key and deadlines). `cargo clippy` is clean.
- On this PC over its own Tailscale address: the first connection asked once
  on each side, the reconnect needed no prompt, and the LAN address refused.
- Not yet run from a second machine (the laptop). See the M1 check in
  `docs/roadmap/roadmap.md`.

## [0.1.0] - 2026-10-19

First milestone, M0: the repository, the design and the plan. Nothing connects
yet.

### Added

- Rust workspace (edition 2024) under GPL-3.0-only, with two crates:
  `illum` (the single binary) and `illum-core` (shared code).
- `illum` picks its role from the command line, the same way AnyDesk's single
  binary does: the app window (no flag), `--service`, `--backend` and
  `--control`. Each role reports that it isn't implemented yet. Unknown flags
  exit with a usage message.
- Design docs:
  - `docs/architecture/`: roles and processes, one TLS 1.3 connection with
    prioritised channels, pairing by fingerprint, the video pipeline, input
    correctness rules, secure-desktop handling, permissions and session policy,
    with D2 diagrams.
  - `docs/lessons/`: evidence from AnyDesk (idle and a live direct session over
    Tailscale), RustDesk and Sunshine behind every decision.
  - `docs/roadmap/`: milestones M0–M7, each with a pass/fail check.
- Verified with `cargo build` and `cargo test` (3 tests passing).
