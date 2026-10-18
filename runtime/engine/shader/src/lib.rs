//! Runtime shader containers and permutation lookup.
//!
//! - [`reflect`]: what the shader compiler says about each compiled entry point (0005 §6).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod reflect;
