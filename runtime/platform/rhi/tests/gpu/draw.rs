//! End-to-end GPU work on the hardware adapter, with validation on, through each barrier path: a
//! compute dispatch writing through the bindless table, and indexed draws with vertex pulling, a
//! uniform buffer, a sampled texture and reverse-Z depth testing, read back to the CPU.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferTextureCopy, BufferUsage, BufferViewDesc, BufferViewKind,
    ColorAttachment, ColorTargetState, ComputePipelineDesc, DepthAttachment, DepthState,
    DepthStencilViewDesc, Filter, Format, GraphicsPipelineDesc, IndexFormat, LoadOp,
    MemoryLocation, PrimitiveTopology, RasterState, Rect, RenderPassDesc, RenderTargetViewDesc,
    SamplerDesc, StoreOp, TextureBarrier, TextureDesc, TextureState, TextureUsage, TextureViewDesc,
    Viewport,
};
use support::{buffer, devices, read_words, run, shader};

#[test]
fn compute_writes_through_bindless_view() {
    for device in devices() {
        const COUNT: u32 = 256;
        let output = buffer(
            device,
            COUNT as u64 * 4,
            BufferUsage::SHADER_WRITE | BufferUsage::COPY_SRC,
            MemoryLocation::GpuOnly,
        );
        let readback = buffer(
            device,
            COUNT as u64 * 4,
            BufferUsage::COPY_DST,
            MemoryLocation::Readback,
        );
        let view = device
            .create_buffer_view(&output, &BufferViewDesc::whole(BufferViewKind::Raw, true))
            .unwrap();
        let pipeline = device
            .create_compute_pipeline(&ComputePipelineDesc {
                shader: shader("Fill"),
                cache: None,
            })
            .unwrap();

        run(device, |list| {
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[view.index(), 1000]);
            list.dispatch(COUNT / 64, 1, 1);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &output,
                before: BufferState::COMPUTE_WRITE,
                after: BufferState::COPY_SRC,
            }]);
            list.copy_buffer(&output, 0, &readback, 0, COUNT as u64 * 4);
        });

        let words = read_words(&readback, 0, COUNT as usize);
        for (i, &w) in words.iter().enumerate() {
            assert_eq!(w, 1000 + 3 * i as u32, "word {i}");
        }
        assert_eq!(device.validation_errors(), 0);
    }
}

const SIZE: u32 = 64;
const RED: u32 = 0xff00_00ff;
const BLUE: u32 = 0xffff_0000;
const BLACK: u32 = 0xff00_0000;

#[test]
fn draws_with_vertex_pulling_textures_and_depth() {
    for device in devices() {
        // Staging: vertices at 0, indices at 64, two tints at 256 and 512, texels at 1024.
        let staging = buffer(
            device,
            2048,
            BufferUsage::COPY_SRC | BufferUsage::UNIFORM,
            MemoryLocation::Upload,
        );
        let vertices: [[f32; 2]; 4] = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
        let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let tints: [[f32; 4]; 3] = [
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
        ];
        // SAFETY: no GPU work exists yet.
        unsafe {
            staging.write(0, &bytes(vertices.as_flattened()));
            staging.write(64, &indices.map(u16::to_le_bytes).concat());
            for (i, tint) in tints.iter().enumerate() {
                staging.write(256 * (i as u64 + 1), &bytes(tint));
            }
            staging.write(1024, &[0xff; 8]);
            staging.write(1024 + 256, &[0xff; 8]);
        }

        let vertex_buffer = buffer(
            device,
            32,
            BufferUsage::SHADER_READ | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let index_buffer = buffer(
            device,
            12,
            BufferUsage::INDEX | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let vertex_view = device
            .create_buffer_view(
                &vertex_buffer,
                &BufferViewDesc::whole(BufferViewKind::Structured { stride: 8 }, false),
            )
            .unwrap();
        let tint_views = [1u64, 2, 3].map(|i| {
            device
                .create_buffer_view(
                    &staging,
                    &BufferViewDesc {
                        kind: BufferViewKind::Uniform,
                        writable: false,
                        offset: 256 * i,
                        size: 16,
                    },
                )
                .unwrap()
        });

        let white = device
            .create_texture(&TextureDesc::new_2d(
                2,
                2,
                Format::Rgba8Unorm,
                TextureUsage::SAMPLED | TextureUsage::COPY_DST,
            ))
            .unwrap();
        let white_view = device
            .create_texture_view(&white, &TextureViewDesc::SAMPLED)
            .unwrap();
        let sampler = device
            .create_sampler(&SamplerDesc {
                min_filter: Filter::Nearest,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Nearest,
                ..SamplerDesc::default()
            })
            .unwrap();

        let target = device
            .create_texture(&TextureDesc {
                clear_value: Some(marlin_rhi::ClearValue::Color([0.0, 0.0, 0.0, 1.0])),
                ..TextureDesc::new_2d(
                    SIZE,
                    SIZE,
                    Format::Rgba8Unorm,
                    TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC,
                )
            })
            .unwrap();
        let target_view = device
            .create_render_target_view(&target, &RenderTargetViewDesc::default())
            .unwrap();
        let depth = device
            .create_texture(&TextureDesc {
                clear_value: Some(marlin_rhi::ClearValue::Depth(0.0)),
                ..TextureDesc::new_2d(SIZE, SIZE, Format::D32Float, TextureUsage::DEPTH_STENCIL)
            })
            .unwrap();
        let depth_view = device
            .create_depth_stencil_view(&depth, &DepthStencilViewDesc::default())
            .unwrap();
        let readback = buffer(
            device,
            (SIZE * 256) as u64,
            BufferUsage::COPY_DST,
            MemoryLocation::Readback,
        );

        let pipeline = device
            .create_graphics_pipeline(&GraphicsPipelineDesc {
                vertex: shader("QuadVs"),
                pixel: Some(shader("QuadPs")),
                topology: PrimitiveTopology::TriangleList,
                raster: RasterState::default(),
                depth: Some(DepthState::reverse_z(Format::D32Float)),
                color_targets: &[ColorTargetState::new(Format::Rgba8Unorm)],
                multisample: marlin_rhi::MultisampleState::NONE,
                primitive_restart: None,
                cache: None,
            })
            .unwrap();

        run(device, |list| {
            list.copy_buffer(&staging, 0, &vertex_buffer, 0, 32);
            list.copy_buffer(&staging, 64, &index_buffer, 0, 12);
            list.texture_barriers(&[
                TextureBarrier::new(&white, TextureState::UNDEFINED, TextureState::COPY_DST),
                TextureBarrier::new(
                    &target,
                    TextureState::UNDEFINED,
                    TextureState::RENDER_TARGET,
                ),
                TextureBarrier::new(&depth, TextureState::UNDEFINED, TextureState::DEPTH_WRITE),
            ]);
            list.copy_buffer_to_texture(
                &staging,
                &white,
                &BufferTextureCopy::whole_mip(white.desc(), 0, 0, 1024),
            );
            list.texture_barriers(&[TextureBarrier::new(
                &white,
                TextureState::COPY_DST,
                TextureState::PIXEL_SHADER_READ,
            )]);
            list.buffer_barriers(&[
                BufferBarrier {
                    buffer: &vertex_buffer,
                    before: BufferState::COPY_DST,
                    after: BufferState::SHADER_READ,
                },
                BufferBarrier {
                    buffer: &index_buffer,
                    before: BufferState::COPY_DST,
                    after: BufferState::INDEX,
                },
            ]);

            list.begin_render_pass(&RenderPassDesc {
                color: &[ColorAttachment {
                    view: &target_view,
                    load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                    store: StoreOp::Store,
                }],
                depth: Some(DepthAttachment {
                    view: &depth_view,
                    load: LoadOp::Clear(0.0),
                    store: StoreOp::DontCare,
                    stencil_load: LoadOp::DontCare,
                    stencil_store: StoreOp::DontCare,
                }),
            });
            list.set_graphics_pipeline(&pipeline);
            list.set_viewport(&Viewport::full(SIZE, SIZE));
            list.set_scissor(&Rect {
                x: 0,
                y: 0,
                width: SIZE,
                height: SIZE,
            });
            list.set_index_buffer(&index_buffer, 0, IndexFormat::U16);
            let mut draw = |tint: usize, depth: f32, index_count| {
                list.push_constants(
                    0,
                    &[
                        vertex_view.index(),
                        tint_views[tint].index(),
                        white_view.index(),
                        sampler.index(),
                        depth.to_bits(),
                    ],
                );
                list.draw_indexed(index_count, 1, 0, 0, 0);
            };
            draw(0, 0.75, 6); // red quad
            draw(1, 0.25, 6); // green, farther (reverse-Z): hidden
            draw(2, 0.9, 3); // blue, nearer, lower-right triangle only
            list.end_render_pass();

            list.texture_barriers(&[TextureBarrier::new(
                &target,
                TextureState::RENDER_TARGET,
                TextureState::COPY_SRC,
            )]);
            list.copy_texture_to_buffer(
                &target,
                &readback,
                &BufferTextureCopy::whole_mip(target.desc(), 0, 0, 0),
            );
        });

        let pixels = read_words(&readback, 0, (SIZE * 64) as usize);
        let pixel = |x: u32, y: u32| pixels[(y * 64 + x) as usize];
        assert_eq!(pixel(2, 2), BLACK, "outside the quad");
        assert_eq!(
            pixel(24, 24),
            RED,
            "upper-left half: red beats the farther green"
        );
        assert_eq!(
            pixel(40, 40),
            BLUE,
            "lower-right half: the nearer blue wins"
        );
        assert_eq!(device.validation_errors(), 0);
    }
}

fn bytes(floats: &[f32]) -> Vec<u8> {
    floats.iter().flat_map(|f| f.to_le_bytes()).collect()
}
