//! A ring of arenas for overlapping frames.

use super::Arena;

/// `N` arenas used in turn by frames `0, 1, 2, …`: frame `f` allocates from arena `f % N`.
///
/// Frames overlap: frame N's simulation writes its arena while the renderer still reads frame
/// N - 1's. With `N = 3`, an arena is reused three frames later, once the pipeline
/// guarantees its readers are done (in the engine, the job handle of the frame that consumed it).
pub struct FrameArenas<const N: usize = 3> {
    arenas: [Arena; N],
}

impl<const N: usize> FrameArenas<N> {
    /// `N` arenas reserving `capacity` bytes each.
    pub fn new(capacity: usize) -> marlin_pal::Result<Self> {
        const { assert!(N > 0) };
        let arenas: [marlin_pal::Result<Arena>; N] = core::array::from_fn(|_| Arena::new(capacity));
        if let Some(error) = arenas.iter().find_map(|a| a.as_ref().err()) {
            return Err(*error);
        }
        Ok(Self {
            arenas: arenas.map(|a| a.expect("checked above")),
        })
    }

    /// Frame `frame`'s arena.
    #[inline]
    pub fn get(&self, frame: u64) -> &Arena {
        &self.arenas[(frame % N as u64) as usize]
    }

    /// Resets frame `frame`'s arena, the one frame `frame - N` used, and returns it.
    ///
    /// # Safety
    /// Nothing allocated in frame `frame - N` is used any more, and no thread allocates from that
    /// arena concurrently.
    pub unsafe fn begin(&self, frame: u64) -> &Arena {
        let arena = self.get(frame);
        // SAFETY: forwarded from the caller.
        unsafe { arena.reset_shared() };
        arena
    }

    /// Resets every arena.
    pub fn reset(&mut self) {
        self.arenas.iter_mut().for_each(Arena::reset);
    }

    /// Bytes committed across the ring.
    pub fn committed(&self) -> usize {
        self.arenas.iter().map(Arena::committed).sum()
    }

    /// [`Arena::trim`] on each arena.
    pub fn trim(&mut self, keep: Option<usize>) {
        self.arenas.iter_mut().for_each(|a| a.trim(keep));
    }
}

impl<const N: usize> core::fmt::Debug for FrameArenas<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(&self.arenas).finish()
    }
}
