// SPDX-License-Identifier: GPL-3.0-only
//! Which local addresses to listen on. Default: Tailscale interfaces only (architecture §4.1).

use std::net::IpAddr;

/// True for Tailscale's address ranges: `100.64.0.0/10` and `fd7a:115c:a1e0::/48`.
pub fn is_tailscale(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..=127).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// This machine's Tailscale addresses (empty if Tailscale isn't running).
pub fn tailscale_addresses() -> Vec<IpAddr> {
    let mut addrs: Vec<IpAddr> = if_addrs::get_if_addrs()
        .map(|ifs| {
            ifs.into_iter()
                .map(|i| i.ip())
                .filter(is_tailscale)
                .collect()
        })
        .unwrap_or_default();
    addrs.sort();
    addrs.dedup();
    addrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_ranges() {
        let yes = [
            "100.64.0.1",
            "100.100.101.1",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
        ];
        let no = [
            "100.63.255.255",
            "100.128.0.1",
            "192.168.50.123",
            "127.0.0.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ];
        for ip in yes {
            assert!(is_tailscale(&ip.parse().unwrap()), "{ip}");
        }
        for ip in no {
            assert!(!is_tailscale(&ip.parse().unwrap()), "{ip}");
        }
    }
}
