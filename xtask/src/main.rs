//! Build orchestration for Marlin: `cargo xtask <command>`.

mod build;
mod cfg;
mod check;
mod platform;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T = (), E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

const HELP: &str = "\
Usage: cargo xtask <command> [options]

Commands:
  platforms       List the platforms found under platforms/
  gen-cfg         Regenerate the desktop --cfg block of .cargo/config.toml from platforms/*/platform.toml
  build           Build an app for a platform
                    --platform <name>   default: the host platform
                    --config <config>   debug | development | profile | shipping (default: debug)
                    --app <app>         game | editor (default: game)
                    --target <triple>   default: the platform's first target
                    -- <args>...        passed through to cargo build
  check-layers    Check crate layering and that runtime crates are no_std
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = root();
    let result = match args.first().map(String::as_str) {
        Some("platforms") => platform::list(&root),
        Some("gen-cfg") => cfg::gen_cfg(&root),
        Some("build") => build::run(&root, &args[1..]),
        Some("check-layers") => check::layers(&root),
        Some("help" | "-h" | "--help") | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(command) => Err(format!("unknown command `{command}`\n\n{HELP}").into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root; xtask lives one level below it.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The cargo that invoked us, so the pinned toolchain is used.
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn run(mut command: Command) -> Result {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed: {status}").into())
    }
}
