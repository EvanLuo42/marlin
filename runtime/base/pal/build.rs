//! Exposes the `marlin_platform` cfg (from platforms/<name>/platform.toml, see `cargo xtask
//! gen-cfg`) as the `MARLIN_PLATFORM` environment variable behind [`PLATFORM`](crate::PLATFORM).

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_MARLIN_PLATFORM");
    let platform = std::env::var("CARGO_CFG_MARLIN_PLATFORM").unwrap_or_else(|_| {
        panic!(
            "no `marlin_platform` cfg for this target: build through .cargo/config.toml or \
             `cargo xtask` (a RUSTFLAGS variable replaces the configured flags)"
        )
    });
    println!("cargo:rustc-env=MARLIN_PLATFORM={platform}");
}
