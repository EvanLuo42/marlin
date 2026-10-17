//! Stack capture, module lookup and symbols.

use marlin_pal::debug;

#[inline(never)]
fn capture_here(skip: usize) -> Vec<usize> {
    let mut frames = [0usize; 64];
    let n = debug::capture_stack(skip, &mut frames);
    frames[..n].to_vec()
}

#[inline(never)]
pub fn outer_marker_function() -> Vec<usize> {
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

#[test]
fn unknown_address_has_no_module() {
    let mut path = [0u8; 64];
    assert!(debug::module_of(16, &mut path).is_none());
}
