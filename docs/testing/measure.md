# Measurement runs

The simulated-link harness (architecture §5.7) scores the controllers against a model. This is
the same idea on the real link: a plan of **fixed** settings, each streamed for a fixed time
with the adaptation switched off, every second recorded, and one table per run. Two runs in
opposite directions (desktop → laptop, laptop → desktop) cross-check each other. Decisions about
the ladder, the parity controller and the codec choice are made from these tables, not from how
a session felt.

## What a run consists of

**Host** (the machine whose screen is streamed), two consoles:

```powershell
illum --service              # as usual
illum workload               # a full-screen window with the known content; Esc closes it
```

With several monitors, `illum workload --screen 1` fills the second one and leaves the first
usable; the viewer must then stream that display too (`measure.ps1 -Screen 1`, or `view
--screen 1`). The host log names the display it captures.

The workload cycles on the wall clock, so the viewer knows the phase without being told:

| seconds of each minute | phase | what the screen does |
|---|---|---|
| 0–19 | **still** | a desktop left alone: one small change per second (a seconds clock) |
| 20–39 | **detail** | a page of small text scrolling 120 px/s: every frame changes a little, all of it fine detail |
| 40–59 | **motion** | video-like: every pixel changes every frame, plus a millisecond clock |

The millisecond clock is also what a slow-motion phone video reads for a screen-to-screen
measurement: film the host screen and the viewer window together and subtract.

**Viewer**, one console:

```powershell
.\scripts\measure.ps1 -Target 100.100.101.1
```

The script reads `scripts/measure-plan.csv`, runs one `illum view` per line with `--fixed
--duration N --report <step>.json`, waits a few seconds between steps, and finishes with
`illum report <folder>`, which prints the table and writes `report.md` next to the JSON files.
The folder is `measurements/<date>-<viewer machine>/` in the repository (ignored by git).

Each step is a fresh session, so nothing carries over. With `--fixed` the host streams at
exactly the bitrate, frame rate, parity and scale asked for; the ladder and the parity
controller are not consulted, only the grade is still computed for the log.

## The plan file

```csv
name,codec,decode,render,bitrate,fps,parity,scale,seconds
av1-06,av1,gpu,gpu,6,60,10,100,60
```

| column | values |
|---|---|
| `codec` | `av1` (AV1 where both sides can, else H.265), `hevc`, `tiles` |
| `decode` | `gpu` or `cpu` (`--software-decode`) |
| `render` | `gpu` or `cpu` (`--software-render`) |
| `bitrate` | Mb/s, the one rate streamed |
| `fps` | frame-rate cap |
| `parity` | percent, fixed |
| `scale` | percent of the host display (100, 75, 50) |
| `seconds` | step length; 60 covers every phase once (`-Seconds` on the script overrides all) |

The default plan is 24 steps of 60 s: a bitrate sweep per codec on GPU decode and GPU
presentation, the decode and render variants at 6 Mb/s, then frame rate, parity and scale
variants, and one tiles step. When the host cannot encode AV1 (the laptop), delete the `av1`
lines or accept that they show up as H.265 in the table.

## Reading the table

One row per step and phase; the first two seconds of each phase are left out because the two
machines straddle the boundary. Columns:

- **fps / Mb/s**: frames presented per second and video bits received.
- **lat p50 / p95 / max**: capture on the host → presented on the viewer, in ms, from the
  per-frame samples of that phase (the host clock corrected by the probes).
- **stall**: the longest gap between two presented frames, in ms. A keyframe after a loss shows
  up here.
- **decode / draw / encode**: medians per frame, ms.
- **rtt**: median round trip of the clock probes.
- **loss %**: datagrams lost before any repair; **frames lost**: frames that could not be
  rebuilt; **keyframes**: how many arrived (one at the start, one per lost frame).
- **host taken / capped / sent**: per second, frames that came out of capture, frames the
  frame-rate cap skipped, frames encoded and sent. If `taken` is low in the still phase, the
  screen did not change; if `sent` is far below `taken` in motion, the cap or the encoder was
  the limit.

What to look for:

- **The link's ceiling**: the bitrate step where `loss %` and `stall` start to climb while the
  host still sends 60 fps in motion. That is the number the ladder's top should respect on this
  link, and whether loss grows with bitrate (a queue: the ladder's business) or stays flat
  (random loss: parity's business).
- **The viewer's cost**: the `cpudec` / `cpurender` / `cpu` rows against the GPU row at the same
  bitrate: decode, draw and fps say what each path costs on that machine.
- **Parity's price and worth**: `parity05` vs `parity30` at the same bitrate: frames lost and
  stall against the bits spent.
- **Scale rungs**: whether 75 % and 50 % buy anything in stall or loss at the same bitrate, and
  what they cost in fps or detail (judge the detail phase by eye too).
- **AV1 vs H.265** at the same bitrate: lat, decode and, by eye, the detail phase.

The JSON files keep every per-second record and every per-frame latency, decode and draw
sample, so other cuts can be made later without re-running.

## Single steps by hand

Every piece is an ordinary command:

```powershell
illum view 100.100.101.1 --fixed --max-bitrate 6 --fec-min 10 --fec-max 10 --duration 60 --report step.json
illum report .\folder-with-json-files
```

`--fixed` is also useful on its own, to hold a quality while looking at something.
