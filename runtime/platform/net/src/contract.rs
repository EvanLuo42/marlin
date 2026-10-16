//! The backend contract.
//!
//! A backend is a unit type `Backend` implementing [`Net`], with associated types for sockets,
//! queues and per-operation request storage: the shape of marlin-pal's file queue contract. The
//! public types in `queue.rs` own the sockets and the requests and forward here.

use alloc::vec::Vec;
use core::net::SocketAddr;
use core::ptr::NonNull;
use core::time::Duration;

use crate::types::{Op, RawCompletion, SockOpt};
use crate::{Engine, Result, Shutdown};

/// # Safety
///
/// A queue must report every pushed operation exactly once, and must not touch its request or
/// buffer after reporting it.
///
/// Sends complete whole (all bytes or an error) and, like receives, in the order they were pushed
/// for their socket. A backend whose OS does not promise that (io_uring under backpressure)
/// serializes each socket's sends and receives itself.
pub(crate) unsafe trait Net {
    /// An open socket; dropping it closes it. Queues refer to it by OS handle, so it may move.
    type Socket: Send;
    /// A completion queue; dropping it releases OS resources. The caller drains it first.
    type Queue: Send;
    /// Per-operation storage the caller owns, pinned from push until its completion is polled.
    type Request;
    const REQUEST_INIT: Self::Request;

    /// A socket ready for a connect operation to `family`'s addresses.
    fn socket_for(peer: &SocketAddr) -> Result<Self::Socket>;
    /// A socket bound to `addr` and listening.
    fn listen(addr: &SocketAddr, backlog: u32) -> Result<Self::Socket>;
    fn local_addr(socket: &Self::Socket) -> Result<SocketAddr>;
    fn peer_addr(socket: &Self::Socket) -> Result<SocketAddr>;
    fn set_option(socket: &Self::Socket, option: SockOpt, value: i32) -> Result<()>;
    fn option(socket: &Self::Socket, option: SockOpt) -> Result<i32>;
    fn shutdown(socket: &Self::Socket, how: Shutdown) -> Result<()>;
    /// Appends `host`'s TCP addresses, with `port`. `host` is not empty and has no NUL.
    fn resolve(host: &str, port: u16, out: &mut Vec<SocketAddr>) -> Result<()>;

    /// `capacity` is in `1..=MAX_QUEUE_CAPACITY`. `fallback` asks for the portable engine.
    fn queue_new(capacity: u32, fallback: bool) -> Result<Self::Queue>;
    fn queue_engine(queue: &Self::Queue) -> Engine;
    /// Prepares `op`; the engine may also start it. Errors here mean the operation was not
    /// queued; IO errors are reported through its completion instead.
    ///
    /// # Safety
    /// Fewer than `capacity` operations are in flight. `request`, the buffer and the socket stay
    /// valid (the socket may move) until the completion is polled.
    unsafe fn queue_push(
        queue: &mut Self::Queue,
        request: NonNull<Self::Request>,
        op: &Op<'_, Self::Socket>,
    ) -> Result<()>;
    /// Asks the OS to cancel an operation. It still completes, with `Cancelled` or, if it won the
    /// race, normally.
    ///
    /// # Safety
    /// `request` was pushed on `socket` and its completion has not been polled.
    unsafe fn queue_cancel(
        queue: &mut Self::Queue,
        request: NonNull<Self::Request>,
        socket: &Self::Socket,
    );
    /// Hands prepared operations to the OS.
    fn queue_submit(queue: &mut Self::Queue) -> Result<()>;
    /// Submits, then writes up to `out.len()` (at least 1) completions, waiting up to `timeout`
    /// (`None` = forever) for the first. At least one operation is in flight.
    fn queue_poll(
        queue: &mut Self::Queue,
        out: &mut [RawCompletion],
        timeout: Option<Duration>,
    ) -> Result<usize>;

    /// Turns a polled accept completion into the accepted socket and its peer, releasing what
    /// the request holds on failure.
    ///
    /// # Safety
    /// `request` carried an accept on `listener`, and this is its polled completion's result.
    unsafe fn accept_result(
        request: NonNull<Self::Request>,
        listener: &Self::Socket,
        result: Result<usize>,
    ) -> Result<(Self::Socket, SocketAddr)>;
    /// Finishes a polled connect completion.
    fn connect_result(socket: &Self::Socket, result: Result<usize>) -> Result<()>;
}
