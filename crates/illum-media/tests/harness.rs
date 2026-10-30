// SPDX-License-Identifier: GPL-3.0-only
//! The simulated-link scenarios of architecture §5.7, run against the real controllers. Each
//! test prints its table (`cargo test -p illum-media --test harness -- --nocapture`) and
//! asserts the behaviour the design promises, not exact numbers.

use illum_media::adapt::{Grade, Limits};
use illum_media::sim::{Link, Outcome, Scenario, Settings, run};

fn show(o: &Outcome) {
    eprintln!("{}", o.table(10));
    eprintln!("{:#?}", o.scores);
}

fn settings(max_bitrate_bps: u64) -> Settings {
    Settings {
        limits: Limits {
            max_bitrate_bps,
            max_fps: 60,
        },
        ..Settings::default()
    }
}

#[test]
fn clean_lan_never_moves() {
    let s = Scenario::new("clean LAN", 180, Link::clean(100_000_000));
    let o = run(&s, &settings(20_000_000), 1);
    show(&o);
    assert_eq!(o.scores.false_drops, 0);
    assert_eq!(o.scores.flaps, 0);
    assert!(
        o.seconds.iter().all(|s| s.rung_index == 0),
        "stays at the top"
    );
    assert!(
        o.scores.seconds_over_limit <= 1,
        "{}",
        o.scores.seconds_over_limit
    );
    assert_eq!(o.scores.frames_lost, 0);
}

#[test]
fn wifi_at_10_mbps_settles_below_the_link() {
    // The laptop case: a 20 Mb/s ladder top on a path that carries 10.
    let s = Scenario::new(
        "Wi-Fi 10 Mb/s",
        240,
        Link {
            base_delay_ms: 5.0,
            ..Link::clean(10_000_000)
        },
    );
    let o = run(&s, &settings(20_000_000), 2);
    show(&o);
    let settled = &o.seconds[o.seconds.len() - 60..];
    assert!(
        settled.iter().all(|s| s.rung.bitrate_bps < 10_000_000),
        "ends below the link's capacity"
    );
    assert!(o.scores.flaps <= 2, "flaps: {}", o.scores.flaps);
    let first_fit = o
        .seconds
        .iter()
        .position(|s| s.rung.bitrate_bps < 10_000_000)
        .expect("drops below 10 Mb/s");
    eprintln!("first rung under the link after {first_fit} s");
    assert!(first_fit <= 90, "took {first_fit} s to get under the link");
    assert!(
        o.scores.utilisation > 0.5,
        "utilisation {}",
        o.scores.utilisation
    );
}

#[test]
fn bandwidth_drop_recovers_and_raise_comes_back() {
    let s = Scenario::new(
        "20 → 4 Mb/s at 60 s, back at 180 s",
        360,
        Link::clean(100_000_000),
    )
    .then(60, Link::clean(4_000_000))
    .then(180, Link::clean(100_000_000));
    let o = run(&s, &settings(20_000_000), 3);
    show(&o);
    let r = &o.scores.recovery_s;
    assert_eq!(r.len(), 2);
    assert!(
        r[0].is_some_and(|t| t <= 150),
        "recovery after the drop: {:?}",
        r[0]
    );
    assert!(r[1].is_some(), "green again after the restore");
    let at_170 = &o.seconds[170];
    assert!(
        at_170.rung.bitrate_bps < 4_000_000,
        "under 4 Mb/s before the restore: {}",
        at_170.rung
    );
    let end = &o.seconds[o.seconds.len() - 1];
    assert!(
        end.rung_index < at_170.rung_index,
        "climbed back after the restore: {} vs {}",
        end.rung,
        at_170.rung
    );
}

#[test]
fn random_loss_raises_parity_not_the_ladder() {
    let s = Scenario::new(
        "2 % random loss on a fast link",
        240,
        Link {
            loss: 0.02,
            ..Link::clean(100_000_000)
        },
    );
    let o = run(&s, &settings(20_000_000), 4);
    show(&o);
    let fec_at = |t: usize| o.seconds[t].fec_percent;
    assert_eq!(fec_at(10), 5);
    assert!(fec_at(100) >= 15, "parity after 100 s: {} %", fec_at(100));
    let early: u32 = o.seconds[..30].iter().map(|s| s.frames_lost).sum();
    let late: u32 = o.seconds[180..].iter().map(|s| s.frames_lost).sum();
    assert!(
        late * 4 < early.max(1),
        "frames lost early {early}, late {late}"
    );
    assert!(
        o.seconds.iter().all(|s| s.rung_index <= 1),
        "loss alone does not walk the ladder down while latency is fine"
    );
}

#[test]
fn bursty_outages_are_survived() {
    // 100 ms of total loss every 4 s (≈ 2.5 % loss) on a link with headroom.
    let s = Scenario::new("100 ms outage every 4 s", 240, Link::clean(50_000_000))
        .with_bursts(4.0, 100.0);
    let o = run(&s, &settings(20_000_000), 5);
    show(&o);
    let shown: u32 = o.seconds[180..].iter().map(|s| s.shown).sum();
    assert!(
        shown > 50 * 60 * 6 / 10,
        "picture keeps moving: {shown} frames in the last 60 s"
    );
    let greens = o.seconds[180..]
        .iter()
        .filter(|s| s.grade == Grade::Green)
        .count();
    assert!(greens >= 40, "mostly green once settled: {greens}/60");
}

#[test]
fn low_cap_is_respected() {
    let s = Scenario::new("user cap 6 Mb/s", 120, Link::clean(100_000_000));
    let o = run(&s, &settings(6_000_000), 6);
    show(&o);
    assert!(o.seconds.iter().all(|s| s.rung.bitrate_bps <= 6_000_000));
    assert!(
        o.seconds.iter().all(|s| s.sent_bps <= 6_600_000),
        "pacing holds the cap (+10 % headers)"
    );
}
