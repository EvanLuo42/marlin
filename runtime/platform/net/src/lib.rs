//! Completion-based TCP sockets and name resolution.
//!
//! The network half of marlin-io's design, for the game's dev-streaming client (pipelined
//! requests for cooked units) and the `marlin-store` cache server (many connections, high request
//! rates):
//!
//! - [`NetQueue`]: the asynchronous core. It owns sockets ([`SocketId`]); submit connects,
//!   accepts, sends and receives of owned [`IoBuffer`]s, get [`Completed`] operations back with
//!   their buffers. Many operations per socket may be in flight, so requests pipeline.
//! - [`TcpStream`] / [`TcpListener`]: blocking convenience on top (`connect`, `send_all`,
//!   `recv_exact`), each over its own one-operation queue.
//! - [`resolve`]: name resolution. Addresses are `core::net`'s, re-exported here.
//! - [`SocketOptions`]: `TCP_NODELAY` (on by default), buffer sizes, keepalive, linger, and
//!   [`recv_when_readable`](SocketOptions::recv_when_readable) for servers with many small
//!   messages or idle connections.
//!
//! ```ignore
//! let mut queue = NetQueue::new(256)?;
//! let listener = queue.listen("127.0.0.1:0".parse()?, &ListenOptions::default())?;
//! queue.accept(listener)?;
//! while let Some(done) = queue.wait()? {
//!     match done {
//!         Completed::Accepted { listener, result: Ok((socket, _peer)), .. } => {
//!             queue.accept(listener)?;
//!             queue.recv(socket, IoBuffer::new(64 << 10), ..)?;
//!         }
//!         Completed::Received { socket, buffer, result: Ok(n @ 1..), .. } => {
//!             queue.send(socket, buffer, ..n)?; // echo
//!         }
//!         Completed::Sent { socket, buffer, result: Ok(_), .. } => {
//!             queue.recv(socket, buffer, ..)?;
//!         }
//!         other => queue.close(other.socket())?,
//!     }
//! }
//! ```
//!
//! # Backends
//!
//! | Platform | [`Engine`] |
//! |---|---|
//! | Windows | `Iocp`: `AcceptEx`/`ConnectEx`/`WSASend`/`WSARecv` on a completion port, with `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` |
//! | Linux, macOS | not written yet: [`NetQueue::new`] fails with `Unsupported` |
//!
//! Planned: Registered IO (RIO) on Windows, io_uring on Linux (the approach of marlin-io's file
//! queue), kqueue on macOS. The contract asks every backend for whole, in-order sends per socket,
//! which io_uring must provide by serializing each socket's sends itself.
//!
//! The PAL pattern: a `contract` trait, public types forwarding to `backend::imp`. Console
//! backends come from `platforms/<name>/net/`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod backend;
mod blocking;
mod contract;
mod error;
mod queue;
mod types;

use alloc::vec::Vec;

pub use blocking::{TcpListener, TcpStream};
pub use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
pub use error::{Error, ErrorKind, Result};
pub use marlin_io::IoBuffer;
pub use queue::{Completed, NetQueue, RequestId, Socket, SocketId, SubmitError};
pub use types::{
    Engine, Keepalive, ListenOptions, MAX_IO_LEN, MAX_QUEUE_CAPACITY, Shutdown, SocketOptions,
};

use crate::backend::imp::Backend;
use crate::contract::Net;

/// Longest host name [`resolve`] accepts, in bytes (DNS allows 253).
pub const MAX_HOST_LEN: usize = 255;

/// The TCP addresses of `host` (a name or an IP literal, IPv6 optionally in brackets), with
/// `port`. Blocks on the OS resolver (DNS, hosts file). `NotFound` when nothing resolves.
pub fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let literal = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = literal.parse::<IpAddr>() {
        return Ok(alloc::vec![SocketAddr::new(ip, port)]);
    }
    if host.is_empty() || host.len() > MAX_HOST_LEN || host.as_bytes().contains(&0) {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    let mut out = Vec::new();
    Backend::resolve(host, port, &mut out)?;
    if out.is_empty() {
        return Err(Error::from_kind(ErrorKind::NotFound));
    }
    Ok(out)
}

// The selected backend must implement the whole contract.
const _: () = {
    const fn assert_net<N: contract::Net>() {}
    assert_net::<backend::imp::Backend>();
};
