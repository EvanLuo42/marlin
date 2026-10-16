pub(crate) trait Process {
    /// Terminates the process abnormally, without running destructors or exit handlers.
    fn abort() -> !;
}
