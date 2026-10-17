//! The process API.

use marlin_rt::process;

#[test]
fn args_match_std() {
    let ours: Vec<&str> = process::all_args().to_vec();
    let std: Vec<String> = std::env::args().collect();
    assert_eq!(ours, std);
    assert_eq!(process::args().collect::<Vec<_>>(), &ours[1..]);
    // Cached: the same slice every time.
    assert_eq!(process::all_args().as_ptr(), process::all_args().as_ptr());
}

#[test]
fn environment_and_paths() {
    assert_eq!(process::id(), std::process::id());
    assert_eq!(
        std::path::Path::new(&process::current_exe().unwrap()),
        std::env::current_exe().unwrap()
    );
    assert_eq!(
        std::path::Path::new(&process::current_dir().unwrap()),
        std::env::current_dir().unwrap()
    );
    // SAFETY: no other test in this binary touches this variable.
    unsafe { std::env::set_var("MARLIN_RT_TEST_VAR", "\u{1F41F} fish") };
    assert_eq!(
        process::env_var("MARLIN_RT_TEST_VAR").as_deref(),
        Some("\u{1F41F} fish")
    );
    assert_eq!(process::env_var("MARLIN_RT_TEST_VAR_UNSET"), None);
}
