//! The Win32 backend: lifecycle notifications sent to the hidden helper window exactly as Windows
//! sends them (sleep, session lock, display off), display device names, and windows driven
//! through Win32 messages and queried through Win32 calls.

use std::ptr;

use marlin_app::os::windows::{AppExt, WindowExt};
use marlin_app::{
    App, CursorGrab, Event, Lifecycle, Point, Size, Surface, Window, WindowDesc, WindowMode,
};
use marlin_test_support::one_at_a_time;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Power::POWERBROADCAST_SETTING;
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::UI::Shell::DROPFILES;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetClipCursor, GetWindow, GetWindowLongPtrW, GetWindowRect,
    PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, PostMessageW, SW_MINIMIZE,
    SW_RESTORE, SendMessageW, SetForegroundWindow, ShowWindow, WM_CLOSE, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_DROPFILES, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WS_EX_APPWINDOW,
    WS_POPUP, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use crate::hidden;

fn send(app: &App, msg: u32, wparam: usize, lparam: isize) {
    // SAFETY: sending to the app's live helper window on its own thread; pointer lparams point to
    // live locals.
    unsafe { SendMessageW(app.message_window(), msg, wparam, lparam) };
}

#[test]
fn suspend_and_resume() {
    let mut app = App::new().unwrap();
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    // Windows tells every top-level window; reported once.
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    assert_eq!(app.pump(), &[Event::Suspended]);
    assert_eq!(app.lifecycle(), Lifecycle::Suspended);

    send(&app, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize, 0);
    assert_eq!(app.pump(), &[Event::Resumed]);
    assert_eq!(app.lifecycle(), Lifecycle::Running);
}

/// A `PBT_POWERSETTINGCHANGE` for the console display state (0 off, 1 on, 2 dimmed).
fn display_state(app: &App, state: u32) {
    #[repr(C)]
    struct Setting {
        header: POWERBROADCAST_SETTING,
        rest: [u8; 3],
    }
    let mut setting = Setting {
        header: POWERBROADCAST_SETTING {
            PowerSetting: GUID_CONSOLE_DISPLAY_STATE,
            DataLength: 4,
            Data: [0],
        },
        rest: [0; 3],
    };
    let data = state.to_le_bytes();
    setting.header.Data[0] = data[0];
    setting.rest.copy_from_slice(&data[1..]);
    send(
        app,
        WM_POWERBROADCAST,
        PBT_POWERSETTINGCHANGE as usize,
        (&raw const setting) as isize,
    );
}

#[test]
fn constrained_while_locked_disconnected_or_display_off() {
    let mut app = App::new().unwrap();
    send(&app, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize, 0);
    assert_eq!(app.pump(), &[Event::Constrained]);
    assert_eq!(app.lifecycle(), Lifecycle::Constrained);

    // Further reasons while constrained change nothing visible.
    display_state(&app, 0);
    send(
        &app,
        WM_WTSSESSION_CHANGE,
        WTS_REMOTE_DISCONNECT as usize,
        0,
    );
    assert!(app.pump().is_empty());

    // Dimmed counts as on; constrained until the last reason clears.
    display_state(&app, 2);
    send(&app, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize, 0);
    assert!(app.pump().is_empty());
    assert_eq!(app.lifecycle(), Lifecycle::Constrained);
    send(&app, WM_WTSSESSION_CHANGE, WTS_REMOTE_CONNECT as usize, 0);
    assert_eq!(app.pump(), &[Event::Unconstrained]);
    assert_eq!(app.lifecycle(), Lifecycle::Running);

    // Suspended takes precedence.
    display_state(&app, 0);
    send(&app, WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
    assert_eq!(app.pump(), &[Event::Constrained, Event::Suspended]);
    assert_eq!(app.lifecycle(), Lifecycle::Suspended);
    send(&app, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize, 0);
    display_state(&app, 1);
    assert_eq!(app.pump(), &[Event::Resumed, Event::Unconstrained]);
}

#[test]
fn display_changes_coalesce() {
    let mut app = App::new().unwrap();
    for _ in 0..3 {
        send(&app, WM_DISPLAYCHANGE, 32, 0);
    }
    assert_eq!(app.pump(), &[Event::DisplaysChanged]);
}

#[test]
fn display_device_names() {
    let app = App::new().unwrap();
    for d in &app.displays() {
        assert!(d.device_name().starts_with(r"\\.\DISPLAY"), "{d:?}");
    }
}

#[test]
fn window_lifecycle() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app.create_window(&hidden("marlin-app test")).unwrap();

    let Surface::Win32 { hwnd, .. } = window.surface() else {
        unreachable!("Win32 window")
    };
    assert_eq!(hwnd.as_ptr(), window.hwnd());
    let scale = window.dpi() as f32 / 96.0;
    assert_eq!(window.scale_factor(), scale);
    let size = window.size();
    assert_eq!(size.width, (320.0 * scale) as u32);
    assert_eq!(size.height, (200.0 * scale) as u32);
    assert_eq!(window.mode(), WindowMode::Windowed);
    assert!(!window.is_visible());
    window.set_title("renamed — 名前");

    // Borderless fullscreen and back restores the windowed size.
    window.set_mode(WindowMode::BorderlessFullscreen).unwrap();
    assert_eq!(window.mode(), WindowMode::BorderlessFullscreen);
    let events = app.pump();
    assert!(events.iter().any(|e| matches!(e, Event::Resized { .. })));
    let display = window.display().unwrap();
    assert_eq!(
        window.size(),
        Size::new(display.bounds.width, display.bounds.height)
    );
    window.toggle_fullscreen().unwrap();
    assert_eq!(window.mode(), WindowMode::Windowed);
    app.pump();
    assert_eq!(window.size(), size);

    // Closing asks; the window stays until dropped.
    let id = window.id();
    // SAFETY: posting WM_CLOSE to our own live window.
    unsafe { PostMessageW(window.hwnd(), WM_CLOSE, 0, 0) };
    assert!(app.pump().contains(&Event::CloseRequested(id)));
    drop(window);
    app.pump();
    assert_eq!(app.dropped_events(), 0);
}

#[test]
fn minimize_restore_and_visibility() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app.create_window(&hidden("minimize")).unwrap();
    let id = window.id();
    app.pump();

    window.set_visible(true);
    assert!(window.is_visible());
    assert!(app.pump().contains(&Event::VisibilityChanged {
        window: id,
        visible: true,
    }));

    // SAFETY: our own live window.
    unsafe { ShowWindow(window.hwnd(), SW_MINIMIZE) };
    assert!(window.is_minimized());
    assert!(app.pump().contains(&Event::Minimized(id)));
    // SAFETY: as above.
    unsafe { ShowWindow(window.hwnd(), SW_RESTORE) };
    assert!(!window.is_minimized());
    let events = app.pump();
    assert!(events.contains(&Event::Restored(id)), "{events:?}");

    window.set_visible(false);
    assert!(!window.is_visible());
    assert!(app.pump().contains(&Event::VisibilityChanged {
        window: id,
        visible: false,
    }));
}

#[test]
fn dpi_change_rescales() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app.create_window(&hidden("dpi")).unwrap();
    let id = window.id();
    app.pump();
    let original = window.dpi();
    let new_dpi = if original == 192 { 144 } else { 192 };

    // What Windows sends when the window crosses to a display of another scale: the new DPI and
    // a suggested window rectangle.
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: our own live window; valid out pointer.
    unsafe { GetWindowRect(window.hwnd(), &mut r) };
    let factor = |v: i32| v * new_dpi as i32 / original as i32;
    let suggested = RECT {
        left: r.left,
        top: r.top,
        right: r.left + factor(r.right - r.left),
        bottom: r.top + factor(r.bottom - r.top),
    };
    // SAFETY: as above; `suggested` outlives the synchronous send.
    unsafe {
        SendMessageW(
            window.hwnd(),
            WM_DPICHANGED,
            (new_dpi | new_dpi << 16) as usize,
            (&raw const suggested) as isize,
        )
    };
    assert_eq!(window.dpi(), new_dpi);
    let events = app.pump();
    assert_eq!(
        events[0],
        Event::DpiChanged {
            window: id,
            dpi: new_dpi,
        }
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Resized { window, .. } if *window == id))
    );
}

#[test]
fn file_drop() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app
        .create_window(&WindowDesc {
            accept_files: true,
            ..hidden("drop")
        })
        .unwrap();
    app.pump();

    // An HDROP as the shell builds it: DROPFILES, then NUL-separated wide paths and a final NUL.
    let paths = [r"C:\assets\ゲーム.png", r"D:\level one.mlevel"];
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(p.encode_utf16());
        wide.push(0);
    }
    wide.push(0);
    let header = size_of::<DROPFILES>();
    // SAFETY: a zeroed movable block large enough for the header and paths; the window procedure
    // frees it with DragFinish.
    unsafe {
        let memory = GlobalAlloc(GHND, header + wide.len() * 2);
        let data = GlobalLock(memory).cast::<u8>();
        data.cast::<DROPFILES>().write_unaligned(DROPFILES {
            pFiles: header as u32,
            pt: POINT { x: 12, y: 34 },
            fNC: 0,
            fWide: 1,
        });
        ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), data.add(header), wide.len() * 2);
        GlobalUnlock(memory);
        PostMessageW(window.hwnd(), WM_DROPFILES, memory as usize, 0);
    }
    let events = app.pump();
    assert!(events.contains(&Event::FilesDropped {
        window: window.id(),
        count: 2,
        position: Point::new(12, 34),
    }));
    assert_eq!(window.take_dropped_files(), paths);
    assert!(window.take_dropped_files().is_empty());
}

#[test]
fn owned_and_undecorated_windows() {
    let _desktop = one_at_a_time();
    let app = App::new().unwrap();
    let owner = app.create_window(&hidden("owner")).unwrap();
    let tool: Window = app
        .create_window(&WindowDesc {
            owner: Some(&owner),
            decorated: false,
            ..hidden("tool")
        })
        .unwrap();
    // SAFETY: querying our own live windows.
    unsafe {
        assert_eq!(GetWindow(tool.hwnd(), GW_OWNER), owner.hwnd());
        assert_eq!(
            GetWindowLongPtrW(tool.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_APPWINDOW,
            0
        );
        assert_ne!(
            GetWindowLongPtrW(owner.hwnd(), GWL_EXSTYLE) as u32 & WS_EX_APPWINDOW,
            0
        );
        assert_ne!(
            GetWindowLongPtrW(tool.hwnd(), GWL_STYLE) as u32 & WS_POPUP,
            0
        );
    }
    let scale = tool.scale_factor();
    assert_eq!(tool.size().width, (320.0 * scale) as u32);
    assert_eq!(
        format!("{tool:?}"),
        format!("Window {{ id: {:?} }}", tool.id())
    );
}

#[test]
fn cursor_grab_clips_while_focused() {
    let _desktop = one_at_a_time();
    let mut app = App::new().unwrap();
    let window = app
        .create_window(&WindowDesc {
            visible: true,
            ..hidden("cursor")
        })
        .unwrap();
    // SAFETY: our own live window. Windows may refuse to move the foreground to a background
    // process; then only the bookkeeping is checked.
    unsafe { SetForegroundWindow(window.hwnd()) };
    app.pump();

    let clip = || {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: valid out pointer.
        unsafe { GetClipCursor(&mut r) };
        r
    };
    window.set_cursor_grab(CursorGrab::Confine);
    assert_eq!(window.cursor_grab(), CursorGrab::Confine);
    let origin = window.position();
    let size = window.size();
    if window.is_focused() {
        let r = clip();
        assert_eq!((r.left, r.top), (origin.x, origin.y));
        assert_eq!(
            ((r.right - r.left) as u32, (r.bottom - r.top) as u32),
            (size.width, size.height)
        );
        window.set_cursor_grab(CursorGrab::Lock);
        let r = clip();
        assert_eq!((r.right - r.left, r.bottom - r.top), (1, 1));
    } else {
        eprintln!("window did not get focus; clip rectangle not checked");
    }
    window.set_cursor_grab(CursorGrab::None);
    window.set_cursor_visible(false);
    window.set_cursor_visible(true);
    window.set_cursor_position(Point::new(5, 5)).unwrap();
}
