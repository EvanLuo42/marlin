//! Type-erased columns: a `Vec<T>` whose `T` is known only by layout and drop function, with each
//! row's change ticks beside it.

use alloc::alloc::{alloc, dealloc, handle_alloc_error, realloc};
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ptr::{self, NonNull};

use crate::change::{Tick, TickCell};
use crate::component::ComponentInfo;

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

    fn reserve_one(&mut self) {
        if self.len < self.capacity {
            return;
        }
        let new_capacity = (self.capacity * 2).max(4);
        let new_layout = self.array(new_capacity);
        // SAFETY: non-zero size (zero-sized columns never get here); `data` was allocated with
        // `array(capacity)` when capacity > 0.
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
