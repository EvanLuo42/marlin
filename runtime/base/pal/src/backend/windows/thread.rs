use core::ffi::c_void;
use core::ptr::{self, NonNull};
use core::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateThread,
    CreateWaitableTimerExW, GetActiveProcessorCount, GetCurrentThread, GetCurrentThreadId,
    INFINITE, STACK_SIZE_PARAM_IS_A_RESERVATION, SetThreadDescription, SetWaitableTimer, Sleep,
    SwitchToThread, TIMER_ALL_ACCESS, WaitForSingleObject,
};

use super::error::last_error;
use super::{Backend, for_each_wide_chunk};
use crate::Result;
use crate::contract::Thread;
use crate::thread::ThreadStart;

pub(crate) struct Handle(HANDLE);

// SAFETY: a thread handle may be waited on and closed from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; `Handle` has no `&self` methods.
unsafe impl Sync for Handle {}

unsafe extern "system" fn thread_start(param: *mut c_void) -> u32 {
    // SAFETY: `spawn`'s caller guarantees `param` is a live `ThreadStart`.
    unsafe {
        let start = NonNull::new_unchecked(param.cast::<ThreadStart>());
        (start.as_ref().entry)(start);
    }
    0
}

// SAFETY: `thread_start` calls the entry exactly once, on the new thread.
unsafe impl Thread for Backend {
    type Handle = Handle;

    unsafe fn spawn(stack_size: usize, start: NonNull<ThreadStart>) -> Result<Handle> {
        // Reserve rather than commit the requested stack, like the default stack.
        let flags = if stack_size == 0 {
            0
        } else {
            STACK_SIZE_PARAM_IS_A_RESERVATION
        };
        // SAFETY: the caller guarantees `start` outlives the entry call.
        let handle = unsafe {
            CreateThread(
                ptr::null(),
                stack_size,
                Some(thread_start),
                start.as_ptr().cast(),
                flags,
                ptr::null_mut(),
            )
        };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(Handle(handle))
        }
    }

    fn join(handle: Handle) -> Result<()> {
        // SAFETY: we own the handle.
        let result = match unsafe { WaitForSingleObject(handle.0, INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            _ => Err(last_error()),
        };
        // SAFETY: we own the handle and do not use it again.
        unsafe { CloseHandle(handle.0) };
        result
    }

    fn detach(handle: Handle) {
        // SAFETY: we own the handle and do not use it again.
        unsafe { CloseHandle(handle.0) };
    }

    fn current_id() -> u64 {
        // SAFETY: no preconditions.
        unsafe { GetCurrentThreadId() }.into()
    }

    fn set_current_name(name: &str) {
        let mut first = true;
        for_each_wide_chunk(name, |chunk| {
            if first {
                // SAFETY: `chunk` is NUL-terminated; GetCurrentThread returns a pseudo-handle.
                unsafe { SetThreadDescription(GetCurrentThread(), chunk.as_ptr()) };
                first = false;
            }
        });
    }

    fn sleep(duration: Duration) {
        if duration.is_zero() {
            Self::yield_now();
            return;
        }

        // A high-resolution waitable timer (Windows 10 1803+) avoids rounding to the 1-15.6 ms
        // scheduler tick that Sleep has.
        // SAFETY: no preconditions; null attributes and name are allowed.
        let timer = unsafe {
            CreateWaitableTimerExW(
                ptr::null(),
                ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        if !timer.is_null() {
            // Negative = relative, in 100 ns units.
            let due = -(duration.as_nanos().div_ceil(100).min(i64::MAX as u128) as i64);
            // SAFETY: `timer` is a timer handle we own; `due` outlives the call.
            let set = unsafe { SetWaitableTimer(timer, &due, 0, None, ptr::null(), 0) } != 0;
            if set {
                // SAFETY: as above.
                unsafe { WaitForSingleObject(timer, INFINITE) };
            }
            // SAFETY: we own the handle and do not use it again.
            unsafe { CloseHandle(timer) };
            if set {
                return;
            }
        }

        let ms = duration
            .as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(INFINITE - 1)) as u32;
        // SAFETY: no preconditions.
        unsafe { Sleep(ms) };
    }

    fn yield_now() {
        // SAFETY: no preconditions.
        unsafe { SwitchToThread() };
    }

    fn available_parallelism() -> usize {
        // SAFETY: no preconditions.
        (unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize).max(1)
    }
}
