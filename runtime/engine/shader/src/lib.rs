//! Runtime shader containers and permutation lookup.
//!
//! - [`reflect`]: what the shader compiler says about each compiled entry point (0005 §6).
//! - [`asset`]: shader packages and the libraries the cooker makes of them (0003 §4.4).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
pub mod reflect;
