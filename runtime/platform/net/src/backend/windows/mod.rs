//! Winsock 2 with overlapped IO on a completion port. The Xbox backend reuses most of it.

mod iocp;

use alloc::vec::Vec;
use core::cell::Cell;
use core::mem::size_of;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_CONNECTION_ABORTED, ERROR_CONNECTION_REFUSED,
    ERROR_HOST_UNREACHABLE, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NETNAME_DELETED,
    ERROR_NETWORK_UNREACHABLE, ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED,
    ERROR_OPERATION_ABORTED, ERROR_OUTOFMEMORY, ERROR_PORT_UNREACHABLE, ERROR_REQUEST_ABORTED,
    ERROR_SEM_TIMEOUT, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    ADDRINFOW, AF_INET, AF_INET6, AF_UNSPEC, FIONBIO, FreeAddrInfoW, GetAddrInfoW, IN_ADDR,
    IN_ADDR_0, IN6_ADDR, IN6_ADDR_0, INVALID_SOCKET, IPPROTO_TCP, LINGER, SD_BOTH, SD_RECEIVE,
    SD_SEND, SIO_GET_EXTENSION_FUNCTION_POINTER, SO_KEEPALIVE, SO_LINGER, SO_RCVBUF, SO_SNDBUF,
    SO_UPDATE_ACCEPT_CONTEXT, SO_UPDATE_CONNECT_CONTEXT, SOCK_STREAM, SOCKADDR, SOCKADDR_IN,
    SOCKADDR_IN6, SOCKADDR_IN6_0, SOCKADDR_STORAGE, SOCKET, SOCKET_ERROR, SOL_SOCKET, TCP_KEEPCNT,
    TCP_KEEPIDLE, TCP_KEEPINTVL, TCP_NODELAY, WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED,
    WSADATA, WSAGetLastError, WSAID_ACCEPTEX, WSAID_CONNECTEX, WSAIoctl, WSASocketW, WSAStartup,
    bind, closesocket, getpeername, getsockname, getsockopt, ioctlsocket, listen, setsockopt,
    shutdown,
};
use windows_sys::core::GUID;

use crate::types::{Op, RawCompletion, SockOpt};
use crate::{Engine, Error, ErrorKind, Result, Shutdown};

pub(crate) use iocp::{Port, Request};

pub(crate) struct Backend;

pub(crate) struct Socket {
    raw: SOCKET,
    family: u16,
    /// Id of the completion port the socket is bound to, or 0. Binding is for the socket's life.
    port: Cell<usize>,
    /// Whether operations that complete inside the call skip the port. Set with the binding;
    /// `SetFileCompletionNotificationModes` may refuse it (non-IFS layered providers).
    skip_port: Cell<bool>,
    /// `SocketOptions::recv_when_readable`: receives post a zero-byte `WSARecv` and `recv` once
    /// it completes. The socket is non-blocking from then on, so that `recv` never waits.
    zero_byte: Cell<bool>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: we own the socket and do not use it again.
        unsafe { closesocket(self.raw) };
    }
}

// SAFETY: queues report each operation exactly once, after the OS is done with it (see iocp.rs).
// Sends complete whole and in order: overlapped `WSASend` on a stream socket finishes only when
// all its bytes are buffered, and AFD serves a socket's sends and receives in posting order.
unsafe impl crate::contract::Net for Backend {
    type Socket = Socket;
    type Queue = Port;
    type Request = Request;
    const REQUEST_INIT: Request = iocp::REQUEST_INIT;

    fn socket_for(peer: &SocketAddr) -> Result<Socket> {
        let socket = Socket::new(family(peer))?;
        // ConnectEx needs a bound socket.
        let any = match peer {
            SocketAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
            SocketAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
        };
        let (addr, len) = to_raw(&any);
        // SAFETY: `addr` holds a sockaddr of `len` bytes.
        check(unsafe { bind(socket.raw, addr.as_ptr(), len) })?;
        Ok(socket)
    }

    fn listen(addr: &SocketAddr, backlog: u32) -> Result<Socket> {
        let socket = Socket::new(family(addr))?;
        let (raw, len) = to_raw(addr);
        // SAFETY: `raw` holds a sockaddr of `len` bytes.
        check(unsafe { bind(socket.raw, raw.as_ptr(), len) })?;
        // SOMAXCONN_HINT(n) is -n: a backlog of n, clamped to 200..=65535 on every SKU (plain
        // SOMAXCONN is 200 on client Windows).
        let backlog = backlog.clamp(200, 65535) as i32;
        // SAFETY: plain call on a live socket.
        check(unsafe { listen(socket.raw, -backlog) })?;
        Ok(socket)
    }

    fn local_addr(socket: &Socket) -> Result<SocketAddr> {
        // SAFETY: getsockname writes at most `len` bytes.
        read_addr(|addr, len| unsafe { getsockname(socket.raw, addr, len) })
    }

    fn peer_addr(socket: &Socket) -> Result<SocketAddr> {
        // SAFETY: getpeername writes at most `len` bytes.
        read_addr(|addr, len| unsafe { getpeername(socket.raw, addr, len) })
    }

    fn set_option(socket: &Socket, option: SockOpt, value: i32) -> Result<()> {
        if option == SockOpt::RecvWhenReadable {
            if value != 0 && !socket.zero_byte.get() {
                // Overlapped calls ignore the mode; only the follow-up `recv` sees it.
                let mut nonblocking = 1u32;
                // SAFETY: plain call on a live socket; `nonblocking` is a valid in pointer.
                check(unsafe { ioctlsocket(socket.raw, FIONBIO, &mut nonblocking) })?;
            }
            socket.zero_byte.set(value != 0);
            return Ok(());
        }
        if option == SockOpt::Linger {
            let linger = LINGER {
                l_onoff: u16::from(value >= 0),
                l_linger: value.clamp(0, u16::MAX.into()) as u16,
            };
            return set(socket.raw, SOL_SOCKET, SO_LINGER, &linger);
        }
        let (level, name) = option_name(option);
        set(socket.raw, level, name, &value)
    }

    fn option(socket: &Socket, option: SockOpt) -> Result<i32> {
        if option == SockOpt::RecvWhenReadable {
            return Ok(socket.zero_byte.get().into());
        }
        if option == SockOpt::Linger {
            let linger: LINGER = get(socket.raw, SOL_SOCKET, SO_LINGER)?;
            return Ok(if linger.l_onoff != 0 {
                linger.l_linger.into()
            } else {
                -1
            });
        }
        let (level, name) = option_name(option);
        get(socket.raw, level, name)
    }

    fn shutdown(socket: &Socket, how: Shutdown) -> Result<()> {
        let how = match how {
            Shutdown::Read => SD_RECEIVE,
            Shutdown::Write => SD_SEND,
            Shutdown::Both => SD_BOTH,
        };
        // SAFETY: plain call on a live socket.
        check(unsafe { shutdown(socket.raw, how) })
    }

    fn resolve(host: &str, port: u16, out: &mut Vec<SocketAddr>) -> Result<()> {
        startup()?;
        // At most MAX_HOST_LEN UTF-8 bytes, so at most as many UTF-16 units, plus the NUL.
        let mut wide = [0u16; crate::MAX_HOST_LEN + 1];
        for (dst, unit) in wide.iter_mut().zip(host.encode_utf16()) {
            *dst = unit;
        }
        let hints = ADDRINFOW {
            ai_family: AF_UNSPEC as i32,
            ai_socktype: SOCK_STREAM,
            ai_protocol: IPPROTO_TCP,
            ..ADDRINFOW::default()
        };
        let mut list = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `list` receives a list freed below.
        let code = unsafe { GetAddrInfoW(wide.as_ptr(), ptr::null(), &hints, &mut list) };
        if code != 0 {
            return Err(from_code(code as u32));
        }
        let mut node = list;
        while !node.is_null() {
            // SAFETY: `node` is an element of the list GetAddrInfoW returned.
            let info = unsafe { &*node };
            if !info.ai_addr.is_null() {
                // SAFETY: `ai_addr` points at `ai_addrlen` bytes of sockaddr.
                if let Ok(mut addr) = unsafe { from_raw(info.ai_addr, info.ai_addrlen) } {
                    addr.set_port(port);
                    out.push(addr);
                }
            }
            node = info.ai_next;
        }
        // SAFETY: returned by GetAddrInfoW and not used again.
        unsafe { FreeAddrInfoW(list) };
        Ok(())
    }

    fn queue_new(capacity: u32, _fallback: bool) -> Result<Port> {
        Port::new(capacity)
    }

    fn queue_engine(_: &Port) -> Engine {
        Engine::Iocp
    }

    unsafe fn queue_push(
        queue: &mut Port,
        request: NonNull<Request>,
        op: &Op<'_, Socket>,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { queue.push(request, op) }
    }

    unsafe fn queue_cancel(queue: &mut Port, request: NonNull<Request>, socket: &Socket) {
        // SAFETY: forwarded from the caller.
        unsafe { queue.cancel(request, socket) }
    }

    fn queue_submit(_: &mut Port) -> Result<()> {
        // Overlapped calls start at push.
        Ok(())
    }

    fn queue_poll(
        queue: &mut Port,
        out: &mut [RawCompletion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        queue.poll(out, timeout)
    }

    unsafe fn accept_result(
        request: NonNull<Request>,
        listener: &Socket,
        result: Result<usize>,
    ) -> Result<(Socket, SocketAddr)> {
        // SAFETY: the request carried an accept, which created this socket; it is ours now.
        let socket = unsafe { iocp::take_accepted(request) };
        result?;
        // Inherits the listener's properties and enables getpeername/shutdown.
        set(
            socket.raw,
            SOL_SOCKET,
            SO_UPDATE_ACCEPT_CONTEXT,
            &listener.raw,
        )?;
        let peer = Self::peer_addr(&socket)?;
        Ok((socket, peer))
    }

    fn connect_result(socket: &Socket, result: Result<usize>) -> Result<()> {
        result?;
        // Enables getpeername/shutdown on a ConnectEx socket.
        // SAFETY: SO_UPDATE_CONNECT_CONTEXT takes no value.
        check(unsafe {
            setsockopt(
                socket.raw,
                SOL_SOCKET,
                SO_UPDATE_CONNECT_CONTEXT,
                ptr::null(),
                0,
            )
        })
    }
}

impl Socket {
    fn new(family: u16) -> Result<Self> {
        startup()?;
        // SAFETY: no pointers; a null protocol info picks the TCP provider.
        let raw = unsafe {
            WSASocketW(
                family.into(),
                SOCK_STREAM,
                IPPROTO_TCP,
                ptr::null(),
                0,
                WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
            )
        };
        if raw == INVALID_SOCKET {
            return Err(last_error());
        }
        Ok(Self::from_raw(raw, family))
    }

    /// Takes ownership of `raw`, a fresh socket bound to no port.
    fn from_raw(raw: SOCKET, family: u16) -> Self {
        Self {
            raw,
            family,
            port: Cell::new(0),
            skip_port: Cell::new(false),
            zero_byte: Cell::new(false),
        }
    }
}

/// A point in time for waits that may resume after spurious wake-ups.
struct Deadline(Option<u64>);

impl Deadline {
    /// `None` never expires.
    fn after(timeout: Option<Duration>) -> Self {
        Self(timeout.map(|t| {
            let tps = u128::from(marlin_pal::time::ticks_per_second());
            let ticks = (t.as_nanos() * tps).div_ceil(1_000_000_000);
            marlin_pal::time::ticks().saturating_add(ticks.min(u128::from(u64::MAX)) as u64)
        }))
    }

    /// `None` = wait forever, zero = expired.
    fn remaining(&self) -> Option<Duration> {
        let end = self.0?;
        let left = end.saturating_sub(marlin_pal::time::ticks());
        let tps = u128::from(marlin_pal::time::ticks_per_second());
        let nanos = (u128::from(left) * 1_000_000_000).div_ceil(tps);
        Some(Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64))
    }
}

/// WSAStartup, once per process; Winsock is never cleaned up.
fn startup() -> Result<()> {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut data = WSADATA::default();
    // SAFETY: `data` is a valid out pointer. Racing first calls just raise Winsock's refcount.
    let code = unsafe { WSAStartup(0x0202, &mut data) };
    if code != 0 {
        return Err(from_code(code as u32));
    }
    STARTED.store(true, Ordering::Release);
    Ok(())
}

/// `AcceptEx` / `ConnectEx` from the TCP provider, looked up once through `socket`.
fn extension(socket: SOCKET, id: &GUID, cache: &AtomicUsize) -> Result<usize> {
    let cached = cache.load(Ordering::Relaxed);
    if cached != 0 {
        return Ok(cached);
    }
    let mut function = 0usize;
    let mut returned = 0;
    // SAFETY: in and out buffers are valid for the sizes passed; not overlapped.
    let rc = unsafe {
        WSAIoctl(
            socket,
            SIO_GET_EXTENSION_FUNCTION_POINTER,
            ptr::from_ref(id).cast(),
            size_of::<GUID>() as u32,
            (&raw mut function).cast(),
            size_of::<usize>() as u32,
            &mut returned,
            ptr::null_mut(),
            None,
        )
    };
    if rc == SOCKET_ERROR {
        return Err(last_error());
    }
    // Racing lookups store the same pointer.
    cache.store(function, Ordering::Relaxed);
    Ok(function)
}

fn accept_ex(socket: SOCKET) -> Result<usize> {
    static ACCEPT_EX: AtomicUsize = AtomicUsize::new(0);
    extension(socket, &WSAID_ACCEPTEX, &ACCEPT_EX)
}

fn connect_ex(socket: SOCKET) -> Result<usize> {
    static CONNECT_EX: AtomicUsize = AtomicUsize::new(0);
    extension(socket, &WSAID_CONNECTEX, &CONNECT_EX)
}

fn family(addr: &SocketAddr) -> u16 {
    match addr {
        SocketAddr::V4(_) => AF_INET,
        SocketAddr::V6(_) => AF_INET6,
    }
}

fn option_name(option: SockOpt) -> (i32, i32) {
    match option {
        SockOpt::NoDelay => (IPPROTO_TCP, TCP_NODELAY),
        SockOpt::SendBuffer => (SOL_SOCKET, SO_SNDBUF),
        SockOpt::RecvBuffer => (SOL_SOCKET, SO_RCVBUF),
        SockOpt::KeepAlive => (SOL_SOCKET, SO_KEEPALIVE),
        // Windows 10 1709+.
        SockOpt::KeepIdle => (IPPROTO_TCP, TCP_KEEPIDLE),
        SockOpt::KeepInterval => (IPPROTO_TCP, TCP_KEEPINTVL),
        SockOpt::KeepCount => (IPPROTO_TCP, TCP_KEEPCNT),
        SockOpt::Linger => (SOL_SOCKET, SO_LINGER),
        SockOpt::RecvWhenReadable => unreachable!("not a socket option"),
    }
}

fn set<T>(socket: SOCKET, level: i32, name: i32, value: &T) -> Result<()> {
    // SAFETY: `value` is valid for `size_of::<T>()` bytes.
    check(unsafe {
        setsockopt(
            socket,
            level,
            name,
            ptr::from_ref(value).cast(),
            size_of::<T>() as i32,
        )
    })
}

/// Reads an option of type `T`. Zeroed first: some options write fewer bytes (BOOLs as 1).
fn get<T: Default>(socket: SOCKET, level: i32, name: i32) -> Result<T> {
    let mut value = T::default();
    let mut len = size_of::<T>() as i32;
    // SAFETY: `value` is valid for `len` bytes.
    check(unsafe { getsockopt(socket, level, name, (&raw mut value).cast(), &mut len) })?;
    Ok(value)
}

/// A sockaddr large enough for both families.
#[repr(C)]
pub(crate) union RawAddr {
    v4: SOCKADDR_IN,
    v6: SOCKADDR_IN6,
    storage: SOCKADDR_STORAGE,
}

impl RawAddr {
    fn as_ptr(&self) -> *const SOCKADDR {
        ptr::from_ref(self).cast()
    }
}

pub(crate) fn to_raw(addr: &SocketAddr) -> (RawAddr, i32) {
    match addr {
        SocketAddr::V4(a) => {
            let v4 = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: a.port().to_be(),
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(a.ip().octets()),
                    },
                },
                sin_zero: [0; 8],
            };
            (RawAddr { v4 }, size_of::<SOCKADDR_IN>() as i32)
        }
        SocketAddr::V6(a) => {
            let v6 = SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: a.port().to_be(),
                sin6_flowinfo: a.flowinfo(),
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 {
                        Byte: a.ip().octets(),
                    },
                },
                Anonymous: SOCKADDR_IN6_0 {
                    sin6_scope_id: a.scope_id(),
                },
            };
            (RawAddr { v6 }, size_of::<SOCKADDR_IN6>() as i32)
        }
    }
}

/// # Safety
/// `addr` points at `len` readable bytes holding a sockaddr.
unsafe fn from_raw(addr: *const SOCKADDR, len: usize) -> Result<SocketAddr> {
    // SAFETY: every sockaddr starts with its family.
    let family = unsafe { (*addr).sa_family };
    match family {
        AF_INET if len >= size_of::<SOCKADDR_IN>() => {
            // SAFETY: checked family and length; the read tolerates misalignment.
            let a = unsafe { addr.cast::<SOCKADDR_IN>().read_unaligned() };
            // SAFETY: every bit pattern is a valid address.
            let ip = unsafe { a.sin_addr.S_un.S_addr }.to_ne_bytes();
            Ok(SocketAddrV4::new(ip.into(), u16::from_be(a.sin_port)).into())
        }
        AF_INET6 if len >= size_of::<SOCKADDR_IN6>() => {
            // SAFETY: as above.
            let a = unsafe { addr.cast::<SOCKADDR_IN6>().read_unaligned() };
            // SAFETY: as above.
            let (ip, scope) = unsafe { (a.sin6_addr.u.Byte, a.Anonymous.sin6_scope_id) };
            Ok(
                SocketAddrV6::new(ip.into(), u16::from_be(a.sin6_port), a.sin6_flowinfo, scope)
                    .into(),
            )
        }
        _ => Err(Error::from_kind(ErrorKind::Unsupported)),
    }
}

fn read_addr(f: impl FnOnce(*mut SOCKADDR, *mut i32) -> i32) -> Result<SocketAddr> {
    let mut addr = RawAddr {
        storage: SOCKADDR_STORAGE::default(),
    };
    let mut len = size_of::<RawAddr>() as i32;
    check(f((&raw mut addr).cast(), &mut len))?;
    // SAFETY: the OS wrote `len` bytes of sockaddr.
    unsafe { from_raw(addr.as_ptr(), len as usize) }
}

/// Converts a Winsock `int` result (`SOCKET_ERROR` on failure).
fn check(rc: i32) -> Result<()> {
    if rc == SOCKET_ERROR {
        Err(last_error())
    } else {
        Ok(())
    }
}

/// The calling thread's last Winsock error.
fn last_error() -> Error {
    // SAFETY: no preconditions.
    from_code(unsafe { WSAGetLastError() } as u32)
}

/// Maps Winsock (`WSAE*`) and Win32 (`ERROR_*`, from completion statuses) codes.
pub(crate) fn from_code(code: u32) -> Error {
    use windows_sys::Win32::Networking::WinSock as ws;
    let wsa = code as i32;
    let kind = match code {
        ERROR_CONNECTION_REFUSED | ERROR_PORT_UNREACHABLE => ErrorKind::ConnectionRefused,
        ERROR_NETNAME_DELETED => ErrorKind::ConnectionReset,
        ERROR_CONNECTION_ABORTED | ERROR_REQUEST_ABORTED => ErrorKind::ConnectionAborted,
        ERROR_HOST_UNREACHABLE => ErrorKind::HostUnreachable,
        ERROR_NETWORK_UNREACHABLE => ErrorKind::NetworkUnreachable,
        ERROR_OPERATION_ABORTED => ErrorKind::Cancelled,
        ERROR_SEM_TIMEOUT | WAIT_TIMEOUT => ErrorKind::TimedOut,
        ERROR_NOT_ENOUGH_MEMORY | ERROR_OUTOFMEMORY => ErrorKind::OutOfMemory,
        ERROR_INVALID_PARAMETER | ERROR_INVALID_HANDLE => ErrorKind::InvalidInput,
        ERROR_ACCESS_DENIED => ErrorKind::PermissionDenied,
        ERROR_NOT_SUPPORTED => ErrorKind::Unsupported,
        _ => match wsa {
            ws::WSAECONNREFUSED => ErrorKind::ConnectionRefused,
            ws::WSAECONNRESET | ws::WSAENETRESET => ErrorKind::ConnectionReset,
            ws::WSAECONNABORTED => ErrorKind::ConnectionAborted,
            ws::WSAENOTCONN => ErrorKind::NotConnected,
            ws::WSAESHUTDOWN => ErrorKind::BrokenPipe,
            ws::WSAEADDRINUSE => ErrorKind::AddrInUse,
            ws::WSAEADDRNOTAVAIL => ErrorKind::AddrNotAvailable,
            ws::WSAEHOSTUNREACH => ErrorKind::HostUnreachable,
            ws::WSAENETUNREACH | ws::WSAENETDOWN => ErrorKind::NetworkUnreachable,
            ws::WSAETIMEDOUT => ErrorKind::TimedOut,
            ws::WSAEWOULDBLOCK => ErrorKind::WouldBlock,
            ws::WSAEINTR => ErrorKind::Interrupted,
            ws::WSAEINVAL | ws::WSAEFAULT | ws::WSAENOTSOCK => ErrorKind::InvalidInput,
            ws::WSAEACCES => ErrorKind::PermissionDenied,
            ws::WSAENOBUFS => ErrorKind::OutOfMemory,
            ws::WSAEAFNOSUPPORT | ws::WSAEPROTONOSUPPORT | ws::WSAEOPNOTSUPP => {
                ErrorKind::Unsupported
            }
            ws::WSAHOST_NOT_FOUND | ws::WSANO_DATA => ErrorKind::NotFound,
            _ => ErrorKind::Other,
        },
    };
    Error::new(kind, wsa)
}

/// Milliseconds for a Win32 wait: rounded up, `INFINITE` for `None`.
fn timeout_ms(timeout: Option<Duration>) -> u32 {
    const INFINITE: u32 = u32::MAX;
    match timeout {
        None => INFINITE,
        Some(t) => {
            let ms = t.as_nanos().div_ceil(1_000_000);
            ms.min(u128::from(INFINITE - 1)) as u32
        }
    }
}
