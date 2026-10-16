//! The game executable shared by every Marlin game: reads the boot configuration from its package
//! and runs the engine.
#![no_std]
#![no_main]

// Link the whole runtime.
use marlin as _;

marlin_rt::game_entry!(game_main);

fn game_main() -> i32 {
    // Read boot.toml from the mounted package and hand it to the framework.
    0
}
