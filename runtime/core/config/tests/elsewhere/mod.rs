//! Declarations in another module of the test crate, never listed anywhere.

use std::fmt::Write;

use marlin_config::{Command, CommandError, CvarF64, CvarString};

marlin_config::declare! {
    pub static ELSEWHERE_FLOAT: CvarF64 = CvarF64::new("decl.elsewhere.float", 0.5, "From a module.");
    pub static PING: Command = Command::new("decl.elsewhere.ping", "Answers.", ping);
    /// Same name as one in the test file, in another case.
    pub static DUPLICATE: CvarString = CvarString::new("DECL.DUP", "", "");
}

fn ping(_: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    out.write_str("pong\n")?;
    Ok(())
}
