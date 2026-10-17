use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use marlin_sync::{
    Channel, RecvError, RecvTimeoutError, SendError, SendTimeoutError, TryRecvError, TrySendError,
};

static STATIC_CHANNEL: Channel<u32, 4> = Channel::new();

/// Timed waits may end a little early by the OS's clock; allow 1 ms, as the PAL tests do.
fn assert_waited(start: Instant, timeout: Duration) {
    let elapsed = start.elapsed();
    assert!(elapsed + Duration::from_millis(1) >= timeout, "{elapsed:?}");
}

/// Counts its drops, to check the channel drops exactly what it still holds.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn fifo_and_bounds() {
    let ch = Channel::<u32, 3>::new();
    assert_eq!(ch.capacity(), 3);
    assert!(ch.is_empty());
    assert_eq!(ch.try_recv(), Err(TryRecvError::Empty));
    for i in 0..3 {
        ch.try_send(i).unwrap();
    }
    assert!(ch.is_full());
    assert_eq!(ch.len(), 3);
    assert_eq!(ch.try_send(9), Err(TrySendError::Full(9)));
    for i in 0..3 {
        assert_eq!(ch.try_recv(), Ok(i));
    }
    assert!(ch.is_empty());
}

#[test]
fn wraps_around_many_laps() {
    // Capacity 3 is not a power of two, so index and lap arithmetic both get exercised.
    let ch = Channel::<usize, 3>::new();
    let mut next_out = 0;
    for i in 0..1_000 {
        ch.send(i).unwrap();
        if i % 3 == 2 {
            assert_eq!(ch.len(), 3);
            while let Ok(v) = ch.try_recv() {
                assert_eq!(v, next_out);
                next_out += 1;
            }
        }
    }
    while let Ok(v) = ch.try_recv() {
        assert_eq!(v, next_out);
        next_out += 1;
    }
    assert_eq!(next_out, 1_000);
}

#[test]
fn capacity_one() {
    let ch = Channel::<u8, 1>::new();
    for i in 0..10 {
        ch.try_send(i).unwrap();
        assert_eq!(ch.try_send(i), Err(TrySendError::Full(i)));
        assert_eq!(ch.len(), 1);
        assert_eq!(ch.recv(), Ok(i));
    }
}

#[test]
fn close_rejects_sends_and_drains() {
    let ch = Channel::<String, 4>::new();
    ch.send("a".to_owned()).unwrap();
    ch.send("b".to_owned()).unwrap();
    assert!(ch.close());
    assert!(!ch.close());
    assert!(ch.is_closed());

    let SendError(back) = ch.send("c".to_owned()).unwrap_err();
    assert_eq!(back, "c");
    assert_eq!(
        ch.try_send("d".to_owned()).unwrap_err().into_inner(),
        "d".to_owned()
    );
    match ch.send_timeout("e".to_owned(), Duration::from_secs(10)) {
        Err(SendTimeoutError::Closed(v)) => assert_eq!(v, "e"),
        other => panic!("{other:?}"),
    }

    assert_eq!(ch.recv().as_deref(), Ok("a"));
    assert_eq!(ch.try_recv().as_deref(), Ok("b"));
    assert_eq!(ch.recv(), Err(RecvError));
    assert_eq!(ch.try_recv(), Err(TryRecvError::Closed));
    assert_eq!(
        ch.recv_timeout(Duration::from_secs(10)),
        Err(RecvTimeoutError::Closed)
    );
}

#[test]
fn close_wakes_blocked_receivers_and_senders() {
    let empty = Arc::new(Channel::<u32, 2>::new());
    let receivers: Vec<_> = (0..3)
        .map(|_| {
            let ch = empty.clone();
            thread::spawn(move || ch.recv())
        })
        .collect();

    let full = Arc::new(Channel::<u32, 2>::new());
    full.send(1).unwrap();
    full.send(2).unwrap();
    let senders: Vec<_> = (0..3)
        .map(|i| {
            let ch = full.clone();
            thread::spawn(move || ch.send(10 + i))
        })
        .collect();

    thread::sleep(Duration::from_millis(30));
    empty.close();
    full.close();
    for r in receivers {
        assert_eq!(r.join().unwrap(), Err(RecvError));
    }
    let mut rejected: Vec<_> = senders
        .into_iter()
        .map(|s| s.join().unwrap().unwrap_err().into_inner())
        .collect();
    rejected.sort_unstable();
    assert_eq!(rejected, [10, 11, 12]);
    // What was in before the close is still delivered.
    assert_eq!(full.recv(), Ok(1));
    assert_eq!(full.recv(), Ok(2));
    assert_eq!(full.recv(), Err(RecvError));
}

#[test]
fn timeouts_expire() {
    let ch = Channel::<u32, 1>::new();
    let timeout = Duration::from_millis(30);

    let start = Instant::now();
    assert_eq!(ch.recv_timeout(timeout), Err(RecvTimeoutError::Timeout));
    assert_waited(start, timeout);
    assert_eq!(
        ch.recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Timeout)
    );

    ch.send(1).unwrap();
    let start = Instant::now();
    match ch.send_timeout(2, timeout) {
        Err(SendTimeoutError::Timeout(2)) => {}
        other => panic!("{other:?}"),
    }
    assert_waited(start, timeout);
    assert_eq!(ch.recv_timeout(Duration::ZERO), Ok(1));
}

#[test]
fn blocked_sender_resumes_when_space_frees() {
    let send = thread::spawn(|| {
        for i in 0..20 {
            STATIC_CHANNEL.send(i).unwrap();
        }
    });
    // Let the sender fill the channel and block.
    thread::sleep(Duration::from_millis(30));
    assert!(STATIC_CHANNEL.is_full());
    for i in 0..20 {
        assert_eq!(STATIC_CHANNEL.recv_timeout(Duration::from_secs(10)), Ok(i));
    }
    send.join().unwrap();
}

#[test]
fn blocked_receiver_resumes_on_send() {
    let ch = Arc::new(Channel::<u32, 2>::new());
    let r = {
        let ch = ch.clone();
        thread::spawn(move || ch.recv_timeout(Duration::from_secs(10)))
    };
    thread::sleep(Duration::from_millis(20));
    ch.send(7).unwrap();
    assert_eq!(r.join().unwrap(), Ok(7));
}

#[test]
fn drop_drops_leftovers() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let ch = Channel::<Counted, 4>::new();
        for _ in 0..4 {
            ch.send(Counted(drops.clone())).unwrap();
        }
        // A rejected value comes back to the caller, who drops it.
        drop(ch.try_send(Counted(drops.clone())).unwrap_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(ch.recv().unwrap());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        ch.send(Counted(drops.clone())).unwrap();
        ch.close();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
    // 4 were still inside when the channel dropped.
    assert_eq!(drops.load(Ordering::SeqCst), 6);
}

#[test]
fn drop_after_wrap_drops_exactly_leftovers() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let ch = Channel::<Counted, 3>::new();
        for _ in 0..10 {
            ch.send(Counted(drops.clone())).unwrap();
            drop(ch.recv().unwrap());
        }
        ch.send(Counted(drops.clone())).unwrap();
        ch.send(Counted(drops.clone())).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 10);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 12);
}

#[test]
fn mpmc_delivers_everything_once_in_per_producer_order() {
    const PRODUCERS: usize = 4;
    const CONSUMERS: usize = 4;
    const ITEMS: usize = 10_000;
    let ch = Arc::new(Channel::<(usize, usize), 8>::new());

    let consumers: Vec<_> = (0..CONSUMERS)
        .map(|_| {
            let ch = ch.clone();
            thread::spawn(move || {
                let mut last = [None::<usize>; PRODUCERS];
                let mut got = 0usize;
                let mut sum = 0usize;
                while let Ok((p, i)) = ch.recv() {
                    // One producer's values reach any one consumer in the order sent.
                    assert!(
                        last[p].is_none_or(|l| l < i),
                        "producer {p}: {i} after {last:?}"
                    );
                    last[p] = Some(i);
                    got += 1;
                    sum += i;
                }
                (got, sum)
            })
        })
        .collect();

    let producers: Vec<_> = (0..PRODUCERS)
        .map(|p| {
            let ch = ch.clone();
            thread::spawn(move || {
                for i in 0..ITEMS {
                    // Mix blocking and timed sends; a timed send retries until it gets through.
                    if i % 2 == 0 {
                        ch.send((p, i)).unwrap();
                    } else {
                        let mut v = (p, i);
                        loop {
                            match ch.send_timeout(v, Duration::from_micros(20)) {
                                Ok(()) => break,
                                Err(SendTimeoutError::Timeout(back)) => v = back,
                                Err(SendTimeoutError::Closed(_)) => panic!("closed early"),
                            }
                        }
                    }
                }
            })
        })
        .collect();

    for p in producers {
        p.join().unwrap();
    }
    ch.close();
    let (got, sum) = consumers
        .into_iter()
        .map(|c| c.join().unwrap())
        .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    assert_eq!(got, PRODUCERS * ITEMS);
    assert_eq!(sum, PRODUCERS * (ITEMS * (ITEMS - 1) / 2));
    assert!(ch.is_empty());
}

#[test]
fn timed_receivers_under_contention_lose_nothing() {
    // Receivers that time out constantly must not swallow the wakeups other receivers need.
    const ITEMS: usize = 5_000;
    let ch = Arc::new(Channel::<usize, 2>::new());
    let received = Arc::new(AtomicUsize::new(0));
    let consumers: Vec<_> = (0..4)
        .map(|c| {
            let (ch, received) = (ch.clone(), received.clone());
            thread::spawn(move || {
                loop {
                    let r = if c % 2 == 0 {
                        ch.recv_timeout(Duration::from_micros(10))
                    } else {
                        ch.recv().map_err(|_| RecvTimeoutError::Closed)
                    };
                    match r {
                        Ok(_) => {
                            received.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Closed) => return,
                    }
                }
            })
        })
        .collect();
    for i in 0..ITEMS {
        ch.send(i).unwrap();
    }
    ch.close();
    for c in consumers {
        c.join().unwrap();
    }
    assert_eq!(received.load(Ordering::SeqCst), ITEMS);
}

#[test]
fn values_cross_threads_intact() {
    // Non-Copy payloads: a torn or doubled slot would show up as a bad string or a double free.
    let ch = Arc::new(Channel::<String, 4>::new());
    let producer = {
        let ch = ch.clone();
        thread::spawn(move || {
            for i in 0..2_000 {
                ch.send(i.to_string()).unwrap();
            }
            ch.close();
        })
    };
    let mut expected = 0;
    while let Ok(s) = ch.recv() {
        assert_eq!(s, expected.to_string());
        expected += 1;
    }
    assert_eq!(expected, 2_000);
    producer.join().unwrap();
}
