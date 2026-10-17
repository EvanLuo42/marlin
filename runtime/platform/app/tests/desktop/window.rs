//! Window placement: position, size, limits and displays.

use marlin_app::{App, Event, Point, Size, WindowDesc, WindowMode};
use marlin_test_support::one_at_a_time;

use crate::hidden;

#[test]
fn position_size_and_limits() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app.create_window(&hidden("placement")).unwrap();
    let id = window.id();
    app.pump();

    let work = window.display().unwrap().work_area;
    let target = Point::new(work.x + 40, work.y + 50);
    window.set_position(target);
    assert_eq!(window.position(), target);
    let events = app.pump();
    assert!(events.contains(&Event::Moved {
        window: id,
        position: target,
    }));

    window.set_size(Size::new(400, 300));
    assert_eq!(window.size(), Size::new(400, 300));
    assert!(app.pump().contains(&Event::Resized {
        window: id,
        size: Size::new(400, 300),
    }));
    // Resizing keeps the client origin.
    assert_eq!(window.position(), target);

    // Limits clamp both the current size and later requests.
    window.set_min_size(Some(Size::new(500, 350)));
    assert_eq!(window.size(), Size::new(500, 350));
    window.set_max_size(Some(Size::new(600, 400)));
    window.set_size(Size::new(2000, 2000));
    assert_eq!(window.size(), Size::new(600, 400));
    window.set_size(Size::new(10, 10));
    assert_eq!(window.size(), Size::new(500, 350));
    window.set_min_size(None);
    window.set_max_size(None);
    window.set_size(Size::new(200, 150));
    assert_eq!(window.size(), Size::new(200, 150));
}

#[test]
fn windows_open_on_the_chosen_display() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let displays = app.displays();
    for display in &displays {
        let window = app
            .create_window(&WindowDesc {
                display: Some(display.id),
                ..hidden("display")
            })
            .unwrap();
        assert_eq!(window.display().unwrap().id, display.id);
        assert!(display.work_area.contains(window.position()));

        // Fullscreen covers the window's display.
        window.set_mode(WindowMode::BorderlessFullscreen).unwrap();
        let size = Size::new(display.bounds.width, display.bounds.height);
        assert_eq!(window.size(), size);
        assert_eq!(
            window.position(),
            Point::new(display.bounds.x, display.bounds.y)
        );

        // Moving a fullscreen window to every display covers that display.
        for other in &displays {
            window.set_display(other.id).unwrap();
            assert_eq!(window.display().unwrap().id, other.id);
            assert_eq!(
                window.position(),
                Point::new(other.bounds.x, other.bounds.y)
            );
        }
        window.set_mode(WindowMode::Windowed).unwrap();
        window.set_display(display.id).unwrap();
        assert_eq!(window.display().unwrap().id, display.id);
        app.pump();
    }
}
