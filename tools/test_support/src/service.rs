use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

/// A service on a background thread that binds its own listener: for servers that cannot take
/// the std listener [`BackgroundServer`](crate::BackgroundServer) hands out (marlin-net's queue
/// binds its own). `serve` binds, reports its address through [`Ready::listening`], then serves
/// until the stop flag is set; dropping the service sets the flag and joins the thread.
pub struct BackgroundService {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Passed to the service thread: reports the address it listens on.
pub struct Ready(Sender<SocketAddr>);

impl Ready {
    pub fn listening(self, addr: SocketAddr) {
        let _ = self.0.send(addr);
    }
}

impl BackgroundService {
    /// Starts `serve` on a thread and returns once it reported its address. A panic before it
    /// did reaches the caller.
    pub fn spawn(serve: impl FnOnce(Ready, &AtomicBool) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || serve(Ready(tx), &flag));
        match rx.recv() {
            Ok(addr) => Self {
                addr,
                stop,
                thread: Some(thread),
            },
            Err(_) => match thread.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => panic!("the background service returned without listening"),
            },
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Whether the service thread has ended (it was stopped, or it failed).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stops the service and joins it, handing back how the thread ended instead of re-raising
    /// its panic: for tests that expect the service to fail.
    pub fn finish(mut self) -> std::thread::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.thread.take().map_or(Ok(()), JoinHandle::join)
    }
}

impl Drop for BackgroundService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if result.is_err() && !std::thread::panicking() {
                panic!("the background service panicked");
            }
        }
    }
}
