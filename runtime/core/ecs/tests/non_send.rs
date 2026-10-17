//! Non-send resources: usable on their thread and from exclusive systems, refused elsewhere.

use std::cell::Cell;
use std::rc::Rc;

use marlin_ecs::{Schedule, System, World};
use marlin_jobs::{Config, JobSystem};

/// A `!Send + !Sync` resource.
struct Window {
    title: Rc<Cell<&'static str>>,
}

#[test]
fn non_send_resources_live_on_their_thread() {
    let mut world = World::new();
    let title = Rc::new(Cell::new("untitled"));
    world.insert_non_send(Window {
        title: title.clone(),
    });
    assert!(world.has_non_send::<Window>());
    world.non_send_mut::<Window>().unwrap().title.set("game");
    assert_eq!(world.non_send::<Window>().unwrap().title.get(), "game");

    // Exclusive systems run on the thread calling `run`.
    let jobs = JobSystem::new(Config {
        workers: 2,
        ..Config::default()
    })
    .unwrap();
    let mut schedule = Schedule::new();
    schedule.add_system(System::exclusive("rename", |world, _| {
        world.non_send_mut::<Window>().unwrap().title.set("renamed");
    }));
    schedule.add_system(|world: &mut World| {
        assert_eq!(world.non_send::<Window>().unwrap().title.get(), "renamed");
    });
    schedule.run(&mut world, &jobs);
    assert_eq!(title.get(), "renamed");
    let window = world.remove_non_send::<Window>().unwrap();
    assert_eq!(Rc::strong_count(&window.title), 2);
}

#[test]
fn other_threads_are_refused() {
    let mut world = World::new();
    world.insert_non_send(Window {
        title: Rc::new(Cell::new("main")),
    });
    world.insert_resource(1u32);
    let result = std::thread::spawn(move || {
        // Ordinary resources still work on any thread.
        assert_eq!(world.resource::<u32>(), Some(&1));
        let _ = world.non_send::<Window>();
    })
    .join();
    // The world (and its leaked window) went with the thread.
    let message = result.unwrap_err();
    let message = message
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| message.downcast_ref::<String>().map(String::as_str))
        .unwrap();
    assert!(message.contains("thread other than the one that inserted them"));
}
