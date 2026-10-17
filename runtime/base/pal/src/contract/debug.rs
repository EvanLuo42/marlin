use crate::debug::{Module, Symbol};

pub(crate) trait Debug {
    /// Writes `s` to stderr and, when a debugger is attached, to the debugger.
    ///
    /// Reports panics, so it must not allocate or take locks a panicking thread could hold.
    fn output(s: &str);
    fn is_debugger_present() -> bool;
    /// Raises a breakpoint trap. Without a debugger this usually terminates the process.
    fn break_point();

    /// Writes the return addresses of the calling thread's stack, innermost first, skipping the
    /// innermost `skip` frames of the caller; returns how many were written.
    fn capture_stack(skip: usize, out: &mut [usize]) -> usize;
    /// The module containing `addr`, its path written like `crate::process` strings.
    fn module_of(addr: usize, path: &mut [u8]) -> Option<Module>;
    /// The function containing `addr` and, where known, its source line. Names and file paths
    /// are written like `crate::process` strings.
    fn symbolize(addr: usize, name: &mut [u8], file: &mut [u8]) -> Option<Symbol>;
}
