//! The store over real sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use marlin_cook_core::Ddc;
use marlin_net::TcpStream;
use marlin_store::{Connection, Refs, RemoteDdc, Server, Store, StoreClient, protocol};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "marlin-store-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A store serving on a free local port until dropped.
struct Running {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(root: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut store = Store::open(&root).unwrap();
            let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            tx.send(server.local_addr()).unwrap();
            server
                .run(&mut store, &s, Duration::from_millis(5))
                .unwrap();
        });
        Self {
            addr: rx.recv().unwrap(),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

#[test]
fn requests_and_responses() {
    let root = dir("basic");
    let server = Running::start(root.clone());
    let mut c = StoreClient::connect(server.addr).unwrap();
    assert_eq!(c.get(1).unwrap(), None);
    assert!(!c.has(1).unwrap());
    c.put(1, b"one").unwrap();
    c.put(2, &[]).unwrap();
    assert_eq!(c.get(1).unwrap().unwrap(), b"one");
    assert_eq!(c.get(2).unwrap().unwrap(), b"");
    assert_eq!(c.has_many(&[1, 2, 3]).unwrap(), [true, true, false]);

    // Big entries cross many receives.
    let big: Vec<u8> = (0..5_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    c.put(3, &big).unwrap();
    assert_eq!(c.get(3).unwrap().unwrap(), big);

    // Pipelined: 500 requests in one write.
    for k in 100..600u128 {
        c.put(k, &k.to_le_bytes()).unwrap();
    }
    let keys: Vec<u128> = (100..600).chain([7777]).collect();
    let got = c.get_many(&keys).unwrap();
    for (k, v) in keys.iter().zip(&got) {
        match k {
            7777 => assert!(v.is_none()),
            _ => assert_eq!(v.as_deref().unwrap(), &k.to_le_bytes()),
        }
    }
    let (entries, bytes, refs) = c.stats().unwrap();
    assert_eq!((entries, refs), (503, 0));
    assert!(bytes > 5_000_000);
    drop(server);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn refs_compare_and_swap_and_persist() {
    let root = dir("refs");
    {
        let server = Running::start(root.clone());
        let mut c = StoreClient::connect(server.addr).unwrap();
        assert_eq!(c.ref_get("main").unwrap(), None);
        assert_eq!(c.ref_set("main", 10, None).unwrap(), Ok(()));
        assert_eq!(
            c.ref_set("main", 11, Some(9)).unwrap(),
            Err(Some(10)),
            "stale expectation"
        );
        assert_eq!(c.ref_set("main", 11, Some(10)).unwrap(), Ok(()));
        assert_eq!(c.ref_set("new", 1, Some(5)).unwrap(), Err(None));
        assert_eq!(c.ref_get("main").unwrap(), Some(11));
        c.put(42, b"kept").unwrap();
    }
    // A torn record at the end of the log is dropped.
    let log = root.join("refs.log");
    let mut bytes = std::fs::read(&log).unwrap();
    bytes.extend_from_slice(b"REF1\x04\x00\x00\x00ma");
    std::fs::write(&log, &bytes).unwrap();
    let refs = Refs::open(&log).unwrap();
    assert_eq!((refs.get("main"), refs.len()), (Some(11), 1));
    drop(refs);

    let server = Running::start(root.clone());
    let mut c = StoreClient::connect(server.addr).unwrap();
    assert_eq!(c.ref_get("main").unwrap(), Some(11));
    assert_eq!(c.get(42).unwrap().unwrap(), b"kept");
    drop(server);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn many_clients_at_once() {
    let root = dir("concurrent");
    let server = Running::start(root.clone());
    let addr = server.addr;
    std::thread::scope(|s| {
        for t in 0..8u128 {
            s.spawn(move || {
                let mut c = StoreClient::connect(addr).unwrap();
                for i in 0..200u128 {
                    let k = t << 64 | i;
                    c.put(k, &k.to_le_bytes().repeat((i % 5 + 1) as usize))
                        .unwrap();
                }
                for i in 0..200u128 {
                    let k = t << 64 | i;
                    assert_eq!(
                        c.get(k).unwrap().unwrap(),
                        k.to_le_bytes().repeat((i % 5 + 1) as usize)
                    );
                }
            });
        }
    });
    let mut c = StoreClient::connect(addr).unwrap();
    assert_eq!(c.stats().unwrap().0, 1600);
    drop(server);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_damaged_frame_closes_only_its_connection() {
    let root = dir("damaged");
    let server = Running::start(root.clone());
    let mut good = StoreClient::connect(server.addr).unwrap();
    good.put(1, b"fine").unwrap();

    let mut bad = TcpStream::connect(server.addr).unwrap();
    let mut frame = marlin_asset::wire::frame(protocol::GET, 1, &protocol::key(1));
    frame[18] ^= 0xff; // payload byte: the checksum no longer matches
    bad.send_all(&frame).unwrap();
    let mut buf = [0u8; 64];
    assert!(
        matches!(bad.recv(&mut buf), Ok(0) | Err(_)),
        "the server hangs up"
    );

    // A frame claiming a huge length is refused too, not allocated.
    let mut huge = TcpStream::connect(server.addr).unwrap();
    let mut header = marlin_asset::wire::frame(protocol::PUT, 1, &[]);
    header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    huge.send_all(&header).unwrap();
    assert!(matches!(huge.recv(&mut buf), Ok(0) | Err(_)));

    // Unknown requests are errors, not hang-ups.
    let mut raw = Connection::connect(server.addr, None).unwrap();
    let f = raw.call(999, &[]).unwrap();
    assert_eq!(f.kind, protocol::ERROR);

    assert_eq!(good.get(1).unwrap().unwrap(), b"fine");
    drop(server);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn remote_ddc_is_a_ddc() {
    let root = dir("remote");
    let server = Running::start(root.clone());
    let ddc = RemoteDdc::connect(server.addr).unwrap();
    assert!(!ddc.contains(5));
    ddc.put(5, b"cooked").unwrap();
    assert!(ddc.contains(5));
    assert_eq!(ddc.get(5).unwrap().unwrap(), b"cooked");
    // The server restarts: the client reconnects.
    drop(server);
    let server = Running::start(root.clone());
    let ddc2 = RemoteDdc::connect(server.addr).unwrap();
    assert_eq!(ddc2.get(5).unwrap().unwrap(), b"cooked");
    drop(server);
    assert!(ddc2.get(5).is_err(), "no server: an error, not a hang");
    assert!(!ddc2.contains(5));
    let _ = std::fs::remove_dir_all(root);
}
