use core::ffi::c_void;
use core::mem;
use core::ptr::{self, NonNull};
use core::time::Duration;

use super::{Backend, check_code, errno};
use crate::Result;
use crate::contract::{Thread, VMem};
use crate::thread::ThreadStart;

pub(crate) struct Handle(libc::pthread_t);

// SAFETY: a pthread_t may be joined or detached from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; `Handle` has no `&self` methods.
unsafe impl Sync for Handle {}

extern "C" fn thread_start(arg: *mut c_void) -> *mut c_void {
    // SAFETY: `spawn`'s caller guarantees `arg` is a live `ThreadStart`.
    unsafe {
        let start = NonNull::new_unchecked(arg.cast::<ThreadStart>());
        (start.as_ref().entry)(start);
    }
    ptr::null_mut()
}

// SAFETY: `thread_start` calls the entry exactly once, on the new thread.
unsafe impl Thread for Backend {
    type Handle = Handle;

    unsafe fn spawn(stack_size: usize, start: NonNull<ThreadStart>) -> Result<Handle> {
        // SAFETY: `pthread_attr_t` is plain data; it is initialized before use.
        let mut attr: libc::pthread_attr_t = unsafe { mem::zeroed() };
        // SAFETY: `attr` is a valid out pointer.
        check_code(unsafe { libc::pthread_attr_init(&mut attr) })?;

        let size = stack_size
            .max(libc::PTHREAD_STACK_MIN)
            .next_multiple_of(Self::page_size());
        let mut result = if stack_size == 0 {
            Ok(())
        } else {
            // SAFETY: `attr` is initialized.
            check_code(unsafe { libc::pthread_attr_setstacksize(&mut attr, size) })
        };
        // SAFETY: `pthread_t` is plain data; pthread_create initializes it.
        let mut thread: libc::pthread_t = unsafe { mem::zeroed() };
        if result.is_ok() {
            // SAFETY: `attr` is initialized; the caller guarantees `start` outlives the entry call.
            result = check_code(unsafe {
                libc::pthread_create(&mut thread, &attr, thread_start, start.as_ptr().cast())
            });
        }
        // SAFETY: `attr` is initialized and no longer needed.
        unsafe { libc::pthread_attr_destroy(&mut attr) };
        result.map(|()| Handle(thread))
    }

    fn join(handle: Handle) -> Result<()> {
        // SAFETY: we own the joinable thread.
        check_code(unsafe { libc::pthread_join(handle.0, ptr::null_mut()) })
    }

    fn detach(handle: Handle) {
        // SAFETY: we own the joinable thread.
        unsafe { libc::pthread_detach(handle.0) };
    }

    fn current_id() -> u64 {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: no preconditions.
            unsafe { libc::syscall(libc::SYS_gettid) as u64 }
        }
        #[cfg(target_vendor = "apple")]
        {
            let mut id = 0;
            // SAFETY: `id` is a valid out pointer.
            unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut id) };
            id
        }
    }

    fn set_current_name(name: &str) {
        // Including the NUL.
        #[cfg(target_os = "linux")]
        const MAX: usize = 16;
        #[cfg(target_vendor = "apple")]
        const MAX: usize = 64;

        let mut len = name.len().min(MAX - 1);
        while !name.is_char_boundary(len) {
            len -= 1;
        }
        let bytes = &name.as_bytes()[..len];
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(len);
        let mut buf = [0u8; MAX];
        buf[..len].copy_from_slice(&bytes[..len]);

        // SAFETY: `buf` is NUL-terminated.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::pthread_setname_np(libc::pthread_self(), buf.as_ptr().cast())
        };
        // SAFETY: `buf` is NUL-terminated. macOS can only name the calling thread.
        #[cfg(target_vendor = "apple")]
        unsafe {
            libc::pthread_setname_np(buf.as_ptr().cast())
        };
    }

    fn sleep(duration: Duration) {
        // SAFETY: `timespec` is plain data; all-zero is valid.
        let mut ts: libc::timespec = unsafe { mem::zeroed() };
        ts.tv_sec = duration.as_secs().try_into().unwrap_or(libc::time_t::MAX);
        ts.tv_nsec = duration.subsec_nanos() as _;
        loop {
            let mut remaining = ts;
            // SAFETY: both pointers are valid.
            if unsafe { libc::nanosleep(&ts, &mut remaining) } == 0 || errno() != libc::EINTR {
                return;
            }
            ts = remaining;
        }
    }

    fn yield_now() {
        // SAFETY: no preconditions.
        unsafe { libc::sched_yield() };
    }

    fn available_parallelism() -> usize {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `cpu_set_t` is plain data; all-zero is an empty set.
            let mut set: libc::cpu_set_t = unsafe { mem::zeroed() };
            // SAFETY: `set` is a valid out pointer of the given size.
            if unsafe { libc::sched_getaffinity(0, mem::size_of_val(&set), &mut set) } == 0 {
                // SAFETY: `set` was filled in by sched_getaffinity.
                let count = unsafe { libc::CPU_COUNT(&set) };
                if count > 0 {
                    return count as usize;
                }
            }
        }
        // SAFETY: no preconditions.
        (unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }).max(1) as usize
    }
}
