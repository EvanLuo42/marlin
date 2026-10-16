// Each test binary uses a different subset.
#![allow(dead_code)]

use std::fmt::Write;
use std::sync::Mutex;

use marlin_diag::{Level, Record, Sink};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    pub level: Level,
    pub target: String,
    pub file: String,
    pub line: u32,
    pub thread_id: u64,
    pub message: String,
}

/// Records everything it is given. Tests in one binary run in parallel and share the registry,
/// so each test reads back only its own thread's records.
pub struct Capture {
    records: Mutex<Vec<Captured>>,
}

impl Capture {
    pub const fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    pub fn take_current_thread(&self) -> Vec<Captured> {
        let id = marlin_pal::thread::current_id();
        let mut records = self.records.lock().unwrap();
        let (mine, rest) = records.drain(..).partition(|r| r.thread_id == id);
        *records = rest;
        mine
    }

    pub fn messages_current_thread(&self) -> Vec<String> {
        self.take_current_thread()
            .into_iter()
            .map(|r| r.message)
            .collect()
    }
}

impl Sink for Capture {
    fn log(&self, record: &Record<'_>) {
        let mut message = String::new();
        write!(message, "{}", record.args()).unwrap();
        self.records.lock().unwrap().push(Captured {
            level: record.level(),
            target: record.target().to_owned(),
            file: record.file().to_owned(),
            line: record.line(),
            thread_id: record.thread_id(),
            message,
        });
    }
}
