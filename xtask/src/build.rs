//! `cargo xtask build`.

use std::path::Path;

use crate::cfg::{self, Config};
use crate::platform::{self, Family};
use crate::{Result, cargo};

pub fn run(root: &Path, args: &[String]) -> Result {
    let mut platform = None;
    let mut config = Config::Debug;
    let mut app = "game".to_owned();
    let mut target = None;
    let mut passthrough = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "--platform" => platform = Some(value()?),
            "--config" => config = Config::parse(&value()?)?,
            "--app" => app = value()?,
            "--target" => target = Some(value()?),
            "--" => {
                passthrough.extend(it.by_ref().cloned());
                break;
            }
            _ => return Err(format!("unexpected argument `{arg}`").into()),
        }
    }

    let platform = platform::find(root, platform.as_deref().unwrap_or(platform::host()))?;
    let package = match app.as_str() {
        "game" => "marlin-game",
        "editor" if platform.family == Family::Desktop => "marlin-editor",
        "editor" => return Err("the editor only runs on desktop platforms".into()),
        _ => return Err(format!("unknown app `{app}` (expected game or editor)").into()),
    };

    let mut command = cargo();
    command
        .current_dir(root)
        .args(["build", "--package", package, "--profile", config.cargo_profile()])
        .arg("--target-dir")
        .arg(root.join("target").join(&platform.name));
    match (target, &platform.target_spec) {
        (Some(t), _) if !platform.targets.contains(&t) => {
            return Err(format!("`{t}` is not a target of {}", platform.name).into());
        }
        (Some(t), _) => command.args(["--target", &t]),
        (None, Some(spec)) => command.arg("--target").arg(platform.dir.join(spec)),
        (None, None) => command.args(["--target", &platform.targets[0]]),
    };
    if platform.build_std {
        command.arg("-Zbuild-std=core,alloc");
    }
    command.args(&passthrough);

    // Replaces the .cargo/config.toml flags; with an explicit --target they only reach target
    // crates, not build scripts or proc macros.
    command
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", cfg::flags(&platform, Some(config)).join("\x1f"));
    crate::run(command)
}
