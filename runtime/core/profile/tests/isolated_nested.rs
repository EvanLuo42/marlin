//! Nesting, trees, carry-over across frame marks and the frame ring, on one thread.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the profiler is global.
//! Run alone with `cargo test -p marlin-profile --test isolated_nested`.
#![cfg(not(marlin_build = "shipping"))]

use marlin_profile::{
    clear, frame_index, frame_mark, profile_scope, set_history_len, set_thread_name, with_history,
};

/// Usually begins and ends within one clock tick; nesting comes from buffer order, not ticks.
fn leaf() {
    profile_scope!("leaf");
}

fn middle() {
    profile_scope!("middle");
    leaf();
    leaf();
}

#[test]
fn nested_zones() {
    set_thread_name("main \"test\"");
    clear();

    {
        profile_scope!("root");
        middle();
        middle();
        leaf();
    }
    frame_mark();

    with_history(|h| {
        let frame = h.last().unwrap();
        let zones = frame.zones();
        // root, 2 middle, 5 leaf.
        assert_eq!(zones.len(), 8);
        assert!(
            zones
                .iter()
                .all(|z| !z.suspended() && z.thread() == z.end_thread())
        );
        assert_eq!(zones[0].name(), "root");
        assert_eq!(zones[0].parent(), None);
        for z in &zones[1..] {
            let parent = &zones[z.parent().unwrap()];
            let expected = if z.name() == "middle" {
                "root"
            } else {
                "middle"
            };
            // The last leaf is root's direct child.
            assert!(parent.name() == expected || (z.name() == "leaf" && parent.name() == "root"));
            assert!(parent.start_ticks() <= z.start_ticks() && z.end_ticks() <= parent.end_ticks());
        }

        let thread = h.thread(zones[0].thread()).unwrap();
        assert_eq!(thread.name(), Some("main \"test\""));

        let tree = h.last_tree().unwrap();
        assert_eq!(tree.threads.len(), 1);
        let t = &tree.threads[0];
        assert_eq!(t.name.as_deref(), Some("main \"test\""));
        assert_eq!(t.roots.len(), 1);
        let root = &t.roots[0];
        assert_eq!((root.name, root.calls), ("root", 1));
        let names: Vec<_> = root.children.iter().map(|c| (c.name, c.calls)).collect();
        assert_eq!(names, [("middle", 2), ("leaf", 1)]);
        let middle = root.child("middle").unwrap();
        assert_eq!(middle.children.len(), 1);
        assert_eq!(
            (middle.children[0].name, middle.children[0].calls),
            ("leaf", 4)
        );
        let inner: std::time::Duration = root.children.iter().map(|c| c.total).sum();
        assert_eq!(root.self_time, root.total - inner);
    });

    // A zone open across a mark is reported, whole, in the frame where it ends.
    let open_frame = frame_index();
    {
        profile_scope!("long");
        frame_mark();
        leaf();
    }
    frame_mark();
    with_history(|h| {
        assert!(h.frame(open_frame).unwrap().zones().is_empty());
        let frame = h.frame(open_frame + 1).unwrap();
        let long = frame.zones().iter().find(|z| z.name() == "long").unwrap();
        assert!(long.start_ticks() <= frame.start_ticks());
        let leaf = frame.zones().iter().find(|z| z.name() == "leaf").unwrap();
        assert_eq!(frame.zones()[leaf.parent().unwrap()].name(), "long");
    });

    // The ring keeps the last N frames.
    set_history_len(4);
    for _ in 0..10 {
        leaf();
        frame_mark();
    }
    with_history(|h| {
        assert_eq!(h.frames().len(), 4);
        let indices: Vec<_> = h.frames().map(|f| f.index()).collect();
        let last = frame_index() - 1;
        assert_eq!(indices, [last - 3, last - 2, last - 1, last]);
        assert!(h.frames().all(|f| f.zones().len() == 1));
        assert!(h.frames().all(|f| f.start_ticks() <= f.end_ticks()));
        assert_eq!(h.dropped_events(), 0);
    });
}
