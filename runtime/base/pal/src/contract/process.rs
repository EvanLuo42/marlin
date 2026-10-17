use crate::Result;

/// Strings are written as UTF-8 to `out`; each call returns the full length, and `out` holds the
/// whole string only when that length is at most `out.len()` (the caller checks).
pub(crate) trait Process {
    /// Terminates the process abnormally, without running destructors or exit handlers.
    fn abort() -> !;
    /// Terminates the process normally with `code`.
    fn exit(code: i32) -> !;
    fn id() -> u32;

    /// Number of command-line arguments, the program name included.
    fn arg_count() -> usize;
    /// Argument `index` (0 = the program name), or `None` past the end. `InvalidData` if it is not
    /// valid Unicode.
    fn arg(index: usize, out: &mut [u8]) -> Result<Option<usize>>;

    fn current_exe(out: &mut [u8]) -> Result<usize>;
    fn current_dir(out: &mut [u8]) -> Result<usize>;
    /// `path` is non-empty, at most `MAX_PATH_LEN` bytes and has no NUL (checked by the caller).
    fn set_current_dir(path: &str) -> Result<()>;
    /// `None` if the variable is not set. `name` is non-empty, has no NUL or `=` and is at most
    /// `MAX_ENV_NAME_LEN` bytes (checked by the caller).
    fn env_var(name: &str, out: &mut [u8]) -> Result<Option<usize>>;
}
