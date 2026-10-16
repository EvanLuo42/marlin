//! x86-64 fiber context switching.

/// System V (Linux, macOS, most consoles): callee-saved rbx, rbp, r12-r15, MXCSR and the x87
/// control word.
#[cfg(not(windows))]
pub(crate) mod sysv {
    use core::ffi::c_void;

    use crate::fiber::{Context, Entry, Stack};

    /// MXCSR and x87 control word defaults (all exceptions masked, round to nearest).
    const FP_CONTROL: u64 = 0x1F80 | (0x037F << 32);

    /// Builds the frame `switch` pops when first switching to the context.
    ///
    /// # Safety
    /// `stack` stays alive while the context may run.
    pub(crate) unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
        // Frame from the saved stack pointer up: FP control, r15, r14, r13 (arg), r12 (entry),
        // rbx, rbp, return address. After `ret` the stack is 16-byte aligned, as the trampoline's
        // `call` needs.
        let top = stack.top().map_addr(|a| a & !15);
        // SAFETY: the frame lies in the committed top of the stack.
        unsafe {
            let frame = top.sub(0x50).cast::<u64>();
            frame.write(FP_CONTROL);
            for i in 1..7 {
                frame.add(i).write(0);
            }
            frame.add(3).write(arg as u64);
            frame.add(4).write(entry as usize as u64);
            frame.add(7).write((trampoline as *const ()).addr() as u64);
            Context(frame.cast())
        }
    }

    /// # Safety
    /// `to` is a context made by `make_context` or saved by `switch`, not running anywhere.
    #[unsafe(naked)]
    pub(crate) unsafe extern "C" fn switch(save: *mut Context, to: Context) {
        core::arch::naked_asm!(
            "push rbp",
            "push rbx",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "sub rsp, 8",
            "stmxcsr [rsp]",
            "fnstcw [rsp + 4]",
            "mov [rdi], rsp",
            "mov rsp, rsi",
            "ldmxcsr [rsp]",
            "fldcw [rsp + 4]",
            "add rsp, 8",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbx",
            "pop rbp",
            "ret",
        )
    }

    /// First code a fresh context runs: `entry(arg)`, which must never return.
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("mov rdi, r13", "call r12", "ud2")
    }
}

/// Windows x64: callee-saved rbx, rbp, rdi, rsi, r12-r15, xmm6-xmm15, MXCSR and the x87 control
/// word. The TEB's stack bounds are switched too: stack probes (`__chkstk`) and exception
/// dispatch check them.
#[cfg(windows)]
pub(crate) mod win64 {
    use core::ffi::c_void;

    use crate::fiber::{Context, Entry, Stack};

    const FP_CONTROL: u64 = 0x1F80 | (0x037F << 32);

    /// Builds the frame `switch` pops when first switching to the context.
    ///
    /// # Safety
    /// `stack` stays alive while the context may run.
    pub(crate) unsafe fn make_context(stack: &Stack, entry: Entry, arg: *mut c_void) -> Context {
        // Frame from the saved stack pointer up (in u64 slots): xmm6-xmm15 (0-19), FP control
        // (20), TEB StackBase (21), StackLimit (22), DeallocationStack (23), r15, r14, r13 (arg),
        // r12 (entry), rsi, rdi, rbx, rbp (24-31), return address (32).
        let top = stack.top().map_addr(|a| a & !15);
        // SAFETY: the frame lies in the committed top of the stack.
        unsafe {
            let frame = top.sub(0x110).cast::<u64>();
            for i in 0..32 {
                frame.add(i).write(0);
            }
            frame.add(20).write(FP_CONTROL);
            frame.add(21).write(stack.top() as u64);
            frame.add(22).write(stack.limit() as u64);
            frame.add(23).write(stack.base() as u64);
            frame.add(26).write(arg as u64);
            frame.add(27).write(entry as usize as u64);
            frame.add(32).write((trampoline as *const ()).addr() as u64);
            Context(frame.cast())
        }
    }

    /// # Safety
    /// `to` is a context made by `make_context` or saved by `switch`, not running anywhere.
    #[unsafe(naked)]
    pub(crate) unsafe extern "C" fn switch(save: *mut Context, to: Context) {
        core::arch::naked_asm!(
            "push rbp",
            "push rbx",
            "push rdi",
            "push rsi",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "sub rsp, 0xC0",
            "movdqu [rsp + 0x00], xmm6",
            "movdqu [rsp + 0x10], xmm7",
            "movdqu [rsp + 0x20], xmm8",
            "movdqu [rsp + 0x30], xmm9",
            "movdqu [rsp + 0x40], xmm10",
            "movdqu [rsp + 0x50], xmm11",
            "movdqu [rsp + 0x60], xmm12",
            "movdqu [rsp + 0x70], xmm13",
            "movdqu [rsp + 0x80], xmm14",
            "movdqu [rsp + 0x90], xmm15",
            "stmxcsr [rsp + 0xA0]",
            "fnstcw [rsp + 0xA4]",
            "mov rax, qword ptr gs:[0x08]",
            "mov [rsp + 0xA8], rax",
            "mov rax, qword ptr gs:[0x10]",
            "mov [rsp + 0xB0], rax",
            "mov rax, qword ptr gs:[0x1478]",
            "mov [rsp + 0xB8], rax",
            "mov [rcx], rsp",
            "mov rsp, rdx",
            "mov rax, [rsp + 0xB8]",
            "mov qword ptr gs:[0x1478], rax",
            "mov rax, [rsp + 0xB0]",
            "mov qword ptr gs:[0x10], rax",
            "mov rax, [rsp + 0xA8]",
            "mov qword ptr gs:[0x08], rax",
            "fldcw [rsp + 0xA4]",
            "ldmxcsr [rsp + 0xA0]",
            "movdqu xmm6, [rsp + 0x00]",
            "movdqu xmm7, [rsp + 0x10]",
            "movdqu xmm8, [rsp + 0x20]",
            "movdqu xmm9, [rsp + 0x30]",
            "movdqu xmm10, [rsp + 0x40]",
            "movdqu xmm11, [rsp + 0x50]",
            "movdqu xmm12, [rsp + 0x60]",
            "movdqu xmm13, [rsp + 0x70]",
            "movdqu xmm14, [rsp + 0x80]",
            "movdqu xmm15, [rsp + 0x90]",
            "add rsp, 0xC0",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rsi",
            "pop rdi",
            "pop rbx",
            "pop rbp",
            "ret",
        )
    }

    /// First code a fresh context runs: `entry(arg)`, which must never return. Reserves the 32-byte
    /// shadow space and realigns the stack for the call.
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("sub rsp, 0x28", "mov rcx, r13", "call r12", "ud2")
    }
}
