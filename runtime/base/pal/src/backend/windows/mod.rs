//! Win32 backend. The Xbox backend reuses most of it.

mod error;
#[cfg(marlin_ext = "large_pages")]
mod large_pages;
mod process;
mod vmem;

pub(crate) struct Backend;

impl crate::contract::Platform for Backend {}
