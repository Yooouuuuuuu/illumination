# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Entries record *what shipped*, kept simple. The *why* behind a design lives in
the docs: [docs/architecture/architecture.md](docs/architecture/architecture.md)
and the evidence in [docs/lessons/lessons.md](docs/lessons/lessons.md).

## [Unreleased]

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
