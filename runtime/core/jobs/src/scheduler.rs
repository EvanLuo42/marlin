//! Workers, fibers and queues.
//!
//! Each worker thread runs [`scheduler_loop`] on a fiber from the pool: it resumes ready fibers
//! first, then runs queued jobs, then sleeps. A job that waits parks its fiber and the worker
//! continues on another one.
//!
//! Whatever must happen only once a fiber's context is saved (returning it to the pool, parking
//! it on a counter) is left in [`Worker::pending`] and done by the fiber switched to. Doing it
//! before the switch would let another thread resume a fiber that is still running.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ffi::c_void;
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use marlin_std::sync::{Condvar, Mutex};
use marlin_std::thread::fiber::{self, Context, Stack};
use marlin_std::thread::{self, Builder, CpuSet, Error, JoinHandle};

use crate::counter::Counter;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Worker threads; 0 means one per logical CPU but one (the main thread's).
    pub workers: usize,
    /// Fibers in the pool. Every job parked in a wait holds one, so deep or wide waiting job
    /// graphs need more; running out aborts.
    pub fibers: usize,
    pub fiber_stack_size: usize,
    /// Pins worker `i` to logical CPU `i + 1`, leaving CPU 0 to the main thread.
    pub pin_workers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workers: 0,
            fibers: 128,
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

pub(crate) struct Job {
    pub(crate) run: unsafe fn(*mut ()),
    pub(crate) data: *mut (),
}

// SAFETY: jobs are built from `Send` closures (see `Scope::spawn_with`).
unsafe impl Send for Job {}

pub(crate) struct Fiber {
    context: Context,
    /// Link in a counter's waiter list.
    pub(crate) next: *mut Fiber,
    shared: *const Shared,
    _stack: Stack,
}

struct FiberPtr(*mut Fiber);

// SAFETY: queued fibers are suspended; whoever dequeues one owns it.
unsafe impl Send for FiberPtr {}

pub(crate) struct Shared {
    /// Global queues by priority: jobs from outside threads, and High/Low jobs from anywhere.
    queues: [Mutex<VecDeque<Job>>; 3],
    /// Per worker: Normal jobs spawned by its jobs. The owner pops the newest (depth first, which
    /// bounds how many fibers nested waits park); idle workers steal the oldest.
    locals: Box<[Mutex<VecDeque<Job>>]>,
    /// Parked fibers whose wait is over; served before new jobs.
    ready: Mutex<VecDeque<FiberPtr>>,
    free: Mutex<Vec<FiberPtr>>,
    fibers: Vec<*mut Fiber>,
    /// Eventcount for idle workers: bumped after every push.
    epoch: AtomicU32,
    sleepers: AtomicU32,
    idle: Mutex<()>,
    wake: Condvar,
    shutdown: AtomicBool,
}

// SAFETY: the raw pointers are to fibers owned by `Shared` and handed between threads only
// through its queues.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Shared {
    pub(crate) fn push_job(&self, job: Job, priority: Priority) {
        match self.local_index() {
            Some(index) if priority == Priority::Normal => self.locals[index].lock().push_back(job),
            _ => self.queues[priority as usize].lock().push_back(job),
        }
        self.notify();
    }

    /// The calling worker's index, if it is one of ours.
    fn local_index(&self) -> Option<usize> {
        let worker = current_worker();
        // SAFETY: a non-null worker pointer is valid on its thread.
        (!worker.is_null() && ptr::eq(unsafe { (*worker).shared }, self))
            .then(|| unsafe { (*worker).index })
    }

    /// Next job for worker `index`: High, own newest, Normal, stolen oldest, then Low.
    fn pop_job(&self, index: usize) -> Option<Job> {
        let [high, normal, low] = &self.queues;
        high.lock()
            .pop_front()
            .or_else(|| self.locals[index].lock().pop_back())
            .or_else(|| normal.lock().pop_front())
            .or_else(|| {
                let n = self.locals.len();
                (1..n).find_map(|k| self.locals[(index + k) % n].lock().pop_front())
            })
            .or_else(|| low.lock().pop_front())
    }

    fn pop_ready(&self) -> Option<*mut Fiber> {
        self.ready.lock().pop_front().map(|f| f.0)
    }

    fn take_free(&self) -> *mut Fiber {
        match self.free.lock().pop() {
            Some(fiber) => fiber.0,
            None => panic!(
                "marlin-jobs: fiber pool exhausted ({} fibers); raise Config::fibers",
                self.fibers.len()
            ),
        }
    }

    fn has_work(&self) -> bool {
        !self.ready.lock().is_empty()
            || self.queues.iter().any(|q| !q.lock().is_empty())
            || self.locals.iter().any(|q| !q.lock().is_empty())
    }

    /// Wakes a sleeping worker after new work was queued.
    fn notify(&self) {
        // Eventcount: a worker reads `epoch` before checking the queues and again under `idle`
        // before sleeping. Bumping it after the push means the worker either sees the work, sees
        // the new epoch, or is counted in `sleepers` and gets notified.
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if self.sleepers.load(Ordering::SeqCst) > 0 {
            let _idle = self.idle.lock();
            self.wake.notify_one();
        }
    }

    fn sleep(&self) {
        let epoch = self.epoch.load(Ordering::SeqCst);
        if self.has_work() || self.shutdown.load(Ordering::Acquire) {
            return;
        }
        let mut idle = self.idle.lock();
        self.sleepers.fetch_add(1, Ordering::SeqCst);
        if self.epoch.load(Ordering::SeqCst) == epoch && !self.shutdown.load(Ordering::Acquire) {
            self.wake.wait(&mut idle);
        }
        self.sleepers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for &fiber in &self.fibers {
            // SAFETY: the workers are gone; nothing runs on or refers to the fibers any more.
            drop(unsafe { Box::from_raw(fiber) });
        }
    }
}

/// Makes a parked fiber runnable on its job system.
///
/// # Safety
/// `fiber` is parked and owned by the caller.
pub(crate) unsafe fn make_ready(fiber: *mut Fiber) {
    // SAFETY: a fiber's job system outlives it.
    let shared = unsafe { &*(*fiber).shared };
    shared.ready.lock().push_back(FiberPtr(fiber));
    shared.notify();
}

// --- Workers ---

enum Pending {
    None,
    /// Return this fiber to the pool.
    Free(*mut Fiber),
    /// Park this fiber until the counter reaches zero.
    Park(*mut Fiber, *const Counter),
}

/// Per worker thread, on its own stack.
struct Worker {
    shared: *const Shared,
    /// Into `Shared::locals`.
    index: usize,
    /// The worker thread's original context, resumed at shutdown.
    thread: Context,
    current: *mut Fiber,
    pending: Pending,
}

#[thread_local]
static WORKER: Cell<*mut Worker> = Cell::new(null_mut());

// Fibers migrate between threads: never let the compiler reuse WORKER's address across a switch.
#[inline(never)]
fn current_worker() -> *mut Worker {
    WORKER.get()
}

#[inline(never)]
fn set_current_worker(worker: *mut Worker) {
    WORKER.set(worker);
}

fn worker_main(shared: &Shared, index: usize) {
    let mut worker = Worker {
        shared,
        index,
        thread: Context::NULL,
        current: null_mut(),
        pending: Pending::None,
    };
    let w = &raw mut worker;
    set_current_worker(w);
    let fiber = shared.take_free();
    // SAFETY: `fiber` came from the pool, so it is suspended and ours. We return here only at
    // shutdown.
    unsafe {
        (*w).current = fiber;
        fiber::switch(&raw mut (*w).thread, (*fiber).context);
    }
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
        // SAFETY: on a worker thread `worker` is valid, and so is its job system.
        unsafe {
            let shared = &*(*worker).shared;
            if shared.shutdown.load(Ordering::Acquire) {
                let me = (*worker).current;
                fiber::switch(&raw mut (*me).context, (*worker).thread);
                unreachable!("a fiber resumed after shutdown");
            }
            if let Some(fiber) = shared.pop_ready() {
                switch_to(fiber, Pending::Free((*worker).current));
            } else if let Some(job) = shared.pop_job((*worker).index) {
                (job.run)(job.data);
            } else {
                shared.sleep();
            }
        }
    }
}

/// Switches the current worker from its fiber to `target`, leaving `pending` for `target` to do.
///
/// # Safety
/// On a worker; `target` is suspended and owned by the caller.
unsafe fn switch_to(target: *mut Fiber, pending: Pending) {
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
            Pending::Free(fiber) => (*(*fiber).shared).free.lock().push(FiberPtr(fiber)),
            Pending::Park(fiber, counter) => (*counter).add_waiter(fiber),
        }
    }
}

/// Parks the current job's fiber until `counter` reaches zero. Returns false, doing nothing, when
/// not called on a worker.
pub(crate) fn park_current_until_zero(counter: &Counter) -> bool {
    let worker = current_worker();
    if worker.is_null() {
        return false;
    }
    // SAFETY: on a worker; the next fiber is suspended and ours.
    unsafe {
        let shared = &*(*worker).shared;
        let next = shared.pop_ready().unwrap_or_else(|| shared.take_free());
        switch_to(next, Pending::Park((*worker).current, counter));
    }
    true
}

// --- System ---

pub struct JobSystem {
    pub(crate) shared: Box<Shared>,
    threads: Vec<JoinHandle<()>>,
}

struct SharedPtr(*const Shared);

// SAFETY: `Shared` is Sync and outlives the worker threads (joined on drop).
unsafe impl Send for SharedPtr {}

impl JobSystem {
    /// Starts the worker threads and allocates the fiber pool.
    pub fn new(config: Config) -> Result<Self, Error> {
        let workers = match config.workers {
            0 => thread::available_parallelism().saturating_sub(1).max(1),
            n => n,
        };
        let fiber_count = config.fibers.max(workers + 1);
        let mut fibers = Vec::with_capacity(fiber_count);
        for _ in 0..fiber_count {
            let stack = Stack::new(config.fiber_stack_size)?;
            // SAFETY: the stack moves into the fiber with its context and lives as long.
            let context = unsafe { fiber::make_context(&stack, fiber_main, ptr::null_mut()) };
            fibers.push(Box::into_raw(Box::new(Fiber {
                context,
                next: null_mut(),
                shared: ptr::null(),
                _stack: stack,
            })));
        }

        let shared = Box::new(Shared {
            queues: [const { Mutex::new(VecDeque::new()) }; 3],
            locals: (0..workers).map(|_| Mutex::new(VecDeque::new())).collect(),
            ready: Mutex::new(VecDeque::new()),
            free: Mutex::new(fibers.iter().map(|&f| FiberPtr(f)).collect()),
            fibers,
            epoch: AtomicU32::new(0),
            sleepers: AtomicU32::new(0),
            idle: Mutex::new(()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        for &fiber in &shared.fibers {
            // SAFETY: no fiber runs yet.
            unsafe { (*fiber).shared = &*shared };
        }

        let mut system = Self {
            shared,
            threads: Vec::with_capacity(workers),
        };
        let parallelism = thread::available_parallelism();
        for i in 0..workers {
            let mut builder = Builder::new().name(format!("marlin-worker-{i}"));
            if config.pin_workers && i + 1 < parallelism {
                builder = builder.affinity(CpuSet::single(i as u32 + 1));
            }
            let shared = SharedPtr(&*system.shared);
            let handle = builder.spawn(move || {
                let shared = shared;
                // SAFETY: the system joins this thread before freeing `Shared`.
                worker_main(unsafe { &*shared.0 }, i);
            })?;
            system.threads.push(handle);
        }
        Ok(system)
    }

    pub fn worker_count(&self) -> usize {
        self.threads.len()
    }
}

impl Drop for JobSystem {
    /// Stops the workers once they finish what they are running. Scopes guarantee no job is
    /// outstanding. Must not be called from a job of this system.
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        {
            let _idle = self.shared.idle.lock();
            self.shared.wake.notify_all();
        }
        for thread in self.threads.drain(..) {
            thread.join();
        }
    }
}
