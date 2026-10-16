//! Logging and assertion macros.

/// Logs at a level chosen at run time. Prefer the fixed-level macros ([`error!`](crate::error),
/// ...), which compile out entirely above [`STATIC_MAX_LEVEL`](crate::STATIC_MAX_LEVEL).
///
/// ```ignore
/// marlin_diag::log!(level, "loaded {} assets", count);
/// ```
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)+) => {{
        let level: $crate::Level = $level;
        if $crate::enabled(level) {
            $crate::__private::log(
                level,
                ::core::module_path!(),
                ::core::file!(),
                ::core::line!(),
                ::core::format_args!($($arg)+),
            );
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_static {
    ($level:path, $($arg:tt)+) => {
        // A `const` condition guarantees no code is generated for compiled-out levels, even
        // without optimizations, while the arguments are still type-checked (and count as used).
        if const { $crate::STATIC_MAX_LEVEL.allows($level) } {
            if $crate::max_level().allows($level) {
                $crate::__private::log(
                    $level,
                    ::core::module_path!(),
                    ::core::file!(),
                    ::core::line!(),
                    ::core::format_args!($($arg)+),
                );
            }
        }
    };
}

/// Logs at [`Level::Error`](crate::Level::Error). Never compiled out.
#[macro_export]
macro_rules! error {
    ($($arg:tt)+) => { $crate::__log_static!($crate::Level::Error, $($arg)+) };
}

/// Logs at [`Level::Warn`](crate::Level::Warn). Never compiled out.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)+) => { $crate::__log_static!($crate::Level::Warn, $($arg)+) };
}

/// Logs at [`Level::Info`](crate::Level::Info). Compiled out in shipping builds.
#[macro_export]
macro_rules! info {
    ($($arg:tt)+) => { $crate::__log_static!($crate::Level::Info, $($arg)+) };
}

/// Logs at [`Level::Debug`](crate::Level::Debug). Compiled in only in debug and development
/// builds.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)+) => { $crate::__log_static!($crate::Level::Debug, $($arg)+) };
}

/// Logs at [`Level::Trace`](crate::Level::Trace). Compiled in only in debug and development
/// builds.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)+) => { $crate::__log_static!($crate::Level::Trace, $($arg)+) };
}

/// Reports an unrecoverable error and panics, in every build. Use where the code decides nothing
/// can be done; for a `Result`, [`OrFatal::or_fatal`](crate::OrFatal::or_fatal) adds the error's
/// source chain.
///
/// Logs at `Error` (ignoring the run-time filter), flushes the sinks, breaks into an attached
/// debugger, then panics, which aborts the game. The panic handler does not log it again.
///
/// ```ignore
/// marlin_diag::fatal!("device lost twice: {}", marlin_diag::chain(&e));
/// ```
#[macro_export]
macro_rules! fatal {
    ($($arg:tt)+) => {
        $crate::__private::fatal(::core::module_path!(), ::core::format_args!($($arg)+))
    };
}

/// Checks an invariant in every build except shipping, where the condition is not evaluated (but
/// still type-checked). Like `debug_assert!`, but kept in optimized development builds.
///
/// On failure: logs at `Error` (ignoring the run-time filter), flushes the sinks, breaks into an
/// attached debugger, then panics, which aborts the game.
///
/// ```ignore
/// marlin_assert!(index < len);
/// marlin_assert!(index < len, "index {index} out of {len}");
/// ```
#[macro_export]
macro_rules! marlin_assert {
    ($cond:expr $(,)?) => {
        if const { $crate::ASSERTIONS_ENABLED } {
            $crate::marlin_verify!($cond);
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if const { $crate::ASSERTIONS_ENABLED } {
            $crate::marlin_verify!($cond, $($arg)+);
        }
    };
}

/// Like [`marlin_assert!`], but evaluated and checked in every build, shipping included. Like
/// `assert!`: for conditions whose failure must never go unnoticed, or whose evaluation has side
/// effects.
#[macro_export]
macro_rules! marlin_verify {
    ($cond:expr $(,)?) => {
        if !$cond {
            $crate::__private::assert_failed(
                ::core::stringify!($cond),
                ::core::module_path!(),
                ::core::option::Option::None,
            );
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            $crate::__private::assert_failed(
                ::core::stringify!($cond),
                ::core::module_path!(),
                ::core::option::Option::Some(::core::format_args!($($arg)+)),
            );
        }
    };
}
