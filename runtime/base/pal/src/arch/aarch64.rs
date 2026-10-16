//! AArch64 fiber context switching.
//!
//! Callee-saved under AAPCS64: x19-x28, fp (x29), lr (x30) and the low halves of v8-v15 (d8-d15).
//! x18 is left alone: it is the platform register on Apple and the TEB pointer on Windows.

/// Frame of `aapcs::switch`, from the saved stack pointer up (in u64 slots): x19 (entry), x20
/// (arg), x21-x28, fp, lr (return address), d8-d15.
const AAPCS_FRAME: usize = 0xA0;

/// AAPCS64 (Linux, macOS, most consoles).
#[cfg(not(windows))]
pub(crate) mod aapcs {
    use core::ffi::c_void;

    use super::AAPCS_FRAME;
    use crate::fiber::{Context, Entry, Stack};

    /// Builds the frame `switch` pops when first switching to the context.
    ///
    /// # Safety
    /// `stack` stays alive while the context may run.
    pub(crate) unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
        let top = stack.top().map_addr(|a| a & !15);
        // SAFETY: the frame lies in the committed top of the stack.
        unsafe {
            let frame = top.sub(AAPCS_FRAME + 0x10).cast::<u64>();
            for i in 0..AAPCS_FRAME / 8 {
                frame.add(i).write(0);
            }
            frame.write(entry as usize as u64);
            frame.add(1).write(arg as u64);
            frame.add(11).write((trampoline as *const ()).addr() as u64);
            Context(frame.cast())
        }
    }

    /// # Safety
    /// `to` is a context made by `make_context` or saved by `switch`, not running anywhere.
    #[unsafe(naked)]
    pub(crate) unsafe extern "C" fn switch(save: *mut Context, to: Context) {
        core::arch::naked_asm!(
            "sub sp, sp, #0xA0",
            "stp x19, x20, [sp, #0x00]",
            "stp x21, x22, [sp, #0x10]",
            "stp x23, x24, [sp, #0x20]",
            "stp x25, x26, [sp, #0x30]",
            "stp x27, x28, [sp, #0x40]",
            "stp x29, x30, [sp, #0x50]",
            "stp d8, d9, [sp, #0x60]",
            "stp d10, d11, [sp, #0x70]",
            "stp d12, d13, [sp, #0x80]",
            "stp d14, d15, [sp, #0x90]",
            "mov x2, sp",
            "str x2, [x0]",
            "mov sp, x1",
            "ldp x19, x20, [sp, #0x00]",
            "ldp x21, x22, [sp, #0x10]",
            "ldp x23, x24, [sp, #0x20]",
            "ldp x25, x26, [sp, #0x30]",
            "ldp x27, x28, [sp, #0x40]",
            "ldp x29, x30, [sp, #0x50]",
            "ldp d8, d9, [sp, #0x60]",
            "ldp d10, d11, [sp, #0x70]",
            "ldp d12, d13, [sp, #0x80]",
            "ldp d14, d15, [sp, #0x90]",
            "add sp, sp, #0xA0",
            "ret",
        )
    }

    /// First code a fresh context runs: `entry(arg)`, which must never return.
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("mov x0, x20", "blr x19", "brk #0x1")
    }
}

/// Windows on Arm: AAPCS64 plus the TEB's stack bounds (x18 points to the TEB), which stack probes
/// and exception dispatch check.
#[cfg(windows)]
pub(crate) mod win {
    use core::ffi::c_void;

    use super::AAPCS_FRAME;
    use crate::fiber::{Context, Entry, Stack};

    /// AAPCS frame, then TEB StackBase, StackLimit, DeallocationStack and padding.
    const FRAME: usize = AAPCS_FRAME + 0x20;

    /// Builds the frame `switch` pops when first switching to the context.
    ///
    /// # Safety
    /// `stack` stays alive while the context may run.
    pub(crate) unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
        let top = stack.top().map_addr(|a| a & !15);
        // SAFETY: the frame lies in the committed top of the stack.
        unsafe {
            let frame = top.sub(FRAME + 0x10).cast::<u64>();
            for i in 0..FRAME / 8 {
                frame.add(i).write(0);
            }
            frame.write(entry as usize as u64);
            frame.add(1).write(arg as u64);
            frame.add(11).write((trampoline as *const ()).addr() as u64);
            frame.add(20).write(stack.top() as u64);
            frame.add(21).write(stack.limit() as u64);
            frame.add(22).write(stack.base() as u64);
            Context(frame.cast())
        }
    }

    /// # Safety
    /// `to` is a context made by `make_context` or saved by `switch`, not running anywhere.
    #[unsafe(naked)]
    pub(crate) unsafe extern "C" fn switch(save: *mut Context, to: Context) {
        core::arch::naked_asm!(
            "sub sp, sp, #0xC0",
            "stp x19, x20, [sp, #0x00]",
            "stp x21, x22, [sp, #0x10]",
            "stp x23, x24, [sp, #0x20]",
            "stp x25, x26, [sp, #0x30]",
            "stp x27, x28, [sp, #0x40]",
            "stp x29, x30, [sp, #0x50]",
            "stp d8, d9, [sp, #0x60]",
            "stp d10, d11, [sp, #0x70]",
            "stp d12, d13, [sp, #0x80]",
            "stp d14, d15, [sp, #0x90]",
            "ldr x2, [x18, #0x08]",
            "ldr x3, [x18, #0x10]",
            "stp x2, x3, [sp, #0xA0]",
            "ldr x2, [x18, #0x1478]",
            "str x2, [sp, #0xB0]",
            "mov x2, sp",
            "str x2, [x0]",
            "mov sp, x1",
            "ldp x2, x3, [sp, #0xA0]",
            "str x2, [x18, #0x08]",
            "str x3, [x18, #0x10]",
            "ldr x2, [sp, #0xB0]",
            "str x2, [x18, #0x1478]",
            "ldp x19, x20, [sp, #0x00]",
            "ldp x21, x22, [sp, #0x10]",
            "ldp x23, x24, [sp, #0x20]",
            "ldp x25, x26, [sp, #0x30]",
            "ldp x27, x28, [sp, #0x40]",
            "ldp x29, x30, [sp, #0x50]",
            "ldp d8, d9, [sp, #0x60]",
            "ldp d10, d11, [sp, #0x70]",
            "ldp d12, d13, [sp, #0x80]",
            "ldp d14, d15, [sp, #0x90]",
            "add sp, sp, #0xC0",
            "ret",
        )
    }

    /// First code a fresh context runs: `entry(arg)`, which must never return.
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("mov x0, x20", "blr x19", "brk #0x1")
    }
}
