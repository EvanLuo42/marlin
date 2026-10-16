//! `marlin-store [--root <dir>] [--listen <addr>]`: serves a shared cache (default
//! `./store` on port 47100, all interfaces).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use marlin_store::{Server, Store, protocol};

fn main() -> ExitCode {
    let mut root = String::from("store");
    let mut listen: SocketAddr = SocketAddr::from(([0, 0, 0, 0], protocol::PORT));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match (flag.as_str(), rest.next()) {
            ("--root", Some(r)) => root = r.clone(),
            ("--listen", Some(a)) => match a.parse() {
                Ok(a) => listen = a,
                Err(_) => {
                    eprintln!("--listen: not an address: {a}");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("usage: marlin-store [--root <dir>] [--listen <addr>]");
                return ExitCode::from(2);
            }
        }
    }
    let mut store = match Store::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{root}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut server = match Server::bind(listen) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "marlin-store: {} entries in {root}, listening on {}",
        store.data().len(),
        server.local_addr()
    );
    let stop = AtomicBool::new(false);
    match server.run(&mut store, &stop, Duration::from_millis(100)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
