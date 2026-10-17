mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use common::{TempFile, pattern};
use marlin_io::{Engine, ErrorKind, File, IoBuffer, IoQueue, OpKind, RequestId};

/// Both engines this platform has: the best one and the fallback.
fn queues<'f>(capacity: u32) -> Vec<IoQueue<'f>> {
    let best = IoQueue::new(capacity).unwrap();
    let fallback = IoQueue::with_fallback(capacity).unwrap();
    let expected: &[Engine] = if cfg!(windows) {
        &[Engine::IoRing, Engine::Iocp]
    } else if cfg!(target_os = "linux") {
        &[Engine::IoUring, Engine::Blocking]
    } else {
        &[Engine::Blocking]
    };
    // IoRing needs Windows 11 22H2 and io_uring may be disabled; only the fallback is certain.
    assert!(expected.contains(&best.engine()), "{:?}", best.engine());
    assert_eq!(fallback.engine(), *expected.last().unwrap());
    println!("engines: {:?} / {:?}", best.engine(), fallback.engine());
    vec![best, fallback]
}

/// Reads every `chunk`-sized piece of `file` with up to `queue.capacity()` reads in flight,
/// keeping the queue full, and checks each against `data`.
fn read_all_chunks<'f>(queue: &mut IoQueue<'f>, file: &'f File, data: &[u8], chunk: usize) {
    let chunks = data.len().div_ceil(chunk);
    let mut pending: HashMap<RequestId, usize> = HashMap::new();
    let mut next = 0;
    let mut done = 0;
    let mut max_in_flight = 0;
    while done < chunks {
        while next < chunks {
            let buffer = IoBuffer::for_file(file, chunk);
            match queue.read(file, buffer, (next * chunk) as u64) {
                Ok(id) => {
                    pending.insert(id, next);
                    next += 1;
                }
                Err(e) if e.error.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("submit failed: {e}"),
            }
        }
        max_in_flight = max_in_flight.max(queue.in_flight());
        let completed = queue.wait().unwrap().expect("operations in flight");
        assert_eq!(completed.kind, OpKind::Read);
        let index = pending.remove(&completed.id).expect("unknown request id");
        let start = index * chunk;
        let expected = &data[start..data.len().min(start + chunk)];
        let n = completed.result.unwrap();
        assert_eq!(n, expected.len(), "chunk {index}");
        assert!(completed.buffer[..n] == *expected, "chunk {index} differs");
        done += 1;
    }
    assert!(pending.is_empty());
    assert_eq!(queue.in_flight(), 0);
    assert_eq!(max_in_flight, queue.capacity().min(chunks as u32));
}

#[test]
fn many_concurrent_reads() {
    let data = pattern(4 << 20);
    let tmp = TempFile::with_data("queue-reads", &data);
    let file = File::open(tmp.path()).unwrap();
    for mut queue in queues(64) {
        read_all_chunks(&mut queue, &file, &data, 16 * 1024);
        // Odd sizes and a short last chunk.
        read_all_chunks(&mut queue, &file, &data, 12_345);
    }
}

/// Unbuffered reads need sector-aligned buffers, offsets and lengths, which `IoBuffer::for_file`
/// provides. The last chunk reads past the end and comes back short.
#[cfg(windows)]
#[test]
fn unbuffered_aligned_reads() {
    let data = pattern((2 << 20) + 1000);
    let tmp = TempFile::with_data("queue-unbuffered", &data);
    let file =
        File::with_options(tmp.path(), &marlin_io::OpenOptions::read().unbuffered(true)).unwrap();
    let align = file.alignment();
    assert!(align >= 512 && align.is_power_of_two(), "{align}");

    for mut queue in queues(32) {
        read_all_chunks(&mut queue, &file, &data, 64 * 1024);

        // Misaligned requests are refused up front, handing the buffer back.
        let err = queue.read(&file, IoBuffer::new(100), 0).unwrap_err();
        assert_eq!(err.error.kind(), ErrorKind::InvalidInput);
        assert_eq!(err.buffer.len(), 100);
        let err = queue
            .read(&file, IoBuffer::for_file(&file, align), 1)
            .unwrap_err();
        assert_eq!(err.error.kind(), ErrorKind::InvalidInput);
    }

    // The sync path has the same rule.
    let mut buf = IoBuffer::for_file(&file, align);
    assert_eq!(file.read_at(&mut buf, 0).unwrap(), align);
    assert_eq!(&buf[..], &data[..align]);
    assert_eq!(
        file.read_at(&mut buf[..align / 2], 0).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn writes_then_reads() {
    let data = pattern(1 << 20);
    let chunk = 8 * 1024;
    let tmps: Vec<_> = (0..2)
        .map(|i| TempFile::new(&format!("queue-writes-{i}")))
        .collect();
    let files: Vec<_> = tmps
        .iter()
        .map(|t| File::create(t.path()).unwrap())
        .collect();
    for ((mut queue, tmp), file) in queues(16).into_iter().zip(&tmps).zip(&files) {
        let mut writes = 0;
        for (index, piece) in data.chunks(chunk).enumerate() {
            let mut buffer = IoBuffer::new(piece.len());
            buffer.copy_from_slice(piece);
            loop {
                match queue.write(file, buffer, (index * chunk) as u64) {
                    Ok(_) => break,
                    Err(e) if e.error.kind() == ErrorKind::WouldBlock => {
                        buffer = e.buffer;
                        let done = queue.wait().unwrap().unwrap();
                        assert_eq!(done.kind, OpKind::Write);
                        assert_eq!(done.result.unwrap(), done.buffer.len());
                        writes += 1;
                    }
                    Err(e) => panic!("submit failed: {e}"),
                }
            }
        }
        while let Some(done) = queue.wait().unwrap() {
            assert_eq!(done.result.unwrap(), done.buffer.len());
            writes += 1;
        }
        assert_eq!(writes, data.len().div_ceil(chunk));
        assert_eq!(std::fs::read(&tmp.0).unwrap(), data);

        // Read it back through the same queue engine.
        read_all_chunks(&mut queue, file, &data, chunk);
    }
}

#[test]
fn reads_past_end_and_errors_complete() {
    let data = pattern(1000);
    let tmp = TempFile::with_data("queue-eof", &data);
    let file = File::open(tmp.path()).unwrap();
    for mut queue in queues(4) {
        let short = queue.read(&file, IoBuffer::new(4096), 900).unwrap();
        let past = queue.read(&file, IoBuffer::new(16), 5000).unwrap();
        // The file is read-only, so the write fails, but through its completion.
        let denied = queue.write(&file, IoBuffer::new(16), 0).unwrap();

        let mut results = HashMap::new();
        while let Some(done) = queue.wait().unwrap() {
            results.insert(done.id, (done.result, done.buffer));
        }
        let (n, buffer) = &results[&short];
        assert_eq!(*n.as_ref().unwrap(), 100);
        assert_eq!(&buffer[..100], &data[900..]);
        assert_eq!(*results[&past].0.as_ref().unwrap(), 0);
        assert!(results[&denied].0.is_err(), "{:?}", results[&denied].0);
    }
}

#[test]
fn idle_and_timeouts() {
    let data = pattern(64);
    let tmp = TempFile::with_data("queue-idle", &data);
    let file = File::open(tmp.path()).unwrap();
    for mut queue in queues(2) {
        // Nothing in flight: no waiting, even without a timeout.
        assert!(queue.poll().unwrap().is_none());
        assert!(queue.wait().unwrap().is_none());
        let start = Instant::now();
        assert!(
            queue
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() < Duration::from_secs(1));

        // Capacity is enforced and the buffer comes back.
        queue.read(&file, IoBuffer::new(8), 0).unwrap();
        queue.read(&file, IoBuffer::new(8), 8).unwrap();
        let err = queue.read(&file, IoBuffer::new(3), 0).unwrap_err();
        assert_eq!(err.error.kind(), ErrorKind::WouldBlock);
        assert_eq!(err.buffer.len(), 3);
        queue.submit().unwrap();

        let mut got = 0;
        while got < 2 {
            if let Some(done) = queue.wait_timeout(Duration::from_secs(10)).unwrap() {
                assert_eq!(done.result.unwrap(), 8);
                got += 1;
            }
        }
        assert_eq!(queue.in_flight(), 0);
    }
}

#[test]
fn drop_waits_for_in_flight_operations() {
    let data = pattern(1 << 20);
    let tmp = TempFile::with_data("queue-drop", &data);
    let file = File::open(tmp.path()).unwrap();
    for mut queue in queues(32) {
        for i in 0..32 {
            queue
                .read(&file, IoBuffer::new(32 * 1024), i * 32 * 1024)
                .unwrap();
        }
        queue.submit().unwrap();
        drop(queue);
    }
}

#[test]
fn queue_moves_to_another_thread() {
    let data = pattern(256 * 1024);
    let tmp = TempFile::with_data("queue-thread", &data);
    let file = File::open(tmp.path()).unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut queue = IoQueue::new(8).unwrap();
            read_all_chunks(&mut queue, &file, &data, 4096);
        });
    });
}

#[test]
fn file_bound_to_one_iocp_queue() {
    let tmp = TempFile::with_data("queue-bind", b"0123456789");
    let file = File::open(tmp.path()).unwrap();
    let mut first = IoQueue::with_fallback(2).unwrap();
    let mut second = IoQueue::with_fallback(2).unwrap();
    first.read(&file, IoBuffer::new(4), 0).unwrap();
    assert_eq!(first.wait().unwrap().unwrap().result.unwrap(), 4);
    let result = second.read(&file, IoBuffer::new(4), 0);
    if first.engine() == Engine::Iocp {
        // A handle posts to one completion port for life.
        assert_eq!(result.unwrap_err().error.kind(), ErrorKind::InvalidInput);
    } else {
        result.unwrap();
        assert_eq!(second.wait().unwrap().unwrap().result.unwrap(), 4);
    }
    // The sync path still works on a handle bound to a port.
    let mut buf = [0; 10];
    assert_eq!(file.read_at(&mut buf, 0).unwrap(), 10);
    assert_eq!(&buf, b"0123456789");
}
