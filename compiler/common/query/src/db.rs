//! The database: table registry, revisions, the read/write gate and the wait-for graph.

use std::any::{Any, TypeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock};

use crate::cx::Snapshot;
use crate::table::{AnyTable, InputTable, QueryTable};
use crate::{Durability, Input, Query, Revision};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TableId(pub(crate) u32);

/// One read of one key: a dependency edge, and the identity of an active query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DepKey {
    pub(crate) table: TableId,
    pub(crate) key: u32,
}

/// Identifies a snapshot, which is used by one thread at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RunnerId(pub(crate) u64);

#[derive(Default)]
struct Registry {
    by_type: HashMap<TypeId, TableId>,
    tables: Vec<Arc<dyn AnyTable>>,
    typed: Vec<Arc<dyn Any + Send + Sync>>,
}

#[derive(Default)]
struct Gate {
    readers: usize,
    writing: bool,
}

/// The memoized results of all queries and the values of all inputs.
///
/// Shared between threads (`Arc<Db>`). Reads go through a [`Snapshot`] per thread; [`set`]
/// and [`update`] change inputs. A change first asks running reads to stop (they return
/// [`Cancelled`](crate::Cancelled)), waits until every snapshot is dropped, then applies.
///
/// [`set`]: Db::set
/// [`update`]: Db::update
pub struct Db {
    registry: RwLock<Registry>,
    revision: AtomicU64,
    /// Per durability: the last revision in which an input of that durability or higher changed.
    last_changed: [AtomicU64; 2],
    gate: Mutex<Gate>,
    gate_changed: Condvar,
    cancel: AtomicBool,
    next_runner: AtomicU64,
    /// Which runner each blocked runner waits for, and on which slot. An edge exists only while
    /// that slot is running: releasing the slot removes it ([`Db::release_waiters`]).
    waits: Mutex<HashMap<RunnerId, (RunnerId, DepKey)>>,
    executions: AtomicU64,
    deep_verifications: AtomicU64,
}

/// Work counters since the `Db` was created, for tests and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Calls of `Query::execute`.
    pub executions: u64,
    /// Memos whose dependencies were walked to decide whether they are still current.
    pub deep_verifications: u64,
}

thread_local! {
    /// Snapshots alive on this thread: `set` from such a thread would wait for itself.
    static HELD: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A query that panicked (in a test, where panics unwind) leaves consistent state behind:
    // the claim guards reset what it held.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Db {
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Registry::default()),
            revision: AtomicU64::new(0),
            last_changed: [AtomicU64::new(0), AtomicU64::new(0)],
            gate: Mutex::new(Gate::default()),
            gate_changed: Condvar::new(),
            cancel: AtomicBool::new(false),
            next_runner: AtomicU64::new(0),
            waits: Mutex::new(HashMap::new()),
            executions: AtomicU64::new(0),
            deep_verifications: AtomicU64::new(0),
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            executions: self.executions.load(Ordering::Relaxed),
            deep_verifications: self.deep_verifications.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn count_execution(&self) {
        self.executions.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_deep_verification(&self) {
        self.deep_verifications.fetch_add(1, Ordering::Relaxed);
    }

    /// The current revision.
    pub fn revision(&self) -> Revision {
        Revision(self.revision.load(Ordering::Acquire))
    }

    /// Starts reading. Waits while an input change is being applied. Each thread takes its own.
    pub fn snapshot(&self) -> Snapshot<'_> {
        let mut gate = lock(&self.gate);
        while gate.writing {
            gate = self
                .gate_changed
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        gate.readers += 1;
        drop(gate);
        HELD.with(|h| h.set(h.get() + 1));
        let runner = RunnerId(self.next_runner.fetch_add(1, Ordering::Relaxed));
        Snapshot::new(self, runner)
    }

    pub(crate) fn release_snapshot(&self) {
        HELD.with(|h| h.set(h.get() - 1));
        let mut gate = lock(&self.gate);
        gate.readers -= 1;
        if gate.readers == 0 {
            self.gate_changed.notify_all();
        }
    }

    /// Sets one input. Same as an [`update`](Db::update) with one change.
    pub fn set<I: Input>(&self, key: I::Key, value: I::Value, durability: Durability) {
        self.update(|u| u.set::<I>(key, value, durability));
    }

    /// Applies several input changes as one revision. Running reads are cancelled first; this
    /// returns once the change is in place.
    ///
    /// # Panics
    /// If this thread holds a [`Snapshot`]: the change would wait for it forever.
    pub fn update(&self, f: impl FnOnce(&mut Updater<'_>)) {
        assert_eq!(
            HELD.with(Cell::get),
            0,
            "Db::update while this thread holds a Snapshot; drop it first"
        );
        let mut gate = lock(&self.gate);
        while gate.writing {
            gate = self
                .gate_changed
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        gate.writing = true;
        self.cancel.store(true, Ordering::Release);
        // Readers blocked on a running query wake up and see the flag.
        self.wake_all_tables();
        while gate.readers > 0 {
            gate = self
                .gate_changed
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(gate);

        let next = Revision(self.revision.load(Ordering::Acquire) + 1);
        let mut updater = Updater {
            db: self,
            next,
            changed: None,
        };
        f(&mut updater);
        if let Some(highest) = updater.changed {
            self.revision.store(next.0, Ordering::Release);
            self.last_changed[0].store(next.0, Ordering::Release);
            if highest == Durability::High {
                self.last_changed[1].store(next.0, Ordering::Release);
            }
        }

        self.cancel.store(false, Ordering::Release);
        let mut gate = lock(&self.gate);
        gate.writing = false;
        self.gate_changed.notify_all();
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// The last revision in which an input a memo of `durability` may depend on changed.
    pub(crate) fn last_changed(&self, durability: Durability) -> Revision {
        Revision(self.last_changed[durability as usize].load(Ordering::Acquire))
    }

    fn wake_all_tables(&self) {
        let registry = self.registry.read().unwrap_or_else(PoisonError::into_inner);
        for table in &registry.tables {
            table.wake_waiters();
        }
    }

    pub(crate) fn table_by_id(&self, id: TableId) -> Arc<dyn AnyTable> {
        let registry = self.registry.read().unwrap_or_else(PoisonError::into_inner);
        registry.tables[id.0 as usize].clone()
    }

    fn table<T: AnyTable + 'static>(&self, make: impl FnOnce(TableId) -> T) -> (TableId, Arc<T>) {
        let type_id = TypeId::of::<T>();
        {
            let registry = self.registry.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&id) = registry.by_type.get(&type_id) {
                let typed = registry.typed[id.0 as usize].clone();
                return (id, typed.downcast().expect("table type"));
            }
        }
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(&id) = registry.by_type.get(&type_id) {
            let typed = registry.typed[id.0 as usize].clone();
            return (id, typed.downcast().expect("table type"));
        }
        let id = TableId(registry.tables.len() as u32);
        let table = Arc::new(make(id));
        registry.by_type.insert(type_id, id);
        registry.tables.push(table.clone());
        registry.typed.push(table.clone());
        (id, table)
    }

    pub(crate) fn query_table<Q: Query>(&self) -> (TableId, Arc<QueryTable<Q>>) {
        self.table(QueryTable::<Q>::new)
    }

    pub(crate) fn input_table<I: Input>(&self) -> (TableId, Arc<InputTable<I>>) {
        self.table(InputTable::<I>::new)
    }

    /// Records that `waiter` is about to block on `owner`, which runs `slot`, unless that closes
    /// a cycle of waiting runners; then returns `false` and records nothing. Called with the
    /// slot's table locked, so the slot cannot be released in between.
    pub(crate) fn begin_wait(&self, waiter: RunnerId, owner: RunnerId, slot: DepKey) -> bool {
        let mut waits = lock(&self.waits);
        let mut at = owner;
        loop {
            if at == waiter {
                return false;
            }
            match waits.get(&at) {
                Some(&(next, _)) => at = next,
                None => break,
            }
        }
        waits.insert(waiter, (owner, slot));
        true
    }

    pub(crate) fn end_wait(&self, waiter: RunnerId) {
        lock(&self.waits).remove(&waiter);
    }

    /// `owner` stopped running `slot`: the runners blocked on it no longer wait for `owner`.
    /// Called with the slot's table locked, before waking them. A woken waiter only removes its
    /// own edge once it has that lock back; if `owner` asked for one of the waiter's slots in
    /// between, the stale edge would close a cycle that does not exist.
    pub(crate) fn release_waiters(&self, owner: RunnerId, slot: DepKey) {
        lock(&self.waits).retain(|_, edge| *edge != (owner, slot));
    }
}

impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}

/// Changes inputs inside [`Db::update`].
pub struct Updater<'db> {
    db: &'db Db,
    next: Revision,
    /// The highest durability among the inputs that changed, if any did.
    changed: Option<Durability>,
}

impl Updater<'_> {
    /// Sets `I(key)`. Setting the value and durability it already has changes nothing.
    pub fn set<I: Input>(&mut self, key: I::Key, value: I::Value, durability: Durability) {
        let (_, table) = self.db.input_table::<I>();
        if let Some(affected) = table.set(key, value, durability, self.next) {
            self.changed = Some(self.changed.map_or(affected, |d| d.max(affected)));
        }
    }
}
