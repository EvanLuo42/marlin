//! The game executable shared by every Marlin game.
//!
//! `<Name> [+cvar value]...`: runs the game staged around it (0003 §4.6): `marlin-cook stage`
//! copies this executable, renamed after the project, next to `content/paks/` with the game's
//! packages. It mounts them, applies the config cooked into them, loads `game.default_level`
//! and runs it in a window titled with the executable's name. It takes no content, project or
//! boot file arguments: to run a game, cook, package and stage it.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use marlin_diag::{chain, error, info};

marlin_rt::game_entry!(game_main);

fn game_main() -> i32 {
    let args: Vec<&str> = marlin_rt::args().collect();
    let exe = match marlin_rt::process::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            error!("finding the executable: {e}");
            return 1;
        }
    };
    let (dir, file) = exe.rsplit_once(['/', '\\']).unwrap_or(("", exe.as_str()));
    let title = String::from(file.rsplit_once('.').map_or(file, |(stem, _)| stem));
    // The modules register first: their cvars exist when the config sets them.
    let modules = match marlin::world::Modules::new(marlin::game_modules()) {
        Ok(modules) => modules,
        Err(e) => {
            error!("{e}");
            return 1;
        }
    };
    let boot = match marlin_framework::boot(dir, args.iter().copied()) {
        Ok(boot) => boot,
        Err(e) => {
            error!("boot: {}", chain(&e));
            return 1;
        }
    };
    info!("content: {:?}", boot.vfs.layer_names().collect::<Vec<_>>());
    match marlin_framework::run(&boot, &title, modules) {
        Ok(()) => 0,
        Err(e) => {
            error!("{}", chain(&e));
            1
        }
    }
}
