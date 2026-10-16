pub(crate) trait Time {
    /// Monotonic tick counter: never goes backwards and ignores wall-clock changes.
    fn ticks() -> u64;
    /// Ticks per second; constant for the life of the process.
    fn ticks_per_second() -> u64;
}
