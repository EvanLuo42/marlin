//! The machine running the tests was built for, so the check must pass, and must agree with std's
//! own runtime detection.

#[test]
fn this_cpu_has_everything_the_build_requires() {
    if let Err(missing) = marlin_rt::cpu::check() {
        panic!("{missing}");
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn agrees_with_std_detection() {
    // Whatever the build enables, std must detect it at runtime too.
    if cfg!(target_feature = "avx2") {
        assert!(std::arch::is_x86_feature_detected!("avx2"));
        assert!(std::arch::is_x86_feature_detected!("fma"));
        assert!(std::arch::is_x86_feature_detected!("bmi2"));
    }
}
