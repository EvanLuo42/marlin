//! Archetypes: one table per distinct set of component types.

use alloc::boxed::Box;
use alloc::vec::Vec;

use marlin_containers::{HashMap, HashMapExt};

use crate::component::{ComponentId, Components};
use crate::entity::Entity;
use crate::storage::Column;

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

/// Entities sharing one set of component types, stored as one column per component (struct of
/// arrays): row `i` of every column belongs to `entities()[i]`.
pub struct Archetype {
    id: ArchetypeId,
    /// Sorted.
    pub(crate) components: Box<[ComponentId]>,
    /// Parallel to `components`.
    pub(crate) columns: Box<[Column]>,
    pub(crate) entities: Vec<Entity>,
    /// Cached transitions: the archetype reached by adding or removing one component.
    pub(crate) add_edges: HashMap<ComponentId, ArchetypeId>,
    pub(crate) remove_edges: HashMap<ComponentId, ArchetypeId>,
}

impl Archetype {
    #[inline]
    pub fn id(&self) -> ArchetypeId {
        self.id
    }

    /// Component types, sorted by id.
    #[inline]
    pub fn components(&self) -> &[ComponentId] {
        &self.components
    }

    #[inline]
    pub fn entities(&self) -> &[Entity] {
        &self.entities
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
        self.column_index(id).is_some()
    }

    #[inline]
    pub(crate) fn column_index(&self, id: ComponentId) -> Option<usize> {
        self.components.binary_search(&id).ok()
    }

    #[inline]
    pub(crate) fn column(&self, id: ComponentId) -> Option<&Column> {
        self.column_index(id).map(|i| &self.columns[i])
    }
}

pub(crate) struct Archetypes {
    pub(crate) list: Vec<Archetype>,
    by_components: HashMap<Box<[ComponentId]>, ArchetypeId>,
}

impl Archetypes {
    pub(crate) fn new() -> Self {
        let mut archetypes = Self {
            list: Vec::new(),
            by_components: HashMap::new(),
        };
        let empty = archetypes.get_or_create(&[], &Components::new());
        debug_assert_eq!(empty, ArchetypeId::EMPTY);
        archetypes
    }

    /// The archetype for exactly these (sorted, deduplicated) components.
    pub(crate) fn get_or_create(
        &mut self,
        components: &[ComponentId],
        registry: &Components,
    ) -> ArchetypeId {
        debug_assert!(components.windows(2).all(|w| w[0] < w[1]));
        if let Some(&id) = self.by_components.get(components) {
            return id;
        }
        let id = ArchetypeId(u32::try_from(self.list.len()).expect("too many archetypes"));
        self.list.push(Archetype {
            id,
            components: components.into(),
            columns: components
                .iter()
                .map(|&c| Column::new(registry.info(c)))
                .collect(),
            entities: Vec::new(),
            add_edges: HashMap::new(),
            remove_edges: HashMap::new(),
        });
        self.by_components.insert(components.into(), id);
        id
    }

    /// The archetype reached from `from` by adding `component` (which it lacks).
    pub(crate) fn with_added(
        &mut self,
        from: ArchetypeId,
        component: ComponentId,
        registry: &Components,
    ) -> ArchetypeId {
        if let Some(&to) = self.list[from.index()].add_edges.get(&component) {
            return to;
        }
        let mut set: Vec<ComponentId> = self.list[from.index()].components.to_vec();
        let at = set.binary_search(&component).unwrap_err();
        set.insert(at, component);
        let to = self.get_or_create(&set, registry);
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
        let to = self.get_or_create(&set, registry);
        self.list[from.index()].remove_edges.insert(component, to);
        self.list[to.index()].add_edges.insert(component, from);
        to
    }

    /// Two distinct archetypes, mutably.
    pub(crate) fn pair_mut(
        &mut self,
        a: ArchetypeId,
        b: ArchetypeId,
    ) -> (&mut Archetype, &mut Archetype) {
        assert_ne!(a, b);
        let (a, b) = (a.index(), b.index());
        if a < b {
            let (lo, hi) = self.list.split_at_mut(b);
            (&mut lo[a], &mut hi[0])
        } else {
            let (lo, hi) = self.list.split_at_mut(a);
            (&mut hi[0], &mut lo[b])
        }
    }
}
