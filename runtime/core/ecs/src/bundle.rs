//! Bundles: tuples of components spawned together.

use core::mem::ManuallyDrop;

use crate::component::{Component, ComponentId, Components};

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
