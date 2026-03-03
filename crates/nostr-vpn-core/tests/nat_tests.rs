use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

use nostr_vpn_core::nat::{discover_public_udp_endpoint, hole_punch_udp};

#[test]
fn discovers_public_endpoint_from_reflector() {
    let reflector = UdpSocket::bind("127.0.0.1:0").expect("bind reflector");
    reflector
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");
    let reflector_addr = reflector.local_addr().expect("reflector addr");

    let server = thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let (read, src) = reflector.recv_from(&mut buf).expect("receive discover");
        let payload = std::str::from_utf8(&buf[..read]).expect("utf8");
        assert!(payload.starts_with("NVPN_DISCOVER"));

        let response = format!("NVPN_ENDPOINT {}", src);
        reflector
            .send_to(response.as_bytes(), src)
            .expect("send response");
    });

    let discovered = discover_public_udp_endpoint(reflector_addr, 51820, Duration::from_secs(2))
        .expect("discover endpoint");
    assert!(
        discovered.ends_with(":51820"),
        "discovered endpoint was {discovered}"
    );

    server.join().expect("reflector thread");
}

#[test]
fn hole_punch_sends_and_receives_packets() {
    let receiver = UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set timeout");
    let receiver_addr = receiver.local_addr().expect("receiver addr");

    let server = thread::spawn(move || {
        let mut total = 0usize;
        let mut buf = [0u8; 1024];
        while total < 3 {
            let (read, src) = receiver.recv_from(&mut buf).expect("receive probe");
            let payload = &buf[..read];
            if payload.starts_with(b"NVPN_PUNCH") {
                total += 1;
                receiver.send_to(b"NVPN_ACK", src).expect("send ack");
            }
        }
    });

    let report = hole_punch_udp(
        0,
        receiver_addr,
        3,
        Duration::from_millis(20),
        Duration::from_millis(100),
    )
    .expect("hole punch");

    assert_eq!(report.packets_sent, 3);
    assert!(
        report.packet_received,
        "expected to receive at least one ack"
    );

    server.join().expect("receiver thread");
}
