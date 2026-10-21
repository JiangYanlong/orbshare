use std::net::IpAddr;

/// Whether a peer address is valid for OrbShare's LAN-only listener.
///
/// Private IPv4, link-local addresses and local/unique-local IPv6 are allowed.
/// Public Internet addresses are rejected even when a host firewall exposes
/// the listening port accidentally.
pub fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_private_loopback_and_link_local_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.8",
            "172.16.3.4",
            "192.168.1.42",
            "169.254.4.2",
            "::1",
            "fd00::8",
            "fe80::1",
        ] {
            assert!(is_lan_ip(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn rejects_public_addresses() {
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_lan_ip(address.parse().unwrap()), "{address}");
        }
    }
}
