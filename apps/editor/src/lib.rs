//! The Marlin editor's own code, beside its executable (`main.rs`) so tests reach it.
//!
//! - [`app`]: the editor as a host application ([`app::EditorApp`]).
//! - [`shell`]: the window shell, drawn with the UI renderer.

pub mod app;
pub mod shell;
