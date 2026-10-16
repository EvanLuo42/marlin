use core::fmt;

/// Access allowed to a range of committed pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protection {
    NoAccess,
    Read,
    ReadWrite,
    ReadExecute,
    ReadWriteExecute,
}

/// A set of logical CPUs, indexed by [`LogicalCpu::id`](crate::sysinfo::LogicalCpu::id).
///
/// One 64-bit word per Windows processor group (bit = processor number in the group), which is
/// also the layout of Linux `cpu_set_t`. Holds [`CpuSet::CAPACITY`] CPUs, the size of `cpu_set_t`
/// and 16 Windows groups.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CpuSet {
    words: [u64; Self::WORDS],
}

impl CpuSet {
    pub const CAPACITY: usize = 1024;
    /// Number of 64-CPU words; see [`word`](Self::word).
    pub const WORDS: usize = Self::CAPACITY / 64;

    #[inline]
    pub const fn new() -> Self {
        Self {
            words: [0; Self::WORDS],
        }
    }

    /// A set holding only `cpu`.
    ///
    /// # Panics
    /// If `cpu >= CAPACITY`.
    #[inline]
    pub fn single(cpu: u32) -> Self {
        let mut set = Self::new();
        set.insert(cpu);
        set
    }

    /// # Panics
    /// If `cpu >= CAPACITY`.
    #[inline]
    pub fn insert(&mut self, cpu: u32) {
        let cpu = cpu as usize;
        assert!(cpu < Self::CAPACITY, "CPU id out of CpuSet range");
        self.words[cpu / 64] |= 1 << (cpu % 64);
    }

    #[inline]
    pub fn remove(&mut self, cpu: u32) {
        let cpu = cpu as usize;
        if cpu < Self::CAPACITY {
            self.words[cpu / 64] &= !(1 << (cpu % 64));
        }
    }

    #[inline]
    pub fn contains(&self, cpu: u32) -> bool {
        let cpu = cpu as usize;
        cpu < Self::CAPACITY && self.words[cpu / 64] & (1 << (cpu % 64)) != 0
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The CPUs in the set, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &word)| {
            let mut bits = word;
            core::iter::from_fn(move || {
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros();
                    bits &= bits - 1;
                    i as u32 * 64 + bit
                })
            })
        })
    }

    /// The bits for ids `64 * index ..= 64 * index + 63`: on Windows, processor group `index`'s
    /// affinity mask.
    ///
    /// # Panics
    /// If `index >= WORDS`.
    #[inline]
    pub fn word(&self, index: usize) -> u64 {
        self.words[index]
    }

    /// # Panics
    /// If `index >= WORDS`.
    #[inline]
    pub fn set_word(&mut self, index: usize, word: u64) {
        self.words[index] = word;
    }
}

impl fmt::Debug for CpuSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// Scheduling priority of a thread, relative to the other threads of the process.
///
/// | | Windows | Linux (nice) | macOS (QoS class) |
/// |---|---|---|---|
/// | `Background` | `LOWEST` | 10 | `BACKGROUND` (E cores only) |
/// | `Low` | `BELOW_NORMAL` | 5 | `UTILITY` |
/// | `Normal` | `NORMAL` | 0 | `DEFAULT` |
/// | `High` | `ABOVE_NORMAL` | -5 | `USER_INITIATED` |
/// | `TimeCritical` | `TIME_CRITICAL` | -10 | `USER_INTERACTIVE` |
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ThreadPriority {
    /// Work that may be delayed indefinitely: streaming prefetch, shader cache writes.
    Background,
    Low,
    #[default]
    Normal,
    /// Main and render threads.
    High,
    /// Audio mixing and other deadline-bound work. Keep the work short: on Windows this level
    /// preempts nearly everything else in the system.
    TimeCritical,
}
