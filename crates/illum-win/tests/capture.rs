// SPDX-License-Identifier: GPL-3.0-only
//! Needs a real desktop, so it is ignored by default: `cargo test -p illum-win -- --ignored`.

#![cfg(windows)]

use illum_media::source::{ScreenSource, SourceEvent};
use illum_win::{DdaSource, list_displays};
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs an interactive desktop"]
fn captures_the_primary_display() {
    let displays = list_displays().unwrap();
    assert!(!displays.is_empty());
    assert!(
        displays[0].primary,
        "primary display is listed first: {displays:?}"
    );
    let mut source = DdaSource::new(0).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        assert!(Instant::now() < deadline, "no frame within 5 s");
        let events = source.poll(Duration::from_millis(100)).unwrap();
        if let Some(frame) = events.into_iter().find_map(|e| match e {
            SourceEvent::Frame(f) => Some(f),
            _ => None,
        }) {
            break frame;
        }
    };
    assert_eq!(
        (frame.width, frame.height),
        (displays[0].width, displays[0].height)
    );
    assert!(
        frame.data.chunks_exact(4).all(|p| p[3] == 255),
        "alpha forced to 255"
    );
    assert!(
        frame.data.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
        "not an all-black frame"
    );
}
