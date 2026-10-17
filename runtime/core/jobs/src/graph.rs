//! Frame graphs: a fixed set of named jobs with dependencies, built once and run every frame.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt;
use core::panic::Location;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_profile::{Zone, ZoneSite};

use crate::counter::{Counter, Target};
use crate::scheduler::{Job, JobSystem, Priority, Shared};

/// A node of a [`GraphBuilder`] (and of the [`Graph`] built from it).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);

type NodeFn<C> = Box<dyn Fn(&C) + Send + Sync>;

/// Collects the nodes and dependencies of a [`Graph`]. Nodes run a function of the context
/// `C` passed to each [`Graph::run`].
pub struct GraphBuilder<C = ()> {
    nodes: Vec<Desc<C>>,
}

struct Desc<C> {
    name: &'static str,
    location: &'static Location<'static>,
    f: NodeFn<C>,
    deps: Vec<u32>,
    target: Target,
}

impl<C: Sync> GraphBuilder<C> {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Adds a node running `f` on any worker. `name` names its profiler zone.
    #[track_caller]
    pub fn node(&mut self, name: &'static str, f: impl Fn(&C) + Send + Sync + 'static) -> NodeId {
        self.add(name, Box::new(f), Target::Queue(Priority::Normal))
    }

    /// Adds a node running `f` on the main thread only.
    #[track_caller]
    pub fn main_node(
        &mut self,
        name: &'static str,
        f: impl Fn(&C) + Send + Sync + 'static,
    ) -> NodeId {
        self.add(name, Box::new(f), Target::Main)
    }

    #[track_caller]
    fn add(&mut self, name: &'static str, f: NodeFn<C>, target: Target) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Desc {
            name,
            location: Location::caller(),
            f,
            deps: Vec::new(),
            target,
        });
        id
    }

    /// Makes `node` start only after every node in `deps` has finished.
    ///
    /// # Panics
    /// If a node is not of this builder.
    pub fn after(&mut self, node: NodeId, deps: &[NodeId]) -> &mut Self {
        let n = self.nodes.len() as u32;
        assert!(node.0 < n, "marlin-jobs: node of another graph");
        for dep in deps {
            assert!(dep.0 < n, "marlin-jobs: node of another graph");
            let list = &mut self.nodes[node.0 as usize].deps;
            if !list.contains(&dep.0) {
                list.push(dep.0);
            }
        }
        self
    }

    /// The queue priority of a node not on the main thread (default: Normal).
    pub fn priority(&mut self, node: NodeId, priority: Priority) -> &mut Self {
        let desc = &mut self.nodes[node.0 as usize];
        if let Target::Queue(p) = &mut desc.target {
            *p = priority;
        }
        self
    }

    /// Checks for cycles and builds the graph.
    pub fn build(self) -> Result<Graph<C>, CycleError> {
        let n = self.nodes.len();
        let mut dependents: Vec<Vec<u32>> = (0..n).map(|_| Vec::new()).collect();
        for (i, desc) in self.nodes.iter().enumerate() {
            for &dep in &desc.deps {
                dependents[dep as usize].push(i as u32);
            }
        }
        // Kahn's algorithm: whatever never runs out of dependencies is on or after a cycle.
        let mut remaining: Vec<u32> = self.nodes.iter().map(|d| d.deps.len() as u32).collect();
        let mut order: Vec<u32> = (0..n as u32)
            .filter(|&i| remaining[i as usize] == 0)
            .collect();
        let roots = order.clone().into_boxed_slice();
        let mut next = 0;
        while next < order.len() {
            let node = order[next] as usize;
            next += 1;
            for &d in &dependents[node] {
                remaining[d as usize] -= 1;
                if remaining[d as usize] == 0 {
                    order.push(d);
                }
            }
        }
        if order.len() < n {
            return Err(CycleError::find(&self.nodes, &remaining));
        }

        // The profiler keeps zone sites for its whole history: they must be 'static.
        let sites: Option<&'static [ZoneSite]> = marlin_profile::ENABLED.then(|| {
            let sites: Box<[ZoneSite]> = self
                .nodes
                .iter()
                .map(|d| ZoneSite {
                    name: d.name,
                    file: d.location.file(),
                    line: d.location.line(),
                })
                .collect();
            &*Box::leak(sites)
        });
        let nodes: Box<[Node<C>]> = self
            .nodes
            .into_iter()
            .zip(dependents)
            .enumerate()
            .map(|(i, (desc, dependents))| Node {
                f: desc.f,
                name: desc.name,
                deps: desc.deps.len() as u32,
                pending: AtomicU32::new(0),
                dependents: dependents.into_boxed_slice(),
                target: desc.target,
                site: sites.map(|s| &s[i]),
                inner: ptr::null(),
            })
            .collect();
        let mut inner = Box::new(Inner {
            nodes,
            roots,
            counter: Arc::new(Counter::new()),
            run: UnsafeCell::new(Run {
                ctx: ptr::null(),
                shared: ptr::null(),
            }),
        });
        let raw: *const Inner<C> = &*inner;
        for node in inner.nodes.iter_mut() {
            node.inner = raw;
        }
        Ok(Graph { inner })
    }
}

impl<C: Sync> Default for GraphBuilder<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// A validated job graph, run as a whole by [`run`](Self::run): a frame's jobs and their
/// dependencies, built once by a [`GraphBuilder`].
///
/// ```ignore
/// let mut g = GraphBuilder::<Frame>::new();
/// let input = g.main_node("input", |f| f.poll_input());
/// let sim = g.node("sim", |f| f.simulate());
/// let anim = g.node("anim", |f| f.animate());
/// let render = g.node("render", |f| f.render());
/// let present = g.main_node("present", |f| f.present());
/// g.after(sim, &[input]).after(anim, &[input]).after(render, &[sim, anim]).after(present, &[render]);
/// let mut graph = g.build()?; // Fails on a cycle.
/// loop {
///     graph.run(&jobs, &frame); // Every node has finished here.
/// }
/// ```
///
/// Building checks the graph for cycles and allocates everything: a run only resets one counter
/// per node and queues each node as its last dependency finishes, without allocating. Each node
/// runs inside a profiler zone named after it (none in shipping builds, as for
/// `marlin_profile::profile_scope!`); the zone sites are leaked once per built graph, since the
/// profiler's history keeps references to them. Main-thread nodes run only on the registered
/// main thread (see [`JobSystem::main_thread`]): run such a graph from the main thread, or have
/// it pump.
pub struct Graph<C = ()> {
    inner: Box<Inner<C>>,
}

struct Inner<C> {
    nodes: Box<[Node<C>]>,
    /// Nodes without dependencies.
    roots: Box<[u32]>,
    /// Unfinished nodes of the current run. An `Arc`: the last node's decrement may still touch
    /// it after `run` has returned.
    counter: Arc<Counter>,
    run: UnsafeCell<Run<C>>,
}

/// The current run's arguments: written by `run` before any node is queued, read by the nodes.
struct Run<C> {
    ctx: *const C,
    shared: *const Shared,
}

struct Node<C> {
    f: NodeFn<C>,
    name: &'static str,
    deps: u32,
    /// Dependencies not finished in the current run.
    pending: AtomicU32,
    dependents: Box<[u32]>,
    target: Target,
    site: Option<&'static ZoneSite>,
    inner: *const Inner<C>,
}

// SAFETY: the raw pointers are into the graph itself (stable in its box), and to the context
// and job system of a run, which outlive it; `Run` is written only by `run`, through `&mut`,
// before any node is queued. Nodes call the functions (`Send + Sync`) with `&C` from many
// threads, hence `C: Sync`.
unsafe impl<C: Sync> Send for Graph<C> {}
// SAFETY: as above; `&Graph` allows nothing but reading names.
unsafe impl<C: Sync> Sync for Graph<C> {}

impl<C: Sync> Graph<C> {
    /// Runs every node, each once its dependencies have finished, and returns when all have.
    /// Waits like any other wait: parks the fiber in a job, runs jobs on the main thread,
    /// blocks other threads.
    pub fn run(&mut self, jobs: &JobSystem, ctx: &C) {
        let inner = &*self.inner;
        if inner.nodes.is_empty() {
            return;
        }
        // SAFETY: no node of a previous run is queued or running (that run waited for all of
        // them), and `&mut self` excludes other runs.
        unsafe {
            *inner.run.get() = Run {
                ctx,
                shared: &*jobs.shared,
            };
        }
        for node in inner.nodes.iter() {
            node.pending.store(node.deps, Ordering::Relaxed);
        }
        inner.counter.add(inner.nodes.len() as u32);
        for &root in inner.roots.iter() {
            inner.queue(root);
        }
        inner.counter.wait();
    }

    pub fn len(&self) -> usize {
        self.inner.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.nodes.is_empty()
    }

    pub fn name(&self, node: NodeId) -> &'static str {
        self.inner.nodes[node.0 as usize].name
    }
}

impl<C: Sync> Inner<C> {
    fn queue(&self, index: u32) {
        let node = &self.nodes[index as usize];
        let job = Job {
            run: run_node::<C>,
            data: ptr::from_ref(node).cast_mut().cast(),
        };
        // SAFETY: set by `run` before the first node was queued.
        let shared = unsafe { &*(*self.run.get()).shared };
        node.target.push(shared, job);
    }
}

/// # Safety
/// `data` is a node of a graph being run.
unsafe fn run_node<C: Sync>(data: *mut ()) {
    // SAFETY: the graph and the run's context outlive the run, which waits for this node.
    unsafe {
        let node = &*data.cast::<Node<C>>();
        let inner = &*node.inner;
        {
            let _zone = node.site.map(Zone::begin);
            (node.f)(&*(*inner.run.get()).ctx);
        }
        for &dependent in node.dependents.iter() {
            // AcqRel: the last dependency to finish passes on what all of them wrote.
            if inner.nodes[dependent as usize]
                .pending
                .fetch_sub(1, Ordering::AcqRel)
                == 1
            {
                inner.queue(dependent);
            }
        }
        // Last: once the count reaches zero, `run` may return and the graph go away.
        let counter = inner.counter.clone();
        counter.decrement();
    }
}

/// [`GraphBuilder::build`] found a cycle.
#[derive(Clone, PartialEq, Eq)]
pub struct CycleError {
    cycle: Vec<&'static str>,
}

impl CycleError {
    /// The names of the nodes on one cycle, each a dependency of the next and the last of the
    /// first.
    pub fn cycle(&self) -> &[&'static str] {
        &self.cycle
    }

    /// Walks back from a node left over by Kahn's algorithm through left-over dependencies (each
    /// has one) until a node repeats.
    fn find<C>(nodes: &[Desc<C>], remaining: &[u32]) -> Self {
        let left = |i: usize| remaining[i] > 0;
        let mut seen = alloc::vec![usize::MAX; nodes.len()];
        let mut path = Vec::new();
        let mut node = (0..nodes.len()).find(|&i| left(i)).unwrap();
        while seen[node] == usize::MAX {
            seen[node] = path.len();
            path.push(node);
            node = nodes[node]
                .deps
                .iter()
                .map(|&d| d as usize)
                .find(|&d| left(d))
                .expect("a left-over node has a left-over dependency");
        }
        let cycle = path[seen[node]..]
            .iter()
            .rev()
            .map(|&i| nodes[i].name)
            .collect();
        Self { cycle }
    }
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("job graph has a cycle: ")?;
        for name in &self.cycle {
            write!(f, "{name} -> ")?;
        }
        f.write_str(self.cycle.first().copied().unwrap_or(""))
    }
}

impl fmt::Debug for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl core::error::Error for CycleError {}
