use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt;
use core::mem::ManuallyDrop;
use core::net::SocketAddr;
use core::ops::{Bound, RangeBounds};
use core::ptr::{self, NonNull};
use core::time::Duration;

use marlin_io::IoBuffer;

use crate::backend::imp::Backend;
use crate::contract::Net;
use crate::types::{Op, OpKind, RawCompletion};
use crate::{
    Engine, Error, ErrorKind, ListenOptions, MAX_IO_LEN, MAX_QUEUE_CAPACITY, Result, Shutdown,
    SocketOptions,
};

type RawSocket = <Backend as Net>::Socket;
type RawQueue = <Backend as Net>::Queue;
type Request = <Backend as Net>::Request;

/// Names a socket owned by a [`NetQueue`]. Stale ids (closed sockets) are `InvalidInput`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SocketId {
    index: u32,
    generation: u32,
}

/// Identifies one submitted operation until its [`Completed`] is returned.
///
/// Ids are not reused while their operation is in flight; a slot's next id gets a new generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RequestId {
    index: u32,
    generation: u32,
}

impl RequestId {
    fn tag(self) -> u64 {
        u64::from(self.generation) << 32 | u64::from(self.index)
    }

    fn from_tag(tag: u64) -> Self {
        Self {
            index: tag as u32,
            generation: (tag >> 32) as u32,
        }
    }
}

/// A finished operation, handing back its buffer.
#[derive(Debug)]
pub enum Completed {
    /// [`NetQueue::connect`] finished. On failure the socket stays open (unusable); close it.
    Connected {
        id: RequestId,
        socket: SocketId,
        result: Result<()>,
    },
    /// [`NetQueue::accept`] finished: the new connection, owned by this queue, and its peer.
    Accepted {
        id: RequestId,
        listener: SocketId,
        result: Result<(SocketId, SocketAddr)>,
    },
    /// [`NetQueue::send`] finished: every byte of the range was handed to the OS, or the
    /// connection failed.
    Sent {
        id: RequestId,
        socket: SocketId,
        buffer: IoBuffer,
        result: Result<usize>,
    },
    /// [`NetQueue::recv`] finished: `n` bytes arrived at `buffer[start..start + n]`. `Ok(0)` means
    /// the peer shut down sending (end of stream).
    Received {
        id: RequestId,
        socket: SocketId,
        buffer: IoBuffer,
        start: usize,
        result: Result<usize>,
    },
    /// [`NetQueue::close`] finished: every operation on the socket has completed, the OS socket
    /// is closed and the id is stale from now on. Nothing else arrives for it.
    Closed { socket: SocketId },
}

impl Completed {
    /// The operation's id; `None` for [`Closed`](Self::Closed).
    pub fn id(&self) -> Option<RequestId> {
        match *self {
            Self::Connected { id, .. }
            | Self::Accepted { id, .. }
            | Self::Sent { id, .. }
            | Self::Received { id, .. } => Some(id),
            Self::Closed { .. } => None,
        }
    }

    /// The socket the operation ran on (the listener, for an accept).
    pub fn socket(&self) -> SocketId {
        match *self {
            Self::Connected { socket, .. }
            | Self::Sent { socket, .. }
            | Self::Received { socket, .. }
            | Self::Closed { socket } => socket,
            Self::Accepted { listener, .. } => listener,
        }
    }
}

/// A send or receive that was refused (queue full, stale socket, bad range), handing back the
/// buffer.
pub struct SubmitError {
    pub error: Error,
    pub buffer: IoBuffer,
}

impl fmt::Debug for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl core::error::Error for SubmitError {}

/// A connected or listening socket outside any queue: [`NetQueue::detach`] makes one,
/// [`NetQueue::attach`] takes it. Dropping it closes the socket.
///
/// For handing connections from an accepting queue to worker queues on other threads. On
/// Windows a socket that has run an operation in one queue is bound to that queue's completion
/// port for life, so detach sockets before their first operation (accepted sockets qualify).
pub struct Socket(pub(crate) RawSocket);

impl Socket {
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Backend::local_addr(&self.0)
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        Backend::peer_addr(&self.0)
    }
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("local", &self.local_addr().ok())
            .field("peer", &self.peer_addr().ok())
            .finish()
    }
}

struct Slot {
    kind: OpKind,
    generation: u32,
    /// Pushed, completion not yet returned.
    busy: bool,
    socket: u32,
    buffer: Option<IoBuffer>,
    start: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Open,
    /// `close` was called; the OS socket closes when its last operation completes.
    Closing,
    /// Closed; the `Closed` completion is pending.
    Closed,
}

struct Entry {
    raw: Option<RawSocket>,
    state: State,
    generation: u32,
    /// Operations on this socket whose completions have not been returned.
    ops: u32,
    /// Listeners: options for accepted sockets.
    accepted: SocketOptions,
}

/// Completion-based TCP: connect, accept, send and receive with owned buffers.
///
/// The queue owns its sockets and names them by [`SocketId`]. Operations take ownership of an
/// [`IoBuffer`] (the type marlin-io reads files into, so cooked data goes from disk to the wire
/// without a copy) and return a [`RequestId`]; [`poll`](Self::poll), [`wait`](Self::wait) and
/// [`wait_timeout`](Self::wait_timeout) return [`Completed`] operations with their buffers, in
/// completion order.
///
/// Any number of sends and receives may be in flight per socket (pipelining): sends complete
/// whole and go out in submission order, and receives fill in submission order. Completions,
/// though, may be reported out of order (an operation that finishes inside its call can overtake
/// one already finished in the OS), so keep one receive per socket in flight unless you reorder
/// them by id.
///
/// [`close`](Self::close) cancels a socket's operations and closes it once they are done;
/// dropping the queue does that for every socket and waits. The queue is `Send` but not `Sync`:
/// give it to one IO thread. Servers run one queue per thread and hand accepted connections over
/// with [`detach`](Self::detach) / [`attach`](Self::attach).
pub struct NetQueue {
    raw: ManuallyDrop<RawQueue>,
    /// The OS may write these while operations are in flight, so they are only reached through
    /// raw pointers. Leaked, not freed, if the queue is leaked.
    requests: NonNull<Request>,
    slots: Vec<Slot>,
    free: Vec<u32>,
    sockets: Vec<Entry>,
    free_sockets: Vec<u32>,
    /// Operations in the backend whose completions it has not reported yet.
    pushed: u32,
    /// Completions fetched from the backend but not yet returned.
    staged: Box<[RawCompletion]>,
    staged_next: usize,
    staged_len: usize,
    closed: VecDeque<SocketId>,
}

// SAFETY: the requests are owned by the queue; everything else is Send.
unsafe impl Send for NetQueue {}

impl NetQueue {
    /// A queue on the platform's best engine for up to `capacity` operations in flight
    /// (`1..=MAX_QUEUE_CAPACITY`). Sockets do not count, only operations.
    pub fn new(capacity: u32) -> Result<Self> {
        Self::create(capacity, false)
    }

    /// A queue on the platform's portable engine. The same as [`new`](Self::new) on Windows.
    pub fn with_fallback(capacity: u32) -> Result<Self> {
        Self::create(capacity, true)
    }

    fn create(capacity: u32, fallback: bool) -> Result<Self> {
        if !(1..=MAX_QUEUE_CAPACITY).contains(&capacity) {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let raw = Backend::queue_new(capacity, fallback)?;
        let requests: Box<[Request]> = (0..capacity)
            .map(|_| <Backend as Net>::REQUEST_INIT)
            .collect();
        let requests = NonNull::new(Box::into_raw(requests).cast::<Request>()).unwrap();
        let slots = (0..capacity)
            .map(|_| Slot {
                kind: OpKind::Recv,
                generation: 0,
                busy: false,
                socket: 0,
                buffer: None,
                start: 0,
            })
            .collect();
        let staged = alloc::vec![RawCompletion::EMPTY; capacity.min(64) as usize];
        Ok(Self {
            raw: ManuallyDrop::new(raw),
            requests,
            slots,
            // Popped from the back, so slot 0 goes first.
            free: (0..capacity).rev().collect(),
            sockets: Vec::new(),
            free_sockets: Vec::new(),
            pushed: 0,
            staged: staged.into_boxed_slice(),
            staged_next: 0,
            staged_len: 0,
            closed: VecDeque::new(),
        })
    }

    #[inline]
    pub fn engine(&self) -> Engine {
        Backend::queue_engine(&self.raw)
    }

    #[inline]
    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    /// Operations submitted whose completions have not been returned yet.
    #[inline]
    pub fn in_flight(&self) -> u32 {
        (self.slots.len() - self.free.len()) as u32
    }

    /// A socket listening on `addr`. Port 0 picks a free port; see
    /// [`local_addr`](Self::local_addr).
    pub fn listen(&mut self, addr: SocketAddr, options: &ListenOptions) -> Result<SocketId> {
        let raw = Backend::listen(&addr, options.backlog)?;
        Ok(self.insert(raw, options.accepted))
    }

    /// Starts connecting a new socket to `addr`; [`Completed::Connected`] reports the outcome.
    pub fn connect(
        &mut self,
        addr: SocketAddr,
        options: &SocketOptions,
    ) -> Result<(SocketId, RequestId)> {
        if self.free.is_empty() {
            return Err(Error::from_kind(ErrorKind::WouldBlock));
        }
        let raw = Backend::socket_for(&addr)?;
        options.apply(&raw, true, Backend::set_option)?;
        let socket = self.insert(raw, SocketOptions::default());
        match self.push(OpKind::Connect, socket, None, 0, 0, Some(addr)) {
            Ok(id) => Ok((socket, id)),
            Err((error, _)) => {
                self.release(socket.index);
                Err(error)
            }
        }
    }

    /// Accepts one connection on `listener`. Servers keep several accepts in flight so bursts of
    /// connections are taken without a round trip each.
    pub fn accept(&mut self, listener: SocketId) -> Result<RequestId> {
        self.push(OpKind::Accept, listener, None, 0, 0, None)
            .map_err(|(error, _)| error)
    }

    /// Sends `buffer[range]` (not empty, at most [`MAX_IO_LEN`] bytes).
    ///
    /// Fails with `WouldBlock` when `capacity` operations are in flight; connection errors
    /// arrive in the [`Completed`] result instead.
    pub fn send(
        &mut self,
        socket: SocketId,
        buffer: IoBuffer,
        range: impl RangeBounds<usize>,
    ) -> Result<RequestId, SubmitError> {
        let Some((start, end)) = bounds(&range, buffer.len()) else {
            return Err(invalid(buffer));
        };
        if end - start > MAX_IO_LEN {
            return Err(invalid(buffer));
        }
        self.push(OpKind::Send, socket, Some(buffer), start, end - start, None)
            .map_err(submit_error)
    }

    /// Receives into `buffer[range]` (not empty; clamped to [`MAX_IO_LEN`] bytes) whatever
    /// arrives first: possibly fewer bytes than the range holds. Otherwise like
    /// [`send`](Self::send).
    pub fn recv(
        &mut self,
        socket: SocketId,
        buffer: IoBuffer,
        range: impl RangeBounds<usize>,
    ) -> Result<RequestId, SubmitError> {
        let Some((start, end)) = bounds(&range, buffer.len()) else {
            return Err(invalid(buffer));
        };
        let len = (end - start).min(MAX_IO_LEN);
        self.push(OpKind::Recv, socket, Some(buffer), start, len, None)
            .map_err(submit_error)
    }

    fn push(
        &mut self,
        kind: OpKind,
        socket: SocketId,
        mut buffer: Option<IoBuffer>,
        start: usize,
        len: usize,
        addr: Option<SocketAddr>,
    ) -> Result<RequestId, (Error, Option<IoBuffer>)> {
        let raw = match open(&self.sockets, socket) {
            Ok(entry) => entry.raw.as_ref().unwrap(),
            Err(error) => return Err((error, buffer)),
        };
        if buffer.is_some() && len == 0 {
            return Err((Error::from_kind(ErrorKind::InvalidInput), buffer));
        }
        let Some(index) = self.free.pop() else {
            return Err((Error::from_kind(ErrorKind::WouldBlock), buffer));
        };
        let id = RequestId {
            index,
            generation: self.slots[index as usize].generation.wrapping_add(1),
        };
        let buf = match &mut buffer {
            Some(buffer) => buffer[start..].as_mut_ptr(),
            None => ptr::null_mut(),
        };
        let op = Op {
            kind,
            socket: raw,
            buf,
            len: len as u32,
            addr,
            tag: id.tag(),
        };
        // SAFETY: slot `index` was free, so its request is idle; it stays put until the queue is
        // dropped. The buffer's heap memory moves into the slot below and stays there, untouched,
        // until the completion is returned. The socket is closed only after its last operation's
        // completion. Fewer than `capacity` operations are in flight: the slot was free.
        let pushed =
            unsafe { Backend::queue_push(&mut self.raw, request(self.requests, index), &op) };
        if let Err(error) = pushed {
            self.free.push(index);
            return Err((error, buffer));
        }
        self.slots[index as usize] = Slot {
            kind,
            generation: id.generation,
            busy: true,
            socket: socket.index,
            buffer,
            start,
        };
        self.sockets[socket.index as usize].ops += 1;
        self.pushed += 1;
        Ok(id)
    }

    /// Asks the OS to cancel an operation. It still completes: with `Cancelled`, or normally if
    /// it finished first. Does nothing if `id` has already completed.
    pub fn cancel(&mut self, id: RequestId) {
        let Some(slot) = self.slots.get(id.index as usize) else {
            return;
        };
        if slot.busy && slot.generation == id.generation {
            let raw = self.sockets[slot.socket as usize].raw.as_ref().unwrap();
            let request = request(self.requests, id.index);
            // SAFETY: the request is in flight on this socket.
            unsafe { Backend::queue_cancel(&mut self.raw, request, raw) };
        }
    }

    /// Cancels the socket's operations and closes it when they have completed; then
    /// [`Completed::Closed`] arrives. The id is unusable for new operations from now on.
    ///
    /// A connection closes gracefully (FIN after queued data) unless its
    /// [`linger`](SocketOptions::linger) is zero, which resets it.
    pub fn close(&mut self, socket: SocketId) -> Result<()> {
        let entry = open_mut(&mut self.sockets, socket)?;
        entry.state = State::Closing;
        if entry.ops == 0 {
            self.finish_close(socket.index);
            return Ok(());
        }
        let raw = self.sockets[socket.index as usize].raw.as_ref().unwrap();
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.busy && slot.socket == socket.index {
                let request = request(self.requests, index as u32);
                // SAFETY: the request is in flight on this socket.
                unsafe { Backend::queue_cancel(&mut self.raw, request, raw) };
            }
        }
        Ok(())
    }

    /// Shuts down one or both directions. Takes effect at once: wait for sends to complete
    /// before shutting down writing, or the FIN may overtake them.
    pub fn shutdown(&mut self, socket: SocketId, how: Shutdown) -> Result<()> {
        Backend::shutdown(self.raw_socket(socket)?, how)
    }

    /// Sets every option, including those at their defaults.
    pub fn set_options(&mut self, socket: SocketId, options: &SocketOptions) -> Result<()> {
        options.apply(self.raw_socket(socket)?, false, Backend::set_option)
    }

    /// The options in effect, as the OS reports them.
    pub fn options(&self, socket: SocketId) -> Result<SocketOptions> {
        SocketOptions::read(self.raw_socket(socket)?, Backend::option)
    }

    pub fn local_addr(&self, socket: SocketId) -> Result<SocketAddr> {
        Backend::local_addr(self.raw_socket(socket)?)
    }

    pub fn peer_addr(&self, socket: SocketId) -> Result<SocketAddr> {
        Backend::peer_addr(self.raw_socket(socket)?)
    }

    /// Takes a socket out of the queue. `WouldBlock` while it has operations in flight.
    pub fn detach(&mut self, socket: SocketId) -> Result<Socket> {
        let entry = open_mut(&mut self.sockets, socket)?;
        if entry.ops != 0 {
            return Err(Error::from_kind(ErrorKind::WouldBlock));
        }
        // Open entries always hold a socket. (Not unwrapped here: the stub backend's socket type
        // is uninhabited, and code after an unwrap of it is unreachable.)
        let raw = entry.raw.take();
        self.release(socket.index);
        raw.map(Socket)
            .ok_or(Error::from_kind(ErrorKind::InvalidInput))
    }

    /// Adds a detached socket to this queue.
    pub fn attach(&mut self, socket: Socket) -> SocketId {
        self.insert(socket.0, SocketOptions::default())
    }

    /// Starts queued operations without waiting for any. The waits do this too.
    pub fn submit(&mut self) -> Result<()> {
        Backend::queue_submit(&mut self.raw)
    }

    /// Returns a finished operation if there is one, without waiting.
    pub fn poll(&mut self) -> Result<Option<Completed>> {
        self.next(Some(Duration::ZERO))
    }

    /// Waits for an operation to finish. `None` only when nothing is in flight.
    pub fn wait(&mut self) -> Result<Option<Completed>> {
        self.next(None)
    }

    /// Waits up to `timeout` for an operation to finish. `None` on timeout or when nothing is in
    /// flight.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<Completed>> {
        self.next(Some(timeout))
    }

    fn next(&mut self, timeout: Option<Duration>) -> Result<Option<Completed>> {
        if let Some(socket) = self.closed.pop_front() {
            self.release(socket.index);
            return Ok(Some(Completed::Closed { socket }));
        }
        if self.staged_next == self.staged_len {
            self.staged_next = 0;
            self.staged_len = 0;
            if self.pushed == 0 {
                return Ok(None);
            }
            let n = Backend::queue_poll(&mut self.raw, &mut self.staged, timeout)?;
            debug_assert!(n as u32 <= self.pushed);
            self.pushed -= n as u32;
            self.staged_len = n;
            if n == 0 {
                return Ok(None);
            }
        }
        let completion = self.staged[self.staged_next];
        self.staged_next += 1;
        Ok(Some(self.complete(completion)))
    }

    fn complete(&mut self, completion: RawCompletion) -> Completed {
        let id = RequestId::from_tag(completion.tag);
        let slot = &mut self.slots[id.index as usize];
        debug_assert!(slot.busy && slot.generation == id.generation);
        slot.busy = false;
        let (kind, socket_index, buffer, start) =
            (slot.kind, slot.socket, slot.buffer.take(), slot.start);
        self.free.push(id.index);

        let entry = &self.sockets[socket_index as usize];
        let socket = SocketId {
            index: socket_index,
            generation: entry.generation,
        };
        let raw = entry.raw.as_ref().unwrap();
        let result = completion.result;
        let completed = match kind {
            OpKind::Send => Completed::Sent {
                id,
                socket,
                buffer: buffer.unwrap(),
                result,
            },
            OpKind::Recv => Completed::Received {
                id,
                socket,
                buffer: buffer.unwrap(),
                start,
                result,
            },
            OpKind::Connect => Completed::Connected {
                id,
                socket,
                result: Backend::connect_result(raw, result),
            },
            OpKind::Accept => {
                let options = entry.accepted;
                let request = request(self.requests, id.index);
                // SAFETY: the request carried an accept on this listener, and this is its result.
                let accepted = unsafe { Backend::accept_result(request, raw, result) }.and_then(
                    |(raw, peer)| {
                        options.apply(&raw, true, Backend::set_option)?;
                        Ok((raw, peer))
                    },
                );
                Completed::Accepted {
                    id,
                    listener: socket,
                    result: accepted
                        .map(|(raw, peer)| (self.insert(raw, SocketOptions::default()), peer)),
                }
            }
        };

        let entry = &mut self.sockets[socket_index as usize];
        entry.ops -= 1;
        if entry.state == State::Closing && entry.ops == 0 {
            self.finish_close(socket_index);
        }
        completed
    }

    fn finish_close(&mut self, index: u32) {
        let entry = &mut self.sockets[index as usize];
        entry.raw = None;
        entry.state = State::Closed;
        self.closed.push_back(SocketId {
            index,
            generation: entry.generation,
        });
    }

    fn insert(&mut self, raw: RawSocket, accepted: SocketOptions) -> SocketId {
        let index = match self.free_sockets.pop() {
            Some(index) => index,
            None => {
                self.sockets.push(Entry {
                    raw: None,
                    state: State::Free,
                    generation: 0,
                    ops: 0,
                    accepted,
                });
                self.sockets.len() as u32 - 1
            }
        };
        let entry = &mut self.sockets[index as usize];
        entry.raw = Some(raw);
        entry.state = State::Open;
        entry.accepted = accepted;
        SocketId {
            index,
            generation: entry.generation,
        }
    }

    /// Frees an entry (closing its socket, if any) and invalidates its ids.
    fn release(&mut self, index: u32) {
        let entry = &mut self.sockets[index as usize];
        entry.raw = None;
        entry.state = State::Free;
        entry.generation = entry.generation.wrapping_add(1);
        self.free_sockets.push(index);
    }

    fn raw_socket(&self, socket: SocketId) -> Result<&RawSocket> {
        Ok(open(&self.sockets, socket)?.raw.as_ref().unwrap())
    }

    /// Releases what a completion holds without reporting it: accepted sockets are closed.
    fn discard(&mut self, completion: RawCompletion) {
        let id = RequestId::from_tag(completion.tag);
        let slot = &mut self.slots[id.index as usize];
        slot.busy = false;
        if slot.kind == OpKind::Accept {
            let listener = self.sockets[slot.socket as usize].raw.as_ref().unwrap();
            let request = request(self.requests, id.index);
            // SAFETY: the request carried an accept on this listener, and this is its result.
            let accepted = unsafe { Backend::accept_result(request, listener, completion.result) };
            drop(accepted);
        }
    }
}

impl Drop for NetQueue {
    fn drop(&mut self) {
        // Socket operations may never finish on their own (a receive on an idle connection), so
        // cancel them all, then wait: the OS may still write into the requests and buffers.
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.busy {
                let raw = self.sockets[slot.socket as usize].raw.as_ref().unwrap();
                let request = request(self.requests, index as u32);
                // SAFETY: the request is in flight on this socket.
                unsafe { Backend::queue_cancel(&mut self.raw, request, raw) };
            }
        }
        while self.staged_next < self.staged_len {
            let completion = self.staged[self.staged_next];
            self.staged_next += 1;
            self.discard(completion);
        }
        while self.pushed > 0 {
            let Ok(n) = Backend::queue_poll(&mut self.raw, &mut self.staged, None) else {
                // Leaving operations in flight would be unsound.
                marlin_pal::process::abort();
            };
            self.pushed -= n as u32;
            for i in 0..n {
                self.discard(self.staged[i]);
            }
        }
        // SAFETY: `raw` is not used again, and nothing is in flight.
        unsafe { ManuallyDrop::drop(&mut self.raw) };
        let requests = ptr::slice_from_raw_parts_mut(self.requests.as_ptr(), self.slots.len());
        // SAFETY: allocated in `create` as a boxed slice of `capacity` requests; the OS is done
        // with them.
        drop(unsafe { Box::from_raw(requests) });
        // The sockets close as `sockets` drops.
    }
}

/// Slot `index`'s request.
fn request(requests: NonNull<Request>, index: u32) -> NonNull<Request> {
    // SAFETY: callers pass slot indices, which are below capacity, the length of the array.
    unsafe { requests.add(index as usize) }
}

fn open(sockets: &[Entry], socket: SocketId) -> Result<&Entry> {
    match sockets.get(socket.index as usize) {
        Some(entry) if entry.generation == socket.generation && entry.state == State::Open => {
            Ok(entry)
        }
        _ => Err(Error::from_kind(ErrorKind::InvalidInput)),
    }
}

fn open_mut(sockets: &mut [Entry], socket: SocketId) -> Result<&mut Entry> {
    match sockets.get_mut(socket.index as usize) {
        Some(entry) if entry.generation == socket.generation && entry.state == State::Open => {
            Ok(entry)
        }
        _ => Err(Error::from_kind(ErrorKind::InvalidInput)),
    }
}

/// `range` within `0..len`, if it fits.
pub(crate) fn bounds(range: &impl RangeBounds<usize>, len: usize) -> Option<(usize, usize)> {
    let start = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n.checked_add(1)?,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n.checked_add(1)?,
        Bound::Excluded(&n) => n,
        Bound::Unbounded => len,
    };
    (start <= end && end <= len).then_some((start, end))
}

fn invalid(buffer: IoBuffer) -> SubmitError {
    SubmitError {
        error: Error::from_kind(ErrorKind::InvalidInput),
        buffer,
    }
}

fn submit_error((error, buffer): (Error, Option<IoBuffer>)) -> SubmitError {
    SubmitError {
        error,
        buffer: buffer.unwrap(),
    }
}
