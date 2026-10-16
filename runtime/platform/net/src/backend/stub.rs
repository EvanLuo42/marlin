//! Placeholder for platforms without a network backend yet: queues and sockets fail with
//! `Unsupported`, so the other types can never exist. IP literals still resolve.

use alloc::vec::Vec;
use core::net::SocketAddr;
use core::ptr::NonNull;
use core::time::Duration;

use crate::types::{Op, RawCompletion, SockOpt};
use crate::{Engine, Error, ErrorKind, Result, Shutdown};

pub(crate) struct Backend;

pub(crate) enum Never {}

fn unsupported<T>() -> Result<T> {
    Err(Error::from_kind(ErrorKind::Unsupported))
}

// SAFETY: no queue can be created, so no operation is ever pushed.
unsafe impl crate::contract::Net for Backend {
    type Socket = Never;
    type Queue = Never;
    type Request = u8;
    const REQUEST_INIT: u8 = 0;

    fn socket_for(_: &SocketAddr) -> Result<Never> {
        unsupported()
    }

    fn listen(_: &SocketAddr, _: u32) -> Result<Never> {
        unsupported()
    }

    fn local_addr(socket: &Never) -> Result<SocketAddr> {
        match *socket {}
    }

    fn peer_addr(socket: &Never) -> Result<SocketAddr> {
        match *socket {}
    }

    fn set_option(socket: &Never, _: SockOpt, _: i32) -> Result<()> {
        match *socket {}
    }

    fn option(socket: &Never, _: SockOpt) -> Result<i32> {
        match *socket {}
    }

    fn shutdown(socket: &Never, _: Shutdown) -> Result<()> {
        match *socket {}
    }

    fn resolve(_: &str, _: u16, _: &mut Vec<SocketAddr>) -> Result<()> {
        unsupported()
    }

    fn queue_new(_: u32, _: bool) -> Result<Never> {
        unsupported()
    }

    fn queue_engine(queue: &Never) -> Engine {
        match *queue {}
    }

    unsafe fn queue_push(queue: &mut Never, _: NonNull<u8>, _: &Op<'_, Never>) -> Result<()> {
        match *queue {}
    }

    unsafe fn queue_cancel(queue: &mut Never, _: NonNull<u8>, _: &Never) {
        match *queue {}
    }

    fn queue_submit(queue: &mut Never) -> Result<()> {
        match *queue {}
    }

    fn queue_poll(
        queue: &mut Never,
        _: &mut [RawCompletion],
        _: Option<Duration>,
    ) -> Result<usize> {
        match *queue {}
    }

    unsafe fn accept_result(
        _: NonNull<u8>,
        listener: &Never,
        _: Result<usize>,
    ) -> Result<(Never, SocketAddr)> {
        match *listener {}
    }

    fn connect_result(socket: &Never, _: Result<usize>) -> Result<()> {
        match *socket {}
    }
}
