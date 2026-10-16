//! Windows 11 IoRing.
//!
//! Loaded at run time: the API set does not exist on Windows 10, and a static import of it would
//! stop the executable from loading there. Needs IORING_VERSION_3 (Windows 11 22H2) for writes.

use core::ffi::CStr;
use core::mem;
use core::ptr;
use core::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, S_FALSE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_WRITE_FLAGS, FILE_WRITE_FLAGS_NONE, HIORING, IORING_BUFFER_REF, IORING_BUFFER_REF_0,
    IORING_CAPABILITIES, IORING_CQE, IORING_CREATE_FLAGS, IORING_FEATURE_SET_COMPLETION_EVENT,
    IORING_HANDLE_REF, IORING_HANDLE_REF_0, IORING_OP_CODE, IORING_OP_READ, IORING_OP_WRITE,
    IORING_REF_RAW, IORING_SQE_FLAGS, IORING_VERSION, IORING_VERSION_3, IOSQE_FLAGS_NONE,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::core::{BOOL, HRESULT};

use super::{hresult_error, hresult_result, timeout_ms};
use crate::Result;
use crate::backend::windows::error::last_error;
use crate::file::{Completion, Deadline, Op, OpKind};

// Tags travel in the pointer-sized user data.
const _: () = assert!(size_of::<usize>() == size_of::<u64>());

/// What GetProcAddress returns.
type Proc = unsafe extern "system" fn() -> isize;
type CreateFn = unsafe extern "system" fn(
    IORING_VERSION,
    IORING_CREATE_FLAGS,
    u32,
    u32,
    *mut HIORING,
) -> HRESULT;
type QueryCapsFn = unsafe extern "system" fn(*mut IORING_CAPABILITIES) -> HRESULT;
type IsOpSupportedFn = unsafe extern "system" fn(HIORING, IORING_OP_CODE) -> BOOL;
type SetEventFn = unsafe extern "system" fn(HIORING, HANDLE) -> HRESULT;
type BuildReadFn = unsafe extern "system" fn(
    HIORING,
    IORING_HANDLE_REF,
    IORING_BUFFER_REF,
    u32,
    u64,
    usize,
    IORING_SQE_FLAGS,
) -> HRESULT;
type BuildWriteFn = unsafe extern "system" fn(
    HIORING,
    IORING_HANDLE_REF,
    IORING_BUFFER_REF,
    u32,
    u64,
    FILE_WRITE_FLAGS,
    usize,
    IORING_SQE_FLAGS,
) -> HRESULT;
type SubmitFn = unsafe extern "system" fn(HIORING, u32, u32, *mut u32) -> HRESULT;
type PopFn = unsafe extern "system" fn(HIORING, *mut IORING_CQE) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HIORING) -> HRESULT;

struct Api {
    create: CreateFn,
    query_caps: QueryCapsFn,
    is_op_supported: IsOpSupportedFn,
    set_event: SetEventFn,
    build_read: BuildReadFn,
    build_write: BuildWriteFn,
    submit: SubmitFn,
    pop: PopFn,
    close: CloseFn,
}

const fn wide<const N: usize>(ascii: &[u8; N]) -> [u16; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < N {
        out[i] = ascii[i] as u16;
        i += 1;
    }
    out
}

impl Api {
    fn load() -> Option<Self> {
        // KernelBase implements the API set; kernel32 exports the newer write call on some builds.
        // Both are loaded in every process, so there is nothing to load or free.
        const KERNELBASE: [u16; 15] = wide(b"kernelbase.dll\0");
        const KERNEL32: [u16; 13] = wide(b"kernel32.dll\0");
        // SAFETY: the names are NUL-terminated.
        let modules = unsafe {
            [
                GetModuleHandleW(KERNELBASE.as_ptr()),
                GetModuleHandleW(KERNEL32.as_ptr()),
            ]
        };
        let find = |name: &CStr| {
            modules
                .iter()
                .filter(|module| !module.is_null())
                // SAFETY: `module` is a loaded module; `name` is NUL-terminated.
                .find_map(|&module| unsafe { GetProcAddress(module, name.as_ptr().cast()) })
        };
        // SAFETY: each export has the signature windows-sys declares for it, which the target
        // types copy.
        unsafe {
            Some(Self {
                create: mem::transmute::<Proc, CreateFn>(find(c"CreateIoRing")?),
                query_caps: mem::transmute::<Proc, QueryCapsFn>(find(c"QueryIoRingCapabilities")?),
                is_op_supported: mem::transmute::<Proc, IsOpSupportedFn>(find(
                    c"IsIoRingOpSupported",
                )?),
                set_event: mem::transmute::<Proc, SetEventFn>(find(c"SetIoRingCompletionEvent")?),
                build_read: mem::transmute::<Proc, BuildReadFn>(find(c"BuildIoRingReadFile")?),
                build_write: mem::transmute::<Proc, BuildWriteFn>(find(c"BuildIoRingWriteFile")?),
                submit: mem::transmute::<Proc, SubmitFn>(find(c"SubmitIoRing")?),
                pop: mem::transmute::<Proc, PopFn>(find(c"PopIoRingCompletion")?),
                close: mem::transmute::<Proc, CloseFn>(find(c"CloseIoRing")?),
            })
        }
    }
}

pub(crate) struct Ring {
    api: Api,
    ring: HIORING,
    /// Auto-reset event the ring signals when it posts completions; lets `poll` wait with a
    /// timeout.
    event: HANDLE,
}

// SAFETY: an IoRing may be used from any thread, one at a time (`&mut self`).
unsafe impl Send for Ring {}

impl Ring {
    /// `None` when IoRing is missing, too old, or cannot be created; the caller falls back to IOCP.
    pub(super) fn new(capacity: u32) -> Option<Self> {
        let api = Api::load()?;
        let mut caps = IORING_CAPABILITIES::default();
        // SAFETY: `caps` is a valid out pointer.
        if unsafe { (api.query_caps)(&mut caps) } < 0
            || caps.MaxVersion < IORING_VERSION_3
            || caps.FeatureFlags & IORING_FEATURE_SET_COMPLETION_EVENT == 0
            || capacity > caps.MaxSubmissionQueueSize
        {
            return None;
        }
        // Completions never outnumber operations in flight, which never exceed `capacity`.
        let cq_size = capacity
            .saturating_mul(2)
            .min(caps.MaxCompletionQueueSize)
            .max(capacity);
        let mut ring = ptr::null_mut();
        // SAFETY: `ring` is a valid out pointer.
        let hr = unsafe {
            (api.create)(
                IORING_VERSION_3,
                IORING_CREATE_FLAGS::default(),
                capacity,
                cq_size,
                &mut ring,
            )
        };
        if hr < 0 {
            return None;
        }
        // From here on `Drop` releases whatever was created.
        let mut this = Self {
            api,
            ring,
            event: ptr::null_mut(),
        };
        // SAFETY: the ring is live.
        let ops_supported = unsafe {
            (this.api.is_op_supported)(ring, IORING_OP_READ) != 0
                && (this.api.is_op_supported)(ring, IORING_OP_WRITE) != 0
        };
        if !ops_supported {
            return None;
        }
        // SAFETY: auto-reset, unsignaled, unnamed; null attributes are allowed.
        this.event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        // SAFETY: the ring and the event are live.
        if this.event.is_null() || unsafe { (this.api.set_event)(ring, this.event) } < 0 {
            return None;
        }
        Some(this)
    }

    /// Builds the entry; `submit` starts it.
    ///
    /// # Safety
    /// The buffer and the file stay valid until the completion is polled.
    pub(super) unsafe fn push(&mut self, op: &Op<'_>) -> Result<()> {
        let file = IORING_HANDLE_REF {
            Kind: IORING_REF_RAW,
            Handle: IORING_HANDLE_REF_0 {
                Handle: op.file.handle,
            },
        };
        let buffer = IORING_BUFFER_REF {
            Kind: IORING_REF_RAW,
            Buffer: IORING_BUFFER_REF_0 {
                Address: op.buf.cast(),
            },
        };
        let tag = op.tag as usize;
        // SAFETY: the ring is live; the caller keeps the buffer and the file valid.
        let hr = unsafe {
            match op.kind {
                OpKind::Read => (self.api.build_read)(
                    self.ring,
                    file,
                    buffer,
                    op.len,
                    op.offset,
                    tag,
                    IOSQE_FLAGS_NONE,
                ),
                OpKind::Write => (self.api.build_write)(
                    self.ring,
                    file,
                    buffer,
                    op.len,
                    op.offset,
                    FILE_WRITE_FLAGS_NONE,
                    tag,
                    IOSQE_FLAGS_NONE,
                ),
            }
        };
        if hr < 0 {
            Err(hresult_error(hr))
        } else {
            Ok(())
        }
    }

    pub(super) fn submit(&mut self) -> Result<()> {
        let mut submitted = 0;
        // SAFETY: the ring is live; waiting for 0 operations returns at once.
        let hr = unsafe { (self.api.submit)(self.ring, 0, 0, &mut submitted) };
        if hr < 0 {
            Err(hresult_error(hr))
        } else {
            Ok(())
        }
    }

    pub(super) fn poll(
        &mut self,
        out: &mut [Completion],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        self.submit()?;
        let deadline = Deadline::after(timeout);
        loop {
            let n = self.pop(out)?;
            if n > 0 {
                return Ok(n);
            }
            let remaining = deadline.remaining();
            if remaining == Some(Duration::ZERO) {
                return Ok(0);
            }
            // The event is set for completions posted since it was last waited on, including any
            // that raced with the pop above, so no wakeup is lost; extra wakeups just loop.
            // SAFETY: the event is live.
            match unsafe { WaitForSingleObject(self.event, timeout_ms(remaining)) } {
                WAIT_OBJECT_0 | WAIT_TIMEOUT => {}
                _ => return Err(last_error()),
            }
        }
    }

    fn pop(&mut self, out: &mut [Completion]) -> Result<usize> {
        let mut n = 0;
        while n < out.len() {
            let mut cqe = IORING_CQE::default();
            // SAFETY: the ring is live; `cqe` is a valid out pointer.
            let hr = unsafe { (self.api.pop)(self.ring, &mut cqe) };
            if hr == S_FALSE {
                break;
            }
            if hr < 0 {
                return Err(hresult_error(hr));
            }
            out[n] = Completion {
                tag: cqe.UserData as u64,
                result: hresult_result(cqe.ResultCode, cqe.Information),
            };
            n += 1;
        }
        Ok(n)
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: we own the ring and the event and do not use them again. The caller drained the
        // ring, so the OS holds no references into caller memory.
        unsafe {
            (self.api.close)(self.ring);
            if !self.event.is_null() {
                CloseHandle(self.event);
            }
        }
    }
}
