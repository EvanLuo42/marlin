//! An app, its message pump and Input, without windows: updates, lifetimes, hook slots and key
//! labels.

use marlin_app::App;
use marlin_input::{Input, KEY_LABEL_CAPACITY, Key};

#[test]
fn update_without_windows() {
    let mut app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    for _ in 0..3 {
        app.pump();
        input.update();
        let state = input.state();
        assert_eq!(state.keys_down().count(), 0);
        assert_eq!(state.text(), "");
        assert_eq!(state.mouse().delta(), [0.0, 0.0]);
    }
}

#[test]
fn input_outlives_app() {
    let app = App::new().unwrap();
    let mut input = Input::new(&app).unwrap();
    drop(app);
    input.update();
}

#[test]
fn several_inputs_per_app() {
    let app = App::new().unwrap();
    let inputs: Vec<_> = (0..3).map(|_| Input::new(&app).unwrap()).collect();
    drop(inputs);
    // Dropped inputs free their hook slots.
    let _more: Vec<_> = (0..8).map(|_| Input::new(&app).unwrap()).collect();
}

#[test]
fn key_labels_follow_the_layout() {
    let app = App::new().unwrap();
    let input = Input::new(&app).unwrap();
    let mut buf = [0; KEY_LABEL_CAPACITY];
    // The text depends on the layout and display language, but every letter key has one.
    let label = input.key_label(Key::A, &mut buf).unwrap();
    assert!(!label.is_empty());
    assert!(input.key_label(Key::from_hid(0), &mut buf).is_none());
}
