//! [`AnimModule`]: animation's part in the engine: skeletons and clips load in place.

use marlin_world::{Module, Registry};

use crate::asset::{AnimationClip, Skeleton};

/// The animation module.
pub struct AnimModule;

impl AnimModule {
    pub const NAME: &'static str = "Anim";
}

impl Module for AnimModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn register(&self, r: &mut Registry) {
        r.add_loaders(|loaders, _| {
            loaders.register_in_place::<Skeleton>();
            loaders.register_in_place::<AnimationClip>();
        });
    }
}
