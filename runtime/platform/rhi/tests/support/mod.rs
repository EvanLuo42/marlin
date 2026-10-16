//! Shared test helpers: hardware devices, buffers, and the test shaders.
//!
//! Shaders are looked up by name and stage. This is the shape the planned test-shader crate will
//! have: its build.rs compiles every Marlin shader under `tests/shaders/` with the Marlin shader
//! compiler library and generates this lookup. Until that compiler exists, the blobs are
//! PLACEHOLDERS: the `.hlsl` files beside them, compiled once by hand with the Windows SDK's dxc:
//!
//! ```text
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E main         -Fo fill.cs.dxil fill.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T vs_6_6 -E vs_main      -Fo quad.vs.dxil quad.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main      -Fo quad.ps.dxil quad.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T vs_6_6 -E vs_main      -Fo columns.vs.dxil columns.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main      -Fo columns.ps.dxil columns.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ms_6_6 -E ms_main      -Fo mesh.ms.dxil mesh.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T as_6_6 -E as_main      -Fo mesh.as.dxil mesh.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ms_6_6 -E ms_amplified -Fo mesh.amplified.ms.dxil mesh.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main      -Fo mesh.ps.dxil mesh.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E main         -Fo count.cs.dxil count.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E write_main   -Fo volume.write.cs.dxil volume.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T cs_6_6 -E read_main    -Fo volume.read.cs.dxil volume.hlsl
//! ```
#![allow(dead_code, reason = "each test file uses a subset")]

use marlin_rhi::{
    AdapterPreference, Buffer, BufferDesc, BufferUsage, CommandList, Device, DeviceDesc, Instance,
    InstanceDesc, MemoryLocation, QueueType, ShaderModule,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Vertex,
    Pixel,
    Compute,
    Amplification,
    Mesh,
}

/// The compiled shader `name` for `stage`. Panics if there is none.
pub fn shader(name: &str, stage: Stage) -> ShaderModule<'static> {
    let (code, entry): (&'static [u8], _) = match (name, stage) {
        ("fill", Stage::Compute) => (include_bytes!("../shaders/fill.cs.dxil"), "main"),
        ("quad", Stage::Vertex) => (include_bytes!("../shaders/quad.vs.dxil"), "vs_main"),
        ("quad", Stage::Pixel) => (include_bytes!("../shaders/quad.ps.dxil"), "ps_main"),
        ("columns", Stage::Vertex) => (include_bytes!("../shaders/columns.vs.dxil"), "vs_main"),
        ("columns", Stage::Pixel) => (include_bytes!("../shaders/columns.ps.dxil"), "ps_main"),
        ("mesh", Stage::Mesh) => (include_bytes!("../shaders/mesh.ms.dxil"), "ms_main"),
        ("mesh", Stage::Amplification) => (include_bytes!("../shaders/mesh.as.dxil"), "as_main"),
        ("mesh.amplified", Stage::Mesh) => (
            include_bytes!("../shaders/mesh.amplified.ms.dxil"),
            "ms_amplified",
        ),
        ("mesh", Stage::Pixel) => (include_bytes!("../shaders/mesh.ps.dxil"), "ps_main"),
        ("count", Stage::Compute) => (include_bytes!("../shaders/count.cs.dxil"), "main"),
        ("volume.write", Stage::Compute) => (
            include_bytes!("../shaders/volume.write.cs.dxil"),
            "write_main",
        ),
        ("volume.read", Stage::Compute) => (
            include_bytes!("../shaders/volume.read.cs.dxil"),
            "read_main",
        ),
        _ => panic!("no test shader {name} for {stage:?}"),
    };
    ShaderModule { code, entry }
}

/// A device on the highest-performance hardware adapter, with validation on. GPU tests run on
/// real hardware, never on a software rasterizer.
pub fn hardware_device() -> Device {
    let instance = Instance::new(&InstanceDesc { validation: true }).unwrap();
    instance
        .create_device(&DeviceDesc {
            adapter: AdapterPreference::HighPerformance,
            ..DeviceDesc::default()
        })
        .unwrap()
}

/// The hardware adapter with validation, through enhanced barriers (where supported), then
/// legacy ones.
pub fn devices() -> [Device; 2] {
    let instance = Instance::new(&InstanceDesc { validation: true }).unwrap();
    [false, true].map(|legacy| {
        instance
            .create_device(&DeviceDesc {
                adapter: AdapterPreference::HighPerformance,
                disable_enhanced_barriers: legacy,
            })
            .unwrap()
    })
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

/// An R32Uint render target of [`COLUMNS`] columns that the `columns` and `mesh` shaders draw
/// into, read back one value per column.
pub struct ColumnTarget {
    pub texture: marlin_rhi::Texture,
    pub view: marlin_rhi::RenderTargetView,
    pub readback: Buffer,
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
        list.texture_barriers(&[TextureBarrier::new(
            &self.texture,
            TextureState::UNDEFINED,
            TextureState::RENDER_TARGET,
        )]);
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: &self.view,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
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

/// The `columns` shaders drawing into a [`ColumnTarget`], with optional depth-stencil state.
pub fn columns_pipeline(
    device: &Device,
    depth: Option<marlin_rhi::DepthState>,
) -> marlin_rhi::GraphicsPipeline {
    use marlin_rhi::{CullMode, GraphicsPipelineDesc, PrimitiveTopology, RasterState};
    device
        .create_graphics_pipeline(&GraphicsPipelineDesc {
            vertex: shader("columns", Stage::Vertex),
            pixel: Some(shader("columns", Stage::Pixel)),
            topology: PrimitiveTopology::TriangleList,
            raster: RasterState {
                cull: CullMode::None,
                ..RasterState::default()
            },
            depth,
            color_targets: &ColumnTarget::color_targets(),
        })
        .unwrap()
}
