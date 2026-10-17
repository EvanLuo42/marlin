//! The incremental query engine of the Marlin language toolchain
//! (`docs/design/0015-language-frontend.md` §4).
//!
//! A compiler is a set of pure functions over inputs: a [`Query`] maps a key to a value and may
//! read [`Input`]s and other queries through its [`Cx`]. The [`Db`] memoizes every result with
//! the dependencies it read. When an input changes, a memo is checked again only when it is
//! asked for: if none of its dependencies changed it is reused as is ("green"); otherwise it is
//! recomputed, and if the new value equals the old one, the queries that depend on it are not
//! recomputed either (backdating).
//!
//! - Any crate adds a query by adding a type; all queries of all languages live in one `Db`.
//! - The `Db` is `Sync`. Each thread reads through its own [`Snapshot`]; a key is computed once
//!   and other threads asking for it wait.
//! - Changing an input cancels running reads cooperatively: every read returns
//!   `Result<_, Cancelled>`, and queries pass `Cancelled` up with `?`.
//! - Cycles are detected, within a thread and across threads. A query that may sit on a cycle
//!   gives a [`Query::cycle_fallback`]; a cycle through any other query is a compiler bug and
//!   panics, naming every query on it.

mod cx;
mod db;
mod table;

use std::fmt::{self, Debug};
use std::hash::Hash;

pub use cx::{Cx, Snapshot};
pub use db::{Db, Stats, Updater};

/// A read was abandoned because an input is being changed. Return it with `?`; the host
/// retries the request after the change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the query was cancelled by an input change")
    }
}

impl std::error::Error for Cancelled {}

/// How often an input changes. Memos that read only `High` inputs are not checked again when
/// only `Low` inputs change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Durability {
    /// Project sources and anything else that is edited.
    Low,
    /// The standard and core libraries, target descriptions: rarely changed.
    High,
}

/// What keys and values must be: owned, comparable, shareable data.
pub trait Data: Clone + Eq + Debug + Send + Sync + 'static {}

impl<T: Clone + Eq + Debug + Send + Sync + 'static> Data for T {}

/// A value the host sets: a source text, a dialect, a target description.
pub trait Input: 'static {
    type Key: Data + Hash;
    type Value: Data;
}

/// A derived value: a pure function of inputs and other queries.
pub trait Query: 'static {
    type Key: Data + Hash;
    /// Usually an `Arc` or a small value: it is cloned on every read.
    type Value: Data;

    /// Computes the value. Reads only through `cx`, and returns `Err(Cancelled)` exactly when a
    /// read it made did.
    fn execute(cx: &Cx<'_>, key: &Self::Key) -> Result<Self::Value, Cancelled>;

    /// The value a read of `key` gets when that read would close a cycle. `None` (the default)
    /// means a cycle through this query is a bug, and the read panics.
    fn cycle_fallback(cycle: &Cycle, key: &Self::Key) -> Option<Self::Value> {
        let _ = (cycle, key);
        None
    }
}

/// The queries on a cycle, from the one read again back to the read that closed the cycle,
/// each as `Query(key)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    pub participants: Vec<String>,
    /// Whether the cycle runs through queries on other threads.
    pub across_threads: bool,
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.participants.join(" -> "))?;
        if let Some(first) = self.participants.first() {
            write!(f, " -> {first}")?;
        }
        if self.across_threads {
            f.write_str(" (across threads)")?;
        }
        Ok(())
    }
}

/// A point in the history of the inputs. Every change that alters an input starts a new one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

/// `std::any::type_name` without the module path, for messages.
fn short_name<T: ?Sized>() -> &'static str {
    let name = std::any::type_name::<T>();
    match name.find('<') {
        Some(generic) => {
            let base = &name[..generic];
            &name[base.rfind("::").map_or(0, |i| i + 2)..]
        }
        None => &name[name.rfind("::").map_or(0, |i| i + 2)..],
    }
}
