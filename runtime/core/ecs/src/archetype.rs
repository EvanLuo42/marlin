//! Archetypes: one per distinct set of component types.

use alloc::boxed::Box;
use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};

use crate::bundle::BundleId;
use crate::component::{ComponentId, Components, StorageType};
use crate::entity::Entity;
use crate::storage::{TableId, Tables};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ArchetypeId(pub(crate) u32);

impl ArchetypeId {
    /// The archetype of entities without components.
    pub const EMPTY: Self = Self(0);
    pub(crate) const INVALID: Self = Self(u32::MAX);

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Entities sharing one set of component types. Their table-stored components live in the
/// archetype's [`Table`](crate::Table) (shared with archetypes that differ only in sparse-set
/// components); `entities()[i]` is at table row `table_rows()[i]`.
pub struct Archetype {
    id: ArchetypeId,
    pub(crate) table: TableId,
    /// Every component, table-stored or not. Sorted.
    pub(crate) components: Box<[ComponentId]>,
    /// The sparse-set ones. Sorted.
    pub(crate) sparse: Box<[ComponentId]>,
    pub(crate) entities: Vec<Entity>,
    /// Parallel to `entities`.
    pub(crate) table_rows: Vec<u32>,
    /// Cached transitions: the archetype reached by adding or removing one component.
    pub(crate) add_edges: HashMap<ComponentId, ArchetypeId>,
    pub(crate) remove_edges: HashMap<ComponentId, ArchetypeId>,
    /// Cached bundle insertions: index into [`Bundles::edges`](crate::bundle::Bundles).
    pub(crate) bundle_edges: HashMap<BundleId, u32>,
}

impl Archetype {
    #[inline]
    pub fn id(&self) -> ArchetypeId {
        self.id
    }

    #[inline]
    pub fn table_id(&self) -> TableId {
        self.table
    }

    /// Component types, sorted by id.
    #[inline]
    pub fn components(&self) -> &[ComponentId] {
        &self.components
    }

    /// The sparse-set component types, sorted by id.
    #[inline]
    pub fn sparse_components(&self) -> &[ComponentId] {
        &self.sparse
    }

    #[inline]
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// The table row of each entity.
    #[inline]
    pub fn table_rows(&self) -> &[u32] {
        &self.table_rows
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    #[inline]
    pub fn contains(&self, id: ComponentId) -> bool {
        self.components.binary_search(&id).is_ok()
    }

    /// Appends an entity at `table_row`, returning its archetype row.
    #[inline]
    pub(crate) fn push(&mut self, entity: Entity, table_row: u32) -> u32 {
        let row = self.entities.len() as u32;
        self.entities.push(entity);
        self.table_rows.push(table_row);
        row
    }
}

pub(crate) struct Archetypes {
    pub(crate) list: Vec<Archetype>,
    by_components: HashMap<Box<[ComponentId]>, ArchetypeId>,
}

impl Archetypes {
    pub(crate) fn new(tables: &mut Tables) -> Self {
        let mut archetypes = Self {
            list: Vec::new(),
            by_components: HashMap::new(),
        };
        let empty = archetypes.get_or_create(&[], &Components::new(), tables);
        debug_assert_eq!(empty, ArchetypeId::EMPTY);
        archetypes
    }

    /// How many archetypes exist. Archetypes are never removed, so this only grows: a query that
    /// has looked at the first `generation()` archetypes only needs to look at the rest.
    #[inline]
    pub(crate) fn generation(&self) -> usize {
        self.list.len()
    }

    /// The archetype for exactly these (sorted, deduplicated) components.
    pub(crate) fn get_or_create(
        &mut self,
        components: &[ComponentId],
        registry: &Components,
        tables: &mut Tables,
    ) -> ArchetypeId {
        debug_assert!(components.windows(2).all(|w| w[0] < w[1]));
        if let Some(&id) = self.by_components.get(components) {
            return id;
        }
        let (sparse, dense): (Vec<ComponentId>, Vec<ComponentId>) = components
            .iter()
            .partition(|&&c| registry.info(c).storage() == StorageType::SparseSet);
        let table = tables.get_or_create(&dense, registry);
        tables.list[table.index()].archetypes += 1;
        let id = ArchetypeId(u32::try_from(self.list.len()).expect("too many archetypes"));
        self.list.push(Archetype {
            id,
            table,
            components: components.into(),
            sparse: sparse.into(),
            entities: Vec::new(),
            table_rows: Vec::new(),
            add_edges: HashMap::new(),
            remove_edges: HashMap::new(),
            bundle_edges: HashMap::new(),
        });
        self.by_components.insert(components.into(), id);
        id
    }

    /// The archetype reached from `from` by adding `components` (some of which it may have).
    pub(crate) fn with_added_many(
        &mut self,
        from: ArchetypeId,
        components: &[ComponentId],
        registry: &Components,
        tables: &mut Tables,
    ) -> ArchetypeId {
        if let [one] = components {
            if self.list[from.index()].contains(*one) {
                return from;
            }
            return self.with_added(from, *one, registry, tables);
        }
        let mut set: Vec<ComponentId> = self.list[from.index()].components.to_vec();
        for &c in components {
            if let Err(at) = set.binary_search(&c) {
                set.insert(at, c);
            }
        }
        self.get_or_create(&set, registry, tables)
    }

    /// The archetype reached from `from` by adding `component` (which it lacks).
    pub(crate) fn with_added(
        &mut self,
        from: ArchetypeId,
        component: ComponentId,
        registry: &Components,
        tables: &mut Tables,
    ) -> ArchetypeId {
        if let Some(&to) = self.list[from.index()].add_edges.get(&component) {
            return to;
        }
        let mut set: Vec<ComponentId> = self.list[from.index()].components.to_vec();
        let at = set.binary_search(&component).unwrap_err();
        set.insert(at, component);
        let to = self.get_or_create(&set, registry, tables);
        self.list[from.index()].add_edges.insert(component, to);
        self.list[to.index()].remove_edges.insert(component, from);
        to
    }

    /// The archetype reached from `from` by removing `component` (which it has).
    pub(crate) fn with_removed(
        &mut self,
        from: ArchetypeId,
        component: ComponentId,
        registry: &Components,
        tables: &mut Tables,
    ) -> ArchetypeId {
        if let Some(&to) = self.list[from.index()].remove_edges.get(&component) {
            return to;
        }
        let set: Vec<ComponentId> = self.list[from.index()]
            .components
            .iter()
            .copied()
            .filter(|&c| c != component)
            .collect();
        let to = self.get_or_create(&set, registry, tables);
        self.list[from.index()].remove_edges.insert(component, to);
        self.list[to.index()].add_edges.insert(component, from);
        to
    }
}
