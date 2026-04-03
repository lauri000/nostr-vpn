use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use tokio::net::UdpSocket;

use super::parse_advertise_ip;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelayPortRange {
    pub(crate) start: u16,
    pub(crate) end: u16,
}

impl RelayPortRange {
    pub(crate) fn new(start: u16, end: u16) -> Result<Self> {
        if start == 0 || end == 0 {
            return Err(anyhow!("relay port range cannot include port 0"));
        }
        if end < start {
            return Err(anyhow!(
                "invalid relay port range {start}-{end}: end must be >= start"
            ));
        }

        Ok(Self { start, end })
    }

    pub(crate) fn len(self) -> usize {
        usize::from(self.end - self.start) + 1
    }

    pub(crate) fn capacity_sessions(self) -> usize {
        self.len() / 2
    }

    pub(crate) fn contains(self, port: u16) -> bool {
        (self.start..=self.end).contains(&port)
    }

    pub(crate) fn next_after(self, port: u16) -> u16 {
        if port >= self.end {
            self.start
        } else {
            port + 1
        }
    }
}

#[derive(Debug)]
pub(crate) struct RelayPortAllocator {
    range: RelayPortRange,
    next_port: u16,
}

impl RelayPortAllocator {
    pub(crate) fn new(range: RelayPortRange) -> Self {
        Self {
            range,
            next_port: range.start,
        }
    }

    pub(crate) fn bind_pair(
        &mut self,
        bind_ip: IpAddr,
        advertise_host: &str,
    ) -> Result<(Arc<UdpSocket>, Arc<UdpSocket>, String, String)> {
        let advertise_ip = parse_advertise_ip(advertise_host)?;
        let mut first_port = self.next_port;
        let start_port = self.next_port;
        let mut tried_first = false;

        while !tried_first || first_port != start_port {
            tried_first = true;
            let requester_bind_addr = SocketAddr::new(bind_ip, first_port);
            let requester_socket = match bind_udp_socket(requester_bind_addr) {
                Ok(socket) => socket,
                Err(error) if io_error_kind(&error) == Some(ErrorKind::AddrInUse) => {
                    first_port = self.range.next_after(first_port);
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to bind requester relay leg on {requester_bind_addr}")
                    });
                }
            };

            let requester_addr = requester_socket
                .local_addr()
                .context("failed to read requester leg addr")?;
            let second_start = self.range.next_after(first_port);
            let mut second_port = second_start;
            let mut tried_second = false;

            while !tried_second || second_port != second_start {
                tried_second = true;
                if second_port == first_port {
                    break;
                }

                let target_bind_addr = SocketAddr::new(bind_ip, second_port);
                match bind_udp_socket(target_bind_addr) {
                    Ok(target_socket) => {
                        let target_addr = target_socket
                            .local_addr()
                            .context("failed to read target leg addr")?;
                        self.next_port = self.range.next_after(second_port);
                        let requester_ingress_endpoint =
                            SocketAddr::new(advertise_ip, requester_addr.port()).to_string();
                        let target_ingress_endpoint =
                            SocketAddr::new(advertise_ip, target_addr.port()).to_string();
                        return Ok((
                            requester_socket,
                            target_socket,
                            requester_ingress_endpoint,
                            target_ingress_endpoint,
                        ));
                    }
                    Err(error) if io_error_kind(&error) == Some(ErrorKind::AddrInUse) => {
                        second_port = self.range.next_after(second_port);
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to bind target relay leg on {target_bind_addr}")
                        });
                    }
                }
            }

            drop(requester_socket);
            first_port = self.range.next_after(first_port);
        }

        Err(anyhow!(
            "no free relay port pair available in configured range {}-{}",
            self.range.start,
            self.range.end
        ))
    }
}

pub(crate) fn bind_relay_leg_pair(
    bind_ip: IpAddr,
    advertise_host: &str,
    relay_port_allocator: Option<&Arc<StdMutex<RelayPortAllocator>>>,
) -> Result<(Arc<UdpSocket>, Arc<UdpSocket>, String, String)> {
    if let Some(relay_port_allocator) = relay_port_allocator {
        let mut relay_port_allocator = relay_port_allocator
            .lock()
            .map_err(|_| anyhow!("relay port allocator poisoned"))?;
        return relay_port_allocator.bind_pair(bind_ip, advertise_host);
    }

    let requester_socket = bind_udp_socket(SocketAddr::new(bind_ip, 0))
        .with_context(|| format!("failed to bind requester leg on {bind_ip}"))?;
    let target_socket = bind_udp_socket(SocketAddr::new(bind_ip, 0))
        .with_context(|| format!("failed to bind target leg on {bind_ip}"))?;
    let requester_addr = requester_socket
        .local_addr()
        .context("failed to read requester leg addr")?;
    let target_addr = target_socket
        .local_addr()
        .context("failed to read target leg addr")?;
    let advertise_ip = parse_advertise_ip(advertise_host)?;
    let requester_ingress_endpoint =
        SocketAddr::new(advertise_ip, requester_addr.port()).to_string();
    let target_ingress_endpoint = SocketAddr::new(advertise_ip, target_addr.port()).to_string();
    Ok((
        requester_socket,
        target_socket,
        requester_ingress_endpoint,
        target_ingress_endpoint,
    ))
}

pub(crate) fn relay_port_range(args: &super::Args) -> Result<Option<RelayPortRange>> {
    match (args.relay_port_range_start, args.relay_port_range_end) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(anyhow!(
            "both --relay-port-range-start and --relay-port-range-end are required together"
        )),
        (Some(_), Some(_)) if args.disable_relay => Err(anyhow!(
            "relay port range flags cannot be used with --disable-relay"
        )),
        (Some(start), Some(end)) => {
            let range = RelayPortRange::new(start, end)?;
            if range.capacity_sessions() == 0 {
                return Err(anyhow!(
                    "relay port range {}-{} must contain at least 2 ports",
                    range.start,
                    range.end
                ));
            }
            if args.enable_nat_assist && range.contains(args.nat_assist_port) {
                return Err(anyhow!(
                    "nat assist port {} overlaps relay port range {}-{}",
                    args.nat_assist_port,
                    range.start,
                    range.end
                ));
            }
            if args.max_active_sessions > range.capacity_sessions() {
                return Err(anyhow!(
                    "relay port range {}-{} supports at most {} simultaneous sessions; lower --max-active-sessions or widen the range",
                    range.start,
                    range.end,
                    range.capacity_sessions()
                ));
            }
            Ok(Some(range))
        }
    }
}

pub(crate) fn bind_udp_socket(bind_addr: SocketAddr) -> Result<Arc<UdpSocket>> {
    let socket = std::net::UdpSocket::bind(bind_addr)
        .with_context(|| format!("failed to bind udp socket on {bind_addr}"))?;
    socket
        .set_nonblocking(true)
        .with_context(|| format!("failed to set nonblocking on udp socket {bind_addr}"))?;
    let socket = UdpSocket::from_std(socket)
        .with_context(|| format!("failed to create async udp socket for {bind_addr}"))?;
    Ok(Arc::new(socket))
}

fn io_error_kind(error: &anyhow::Error) -> Option<ErrorKind> {
    error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
}
