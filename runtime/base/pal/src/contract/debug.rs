pub(crate) trait Debug {
    /// Writes `s` to stderr and, when a debugger is attached, to the debugger.
    ///
    /// Reports panics, so it must not allocate or take locks a panicking thread could hold.
    fn output(s: &str);
    fn is_debugger_present() -> bool;
    /// Raises a breakpoint trap. Without a debugger this usually terminates the process.
    fn break_point();
}
