//! Queue cancellation: an operation that would never finish completes with `Cancelled`.

use std::ptr::NonNull;
use std::time::Duration;

use marlin_pal::ErrorKind;
use marlin_pal::file::{CANCEL_TAG, Completion, File, OpenOptions, Queue, Request};
use marlin_test_support::TempFile;

/// Pushes a read that blocks forever, cancels it, and expects exactly one `Cancelled` completion.
#[cfg(any(marlin_platform = "windows", marlin_platform = "linux"))]
pub fn cancel_pending_read(queue: &mut Queue, file: &File) {
    let mut request = Box::new(Request::new());
    let mut buf = vec![0u8; 64];
    let request_ptr = NonNull::from(&mut *request);
    // SAFETY: the request, buffer and file outlive the operation, which is polled below.
    unsafe { queue.read(request_ptr, file, buf.as_mut_ptr(), buf.len(), 0, 7) }.unwrap();
    queue.submit().unwrap();

    let mut out = [Completion::EMPTY; 4];
    assert_eq!(
        queue
            .poll(&mut out, Some(Duration::from_millis(50)))
            .unwrap(),
        0
    );

    // SAFETY: the read is in flight with tag 7.
    unsafe { queue.cancel(request_ptr, 7) }.unwrap();
    let n = queue.poll(&mut out, Some(Duration::from_secs(5))).unwrap();
    assert_eq!(n, 1);
    assert_eq!(out[0].tag, 7);
    assert_eq!(out[0].result.unwrap_err().kind(), ErrorKind::Cancelled);
    assert_eq!(queue.in_flight(), 0);
}

#[test]
fn cancel_tag_is_reserved() {
    let path = TempFile::new("pal-cancel-tag");
    std::fs::write(&path, b"x").unwrap();
    let file = File::open(path.to_str().unwrap(), &OpenOptions::read()).unwrap();
    let mut queue = Queue::new(4).unwrap();
    let mut request = Box::new(Request::new());
    let mut buf = [0u8; 1];
    // SAFETY: nothing is queued, since the call fails.
    let error = unsafe {
        queue.read(
            NonNull::from(&mut *request),
            &file,
            buf.as_mut_ptr(),
            1,
            0,
            CANCEL_TAG,
        )
    }
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    drop(file);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn cancelling_finished_operation_keeps_its_result() {
    let path = TempFile::new("pal-cancel-done");
    std::fs::write(&path, b"hello").unwrap();
    let file = File::open(path.to_str().unwrap(), &OpenOptions::read()).unwrap();
    for fallback in [false, true] {
        let mut queue = if fallback {
            Queue::new_fallback(4).unwrap()
        } else {
            Queue::new(4).unwrap()
        };
        let mut request = Box::new(Request::new());
        let request_ptr = NonNull::from(&mut *request);
        let mut buf = [0u8; 5];
        // SAFETY: the request, buffer and file outlive the operation, which is polled below.
        unsafe { queue.read(request_ptr, &file, buf.as_mut_ptr(), 5, 0, 1) }.unwrap();
        // Run it to completion inside the OS before cancelling; the completion stays queued.
        queue.submit().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // SAFETY: the read was pushed with tag 1 and not polled.
        unsafe { queue.cancel(request_ptr, 1) }.unwrap();
        let mut out = [Completion::EMPTY; 4];
        let n = queue.poll(&mut out, Some(Duration::from_secs(5))).unwrap();
        assert_eq!(n, 1);
        // Either it finished first or the cancel caught it; both are exactly one completion.
        match out[0].result {
            Ok(5) => assert_eq!(&buf, b"hello"),
            Err(e) => assert_eq!(e.kind(), ErrorKind::Cancelled),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            queue
                .poll(&mut out, Some(Duration::from_millis(20)))
                .unwrap(),
            0
        );
    }
    drop(file);
    std::fs::remove_file(&path).unwrap();
}

/// The blocking engine runs operations at submit; one cancelled before that never runs.
#[cfg(not(marlin_platform = "windows"))]
#[test]
fn cancel_before_submit_blocking() {
    let path = TempFile::new("pal-cancel-blocking");
    std::fs::write(&path, b"data").unwrap();
    let file = File::open(path.to_str().unwrap(), &OpenOptions::read()).unwrap();
    let mut queue = Queue::new_fallback(4).unwrap();
    let mut requests = [Request::new(), Request::new()];
    let mut bufs = [[0u8; 4]; 2];
    for (i, (request, buf)) in requests.iter_mut().zip(&mut bufs).enumerate() {
        // SAFETY: requests, buffers and file outlive the operations, which are polled below.
        unsafe {
            queue.read(
                NonNull::from(request),
                &file,
                buf.as_mut_ptr(),
                4,
                0,
                i as u64,
            )
        }
        .unwrap();
    }
    // SAFETY: request 0 is pending with tag 0.
    unsafe { queue.cancel(NonNull::from(&mut requests[0]), 0) }.unwrap();
    let mut out = [Completion::EMPTY; 4];
    let n = queue.poll(&mut out, None).unwrap();
    assert_eq!(n, 2);
    let cancelled = out[..2].iter().find(|c| c.tag == 0).unwrap();
    assert_eq!(cancelled.result.unwrap_err().kind(), ErrorKind::Cancelled);
    let read = out[..2].iter().find(|c| c.tag == 1).unwrap();
    assert_eq!(read.result, Ok(4));
    assert_eq!(bufs[0], [0; 4]);
    drop(file);
    std::fs::remove_file(&path).unwrap();
}
