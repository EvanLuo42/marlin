//! Win32 windows. Each test opens real windows, so they need a desktop session:
//! `cargo test -p marlin-app -- --ignored`. Some briefly appear on screen.
#![cfg(marlin_platform = "windows")]

use std::ptr;

use marlin_app::os::windows::WindowExt;
use marlin_app::{App, CursorGrab, Event, Point, Size, Surface, Window, WindowDesc, WindowMode};
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Shell::DROPFILES;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetClipCursor, GetWindow, GetWindowLongPtrW, GetWindowRect,
    PostMessageW, SW_MINIMIZE, SW_RESTORE, SendMessageW, SetForegroundWindow, ShowWindow, WM_CLOSE,
    WM_DPICHANGED, WM_DROPFILES, WS_EX_APPWINDOW, WS_POPUP,
};

fn hidden(title: &str) -> WindowDesc<'_> {
    WindowDesc {
        title,
        width: 320,
        height: 200,
        visible: false,
        ..WindowDesc::default()
    }
}

#[test]
#[ignore = "needs a desktop session"]
fn window_lifecycle() {
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
#[ignore = "needs a desktop session"]
fn position_size_and_limits() {
    let mut app = App::new().unwrap();
    let window = app.create_window(&hidden("placement")).unwrap();
    let id = window.id();
    app.pump();

    let work = window.display().unwrap().work_area;
    let target = Point::new(work.x + 40, work.y + 50);
    window.set_position(target);
    assert_eq!(window.position(), target);
    let events = app.pump();
    assert!(events.contains(&Event::Moved {
        window: id,
        position: target,
    }));

    window.set_size(Size::new(400, 300));
    assert_eq!(window.size(), Size::new(400, 300));
    assert!(app.pump().contains(&Event::Resized {
        window: id,
        size: Size::new(400, 300),
    }));
    // Resizing keeps the client origin.
    assert_eq!(window.position(), target);

    // Limits clamp both the current size and later requests.
    window.set_min_size(Some(Size::new(500, 350)));
    assert_eq!(window.size(), Size::new(500, 350));
    window.set_max_size(Some(Size::new(600, 400)));
    window.set_size(Size::new(2000, 2000));
    assert_eq!(window.size(), Size::new(600, 400));
    window.set_size(Size::new(10, 10));
    assert_eq!(window.size(), Size::new(500, 350));
    window.set_min_size(None);
    window.set_max_size(None);
    window.set_size(Size::new(200, 150));
    assert_eq!(window.size(), Size::new(200, 150));
}

#[test]
#[ignore = "needs a desktop session"]
fn windows_open_on_the_chosen_display() {
    let mut app = App::new().unwrap();
    let displays = app.displays();
    for display in &displays {
        let window = app
            .create_window(&WindowDesc {
                display: Some(display.id),
                ..hidden("display")
            })
            .unwrap();
        assert_eq!(window.display().unwrap().id, display.id);
        assert!(display.work_area.contains(window.position()));

        // Fullscreen covers the window's display.
        window.set_mode(WindowMode::BorderlessFullscreen).unwrap();
        let size = Size::new(display.bounds.width, display.bounds.height);
        assert_eq!(window.size(), size);
        assert_eq!(
            window.position(),
            Point::new(display.bounds.x, display.bounds.y)
        );

        // Moving a fullscreen window to every display covers that display.
        for other in &displays {
            window.set_display(other.id).unwrap();
            assert_eq!(window.display().unwrap().id, other.id);
            assert_eq!(
                window.position(),
                Point::new(other.bounds.x, other.bounds.y)
            );
        }
        window.set_mode(WindowMode::Windowed).unwrap();
        window.set_display(display.id).unwrap();
        assert_eq!(window.display().unwrap().id, display.id);
        app.pump();
    }
}

#[test]
#[ignore = "needs a desktop session"]
fn minimize_restore_and_visibility() {
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
#[ignore = "needs a desktop session"]
fn dpi_change_rescales() {
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
#[ignore = "needs a desktop session"]
fn file_drop() {
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
#[ignore = "needs a desktop session"]
fn owned_and_undecorated_windows() {
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
#[ignore = "needs a desktop session"]
fn cursor_grab_clips_while_focused() {
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
