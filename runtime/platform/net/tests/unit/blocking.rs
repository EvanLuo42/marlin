//! TcpStream, TcpListener and name resolution.

use std::time::{Duration, Instant};

use marlin_net::{
    ErrorKind, IoBuffer, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketOptions, TcpListener,
    TcpStream, resolve,
};
use marlin_test_support::{assert_duration_in, assert_waited};

use crate::support::{Mode, Server, any_port, buffer_from, pattern};

#[test]
fn listener_and_stream() {
    let mut listener = TcpListener::bind(any_port()).unwrap();
    let addr = listener.local_addr().unwrap();
    let data = pattern(3 << 20, 3);
    let expected = data.clone();
    let client = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), addr);
        stream.send_all(&data).unwrap();
        // Zero-copy variants: the buffer comes back.
        let buffer = stream.send_all_buffer(buffer_from(b"tail"), ..).unwrap();
        assert_eq!(&buffer[..], b"tail");
        let buffer = stream.recv_exact_buffer(IoBuffer::new(10), 2..7).unwrap();
        assert_eq!(&buffer[2..7], b"thank");
        stream.local_addr().unwrap()
    });

    let (mut stream, peer) = listener.accept().unwrap();
    let mut received = vec![0; expected.len() + 4];
    stream.recv_exact(&mut received).unwrap();
    assert!(received[..expected.len()] == expected);
    assert_eq!(&received[expected.len()..], b"tail");
    stream.send_all(b"thank you").unwrap();
    assert_eq!(client.join().unwrap(), peer);
}

#[test]
fn receive_timeout() {
    let server = Server::start(Mode::Discard);
    let mut stream = TcpStream::connect(server.addr).unwrap();
    stream.set_timeout(Some(Duration::from_millis(100)));
    let start = Instant::now();
    let mut buf = [0; 8];
    assert_eq!(
        stream.recv(&mut buf).unwrap_err().kind(),
        ErrorKind::TimedOut
    );
    assert_waited(start, Duration::from_millis(90));
    // Still usable after a timed-out receive.
    stream.send_all(b"still here").unwrap();
}

#[test]
fn connect_errors() {
    // Nothing listens there any more.
    let addr = TcpListener::bind(any_port()).unwrap().local_addr().unwrap();
    let error = TcpStream::connect(addr).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::ConnectionRefused);
    // Discard prefix (RFC 6666) goes nowhere: either refused by routing or timed out.
    let blackhole: SocketAddr = "[100::1]:9".parse().unwrap();
    let start = Instant::now();
    let error = TcpStream::connect_with(
        blackhole,
        &SocketOptions::default(),
        Some(Duration::from_millis(200)),
    )
    .err()
    .unwrap();
    assert_duration_in(start.elapsed(), ..Duration::from_secs(5));
    println!("blackhole: {error}");
}

#[test]
fn resolve_names() {
    let v4 = resolve("127.0.0.1", 80).unwrap();
    assert_eq!(v4, [SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80)]);
    let v6 = resolve("[::1]", 81).unwrap();
    assert_eq!(v6, [SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 81)]);

    let local = resolve("localhost", 8080).unwrap();
    assert!(!local.is_empty());
    assert!(
        local
            .iter()
            .all(|a| a.port() == 8080 && a.ip().is_loopback()),
        "{local:?}"
    );

    let missing = resolve("no-such-host.invalid", 1).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::NotFound, "{missing}");
    for bad in ["", "a\0b", &"x".repeat(300)] {
        assert_eq!(resolve(bad, 1).unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    let server = Server::start(Mode::Echo);
    let mut stream = TcpStream::connect_host(
        "localhost",
        server.addr.port(),
        &SocketOptions::default(),
        Some(Duration::from_secs(5)),
    )
    .unwrap();
    assert!(matches!(
        stream.peer_addr().unwrap().ip(),
        IpAddr::V4(_) | IpAddr::V6(_)
    ));
    stream.send_all(b"hi").unwrap();
    let mut buf = [0; 2];
    stream.recv_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hi");
}
