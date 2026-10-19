// SPDX-License-Identifier: GPL-3.0-only
//! Shared code for every Illumination role: protocol messages, configuration and identity.

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

impl Role {
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Result<Role, String> {
        let mut role = Role::Frontend;
        for arg in args {
            role = match arg.as_str() {
                "--service" => Role::Service,
                "--backend" => Role::Backend,
                "--control" => Role::Control,
                other => return Err(format!("unknown argument: {other}")),
            };
        }
        Ok(role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Role, String> {
        Role::from_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_flag_is_frontend() {
        assert_eq!(parse(&[]), Ok(Role::Frontend));
    }

    #[test]
    fn role_flags() {
        assert_eq!(parse(&["--service"]), Ok(Role::Service));
        assert_eq!(parse(&["--backend"]), Ok(Role::Backend));
        assert_eq!(parse(&["--control"]), Ok(Role::Control));
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse(&["--nope"]).is_err());
    }
}
