pub(crate) trait Time {
    /// Monotonic tick counter: never goes backwards and ignores wall-clock changes.
    fn ticks() -> u64;
    /// Ticks per second; constant for the life of the process.
    fn ticks_per_second() -> u64;
    /// Wall-clock time: seconds and nanoseconds since 1970-01-01 00:00:00 UTC.
    fn system_time() -> (i64, u32);
    /// Offset of local time from UTC at `unix_seconds`, in seconds east of UTC (DST included).
    fn local_offset(unix_seconds: i64) -> Option<i32>;
}
