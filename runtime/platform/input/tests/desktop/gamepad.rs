//! Gamepad slots without a gamepad.

use marlin_app::App;
use marlin_input::{Input, MAX_GAMEPADS};

#[test]
fn rumble_on_empty_slot_fails() {
    let app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    input.update();
    for pad in 0..MAX_GAMEPADS {
        if !input.state().gamepad(pad).is_connected() {
            assert!(input.set_rumble(pad, 0.0, 0.0).is_err());
        }
    }
}
