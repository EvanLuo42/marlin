//! Backtrace capture, display and legacy Rust demangling.

use marlin_diag::Backtrace;
use marlin_diag::backtrace::Demangle;

#[inline(never)]
fn marker_function_for_backtrace() -> Backtrace {
    std::hint::black_box(Backtrace::capture())
}

#[test]
fn captures_from_the_caller() {
    let trace = marker_function_for_backtrace();
    assert!(trace.frames().len() >= 3);
    assert!(!trace.is_empty());
    assert!(Backtrace::empty().is_empty());
}

#[test]
fn display_lists_frames() {
    let trace = marker_function_for_backtrace();
    let text = trace.to_string();
    // dbghelp resolves names and lines from the test's PDB; POSIX shows module offsets.
    #[cfg(windows)]
    {
        let first = text.lines().next().unwrap();
        assert!(first.contains("marker_function_for_backtrace"), "{text}");
        assert!(text.contains("backtrace.rs:"), "{text}");
    }
    #[cfg(not(windows))]
    assert!(text.contains(" +0x"), "{text}");
}

#[test]
fn demangles_legacy_symbols() {
    let cases = [
        ("_ZN4core3fmt5write17h0123456789abcdefE", "core::fmt::write"),
        (
            "_ZN60_$LT$alloc..vec..Vec$LT$T$GT$$u20$as$u20$core..ops..Drop$GT$4drop17h0123456789abcdefE",
            "<alloc::vec::Vec<T> as core::ops::Drop>::drop",
        ),
        ("_ZN3foo3barE", "foo::bar"),
        ("__ZN3foo3barE", "foo::bar"),
        (
            "_ZN17closure$u7b$$u7d$4call17h0000000000000000E",
            "closure{}::call",
        ),
        // Not legacy-mangled: shown as is.
        ("main", "main"),
        ("_RNvCs1234_7mycrate3foo", "_RNvCs1234_7mycrate3foo"),
        ("_ZN3fooE_junk", "_ZN3fooE_junk"),
        ("_ZN99tooshortE", "_ZN99tooshortE"),
    ];
    for (mangled, expected) in cases {
        assert_eq!(Demangle(mangled).to_string(), expected, "{mangled}");
    }
}
