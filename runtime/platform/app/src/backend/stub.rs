//! Placeholder for platforms without a windowing backend yet: [`App::new`] fails with
//! `Unsupported`, so the other types can never exist.

use core::time::Duration;

use alloc::string::String;
use alloc::vec::Vec;

use crate::{
    CursorGrab, CursorIcon, Display, DisplayId, Error, ErrorKind, Event, EventQueue, Lifecycle,
    Point, Result, Size, Surface, WindowDesc, WindowId, WindowMode,
};

pub(crate) enum App {}
pub(crate) enum Window {}

impl crate::contract::App for App {
    type Window = Window;

    fn new() -> Result<Self> {
        Err(Error::from_kind(ErrorKind::Unsupported))
    }

    fn create_window(&self, _: &WindowDesc<'_>) -> Result<Window> {
        match *self {}
    }

    fn pump(
        &mut self,
        _: Option<Duration>,
        _: &mut EventQueue,
        _: Option<&mut dyn FnMut(&[Event])>,
    ) {
        match *self {}
    }

    fn lifecycle(&self) -> Lifecycle {
        match *self {}
    }

    fn displays(&self, _: &mut Vec<Display>) {
        match *self {}
    }

    fn clipboard_text(&self) -> Result<Option<String>> {
        match *self {}
    }

    fn set_clipboard_text(&self, _: &str) -> Result<()> {
        match *self {}
    }
}

impl crate::contract::Window for Window {
    fn id(&self) -> WindowId {
        match *self {}
    }

    fn surface(&self) -> Surface {
        match *self {}
    }

    fn size(&self) -> Size {
        match *self {}
    }

    fn set_size(&self, _: Size) {
        match *self {}
    }

    fn position(&self) -> Point {
        match *self {}
    }

    fn set_position(&self, _: Point) {
        match *self {}
    }

    fn set_min_size(&self, _: Option<Size>) {
        match *self {}
    }

    fn set_max_size(&self, _: Option<Size>) {
        match *self {}
    }

    fn dpi(&self) -> u32 {
        match *self {}
    }

    fn display(&self) -> Result<Display> {
        match *self {}
    }

    fn set_display(&self, _: DisplayId) -> Result<()> {
        match *self {}
    }

    fn mode(&self) -> WindowMode {
        match *self {}
    }

    fn set_mode(&self, _: WindowMode) -> Result<()> {
        match *self {}
    }

    fn set_title(&self, _: &str) {
        match *self {}
    }

    fn set_visible(&self, _: bool) {
        match *self {}
    }

    fn is_visible(&self) -> bool {
        match *self {}
    }

    fn is_minimized(&self) -> bool {
        match *self {}
    }

    fn is_focused(&self) -> bool {
        match *self {}
    }

    fn set_cursor_visible(&self, _: bool) {
        match *self {}
    }

    fn set_cursor_icon(&self, _: CursorIcon) {
        match *self {}
    }

    fn set_cursor_grab(&self, _: CursorGrab) {
        match *self {}
    }

    fn cursor_grab(&self) -> CursorGrab {
        match *self {}
    }

    fn set_cursor_position(&self, _: Point) -> Result<()> {
        match *self {}
    }

    fn take_dropped_files(&self) -> Vec<String> {
        match *self {}
    }
}
