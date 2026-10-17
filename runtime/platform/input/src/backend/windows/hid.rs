//! HID gamepads through `hid.dll`: PlayStation and Switch Pro controllers with the drivers in
//! [`crate::hid`], other HID gamepads with a heuristic mapping. Xbox pads (whose HID interface
//! has `IG_` in its path) are left to XInput.
//!
//! Devices are opened for overlapped I/O; each update drains every report that arrived, so
//! button edges between frames survive. Hot-plugging comes from a `CM_Register_Notification`
//! callback that marks the device list stale.

mod generic;

use core::ffi::c_void;
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::boxed::Box;
use alloc::vec::Vec;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER,
    CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification, CM_Unregister_Notification,
    CR_BUFFER_SMALL, CR_SUCCESS, HCMNOTIFICATION,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetAttributes,
    HidD_GetHidGuid, HidD_GetPreparsedData, HidD_SetNumInputBuffers, HidP_GetCaps,
    PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::core::GUID;

use super::{Slot, allocate_slot};
use crate::hid::{self, Calibration, Feedback, Transport, dualsense, dualshock4, switch_pro};
use crate::{
    Error, ErrorKind, GamepadKind, GamepadReading, HapticTone, InputState, MAX_GAMEPADS, Result,
    Trigger, TriggerEffect,
};

/// `IOCTL_HID_GET_FEATURE`: `CTL_CODE(FILE_DEVICE_KEYBOARD, 100, METHOD_OUT_DIRECT, 0)`.
const IOCTL_HID_GET_FEATURE: u32 = 0x000B_0192;
/// Reports drained per device per update, so a flooding device cannot stall a frame.
const MAX_REPORTS_PER_UPDATE: usize = 64;
/// How long a feature read or output write may block the main thread.
const IO_TIMEOUT_MS: u32 = 100;
/// The Switch Pro stops a rumble that is not refreshed.
const SWITCH_RUMBLE_REFRESH_MS: u64 = 50;
/// Device list re-scan period when hot-plug notifications are unavailable.
const RESCAN_SECONDS: u64 = 2;
/// Room for the longest output report a driver writes.
const OUTPUT_CAPACITY: usize = 80;

pub(super) struct HidGamepads {
    hid_guid: GUID,
    /// Set by the hot-plug callback (on a system thread) and at start.
    stale: Box<AtomicBool>,
    notification: HCMNOTIFICATION,
    next_rescan: u64,
    // Boxed: a pending read holds pointers into its pad, which must not move with the Vec.
    #[expect(clippy::vec_box)]
    pads: Vec<Box<Pad>>,
    /// Interfaces seen that are not gamepads, so re-scans skip opening them again.
    ignored: Vec<Vec<u16>>,
}

impl HidGamepads {
    pub(super) fn new() -> Self {
        let mut hid_guid = GUID::default();
        // SAFETY: valid out pointer.
        unsafe { HidD_GetHidGuid(&mut hid_guid) };
        let stale = Box::new(AtomicBool::new(true));
        let mut filter: CM_NOTIFY_FILTER =
            // SAFETY: all-zero is a valid CM_NOTIFY_FILTER.
            unsafe { mem::zeroed() };
        filter.cbSize = size_of::<CM_NOTIFY_FILTER>() as u32;
        filter.FilterType = CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE;
        filter.u.DeviceInterface.ClassGuid = hid_guid;
        let mut notification = ptr::null_mut();
        // SAFETY: the context is the boxed flag, which outlives the registration (`drop`
        // unregisters first, and unregistering waits for running callbacks).
        let result = unsafe {
            CM_Register_Notification(
                &filter,
                (&raw const *stale).cast(),
                Some(on_device_change),
                &mut notification,
            )
        };
        if result != CR_SUCCESS {
            notification = ptr::null_mut();
        }
        Self {
            hid_guid,
            stale,
            notification,
            next_rescan: 0,
            pads: Vec::new(),
            ignored: Vec::new(),
        }
    }

    /// Opens new devices, reads every pad and flushes output. Pads that vanished release their
    /// slots.
    pub(super) fn update(
        &mut self,
        now: u64,
        ticks_per_second: u64,
        state: &mut InputState,
        slots: &mut [Slot; MAX_GAMEPADS],
    ) {
        let rescan = if self.notification.is_null() {
            let due = now >= self.next_rescan;
            if due {
                self.next_rescan = now + RESCAN_SECONDS * ticks_per_second;
            }
            due
        } else {
            self.stale.swap(false, Ordering::Acquire)
        };
        if rescan {
            self.scan(state, slots);
        }
        let refresh = SWITCH_RUMBLE_REFRESH_MS * ticks_per_second / 1000;
        for pad in &mut self.pads {
            pad.read(state);
            pad.flush(now, refresh);
        }
        self.pads.retain(|pad| {
            if pad.alive {
                return true;
            }
            state.set_gamepad(pad.slot, None);
            slots[pad.slot] = Slot::Free;
            false
        });
    }

    fn scan(&mut self, state: &mut InputState, slots: &mut [Slot; MAX_GAMEPADS]) {
        let paths = interface_paths(&self.hid_guid);
        // Forget what is gone; a re-plugged device gets a fresh look.
        self.ignored.retain(|p| paths.contains(p));
        for pad in &mut self.pads {
            if !paths.contains(&pad.path) {
                pad.alive = false;
            }
        }
        for path in paths {
            if self.ignored.contains(&path) || self.pads.iter().any(|p| p.path == path) {
                continue;
            }
            match Pad::open(&path) {
                // With all slots taken the pad is dropped; the next device change looks again.
                Opened::Pad(mut pad) => {
                    if let Some(slot) = allocate_slot(slots, Slot::Hid) {
                        pad.slot = slot;
                        pad.feedback.player = slot as u8 + 1;
                        state.set_gamepad(slot, Some(&connected(pad.kind())));
                        self.pads.push(pad);
                    }
                }
                Opened::NotGamepad => self.ignored.push(path),
                Opened::Failed => {}
            }
        }
    }

    pub(super) fn pad(&mut self, slot: usize) -> Option<&mut Pad> {
        self.pads
            .iter_mut()
            .find(|p| p.slot == slot && p.alive)
            .map(|p| &mut **p)
    }
}

impl Drop for HidGamepads {
    fn drop(&mut self) {
        if !self.notification.is_null() {
            // SAFETY: registered in `new`; waits for callbacks in flight, after which nothing
            // touches `stale`.
            unsafe { CM_Unregister_Notification(self.notification) };
        }
    }
}

unsafe extern "system" fn on_device_change(
    _: HCMNOTIFICATION,
    context: *const c_void,
    _: CM_NOTIFY_ACTION,
    _: *const CM_NOTIFY_EVENT_DATA,
    _: u32,
) -> u32 {
    // SAFETY: the context is the `HidGamepads::stale` flag, alive while registered.
    let stale = unsafe { &*context.cast::<AtomicBool>() };
    stale.store(true, Ordering::Release);
    ERROR_SUCCESS
}

/// Every present HID interface, each path NUL-terminated.
fn interface_paths(guid: &GUID) -> Vec<Vec<u16>> {
    let mut list: Vec<u16> = Vec::new();
    for _ in 0..4 {
        let mut len = 0;
        // SAFETY: valid out pointer; no device-instance filter.
        let r = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                guid,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if r != CR_SUCCESS {
            return Vec::new();
        }
        list.clear();
        list.resize(len as usize, 0);
        // SAFETY: the buffer holds `len` units.
        let r = unsafe {
            CM_Get_Device_Interface_ListW(
                guid,
                ptr::null(),
                list.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if r == CR_SUCCESS {
            break;
        }
        if r != CR_BUFFER_SMALL {
            return Vec::new();
        }
        list.clear();
    }
    // A list of NUL-terminated strings ending with an empty one.
    list.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| s.iter().copied().chain([0]).collect())
        .collect()
}

enum Opened {
    Pad(Box<Pad>),
    NotGamepad,
    /// A gamepad that could not be opened (in use exclusively, or vanished meanwhile).
    Failed,
}

enum Driver {
    DualSense(Calibration),
    DualShock4(Calibration),
    SwitchPro { setup_step: usize },
    Generic(generic::Generic),
}

pub(super) struct Pad {
    path: Vec<u16>,
    file: HANDLE,
    transport: Transport,
    driver: Driver,
    slot: usize,
    alive: bool,
    /// Read state. The OVERLAPPED and buffer stay put while a read is pending: `Pad` is boxed.
    read_overlapped: OVERLAPPED,
    read_event: HANDLE,
    read_buffer: Vec<u8>,
    reading: bool,
    /// Output reports are padded to the longest one the device declares, as Windows requires.
    output: Vec<u8>,
    output_len: usize,
    feedback: Feedback,
    dirty: bool,
    /// Output packet counter (PlayStation Bluetooth sequence, Switch packet number).
    counter: u8,
    next_refresh: u64,
    motion_enabled: bool,
}

impl Pad {
    fn open(path: &[u16]) -> Opened {
        // A first look without access rights works even on devices the system holds exclusively
        // (keyboards, mice).
        let Some(probe) = create(path, 0, 0) else {
            return Opened::Failed;
        };
        let identity = identify(probe, path);
        // SAFETY: opened above.
        unsafe { CloseHandle(probe) };
        let Some((kind, caps)) = identity else {
            return Opened::NotGamepad;
        };
        let Some(file) = create(path, GENERIC_READ | GENERIC_WRITE, FILE_FLAG_OVERLAPPED) else {
            return Opened::Failed;
        };
        // SAFETY: `file` is a live HID handle. More buffers ride out a long frame.
        unsafe { HidD_SetNumInputBuffers(file, 64) };
        let transport = if caps.InputReportByteLength == 64 {
            Transport::Usb
        } else {
            Transport::Bluetooth
        };
        // Feature reads use the longest feature report the device declares, as Windows wants.
        let mut feature = alloc::vec![0u8; (caps.FeatureReportByteLength as usize).max(64)];
        let driver = match kind {
            GamepadKind::DualSense => {
                let read = get_feature(file, dualsense::CALIBRATION_REPORT, &mut feature);
                Driver::DualSense(match read {
                    true => dualsense::calibration(&feature),
                    false => Calibration::NOMINAL,
                })
            }
            GamepadKind::DualShock4 => {
                let id = match transport {
                    Transport::Usb => dualshock4::CALIBRATION_REPORT_USB,
                    Transport::Bluetooth => dualshock4::CALIBRATION_REPORT_BLUETOOTH,
                };
                let read = get_feature(file, id, &mut feature);
                Driver::DualShock4(match read {
                    true => dualshock4::calibration(&feature, transport),
                    false => Calibration::NOMINAL,
                })
            }
            GamepadKind::SwitchPro => Driver::SwitchPro { setup_step: 0 },
            _ => match generic::Generic::new(file) {
                Some(g) => Driver::Generic(g),
                None => {
                    // SAFETY: opened above, not used afterwards.
                    unsafe { CloseHandle(file) };
                    return Opened::NotGamepad;
                }
            },
        };
        // SAFETY: no name; manual reset, as overlapped I/O requires.
        let read_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if read_event.is_null() {
            // SAFETY: opened above, not used afterwards.
            unsafe { CloseHandle(file) };
            return Opened::Failed;
        }
        let mut pad = Box::new(Pad {
            path: path.to_vec(),
            file,
            transport,
            driver,
            slot: 0,
            alive: true,
            // SAFETY: all-zero is a valid OVERLAPPED.
            read_overlapped: unsafe { mem::zeroed() },
            read_event,
            read_buffer: alloc::vec![0; caps.InputReportByteLength as usize],
            reading: false,
            output: alloc::vec![0; (caps.OutputReportByteLength as usize).max(OUTPUT_CAPACITY)],
            output_len: caps.OutputReportByteLength as usize,
            feedback: Feedback::default(),
            dirty: !matches!(kind, GamepadKind::Generic),
            counter: 0,
            next_refresh: 0,
            motion_enabled: true,
        });
        pad.read_overlapped.hEvent = read_event;
        Opened::Pad(pad)
    }

    pub(super) fn kind(&self) -> GamepadKind {
        match self.driver {
            Driver::DualSense(_) => GamepadKind::DualSense,
            Driver::DualShock4(_) => GamepadKind::DualShock4,
            Driver::SwitchPro { .. } => GamepadKind::SwitchPro,
            Driver::Generic(_) => GamepadKind::Generic,
        }
    }

    /// Applies every report that arrived since the last update, then starts the next read.
    fn read(&mut self, state: &mut InputState) {
        for _ in 0..MAX_REPORTS_PER_UPDATE {
            let mut len = 0;
            if self.reading {
                // SAFETY: the read in flight uses this OVERLAPPED; don't wait.
                let done =
                    unsafe { GetOverlappedResult(self.file, &self.read_overlapped, &mut len, 0) }
                        != 0;
                if !done {
                    // SAFETY: no preconditions.
                    if unsafe { GetLastError() } != ERROR_IO_INCOMPLETE {
                        self.alive = false;
                    }
                    return;
                }
                self.reading = false;
                self.apply(len as usize, state);
            }
            // SAFETY: the buffer and OVERLAPPED live in the boxed pad until the read completes
            // or `drop` cancels it.
            let started = unsafe {
                ReadFile(
                    self.file,
                    self.read_buffer.as_mut_ptr(),
                    self.read_buffer.len() as u32,
                    ptr::null_mut(),
                    &mut self.read_overlapped,
                )
            } != 0;
            // SAFETY: no preconditions.
            if !started && unsafe { GetLastError() } != ERROR_IO_PENDING {
                self.alive = false;
                return;
            }
            // Completed immediately or pending: collect it through GetOverlappedResult either way.
            self.reading = true;
        }
    }

    fn apply(&mut self, len: usize, state: &mut InputState) {
        let report = &mut self.read_buffer[..len];
        let reading = match &mut self.driver {
            Driver::DualSense(cal) => dualsense::parse(report, self.transport, cal),
            Driver::DualShock4(cal) => dualshock4::parse(report, self.transport, cal),
            Driver::SwitchPro { .. } => switch_pro::parse(report),
            Driver::Generic(g) => g.parse(report),
        };
        if let Some(mut reading) = reading {
            if !self.motion_enabled {
                reading.motion = None;
            }
            state.set_gamepad(self.slot, Some(&reading));
        }
    }

    /// Sends the next setup command, or the feedback if it changed (or needs refreshing).
    fn flush(&mut self, now: u64, refresh: u64) {
        let counter = self.counter;
        let out = &mut self.output;
        out.fill(0);
        let written = match &mut self.driver {
            Driver::SwitchPro { setup_step } => {
                let player = self.feedback.player;
                if let Some(n) =
                    switch_pro::command(*setup_step, self.transport, counter, player, out)
                {
                    *setup_step += 1;
                    Some(n)
                } else {
                    let active =
                        self.feedback.rumble != [0.0; 2] || self.feedback.haptics.is_some();
                    if active && now >= self.next_refresh {
                        self.dirty = true;
                        self.next_refresh = now + refresh;
                    }
                    let n = self
                        .dirty
                        .then(|| switch_pro::rumble(&self.feedback, counter, out));
                    self.dirty = false;
                    n
                }
            }
            _ if !self.dirty => None,
            Driver::DualSense(_) => {
                self.dirty = false;
                Some(dualsense::output(
                    &self.feedback,
                    self.transport,
                    counter,
                    out,
                ))
            }
            Driver::DualShock4(_) => {
                self.dirty = false;
                Some(dualshock4::output(&self.feedback, self.transport, out))
            }
            Driver::Generic(_) => None,
        };
        if let Some(written) = written {
            self.counter = self.counter.wrapping_add(1) & 0x0F;
            // Padded to the device's declared output length, as Windows wants. A
            // failed write (stalled Bluetooth link) is dropped; the next change sends again.
            write(self.file, &self.output[..self.output_len.max(written)]);
        }
    }

    pub(super) fn set_rumble(&mut self, low: f32, high: f32) -> Result<()> {
        if matches!(self.driver, Driver::Generic(_)) {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        self.feedback.rumble = [low, high];
        self.dirty = true;
        Ok(())
    }

    pub(super) fn set_trigger_effect(
        &mut self,
        trigger: Trigger,
        effect: &TriggerEffect,
    ) -> Result<()> {
        if !matches!(self.driver, Driver::DualSense(_)) {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        let i = match trigger {
            Trigger::Left => 0,
            Trigger::Right => 1,
        };
        self.feedback.triggers[i] = *effect;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn set_haptics(&mut self, tones: Option<[HapticTone; 2]>) -> Result<()> {
        if !matches!(self.driver, Driver::SwitchPro { .. }) {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        self.feedback.haptics = tones;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn set_motion_enabled(&mut self, enabled: bool) -> Result<()> {
        if matches!(self.driver, Driver::Generic(_)) {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        self.motion_enabled = enabled;
        Ok(())
    }
}

impl Drop for Pad {
    fn drop(&mut self) {
        // SAFETY: cancel the read in flight and wait for it to finish before the buffer and
        // OVERLAPPED are freed; then close what `open` created.
        unsafe {
            if self.reading {
                let mut len = 0;
                CancelIoEx(self.file, &self.read_overlapped);
                GetOverlappedResult(self.file, &self.read_overlapped, &mut len, 1);
            }
            CloseHandle(self.read_event);
            CloseHandle(self.file);
        }
    }
}

fn create(path: &[u16], access: u32, flags: u32) -> Option<HANDLE> {
    // SAFETY: `path` is NUL-terminated.
    let file = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    (file != INVALID_HANDLE_VALUE).then_some(file)
}

/// The kind and capabilities of a gamepad interface, or `None` for anything else.
fn identify(file: HANDLE, path: &[u16]) -> Option<(GamepadKind, HIDP_CAPS)> {
    let mut attributes = HIDD_ATTRIBUTES {
        Size: size_of::<HIDD_ATTRIBUTES>() as u32,
        VendorID: 0,
        ProductID: 0,
        VersionNumber: 0,
    };
    // SAFETY: live handle, `Size` set.
    if !unsafe { HidD_GetAttributes(file, &mut attributes) } {
        return None;
    }
    let mut preparsed: PHIDP_PREPARSED_DATA = 0;
    // SAFETY: live handle, valid out pointer.
    if !unsafe { HidD_GetPreparsedData(file, &mut preparsed) } {
        return None;
    }
    // SAFETY: all-zero is a valid HIDP_CAPS.
    let mut caps: HIDP_CAPS = unsafe { mem::zeroed() };
    // SAFETY: `preparsed` came from HidD_GetPreparsedData and is freed right after.
    let ok = unsafe { HidP_GetCaps(preparsed, &mut caps) } == HIDP_STATUS_SUCCESS;
    // SAFETY: as above.
    unsafe { HidD_FreePreparsedData(preparsed) };
    // Generic desktop page: joystick (4) or gamepad (5).
    if !ok || caps.UsagePage != 0x01 || !matches!(caps.Usage, 4 | 5) {
        return None;
    }
    if let Some(kind) = hid::identify(attributes.VendorID, attributes.ProductID) {
        return Some((kind, caps));
    }
    // XInput devices expose a HID interface too; XInput reads those.
    let xinput = path
        .windows(3)
        .any(|w| w == [b'I' as u16, b'G' as u16, b'_' as u16]);
    (!xinput).then_some((GamepadKind::Generic, caps))
}

/// Reads feature report `id` into `buf` (whose length is the report's), waiting at most
/// `IO_TIMEOUT_MS`.
fn get_feature(file: HANDLE, id: u8, buf: &mut [u8]) -> bool {
    buf.fill(0);
    buf[0] = id;
    let len = buf.len() as u32;
    let ptr = buf.as_mut_ptr().cast::<c_void>();
    overlapped(file, |ov, done| {
        // SAFETY: the buffer is both the request (report id) and the reply; it and `ov` outlive
        // the operation (`overlapped` waits or cancels).
        unsafe { DeviceIoControl(file, IOCTL_HID_GET_FEATURE, ptr, len, ptr, len, done, ov) }
    })
}

/// Writes an output report, waiting at most `IO_TIMEOUT_MS`.
fn write(file: HANDLE, report: &[u8]) -> bool {
    overlapped(file, |ov, done| {
        // SAFETY: `report` and `ov` outlive the operation (`overlapped` waits or cancels).
        unsafe { WriteFile(file, report.as_ptr(), report.len() as u32, done, ov) }
    })
}

/// Runs one overlapped operation to completion or timeout. `start` issues it with the given
/// OVERLAPPED and byte-count pointer.
fn overlapped(file: HANDLE, start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32) -> bool {
    // SAFETY: no name; manual reset.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        return false;
    }
    // SAFETY: all-zero is a valid OVERLAPPED.
    let mut ov: OVERLAPPED = unsafe { mem::zeroed() };
    ov.hEvent = event;
    let mut done = 0;
    let mut ok = start(&mut ov, &mut done) != 0;
    // SAFETY: no preconditions.
    if !ok && unsafe { GetLastError() } == ERROR_IO_PENDING {
        // SAFETY: `ov` and its event stay alive until the operation completes: on timeout it is
        // cancelled and waited for.
        unsafe {
            if WaitForSingleObject(event, IO_TIMEOUT_MS) == WAIT_OBJECT_0 {
                ok = GetOverlappedResult(file, &ov, &mut done, 0) != 0;
            } else {
                CancelIoEx(file, &ov);
                GetOverlappedResult(file, &ov, &mut done, 1);
            }
        }
    }
    // SAFETY: created above; the operation is over.
    unsafe { CloseHandle(event) };
    ok
}

/// The reading a fresh connection reports until its first input report.
pub(super) fn connected(kind: GamepadKind) -> GamepadReading {
    GamepadReading {
        kind,
        ..GamepadReading::default()
    }
}
