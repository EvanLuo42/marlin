//! Workers, fibers and queues.
//!
//! Each worker thread runs [`scheduler_loop`] on a fiber from the pool: it resumes ready fibers
//! first, then runs queued jobs, then sleeps. A job that waits parks its fiber and the worker
//! continues on another one (see `wait.rs`). The registered main thread runs the same loop while
//! it waits (see `main_thread.rs`), with its own stack parked as one more fiber.
//!
//! Whatever must happen only once a fiber's context is saved (returning it to the pool, queuing
//! it as a waiter) is left in [`Worker::pending`] and done by the fiber switched to. Doing it
//! before the switch would let another thread resume a fiber that is still running.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;
use core::cell::{Cell, UnsafeCell};
use core::ffi::c_void;
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering, fence};

use marlin_std::sync::{Condvar, Mutex};
use marlin_std::thread::fiber::{self, Context, Stack};
use marlin_std::thread::{self, Builder, CpuSet, Error, JoinHandle};

use crate::counter::Counter;
use crate::deque::{Deque, Steal};

/// Times an idle worker checks for work (between short spins) before it sleeps.
const IDLE_SPINS: u32 = 64;
/// Longest pause between two looks, in spin-loop hints.
const MAX_PAUSE: u32 = 256;
/// Most Normal jobs a worker moves from the global queue to its own at once.
const SHARE_MAX: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Worker threads; 0 means one per logical CPU but one (the main thread's).
    pub workers: usize,
    /// Fibers allocated up front (at least workers + 2). Every job parked in a wait holds one,
    /// so deep or wide waiting job graphs need more; the pool grows on demand up to
    /// [`max_fibers`](Self::max_fibers).
    pub fibers: usize,
    /// Most fibers the pool may grow to (at least [`fibers`](Self::fibers)). At the cap, waits
    /// stop parking and help instead: see the crate docs.
    pub max_fibers: usize,
    pub fiber_stack_size: usize,
    /// Pins worker `i` to logical CPU `i + 1`, leaving CPU 0 to the main thread.
    pub pin_workers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workers: 0,
            fibers: 128,
            max_fibers: 1024,
            fiber_stack_size: 256 << 10,
            pin_workers: false,
        }
    }
}

/// Queues are served strictly in priority order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
}

#[derive(Clone, Copy)]
pub(crate) struct Job {
    pub(crate) run: unsafe fn(*mut ()),
    pub(crate) data: *mut (),
}

// SAFETY: jobs are built from `Send` closures (see `Scope::spawn_with`).
unsafe impl Send for Job {}

pub(crate) struct Fiber {
    context: Context,
    /// Link in lists of fibers being made ready.
    pub(crate) next: *mut Fiber,
    shared: *const Shared,
    /// Resumes only on the main thread: the main thread's own stack, or a fiber running a
    /// main-thread job. Written only by the fiber itself while it runs.
    main_only: bool,
    /// The counter of the detached job running on this fiber (for `is_cancelled`), or null.
    job: *const Counter,
    /// `None` for the main thread's own stack.
    _stack: Option<Stack>,
}

struct FiberPtr(*mut Fiber);

// SAFETY: queued fibers are suspended; whoever dequeues one owns it.
unsafe impl Send for FiberPtr {}

/// Keeps a hot atomic on a cache line of its own.
#[repr(align(64))]
pub(crate) struct Line<T>(T);

impl<T> core::ops::Deref for Line<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// A locked FIFO whose length is mirrored in an atomic, so empty queues are skipped without
/// taking their lock.
#[repr(align(64))]
struct Queue<T> {
    len: AtomicUsize,
    items: Mutex<VecDeque<T>>,
}

impl<T> Queue<T> {
    const fn new() -> Self {
        Self {
            len: AtomicUsize::new(0),
            items: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, item: T) {
        let mut items = self.items.lock();
        items.push_back(item);
        self.len.store(items.len(), Ordering::Relaxed);
    }

    fn pop(&self) -> Option<T> {
        if self.is_empty() || self.items.is_locked() {
            return None;
        }
        let mut items = self.items.try_lock()?;
        let item = items.pop_front();
        self.len.store(items.len(), Ordering::Relaxed);
        item
    }

    /// Pops one item and moves a `1 / takers` share of the rest (at most `max`) into `rest`: one
    /// lock for several items when `takers` threads drain one queue.
    fn pop_share(&self, takers: usize, max: usize, mut rest: impl FnMut(T)) -> Option<T> {
        if self.is_empty() || self.items.is_locked() {
            return None;
        }
        let mut items = self.items.try_lock()?;
        let item = items.pop_front()?;
        let n = (items.len() / takers).min(max);
        for item in items.drain(..n) {
            rest(item);
        }
        self.len.store(items.len(), Ordering::Relaxed);
        Some(item)
    }

    /// Only a hint: may change at once. A sleeper rechecks after its fence (see `sleep`).
    fn is_empty(&self) -> bool {
        self.len.load(Ordering::Relaxed) == 0
    }
}

struct Pool {
    free: Vec<FiberPtr>,
    /// Every fiber made, freed with the system.
    all: Vec<*mut Fiber>,
}

/// What the registered main thread uses while it runs jobs. See `main_thread.rs`.
pub(crate) struct Main {
    /// Whether a thread is registered.
    pub(crate) registered: AtomicBool,
    worker: UnsafeCell<Worker>,
    /// The main thread's own stack, as a fiber parked while the thread runs jobs elsewhere.
    fiber: UnsafeCell<Fiber>,
}

pub(crate) struct Shared {
    /// Global queues by priority: jobs from outside threads, and High/Low jobs from anywhere.
    queues: [Queue<Job>; 3],
    /// Per worker, plus the main thread's last: Normal jobs spawned by its jobs. The owner pops
    /// the newest (depth first, which bounds how many fibers nested waits park); idle workers
    /// steal the oldest.
    locals: Box<[Deque]>,
    /// Parked fibers whose wait is over; served before new jobs.
    ready: Queue<FiberPtr>,
    /// As `ready` and `queues`, for the main thread only: its own stack and fibers running
    /// main-thread jobs, and main-thread jobs.
    main_ready: Queue<FiberPtr>,
    main_jobs: Queue<Job>,
    pool: Mutex<Pool>,
    /// Mirrors `pool.free.len()`, for helpers.
    free_len: Line<AtomicUsize>,
    /// Fibers made or being made.
    fiber_count: AtomicUsize,
    max_fibers: usize,
    stack_size: usize,
    /// Waits running at the fiber cap (see `wait.rs`), woken when a fiber is freed.
    pub(crate) helpers: Line<AtomicU32>,
    /// Workers (and the main thread) looking for work in `idle`.
    searching: Line<AtomicU32>,
    max_searchers: u32,
    /// Searches handed to woken sleepers (see `notify`); under `idle`.
    search_tokens: AtomicU32,
    /// Workers (and the main thread) in `sleep`, counted under `idle`.
    sleepers: Line<AtomicU32>,
    main_sleeping: AtomicBool,
    idle: Mutex<()>,
    wake: Condvar,
    shutdown: AtomicBool,
    pub(crate) main: Main,
    /// Detached jobs (see `handle.rs`) queued, waiting on dependencies or running: the system
    /// waits for them before it stops.
    pub(crate) detached: Counter,
}

// SAFETY: the raw pointers are to fibers owned by `Shared` and handed between threads only
// through its queues; `main` is used only by the registered main thread.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Shared {
    pub(crate) fn push_job(&self, job: Job, priority: Priority) {
        match self.local_index() {
            // SAFETY: we are worker `index`'s thread, the only one using its deque's owner side.
            Some(index) if priority == Priority::Normal => unsafe { self.locals[index].push(job) },
            _ => self.queues[priority as usize].push(job),
        }
        self.notify();
    }

    /// Queues a job only the main thread runs.
    pub(crate) fn push_main(&self, job: Job) {
        self.main_jobs.push(job);
        self.notify_main();
    }

    /// The calling worker's index, if it is one of ours. Our registered main thread has one
    /// even while it runs no jobs: it alone uses the owner side of its local queue.
    fn local_index(&self) -> Option<usize> {
        let worker = current_worker();
        if worker.is_null() {
            return ptr::eq(current_main(), self).then(|| self.locals.len() - 1);
        }
        // SAFETY: a non-null worker pointer is valid on its thread.
        ptr::eq(unsafe { (*worker).shared }, self).then(|| unsafe { (*worker).index })
    }

    /// Next job for worker `index`: High, own newest, Normal, stolen oldest, then Low.
    ///
    /// Taking a Normal job moves a share of the queue's other jobs to the local queue (where
    /// they are next, and can be stolen): a stream of jobs from an outside thread would
    /// otherwise have every worker contend for the queue's lock with that thread, once per job.
    fn pop_job(&self, index: usize) -> Option<Job> {
        let [high, normal, low] = &self.queues;
        let local = &self.locals[index];
        // SAFETY: only worker `index`'s thread calls this with `index`: the deque's owner.
        let push = |job| unsafe { local.push(job) };
        high.pop()
            // SAFETY: as above.
            .or_else(|| unsafe { local.pop() })
            .or_else(|| normal.pop_share(self.locals.len(), SHARE_MAX, push))
            .or_else(|| self.steal(index))
            .or_else(|| low.pop())
    }

    /// As [`pop_job`](Self::pop_job), main-thread jobs first on the main thread. Returns whether
    /// the job is a main-thread one.
    pub(crate) fn pop_any(&self, is_main: bool, index: usize) -> Option<(Job, bool)> {
        if is_main && let Some(job) = self.main_jobs.pop() {
            return Some((job, true));
        }
        self.pop_job(index).map(|job| (job, false))
    }

    /// The oldest job of another worker's deque, trying each in turn after `index`.
    fn steal(&self, index: usize) -> Option<Job> {
        let n = self.locals.len();
        loop {
            let mut retry = false;
            for k in 1..n {
                match self.locals[(index + k) % n].steal() {
                    Steal::Success(job) => return Some(job),
                    Steal::Retry => retry = true,
                    Steal::Empty => {}
                }
            }
            // A lost race means someone else took a job; look again while jobs may remain.
            if !retry {
                return None;
            }
        }
    }

    pub(crate) fn pop_ready(&self, is_main: bool) -> Option<*mut Fiber> {
        if is_main && let Some(fiber) = self.main_ready.pop() {
            return Some(fiber.0);
        }
        self.ready.pop().map(|f| f.0)
    }

    /// A fiber to switch to from a job that waits: a ready one, else a free one (growing the
    /// pool if allowed). `None` at the fiber cap.
    pub(crate) fn next_fiber(&self, is_main: bool) -> Option<*mut Fiber> {
        self.pop_ready(is_main).or_else(|| self.take_free())
    }

    pub(crate) fn take_free(&self) -> Option<*mut Fiber> {
        if self.free_len.load(Ordering::Relaxed) > 0 {
            let mut pool = self.pool.lock();
            let fiber = pool.free.pop();
            self.free_len.store(pool.free.len(), Ordering::Relaxed);
            if let Some(fiber) = fiber {
                return Some(fiber.0);
            }
        }
        self.grow()
    }

    /// Makes one more fiber, unless the pool is at its cap or out of memory.
    #[cold]
    fn grow(&self) -> Option<*mut Fiber> {
        // Reserve a place under the cap, then make the stack outside the lock: it maps memory.
        let mut count = self.fiber_count.load(Ordering::Relaxed);
        loop {
            if count >= self.max_fibers {
                return None;
            }
            match self.fiber_count.compare_exchange_weak(
                count,
                count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(c) => count = c,
            }
        }
        match new_fiber(self, self.stack_size) {
            Ok(fiber) => {
                self.pool.lock().all.push(fiber);
                Some(fiber)
            }
            Err(_) => {
                self.fiber_count.fetch_sub(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn free_fiber(&self, fiber: *mut Fiber) {
        {
            let mut pool = self.pool.lock();
            pool.free.push(FiberPtr(fiber));
            self.free_len.store(pool.free.len(), Ordering::Relaxed);
        }
        // Pairs with the fence in `sleep`, like `notify`.
        fence(Ordering::SeqCst);
        if self.helpers.load(Ordering::Relaxed) > 0 {
            self.wake_all();
        }
    }

    fn has_work(&self, is_main: bool) -> bool {
        !self.ready.is_empty()
            || self.queues.iter().any(|q| !q.is_empty())
            || self.locals.iter().any(|q| !q.is_empty())
            || (is_main && (!self.main_ready.is_empty() || !self.main_jobs.is_empty()))
    }

    /// Wakes a sleeping worker after new work was queued, unless a worker is searching: it will
    /// find the work. The worker woken counts as searching from now on, so further pushes do not
    /// wake more until it is up, and whoever stops searching because it found work wakes the
    /// next (see `found_work`). A stream of jobs from one thread thus costs that thread one
    /// wakeup, not one per job; the workers wake each other as long as work keeps coming.
    fn notify(&self) {
        // A sleeper counts itself in `sleepers` under `idle` (a searcher leaves `searching`
        // first), fences, then checks the queues. With the fence here, after the push, either it
        // sees the work or we see it counted (and not searching), and then it holds `idle` until
        // it waits, so the notify below cannot come too early.
        fence(Ordering::SeqCst);
        if self.searching.load(Ordering::Relaxed) != 0
            || self.sleepers.load(Ordering::Relaxed) == 0
            || self
                .searching
                .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let _idle = self.idle.lock();
        // Claimed by the next thread to leave `sleep`, which then searches. `sleepers` counts
        // the threads yet to leave it, each claiming at most one token: without one left over,
        // nobody would take this search, so give it back. Then every thread is awake, or about
        // to check the queues in `sleep`, and finds the work anyway.
        let tokens = self.search_tokens.load(Ordering::Relaxed);
        if self.sleepers.load(Ordering::Relaxed) <= tokens {
            self.searching.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        self.search_tokens.store(tokens + 1, Ordering::Relaxed);
        self.wake.notify_one();
    }

    /// The caller, searching, found work: if it was the last searcher, more work may follow, so
    /// it wakes another worker to search.
    fn stop_searching(&self) {
        if self.searching.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.notify();
        }
    }

    /// As [`notify`](Self::notify) for main-thread work. The main thread sleeps with the
    /// workers, so all are woken; main-thread work is rare.
    fn notify_main(&self) {
        fence(Ordering::SeqCst);
        if self.main_sleeping.load(Ordering::Relaxed) {
            self.wake_all();
        }
    }

    /// Wakes every sleeper, which rechecks what it waits for.
    pub(crate) fn wake_all(&self) {
        let _idle = self.idle.lock();
        self.wake.notify_all();
    }

    /// Out of work: searches (spins, backing off) before sleeping, since work often follows
    /// soon (a job about to spawn more, an outside thread queuing a stream of them) and waking a
    /// sleeper costs far more. Returns, still searching, when there may be work.
    ///
    /// # Safety
    /// `worker` is the caller's.
    unsafe fn idle(&self, worker: *mut Worker) {
        // SAFETY: forwarded from the caller.
        let worker = unsafe { &mut *worker };
        // Few search at once: searchers poll lines that queuing threads write.
        if !worker.searching {
            if self.searching.fetch_add(1, Ordering::Relaxed) < self.max_searchers {
                worker.searching = true;
            } else {
                self.searching.fetch_sub(1, Ordering::Relaxed);
            }
        }
        if worker.searching {
            let mut pause = 4;
            for _ in 0..IDLE_SPINS {
                for _ in 0..pause {
                    core::hint::spin_loop();
                }
                pause = (pause * 2).min(MAX_PAUSE);
                if self.has_work(worker.is_main) || self.shutdown.load(Ordering::Relaxed) {
                    return;
                }
            }
            // Before sleeping, which rechecks for work after this.
            worker.searching = false;
            self.searching.fetch_sub(1, Ordering::Relaxed);
        }
        worker.searching = self.sleep(worker.is_main, None);
    }

    /// Hands back a search claimed by a sleeper that does not search (a wait at the fiber cap).
    pub(crate) fn decline_search(&self) {
        self.stop_searching();
    }

    /// Sleeps until there may be work (or, for a wait at the fiber cap, until `granted` is set
    /// or a fiber is freed). May return spuriously. Returns true if the caller was woken to
    /// search: it is counted in `searching`, and must stop (`stop_searching`) once it finds
    /// work or gives up.
    #[must_use]
    pub(crate) fn sleep(&self, is_main: bool, granted: Option<&AtomicBool>) -> bool {
        if self.has_work(is_main) {
            return false;
        }
        let mut idle = self.idle.lock();
        self.sleepers.fetch_add(1, Ordering::Relaxed);
        if is_main {
            self.main_sleeping.store(true, Ordering::Relaxed);
        }
        // See `notify`.
        fence(Ordering::SeqCst);
        let wake = self.shutdown.load(Ordering::Relaxed)
            || self.has_work(is_main)
            || granted.is_some_and(|g| {
                g.load(Ordering::Acquire) || self.free_len.load(Ordering::Relaxed) > 0
            });
        if !wake {
            self.wake.wait(&mut idle);
        }
        if is_main {
            self.main_sleeping.store(false, Ordering::Relaxed);
        }
        self.sleepers.fetch_sub(1, Ordering::Relaxed);
        let tokens = self.search_tokens.load(Ordering::Relaxed);
        if tokens > 0 {
            self.search_tokens.store(tokens - 1, Ordering::Relaxed);
        }
        tokens > 0
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for &fiber in &self.pool.get_mut().all {
            // SAFETY: the workers are gone; nothing runs on or refers to the fibers any more.
            drop(unsafe { Box::from_raw(fiber) });
        }
    }
}

fn new_fiber(shared: *const Shared, stack_size: usize) -> Result<*mut Fiber, Error> {
    let stack = Stack::new(stack_size)?;
    // SAFETY: the stack moves into the fiber with its context and lives as long.
    let context = unsafe { fiber::make_context(&stack, fiber_main, ptr::null_mut()) };
    Ok(Box::into_raw(Box::new(Fiber {
        context,
        next: null_mut(),
        shared,
        main_only: false,
        job: ptr::null(),
        _stack: Some(stack),
    })))
}

/// Makes a parked fiber runnable on its job system: on the main thread only if it must resume
/// there.
///
/// # Safety
/// `fiber` is parked and owned by the caller.
pub(crate) unsafe fn make_ready(fiber: *mut Fiber) {
    // SAFETY: a fiber's job system outlives it; `main_only` was written by the fiber before it
    // parked.
    unsafe {
        let shared = &*(*fiber).shared;
        if (*fiber).main_only {
            shared.main_ready.push(FiberPtr(fiber));
            shared.notify_main();
        } else {
            shared.ready.push(FiberPtr(fiber));
            shared.notify();
        }
    }
}

// --- Workers ---

pub(crate) enum Pending {
    None,
    /// Return this fiber to the pool.
    Free(*mut Fiber),
    /// Call `f(fiber, arg)`: queues the parked fiber as a waiter (see `wait.rs`).
    Call(unsafe fn(*mut Fiber, *mut ()), *mut Fiber, *mut ()),
}

/// Per worker thread, on its own stack; the main thread's is in `Shared::main`.
pub(crate) struct Worker {
    pub(crate) shared: *const Shared,
    /// Into `Shared::locals`.
    pub(crate) index: usize,
    /// The main thread's: also runs main-thread work, and is never told to shut down.
    pub(crate) is_main: bool,
    /// The worker thread's original context, resumed at shutdown.
    thread: Context,
    pub(crate) current: *mut Fiber,
    pending: Pending,
    /// Counted in `Shared::searching`.
    searching: bool,
    /// The main thread in `MainThread::pump`: how many more jobs (or resumed fibers) it may run
    /// before returning to its own stack.
    pub(crate) pump: Option<usize>,
    /// Counted while pumping.
    pub(crate) pumped: usize,
}

#[thread_local]
static WORKER: Cell<*mut Worker> = Cell::new(null_mut());

/// The job system whose main thread this is, or null.
#[thread_local]
static MAIN: Cell<*const Shared> = Cell::new(ptr::null());

// Fibers migrate between threads: never let the compiler reuse a thread-local's address across
// a switch. Every read goes through one of these.
#[inline(never)]
pub(crate) fn current_worker() -> *mut Worker {
    WORKER.get()
}

#[inline(never)]
fn set_current_worker(worker: *mut Worker) {
    WORKER.set(worker);
}

#[inline(never)]
pub(crate) fn current_main() -> *const Shared {
    MAIN.get()
}

#[inline(never)]
pub(crate) fn set_current_main(shared: *const Shared) {
    MAIN.set(shared);
}

/// Whether the caller runs as a worker (or the main thread running jobs).
pub(crate) fn on_worker() -> bool {
    !current_worker().is_null()
}

/// The counter of the detached job the caller runs in, or null.
pub(crate) fn current_job() -> *const Counter {
    let worker = current_worker();
    if worker.is_null() {
        return ptr::null();
    }
    // SAFETY: on a worker, its current fiber is valid.
    unsafe { (*(*worker).current).job }
}

/// Marks the running job as the detached one counted by `counter`, until it returns.
pub(crate) fn set_current_job(counter: *const Counter) {
    let worker = current_worker();
    if !worker.is_null() {
        // SAFETY: on a worker, its current fiber is valid and is us.
        unsafe { (*(*worker).current).job = counter };
    }
}

fn worker_main(shared: &Shared, index: usize, fiber: *mut Fiber) {
    let mut worker = Worker {
        shared,
        index,
        is_main: false,
        thread: Context::NULL,
        current: fiber,
        pending: Pending::None,
        searching: false,
        pump: None,
        pumped: 0,
    };
    let w = &raw mut worker;
    set_current_worker(w);
    // SAFETY: `fiber` came from the pool, so it is suspended and ours. We return here only at
    // shutdown.
    unsafe { fiber::switch(&raw mut (*w).thread, (*fiber).context) };
    set_current_worker(null_mut());
}

unsafe extern "C" fn fiber_main(_: *mut c_void) -> ! {
    after_switch();
    scheduler_loop()
}

fn scheduler_loop() -> ! {
    loop {
        // Re-read every iteration: a job may have resumed this fiber on another thread.
        let worker = current_worker();
        // SAFETY: on a worker `worker` is valid, and so is its job system.
        unsafe {
            let shared = &*(*worker).shared;
            let is_main = (*worker).is_main;
            if !is_main && shared.shutdown.load(Ordering::Acquire) {
                let me = (*worker).current;
                fiber::switch(&raw mut (*me).context, (*worker).thread);
                unreachable!("a fiber resumed after shutdown");
            }
            let pumping = is_main && (*worker).pump.is_some();
            if pumping && (*worker).pump == Some(0) {
                end_pump(worker);
            } else if let Some(fiber) = shared.pop_ready(is_main) {
                found_work(worker);
                switch_to(fiber, Pending::Free((*worker).current));
            } else if let Some((job, main)) = shared.pop_any(is_main, (*worker).index) {
                found_work(worker);
                run_job(job, main);
            } else if pumping {
                // Never sleeps: either there is work we lost a race for, or we are done.
                if !shared.has_work(true) {
                    end_pump(worker);
                }
            } else {
                shared.idle(worker);
            }
        }
    }
}

/// The main thread is done pumping: back to its own stack, in `MainThread::pump`.
///
/// # Safety
/// On the main thread, pumping.
unsafe fn end_pump(worker: *mut Worker) {
    // SAFETY: forwarded from the caller; the main fiber waits in `pump` for this switch.
    unsafe {
        (*worker).pump = None;
        let main = main_fiber(&*(*worker).shared);
        switch_to(main, Pending::Free((*worker).current));
    }
}

/// The worker found work: it stops searching, if it was.
///
/// # Safety
/// `worker` is the caller's.
unsafe fn found_work(worker: *mut Worker) {
    // SAFETY: forwarded from the caller; its job system outlives it.
    unsafe {
        if (*worker).searching {
            (*worker).searching = false;
            (*(*worker).shared).stop_searching();
        }
        count_pumped(worker);
    }
}

/// While the main thread pumps, counts one more job or resumed fiber.
///
/// # Safety
/// `worker` is the caller's.
pub(crate) unsafe fn count_pumped(worker: *mut Worker) {
    // SAFETY: forwarded from the caller.
    unsafe {
        if let Some(left) = (*worker).pump {
            (*worker).pump = Some(left - 1);
            (*worker).pumped += 1;
        }
    }
}

/// Runs `job` on the current fiber. A main-thread job pins the fiber to the main thread until
/// it returns.
pub(crate) fn run_job(job: Job, main: bool) {
    // SAFETY: on a worker; the fiber is the one we run on for the whole job, wherever it
    // resumes, and only it writes its own fields.
    unsafe {
        let fiber = (*current_worker()).current;
        let (main_only, cancel) = ((*fiber).main_only, (*fiber).job);
        (*fiber).main_only = main_only || main;
        (*fiber).job = ptr::null();
        (job.run)(job.data);
        (*fiber).main_only = main_only;
        (*fiber).job = cancel;
    }
}

/// Switches the current worker from its fiber to `target`, leaving `pending` for `target` to do.
///
/// # Safety
/// On a worker; `target` is suspended and owned by the caller.
pub(crate) unsafe fn switch_to(target: *mut Fiber, pending: Pending) {
    let worker = current_worker();
    // SAFETY: forwarded from the caller.
    unsafe {
        let me = (*worker).current;
        (*worker).pending = pending;
        (*worker).current = target;
        fiber::switch(&raw mut (*me).context, (*target).context);
    }
    after_switch();
}

/// Runs right after any fiber resumes: completes what the previous fiber left pending.
fn after_switch() {
    let worker = current_worker();
    // SAFETY: on a worker; the pending fiber's context is saved, so it can be handed on.
    unsafe {
        match core::mem::replace(&mut (*worker).pending, Pending::None) {
            Pending::None => {}
            Pending::Free(fiber) => (*(*fiber).shared).free_fiber(fiber),
            Pending::Call(f, fiber, arg) => f(fiber, arg),
        }
    }
}

// --- The main thread ---

/// Makes the registered main thread a worker until [`leave_main`]: its own stack becomes the
/// main fiber, parked whenever the thread runs jobs on another one.
///
/// # Safety
/// On `shared`'s registered main thread, not already between `enter_main` and `leave_main`.
pub(crate) unsafe fn enter_main(shared: *const Shared) {
    // SAFETY: forwarded from the caller; only this thread uses `main`.
    unsafe {
        let main = &(*shared).main;
        let worker = main.worker.get();
        (*worker).current = main.fiber.get();
        set_current_worker(worker);
    }
}

/// # Safety
/// After [`enter_main`], back on the main thread's own stack.
pub(crate) unsafe fn leave_main(shared: *const Shared) {
    // SAFETY: forwarded from the caller; only this thread uses `main`.
    unsafe { found_work((*shared).main.worker.get()) };
    set_current_worker(null_mut());
    // SAFETY: forwarded from the caller.
    let shared = unsafe { &*shared };
    // The main thread may have taken a wakeup meant for work it now leaves to the workers.
    if shared.has_work(false) {
        shared.notify();
    }
}

/// The main thread's own stack (the main fiber), for `pump`.
pub(crate) fn main_fiber(shared: &Shared) -> *mut Fiber {
    shared.main.fiber.get()
}

// --- System ---

pub struct JobSystem {
    pub(crate) shared: Box<Shared>,
    threads: Vec<JoinHandle<()>>,
}

struct Start {
    shared: *const Shared,
    fiber: *mut Fiber,
}

// SAFETY: `Shared` is Sync and outlives the worker threads (joined on drop); the fiber is
// suspended and handed to the one thread.
unsafe impl Send for Start {}

impl JobSystem {
    /// Starts the worker threads and allocates the fiber pool.
    pub fn new(config: Config) -> Result<Self, Error> {
        let workers = match config.workers {
            0 => thread::available_parallelism().saturating_sub(1).max(1),
            n => n,
        };
        // A loop fiber per worker and one for the main thread.
        let initial = config.fibers.max(workers + 2);
        let shared = Box::new(Shared {
            queues: [const { Queue::new() }; 3],
            locals: (0..=workers).map(|_| Deque::new()).collect(),
            ready: Queue::new(),
            main_ready: Queue::new(),
            main_jobs: Queue::new(),
            pool: Mutex::new(Pool {
                free: Vec::with_capacity(initial),
                all: Vec::with_capacity(initial),
            }),
            free_len: Line(AtomicUsize::new(0)),
            fiber_count: AtomicUsize::new(initial),
            max_fibers: config.max_fibers.max(initial),
            stack_size: config.fiber_stack_size,
            helpers: Line(AtomicU32::new(0)),
            searching: Line(AtomicU32::new(0)),
            max_searchers: workers.div_ceil(2) as u32,
            search_tokens: AtomicU32::new(0),
            sleepers: Line(AtomicU32::new(0)),
            main_sleeping: AtomicBool::new(false),
            idle: Mutex::new(()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            main: Main {
                registered: AtomicBool::new(false),
                worker: UnsafeCell::new(Worker {
                    shared: ptr::null(),
                    index: workers,
                    is_main: true,
                    thread: Context::NULL,
                    current: null_mut(),
                    pending: Pending::None,
                    searching: false,
                    pump: None,
                    pumped: 0,
                }),
                fiber: UnsafeCell::new(Fiber {
                    context: Context::NULL,
                    next: null_mut(),
                    shared: ptr::null(),
                    main_only: true,
                    job: ptr::null(),
                    _stack: None,
                }),
            },
            detached: Counter::new(),
        });
        let raw: *const Shared = &*shared;
        // SAFETY: nothing runs yet.
        unsafe {
            (*shared.main.worker.get()).shared = raw;
            (*shared.main.fiber.get()).shared = raw;
        }
        let mut system = Self {
            shared,
            threads: Vec::with_capacity(workers),
        };
        {
            let mut pool = system.shared.pool.lock();
            for _ in 0..initial {
                let fiber = new_fiber(raw, config.fiber_stack_size)?;
                pool.all.push(fiber);
                pool.free.push(FiberPtr(fiber));
            }
            // The workers' first fibers, taken here so a late-starting thread finds its own.
            pool.free.truncate(initial - workers);
            system
                .shared
                .free_len
                .store(pool.free.len(), Ordering::Relaxed);
        }

        let parallelism = thread::available_parallelism();
        for i in 0..workers {
            let mut builder = Builder::new().name(format!("marlin-worker-{i}"));
            if config.pin_workers && i + 1 < parallelism {
                builder = builder.affinity(CpuSet::single(i as u32 + 1));
            }
            let start = Start {
                shared: raw,
                fiber: system.shared.pool.lock().all[initial - workers + i],
            };
            let handle = builder.spawn(move || {
                let start = start;
                // SAFETY: the system joins this thread before freeing `Shared`.
                worker_main(unsafe { &*start.shared }, i, start.fiber);
            })?;
            system.threads.push(handle);
        }
        Ok(system)
    }

    pub fn worker_count(&self) -> usize {
        self.threads.len()
    }

    /// Fibers allocated so far: [`Config::fibers`] or more, at most [`Config::max_fibers`].
    pub fn fiber_count(&self) -> usize {
        self.shared.pool.lock().all.len()
    }
}

impl Drop for JobSystem {
    /// Waits for every detached job (including those still waiting on dependencies), then stops
    /// the workers. While it waits, the calling thread acts as the main thread (unless it is
    /// another system's), so pending main-thread jobs run. Scopes guarantee no scoped job is
    /// outstanding. Must not be called from a job.
    fn drop(&mut self) {
        let main = self.try_main_thread();
        self.shared.detached.wait();
        drop(main);
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_all();
        for thread in self.threads.drain(..) {
            thread.join();
        }
    }
}
