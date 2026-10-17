//! The global registry: case-insensitive lookup of cvars and commands by name, and change
//! callbacks.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use marlin_containers::HashMap;
use marlin_hash::PrehashedState;
use marlin_std::sync::{Mutex, Once, RwLock};
use marlin_text::{name_hash, utf8};

use crate::console::Command;
use crate::cvar::{Cvar, DEV_ENABLED, Flags, Source};
use crate::declared;
use crate::error::Error;

/// A registered name: a cvar or a console command. Both share one namespace.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Cvar(&'static Cvar),
    Command(&'static Command),
}

impl Entry {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cvar(c) => c.name(),
            Self::Command(c) => c.name(),
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Cvar(c) => c.help(),
            Self::Command(c) => c.help(),
        }
    }

    pub fn flags(self) -> Flags {
        match self {
            Self::Cvar(c) => c.flags(),
            Self::Command(c) => c.flags(),
        }
    }

    fn hash(self) -> u64 {
        match self {
            Self::Cvar(c) => c.hash(),
            Self::Command(c) => c.hash(),
        }
    }

    /// The same cvar or command, not just the same name.
    fn is(self, other: Self) -> bool {
        match (self, other) {
            (Self::Cvar(a), Self::Cvar(b)) => ptr::eq(a, b),
            (Self::Command(a), Self::Command(b)) => ptr::eq(a, b),
            _ => false,
        }
    }

    /// Whether lists and completion show it: development-only entries are hidden in shipping
    /// builds.
    fn visible(self) -> bool {
        DEV_ENABLED || !self.flags().contains(Flags::DEV_ONLY)
    }
}

/// Keyed by `name_hash`, which is an xxh3 of the lowercased name and needs no rehashing.
static REGISTRY: RwLock<HashMap<u64, Entry, PrehashedState>> =
    RwLock::new(HashMap::with_hasher(PrehashedState));
static DECLARED: Once = Once::new();

/// Registers the declared entries before the first use of the registry.
fn declared() {
    DECLARED.call_once(|| {
        // Errors are for `register_declared` to report.
        let _ = insert_all(declared::entries());
    });
}

fn insert_all(entries: impl IntoIterator<Item = Entry>) -> Result<(), Error> {
    let mut first = Ok(());
    for entry in entries {
        let result = insert(entry);
        if first.is_ok() {
            first = result;
        }
    }
    first
}

fn insert(entry: Entry) -> Result<(), Error> {
    let mut map = REGISTRY.write();
    match map.get(&entry.hash()) {
        Some(&existing) if existing.is(entry) => return Ok(()),
        // Equal hashes with different names would be a 64-bit collision: refused the same way.
        Some(_) => return Err(Error::Duplicate(entry.name())),
        None => {}
    }
    if let Entry::Cvar(cvar) = entry
        && let Some(other) = map.values().find_map(|other| match *other {
            Entry::Cvar(other)
                if is_prefix(other.name(), cvar.name()) || is_prefix(cvar.name(), other.name()) =>
            {
                Some(other)
            }
            _ => None,
        })
    {
        return Err(Error::Conflict(other.name()));
    }
    map.insert(entry.hash(), entry);
    Ok(())
}

fn remove(entry: Entry) {
    let mut map = REGISTRY.write();
    if map.get(&entry.hash()).is_some_and(|e| e.is(entry)) {
        map.remove(&entry.hash());
    }
}

/// Makes `cvar` findable by name. Registering the same cvar again does nothing; another cvar
/// or command with the same name (in any case) is [`Error::Duplicate`]. A name that is a
/// dotted prefix of another cvar's (`r` and `r.vsync`) is [`Error::Conflict`]: TOML could not
/// hold both.
pub fn register(cvar: &'static Cvar) -> Result<(), Error> {
    declared();
    insert(Entry::Cvar(cvar))
}

/// Whether `name` continues `prefix` with a dot, ignoring case.
fn is_prefix(prefix: &str, name: &str) -> bool {
    utf8::starts_with_ignore_ascii_case(name, prefix)
        && name.as_bytes().get(prefix.len()) == Some(&b'.')
}

/// Registers every cvar it can and returns the first error.
pub fn register_all(cvars: &[&'static Cvar]) -> Result<(), Error> {
    declared();
    insert_all(cvars.iter().map(|&c| Entry::Cvar(c)))
}

/// Makes `command` findable by name; see [`register`].
pub fn register_command(command: &'static Command) -> Result<(), Error> {
    declared();
    insert(Entry::Command(command))
}

/// Registers every cvar and command declared with [`declare!`](crate::declare) in the crates
/// linked into this binary (or shared library), and returns the first error. This happens by
/// itself before the registry is first used; call it to learn of duplicates and conflicts.
pub fn register_declared() -> Result<(), Error> {
    declared();
    insert_all(declared::entries())
}

/// Removes `cvar` (e.g. when its module unloads). Its callbacks stay until removed.
pub fn unregister(cvar: &'static Cvar) {
    remove(Entry::Cvar(cvar));
}

pub fn unregister_command(command: &'static Command) {
    remove(Entry::Command(command));
}

/// The registered cvar or command with this name, in any ASCII case.
pub fn lookup(name: &str) -> Option<Entry> {
    declared();
    let entry = *REGISTRY.read().get(&name_hash(name))?;
    entry.name().eq_ignore_ascii_case(name).then_some(entry)
}

/// The registered cvar with this name, in any ASCII case.
pub fn find(name: &str) -> Option<&'static Cvar> {
    match lookup(name)? {
        Entry::Cvar(cvar) => Some(cvar),
        Entry::Command(_) => None,
    }
}

/// The registered command with this name, in any ASCII case.
pub fn find_command(name: &str) -> Option<&'static Command> {
    match lookup(name)? {
        Entry::Command(command) => Some(command),
        Entry::Cvar(_) => None,
    }
}

/// [`Cvar::set_str`] by name.
pub fn set(name: &str, value: &str, source: Source) -> Result<(), Error> {
    find(name).ok_or(Error::Unknown)?.set_str(value, source)
}

/// The registered entries `keep` accepts, sorted by name ignoring case.
fn sorted<T>(keep: impl Fn(Entry) -> Option<T>, name: impl Fn(&T) -> &str) -> Vec<T> {
    declared();
    let mut all: Vec<T> = REGISTRY.read().values().filter_map(|&e| keep(e)).collect();
    all.sort_by(|a, b| utf8::cmp_ignore_ascii_case(name(a), name(b)));
    all
}

/// Every registered cvar, sorted by name ignoring case.
pub fn cvars() -> Vec<&'static Cvar> {
    sorted(
        |e| match e {
            Entry::Cvar(c) => Some(c),
            Entry::Command(_) => None,
        },
        |c| c.name(),
    )
}

/// Every registered command, sorted by name ignoring case.
pub fn commands() -> Vec<&'static Command> {
    sorted(
        |e| match e {
            Entry::Command(c) => Some(c),
            Entry::Cvar(_) => None,
        },
        |c| c.name(),
    )
}

/// The registered cvars and commands whose names start with `prefix` (ignoring case), sorted;
/// for console completion. Development-only ones are left out of shipping builds.
pub fn complete(prefix: &str) -> Vec<Entry> {
    sorted(
        |e| (e.visible() && utf8::starts_with_ignore_ascii_case(e.name(), prefix)).then_some(e),
        |e| e.name(),
    )
}

/// Every visible entry whose name contains `filter` (ignoring case), sorted.
pub(crate) fn matching(filter: &str) -> Vec<Entry> {
    sorted(
        |e| (e.visible() && utf8::contains_ignore_ascii_case(e.name(), filter)).then_some(e),
        |e| e.name(),
    )
}

type Callback = Arc<dyn Fn(&'static Cvar) + Send + Sync>;

/// Which cvars a callback is for.
#[derive(Clone, Copy)]
enum Filter {
    /// The cvar's address.
    One(usize),
    All,
    /// Cvars with any of these flags.
    Flagged(Flags),
}

struct Callbacks {
    id: u64,
    filter: Filter,
    f: Callback,
}

static CALLBACKS: Mutex<Vec<Callbacks>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Identifies a callback for [`remove_callback`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CallbackId(u64);

fn add(filter: Filter, f: Box<dyn Fn(&'static Cvar) + Send + Sync>) -> CallbackId {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    CALLBACKS.lock().push(Callbacks {
        id,
        filter,
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
    add(Filter::One(ptr::from_ref(cvar) as usize), Box::new(f))
}

/// Calls `f` after each effective change of any cvar; see [`on_change`].
pub fn on_any_change(f: impl Fn(&'static Cvar) + Send + Sync + 'static) -> CallbackId {
    add(Filter::All, Box::new(f))
}

/// Calls `f` after each effective change of a [`Flags::REPLICATED`] cvar, whatever set it: the
/// hook for a net layer, whose server sends these to its clients. See [`on_change`].
pub fn on_replicated_change(f: impl Fn(&'static Cvar) + Send + Sync + 'static) -> CallbackId {
    add(Filter::Flagged(Flags::REPLICATED), Box::new(f))
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
        .filter(|e| match e.filter {
            Filter::One(a) => a == address,
            Filter::All => true,
            Filter::Flagged(flags) => cvar.flags().intersects(flags),
        })
        .map(|e| e.f.clone())
        .collect();
    for f in matching {
        f(cvar);
    }
}
