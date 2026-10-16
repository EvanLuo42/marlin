//! The global registry: case-insensitive lookup by name, and change callbacks.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use marlin_containers::HashMap;
use marlin_hash::PrehashedState;
use marlin_std::sync::{Mutex, RwLock};
use marlin_text::{name_hash, utf8};

use crate::cvar::{Cvar, Source};
use crate::error::Error;

/// Keyed by `name_hash`, which is an xxh3 of the lowercased name and needs no rehashing.
static REGISTRY: RwLock<HashMap<u64, &'static Cvar, PrehashedState>> =
    RwLock::new(HashMap::with_hasher(PrehashedState));

/// Makes `cvar` findable by name. Registering the same cvar again does nothing; another cvar
/// with the same name (in any case) is [`Error::Duplicate`]. A name that is a dotted prefix of
/// another (`r` and `r.vsync`) is [`Error::Conflict`]: TOML could not hold both.
pub fn register(cvar: &'static Cvar) -> Result<(), Error> {
    let mut map = REGISTRY.write();
    match map.get(&cvar.hash()) {
        Some(&existing) if ptr::eq(existing, cvar) => return Ok(()),
        // Equal hashes with different names would be a 64-bit collision: refused the same way.
        Some(_) => return Err(Error::Duplicate(cvar.name())),
        None => {}
    }
    if let Some(other) = map
        .values()
        .find(|other| is_prefix(other.name(), cvar.name()) || is_prefix(cvar.name(), other.name()))
    {
        return Err(Error::Conflict(other.name()));
    }
    map.insert(cvar.hash(), cvar);
    Ok(())
}

/// Whether `name` continues `prefix` with a dot, ignoring case.
fn is_prefix(prefix: &str, name: &str) -> bool {
    utf8::starts_with_ignore_ascii_case(name, prefix) && name.as_bytes().get(prefix.len()) == Some(&b'.')
}

/// Registers every cvar it can and returns the first error.
pub fn register_all(cvars: &[&'static Cvar]) -> Result<(), Error> {
    let mut first = Ok(());
    for &cvar in cvars {
        let result = register(cvar);
        if first.is_ok() {
            first = result;
        }
    }
    first
}

/// Removes `cvar` (e.g. when its module unloads). Its callbacks stay until removed.
pub fn unregister(cvar: &'static Cvar) {
    let mut map = REGISTRY.write();
    if map.get(&cvar.hash()).is_some_and(|&c| ptr::eq(c, cvar)) {
        map.remove(&cvar.hash());
    }
}

/// The registered cvar with this name, in any ASCII case.
pub fn find(name: &str) -> Option<&'static Cvar> {
    let cvar = *REGISTRY.read().get(&name_hash(name))?;
    cvar.name().eq_ignore_ascii_case(name).then_some(cvar)
}

/// [`Cvar::set_str`] by name.
pub fn set(name: &str, value: &str, source: Source) -> Result<(), Error> {
    find(name).ok_or(Error::Unknown)?.set_str(value, source)
}

/// Every registered cvar, sorted by name ignoring case.
pub fn cvars() -> Vec<&'static Cvar> {
    let mut all: Vec<_> = REGISTRY.read().values().copied().collect();
    all.sort_by(|a, b| utf8::cmp_ignore_ascii_case(a.name(), b.name()));
    all
}

/// The registered cvars whose names start with `prefix` (ignoring case), sorted; for console
/// completion.
pub fn complete(prefix: &str) -> Vec<&'static Cvar> {
    let mut all: Vec<_> = REGISTRY
        .read()
        .values()
        .copied()
        .filter(|c| utf8::starts_with_ignore_ascii_case(c.name(), prefix))
        .collect();
    all.sort_by(|a, b| utf8::cmp_ignore_ascii_case(a.name(), b.name()));
    all
}

type Callback = Arc<dyn Fn(&'static Cvar) + Send + Sync>;

struct Entry {
    id: u64,
    /// The cvar's address, or `None` for every cvar.
    cvar: Option<usize>,
    f: Callback,
}

static CALLBACKS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Identifies a callback for [`remove_callback`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CallbackId(u64);

fn add(cvar: Option<&'static Cvar>, f: Box<dyn Fn(&'static Cvar) + Send + Sync>) -> CallbackId {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    CALLBACKS.lock().push(Entry {
        id,
        cvar: cvar.map(|c| ptr::from_ref(c) as usize),
        f: Arc::from(f),
    });
    CallbackId(id)
}

/// Calls `f` after each effective change of `cvar`, on the thread that set it. `f` may read and
/// set cvars, register callbacks and remove them.
pub fn on_change(
    cvar: &'static Cvar,
    f: impl Fn(&'static Cvar) + Send + Sync + 'static,
) -> CallbackId {
    add(Some(cvar), Box::new(f))
}

/// Calls `f` after each effective change of any cvar; see [`on_change`].
pub fn on_any_change(f: impl Fn(&'static Cvar) + Send + Sync + 'static) -> CallbackId {
    add(None, Box::new(f))
}

/// Returns whether the callback existed. A call already under way still finishes.
pub fn remove_callback(id: CallbackId) -> bool {
    let mut callbacks = CALLBACKS.lock();
    let len = callbacks.len();
    callbacks.retain(|e| e.id != id.0);
    callbacks.len() != len
}

/// Runs the callbacks for `cvar` outside the lock, so they may set cvars themselves.
pub(crate) fn notify(cvar: &'static Cvar) {
    let address = ptr::from_ref(cvar) as usize;
    let matching: Vec<Callback> = CALLBACKS
        .lock()
        .iter()
        .filter(|e| e.cvar.is_none_or(|c| c == address))
        .map(|e| e.f.clone())
        .collect();
    for f in matching {
        f(cvar);
    }
}
