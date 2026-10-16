//! Startup check that the CPU has the instruction sets the build was compiled for.
//!
//! Desktop x86-64 builds target x86-64-v3 (AVX2, FMA, BMI1/2, ...; see
//! platforms/windows/platform.toml). On an older CPU the first such instruction would crash with
//! an illegal-instruction fault; this check reports which features are missing instead.
//!
//! Best effort: this code is itself compiled for the same CPU, and while it only does integer work,
//! nothing stops the compiler from using an AVX instruction before the message is out. A
//! guaranteed message needs a launcher built for baseline x86-64.

use core::fmt;

/// Instruction sets the build checks for, in [`MissingFeatures`] bit order.
const NAMES: [&str; 12] = [
    "SSE3",
    "SSSE3",
    "SSE4.1",
    "SSE4.2",
    "POPCNT",
    "MOVBE",
    "AVX",
    "FMA",
    "F16C",
    "AVX2",
    "BMI1/BMI2",
    "LZCNT",
];

/// The instruction sets the build needs that this CPU (or OS) lacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissingFeatures(u16);

impl MissingFeatures {
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        let bits = self.0;
        NAMES
            .iter()
            .enumerate()
            .filter(move |(i, _)| bits & (1 << i) != 0)
            .map(|(_, name)| *name)
    }
}

impl fmt::Display for MissingFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("this CPU lacks instruction sets this build requires:")?;
        for name in self.names() {
            write!(f, " {name}")?;
        }
        f.write_str(" (x86-64-v3: Intel Haswell / AMD Zen or newer)")
    }
}

/// Checks every instruction set the build enables. Always succeeds on non-x86 targets.
pub fn check() -> Result<(), MissingFeatures> {
    #[cfg_attr(not(target_arch = "x86_64"), allow(unused_mut))]
    let mut missing = MissingFeatures::default();
    #[cfg(target_arch = "x86_64")]
    x86::check(&mut missing);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    use super::MissingFeatures;

    pub(super) fn check(missing: &mut MissingFeatures) {
        let max_leaf = __cpuid(0).eax;
        let leaf1 = __cpuid(1);
        let leaf7 = if max_leaf >= 7 {
            __cpuid_count(7, 0)
        } else {
            __cpuid_count(0, 0)
        };
        let max_ext = __cpuid(0x8000_0000).eax;
        let ext1 = if max_ext >= 0x8000_0001 {
            __cpuid(0x8000_0001)
        } else {
            __cpuid(0x8000_0000)
        };
        let has_leaf7 = max_leaf >= 7;
        let has_ext1 = max_ext >= 0x8000_0001;

        // AVX needs the OS to save YMM state: OSXSAVE set and XCR0 enabling SSE and AVX state.
        let os_avx = leaf1.ecx & (1 << 27) != 0 && {
            // SAFETY: OSXSAVE is set, so XGETBV is available.
            let xcr0 = unsafe { core::arch::x86_64::_xgetbv(0) };
            xcr0 & 0b110 == 0b110
        };

        let bit = |reg: u32, n: u32| reg & (1 << n) != 0;
        // In `NAMES` order.
        let checks: [(bool, bool); 12] = [
            (cfg!(target_feature = "sse3"), bit(leaf1.ecx, 0)),
            (cfg!(target_feature = "ssse3"), bit(leaf1.ecx, 9)),
            (cfg!(target_feature = "sse4.1"), bit(leaf1.ecx, 19)),
            (cfg!(target_feature = "sse4.2"), bit(leaf1.ecx, 20)),
            (cfg!(target_feature = "popcnt"), bit(leaf1.ecx, 23)),
            (cfg!(target_feature = "movbe"), bit(leaf1.ecx, 22)),
            (cfg!(target_feature = "avx"), bit(leaf1.ecx, 28) && os_avx),
            (cfg!(target_feature = "fma"), bit(leaf1.ecx, 12) && os_avx),
            (cfg!(target_feature = "f16c"), bit(leaf1.ecx, 29)),
            (
                cfg!(target_feature = "avx2"),
                has_leaf7 && bit(leaf7.ebx, 5) && os_avx,
            ),
            (
                cfg!(target_feature = "bmi2"),
                has_leaf7 && bit(leaf7.ebx, 3) && bit(leaf7.ebx, 8),
            ),
            (cfg!(target_feature = "lzcnt"), has_ext1 && bit(ext1.ecx, 5)),
        ];
        for (i, (required, present)) in checks.into_iter().enumerate() {
            if required && !present {
                missing.0 |= 1 << i;
            }
        }
    }
}
