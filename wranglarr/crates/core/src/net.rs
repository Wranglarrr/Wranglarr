// SPDX-License-Identifier: GPL-3.0-or-later
//! Network ranges as people write them for "the LAN": what the reverse proxy
//! configs and the VPN's allowed networks take.

use std::net::IpAddr;

/// An IP network in CIDR form (`192.168.1.0/24`, `fd00::/8`) or a single address
/// (`10.0.0.1`). Bits set past the prefix are fine (`192.168.1.5/24`): every proxy
/// wranglarr writes for reads them as the network.
pub fn is_network(s: &str) -> bool {
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let Ok(ip) = addr.parse::<IpAddr>() else { return false };
    let Some(prefix) = prefix else { return true };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    !prefix.is_empty()
        && prefix.len() <= 3
        && prefix.bytes().all(|b| b.is_ascii_digit())
        && prefix.parse::<u32>().is_ok_and(|p| p <= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks() {
        for ok in ["192.168.1.0/24", "192.168.1.5/24", "10.0.0.1", "0.0.0.0/0", "fc00::/7", "::1/128", "fe80::1"] {
            assert!(is_network(ok), "{ok}");
        }
        for bad in [
            "",
            "lan",
            "not-a-range",
            "10.0.0.0/",
            "10.0.0.0/33",
            "10.0.0.0/+8",
            "010.0.0.1",
            "fc00::/129",
            "10.0.0.0/255.0.0.0",
            "::/1.2.3.4",
            " 10.0.0.0/8",
        ] {
            assert!(!is_network(bad), "{bad}");
        }
    }
}
