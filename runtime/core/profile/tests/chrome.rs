//! The Chrome trace export parses as JSON and has the events Perfetto expects.
#![cfg(not(marlin_build = "shipping"))]

use std::collections::HashMap;
use std::thread;

use marlin_profile::{
    ByteSink, Zone, ZoneSite, clear, frame_mark, profile_scope, set_thread_name, with_history,
};
use serde_json::Value;

/// A sink that fails after `limit` bytes.
struct Limited {
    limit: usize,
    written: usize,
}

#[derive(Debug, PartialEq)]
struct Full;

impl ByteSink for Limited {
    type Error = Full;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Full> {
        self.written += bytes.len();
        if self.written > self.limit {
            return Err(Full);
        }
        Ok(())
    }
}

#[test]
fn chrome_trace() {
    set_thread_name("main");
    clear();

    static MOVED: ZoneSite = ZoneSite {
        name: "moved \\ \"quoted\"\n",
        file: file!(),
        line: line!(),
    };
    for _ in 0..3 {
        profile_scope!("frame");
        {
            profile_scope!("child");
        }
        let zone = Zone::begin(&MOVED);
        thread::spawn(move || {
            set_thread_name("helper");
            drop(zone);
        })
        .join()
        .unwrap();
        frame_mark();
    }
    frame_mark();

    let mut json = Vec::new();
    with_history(|h| h.write_chrome_trace(&mut json)).unwrap();
    let trace: Value = serde_json::from_slice(&json).expect("valid JSON");
    assert_eq!(trace["displayTimeUnit"], "ms");
    let events = trace["traceEvents"].as_array().unwrap();

    let names: HashMap<u64, &str> = events
        .iter()
        .filter(|e| e["ph"] == "M" && e["name"] == "thread_name" && e["pid"] == 1)
        .map(|e| {
            (
                e["tid"].as_u64().unwrap(),
                e["args"]["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(names[&0], "Frames");
    assert!(names.values().any(|&n| n == "main"));
    assert!(names.values().any(|&n| n == "helper"));

    for e in events.iter().filter(|e| e["ph"] != "M") {
        assert!(
            e["ts"].is_number() && e["pid"].is_u64() && e["tid"].is_u64(),
            "{e}"
        );
        assert!(e["name"].is_string());
    }
    let complete: Vec<&Value> = events.iter().filter(|e| e["ph"] == "X").collect();
    assert!(complete.iter().all(|e| e["dur"].as_f64().unwrap() >= 0.0));
    let count = |name: &str| complete.iter().filter(|e| e["name"] == name).count();
    // The first frame's `frame` zone ends after its mark, in the second frame, and so on.
    assert_eq!(count("frame"), 3);
    assert_eq!(count("child"), 3);
    let frames = complete.iter().filter(|e| e["cat"] == "frame").count();
    assert_eq!(frames, 4);

    // Every zone is on the main thread's track, and children lie within their parent.
    let main_tid = *names.iter().find(|(_, n)| **n == "main").unwrap().0;
    let span = |e: &Value| {
        let ts = e["ts"].as_f64().unwrap();
        (ts, ts + e["dur"].as_f64().unwrap())
    };
    let parents: Vec<_> = complete
        .iter()
        .filter(|e| e["name"] == "frame")
        .map(|e| span(e))
        .collect();
    for child in complete.iter().filter(|e| e["name"] == "child") {
        assert_eq!(child["tid"], main_tid);
        let (s, e) = span(child);
        assert!(parents.iter().any(|&(ps, pe)| ps <= s && e <= pe));
    }

    // Zones that moved threads are async slices with matching begin and end.
    let begins: Vec<_> = events.iter().filter(|e| e["ph"] == "b").collect();
    let ends: Vec<_> = events.iter().filter(|e| e["ph"] == "e").collect();
    assert_eq!(begins.len(), 3);
    assert_eq!(ends.len(), 3);
    for b in &begins {
        assert_eq!(b["name"], "moved \\ \"quoted\"\n");
        assert_eq!(b["tid"], main_tid);
        assert_ne!(b["args"]["end_tid"], main_tid);
        let e = ends.iter().find(|e| e["id"] == b["id"]).unwrap();
        assert_eq!(e["name"], b["name"]);
        assert!(e["ts"].as_f64().unwrap() >= b["ts"].as_f64().unwrap());
    }

    // A failing sink's error comes back.
    let mut sink = Limited {
        limit: json.len() / 2,
        written: 0,
    };
    assert_eq!(with_history(|h| h.write_chrome_trace(&mut sink)), Err(Full));
}
