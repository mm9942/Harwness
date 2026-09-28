//! Tailnet address ranges.

use std::net::IpAddr;

/// Whether `ip` lies in a Tailscale range: IPv4 CGNAT `100.64.0.0/10` or the
/// Tailscale IPv6 ULA prefix `fd7a:115c:a1e0::/48`.
///
/// # Description
/// A necessary, not a sufficient condition: other CGNAT users share the
/// IPv4 range. The [`crate::gate`] therefore also asks `tailscaled`
/// (`whois`) before it admits a peer.
#[must_use]
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..=127).contains(&b)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_tailnet_ip;
    use std::net::IpAddr;

    fn ip(text: &str) -> Result<IpAddr, String> {
        text.parse().map_err(|error| format!("{text}: {error}"))
    }

    #[test]
    fn tailnet_ranges_are_recognised() -> Result<(), String> {
        for inside in [
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(is_tailnet_ip(ip(inside)?), "{inside}");
        }
        for outside in [
            "100.63.255.255",
            "100.128.0.1",
            "10.0.0.1",
            "127.0.0.1",
            "192.168.1.2",
            "::1",
            "fd7a:115c:a1e1::1",
        ] {
            assert!(!is_tailnet_ip(ip(outside)?), "{outside}");
        }
        Ok(())
    }
}
