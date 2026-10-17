//! Type-erased storage: columns (a `Vec<T>` whose `T` is known only by layout and drop function,
//! with each row's change ticks beside it), the tables built from them, and sparse sets.

use alloc::alloc::{alloc, dealloc, handle_alloc_error, realloc};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ptr::{self, NonNull};

use marlin_containers::{HashMap, HashMapExt};

use crate::change::{Tick, TickCell};
use crate::component::{ComponentId, ComponentInfo, Components, StorageType};
use crate::entity::Entity;

/// When a row's component was added and last written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ComponentTicks {
    pub added: Tick,
    pub changed: Tick,
}

impl ComponentTicks {
    pub(crate) const fn new(tick: Tick) -> Self {
        Self {
            added: tick,
            changed: tick,
        }
    }
}

pub(crate) struct Column {
    data: NonNull<u8>,
    /// Parallel to the rows.
    ticks: Vec<TickCell>,
    len: usize,
    capacity: usize,
    item: Layout,
    drop: Option<unsafe fn(*mut u8)>,
}

// SAFETY: columns only hold `Component`s, which are Send + Sync; access is coordinated by the
// world (exclusive borrows, or disjoint declared access in the scheduler).
unsafe impl Send for Column {}
// SAFETY: as above.
unsafe impl Sync for Column {}

impl Column {
    pub(crate) fn new(info: &ComponentInfo) -> Self {
        let item = info.layout();
        Self {
            // A well-aligned dangling pointer: the empty and zero-sized cases never touch memory.
            data: NonNull::without_provenance(
                core::num::NonZero::new(item.align()).expect("alignment is non-zero"),
            ),
            ticks: Vec::new(),
            len: 0,
            capacity: if item.size() == 0 { usize::MAX } else { 0 },
            item,
            drop: info.drop_fn(),
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) fn item_size(&self) -> usize {
        self.item.size()
    }

    /// Pointer to the item at `row`; for `row == len`, one past the end.
    #[inline]
    pub(crate) fn ptr(&self, row: usize) -> *mut u8 {
        debug_assert!(row <= self.len);
        // SAFETY: within (or one past) the allocation; zero-sized items never move the pointer.
        unsafe { self.data.as_ptr().add(row * self.item.size()) }
    }

    fn array(&self, capacity: usize) -> Layout {
        let size = self
            .item
            .size()
            .checked_mul(capacity)
            .expect("column overflow");
        Layout::from_size_align(size, self.item.align()).expect("column overflow")
    }

    /// Room for `additional` more rows.
    pub(crate) fn reserve(&mut self, additional: usize) {
        self.ticks.reserve(additional);
        let needed = self.len.checked_add(additional).expect("column overflow");
        if needed <= self.capacity {
            return;
        }
        self.grow_to(needed.max(self.capacity * 2).max(4));
    }

    #[inline]
    fn reserve_one(&mut self) {
        if self.len == self.capacity {
            self.grow_to((self.capacity * 2).max(4));
        }
    }

    #[cold]
    fn grow_to(&mut self, new_capacity: usize) {
        let new_layout = self.array(new_capacity);
        // SAFETY: non-zero size (zero-sized columns never get here: their capacity is
        // `usize::MAX`); `data` was allocated with `array(capacity)` when capacity > 0.
        let data = unsafe {
            if self.capacity == 0 {
                alloc(new_layout)
            } else {
                realloc(
                    self.data.as_ptr(),
                    self.array(self.capacity),
                    new_layout.size(),
                )
            }
        };
        self.data = NonNull::new(data).unwrap_or_else(|| handle_alloc_error(new_layout));
        self.capacity = new_capacity;
    }

    /// The row ticks, for queries.
    #[inline]
    pub(crate) fn ticks_ptr(&self) -> *const TickCell {
        self.ticks.as_ptr()
    }

    #[inline]
    pub(crate) fn ticks(&self, row: usize) -> ComponentTicks {
        self.ticks[row].read()
    }

    /// The ticks of `row` for writing.
    #[inline]
    pub(crate) fn ticks_mut(&mut self, row: usize) -> &mut ComponentTicks {
        // SAFETY: `&mut self` excludes every other access.
        unsafe { &mut *self.ticks[row].get() }
    }

    /// Clamps every row's ticks against `now` (see [`Tick::clamp`]).
    pub(crate) fn check_ticks(&mut self, now: Tick) {
        for cell in &mut self.ticks {
            // SAFETY: `&mut self` excludes every other access.
            let ticks = unsafe { &mut *cell.get() };
            ticks.added.clamp(now);
            ticks.changed.clamp(now);
        }
    }

    /// Appends the item at `src`, moving it in, with `ticks`.
    ///
    /// # Safety
    /// `src` points to a valid item of this column's type that the caller will not use or drop.
    #[inline]
    pub(crate) unsafe fn push(&mut self, src: *const u8, ticks: ComponentTicks) {
        self.reserve_one();
        self.ticks.push(TickCell::new(ticks));
        // SAFETY: room for one more; the caller hands over the item.
        unsafe { ptr::copy_nonoverlapping(src, self.ptr(self.len), self.item.size()) };
        self.len += 1;
    }

    /// Removes `row`, filling it with the last item, without dropping it: the caller has moved it
    /// out.
    ///
    /// # Safety
    /// `row < len`, and the item at `row` has been moved out.
    pub(crate) unsafe fn swap_remove_forget(&mut self, row: usize) {
        debug_assert!(row < self.len);
        let last = self.len - 1;
        if row != last {
            // SAFETY: distinct rows within the column.
            unsafe { ptr::copy_nonoverlapping(self.ptr(last), self.ptr(row), self.item.size()) };
        }
        self.ticks.swap_remove(row);
        self.len = last;
    }

    /// Drops the item at `row` and fills the hole with the last item.
    ///
    /// # Safety
    /// `row < len`.
    pub(crate) unsafe fn swap_remove_drop(&mut self, row: usize) {
        if let Some(drop) = self.drop {
            // SAFETY: the item is valid and owned by the column.
            unsafe { drop(self.ptr(row)) };
        }
        // SAFETY: the item was just dropped.
        unsafe { self.swap_remove_forget(row) };
    }

    /// Moves the item at `row` to `dst` and fills the hole with the last item.
    ///
    /// # Safety
    /// `row < len`; `dst` is writable storage for one item, which the caller then owns.
    pub(crate) unsafe fn swap_remove_into(&mut self, row: usize, dst: *mut u8) {
        // SAFETY: the item is moved out and its row removed without dropping it.
        unsafe {
            ptr::copy_nonoverlapping(self.ptr(row), dst, self.item.size());
            self.swap_remove_forget(row);
        }
    }

    /// Drops the item at `row` and moves the item at `src` into its place, marked changed at
    /// `tick`.
    ///
    /// # Safety
    /// `row < len`; `src` as for [`push`](Self::push).
    pub(crate) unsafe fn replace(&mut self, row: usize, src: *const u8, tick: Tick) {
        self.ticks_mut(row).changed = tick;
        let dst = self.ptr(row);
        if let Some(drop) = self.drop {
            // SAFETY: the item is valid and owned by the column.
            unsafe { drop(dst) };
        }
        // SAFETY: the old item is gone; the caller hands over the new one.
        unsafe { ptr::copy_nonoverlapping(src, dst, self.item.size()) };
    }
}

impl Drop for Column {
    fn drop(&mut self) {
        if let Some(drop) = self.drop {
            for row in 0..self.len {
                // SAFETY: every row below `len` holds a valid item.
                unsafe { drop(self.ptr(row)) };
            }
        }
        if self.item.size() != 0 && self.capacity != 0 {
            // SAFETY: allocated with this layout.
            unsafe { dealloc(self.data.as_ptr(), self.array(self.capacity)) };
        }
    }
}

// --- Tables ---

/// A table's index in its world.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TableId(pub(crate) u32);

impl TableId {
    /// The table without columns.
    pub const EMPTY: Self = Self(0);
    pub(crate) const INVALID: Self = Self(u32::MAX);

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The rows of every entity whose table-stored components are the same set: one column per
/// component. Archetypes that differ only in sparse-set components share a table.
pub struct Table {
    /// Sorted.
    pub(crate) components: Box<[ComponentId]>,
    /// Parallel to `components`.
    pub(crate) columns: Box<[Column]>,
    pub(crate) entities: Vec<Entity>,
    /// How many archetypes use this table. With one, the archetype's rows are the table's rows in
    /// the same order, which lets queries skip the row indirection.
    pub(crate) archetypes: u32,
}

impl Table {
    #[inline]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Table-stored component types, sorted by id.
    #[inline]
    pub fn components(&self) -> &[ComponentId] {
        &self.components
    }

    #[inline]
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    #[inline]
    pub(crate) fn column_index(&self, id: ComponentId) -> Option<usize> {
        self.components.binary_search(&id).ok()
    }

    #[inline]
    pub(crate) fn column(&self, id: ComponentId) -> Option<&Column> {
        self.column_index(id).map(|i| &self.columns[i])
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.entities.reserve(additional);
        for column in self.columns.iter_mut() {
            column.reserve(additional);
        }
    }
}

pub(crate) struct Tables {
    pub(crate) list: Vec<Table>,
    by_components: HashMap<Box<[ComponentId]>, TableId>,
}

impl Tables {
    pub(crate) fn new() -> Self {
        let mut tables = Self {
            list: Vec::new(),
            by_components: HashMap::new(),
        };
        let empty = tables.get_or_create(&[], &Components::new());
        debug_assert_eq!(empty, TableId::EMPTY);
        tables
    }

    /// The table for exactly these (sorted, deduplicated, table-stored) components.
    pub(crate) fn get_or_create(
        &mut self,
        components: &[ComponentId],
        registry: &Components,
    ) -> TableId {
        if let Some(&id) = self.by_components.get(components) {
            return id;
        }
        let id = TableId(u32::try_from(self.list.len()).expect("too many tables"));
        self.list.push(Table {
            components: components.into(),
            columns: components
                .iter()
                .map(|&c| Column::new(registry.info(c)))
                .collect(),
            entities: Vec::new(),
            archetypes: 0,
        });
        self.by_components.insert(components.into(), id);
        id
    }

    /// Two distinct tables, mutably.
    pub(crate) fn pair_mut(&mut self, a: TableId, b: TableId) -> (&mut Table, &mut Table) {
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

// --- Sparse sets ---

const NO_ROW: u32 = u32::MAX;

/// One sparse-set component's values for the whole world: dense rows, plus a map from entity
/// index to row.
pub(crate) struct SparseSet {
    pub(crate) dense: Column,
    /// Parallel to the rows.
    entities: Vec<Entity>,
    /// By entity index: its row, or [`NO_ROW`].
    sparse: Vec<u32>,
}

impl SparseSet {
    fn new(info: &ComponentInfo) -> Self {
        Self {
            dense: Column::new(info),
            entities: Vec::new(),
            sparse: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn row(&self, entity: Entity) -> Option<usize> {
        let row = *self.sparse.get(entity.index() as usize)?;
        (row != NO_ROW && self.entities[row as usize] == entity).then_some(row as usize)
    }

    /// Adds the value at `src` for an entity that has none.
    ///
    /// # Safety
    /// As for [`Column::push`].
    pub(crate) unsafe fn insert_new(
        &mut self,
        entity: Entity,
        src: *const u8,
        ticks: ComponentTicks,
    ) {
        debug_assert!(self.row(entity).is_none());
        let index = entity.index() as usize;
        if self.sparse.len() <= index {
            self.sparse.resize(index + 1, NO_ROW);
        }
        self.sparse[index] = self.dense.len() as u32;
        self.entities.push(entity);
        // SAFETY: forwarded from the caller.
        unsafe { self.dense.push(src, ticks) };
    }

    /// Drops the entity's value and moves the one at `src` into its place, marked changed at
    /// `tick`.
    ///
    /// # Safety
    /// The entity has a value; `src` as for [`Column::push`].
    pub(crate) unsafe fn replace(&mut self, entity: Entity, src: *const u8, tick: Tick) {
        let row = self.row(entity).expect("replaced sparse component exists");
        // SAFETY: forwarded from the caller.
        unsafe { self.dense.replace(row, src, tick) };
    }

    /// Removes the entity's row, then fixes the row of the entity swapped into its place.
    fn unlink(&mut self, entity: Entity, row: usize) {
        self.sparse[entity.index() as usize] = NO_ROW;
        self.entities.swap_remove(row);
        if let Some(&moved) = self.entities.get(row) {
            self.sparse[moved.index() as usize] = row as u32;
        }
    }

    /// Drops the entity's value; false if it had none.
    pub(crate) fn remove_drop(&mut self, entity: Entity) -> bool {
        let Some(row) = self.row(entity) else {
            return false;
        };
        // SAFETY: `row` holds the entity's value.
        unsafe { self.dense.swap_remove_drop(row) };
        self.unlink(entity, row);
        true
    }

    /// Moves the entity's value to `dst`; false if it had none.
    ///
    /// # Safety
    /// As for [`Column::swap_remove_into`].
    pub(crate) unsafe fn remove_into(&mut self, entity: Entity, dst: *mut u8) -> bool {
        let Some(row) = self.row(entity) else {
            return false;
        };
        // SAFETY: forwarded from the caller.
        unsafe { self.dense.swap_remove_into(row, dst) };
        self.unlink(entity, row);
        true
    }
}

/// Every sparse set of a world, by component id.
#[derive(Default)]
pub(crate) struct SparseSets {
    sets: Vec<Option<SparseSet>>,
}

impl SparseSets {
    #[inline]
    pub(crate) fn get(&self, id: ComponentId) -> Option<&SparseSet> {
        self.sets.get(id.index()).and_then(Option::as_ref)
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, id: ComponentId) -> Option<&mut SparseSet> {
        self.sets.get_mut(id.index()).and_then(Option::as_mut)
    }

    /// The set of a sparse-set component, created on first use.
    pub(crate) fn get_or_create(&mut self, info: &ComponentInfo) -> &mut SparseSet {
        debug_assert_eq!(info.storage(), StorageType::SparseSet);
        let index = info.id().index();
        if self.sets.len() <= index {
            self.sets.resize_with(index + 1, || None);
        }
        self.sets[index].get_or_insert_with(|| SparseSet::new(info))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut SparseSet> {
        self.sets.iter_mut().flatten()
    }
}
