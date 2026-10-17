//! Double-buffered events and per-reader cursors.

use marlin_ecs::{Events, World};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Hit(u32);

#[test]
fn events_live_for_two_updates() {
    let mut events = Events::<Hit>::new();
    events.send(Hit(1));
    events.send(Hit(2));
    assert_eq!(events.iter().copied().collect::<Vec<_>>(), [Hit(1), Hit(2)]);
    events.update();
    events.send(Hit(3));
    assert_eq!(events.len(), 3);
    events.update();
    assert_eq!(events.iter().copied().collect::<Vec<_>>(), [Hit(3)]);
    events.update();
    assert!(events.is_empty());
    assert_eq!(events.count(), 3, "ids keep counting");
}

#[test]
fn cursors_see_each_event_once_and_lose_only_expired_ones() {
    let mut events = Events::<Hit>::new();
    let mut fast = 0;
    let mut slow = 0;
    let read = |events: &Events<Hit>, cursor: &mut usize| {
        let seen: Vec<u32> = events.read_from(*cursor).map(|h| h.0).collect();
        *cursor = events.count();
        seen
    };
    for frame in 0..6u32 {
        events.send(Hit(frame * 10));
        events.send(Hit(frame * 10 + 1));
        assert_eq!(read(&events, &mut fast), [frame * 10, frame * 10 + 1]);
        if frame == 1 || frame == 5 {
            let seen = read(&events, &mut slow);
            match frame {
                1 => assert_eq!(seen, [0, 1, 10, 11]),
                // Frames 2 and 3 expired; 4 is still buffered.
                _ => assert_eq!(seen, [40, 41, 50, 51]),
            }
        }
        events.update();
    }
}

#[test]
fn drain_and_clear_empty_the_buffers() {
    let mut events = Events::<Hit>::new();
    events.send_batch([Hit(1), Hit(2)]);
    events.update();
    events.send(Hit(3));
    assert_eq!(events.drain().collect::<Vec<_>>(), [Hit(1), Hit(2), Hit(3)]);
    assert!(events.is_empty());
    events.send(Hit(4));
    assert_eq!(events.read_from(3).count(), 1, "ids continue after a drain");
    events.clear();
    assert!(events.is_empty());
}

#[test]
fn worlds_send_events() {
    let mut world = World::new();
    world.add_event::<Hit>();
    world.send_event(Hit(9));
    world.add_event::<Hit>();
    assert_eq!(
        world.resource::<Events<Hit>>().unwrap().len(),
        1,
        "add_event keeps events"
    );
}

#[test]
#[should_panic(expected = "never added")]
fn sending_unknown_events_panics() {
    World::new().send_event(Hit(0));
}
