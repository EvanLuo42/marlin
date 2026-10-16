//! Overlapped Winsock on an IO completion port.
//!
//! Sockets are put in `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` mode when they are bound to the port:
//! an operation that finishes inside its call (data already buffered, a connection already
//! waiting) queues no packet, and its completion goes to `ready` instead. That saves a kernel
//! transition per operation under load, when most receives find data waiting. Failures to start
//! never queue a packet either, so they go to `ready` too: every completion arrives through
//! `poll`.
//!
//! Receives on sockets with `recv_when_readable` run in two steps: a zero-byte `WSARecv` that
//! completes when data (or the end of the stream) arrives, then a non-blocking `recv` into the
//! caller's buffer inside `poll`. A pending receive with a real buffer makes the kernel lock the
//! buffer's pages for as long as it waits; the zero-byte one locks nothing. If another receive
//! took the data first, `recv` would block, so the zero-byte receive is posted again instead.

use alloc::collections::VecDeque;
use core::mem::{ManuallyDrop, size_of};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    INVALID_SOCKET, LPFN_ACCEPTEX, LPFN_CONNECTEX, SOCKADDR, SOCKADDR_IN6, SOCKADDR_STORAGE,
    SOCKET, SOCKET_ERROR, WSA_IO_PENDING, WSABUF, WSAEWOULDBLOCK, WSAGetLastError,
    WSAGetOverlappedResult, WSARecv, WSASend, recv,
};
use windows_sys::Win32::Storage::FileSystem::SetFileCompletionNotificationModes;
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED, OVERLAPPED_0,
    OVERLAPPED_ENTRY,
};
use windows_sys::Win32::System::WindowsProgramming::{
    FILE_SKIP_COMPLETION_PORT_ON_SUCCESS, FILE_SKIP_SET_EVENT_ON_HANDLE,
};

use super::{
    Deadline, RawAddr, Socket, accept_ex, connect_ex, from_code, last_error, timeout_ms, to_raw,
};
use crate::types::{Op, OpKind, RawCompletion};
use crate::{Error, ErrorKind, Result};

/// Room AcceptEx needs per address: the largest sockaddr plus 16 bytes.
const ADDR_LEN: usize = size_of::<SOCKADDR_IN6>() + 16;
/// Most completions handled per poll.
const BATCH: usize = 64;

#[repr(C)]
pub(crate) struct Request {
    /// First, so the OVERLAPPED pointer a completion carries is the request pointer.
    ov: OVERLAPPED,
    /// The socket the operation runs on (the listener, for an accept).
    socket: SOCKET,
    tag: u64,
    /// The socket's `skip_port` when the operation started.
    skip_port: bool,
    /// A receive in two steps; the OS operation is the zero-byte one.
    zero_byte: bool,
    /// `cancel` was called: a zero-byte receive must not be posted again.
    cancelled: bool,
    /// Zero-byte receives: the caller's buffer, filled by the follow-up `recv`.
    buf: *mut u8,
    len: u32,
    /// Accept: the socket that becomes the connection. Owned by the request until
    /// `take_accepted`.
    accept: SOCKET,
    family: u16,
    /// Connect: the peer. ConnectEx may read it after the call returns.
    peer: RawAddr,
    /// Accept: where AcceptEx writes the local and remote addresses.
    addrs: [u8; 2 * ADDR_LEN],
}

const OVERLAPPED_INIT: OVERLAPPED = OVERLAPPED {
    Internal: 0,
    InternalHigh: 0,
    Anonymous: OVERLAPPED_0 {
        Pointer: ptr::null_mut(),
    },
    hEvent: ptr::null_mut(),
};

pub(super) const REQUEST_INIT: Request = Request {
    ov: OVERLAPPED_INIT,
    socket: INVALID_SOCKET,
    tag: 0,
    skip_port: false,
    zero_byte: false,
    cancelled: false,
    buf: ptr::null_mut(),
    len: 0,
    accept: INVALID_SOCKET,
    family: 0,
    peer: RawAddr {
        storage: SOCKADDR_STORAGE {
            ss_family: 0,
            __ss_pad1: [0; 6],
            __ss_align: 0,
            __ss_pad2: [0; 112],
        },
    },
    addrs: [0; 2 * ADDR_LEN],
};

/// An OS operation that finished, before the follow-up step of a zero-byte receive.
#[derive(Clone, Copy)]
struct Done {
    request: *mut Request,
    result: Result<usize>,
}

impl Done {
    const EMPTY: Self = Self {
        request: ptr::null_mut(),
        result: Ok(0),
    };
}

/// Port ids start at 1; 0 means "not bound" in `Socket::port`. Ids rather than handle values,
/// because a closed port's handle value can be reused.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

pub(crate) struct Port {
    handle: HANDLE,
    id: usize,
    /// Operations that finished inside their call; see the module docs.
    ready: VecDeque<Done>,
    /// Set when `ready` alone filled a poll, so the port gets the next turn and its packets
    /// cannot starve behind a stream of operations that keep completing inline.
    port_first: bool,
}

// SAFETY: completion ports may be used from any thread; the request pointers in `ready` belong to
// the queue that owns the port.
unsafe impl Send for Port {}

impl Port {
    pub(super) fn new(capacity: u32) -> Result<Self> {
        // SAFETY: creating a port without a file has no preconditions.
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
        if handle.is_null() {
            // SAFETY: no preconditions.
            return Err(from_code(unsafe { GetLastError() }));
        }
        Ok(Self {
            handle,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            ready: VecDeque::with_capacity(capacity as usize),
            port_first: false,
        })
    }

    /// A socket posts completions to one port for its whole life, so a socket used with one
    /// queue cannot move to another.
    fn bind(&self, socket: &Socket) -> Result<()> {
        match socket.port.get() {
            0 => {
                let handle = socket.raw as HANDLE;
                // SAFETY: both handles are live.
                if unsafe { CreateIoCompletionPort(handle, self.handle, 0, 0) }.is_null() {
                    // SAFETY: no preconditions.
                    return Err(from_code(unsafe { GetLastError() }));
                }
                socket.port.set(self.id);
                let modes =
                    (FILE_SKIP_COMPLETION_PORT_ON_SUCCESS | FILE_SKIP_SET_EVENT_ON_HANDLE) as u8;
                // SAFETY: the handle is live.
                let skip = unsafe { SetFileCompletionNotificationModes(handle, modes) } != 0;
                socket.skip_port.set(skip);
                Ok(())
            }
            id if id == self.id => Ok(()),
            _ => Err(Error::from_kind(ErrorKind::InvalidInput)),
        }
    }

    /// # Safety
    /// `request`, the buffer and the socket stay valid until the completion is polled.
    pub(super) unsafe fn push(
        &mut self,
        request: NonNull<Request>,
        op: &Op<'_, Socket>,
    ) -> Result<()> {
        let socket = op.socket;
        self.bind(socket)?;
        // Everything that can fail before the operation starts, so failing here leaves nothing
        // in flight.
        let (function, accept) = match op.kind {
            OpKind::Accept => (accept_ex(socket.raw)?, Some(Socket::new(socket.family)?)),
            OpKind::Connect => (connect_ex(socket.raw)?, None),
            OpKind::Send | OpKind::Recv => (0, None),
        };

        let request = request.as_ptr();
        let zero_byte = op.kind == OpKind::Recv && socket.zero_byte.get();
        // SAFETY: the caller hands us the request; it is not in flight.
        unsafe {
            (*request).ov = OVERLAPPED_INIT;
            (*request).socket = socket.raw;
            (*request).tag = op.tag;
            (*request).skip_port = socket.skip_port.get();
            (*request).zero_byte = zero_byte;
            (*request).cancelled = false;
            (*request).buf = op.buf;
            (*request).len = op.len;
            (*request).family = socket.family;
            // Owned by the request now; `take_accepted` gives it back.
            (*request).accept = accept.map_or(INVALID_SOCKET, |s| ManuallyDrop::new(s).raw);
        }
        if zero_byte {
            // SAFETY: as above; the request stays valid until its completion is polled.
            unsafe { self.post_zero_byte(request) };
            return Ok(());
        }
        // SAFETY: as above.
        let ov = unsafe { &raw mut (*request).ov };
        let mut transferred = 0u32;
        let buf = WSABUF {
            len: op.len,
            buf: op.buf,
        };
        // SAFETY: the caller keeps the request, the buffer and the socket alive until the
        // completion is polled. Winsock copies the WSABUF array during the call. `function` is
        // the extension function for this kind of operation.
        let started = unsafe {
            match op.kind {
                OpKind::Send => WSASend(socket.raw, &buf, 1, &mut transferred, 0, ov, None) == 0,
                OpKind::Recv => {
                    let mut flags = 0;
                    WSARecv(socket.raw, &buf, 1, &mut transferred, &mut flags, ov, None) == 0
                }
                OpKind::Connect => {
                    let (peer, len) = to_raw(&op.addr.unwrap());
                    (*request).peer = peer;
                    let connect_ex: LPFN_CONNECTEX = core::mem::transmute(function);
                    connect_ex.unwrap()(
                        socket.raw,
                        (&raw const (*request).peer).cast::<SOCKADDR>(),
                        len,
                        ptr::null(),
                        0,
                        ptr::null_mut(),
                        ov,
                    ) != 0
                }
                OpKind::Accept => {
                    let accept_ex: LPFN_ACCEPTEX = core::mem::transmute(function);
                    accept_ex.unwrap()(
                        socket.raw,
                        (*request).accept,
                        (&raw mut (*request).addrs).cast(),
                        0,
                        ADDR_LEN as u32,
                        ADDR_LEN as u32,
                        &mut transferred,
                        ov,
                    ) != 0
                }
            }
        };
        // SAFETY: as above.
        unsafe { self.started(request, started, transferred) };
        Ok(())
    }

    /// Posts the zero-byte receive of a two-step receive.
    ///
    /// # Safety
    /// `request` holds a receive whose OS operation is not in flight, and stays valid until its
    /// completion is polled.
    unsafe fn post_zero_byte(&mut self, request: *mut Request) {
        // SAFETY: the request is ours until the receive is posted.
        let (socket, buf) = unsafe {
            (*request).ov = OVERLAPPED_INIT;
            ((*request).socket, (*request).buf)
        };
        let empty = WSABUF { len: 0, buf };
        let (mut transferred, mut flags) = (0, 0);
        // SAFETY: the request stays valid until the completion is polled; zero bytes of the
        // buffer are written.
        let started = unsafe {
            WSARecv(
                socket,
                &empty,
                1,
                &mut transferred,
                &mut flags,
                &raw mut (*request).ov,
                None,
            )
        } == 0;
        // SAFETY: as above.
        unsafe { self.started(request, started, 0) };
    }

    /// Routes the result of an overlapped call: inline completions and failures to start go to
    /// `ready`; pending ones arrive as packets.
    ///
    /// # Safety
    /// `request` was just passed to that call.
    unsafe fn started(&mut self, request: *mut Request, started: bool, transferred: u32) {
        if started {
            // SAFETY: the request is valid; with `skip_port` no packet will touch it.
            if unsafe { (*request).skip_port } {
                self.ready.push_back(Done {
                    request,
                    result: Ok(transferred as usize),
                });
            }
        } else {
            // SAFETY: no preconditions.
            let code = unsafe { WSAGetLastError() };
            if code != WSA_IO_PENDING {
                self.ready.push_back(Done {
                    request,
                    result: Err(from_code(code as u32)),
                });
            }
        }
    }

    /// # Safety
    /// `request` is in flight on `socket`.
    pub(super) unsafe fn cancel(&mut self, request: NonNull<Request>, socket: &Socket) {
        let request = request.as_ptr();
        // SAFETY: the request is valid. The OS only touches its OVERLAPPED, not this flag. Then
        // both are live; CancelIoEx fails harmlessly (ERROR_NOT_FOUND) if the operation finished,
        // including one waiting in `ready`.
        unsafe {
            (*request).cancelled = true;
            CancelIoEx(socket.raw as HANDLE, &raw const (*request).ov);
        }
    }

    pub(super) fn poll(
        &mut self,
        out: &mut [RawCompletion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        let deadline = Deadline::after(timeout);
        let mut done = [Done::EMPTY; BATCH];
        let max = out.len().min(BATCH);
        loop {
            let got = self.collect(&mut done[..max], deadline.remaining())?;
            if got == 0 {
                return Ok(0);
            }
            let mut n = 0;
            for &d in &done[..got] {
                // SAFETY: the OS is done with each collected request.
                if let Some(result) = unsafe { self.finish(d) } {
                    out[n] = RawCompletion {
                        // SAFETY: as above.
                        tag: unsafe { (*d.request).tag },
                        result,
                    };
                    n += 1;
                }
            }
            // Every one was a zero-byte receive that lost its data to another: wait again.
            if n > 0 || deadline.remaining() == Some(Duration::ZERO) {
                return Ok(n);
            }
        }
    }

    /// The operation's result, or `None` if a zero-byte receive was posted again.
    ///
    /// # Safety
    /// The OS is done with `done.request`.
    unsafe fn finish(&mut self, done: Done) -> Option<Result<usize>> {
        let request = done.request;
        // SAFETY: the OS is done with the request.
        let (zero_byte, socket, buf, len, cancelled) = unsafe {
            (
                (*request).zero_byte,
                (*request).socket,
                (*request).buf,
                (*request).len,
                (*request).cancelled,
            )
        };
        if !zero_byte || done.result.is_err() {
            return Some(done.result);
        }
        // Readable, or at the end of the stream.
        // SAFETY: the caller keeps the buffer valid for `len` bytes until the completion is
        // polled. The socket is non-blocking.
        let n = unsafe { recv(socket, buf, len as i32, 0) };
        if n != SOCKET_ERROR {
            return Some(Ok(n as usize));
        }
        // SAFETY: no preconditions.
        let code = unsafe { WSAGetLastError() };
        if code != WSAEWOULDBLOCK {
            return Some(Err(from_code(code as u32)));
        }
        if cancelled {
            return Some(Err(Error::new(ErrorKind::Cancelled, code)));
        }
        // SAFETY: the receive is not in flight; the caller keeps the request valid.
        unsafe { self.post_zero_byte(request) };
        None
    }

    /// Fills `out` with finished operations: some from `ready`, the rest from the port, waiting
    /// up to `timeout` only if nothing was ready.
    fn collect(&mut self, out: &mut [Done], timeout: Option<Duration>) -> Result<usize> {
        let mut n = 0;
        if self.port_first {
            self.port_first = false;
            n = self.dequeue(out, Some(Duration::ZERO))?;
        }
        while n < out.len()
            && let Some(done) = self.ready.pop_front()
        {
            out[n] = done;
            n += 1;
        }
        if n == out.len() {
            self.port_first = !self.ready.is_empty();
            return Ok(n);
        }
        let timeout = if n > 0 { Some(Duration::ZERO) } else { timeout };
        Ok(n + self.dequeue(&mut out[n..], timeout)?)
    }

    fn dequeue(&mut self, out: &mut [Done], timeout: Option<Duration>) -> Result<usize> {
        let mut entries = [OVERLAPPED_ENTRY::default(); BATCH];
        let count = out.len().min(BATCH) as u32;
        let mut removed = 0;
        // SAFETY: `entries` holds at least `count` entries; `removed` is a valid out pointer.
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                self.handle,
                entries.as_mut_ptr(),
                count,
                &mut removed,
                timeout_ms(timeout),
                0,
            )
        };
        if ok == 0 {
            // SAFETY: no preconditions.
            let code = unsafe { GetLastError() };
            return if code == WAIT_TIMEOUT {
                Ok(0)
            } else {
                Err(from_code(code))
            };
        }
        for (entry, out) in entries[..removed as usize].iter().zip(out) {
            // Every packet on this port carries the OVERLAPPED of a request pushed by `push`,
            // which is its first field. The caller keeps it alive until now, and the OS is done
            // with it.
            let request = entry.lpOverlapped.cast::<Request>();
            // SAFETY: as above.
            let (status, socket) = unsafe { ((*request).ov.Internal, (*request).socket) };
            let result = if status == 0 {
                // STATUS_SUCCESS
                Ok(entry.dwNumberOfBytesTransferred as usize)
            } else {
                let (mut n, mut flags) = (0, 0);
                // SAFETY: the operation has finished; this only converts its NTSTATUS to a
                // Winsock error.
                let ok = unsafe {
                    WSAGetOverlappedResult(socket, &raw const (*request).ov, &mut n, 0, &mut flags)
                };
                if ok != 0 {
                    Ok(n as usize)
                } else {
                    Err(last_error())
                }
            };
            *out = Done { request, result };
        }
        Ok(removed as usize)
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: we own the handle and do not use it again.
        unsafe { CloseHandle(self.handle) };
    }
}

/// The socket an accept created, now owned by the caller (closed on drop).
///
/// # Safety
/// `request` carried an accept whose completion has been polled, and this is called once for it.
pub(super) unsafe fn take_accepted(request: NonNull<Request>) -> Socket {
    let request = request.as_ptr();
    // SAFETY: the OS is done with the request.
    let (raw, family) = unsafe { ((*request).accept, (*request).family) };
    // SAFETY: as above.
    unsafe { (*request).accept = INVALID_SOCKET };
    Socket::from_raw(raw, family)
}
