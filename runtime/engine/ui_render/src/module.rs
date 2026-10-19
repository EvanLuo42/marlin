//! [`UiModule`]: the UI renderer's part in the engine: the font loader and the engine's UI font.

use marlin_asset::{AssetError, AssetServer, Handle, InPlace, LoadState, Vfs};
use marlin_font::Font;
use marlin_jobs::JobSystem;
use marlin_world::{EngineContext, Module, ModuleError, Registry};

/// The engine's UI font (`content/engine/fonts/Inter.masset`).
pub const UI_FONT: &str = "Marlin.Engine.Fonts.Inter";

/// Loads the engine's UI font ([`UI_FONT`]) through `assets` (with the UI module's loaders),
/// blocking until it is there.
pub fn load_ui_font(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
) -> Result<Handle<InPlace<Font>>, AssetError> {
    let uuid = vfs.named(UI_FONT).ok_or(AssetError::NotFound)?;
    let handle = assets.load::<InPlace<Font>>(uuid);
    assets.update_blocking(vfs, jobs);
    match assets.state(&handle) {
        LoadState::Loaded => Ok(handle),
        LoadState::Failed(e) => Err(e),
        LoadState::Loading => Err(AssetError::NotFound),
    }
}

/// The UI's engine assets (an engine resource once the UI module has started): the UI font,
/// shared, so whoever draws UI (the editor, the stats overlay) has it without the asset server.
#[derive(Clone)]
pub struct UiAssets {
    pub font: InPlace<Font>,
}

/// The UI module: fonts load in place; the engine's UI font loads at startup ([`UiAssets`]).
pub struct UiModule;

impl UiModule {
    pub const NAME: &'static str = "Ui";
}

impl Module for UiModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn register(&self, r: &mut Registry) {
        r.add_loaders(|loaders, _| loaders.register_in_place::<Font>());
    }

    fn startup(&mut self, cx: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        let fail =
            |e: AssetError| ModuleError::new(Self::NAME, alloc::format!("the UI font: {e:?}"));
        let handle = load_ui_font(cx.assets, cx.vfs, Some(cx.jobs)).map_err(fail)?;
        let font = cx
            .assets
            .get(&handle)
            .ok_or(AssetError::NotFound)
            .map_err(fail)?
            .clone();
        cx.resources.insert(UiAssets { font });
        Ok(())
    }
}
