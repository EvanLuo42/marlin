use core::ffi::c_void;
use core::ptr::{self, NonNull};
use core::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY;
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateThread,
    CreateWaitableTimerExW, GetActiveProcessorCount, GetCurrentThread, GetCurrentThreadId,
    GetThreadGroupAffinity, INFINITE, STACK_SIZE_PARAM_IS_A_RESERVATION, SetThreadDescription,
    SetThreadGroupAffinity, SetThreadPriority, SetWaitableTimer, Sleep, SwitchToThread,
    THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_LOWEST,
    THREAD_PRIORITY_NORMAL, THREAD_PRIORITY_TIME_CRITICAL, TIMER_ALL_ACCESS, WaitForSingleObject,
};

use super::error::{check, last_error};
use super::{Backend, for_each_wide_chunk};
use crate::contract::Thread;
use crate::thread::ThreadStart;
use crate::{CpuSet, Error, ErrorKind, Result, ThreadPriority};

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

    fn set_current_affinity(cpus: &CpuSet) -> Result<()> {
        // A thread runs in exactly one processor group, so the set must not span groups.
        let mut groups = (0..CpuSet::WORDS).filter(|&g| cpus.word(g) != 0);
        let (Some(group), None) = (groups.next(), groups.next()) else {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        };
        let affinity = GROUP_AFFINITY {
            Mask: usize::try_from(cpus.word(group))
                .map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?,
            Group: group as u16,
            Reserved: [0; 3],
        };
        // SAFETY: GetCurrentThread returns a pseudo-handle; `affinity` outlives the call and a null
        // previous-affinity pointer is allowed.
        check(unsafe { SetThreadGroupAffinity(GetCurrentThread(), &affinity, ptr::null_mut()) })
    }

    fn current_affinity() -> Result<CpuSet> {
        let mut affinity = GROUP_AFFINITY::default();
        // SAFETY: GetCurrentThread returns a pseudo-handle; `affinity` is a valid out pointer.
        check(unsafe { GetThreadGroupAffinity(GetCurrentThread(), &mut affinity) })?;
        let group = usize::from(affinity.Group);
        if group >= CpuSet::WORDS {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        let mut set = CpuSet::new();
        set.set_word(group, affinity.Mask as u64);
        Ok(set)
    }

    fn set_current_priority(priority: ThreadPriority) -> Result<()> {
        let value = match priority {
            ThreadPriority::Background => THREAD_PRIORITY_LOWEST,
            ThreadPriority::Low => THREAD_PRIORITY_BELOW_NORMAL,
            ThreadPriority::Normal => THREAD_PRIORITY_NORMAL,
            ThreadPriority::High => THREAD_PRIORITY_ABOVE_NORMAL,
            ThreadPriority::TimeCritical => THREAD_PRIORITY_TIME_CRITICAL,
        };
        // SAFETY: GetCurrentThread returns a pseudo-handle with THREAD_SET_INFORMATION access.
        check(unsafe { SetThreadPriority(GetCurrentThread(), value) })
    }
}
