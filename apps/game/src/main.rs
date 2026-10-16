//! The game executable shared by every Marlin game.
//!
//! `marlin-game [<boot.toml>] [+cvar value]...`: reads the boot configuration (default
//! `boot.toml` in the working directory), mounts its content (a package, or a cook service with
//! `+content.cookd host:port` for live editing), loads the entry level and runs it in a window.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use marlin_diag::{chain, error, info};

// Link the whole runtime.
use marlin as _;

marlin_rt::game_entry!(game_main);

/// The boot file: the first argument that is neither a `+cvar` nor its value.
fn boot_path(args: &[&str]) -> String {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if let Some(spec) = a.strip_prefix('+') {
            i += if spec.contains('=') { 1 } else { 2 };
            continue;
        }
        return String::from(a);
    }
    String::from("boot.toml")
}

fn game_main() -> i32 {
    let args: Vec<&str> = marlin_rt::args().collect();
    let path = boot_path(&args);
    let text = match marlin_io::read(path.as_str()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                error!("{path}: not UTF-8");
                return 1;
            }
        },
        Err(e) => {
            error!("{path}: {e}");
            return 1;
        }
    };
    let base = path.rsplit_once(['/', '\\']).map_or("", |(dir, _)| dir);
    let boot = match marlin_framework::boot(&text, base, args.iter().copied()) {
        Ok(boot) => boot,
        Err(e) => {
            error!("boot: {}", chain(&e));
            return 1;
        }
    };
    info!("content: {:?}", boot.vfs.layer_names().collect::<Vec<_>>());
    match marlin_framework::run(&boot, "Marlin") {
        Ok(()) => 0,
        Err(e) => {
            error!("{}", chain(&e));
            1
        }
    }
}
