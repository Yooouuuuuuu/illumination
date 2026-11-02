// SPDX-License-Identifier: GPL-3.0-only
//! Measurement runs (docs/testing/measure.md): the JSON a viewer writes at the end of a fixed
//! session (`view --fixed --duration N --report file.json`), and the table `illum report`
//! makes from a folder of them. The workload on the host cycles still / detail / motion on the
//! wall clock, so every record is grouped by that phase before anything is averaged.

use illum_media::workload::{PHASE_SECONDS, PHASES, Phase};
use illum_ui::SecondRecord;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The settings a step ran with (what the viewer asked for).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub fec_min: u32,
    pub fec_max: u32,
    pub latency_limit_ms: u32,
    pub scale_percent: u32,
    pub fixed: bool,
    pub hw_decode: bool,
    pub gpu_render: bool,
    pub audio: bool,
    pub duration_s: u64,
}

/// One step's file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub label: String,
    pub viewer: String,
    pub host: String,
    pub started_unix: u64,
    /// The codec the host actually sent, its decoder, and the presenter in use.
    pub codec: String,
    pub decoder: String,
    pub presenter: String,
    pub settings: Settings,
    pub seconds: Vec<SecondRecord>,
}

impl Report {
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn read(path: &Path) -> std::io::Result<Report> {
        serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)
    }
}

/// Every `*.json` report in `dir`, in name order.
pub fn load_dir(dir: &Path) -> std::io::Result<Vec<(PathBuf, Report)>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        match Report::read(&p) {
            Ok(r) => out.push((p, r)),
            Err(e) => eprintln!("skipping {}: {e}", p.display()),
        }
    }
    Ok(out)
}

/// One line of the table: a step in one workload phase.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub codec: String,
    pub decoder: String,
    pub presenter: String,
    pub settings: Settings,
    pub phase: Phase,
    pub seconds: u32,
    pub fps_shown: f64,
    pub mbps: f64,
    pub latency_p50: Option<f64>,
    pub latency_p95: Option<f64>,
    pub latency_max: Option<f64>,
    pub stall_max_ms: f64,
    pub decode_p50_ms: Option<f64>,
    pub draw_p50_ms: Option<f64>,
    pub encode_p50_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub loss_percent: f64,
    pub frames_lost: u32,
    pub keyframes: u32,
    /// Host side per second: frames out of capture, skipped by the cap, sent.
    pub host_taken: Option<f64>,
    pub host_capped: Option<f64>,
    pub host_sent: Option<f64>,
}

/// The first seconds of a phase are left out: the host clock, the viewer clock and the
/// capture pipeline straddle the boundary.
const PHASE_EDGE_SECONDS: u64 = 2;
/// The first seconds of a step are left out too: the opening keyframe, the decoder and the
/// GPU warming up showed as a 250–500 ms "stall" in whichever phase a step happened to start.
const STEP_START_SECONDS: u32 = 3;

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

fn sorted(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut v: Vec<f64> = values.collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// Groups a report's seconds by phase and summarises each group.
pub fn rows(report: &Report) -> Vec<Row> {
    let mut out = Vec::new();
    for phase in PHASES {
        let secs: Vec<&SecondRecord> = report
            .seconds
            .iter()
            .filter(|s| {
                let (p, into) = Phase::at(s.unix_s);
                p == phase
                    && s.t >= STEP_START_SECONDS
                    && (PHASE_EDGE_SECONDS..PHASE_SECONDS).contains(&into)
            })
            .collect();
        if secs.is_empty() {
            continue;
        }
        let n = secs.len() as f64;
        let latencies = sorted(
            secs.iter()
                .flat_map(|s| s.latency_ms.iter().map(|l| f64::from(*l))),
        );
        let decodes = sorted(
            secs.iter()
                .flat_map(|s| s.decode_us.iter().map(|u| f64::from(*u) / 1000.0)),
        );
        let draws = sorted(
            secs.iter()
                .flat_map(|s| s.draw_us.iter().map(|u| f64::from(*u) / 1000.0)),
        );
        let encodes = sorted(
            secs.iter()
                .flat_map(|s| s.encode_us.iter().map(|u| f64::from(*u) / 1000.0)),
        );
        let rtts = sorted(secs.iter().filter_map(|s| s.rtt_ms.map(f64::from)));
        let received: u64 = secs.iter().map(|s| u64::from(s.received)).sum();
        let lost: u64 = secs.iter().map(|s| u64::from(s.lost)).sum();
        let hosts: Vec<_> = secs.iter().filter_map(|s| s.host.as_ref()).collect();
        let host_avg = |f: &dyn Fn(&illum_ui::HostSecond) -> u32| {
            (!hosts.is_empty())
                .then(|| hosts.iter().map(|h| f64::from(f(h))).sum::<f64>() / hosts.len() as f64)
        };
        out.push(Row {
            label: report.label.clone(),
            codec: report.codec.clone(),
            decoder: report.decoder.clone(),
            presenter: report.presenter.clone(),
            settings: report.settings.clone(),
            phase,
            seconds: secs.len() as u32,
            fps_shown: secs.iter().map(|s| f64::from(s.shown)).sum::<f64>() / n,
            mbps: secs.iter().map(|s| s.bytes as f64).sum::<f64>() * 8.0 / n / 1e6,
            latency_p50: percentile(&latencies, 0.5),
            latency_p95: percentile(&latencies, 0.95),
            latency_max: latencies.last().copied(),
            stall_max_ms: secs
                .iter()
                .map(|s| f64::from(s.stall_ms))
                .fold(0.0, f64::max),
            decode_p50_ms: percentile(&decodes, 0.5),
            draw_p50_ms: percentile(&draws, 0.5),
            encode_p50_ms: percentile(&encodes, 0.5),
            rtt_ms: percentile(&rtts, 0.5),
            loss_percent: if received + lost > 0 {
                lost as f64 * 100.0 / (received + lost) as f64
            } else {
                0.0
            },
            frames_lost: secs.iter().map(|s| s.frames_lost).sum(),
            keyframes: secs.iter().map(|s| s.keyframes).sum(),
            host_taken: host_avg(&|h| h.taken),
            host_capped: host_avg(&|h| h.capped),
            host_sent: host_avg(&|h| h.sent),
        });
    }
    out
}

fn opt(v: Option<f64>, digits: usize) -> String {
    v.map_or("–".to_string(), |v| format!("{v:.digits$}"))
}

/// The whole folder as one Markdown table (plus a header naming the machines).
pub fn markdown(reports: &[(PathBuf, Report)]) -> String {
    let mut out = String::new();
    if let Some((_, first)) = reports.first() {
        out.push_str(&format!(
            "# Measurement: {} viewing {}\n\n",
            first.viewer, first.host
        ));
        out.push_str(&format!(
            "{} steps; presenter: {}. Latency is capture → presented (ms); stall is the longest gap between presented frames; host columns are frames per second out of capture / skipped by the cap / sent. The first {PHASE_EDGE_SECONDS} s of each phase and the first {STEP_START_SECONDS} s of each step are left out; in the still phase a stall of about 1000 ms is the content's own one change per second.\n\n",
            reports.len(),
            first.presenter
        ));
    }
    out.push_str("| step | codec | decoder | cap Mb/s | fps | parity | scale | phase | s | fps | Mb/s | lat p50 | p95 | max | stall | decode | draw | encode | rtt | loss % | frames lost | keyframes | host taken/capped/sent |\n");
    out.push_str("|---|---|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|\n");
    for (_, report) in reports {
        for r in rows(report) {
            let s = &r.settings;
            let host = match (r.host_taken, r.host_capped, r.host_sent) {
                (Some(t), Some(c), Some(se)) => format!("{t:.0} / {c:.0} / {se:.0}"),
                _ => "–".to_string(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {:.1} | {} | {} | {} | {} | {} | {:.1} | {:.2} | {} | {} | {} | {:.0} | {} | {} | {} | {} | {:.2} | {} | {} | {} |\n",
                r.label,
                r.codec,
                r.decoder,
                f64::from(s.bitrate_kbps) / 1000.0,
                s.fps,
                s.fec_min,
                s.scale_percent,
                r.phase.name(),
                r.seconds,
                r.fps_shown,
                r.mbps,
                opt(r.latency_p50, 0),
                opt(r.latency_p95, 0),
                opt(r.latency_max, 0),
                r.stall_max_ms,
                opt(r.decode_p50_ms, 1),
                opt(r.draw_p50_ms, 1),
                opt(r.encode_p50_ms, 1),
                opt(r.rtt_ms, 0),
                r.loss_percent,
                r.frames_lost,
                r.keyframes,
                host,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use illum_ui::HostSecond;

    fn second(unix_s: u64, shown: u32, latency: f32) -> SecondRecord {
        SecondRecord {
            unix_s,
            t: unix_s as u32,
            shown,
            frames: shown,
            decoded: shown,
            bytes: 100_000,
            latency_ms: vec![latency; shown as usize],
            decode_us: vec![4000; shown as usize],
            draw_us: vec![1000; shown as usize],
            encode_us: vec![3000; shown as usize],
            received: 100,
            lost: 1,
            rtt_ms: Some(12.0),
            host: Some(HostSecond {
                taken: 60,
                capped: 2,
                sent: shown,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn rows_group_by_phase_and_skip_the_edges() {
        let mut report = Report {
            label: "av1-6".into(),
            codec: "AV1".into(),
            ..Default::default()
        };
        // Seconds 0..60 of a cycle: still 0–19, detail 20–39, motion 40–59.
        for s in 0..60u64 {
            let (phase, _) = Phase::at(s);
            let shown = match phase {
                Phase::Still => 1,
                Phase::Detail => 58,
                Phase::Motion => 60,
            };
            report.seconds.push(second(s, shown, 20.0 + s as f32 % 3.0));
        }
        let rows = rows(&report);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].phase, Phase::Still);
        assert_eq!(
            rows[0].seconds, 17,
            "two edge seconds and the step's first three dropped"
        );
        assert_eq!(rows[1].seconds, 18);
        assert_eq!(rows[1].fps_shown, 58.0);
        assert_eq!(rows[2].fps_shown, 60.0);
        assert_eq!(rows[2].latency_p50, Some(21.0));
        assert!((rows[2].loss_percent - 100.0 / 101.0).abs() < 1e-9);
        assert_eq!(rows[2].host_taken, Some(60.0));
        assert_eq!(rows[2].host_sent, Some(60.0));
        assert_eq!(rows[2].mbps, 0.8);
        let md = markdown(&[(PathBuf::from("x.json"), report)]);
        assert!(md.contains("| av1-6 | AV1 |"));
        assert_eq!(md.matches("| motion |").count(), 1);
    }

    #[test]
    fn reports_round_trip_through_json() {
        let dir = std::env::temp_dir().join(format!("illum-measure-{}", std::process::id()));
        let path = dir.join("step.json");
        let report = Report {
            label: "step".into(),
            seconds: vec![second(100, 3, 25.0)],
            ..Default::default()
        };
        report.write(&path).unwrap();
        let back = Report::read(&path).unwrap();
        assert_eq!(back, report);
        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
