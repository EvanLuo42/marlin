//! What a pass's recording closure gets (0007 §3.2). Recording itself comes with the frame
//! context; until then the graph declares and compiles, and closures are kept but not run.

use core::marker::PhantomData;

use crate::backend::{Backend, Rhi};

/// Borrows the frame for `'a`; names the backend without owning one.
type Marker<'a, B> = PhantomData<(&'a mut (), fn() -> B)>;

/// Records one pass: resolves the pass's handles to bindless indices and issues its commands.
/// Barriers and render pass boundaries are the graph's.
pub struct PassContext<'a, B: Backend = Rhi> {
    _marker: Marker<'a, B>,
}
