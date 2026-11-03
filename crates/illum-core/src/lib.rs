// SPDX-License-Identifier: GPL-3.0-only
//! Shared code for every Illumination role: roles, protocol messages and the trust store.

pub mod clipboard;
pub mod input;
pub mod keymap;
pub mod protocol;
pub mod store;

/// The roles one `illum` binary can run as, mirroring AnyDesk's single-binary design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The window the person sees: device list, connect, viewer.
    Frontend,
    /// Always-on background service (runs as SYSTEM); listens for direct connections.
    Service,
    /// One per incoming session, started by the service as SYSTEM inside the console
    /// session: screen capture, encoding, input injection, desktop switching.
    Backend,
    /// Runs as the logged-in user: clipboard and tray.
    Control,
}
