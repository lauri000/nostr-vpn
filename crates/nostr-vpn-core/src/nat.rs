use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};

pub const DISCOVER_REQUEST_PREFIX: &str = "NVPN_DISCOVER";
pub const ENDPOINT_RESPONSE_PREFIX: &str = "NVPN_ENDPOINT";
pub const PUNCH_REQUEST_PREFIX: &str = "NVPN_PUNCH";
pub const PUNCH_ACK_PREFIX: &str = "NVPN_ACK";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolePunchReport {
    pub packets_sent: u32,
    pub packet_received: bool,
    pub local_addr: SocketAddr,
}

pub fn discover_public_udp_endpoint(
    reflector_addr: SocketAddr,
    listen_port: u16,
    timeout: Duration,
) -> Result<String> {
    let bind_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, listen_port));
    let socket = UdpSocket::bind(bind_addr)
        .with_context(|| format!("failed to bind udp discovery socket on {bind_addr}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .context("failed to set udp discovery read timeout")?;
    socket
        .set_write_timeout(Some(timeout))
        .context("failed to set udp discovery write timeout")?;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let request = format!("{DISCOVER_REQUEST_PREFIX} {nonce}");

    socket
        .send_to(request.as_bytes(), reflector_addr)
        .with_context(|| format!("failed to send udp discovery probe to {reflector_addr}"))?;

    let mut buf = [0u8; 1024];
    let (read, _) = socket
        .recv_from(&mut buf)
        .context("failed to receive udp discovery response")?;

    let payload =
        std::str::from_utf8(&buf[..read]).context("udp discovery response was not utf8")?;
    parse_public_endpoint_response(payload)
}

pub fn hole_punch_udp(
    listen_port: u16,
    peer_endpoint: SocketAddr,
    attempts: u32,
    interval: Duration,
    recv_timeout: Duration,
) -> Result<HolePunchReport> {
    if attempts == 0 {
        return Err(anyhow!("attempts must be > 0"));
    }

    let bind_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, listen_port));
    let socket = UdpSocket::bind(bind_addr)
        .with_context(|| format!("failed to bind udp hole-punch socket on {bind_addr}"))?;
    socket
        .set_read_timeout(Some(recv_timeout))
        .context("failed to set udp hole-punch read timeout")?;
    socket
        .set_write_timeout(Some(recv_timeout))
        .context("failed to set udp hole-punch write timeout")?;

    let local_addr = socket
        .local_addr()
        .context("failed to read udp hole-punch local addr")?;

    let mut packets_sent = 0u32;
    let mut packet_received = false;
    let mut recv_buf = [0u8; 256];

    for attempt in 0..attempts {
        let payload = format!("{PUNCH_REQUEST_PREFIX} {attempt}");
        socket
            .send_to(payload.as_bytes(), peer_endpoint)
            .with_context(|| format!("failed to send hole-punch packet to {peer_endpoint}"))?;
        packets_sent += 1;

        if let Ok((read, src)) = socket.recv_from(&mut recv_buf)
            && src == peer_endpoint
            && read > 0
        {
            packet_received = true;
        }

        if attempt + 1 < attempts {
            thread::sleep(interval);
        }
    }

    Ok(HolePunchReport {
        packets_sent,
        packet_received,
        local_addr,
    })
}

fn parse_public_endpoint_response(payload: &str) -> Result<String> {
    let Some(value) = payload.strip_prefix(ENDPOINT_RESPONSE_PREFIX) else {
        return Err(anyhow!(
            "invalid discovery response: expected '{ENDPOINT_RESPONSE_PREFIX} <ip:port>'"
        ));
    };

    let endpoint = value.trim();
    if endpoint.is_empty() {
        return Err(anyhow!("invalid discovery response: empty endpoint"));
    }

    let parsed: SocketAddr = endpoint
        .parse()
        .with_context(|| format!("invalid discovery endpoint '{endpoint}'"))?;

    Ok(parsed.to_string())
}
