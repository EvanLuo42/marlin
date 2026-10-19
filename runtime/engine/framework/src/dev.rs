//! The game loop's development tools, compiled out of shipping builds (`marlin_build =
//! "shipping"`, 0001 §8): the stats overlay (`r.stats`, F3, [`hud`](crate::hud)) and the frame
//! rate in the window's title.
//!
//! [`DevTools`] is the loop's one handle on them. In shipping it is an empty stub with the same
//! methods, which do nothing: the loop has no `cfg`s, and nothing of the tools (their code, cvars,
//! keys or strings) is in the binary.

#[cfg(not(marlin_build = "shipping"))]
pub(crate) use tools::DevTools;

#[cfg(marlin_build = "shipping")]
pub(crate) use stub::DevTools;

#[cfg(not(marlin_build = "shipping"))]
mod tools {
    use alloc::string::String;
    use alloc::sync::Arc;
    use core::fmt::Write;
    use core::time::Duration;

    use marlin_app::Window;
    use marlin_asset::{AssetServer, BlobStreamer, Handle, Vfs};
    use marlin_diag::{FixedBuf, chain, warn};
    use marlin_font::Font;
    use marlin_input::{InputState, Key};
    use marlin_jobs::JobSystem;
    use marlin_render_graph::{FrameContext, RenderGraph, TextureHandle};
    use marlin_rhi::{Device, Format};
    use marlin_shader::asset::ShaderLibrary;
    use marlin_time::Instant;

    use crate::boot::R_STATS;
    use crate::hud::{Hud, HudFrame, HudInfo, Snapshot};
    use crate::loaders::{InPlace, engine_loaders};
    use crate::shaders::load_ui_font;

    /// The development tools' state.
    pub(crate) struct DevTools<'v> {
        vfs: &'v Vfs,
        adapter: String,
        show: bool,
        /// Made the first time the overlay is shown; with the font, from an asset server of its
        /// own (the engine's is the simulation's while a frame renders).
        hud: Option<(Hud, Handle<InPlace<Font>>)>,
        assets: AssetServer,
        /// Frames since `window.0`, for the frame rate.
        window: (Instant, u32),
    }

    /// One frame of the tools: the overlay's, when it is shown.
    pub(crate) struct DevFrame(Option<HudFrame>);

    impl<'v> DevTools<'v> {
        /// The tools of a game whose content is `vfs`, on the adapter `adapter`.
        pub(crate) fn new(vfs: &'v Vfs, streamer: &Arc<BlobStreamer>, adapter: &str) -> Self {
            Self {
                vfs,
                adapter: String::from(adapter),
                show: R_STATS.get(),
                hud: None,
                assets: AssetServer::new(engine_loaders(streamer)),
                window: (Instant::now(), 0),
            }
        }

        /// The start of a frame `interval` seconds after the last: F3 toggles the overlay, made
        /// the first time it is shown from the engine's shaders `library`, drawing over
        /// `output`.
        pub(crate) fn begin_frame(
            &mut self,
            input: &InputState,
            interval: f32,
            device: &Device,
            library: Option<&ShaderLibrary>,
            output: Format,
            jobs: &JobSystem,
        ) {
            if input.key_pressed(Key::F3) {
                self.show = !self.show;
            }
            if self.show && self.hud.is_none() {
                let hud = library.and_then(|library| {
                    Hud::new(device, library, output)
                        .map_err(|e| warn!("stats overlay: {}", chain(&e)))
                        .ok()
                });
                let font = load_ui_font(&mut self.assets, self.vfs, Some(jobs))
                    .map_err(|e| warn!("stats overlay font: {e:?}"))
                    .ok();
                match (hud, font) {
                    (Some(hud), Some(font)) => self.hud = Some((hud, font)),
                    // Not to be tried every frame.
                    _ => self.show = false,
                }
            }
            if let Some((hud, _)) = self.hud.as_mut().filter(|_| self.show) {
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
            let Some((hud, font)) = self.hud.as_mut().filter(|_| self.show) else {
                return Ok(DevFrame(None));
            };
            let Some(font) = self.assets.get(font).map(|f| f.view()) else {
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
                font,
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
                (Some((hud, _)), Some(frame)) => hud.add_passes(g, frame, target),
                _ => target,
            }
        }

        /// The end of a frame begun at `now`: once a second, the frame rate into `window`'s title
        /// (after `title`) and the profiler's numbers into the overlay.
        pub(crate) fn end_frame(&mut self, now: Instant, window: &Window, title: &str) {
            self.window.1 += 1;
            let elapsed = now.duration_since(self.window.0);
            if elapsed < Duration::from_secs(1) {
                return;
            }
            let fps = self.window.1 as f32 / elapsed.as_secs_f32();
            let mut text = FixedBuf::<96>::new();
            let _ = write!(text, "{title}  {fps:.0} fps");
            window.set_title(text.as_str());
            if let Some((hud, _)) = self.hud.as_mut().filter(|_| self.show) {
                hud.set_snapshot(Snapshot::take(fps));
            }
            self.window = (now, 0);
        }

        /// Makes `frames` forget the tools' resources; call before dropping them.
        pub(crate) fn forget(&self, frames: &mut FrameContext) {
            if let Some((hud, _)) = &self.hud {
                hud.forget(frames);
            }
        }
    }
}

#[cfg(marlin_build = "shipping")]
mod stub {
    use alloc::sync::Arc;
    use core::marker::PhantomData;

    use marlin_app::Window;
    use marlin_asset::{BlobStreamer, Vfs};
    use marlin_input::InputState;
    use marlin_jobs::JobSystem;
    use marlin_render_graph::{FrameContext, RenderGraph, TextureHandle};
    use marlin_rhi::{Device, Format};
    use marlin_shader::asset::ShaderLibrary;
    use marlin_time::Instant;

    /// No tools in shipping builds.
    pub(crate) struct DevTools<'v>(PhantomData<&'v Vfs>);

    pub(crate) struct DevFrame;

    impl<'v> DevTools<'v> {
        #[inline]
        pub(crate) fn new(_: &'v Vfs, _: &Arc<BlobStreamer>, _: &str) -> Self {
            Self(PhantomData)
        }

        #[inline]
        pub(crate) fn begin_frame(
            &mut self,
            _: &InputState,
            _: f32,
            _: &Device,
            _: Option<&ShaderLibrary>,
            _: Format,
            _: &JobSystem,
        ) {
        }

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
