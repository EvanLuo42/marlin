//! Waiting on a primitive (a counter, a lock, a condition variable) from any context.
//!
//! The waiter puts a [`WaitNode`] on its own stack into the primitive's list, and whoever ends the
//! wait grants the node under the primitive's lock. How the waiter sleeps depends on where it runs:
//!
//! - **In a job** (or on the main thread while it runs jobs): the fiber is parked and the worker
//!   switches to a ready or free fiber. The node is registered by that next fiber, after the
//!   switch (see `Pending::Call`), so nobody can resume the fiber before its context is saved.
//! - **In a job, at the fiber cap** (no ready and no free fiber, and the pool may not grow): the
//!   wait *helps*. Its node is registered as a helper, and until it is granted the waiter resumes
//!   any fiber that becomes ready (parking itself properly then, which needs no new fiber) and,
//!   for counters only, runs queued jobs nested on its own stack; when there is nothing to do it
//!   sleeps with the idle workers. Locks do not run jobs nested: a nested job that locked the same
//!   lock would queue behind the waiter beneath it, which can only proceed once that job returns.
//! - **On any other thread**: the thread blocks on the primitive's condition variable.
//!
//! A granted node may vanish at once (its waiter returns), so granting reads the node first and
//! does everything that touches the primitive or the waiter's job system under the lock: a
//! blocked thread or a helper checks its node under the lock before it leaves.

use core::cell::Cell;
use core::mem::ManuallyDrop;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use marlin_std::sync::{Condvar, Mutex, MutexGuard};

use crate::scheduler::{self, Fiber, Pending, Shared};

pub(crate) struct WaitNode {
    next: *mut WaitNode,
    /// What the waiter wants, for the primitive (a shared or exclusive lock, say).
    kind: u32,
    /// Set under the primitive's lock when the wait is over.
    granted: AtomicBool,
    /// Under the primitive's lock.
    waker: Waker,
}

#[derive(Clone, Copy)]
enum Waker {
    /// An outside thread, blocked on the queue's condition variable.
    Thread,
    Fiber(*mut Fiber),
    /// A wait at the fiber cap, sleeping with its job system's idle workers between helping.
    Helper(*const Shared),
}

/// Something a wait node can be queued on.
pub(crate) trait Wait {
    /// Under the lock: queues `node` and returns true, or returns false (not queuing it) when
    /// there is nothing to wait for.
    fn register(&self, node: *mut WaitNode) -> bool;

    fn queue(&self) -> &dyn Queue;
}

/// The parts of a [`WaitQueue`] that do not depend on its state.
pub(crate) trait Queue {
    /// Under the lock: makes the queued `node` wake `fiber`, or returns false if it was granted.
    fn rebind(&self, node: *mut WaitNode, fiber: *mut Fiber) -> bool;
    /// Blocks the calling thread until the queued `node` is granted.
    fn block_thread(&self, node: *mut WaitNode);
    /// Takes and releases the lock: whoever granted a node under it is done with that node.
    fn sync(&self);
}

/// A FIFO of wait nodes and the primitive's own state `S`, under one lock.
pub(crate) struct WaitQueue<S> {
    inner: Mutex<Inner<S>>,
    /// What blocked outside threads sleep on.
    threads: Condvar,
}

struct Inner<S> {
    head: *mut WaitNode,
    tail: *mut WaitNode,
    state: S,
}

// SAFETY: the list is only touched under the mutex; its nodes belong to waiting contexts that
// stay put until granted.
unsafe impl<S: Send> Send for WaitQueue<S> {}
// SAFETY: as above.
unsafe impl<S: Send> Sync for WaitQueue<S> {}

impl<S> WaitQueue<S> {
    pub(crate) const fn new(state: S) -> Self {
        Self {
            inner: Mutex::new(Inner {
                head: null_mut(),
                tail: null_mut(),
                state,
            }),
            threads: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> QueueGuard<'_, S> {
        QueueGuard {
            queue: self,
            inner: ManuallyDrop::new(self.inner.lock()),
            fibers: null_mut(),
            threads: false,
        }
    }

    /// Waits until a node of `kind` is granted. `register` runs under the lock and returns
    /// whether to wait at all; when it does, the node is queued last. `helps`: see the module
    /// docs.
    pub(crate) fn wait(&self, kind: u32, helps: bool, register: impl FnOnce(&mut S) -> bool) {
        let waiting = Waiting {
            queue: self,
            register: Cell::new(Some(register)),
        };
        block(&waiting, kind, helps);
    }
}

impl<S> Queue for WaitQueue<S> {
    fn rebind(&self, node: *mut WaitNode, fiber: *mut Fiber) -> bool {
        let _inner = self.inner.lock();
        // SAFETY: the node is queued, so it is live, and we hold the lock that guards `waker`.
        unsafe {
            if (*node).granted.load(Ordering::Relaxed) {
                return false;
            }
            (*node).waker = Waker::Fiber(fiber);
        }
        true
    }

    fn block_thread(&self, node: *mut WaitNode) {
        let mut inner = self.inner.lock();
        // SAFETY: the node is ours and stays live until we return.
        while !unsafe { (*node).granted.load(Ordering::Relaxed) } {
            self.threads.wait(&mut inner);
        }
    }

    fn sync(&self) {
        drop(self.inner.lock());
    }
}

/// The locked queue. Nodes granted through it are woken as the guard drops: blocked threads are
/// notified before the lock is released, fibers made ready after.
pub(crate) struct QueueGuard<'a, S> {
    queue: &'a WaitQueue<S>,
    inner: ManuallyDrop<MutexGuard<'a, Inner<S>>>,
    /// Granted fibers, linked through `Fiber::next`.
    fibers: *mut Fiber,
    threads: bool,
}

impl<S> QueueGuard<'_, S> {
    pub(crate) fn state(&mut self) -> &mut S {
        &mut self.inner.state
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inner.head.is_null()
    }

    /// The first waiter's kind.
    pub(crate) fn front(&self) -> Option<u32> {
        // SAFETY: queued nodes are live.
        (!self.is_empty()).then(|| unsafe { (*self.inner.head).kind })
    }

    pub(crate) fn push(&mut self, node: *mut WaitNode) {
        // SAFETY: the node is live until granted, and the list is ours under the lock.
        unsafe {
            (*node).next = null_mut();
            if self.inner.tail.is_null() {
                self.inner.head = node;
            } else {
                (*self.inner.tail).next = node;
            }
        }
        self.inner.tail = node;
    }

    /// Ends the first waiter's wait. Returns false if there was none.
    pub(crate) fn grant_front(&mut self) -> bool {
        let node = self.inner.head;
        if node.is_null() {
            return false;
        }
        // SAFETY: queued nodes are live until granted; we read all we need first.
        unsafe {
            self.inner.head = (*node).next;
            if self.inner.head.is_null() {
                self.inner.tail = null_mut();
            }
            match (*node).waker {
                Waker::Thread => {
                    (*node).granted.store(true, Ordering::Release);
                    self.threads = true;
                }
                Waker::Fiber(fiber) => {
                    (*node).granted.store(true, Ordering::Release);
                    // The fiber is parked until made ready, and so is its node: linking through
                    // the fiber is safe.
                    (*fiber).next = self.fibers;
                    self.fibers = fiber;
                }
                Waker::Helper(shared) => {
                    (*node).granted.store(true, Ordering::Release);
                    // Under the lock: the helper leaves only after taking it (`sync`), so its
                    // job system is still there.
                    (*shared).wake_all();
                }
            }
        }
        true
    }

    pub(crate) fn grant_all(&mut self) {
        while self.grant_front() {}
    }
}

impl<S> Drop for QueueGuard<'_, S> {
    fn drop(&mut self) {
        if self.threads {
            // Before unlocking: a woken thread may destroy the primitive as soon as it leaves.
            self.queue.threads.notify_all();
        }
        // SAFETY: dropped once, here.
        unsafe { ManuallyDrop::drop(&mut self.inner) };
        let mut fiber = self.fibers;
        while !fiber.is_null() {
            // SAFETY: granted fibers stay parked, and ours to hand over, until made ready.
            unsafe {
                let next = (*fiber).next;
                scheduler::make_ready(fiber);
                fiber = next;
            }
        }
    }
}

/// A wait on a [`WaitQueue`] with its registration closure.
struct Waiting<'a, S, F> {
    queue: &'a WaitQueue<S>,
    /// Taken by the one call to `register`.
    register: Cell<Option<F>>,
}

impl<S, F: FnOnce(&mut S) -> bool> Wait for Waiting<'_, S, F> {
    fn register(&self, node: *mut WaitNode) -> bool {
        let mut queue = self.queue.lock();
        let register = self.register.take().expect("wait registered twice");
        if !register(queue.state()) {
            return false;
        }
        queue.push(node);
        true
    }

    fn queue(&self) -> &dyn Queue {
        self.queue
    }
}

/// Waits on `primitive` until a node of `kind` is granted, however the caller runs: see the
/// module docs.
pub(crate) fn block<W: Wait>(primitive: &W, kind: u32, helps: bool) {
    let mut node = WaitNode {
        next: null_mut(),
        kind,
        granted: AtomicBool::new(false),
        waker: Waker::Thread,
    };
    let node = &raw mut node;
    if scheduler::on_worker() {
        // SAFETY: on a worker, the node outlives the wait.
        unsafe { block_on_worker(primitive, node, helps) };
        return;
    }
    let main = scheduler::current_main();
    if !main.is_null() {
        // SAFETY: the calling thread is the registered main thread of `main`, which outlives it
        // (`MainThread` borrows the system), and it is not running jobs yet.
        unsafe {
            scheduler::enter_main(main);
            block_on_worker(primitive, node, helps);
            scheduler::leave_main(main);
        }
        return;
    }
    if primitive.register(node) {
        primitive.queue().block_thread(node);
    }
}

/// # Safety
/// On a worker; `node` is the caller's and outlives the wait.
unsafe fn block_on_worker<W: Wait>(primitive: &W, node: *mut WaitNode, helps: bool) {
    let worker = scheduler::current_worker();
    // SAFETY: on a worker, its job system and current fiber are valid. The fiber stays the same
    // for the whole wait even if it migrates.
    unsafe {
        let shared = &*(*worker).shared;
        let me = (*worker).current;
        if let Some(next) = shared.next_fiber((*worker).is_main) {
            (*node).waker = Waker::Fiber(me);
            let mut args = (primitive, node);
            scheduler::switch_to(
                next,
                Pending::Call(register_parked::<W>, me, (&raw mut args).cast()),
            );
            return;
        }

        // At the fiber cap.
        (*node).waker = Waker::Helper(shared);
        if !primitive.register(node) {
            return;
        }
        shared.helpers.fetch_add(1, Ordering::SeqCst);
        loop {
            if (*node).granted.load(Ordering::Acquire) {
                primitive.queue().sync();
                break;
            }
            // Re-read: a nested job may have parked and resumed on another thread.
            let worker = scheduler::current_worker();
            let is_main = (*worker).is_main;
            if let Some(next) = shared.next_fiber(is_main) {
                shared.helpers.fetch_sub(1, Ordering::Relaxed);
                let mut args = (primitive, node);
                scheduler::switch_to(
                    next,
                    Pending::Call(rebind_parked::<W>, me, (&raw mut args).cast()),
                );
                return;
            }
            if helps && let Some((job, main)) = shared.pop_any(is_main, (*worker).index) {
                scheduler::run_job(job, main);
                continue;
            }
            // `sleep` returns at once while jobs are queued that a lock wait may not run.
            core::hint::spin_loop();
            if shared.sleep(is_main, Some(&(*node).granted)) {
                shared.decline_search();
            }
        }
        shared.helpers.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Run by the fiber switched to: queues the parked `fiber`'s node, or resumes it at once.
///
/// # Safety
/// `args` is the `(&W, *mut WaitNode)` of the parked `fiber`, whose context is saved.
unsafe fn register_parked<W: Wait>(fiber: *mut Fiber, args: *mut ()) {
    // SAFETY: forwarded from the caller; the parked stack holding `args` stays put until the
    // fiber is made ready, which happens only after this read.
    unsafe {
        let (primitive, node) = *args.cast::<(&W, *mut WaitNode)>();
        if !primitive.register(node) {
            scheduler::make_ready(fiber);
        }
    }
}

/// Run by the fiber switched to: hands a helping wait's node over to the parked `fiber`.
///
/// # Safety
/// As for [`register_parked`]; the node is queued as a helper.
unsafe fn rebind_parked<W: Wait>(fiber: *mut Fiber, args: *mut ()) {
    // SAFETY: as in `register_parked`.
    unsafe {
        let (primitive, node) = *args.cast::<(&W, *mut WaitNode)>();
        if !primitive.queue().rebind(node, fiber) {
            scheduler::make_ready(fiber);
        }
    }
}
