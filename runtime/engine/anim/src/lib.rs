//! Skeletal animation, blending and IK.
//!
//! So far the cooked [`asset`] forms of skeletons and clips, with a clip sampler.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
mod module;

pub use asset::{AnimationClip, JointPose, Skeleton};
pub use module::AnimModule;
