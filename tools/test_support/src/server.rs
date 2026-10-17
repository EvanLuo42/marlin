use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// A server on a background thread, listening on an ephemeral loopback port. `serve` gets the
/// listener (non-blocking) and a stop flag it must poll; dropping the server sets the flag and
/// joins the thread.
pub struct BackgroundServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BackgroundServer {
    pub fn spawn(serve: impl FnOnce(TcpListener, &AtomicBool) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || serve(listener, &flag));
        Self {
            addr,
            stop,
            thread: Some(thread),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `host:port`, for clients that take a string.
    pub fn url(&self) -> String {
        self.addr.to_string()
    }

    /// Whether the server thread has been asked to stop.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

impl Drop for BackgroundServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if result.is_err() && !std::thread::panicking() {
                panic!("the background server panicked");
            }
        }
    }
}
