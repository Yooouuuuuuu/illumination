// SPDX-License-Identifier: GPL-3.0-only
//! Copies FFmpeg's DLLs next to every executable that links this crate (`illum.exe` and the test
//! binaries in `deps/`), so a dev build runs without touching PATH. `FFMPEG_DIR` is set by
//! `.cargo/config.toml`; the installer does this copy for releases.

use std::path::{Path, PathBuf};

fn copy_fresh(src: &Path, dir: &Path) {
    let Some(name) = src.file_name() else {
        return;
    };
    let dest = dir.join(name);
    let fresh = std::fs::metadata(&dest)
        .and_then(|d| Ok((d.modified()?, std::fs::metadata(src)?.modified()?)))
        .map(|(d, s)| d >= s)
        .unwrap_or(false);
    if !fresh {
        let _ = std::fs::copy(src, &dest);
    }
}

fn main() {
    println!("cargo::rerun-if-env-changed=FFMPEG_DIR");
    let Some(ffmpeg_dir) = std::env::var_os("FFMPEG_DIR") else {
        return;
    };
    let bin = PathBuf::from(ffmpeg_dir).join("bin");
    let Ok(entries) = std::fs::read_dir(&bin) else {
        return;
    };
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; executables live three levels up,
    // test binaries one level further down in deps/.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let deps_dir = profile_dir.join("deps");
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
        {
            copy_fresh(&path, profile_dir);
            copy_fresh(&path, &deps_dir);
        }
    }
}
