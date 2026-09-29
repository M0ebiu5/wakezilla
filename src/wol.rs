use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::sleep;
use tracing::{debug, info, instrument, warn};

/// Send WOL magic packets.
#[instrument(name = "send_wol_packets", skip(mac, config))]
pub async fn send_packets(
    mac: &[u8; 6],
    port: u16,
    count: u32,
    config: &crate::config::Config,
) -> Result<()> {
    let bcast = config.get_default_broadcast_addr();
    let packet = build_magic_packet(mac);
    debug!(
        "Built WOL magic packet for MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    // Use a UDP socket with broadcast enabled
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("Failed to bind UDP socket")?;
    sock.set_broadcast(true)
        .context("Failed to enable broadcast on socket")?;

    let addr = SocketAddrV4::new(bcast, port);
    info!("Sending {} WOL packets to {}:{}", count, bcast, port);

    for i in 0..count {
        debug!("Sending WOL packet {}/{}", i + 1, count);
        sock.send_to(&packet, addr)
            .await
            .context("Failed to send WOL packet")?;
        tokio::time::sleep(config.wol_packet_sleeptime()).await;
    }

    info!(
        "Successfully sent {} WOL packets to {}:{}",
        count, bcast, port
    );
    Ok(())
}

/// Poll a TCP port on a host until it becomes reachable or a timeout is hit.
#[instrument(name = "check_host_reachability", skip(ip, _config))]
pub async fn check_host(
    ip: IpAddr,
    check_tcp_port: u16,
    wait_secs: u64,
    interval_ms: u64,
    connect_timeout_ms: u64,
    _config: &crate::config::Config,
) -> bool {
    let poll_every = Duration::from_millis(interval_ms);
    let connect_timeout = Duration::from_millis(connect_timeout_ms);
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    let target = SocketAddr::new(ip, check_tcp_port);

    info!(
        "Waiting up to {}s for {}:{} ...",
        wait_secs, ip, check_tcp_port
    );

    loop {
        debug!("Checking if {}:{} is reachable", ip, check_tcp_port);
        if tcp_check(target, connect_timeout).await {
            info!("Host {}:{} is UP ✅", ip, check_tcp_port);
            return true;
        }

        if Instant::now() >= deadline {
            warn!("TIMEOUT ❌ waiting for {}:{}", ip, check_tcp_port);
            return false;
        }

        debug!(
            "Host {}:{} not reachable, waiting {:?} before next check",
            ip, check_tcp_port, poll_every
        );

        sleep(poll_every).await;
    }
}

/// One-shot TCP "ping": returns true if connect succeeds within timeout.
pub async fn tcp_check(addr: SocketAddr, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Parse MAC address from common string formats.
#[instrument(name = "parse_mac", skip(s))]
pub fn parse_mac(s: &str) -> Result<[u8; 6]> {
    // Keep only hex digits
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        anyhow::bail!("expected 12 hex digits, got {}", hex.len());
    }
    let mut mac = [0u8; 6];
    for i in 0..6 {
        mac[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).with_context(|| {
            format!(
                "invalid hex in MAC at position {}: '{}'",
                i,
                &hex[2 * i..2 * i + 2]
            )
        })?;
    }
    debug!(
        "Successfully parsed MAC address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    Ok(mac)
}

/// Build WOL magic packet: 6 x 0xFF + 16 repetitions of the MAC.
fn build_magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut pkt = [0u8; 102];
    // 6 bytes of 0xFF
    pkt[0..6].fill(0xFF);
    // 16 repetitions of MAC
    for i in 0..16 {
        let start = 6 + i * 6;
        pkt[start..start + 6].copy_from_slice(mac);
    }
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn magic_packet_has_sync_stream_and_repeated_mac() {
        let mac = [0xDE, 0xAD, 0xBE, 0xEF, 0xFE, 0xED];
        let packet = build_magic_packet(&mac);

        assert_eq!(packet.len(), 102, "magic packet must be 102 bytes");
        assert!(
            packet.iter().take(6).all(|&b| b == 0xFF),
            "packet must start with six 0xFF bytes"
        );

        for (idx, chunk) in packet[6..].chunks_exact(6).enumerate() {
            assert_eq!(chunk, mac, "MAC repetition {} does not match", idx + 1);
        }
    }

    #[test]
    fn parse_mac_accepts_common_formats() {
        let expected = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let inputs = [
            "aa:bb:cc:dd:ee:ff",
            "AA-BB-CC-DD-EE-FF",
            "aabb.ccdd.eeff",
            "AABBCCDDEEFF",
        ];

        for input in inputs {
            let mac = parse_mac(input).expect("MAC should parse");
            assert_eq!(
                mac, expected,
                "parsed MAC did not match for input '{}':",
                input
            );
        }
    }

    #[test]
    fn parse_mac_rejects_invalid_input() {
        let invalid_inputs = [
            "",
            "1234567890ABCD",
            "zz:zz:zz:zz:zz:zz",
            "aa-bb-cc-dd-ee",
            "aa:bb:cc:dd:ee:ff:11",
        ];

        for input in invalid_inputs {
            assert!(
                parse_mac(input).is_err(),
                "expected error for input '{}'",
                input
            );
        }
    }

    #[tokio::test]
    async fn tcp_check_reports_true_when_server_listening() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping tcp_check_reports_true_when_server_listening: {}",
                    err
                );
                return;
            }
            Err(err) => panic!("failed to bind listener: {err}"),
        };
        let addr = listener.local_addr().expect("failed to get addr");
        assert!(tcp_check(addr, Duration::from_millis(100)).await);
        drop(listener);
    }

    #[tokio::test]
    async fn check_host_returns_false_when_unreachable() {
        let config = Config::default();
        let result = check_host(IpAddr::V4(Ipv4Addr::LOCALHOST), 65_000, 0, 10, 10, &config).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn check_host_returns_true_when_host_up() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                eprintln!("skipping check_host_returns_true_when_host_up: {}", err);
                return;
            }
            Err(err) => panic!("failed to bind listener: {err}"),
        };
        let addr = listener.local_addr().expect("failed to get addr");
        let config = Config::default();
        let is_up = check_host(addr.ip(), addr.port(), 1, 10, 50, &config).await;
        assert!(is_up);
        drop(listener);
    }
}
