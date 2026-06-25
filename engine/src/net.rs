//! Endpoint-IP selection for the raw-UDP voice transport. The advertised IP is
//! the address a peer dials back on, which on an overlay (WireGuard/Tailscale)
//! differs from the default-route IP – hence the `HEARTH_ADVERTISE_IP` override.

/// Pick the IP to advertise: the trimmed `HEARTH_ADVERTISE_IP` value when set and
/// non-empty, otherwise the auto-detected route IP.
pub fn pick_advertise_ip(env: Option<String>, detected: &str) -> String {
    match env {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => detected.to_string(),
    }
}

/// Best-effort local IPv4 a peer can dial us on. Honors `HEARTH_ADVERTISE_IP`
/// (the overlay address on a WireGuard/Tailscale net), else discovers the
/// default-route interface via a connect to a public address (no packet is
/// sent), else loopback for same-machine testing.
pub fn advertised_ip() -> String {
    pick_advertise_ip(std::env::var("HEARTH_ADVERTISE_IP").ok(), &detect_route_ip())
}

/// Route-trick: the source IP the kernel would use to reach a public address,
/// i.e. the default-route interface. Loopback fallback when offline.
fn detect_route_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[cfg(test)]
mod tests {
    use super::pick_advertise_ip;

    #[test]
    fn env_override_wins_when_set() {
        assert_eq!(pick_advertise_ip(Some("100.64.0.2".into()), "192.168.1.5"), "100.64.0.2");
    }

    #[test]
    fn falls_back_to_detected_when_env_absent() {
        assert_eq!(pick_advertise_ip(None, "192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn empty_or_whitespace_env_falls_back_to_detected() {
        assert_eq!(pick_advertise_ip(Some("".into()), "192.168.1.5"), "192.168.1.5");
        assert_eq!(pick_advertise_ip(Some("   ".into()), "192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn env_value_is_trimmed() {
        assert_eq!(pick_advertise_ip(Some("  100.64.0.2  ".into()), "192.168.1.5"), "100.64.0.2");
    }
}
