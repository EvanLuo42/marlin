use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::{Event, Parker, ResetMode};
use marlin_test_support::{assert_waited, wait_until};

static STATIC_EVENT: Event = Event::manual();
static STATIC_PARKER: Parker = Parker::new();

#[test]
fn manual_wait_timeout_expires() {
    let event = Event::manual();
    let timeout = Duration::from_millis(30);
    let start = Instant::now();
    assert!(!event.wait_timeout(timeout));
    assert_waited(start, timeout);
    assert!(!event.wait_timeout(Duration::ZERO));
}

#[test]
fn manual_releases_all_and_stays_set() {
    let event = Arc::new(Event::manual());
    let waiters: Vec<_> = (0..6)
        .map(|_| {
            let event = event.clone();
            thread::spawn(move || event.wait())
        })
        .collect();
    thread::sleep(Duration::from_millis(20));
    event.set();
    for w in waiters {
        w.join().unwrap();
    }
    assert!(event.is_set());
    assert!(event.wait_timeout(Duration::ZERO));
    event.reset();
    assert!(!event.is_set());
    assert!(!event.wait_timeout(Duration::from_millis(5)));
}

#[test]
fn manual_pulse_releases_current_waiters() {
    // set immediately followed by reset still wakes everyone who was waiting.
    let event = Arc::new(Event::manual());
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let event = event.clone();
            thread::spawn(move || assert!(event.wait_timeout(Duration::from_secs(10))))
        })
        .collect();
    thread::sleep(Duration::from_millis(30));
    event.set();
    event.reset();
    for w in waiters {
        w.join().unwrap();
    }
}

#[test]
fn static_event() {
    let waiter = thread::spawn(|| STATIC_EVENT.wait());
    STATIC_EVENT.set();
    waiter.join().unwrap();
}

#[test]
fn auto_releases_one_waiter_per_set() {
    const WAITERS: usize = 4;
    let event = Arc::new(Event::new(ResetMode::Auto, false));
    let released = Arc::new(AtomicUsize::new(0));
    let waiters: Vec<_> = (0..WAITERS)
        .map(|_| {
            let event = event.clone();
            let released = released.clone();
            thread::spawn(move || {
                event.wait();
                released.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();
    thread::sleep(Duration::from_millis(20));

    for n in 1..=WAITERS {
        event.set();
        // Wait for the signal to be consumed before the next set, since sets do not accumulate.
        wait_until(Duration::from_secs(10), "waiter released", || {
            released.load(Ordering::SeqCst) >= n
        });
        thread::sleep(Duration::from_millis(5));
        assert_eq!(released.load(Ordering::SeqCst), n);
    }
    for w in waiters {
        w.join().unwrap();
    }
    assert!(!event.is_set());
}

#[test]
fn auto_signal_persists_until_consumed() {
    let event = Event::auto();
    event.set();
    event.set(); // Does not accumulate.
    assert!(event.is_set());
    assert!(event.wait_timeout(Duration::ZERO));
    assert!(!event.is_set());
    let timeout = Duration::from_millis(20);
    let start = Instant::now();
    assert!(!event.wait_timeout(timeout));
    assert_waited(start, timeout);
}

#[test]
fn auto_timeout_does_not_leak_waiter_count() {
    // A timed-out waiter must unregister, or later sets would issue wakes for nobody.
    let event = Event::auto();
    assert!(!event.wait_timeout(Duration::from_millis(5)));
    event.set();
    assert!(event.wait_timeout(Duration::ZERO));
}

#[test]
fn parker_token_before_park() {
    let parker = Parker::new();
    parker.unpark();
    parker.unpark(); // Tokens do not accumulate.
    parker.park();
    let timeout = Duration::from_millis(20);
    let start = Instant::now();
    assert!(!parker.park_timeout(timeout));
    assert_waited(start, timeout);
}

#[test]
fn parker_cross_thread() {
    let parked = thread::spawn(|| {
        for _ in 0..100 {
            STATIC_PARKER.park();
        }
    });
    // Each unpark lets one park through; spin until the parker has consumed the previous one.
    let mut sent = 0;
    while !parked.is_finished() {
        STATIC_PARKER.unpark();
        sent += 1;
        thread::sleep(Duration::from_micros(200));
        assert!(sent < 1_000_000);
    }
    parked.join().unwrap();
}

#[test]
fn parker_timeout_wakes_on_unpark() {
    let parker = Arc::new(Parker::new());
    let p = parker.clone();
    let waiter = thread::spawn(move || p.park_timeout(Duration::from_secs(10)));
    thread::sleep(Duration::from_millis(20));
    parker.unpark();
    assert!(waiter.join().unwrap());
}
