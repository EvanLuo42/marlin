//! Build orchestration for Marlin: `cargo xtask <command>`.

mod bench;
mod build;
mod cfg;
mod check;
mod coverage;
mod fuzz;
mod platform;
mod process;
mod test;
mod unicode;

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
  test            Run the tests by tier (docs/design/0014-testing.md §4)
                    --tier <tier>       quick | gpu | desktop | stress | manual | bench | fuzz | all
                                        (default: quick)
                    -p <crate>          only this package (repeatable)
                    --seed <n>          MARLIN_TEST_SEED for randomized tests
                    --bless             MARLIN_BLESS=1: rewrite golden files
                    --stall <secs>      watchdog: no test finishing for this long is a hang
                                        (bench: a bench binary running this long; default 1800)
                    -- <args>...        passed to every test binary (name filters, ...)
                  bench tier (0014 §7.2): benches/ in release, results in target/bench/
                    --save-baseline <tag>  store the results as target/bench/baselines/<tag>/
                    --baseline <tag>    compare with a saved baseline, flag slower medians
                    --threshold <pct>   how much slower gets flagged (default: 5)
                    -- [filter]... [--quick]  bench name filters; --quick: fewer, shorter samples
                  --tier fuzz: the libFuzzer targets in fuzz/ (0014 §7.3), built with ASan; each run
                  capped at 2048 MB; a find is minimized and copied to <crate>/tests/data/regress/<target>/
                    --target <name>     only this target (repeatable; -p <crate> also filters)
                    --time <secs>       per target (default: 60)
                    --seed <n>          libFuzzer -seed
                    -- <args>...        more libFuzzer flags
  coverage        Source coverage of src/ from the test tiers (0014 §7.4)
                    --tier <tier>       quick | gpu | desktop | all (default: all = the three)
                    -p, --seed, --stall, --  as for test
                    --html              also write target/coverage/html/
                    --save-baseline     store the numbers to compare later runs against
  gen-unicode     Regenerate marlin-text's Unicode tables from the Unicode Character Database
                    --ucd <dir>         the UCD files (default: $MARLIN_UCD_DIR)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = root();
    let result = match args.first().map(String::as_str) {
        Some("platforms") => platform::list(&root),
        Some("gen-cfg") => cfg::gen_cfg(&root),
        Some("build") => build::run(&root, &args[1..]),
        Some("check-layers") => check::layers(&root),
        Some("test") => test::run(&root, &args[1..]),
        Some("coverage") => coverage::run(&root, &args[1..]),
        Some("gen-unicode") => unicode::gen_unicode(&root, &args[1..]),
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

/// Cargo's target directory: `CARGO_TARGET_DIR` (relative to the workspace root) or
/// `<root>/target`, the rule marlin-bench and the golden-image helpers follow too.
fn target_dir(root: &Path) -> PathBuf {
    root.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
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
