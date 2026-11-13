// SPDX-License-Identifier: GPL-3.0-only
//! Puts the app icon into `illum.exe` (Explorer, the taskbar, and the window, which loads it
//! from the resource: `illum_ui::icon`).

fn main() {
    println!("cargo:rerun-if-changed=assets/illumination.ico");
    // The target, not the machine doing the build: a cross-build decides by where it will run.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // `set_icon` stores it as resource 1, which is `illum_ui::icon::RESOURCE_ID`.
        winresource::WindowsResource::new()
            .set_icon("assets/illumination.ico")
            .compile()
            .expect("the icon resource compiles");
    }
}
