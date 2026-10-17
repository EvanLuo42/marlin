//! Stack capture, module lookup and symbols.

use marlin_pal::debug;

#[inline(never)]
fn capture_here(skip: usize) -> Vec<usize> {
    let mut frames = [0usize; 64];
    let n = debug::capture_stack(skip, &mut frames);
    frames[..n].to_vec()
}

#[inline(never)]
fn outer_marker_function() -> Vec<usize> {
    let frames = capture_here(0);
    std::hint::black_box(frames)
}

#[test]
fn captures_frames_inside_this_module() {
    let frames = outer_marker_function();
    assert!(frames.len() >= 3, "{frames:?}");

    let mut path = [0u8; 1024];
    let module = debug::module_of(frames[0], &mut path).unwrap();
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        std::path::Path::new(std::str::from_utf8(&path[..module.path_len]).unwrap())
            .canonicalize()
            .unwrap(),
        exe.canonicalize().unwrap()
    );
    assert!(module.base <= frames[0]);
}

#[test]
fn skip_drops_inner_frames() {
    let all = outer_marker_function();
    let skipped = {
        #[inline(never)]
        fn outer() -> Vec<usize> {
            capture_here(1)
        }
        outer()
    };
    // With one frame skipped, the first frame is no longer inside `capture_here`.
    assert!(!skipped.is_empty());
    assert_ne!(skipped[0], all[0]);
}

#[test]
fn output_buffer_is_reported_in_full() {
    let mut frames = [0usize; 2];
    assert!(debug::capture_stack(0, &mut frames) <= 2);
    assert_eq!(debug::capture_stack(0, &mut []), 0);
}

/// dbghelp reads the test binary's PDB; POSIX `dladdr` only sees exported symbols, so the names
/// there are not checked.
#[cfg(windows)]
#[test]
fn symbolizes_with_names_and_lines() {
    let frames = outer_marker_function();
    let mut name = [0u8; 512];
    let mut file = [0u8; 512];
    let found = frames.iter().any(|&addr| {
        let Some(symbol) = debug::symbolize(addr, &mut name, &mut file) else {
            return false;
        };
        let name = std::str::from_utf8(&name[..symbol.name_len]).unwrap();
        let file = std::str::from_utf8(&file[..symbol.file_len]).unwrap();
        name.contains("outer_marker_function") && file.ends_with("stack.rs") && symbol.line > 0
    });
    assert!(found);

    // Concurrent use is serialized internally.
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let mut name = [0u8; 256];
                let mut file = [0u8; 256];
                for &addr in &frames {
                    let _ = debug::symbolize(addr, &mut name, &mut file);
                }
            });
        }
    });
}

#[test]
fn unknown_address_has_no_module() {
    let mut path = [0u8; 64];
    assert!(debug::module_of(16, &mut path).is_none());
}
