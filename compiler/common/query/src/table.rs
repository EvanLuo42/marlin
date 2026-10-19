//! Storage of inputs and memos, and the red-green fetch.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};

use crate::cx::Cx;
use crate::db::{Db, DepKey, RunnerId, TableId, lock};
use crate::{Cancelled, Durability, Input, Query, Revision, short_name};

/// What the engine needs from a table without knowing its types.
pub(crate) trait AnyTable: Send + Sync {
    /// `Name(key)` for cycle messages.
    fn describe(&self, key: u32) -> String;

    /// Brings `key` up to date. `None` if its value changed after `revision`; otherwise the
    /// durability it has now, which a reader that stays green takes over.
    fn unchanged_since(
        &self,
        cx: &Cx<'_>,
        key: u32,
        revision: Revision,
    ) -> Result<Option<Durability>, Cancelled>;

    /// Wakes threads blocked on a running query here, so they notice a cancellation.
    fn wake_waiters(&self);
}

/// Interned keys of one table: each key gets a stable `u32`.
struct Keys<K> {
    index: HashMap<K, u32>,
    list: Vec<K>,
}

impl<K: Clone + Eq + std::hash::Hash> Keys<K> {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            list: Vec::new(),
        }
    }

    /// The index of `key` and whether it is new.
    fn intern(&mut self, key: &K) -> (u32, bool) {
        if let Some(&i) = self.index.get(key) {
            return (i, false);
        }
        let i = u32::try_from(self.list.len()).expect("more than u32::MAX keys in one table");
        self.list.push(key.clone());
        self.index.insert(key.clone(), i);
        (i, true)
    }
}

// ---- inputs ----

struct InputEntry<V> {
    value: Option<V>,
    changed_at: Revision,
    durability: Durability,
}

struct InputState<I: Input> {
    keys: Keys<I::Key>,
    entries: Vec<InputEntry<I::Value>>,
}

pub(crate) struct InputTable<I: Input> {
    state: RwLock<InputState<I>>,
}

impl<I: Input> InputTable<I> {
    pub(crate) fn new(_: TableId) -> Self {
        Self {
            state: RwLock::new(InputState {
                keys: Keys::new(),
                entries: Vec::new(),
            }),
        }
    }

    /// Stores `value`; returns the durability the change affects (the higher of the old and
    /// new one), or `None` if nothing changed.
    pub(crate) fn set(
        &self,
        key: I::Key,
        value: I::Value,
        durability: Durability,
        revision: Revision,
    ) -> Option<Durability> {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        let (index, new) = state.keys.intern(&key);
        if new {
            state.entries.push(InputEntry {
                value: None,
                changed_at: revision,
                durability,
            });
        }
        let entry = &mut state.entries[index as usize];
        if entry.value.as_ref() == Some(&value) && entry.durability == durability {
            return None;
        }
        let affected = if entry.value.is_some() {
            entry.durability.max(durability)
        } else {
            durability
        };
        entry.value = Some(value);
        entry.changed_at = revision;
        entry.durability = durability;
        Some(affected)
    }

    /// The key's index, value and durability, or `None` if it was never set.
    pub(crate) fn read(&self, key: &I::Key) -> Option<(u32, I::Value, Durability)> {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        let &index = state.keys.index.get(key)?;
        let entry = &state.entries[index as usize];
        Some((index, entry.value.clone()?, entry.durability))
    }
}

impl<I: Input> AnyTable for InputTable<I> {
    fn describe(&self, key: u32) -> String {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        format!("{}({:?})", short_name::<I>(), state.keys.list[key as usize])
    }

    fn unchanged_since(
        &self,
        _: &Cx<'_>,
        key: u32,
        revision: Revision,
    ) -> Result<Option<Durability>, Cancelled> {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        let entry = &state.entries[key as usize];
        Ok((entry.changed_at <= revision).then_some(entry.durability))
    }

    fn wake_waiters(&self) {}
}

// ---- queries ----

struct Memo<V> {
    value: V,
    /// The last revision in which the value was known to be current.
    verified_at: Revision,
    /// The revision in which the value last became different.
    changed_at: Revision,
    /// The lowest durability among the inputs it depends on.
    durability: Durability,
    /// Everything `execute` read, in order.
    deps: Arc<[DepKey]>,
}

struct Slot<V> {
    memo: Option<Memo<V>>,
    /// The snapshot computing or verifying this key right now.
    running: Option<crate::db::RunnerId>,
}

struct QueryState<Q: Query> {
    keys: Keys<Q::Key>,
    slots: Vec<Slot<Q::Value>>,
}

pub(crate) struct QueryTable<Q: Query> {
    id: TableId,
    state: Mutex<QueryState<Q>>,
    /// Signalled whenever a slot stops running.
    finished: Condvar,
}

/// A current value with the facts its readers record.
pub(crate) struct Fetched<V> {
    pub(crate) value: V,
    pub(crate) changed_at: Revision,
    pub(crate) durability: Durability,
}

pub(crate) enum Fetch<V> {
    Value(Fetched<V>),
    /// Waiting for the key would close a cycle of threads waiting on each other.
    Cycle,
}

/// Resets a claimed slot if the query fails or panics before storing a result.
struct Claim<'t, Q: Query> {
    table: &'t QueryTable<Q>,
    db: &'t Db,
    owner: RunnerId,
    index: u32,
    armed: bool,
}

impl<Q: Query> Drop for Claim<'_, Q> {
    fn drop(&mut self) {
        if self.armed {
            let mut state = lock(&self.table.state);
            state.slots[self.index as usize].running = None;
            self.table.released(self.db, self.owner, self.index);
        }
    }
}

impl<Q: Query> QueryTable<Q> {
    pub(crate) fn new(id: TableId) -> Self {
        Self {
            id,
            state: Mutex::new(QueryState {
                keys: Keys::new(),
                slots: Vec::new(),
            }),
            finished: Condvar::new(),
        }
    }

    pub(crate) fn intern(&self, key: &Q::Key) -> u32 {
        let mut state = lock(&self.state);
        let (index, new) = state.keys.intern(key);
        if new {
            state.slots.push(Slot {
                memo: None,
                running: None,
            });
        }
        index
    }

    fn dep(&self, index: u32) -> DepKey {
        DepKey {
            table: self.id,
            key: index,
        }
    }

    /// `owner` stopped running the slot at `index`. Called with the state locked: the waiters'
    /// edges go before they can run again.
    fn released(&self, db: &Db, owner: RunnerId, index: u32) {
        db.release_waiters(owner, self.dep(index));
        self.finished.notify_all();
    }

    /// The current value of the key at `index`: a memo still valid in this revision, a memo
    /// whose dependencies turned out unchanged, or a fresh result.
    pub(crate) fn fetch(&self, cx: &Cx<'_>, index: u32) -> Result<Fetch<Q::Value>, Cancelled> {
        let db = cx.db();
        let me = cx.runner();
        // The revision cannot change while a snapshot exists.
        let current = db.revision();
        let mut state = lock(&self.state);
        let old_deps = loop {
            if db.cancelled() {
                return Err(Cancelled);
            }
            let slot = &mut state.slots[index as usize];
            if let Some(owner) = slot.running {
                if owner == me || !db.begin_wait(me, owner, self.dep(index)) {
                    return Ok(Fetch::Cycle);
                }
                state = self
                    .finished
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                db.end_wait(me);
                continue;
            }
            if let Some(memo) = &mut slot.memo
                && (memo.verified_at == current
                    || memo.verified_at >= db.last_changed(memo.durability))
            {
                memo.verified_at = current;
                return Ok(Fetch::Value(fetched(memo)));
            }
            slot.running = Some(me);
            break slot.memo.as_ref().map(|m| (m.deps.clone(), m.verified_at));
        };
        drop(state);
        let mut claim = Claim {
            table: self,
            db,
            owner: me,
            index,
            armed: true,
        };

        if let Some((deps, verified_at)) = old_deps
            && let Some(durability) =
                cx.deps_unchanged_since(self.dep(index), &deps, verified_at)?
        {
            let mut state = lock(&self.state);
            let slot = &mut state.slots[index as usize];
            slot.running = None;
            let memo = slot.memo.as_mut().expect("verified memo");
            memo.verified_at = current;
            memo.durability = durability;
            let result = fetched(memo);
            claim.armed = false;
            self.released(db, me, index);
            return Ok(Fetch::Value(result));
        }

        let key = lock(&self.state).keys.list[index as usize].clone();
        let (result, deps, durability) = cx.execute::<Q>(self.dep(index), &key);
        let value = result?;

        let mut state = lock(&self.state);
        let slot = &mut state.slots[index as usize];
        let changed_at = match &slot.memo {
            Some(old) if old.value == value => old.changed_at,
            _ => current,
        };
        let memo = slot.memo.insert(Memo {
            value,
            verified_at: current,
            changed_at,
            durability,
            deps,
        });
        let result = fetched(memo);
        slot.running = None;
        claim.armed = false;
        self.released(db, me, index);
        Ok(Fetch::Value(result))
    }
}

fn fetched<V: Clone>(memo: &Memo<V>) -> Fetched<V> {
    Fetched {
        value: memo.value.clone(),
        changed_at: memo.changed_at,
        durability: memo.durability,
    }
}

impl<Q: Query> AnyTable for QueryTable<Q> {
    fn describe(&self, key: u32) -> String {
        let state = lock(&self.state);
        format!("{}({:?})", short_name::<Q>(), state.keys.list[key as usize])
    }

    fn unchanged_since(
        &self,
        cx: &Cx<'_>,
        key: u32,
        revision: Revision,
    ) -> Result<Option<Durability>, Cancelled> {
        Ok(match self.fetch(cx, key)? {
            Fetch::Value(fetched) => (fetched.changed_at <= revision).then_some(fetched.durability),
            // Conservatively changed: the reader recomputes and meets the cycle there.
            Fetch::Cycle => None,
        })
    }

    fn wake_waiters(&self) {
        // Taking the lock orders this after any waiter's check of the cancel flag.
        let _state = lock(&self.state);
        self.finished.notify_all();
    }
}
