use marlin_app::{Event, EventQueue, Size, WindowId};

#[test]
fn starts_empty() {
    let q = EventQueue::<4>::new();
    assert!(q.is_empty());
    assert_eq!(q.len(), 0);
    assert_eq!(q.capacity(), 4);
    assert_eq!(q.dropped(), 0);
    assert_eq!(q.as_slice(), &[]);
}

#[test]
fn keeps_order() {
    let mut q = EventQueue::<8>::new();
    for e in [Event::Suspended, Event::Resumed, Event::Quit] {
        assert!(q.push(e));
    }
    assert_eq!(
        q.as_slice(),
        &[Event::Suspended, Event::Resumed, Event::Quit]
    );
    assert_eq!(q.iter().count(), 3);
}

#[test]
fn drops_and_counts_when_full() {
    let mut q = EventQueue::<2>::new();
    assert!(q.push(Event::Suspended));
    assert!(q.push(Event::Resumed));
    assert!(!q.push(Event::Quit));
    assert!(!q.push(Event::Quit));
    assert_eq!(q.len(), 2);
    assert_eq!(q.dropped(), 2);

    q.clear();
    assert!(q.is_empty());
    assert_eq!(q.dropped(), 0);
    assert!(q.push(Event::Quit));
}

#[test]
fn append_moves_events_and_drop_counts() {
    let mut a = EventQueue::<4>::new();
    let mut b = EventQueue::<2>::new();
    a.push(Event::Suspended);
    b.push(Event::Resumed);
    b.push(Event::Quit);
    b.push(Event::Quit); // dropped
    a.append(&mut b);
    assert_eq!(
        a.as_slice(),
        &[Event::Suspended, Event::Resumed, Event::Quit]
    );
    assert_eq!(a.dropped(), 1);
    assert!(b.is_empty());
    assert_eq!(b.dropped(), 0);
}

#[test]
fn event_window() {
    assert_eq!(Event::Quit.window(), None);
    assert_eq!(Event::Suspended.window(), None);
}

#[test]
fn size_new() {
    assert_eq!(
        Size::new(3, 4),
        Size {
            width: 3,
            height: 4
        }
    );
}

#[test]
fn coalesces_consecutive_resizes_of_one_window() {
    let (a, b) = (WindowId::from_raw(1), WindowId::from_raw(2));
    let resized = |window, w| Event::Resized {
        window,
        size: Size::new(w, 10),
    };
    let mut q = EventQueue::<8>::new();
    q.push(resized(a, 1));
    q.push(resized(a, 2));
    q.push(resized(a, 3));
    assert_eq!(q.as_slice(), &[resized(a, 3)]);

    // Another window's resize, or any other event, ends the run.
    q.push(resized(b, 4));
    q.push(resized(a, 5));
    q.push(Event::Focused {
        window: a,
        focused: true,
    });
    q.push(resized(a, 6));
    assert_eq!(q.len(), 5);
    assert_eq!(q.as_slice()[4], resized(a, 6));
    assert_eq!(q.as_slice()[4].window(), Some(a));

    q.push(Event::DpiChanged { window: b, dpi: 96 });
    q.push(Event::DpiChanged {
        window: b,
        dpi: 144,
    });
    assert_eq!(q.len(), 6);
    assert_eq!(
        q.as_slice()[5],
        Event::DpiChanged {
            window: b,
            dpi: 144
        }
    );
}

#[test]
fn coalescing_works_when_full() {
    let a = WindowId::from_raw(1);
    let mut q = EventQueue::<1>::new();
    q.push(Event::Resized {
        window: a,
        size: Size::new(1, 1),
    });
    assert!(q.push(Event::Resized {
        window: a,
        size: Size::new(2, 2),
    }));
    assert_eq!(q.dropped(), 0);
}
