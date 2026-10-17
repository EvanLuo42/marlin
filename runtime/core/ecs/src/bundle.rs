//! Bundles: tuples of components spawned or inserted together, and what the world caches about
//! each bundle type.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::TypeId;
use core::mem::ManuallyDrop;

use marlin_containers::{HashMap, HashMapExt};

use crate::archetype::{ArchetypeId, Archetypes};
use crate::component::{Component, ComponentId, Components, StorageType};
use crate::storage::Tables;

/// Most components one bundle may hold.
pub const MAX_BUNDLE: usize = 12;

/// A tuple of distinct component types, e.g. `(Position, Velocity)` or `(Name,)`.
///
/// # Safety
///
/// `take` must hand each component over exactly once, in the order of `register`'s ids.
pub unsafe trait Bundle: Send + Sync + 'static {
    const COUNT: usize;

    /// Writes the component ids, in tuple order, into `ids[..COUNT]`.
    fn register(components: &mut Components, ids: &mut [ComponentId]);

    /// Moves the components out: `f(i, ptr)` for the `i`-th component, which the callee must
    /// take ownership of.
    ///
    /// # Safety
    /// `f` takes ownership of every pointee (moves the bytes out and never reads `ptr` again).
    unsafe fn take(self, f: &mut dyn FnMut(usize, *mut u8));
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct BundleId(u32);

/// Where one bundle component goes in the target archetype.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    /// This column of the target table.
    Column(u32),
    /// The world sparse set of this component.
    Sparse(ComponentId),
}

/// Inserting a bundle into entities of one archetype: cached per (archetype, bundle).
pub(crate) struct BundleEdge {
    pub(crate) target: ArchetypeId,
    /// Per bundle component, in tuple order: whether the source archetype lacked it.
    pub(crate) added: Box<[bool]>,
    /// Per bundle component, in tuple order.
    pub(crate) targets: Box<[Target]>,
}

pub(crate) struct BundleInfo {
    /// In tuple order.
    pub(crate) components: Box<[ComponentId]>,
    /// The edge from the empty archetype, used by spawns.
    spawn: Option<u32>,
}

/// The bundle cache: component ids per bundle type, and the archetype transitions they cause.
pub(crate) struct Bundles {
    pub(crate) infos: Vec<BundleInfo>,
    by_type: HashMap<TypeId, BundleId>,
    /// Bundles of components named by id, keyed by their ids in order.
    by_ids: HashMap<Box<[ComponentId]>, BundleId>,
    pub(crate) edges: Vec<BundleEdge>,
}

impl Bundles {
    pub(crate) fn new() -> Self {
        Self {
            infos: Vec::new(),
            by_type: HashMap::new(),
            by_ids: HashMap::new(),
            edges: Vec::new(),
        }
    }

    /// The bundle of exactly these components in this order, registering it on first use.
    ///
    /// # Panics
    /// If a component appears twice.
    pub(crate) fn register_dynamic(&mut self, ids: &[ComponentId]) -> BundleId {
        if let Some(&id) = self.by_ids.get(ids) {
            return id;
        }
        let id = self.register_ids(ids);
        self.by_ids.insert(ids.into(), id);
        id
    }

    /// The bundle's id, registering it and its components on first use.
    ///
    /// # Panics
    /// If the bundle names a component type twice.
    #[inline]
    pub(crate) fn register<B: Bundle>(&mut self, components: &mut Components) -> BundleId {
        if let Some(&id) = self.by_type.get(&TypeId::of::<B>()) {
            return id;
        }
        const { assert!(B::COUNT <= MAX_BUNDLE) };
        let mut ids = [ComponentId(0); MAX_BUNDLE];
        B::register(components, &mut ids[..B::COUNT]);
        let id = self.register_ids(&ids[..B::COUNT]);
        self.by_type.insert(TypeId::of::<B>(), id);
        id
    }

    /// An anonymous bundle of these components, in this order.
    ///
    /// # Panics
    /// If a component appears twice.
    pub(crate) fn register_ids(&mut self, ids: &[ComponentId]) -> BundleId {
        let mut sorted: Vec<ComponentId> = ids.to_vec();
        sorted.sort_unstable();
        assert!(
            sorted.windows(2).all(|w| w[0] != w[1]),
            "a bundle names a component type twice"
        );
        let id = BundleId(u32::try_from(self.infos.len()).expect("too many bundle types"));
        self.infos.push(BundleInfo {
            components: ids.into(),
            spawn: None,
        });
        id
    }

    #[inline]
    pub(crate) fn components(&self, id: BundleId) -> &[ComponentId] {
        &self.infos[id.0 as usize].components
    }

    /// The edge for inserting bundle `id` into entities of archetype `from`.
    pub(crate) fn edge(
        &mut self,
        id: BundleId,
        from: ArchetypeId,
        archetypes: &mut Archetypes,
        components: &Components,
        tables: &mut Tables,
    ) -> u32 {
        if from == ArchetypeId::EMPTY {
            if let Some(edge) = self.infos[id.0 as usize].spawn {
                return edge;
            }
        } else if let Some(&edge) = archetypes.list[from.index()].bundle_edges.get(&id) {
            return edge;
        }
        let ids = &self.infos[id.0 as usize].components;
        let source = &archetypes.list[from.index()];
        let added: Box<[bool]> = ids.iter().map(|&c| !source.contains(c)).collect();
        let target = archetypes.with_added_many(from, ids, components, tables);
        let table = &tables.list[archetypes.list[target.index()].table.index()];
        let targets = ids
            .iter()
            .map(|&c| match components.info(c).storage() {
                StorageType::Table => {
                    Target::Column(table.column_index(c).expect("bundle column") as u32)
                }
                StorageType::SparseSet => Target::Sparse(c),
            })
            .collect();
        let edge = u32::try_from(self.edges.len()).expect("too many bundle edges");
        self.edges.push(BundleEdge {
            target,
            added,
            targets,
        });
        if from == ArchetypeId::EMPTY {
            self.infos[id.0 as usize].spawn = Some(edge);
        } else {
            archetypes.list[from.index()].bundle_edges.insert(id, edge);
        }
        edge
    }
}

macro_rules! tuple_bundle {
    ($(($T:ident, $i:tt)),+) => {
        // SAFETY: each field is handed over once, in tuple order, and never dropped here.
        unsafe impl<$($T: Component),+> Bundle for ($($T,)+) {
            const COUNT: usize = [$($i),+].len();

            fn register(components: &mut Components, ids: &mut [ComponentId]) {
                $(ids[$i] = components.register::<$T>();)+
            }

            unsafe fn take(self, f: &mut dyn FnMut(usize, *mut u8)) {
                let mut this = ManuallyDrop::new(self);
                $(f($i, (&raw mut this.$i).cast());)+
            }
        }
    };
}

tuple_bundle!((A, 0));
tuple_bundle!((A, 0), (B, 1));
tuple_bundle!((A, 0), (B, 1), (C, 2));
tuple_bundle!((A, 0), (B, 1), (C, 2), (D, 3));
tuple_bundle!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4));
tuple_bundle!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5));
tuple_bundle!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5), (G, 6));
tuple_bundle!(
    (A, 0),
    (B, 1),
    (C, 2),
    (D, 3),
    (E, 4),
    (F, 5),
    (G, 6),
    (H, 7)
);
tuple_bundle!(
    (A, 0),
    (B, 1),
    (C, 2),
    (D, 3),
    (E, 4),
    (F, 5),
    (G, 6),
    (H, 7),
    (I, 8)
);
tuple_bundle!(
    (A, 0),
    (B, 1),
    (C, 2),
    (D, 3),
    (E, 4),
    (F, 5),
    (G, 6),
    (H, 7),
    (I, 8),
    (J, 9)
);
tuple_bundle!(
    (A, 0),
    (B, 1),
    (C, 2),
    (D, 3),
    (E, 4),
    (F, 5),
    (G, 6),
    (H, 7),
    (I, 8),
    (J, 9),
    (K, 10)
);
tuple_bundle!(
    (A, 0),
    (B, 1),
    (C, 2),
    (D, 3),
    (E, 4),
    (F, 5),
    (G, 6),
    (H, 7),
    (I, 8),
    (J, 9),
    (K, 10),
    (L, 11)
);
