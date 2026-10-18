//! GPU test helpers shared by every crate that tests on the GPU (dev-dependency only; conventions in
//! docs/design/0014-testing.md §6.2): the shared hardware devices, submit-and-wait, buffers and
//! readback, common render targets and pipelines, the test shaders, and golden images
//! ([`assert_image!`], [`read_texture`]).
//!
//! Test shaders are Marlin shaders, compiled at run time by the Marlin shader compiler the
//! first time a test asks for one ([`shader`], [`Shader`], [`crate_shader!`]): the shared ones
//! in this crate's `shaders/`, a crate's own in its `tests/data/shaders/`.

pub mod image;
mod shaders;

pub use image::{Image, Tolerance, read_texture};
pub use marlin_shaderc::ConstValue;
pub use shaders::{ENGINE, SHARED, Shader, shader};

use marlin_rhi::{
    AdapterPreference, Buffer, BufferDesc, BufferUsage, CommandList, Device, DeviceDesc,
    GRAPHICS_API, GraphicsApi, Instance, InstanceDesc, MemoryLocation, QueueType,
};

/// `DeviceDesc::disable_enhanced_barriers` for each barrier path the backend has: D3D12 has
/// enhanced barriers and the legacy resource-state ones, every other backend one path (Vulkan's
/// synchronization2). Known from the API, without creating a device to ask.
fn barrier_paths() -> &'static [bool] {
    match GRAPHICS_API {
        GraphicsApi::D3D12 => &[false, true],
        _ => &[false],
    }
}

/// The hardware adapter with validation on, one device per barrier path the backend has (D3D12:
/// enhanced barriers where supported, then legacy ones; Vulkan: one). Tests iterate the slice
/// rather than index it. GPU tests run on real hardware, never on a software rasterizer.
///
/// Created once per test binary, shared by its tests and never dropped, as an application keeps
/// its device: tearing down devices while other threads use the same adapter is not what
/// engines do, and it has deadlocked the NVIDIA driver (when another thread created pipelines
/// from cached blobs).
pub fn devices() -> &'static [Device] {
    static DEVICES: std::sync::OnceLock<Vec<Device>> = std::sync::OnceLock::new();
    DEVICES.get_or_init(|| {
        let instance = Instance::new(&InstanceDesc {
            validation: true,
            ..InstanceDesc::default()
        })
        .unwrap();
        barrier_paths()
            .iter()
            .map(|&legacy| {
                instance
                    .create_device(&DeviceDesc {
                        adapter: AdapterPreference::HighPerformance,
                        disable_enhanced_barriers: legacy,
                    })
                    .unwrap()
            })
            .collect()
    })
}

/// The first shared device (see [`devices`]): enhanced barriers on D3D12.
pub fn hardware_device() -> &'static Device {
    &devices()[0]
}

/// Records with `record`, submits, and waits for the GPU.
pub fn run(device: &Device, record: impl FnOnce(&mut CommandList)) {
    run_on(device, QueueType::Graphics, record);
}

/// As [`run`], on the queue that runs `ty` work.
pub fn run_on(device: &Device, ty: QueueType, record: impl FnOnce(&mut CommandList)) {
    let mut pool = device.create_command_pool_for(ty).unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    let fence = device.create_fence(0).unwrap();
    list.begin(&mut pool).unwrap();
    record(&mut list);
    list.end().unwrap();
    // SAFETY: everything the list uses outlives the wait below.
    unsafe {
        device
            .queue_for(ty)
            .submit(&[&list], Some((&fence, 1)))
            .unwrap()
    };
    fence.wait(1, None).unwrap();
}

pub fn buffer(device: &Device, size: u64, usage: BufferUsage, memory: MemoryLocation) -> Buffer {
    device
        .create_buffer(&BufferDesc {
            size,
            usage,
            memory,
        })
        .unwrap()
}

/// An upload buffer holding `bytes`.
pub fn upload(device: &Device, bytes: &[u8], usage: BufferUsage) -> Buffer {
    let b = buffer(
        device,
        bytes.len() as u64,
        usage | BufferUsage::COPY_SRC,
        MemoryLocation::Upload,
    );
    // SAFETY: no GPU work uses it yet.
    unsafe { b.write(0, bytes) };
    b
}

pub fn readback(device: &Device, size: u64) -> Buffer {
    buffer(
        device,
        size,
        BufferUsage::COPY_DST,
        MemoryLocation::Readback,
    )
}

pub fn read_bytes(buffer: &Buffer, offset: u64, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    // SAFETY: the GPU work that wrote it has finished (the callers wait).
    unsafe { buffer.read(offset, &mut bytes) };
    bytes
}

pub fn read_words(buffer: &Buffer, offset: u64, count: usize) -> Vec<u32> {
    read_bytes(buffer, offset, count * 4)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&w| u32::from_le_bytes(w))
        .collect()
}

pub fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// `T`'s bytes, for `#[repr(C)]` plain-data argument structs.
pub fn bytes_of<T: Copy>(values: &[T]) -> Vec<u8> {
    let len = size_of_val(values);
    // SAFETY: `T` is plain data without padding (the RHI's indirect argument structs).
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), len) }.to_vec()
}

/// Columns in a [`ColumnTarget`], and each column's width and the target's height in pixels.
pub const COLUMNS: u32 = 16;
pub const COLUMN_WIDTH: u32 = 8;
pub const COLUMN_HEIGHT: u32 = 8;

/// An R32Uint render target of [`COLUMNS`] columns that the `Columns*` and `Mesh*` shaders draw
/// into, read back one value per column.
pub struct ColumnTarget {
    pub texture: marlin_rhi::Texture,
    pub view: marlin_rhi::RenderTargetView,
    pub readback: Buffer,
    /// The state the last recorded `finish` left the texture in: what a discard comes from (the
    /// render graph's job in the engine). Undefined while the texture is new.
    last: core::cell::Cell<marlin_rhi::TextureState>,
}

impl ColumnTarget {
    /// The target's description.
    pub fn desc() -> marlin_rhi::TextureDesc {
        use marlin_rhi::{Format, TextureDesc, TextureUsage};
        TextureDesc {
            clear_value: Some(marlin_rhi::ClearValue::Color([0.0; 4])),
            ..TextureDesc::new_2d(
                COLUMNS * COLUMN_WIDTH,
                COLUMN_HEIGHT,
                Format::R32Uint,
                TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
            )
        }
    }

    pub fn new(device: &Device) -> Self {
        Self::with_texture(device, device.create_texture(&Self::desc()).unwrap())
    }

    /// A target over `texture`, made from [`desc`](Self::desc) (a placed one, say).
    pub fn with_texture(device: &Device, texture: marlin_rhi::Texture) -> Self {
        use marlin_rhi::RenderTargetViewDesc;
        let view = device
            .create_render_target_view(&texture, &RenderTargetViewDesc::default())
            .unwrap();
        let readback = readback(device, 512 * COLUMN_HEIGHT as u64);
        Self {
            texture,
            view,
            readback,
            last: core::cell::Cell::new(marlin_rhi::TextureState {
                layout: marlin_rhi::Layout::Common,
                ..marlin_rhi::TextureState::UNDEFINED
            }),
        }
    }

    pub fn color_targets() -> [marlin_rhi::ColorTargetState; 1] {
        [marlin_rhi::ColorTargetState::new(
            marlin_rhi::Format::R32Uint,
        )]
    }

    /// Clears to 0 and starts a render pass over the whole target.
    pub fn begin(&self, list: &mut CommandList) {
        use marlin_rhi::{
            ColorAttachment, LoadOp, Rect, RenderPassDesc, StoreOp, TextureBarrier, TextureState,
            Viewport,
        };
        list.texture_barriers(&[TextureBarrier::discard(
            &self.texture,
            self.last.get(),
            TextureState::RENDER_TARGET,
        )]);
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: &self.view,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
            area: None,
        });
        let (w, h) = (COLUMNS * COLUMN_WIDTH, COLUMN_HEIGHT);
        list.set_viewport(&Viewport::full(w, h));
        list.set_scissor(&Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        });
    }

    /// Ends the pass and copies the target into the readback buffer.
    pub fn finish(&self, list: &mut CommandList) {
        use marlin_rhi::{BufferTextureCopy, TextureBarrier, TextureState};
        list.end_render_pass();
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::RENDER_TARGET,
            TextureState::COPY_SRC,
        )]);
        self.last.set(TextureState::COPY_SRC);
        list.copy_texture_to_buffer(
            &self.texture,
            &self.readback,
            &BufferTextureCopy::whole_mip(self.texture.desc(), 0, 0, 0),
        );
    }

    /// The value at the center of each column (after the work in `finish` has run).
    pub fn columns(&self) -> Vec<u32> {
        let row = read_words(&self.readback, 512 * (COLUMN_HEIGHT as u64 / 2), 128);
        (0..COLUMNS)
            .map(|c| row[(c * COLUMN_WIDTH + COLUMN_WIDTH / 2) as usize])
            .collect()
    }
}

/// The `ColumnsVs` and `ColumnsPs` shaders drawing into a [`ColumnTarget`], with optional depth-stencil state.
pub fn columns_pipeline(
    device: &Device,
    depth: Option<marlin_rhi::DepthState>,
) -> marlin_rhi::GraphicsPipeline {
    use marlin_rhi::{CullMode, GraphicsPipelineDesc, PrimitiveTopology, RasterState};
    device
        .create_graphics_pipeline(&GraphicsPipelineDesc {
            vertex: shader("ColumnsVs"),
            pixel: Some(shader("ColumnsPs")),
            topology: PrimitiveTopology::TriangleList,
            raster: RasterState {
                cull: CullMode::None,
                ..RasterState::default()
            },
            depth,
            color_targets: &ColumnTarget::color_targets(),
            multisample: marlin_rhi::MultisampleState::NONE,
            primitive_restart: None,
            cache: None,
        })
        .unwrap()
}

/// The compute entry `name` of the shared test shaders (`Fill`) as a pipeline.
pub fn compute_pipeline(device: &Device, name: &str) -> marlin_rhi::ComputePipeline {
    device
        .create_compute_pipeline(&marlin_rhi::ComputePipelineDesc {
            shader: shader(name),
            cache: None,
        })
        .unwrap()
}

/// A device-local buffer of `words` u32 that shaders write through the returned raw view, and
/// that copies read from.
pub fn storage(device: &Device, words: u64) -> (Buffer, marlin_rhi::ShaderView) {
    use marlin_rhi::{BufferViewDesc, BufferViewKind};
    let b = buffer(
        device,
        words * 4,
        BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        MemoryLocation::GpuOnly,
    );
    let view = device
        .create_buffer_view(&b, &BufferViewDesc::whole(BufferViewKind::Raw, true))
        .unwrap();
    (b, view)
}

/// After compute shaders wrote `buffer`, copies its first `words` into `readback`.
pub fn copy_out(list: &mut CommandList, buffer: &Buffer, readback: &Buffer, words: u64) {
    use marlin_rhi::{BufferBarrier, BufferState};
    list.buffer_barriers(&[BufferBarrier {
        buffer,
        before: BufferState::COMPUTE_WRITE,
        after: BufferState::COPY_SRC,
    }]);
    list.copy_buffer(buffer, 0, readback, 0, words * 4);
}

/// A small `Rgba8Unorm` (or other 4-byte format) render target, single- or multisampled, with a
/// readback buffer for mip 0 of its single-sampled form.
pub struct ColorTarget {
    pub texture: marlin_rhi::Texture,
    pub view: marlin_rhi::RenderTargetView,
    pub readback: Buffer,
    pub size: u32,
}

impl ColorTarget {
    pub fn new(device: &Device, size: u32, format: marlin_rhi::Format, samples: u32) -> Self {
        use marlin_rhi::{RenderTargetViewDesc, TextureDesc, TextureUsage};
        let usage = match samples {
            1 => TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
            _ => TextureUsage::RENDER_TARGET | TextureUsage::SAMPLED,
        };
        let texture = device
            .create_texture(&TextureDesc {
                sample_count: samples,
                ..TextureDesc::new_2d(size, size, format, usage)
            })
            .unwrap();
        let view = device
            .create_render_target_view(&texture, &RenderTargetViewDesc::default())
            .unwrap();
        Self {
            texture,
            view,
            readback: readback(device, 256 * size as u64),
            size,
        }
    }

    /// Moves the target to render target layout (from a new texture's), opens a pass clearing
    /// it to `clear`, and sets the viewport and scissor.
    pub fn begin(&self, list: &mut CommandList, clear: [f32; 4]) {
        use marlin_rhi::{
            ColorAttachment, LoadOp, Rect, RenderPassDesc, StoreOp, TextureBarrier, TextureState,
            Viewport,
        };
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::UNDEFINED,
            TextureState::RENDER_TARGET,
        )]);
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: &self.view,
                load: LoadOp::Clear(clear),
                store: StoreOp::Store,
            }],
            depth: None,
            area: None,
        });
        list.set_viewport(&Viewport::full(self.size, self.size));
        list.set_scissor(&Rect {
            x: 0,
            y: 0,
            width: self.size,
            height: self.size,
        });
    }

    /// Ends the pass and copies the (single-sampled) target into the readback buffer.
    pub fn finish(&self, list: &mut CommandList) {
        use marlin_rhi::{BufferTextureCopy, TextureBarrier, TextureState};
        list.end_render_pass();
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::RENDER_TARGET,
            TextureState::COPY_SRC,
        )]);
        list.copy_texture_to_buffer(
            &self.texture,
            &self.readback,
            &BufferTextureCopy::whole_mip(self.texture.desc(), 0, 0, 0),
        );
    }

    /// Texel (x, y) as little-endian bytes packed in a `u32` (after `finish` ran).
    pub fn texel(&self, x: u32, y: u32) -> u32 {
        read_words(&self.readback, y as u64 * 256 + x as u64 * 4, 1)[0]
    }
}

/// The `SolidVs` vertex shader with pixel entry `pixel` (`SolidPs`, `SolidDual`, `SolidCoord`)
/// into one color target, no culling; `tweak` adjusts the description.
pub fn solid_pipeline(
    device: &Device,
    pixel: &str,
    target: marlin_rhi::ColorTargetState,
    tweak: impl FnOnce(&mut marlin_rhi::GraphicsPipelineDesc<'_>),
) -> marlin_rhi::Result<marlin_rhi::GraphicsPipeline> {
    use marlin_rhi::{CullMode, GraphicsPipelineDesc, PrimitiveTopology, RasterState};
    let targets = [target];
    let mut desc = GraphicsPipelineDesc {
        vertex: shader("SolidVs"),
        pixel: Some(shader(pixel)),
        topology: PrimitiveTopology::TriangleList,
        primitive_restart: None,
        raster: RasterState {
            cull: CullMode::None,
            ..RasterState::default()
        },
        depth: None,
        color_targets: &targets,
        multisample: marlin_rhi::MultisampleState::NONE,
        cache: None,
    };
    tweak(&mut desc);
    device.create_graphics_pipeline(&desc)
}

/// Push constants of the `Solid*` shaders.
pub fn solid_constants(color: [f32; 4], mode: u32, depth: f32, color1: [f32; 4]) -> [u32; 12] {
    let mut words = [0u32; 12];
    for i in 0..4 {
        words[i] = color[i].to_bits();
        words[8 + i] = color1[i].to_bits();
    }
    words[4] = mode;
    words[5] = depth.to_bits();
    words
}

pub use marlin_test_support::{panic_message, skip};

/// `#[gpu_test(api = d3d12)]` in place of `#[test]`: a test only one graphics API (or a few,
/// `api = vulkan | d3d12`) makes sense on, `skip!`ped on the others (see the macro crate).
pub use marlin_gpu_test_macros::gpu_test;

#[doc(hidden)]
pub use marlin_rhi as __rhi;

/// Skips the rest of the test (see [`skip!`]) unless `device.features().$feature` is true.
///
/// ```ignore
/// require_feature!(device, ray_tracing);
/// ```
#[macro_export]
macro_rules! require_feature {
    ($device:expr, $feature:ident) => {
        if !$device.features().$feature {
            $crate::skip!(
                "the device does not support `{}`",
                ::core::stringify!($feature)
            );
        }
    };
}
