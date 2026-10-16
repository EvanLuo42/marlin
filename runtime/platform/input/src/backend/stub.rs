//! Placeholder for platforms without an input backend yet.

use marlin_app::App;

use crate::{Error, ErrorKind, InputState, Key, Result};

pub(crate) enum Input {}

impl crate::contract::Input for Input {
    fn new(_: &App) -> Result<Self> {
        Err(Error::from_kind(ErrorKind::Unsupported))
    }

    fn update(&mut self, _: &mut InputState) {
        match *self {}
    }

    fn set_rumble(&mut self, _: usize, _: f32, _: f32) -> Result<()> {
        match *self {}
    }

    fn key_label<'b>(&self, _: Key, _: &'b mut [u8]) -> Option<&'b str> {
        match *self {}
    }
}
