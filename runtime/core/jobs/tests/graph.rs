//! Frame graphs: validated once, run every frame in dependency order.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use marlin_jobs::{Config, GraphBuilder, JobSystem, NodeId, Priority};
use marlin_std::mem::MarlinHeap;
use marlin_std::thread::current;

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

fn system(workers: usize) -> JobSystem {
    JobSystem::new(Config {
        workers,
        fibers: 32,
        max_fibers: 512,
        fiber_stack_size: 128 << 10,
        pin_workers: false,
    })
    .unwrap()
}

/// Per frame: the order nodes finished in, by index.
#[derive(Default)]
struct Frame {
    order: Mutex<Vec<usize>>,
    frame: AtomicU32,
}

#[test]
fn nodes_run_after_their_dependencies_every_frame() {
    let jobs = system(4);
    let mut g = GraphBuilder::<Frame>::new();
    // A layered graph: 6 layers of 5 nodes, each depending on two of the layer before.
    let mut ids: Vec<NodeId> = Vec::new();
    let mut deps: Vec<Vec<usize>> = Vec::new();
    for layer in 0..6 {
        for k in 0..5 {
            let index = ids.len();
            let id = g.node("node", move |f: &Frame| {
                std::hint::black_box(f.frame.load(Ordering::Relaxed));
                f.order.lock().unwrap().push(index);
            });
            ids.push(id);
            let mine = if layer == 0 {
                Vec::new()
            } else {
                let base = (layer - 1) * 5;
                vec![base + k, base + (k + 2) % 5]
            };
            let dep_ids: Vec<NodeId> = mine.iter().map(|&d| ids[d]).collect();
            g.after(id, &dep_ids);
            deps.push(mine);
        }
    }
    g.priority(ids[0], Priority::High);
    let mut graph = g.build().unwrap();
    assert_eq!(graph.len(), 30);
    assert_eq!(graph.name(ids[3]), "node");
    for frame in 0..200 {
        let f = Frame::default();
        f.frame.store(frame, Ordering::Relaxed);
        graph.run(&jobs, &f);
        let order = f.order.into_inner().unwrap();
        assert_eq!(order.len(), 30, "frame {frame}");
        let mut position = [usize::MAX; 30];
        for (p, &i) in order.iter().enumerate() {
            position[i] = p;
        }
        for (i, d) in deps.iter().enumerate() {
            for &d in d {
                assert!(position[d] < position[i], "frame {frame}: {d} after {i}");
            }
        }
    }
}

#[test]
fn cycles_are_reported() {
    let mut g = GraphBuilder::<()>::new();
    let a = g.node("a", |_| {});
    let b = g.node("b", |_| {});
    let c = g.node("c", |_| {});
    let d = g.node("d", |_| {});
    g.after(b, &[a])
        .after(c, &[b])
        .after(a, &[c])
        .after(d, &[c]);
    let error = g.build().err().unwrap();
    let mut cycle = error.cycle().to_vec();
    assert_eq!(cycle.len(), 3);
    // Some rotation of a -> b -> c.
    while cycle[0] != "a" {
        cycle.rotate_left(1);
    }
    assert_eq!(cycle, ["a", "b", "c"]);
    assert!(error.to_string().starts_with("job graph has a cycle: "));

    let mut g = GraphBuilder::<()>::new();
    let a = g.node("self", |_| {});
    g.after(a, &[a]);
    assert_eq!(g.build().err().unwrap().cycle(), ["self"]);
}

#[test]
fn empty_and_single_node_graphs() {
    let jobs = system(1);
    let mut empty = GraphBuilder::<()>::new().build().unwrap();
    assert!(empty.is_empty());
    empty.run(&jobs, &());
    let n = AtomicUsize::new(0);
    let mut g = GraphBuilder::<AtomicUsize>::new();
    g.node("one", |n: &AtomicUsize| {
        n.fetch_add(1, Ordering::Relaxed);
    });
    let mut graph = g.build().unwrap();
    for _ in 0..100 {
        graph.run(&jobs, &n);
    }
    assert_eq!(n.load(Ordering::Relaxed), 100);
}

#[test]
fn main_thread_nodes() {
    let jobs = system(3);
    let main = jobs.main_thread();
    let me = current::id();
    let mut g = GraphBuilder::<Mutex<Vec<&'static str>>>::new();
    let input = g.main_node("input", move |log| {
        assert_eq!(current::id(), me);
        log.lock().unwrap().push("input");
    });
    let sim = g.node("sim", |log| log.lock().unwrap().push("sim"));
    let present = g.main_node("present", move |log| {
        assert_eq!(current::id(), me);
        log.lock().unwrap().push("present");
    });
    g.after(sim, &[input]).after(present, &[sim]);
    let mut graph = g.build().unwrap();
    for _ in 0..50 {
        let log = Mutex::new(Vec::new());
        graph.run(&jobs, &log);
        assert_eq!(log.into_inner().unwrap(), ["input", "sim", "present"]);
    }
    drop(main);
}

#[test]
fn graphs_run_inside_jobs() {
    let jobs = system(2);
    let mut g = GraphBuilder::<AtomicUsize>::new();
    let a = g.node("a", |n: &AtomicUsize| {
        n.fetch_add(1, Ordering::Relaxed);
    });
    let b = g.node("b", |n: &AtomicUsize| {
        n.fetch_add(10, Ordering::Relaxed);
    });
    g.after(b, &[a]);
    // A fiber-aware lock: it is held across the run's wait.
    let graph = marlin_jobs::sync::Mutex::new(g.build().unwrap());
    let n = AtomicUsize::new(0);
    jobs.parallel_for(0..20, 1, |_| graph.lock().run(&jobs, &n));
    assert_eq!(n.load(Ordering::Relaxed), 220);
}

#[cfg(not(marlin_build = "shipping"))]
#[test]
fn nodes_are_profiler_zones() {
    use marlin_profile::{frame_mark, with_history};

    let jobs = system(2);
    let mut g = GraphBuilder::<()>::new();
    let a = g.node("graph_test_physics", |_| {});
    let b = g.node("graph_test_render", |_| {});
    g.after(b, &[a]);
    let mut graph = g.build().unwrap();
    graph.run(&jobs, &());
    frame_mark();
    with_history(|h| {
        let frame = h.last().unwrap();
        let names: Vec<&str> = frame.zones().iter().map(|z| z.name()).collect();
        assert!(names.contains(&"graph_test_physics"), "{names:?}");
        assert!(names.contains(&"graph_test_render"), "{names:?}");
        let zone = frame
            .zones()
            .iter()
            .find(|z| z.name() == "graph_test_physics")
            .unwrap();
        assert!(zone.site().file.ends_with("graph.rs"));
    });
}
