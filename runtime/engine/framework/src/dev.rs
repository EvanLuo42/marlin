//! The host's development tools, compiled out of shipping builds (`marlin_build = "shipping"`,
//! 0001 §8): the stats overlay (`r.stats`, the host's stats command, [`hud`](crate::hud)) and the
//! frame rate in the window's title.
//!
//! [`DevToolsModule`] is the tools' module: it registers their cvars, and a host draws the overlay
//! ([`DevTools`]) only when its build has the module. Shipping builds list no such module, and
//! [`DevTools`] is an empty stub with the same methods there: the loop has no `cfg`s, and nothing
//! of the tools (their code, cvars, keys or strings) is in the binary.

#[cfg(not(marlin_build = "shipping"))]
pub(crate) use tools::DevTools;
#[cfg(not(marlin_build = "shipping"))]
pub use tools::{DevToolsModule, R_STATS};

#[cfg(marlin_build = "shipping")]
pub(crate) use stub::DevTools;

#[cfg(not(marlin_build = "shipping"))]
mod tools {
    use alloc::string::String;
    use core::fmt::Write;
    use core::time::Duration;

    use marlin_app::Window;
    use marlin_config::CvarBool;
    use marlin_diag::{FixedBuf, chain, warn};
    use marlin_jobs::JobSystem;
    use marlin_render_graph::{FrameContext, RenderGraph, TextureHandle};
    use marlin_rhi::{Device, Format};
    use marlin_time::Instant;
    use marlin_world::{Module, Registry};

    use crate::Engine;
    use crate::hud::{Hud, HudFrame, HudInfo, Snapshot};

    pub static R_STATS: CvarBool = CvarBool::new(
        "r.stats",
        false,
        "Show the stats overlay: frame rate, CPU and GPU times, the costliest passes (F3 toggles).",
    );

    /// The development tools' module: their cvars. A host with it draws [`DevTools`].
    pub struct DevToolsModule;

    impl DevToolsModule {
        pub const NAME: &'static str = "DevTools";
    }

    impl Module for DevToolsModule {
        fn name(&self) -> &'static str {
            Self::NAME
        }

        fn register(&self, r: &mut Registry) {
            r.add_cvars(&[&R_STATS]);
        }
    }

    /// What the overlay draws with: the engine's shader library and UI font (the render and UI
    /// modules' engine resources).
    struct DevAssets {
        library: alloc::sync::Arc<marlin_shader::asset::ShaderLibrary>,
        font: marlin_asset::InPlace<marlin_font::Font>,
    }

    impl DevAssets {
        fn of(engine: &Engine<'_>) -> Option<Self> {
            Some(Self {
                library: engine
                    .resources()
                    .get::<marlin_renderer::RenderShaders>()?
                    .library
                    .clone(),
                font: engine
                    .resources()
                    .get::<marlin_ui_render::UiAssets>()?
                    .font
                    .clone(),
            })
        }
    }

    /// The development tools' state.
    pub(crate) struct DevTools {
        /// What the overlay draws with; none when the build lacks the tools' module (or the
        /// engine's UI assets).
        assets: Option<DevAssets>,
        adapter: String,
        show: bool,
        /// Made the first time the overlay is shown.
        hud: Option<Hud>,
        /// Frames since `window.0`, for the frame rate.
        window: (Instant, u32),
    }

    /// One frame of the tools: the overlay's, when it is shown.
    pub(crate) struct DevFrame(Option<HudFrame>);

    impl DevTools {
        /// The tools of `engine` on the adapter `adapter`: inert when the engine lacks their module
        /// (or the render and UI modules, whose engine shaders and UI font the overlay draws with).
        pub(crate) fn new(engine: &Engine<'_>, adapter: &str) -> Self {
            let assets = engine
                .modules()
                .has(DevToolsModule::NAME)
                .then(|| DevAssets::of(engine))
                .flatten();
            Self {
                show: assets.is_some() && R_STATS.get(),
                assets,
                adapter: String::from(adapter),
                hud: None,
                window: (Instant::now(), 0),
            }
        }

        /// The start of a frame `interval` seconds after the last: `toggle` (the host's stats
        /// command) shows or hides the overlay, made the first time it is shown, drawing over
        /// `output`.
        pub(crate) fn begin_frame(
            &mut self,
            toggle: bool,
            interval: f32,
            device: &Device,
            output: Format,
        ) {
            let Some(assets) = &self.assets else {
                return;
            };
            if toggle {
                self.show = !self.show;
            }
            if self.show && self.hud.is_none() {
                match Hud::new(device, &assets.library, output) {
                    Ok(hud) => self.hud = Some(hud),
                    Err(e) => {
                        warn!("stats overlay: {}", chain(&e));
                        // Not to be tried every frame.
                        self.show = false;
                    }
                }
            }
            if let Some(hud) = self.hud.as_mut().filter(|_| self.show) {
                hud.push_interval(interval);
            }
        }

        /// This frame's overlay for `window`, drawn (on the CPU) before its passes are declared.
        pub(crate) fn prepare(
            &mut self,
            device: &Device,
            frames: &mut FrameContext,
            window: &Window,
            vsync: bool,
            jobs: &JobSystem,
        ) -> marlin_rhi::Result<DevFrame> {
            let (Some(hud), Some(assets)) = (self.hud.as_mut().filter(|_| self.show), &self.assets)
            else {
                return Ok(DevFrame(None));
            };
            marlin_profile::profile_scope!("Overlay");
            let size = window.size();
            let stats = frames.stats();
            let info = HudInfo {
                adapter: &self.adapter,
                width: size.width,
                height: size.height,
                vsync,
                passes: stats.map_or(0, |s| s.passes),
                lists: stats.map_or(0, |s| s.lists),
            };
            let frame = hud.frame(
                device,
                frames,
                assets.font.view(),
                window.scale_factor(),
                &info,
                Some(jobs),
            )?;
            Ok(DevFrame(Some(frame)))
        }

        /// Declares `frame` over `target`, the window's image; returns `target` after it.
        pub(crate) fn add_passes<'g>(
            &'g self,
            g: &mut RenderGraph<'g>,
            frame: &'g DevFrame,
            target: TextureHandle<'g>,
        ) -> TextureHandle<'g> {
            match (&self.hud, &frame.0) {
                (Some(hud), Some(frame)) => hud.add_passes(g, frame, target),
                _ => target,
            }
        }

        /// The end of a frame begun at `now`: once a second, the frame rate into `window`'s title
        /// (after `title`) and the profiler's numbers into the overlay.
        pub(crate) fn end_frame(&mut self, now: Instant, window: &Window, title: &str) {
            if self.assets.is_none() {
                return;
            }
            self.window.1 += 1;
            let elapsed = now.duration_since(self.window.0);
            if elapsed < Duration::from_secs(1) {
                return;
            }
            let fps = self.window.1 as f32 / elapsed.as_secs_f32();
            let mut text = FixedBuf::<96>::new();
            let _ = write!(text, "{title}  {fps:.0} fps");
            window.set_title(text.as_str());
            if let Some(hud) = self.hud.as_mut().filter(|_| self.show) {
                hud.set_snapshot(Snapshot::take(fps));
            }
            self.window = (now, 0);
        }

        /// Makes `frames` forget the tools' resources; call before dropping them.
        pub(crate) fn forget(&self, frames: &mut FrameContext) {
            if let Some(hud) = &self.hud {
                hud.forget(frames);
            }
        }
    }
}

#[cfg(marlin_build = "shipping")]
mod stub {
    use marlin_app::Window;
    use marlin_jobs::JobSystem;
    use marlin_render_graph::{FrameContext, RenderGraph, TextureHandle};
    use marlin_rhi::{Device, Format};
    use marlin_time::Instant;

    use crate::Engine;

    /// No tools in shipping builds.
    pub(crate) struct DevTools;

    pub(crate) struct DevFrame;

    impl DevTools {
        #[inline]
        pub(crate) fn new(_: &Engine<'_>, _: &str) -> Self {
            Self
        }

        #[inline]
        pub(crate) fn begin_frame(&mut self, _: bool, _: f32, _: &Device, _: Format) {}

        #[inline]
        pub(crate) fn prepare(
            &mut self,
            _: &Device,
            _: &mut FrameContext,
            _: &Window,
            _: bool,
            _: &JobSystem,
        ) -> marlin_rhi::Result<DevFrame> {
            Ok(DevFrame)
        }

        #[inline]
        pub(crate) fn add_passes<'g>(
            &'g self,
            _: &mut RenderGraph<'g>,
            _: &'g DevFrame,
            target: TextureHandle<'g>,
        ) -> TextureHandle<'g> {
            target
        }

        #[inline]
        pub(crate) fn end_frame(&mut self, _: Instant, _: &Window, _: &str) {}

        #[inline]
        pub(crate) fn forget(&self, _: &mut FrameContext) {}
    }
}
