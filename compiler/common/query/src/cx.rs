//! Reading: snapshots, the query context, dependency recording and cycle detection.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;

use crate::db::{Db, DepKey, RunnerId};
use crate::table::Fetch;
use crate::{Cancelled, Cycle, Durability, Input, Query};

/// A query being executed (or verified) on this thread, linked to the one that read it.
struct Frame<'a> {
    key: DepKey,
    parent: Option<&'a Frame<'a>>,
    deps: RefCell<Vec<DepKey>>,
    durability: Cell<Durability>,
}

/// The view a query has of the database: reads through it are recorded as dependencies of the
/// query being executed.
pub struct Cx<'a> {
    db: &'a Db,
    runner: RunnerId,
    frame: Option<&'a Frame<'a>>,
}

impl<'a> Cx<'a> {
    pub(crate) fn db(&self) -> &'a Db {
        self.db
    }

    pub(crate) fn runner(&self) -> RunnerId {
        self.runner
    }

    /// The value of query `Q` at `key`.
    pub fn get<Q: Query>(&self, key: &Q::Key) -> Result<Q::Value, Cancelled> {
        self.check_cancelled()?;
        let (table_id, table) = self.db.query_table::<Q>();
        let index = table.intern(key);
        let dep = DepKey {
            table: table_id,
            key: index,
        };
        if let Some(participants) = self.cycle_through(dep) {
            return self.recover::<Q>(dep, key, participants, false);
        }
        match table.fetch(self, index)? {
            Fetch::Value(fetched) => {
                self.record(dep, fetched.durability);
                Ok(fetched.value)
            }
            Fetch::Cycle => {
                let mut participants = self.chain();
                participants.push(self.db.table_by_id(dep.table).describe(dep.key));
                self.recover::<Q>(dep, key, participants, true)
            }
        }
    }

    /// The value of input `I` at `key`.
    ///
    /// # Panics
    /// If the host never set it.
    pub fn input<I: Input>(&self, key: &I::Key) -> Result<I::Value, Cancelled> {
        self.check_cancelled()?;
        let (table_id, table) = self.db.input_table::<I>();
        let Some((index, value, durability)) = table.read(key) else {
            panic!(
                "input {}({key:?}) was read before it was set",
                crate::short_name::<I>()
            );
        };
        self.record(
            DepKey {
                table: table_id,
                key: index,
            },
            durability,
        );
        Ok(value)
    }

    /// `Err(Cancelled)` once an input change is waiting. Call it in long loops.
    pub fn check_cancelled(&self) -> Result<(), Cancelled> {
        if self.db.cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    fn record(&self, dep: DepKey, durability: Durability) {
        if let Some(frame) = self.frame {
            frame.deps.borrow_mut().push(dep);
            frame.durability.set(frame.durability.get().min(durability));
        }
    }

    /// If `dep` is already active on this thread, the queries from it to here.
    fn cycle_through(&self, dep: DepKey) -> Option<Vec<String>> {
        let mut active = Vec::new();
        let mut frame = self.frame;
        while let Some(f) = frame {
            active.push(f.key);
            if f.key == dep {
                active.reverse();
                return Some(
                    active
                        .into_iter()
                        .map(|k| self.db.table_by_id(k.table).describe(k.key))
                        .collect(),
                );
            }
            frame = f.parent;
        }
        None
    }

    /// The active queries on this thread, outermost first.
    fn chain(&self) -> Vec<String> {
        let mut keys = Vec::new();
        let mut frame = self.frame;
        while let Some(f) = frame {
            keys.push(f.key);
            frame = f.parent;
        }
        keys.reverse();
        keys.into_iter()
            .map(|k| self.db.table_by_id(k.table).describe(k.key))
            .collect()
    }

    fn recover<Q: Query>(
        &self,
        dep: DepKey,
        key: &Q::Key,
        participants: Vec<String>,
        across_threads: bool,
    ) -> Result<Q::Value, Cancelled> {
        let cycle = Cycle {
            participants,
            across_threads,
        };
        match Q::cycle_fallback(&cycle, key) {
            Some(value) => {
                // Low durability: whatever read the fallback is checked again after any change.
                self.record(dep, Durability::Low);
                Ok(value)
            }
            None => panic!("query cycle: {cycle}"),
        }
    }

    /// Runs `Q::execute` for `key` in a new frame; returns the result with what it read.
    pub(crate) fn execute<Q: Query>(
        &self,
        dep: DepKey,
        key: &Q::Key,
    ) -> (Result<Q::Value, Cancelled>, Arc<[DepKey]>, Durability) {
        let frame = Frame {
            key: dep,
            parent: self.frame,
            deps: RefCell::new(Vec::new()),
            durability: Cell::new(Durability::High),
        };
        let cx = Cx {
            db: self.db,
            runner: self.runner,
            frame: Some(&frame),
        };
        self.db.count_execution();
        let result = Q::execute(&cx, key);
        let mut deps = frame.deps.into_inner();
        // Keep the first read of each dependency: verification walks them in read order.
        let mut seen = std::collections::HashSet::new();
        deps.retain(|d| seen.insert(*d));
        (result, deps.into(), frame.durability.get())
    }

    /// Checks `deps` in read order, while `dep` (the query they belong to) is active. `None` if
    /// any changed after `revision`; otherwise the lowest durability among them now.
    pub(crate) fn deps_unchanged_since(
        &self,
        dep: DepKey,
        deps: &[DepKey],
        revision: crate::Revision,
    ) -> Result<Option<Durability>, Cancelled> {
        let frame = Frame {
            key: dep,
            parent: self.frame,
            deps: RefCell::new(Vec::new()),
            durability: Cell::new(Durability::High),
        };
        self.db.count_deep_verification();
        let cx = Cx {
            db: self.db,
            runner: self.runner,
            frame: Some(&frame),
        };
        let mut durability = Durability::High;
        for &d in deps {
            cx.check_cancelled()?;
            // A dependency active on this thread is part of a cycle: treat it as changed and
            // let the recomputation meet the cycle.
            if cx.cycle_through(d).is_some() {
                return Ok(None);
            }
            match self
                .db
                .table_by_id(d.table)
                .unchanged_since(&cx, d.key, revision)?
            {
                Some(d) => durability = durability.min(d),
                None => return Ok(None),
            }
        }
        Ok(Some(durability))
    }
}

/// Read access for one thread. While any snapshot exists, inputs cannot change; a pending
/// change makes reads return [`Cancelled`] so that snapshots get dropped quickly.
pub struct Snapshot<'db> {
    cx: Cx<'db>,
    // Bound to its thread: the thread's held-snapshot count must stay right.
    _not_send: PhantomData<*const ()>,
}

impl<'db> Snapshot<'db> {
    pub(crate) fn new(db: &'db Db, runner: RunnerId) -> Self {
        Self {
            cx: Cx {
                db,
                runner,
                frame: None,
            },
            _not_send: PhantomData,
        }
    }
}

impl<'db> Deref for Snapshot<'db> {
    type Target = Cx<'db>;
    fn deref(&self) -> &Cx<'db> {
        &self.cx
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        self.cx.db.release_snapshot();
    }
}
