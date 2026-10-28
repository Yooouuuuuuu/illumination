// SPDX-License-Identifier: GPL-3.0-only
//! Needs a real desktop, so it is ignored by default: `cargo test -p illum-win -- --ignored`.

#![cfg(windows)]

use illum_media::Frame;
use illum_media::source::{ScreenSource, SourceEvent};
use illum_win::{DdaSource, GdiSource, list_displays};
use std::time::{Duration, Instant};

fn first_frame(source: &mut dyn ScreenSource) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "no frame within 5 s");
        let events = source.poll(Duration::from_millis(100)).unwrap();
        if let Some(frame) = events.into_iter().find_map(|e| match e {
            SourceEvent::Frame(f) => Some(f),
            _ => None,
        }) {
            return frame;
        }
    }
}

fn check_frame(frame: &Frame, width: u32, height: u32) {
    assert_eq!((frame.width, frame.height), (width, height));
    assert!(
        frame.data.chunks_exact(4).all(|p| p[3] == 255),
        "alpha forced to 255"
    );
    assert!(
        frame.data.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]),
        "not an all-black frame"
    );
}

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
    let frame = first_frame(&mut source);
    check_frame(&frame, displays[0].width, displays[0].height);
}

#[test]
#[ignore = "needs an interactive desktop"]
fn gdi_fallback_captures_the_primary_display() {
    let displays = list_displays().unwrap();
    let mut source = GdiSource::new(0).unwrap();
    let frame = first_frame(&mut source);
    check_frame(&frame, displays[0].width, displays[0].height);
    // GDI gives no change information, so unchanged captures must be dropped rather than
    // streamed: well under the ~37 polls that fit in 0.6 s at the 16 ms cadence.
    let mut frames = 0;
    let until = Instant::now() + Duration::from_millis(600);
    while Instant::now() < until {
        for e in source.poll(Duration::from_millis(50)).unwrap() {
            if matches!(e, SourceEvent::Frame(_)) {
                frames += 1;
            }
        }
    }
    assert!(
        frames <= 30,
        "{frames} frames in 0.6 s: unchanged frames are not being dropped"
    );
}

#[test]
#[ignore = "installs a system-wide keyboard hook for a moment"]
fn system_key_hook_installs() {
    illum_win::SystemKeyHook::set_active(false); // stays passive: no key is intercepted
    let hook = illum_win::SystemKeyHook::install(|_, _| {}).expect("hook installs");
    drop(hook);
}
