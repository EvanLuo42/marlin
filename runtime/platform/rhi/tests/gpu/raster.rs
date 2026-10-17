//! Rasterization and output-merger features: multisampling and resolves, alpha to coverage,
//! blend constants, dual-source blending, conservative rasterization, depth bounds,
//! variable-rate shading, viewport arrays and primitive restart. Hardware adapter, each barrier
//! path, validation on. Optional features are skipped where the device lacks them.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BlendComponent, BlendFactor, BlendOp, BlendState, BufferTextureCopy, BufferUsage,
    ColorAttachment, ColorTargetState, CompareOp, DepthAttachment, DepthState,
    DepthStencilViewDesc, Device, Format, IndexFormat, LoadOp, MultisampleState, PrimitiveTopology,
    RasterState, Rect, RenderPassDesc, SamplerDesc, ShadingRate, ShadingRateCombiner, StoreOp,
    TextureBarrier, TextureDesc, TextureResolve, TextureState, TextureUsage, TextureViewDesc,
    VariableRateShading, Viewport,
};
use support::{
    ColorTarget, compute_pipeline, copy_out, devices, read_words, readback, run, solid_constants,
    solid_pipeline, storage,
};

const WHITE: [f32; 4] = [1.0; 4];

fn red(texel: u32) -> u32 {
    texel & 0xff
}

#[test]
fn multisampled_edges_resolve_and_read_per_sample() {
    for device in devices() {
        let ms = ColorTarget::new(device, 8, Format::Rgba8Unorm, 4);
        let resolved = device
            .create_texture(&TextureDesc::new_2d(
                8,
                8,
                Format::Rgba8Unorm,
                TextureUsage::COPY_SRC | TextureUsage::SAMPLED,
            ))
            .unwrap();
        let pipeline = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |d| {
                d.multisample = MultisampleState::samples(4);
            },
        )
        .unwrap();
        let samples_view = device
            .create_texture_view(&ms.texture, &TextureViewDesc::SAMPLED)
            .unwrap();
        let reader = compute_pipeline(device, "sample.ms");
        let (samples, samples_out) = storage(device, 4);
        let samples_rb = readback(device, 16);
        run(device, |list| {
            ms.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&pipeline);
            list.push_constants(0, &solid_constants(WHITE, 1, 0.5, [0.0; 4]));
            list.draw(3, 1, 0, 0);
            list.end_render_pass();
            list.texture_barriers(&[
                TextureBarrier::new(
                    &ms.texture,
                    TextureState::RENDER_TARGET,
                    TextureState::RESOLVE_SRC,
                ),
                TextureBarrier::new(
                    &resolved,
                    TextureState::UNDEFINED,
                    TextureState::RESOLVE_DST,
                ),
            ]);
            list.resolve_texture(&ms.texture, &resolved, &TextureResolve::default());
            list.texture_barriers(&[
                TextureBarrier::new(&resolved, TextureState::RESOLVE_DST, TextureState::COPY_SRC),
                TextureBarrier::new(
                    &ms.texture,
                    TextureState::RESOLVE_SRC,
                    TextureState::SHADER_READ,
                ),
            ]);
            list.copy_texture_to_buffer(
                &resolved,
                &ms.readback,
                &BufferTextureCopy::whole_mip(resolved.desc(), 0, 0, 0),
            );
            // The samples of pixel (3, 3), which the edge cuts in half.
            list.set_compute_pipeline(&reader);
            list.push_constants(0, &[samples_view.index(), 0, samples_out.index(), 3]);
            list.dispatch(1, 1, 1);
            copy_out(list, &samples, &samples_rb, 4);
        });
        // Below the diagonal is covered, above it is not, and on it the samples average.
        assert_eq!(ms.texel(0, 7), 0xffff_ffff);
        assert_eq!(ms.texel(7, 0), 0);
        let edge = red(ms.texel(3, 3));
        assert!((100..=160).contains(&edge), "edge pixel resolved to {edge}");
        let per_sample = read_words(&samples_rb, 0, 4);
        let covered = per_sample.iter().filter(|&&s| s == 255).count();
        assert!(
            per_sample.iter().all(|&s| s == 0 || s == 255) && (1..=3).contains(&covered),
            "{per_sample:?}"
        );
        assert_eq!(device.validation_errors(), 0);
    }
}

/// Resolves a multisampled target into a fresh single-sampled one and returns its texels.
fn resolve_all(
    device: &Device,
    ms: &ColorTarget,
    list: &mut marlin_rhi::CommandList,
) -> marlin_rhi::Texture {
    let resolved = device
        .create_texture(&TextureDesc::new_2d(
            ms.size,
            ms.size,
            Format::Rgba8Unorm,
            TextureUsage::COPY_SRC | TextureUsage::SAMPLED,
        ))
        .unwrap();
    list.end_render_pass();
    list.texture_barriers(&[
        TextureBarrier::new(
            &ms.texture,
            TextureState::RENDER_TARGET,
            TextureState::RESOLVE_SRC,
        ),
        TextureBarrier::new(
            &resolved,
            TextureState::UNDEFINED,
            TextureState::RESOLVE_DST,
        ),
    ]);
    list.resolve_texture(&ms.texture, &resolved, &TextureResolve::default());
    list.texture_barriers(&[TextureBarrier::new(
        &resolved,
        TextureState::RESOLVE_DST,
        TextureState::COPY_SRC,
    )]);
    list.copy_texture_to_buffer(
        &resolved,
        &ms.readback,
        &BufferTextureCopy::whole_mip(resolved.desc(), 0, 0, 0),
    );
    resolved
}

#[test]
fn alpha_to_coverage_covers_part_of_each_pixel() {
    let device = support::hardware_device();
    let ms = ColorTarget::new(device, 8, Format::Rgba8Unorm, 4);
    let pipeline = solid_pipeline(
        device,
        "solid",
        ColorTargetState::new(Format::Rgba8Unorm),
        |d| {
            d.multisample = MultisampleState {
                alpha_to_coverage: true,
                ..MultisampleState::samples(4)
            };
        },
    )
    .unwrap();
    let mut keep = None;
    run(device, |list| {
        ms.begin(list, [0.0; 4]);
        list.set_graphics_pipeline(&pipeline);
        list.push_constants(0, &solid_constants([1.0, 1.0, 1.0, 0.5], 0, 0.5, [0.0; 4]));
        list.draw(3, 1, 0, 0);
        keep = Some(resolve_all(device, &ms, list));
    });
    let total: u32 = (0..8)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .map(|(x, y)| red(ms.texel(x, y)))
        .sum();
    let average = total / 64;
    assert!((64..=192).contains(&average), "average coverage {average}");
    assert_eq!(device.validation_errors(), 0);
}

fn blend(src: BlendFactor) -> BlendState {
    BlendState {
        color: BlendComponent {
            src_factor: src,
            dst_factor: BlendFactor::Zero,
            op: BlendOp::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::Zero,
            op: BlendOp::Add,
        },
    }
}

fn near(texel: u32, expect: [u8; 4]) -> bool {
    texel
        .to_le_bytes()
        .iter()
        .zip(expect)
        .all(|(&a, b)| a.abs_diff(b) <= 1)
}

#[test]
fn blend_constants_and_dual_source_blending() {
    for device in devices() {
        let constant = solid_pipeline(
            device,
            "solid",
            ColorTargetState {
                blend: Some(blend(BlendFactor::Constant)),
                ..ColorTargetState::new(Format::Rgba8Unorm)
            },
            |_| {},
        )
        .unwrap();
        let dual = solid_pipeline(
            device,
            "solid.dual",
            ColorTargetState {
                blend: Some(blend(BlendFactor::Src1Color)),
                ..ColorTargetState::new(Format::Rgba8Unorm)
            },
            |_| {},
        )
        .unwrap();
        let a = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        let b = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        run(device, |list| {
            a.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&constant);
            list.set_blend_constants([0.5, 0.25, 1.0, 1.0]);
            list.push_constants(0, &solid_constants(WHITE, 0, 0.5, [0.0; 4]));
            list.draw(3, 1, 0, 0);
            a.finish(list);
            b.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&dual);
            list.push_constants(0, &solid_constants(WHITE, 0, 0.5, [0.5, 0.0, 1.0, 1.0]));
            list.draw(3, 1, 0, 0);
            b.finish(list);
        });
        assert!(
            near(a.texel(1, 1), [128, 64, 255, 255]),
            "{:08x}",
            a.texel(1, 1)
        );
        assert!(
            near(b.texel(1, 1), [128, 0, 255, 255]),
            "{:08x}",
            b.texel(1, 1)
        );
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn dual_source_blending_takes_one_target() {
    let device = support::hardware_device();
    let target = ColorTargetState {
        blend: Some(blend(BlendFactor::Src1Alpha)),
        ..ColorTargetState::new(Format::Rgba8Unorm)
    };
    let targets = [target, target];
    let error = device
        .create_graphics_pipeline(&marlin_rhi::GraphicsPipelineDesc {
            vertex: support::shader("solid", support::Stage::Vertex),
            pixel: Some(support::shader("solid.dual", support::Stage::Pixel)),
            topology: PrimitiveTopology::TriangleList,
            primitive_restart: None,
            raster: RasterState::default(),
            depth: None,
            color_targets: &targets,
            multisample: MultisampleState::NONE,
            cache: None,
        })
        .unwrap_err();
    assert_eq!(error.kind(), marlin_rhi::ErrorKind::InvalidInput);
}

#[test]
fn conservative_rasterization_shades_touched_pixels() {
    let device = support::hardware_device();
    support::require_feature!(device, conservative_rasterization);
    let draw = |conservative: bool| {
        let pipeline = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |d| {
                d.raster = RasterState {
                    conservative,
                    ..d.raster
                };
            },
        )
        .unwrap();
        let target = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        run(device, |list| {
            target.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&pipeline);
            list.push_constants(0, &solid_constants(WHITE, 2, 0.5, [0.0; 4]));
            list.draw(3, 1, 0, 0);
            target.finish(list);
        });
        target.texel(0, 0)
    };
    assert_eq!(draw(false), 0, "the sliver misses the pixel center");
    assert_eq!(draw(true), 0xffff_ffff, "but touches the pixel");
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn depth_bounds_discard_pixels_outside_the_range() {
    for device in devices() {
        support::require_feature!(device, depth_bounds);
        let pipeline = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |d| {
                d.depth = Some(DepthState {
                    write: false,
                    compare: CompareOp::Always,
                    bounds_test: true,
                    ..DepthState::reverse_z(Format::D32Float)
                });
            },
        )
        .unwrap();
        let depth = device
            .create_texture(&TextureDesc::new_2d(
                4,
                4,
                Format::D32Float,
                TextureUsage::DEPTH_STENCIL,
            ))
            .unwrap();
        let depth_view = device
            .create_depth_stencil_view(&depth, &DepthStencilViewDesc::default())
            .unwrap();
        let draw = |bounds: (f32, f32), first: bool| {
            let target = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
            run(device, |list| {
                list.texture_barriers(&[
                    TextureBarrier::new(
                        &target.texture,
                        TextureState::UNDEFINED,
                        TextureState::RENDER_TARGET,
                    ),
                    match first {
                        true => TextureBarrier::new(
                            &depth,
                            TextureState::UNDEFINED,
                            TextureState::DEPTH_WRITE,
                        ),
                        false => TextureBarrier::new(
                            &depth,
                            TextureState::DEPTH_WRITE,
                            TextureState::DEPTH_WRITE,
                        ),
                    },
                ]);
                list.begin_render_pass(&RenderPassDesc {
                    color: &[ColorAttachment {
                        view: &target.view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: Some(DepthAttachment {
                        view: &depth_view,
                        load: LoadOp::Clear(0.25),
                        store: StoreOp::Store,
                        stencil_load: LoadOp::DontCare,
                        stencil_store: StoreOp::DontCare,
                    }),
                });
                list.set_viewport(&Viewport::full(4, 4));
                list.set_scissor(&Rect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 4,
                });
                list.set_graphics_pipeline(&pipeline);
                list.set_depth_bounds(bounds.0, bounds.1);
                list.push_constants(0, &solid_constants(WHITE, 0, 0.5, [0.0; 4]));
                list.draw(3, 1, 0, 0);
                target.finish(list);
            });
            target.texel(2, 2)
        };
        // The stored depth (0.25) decides, not the drawn one (0.5).
        assert_eq!(draw((0.5, 1.0), true), 0);
        assert_eq!(draw((0.0, 0.5), false), 0xffff_ffff);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn variable_rate_shading_per_draw_and_by_image() {
    let device = support::hardware_device();
    let features = device.features();
    if features.variable_rate_shading == VariableRateShading::None {
        return;
    }
    let pipeline = solid_pipeline(
        device,
        "solid.coord",
        ColorTargetState::new(Format::R32Uint),
        |_| {},
    )
    .unwrap();
    let draw = |rate: ShadingRate, image: Option<&marlin_rhi::Texture>, size: u32| {
        let target = ColorTarget::new(device, size, Format::R32Uint, 1);
        run(device, |list| {
            target.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&pipeline);
            match image {
                None => list.set_shading_rate(rate, [ShadingRateCombiner::Keep; 2]),
                Some(image) => {
                    list.set_shading_rate(
                        rate,
                        [ShadingRateCombiner::Keep, ShadingRateCombiner::Replace],
                    );
                    list.set_shading_rate_image(Some(image));
                }
            }
            list.push_constants(0, &solid_constants(WHITE, 0, 0.5, [0.0; 4]));
            list.draw(3, 1, 0, 0);
            target.finish(list);
        });
        [
            target.texel(0, 0),
            target.texel(1, 0),
            target.texel(0, 1),
            target.texel(2, 0),
        ]
    };
    let fine = draw(ShadingRate::R1x1, None, 8);
    assert_ne!(fine[0], fine[1]);
    let coarse = draw(ShadingRate::R2x2, None, 8);
    assert!(
        coarse[0] == coarse[1] && coarse[0] == coarse[2],
        "{coarse:x?}"
    );
    assert_ne!(coarse[0], coarse[3]);

    if features.variable_rate_shading != VariableRateShading::Image {
        return;
    }
    let tile = features.shading_rate_tile_size;
    let image_desc = TextureDesc::new_2d(
        2,
        2,
        Format::R8Uint,
        TextureUsage::SHADING_RATE | TextureUsage::COPY_DST,
    );
    let image = device.create_texture(&image_desc).unwrap();
    let rates = [ShadingRate::R2x2.encoded(); 512];
    let staging = support::upload(device, &rates, BufferUsage::COPY_SRC);
    run(device, |list| {
        list.texture_barriers(&[TextureBarrier::new(
            &image,
            TextureState::UNDEFINED,
            TextureState::COPY_DST,
        )]);
        list.copy_buffer_to_texture(
            &staging,
            &image,
            &BufferTextureCopy::whole_mip(&image_desc, 0, 0, 0),
        );
        list.texture_barriers(&[TextureBarrier::new(
            &image,
            TextureState::COPY_DST,
            TextureState::SHADING_RATE,
        )]);
    });
    let by_image = draw(ShadingRate::R1x1, Some(&image), 2 * tile);
    assert!(
        by_image[0] == by_image[1] && by_image[0] == by_image[2],
        "{by_image:x?}"
    );
    assert_eq!(device.validation_errors(), 0);
}

#[test]
fn viewport_arrays_and_primitive_restart() {
    for device in devices() {
        let strip = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |d| {
                d.topology = PrimitiveTopology::TriangleStrip;
                d.primitive_restart = Some(IndexFormat::U16);
            },
        )
        .unwrap();
        let indices: [u16; 8] = [0, 1, 2, 0xffff, 0, 1, 2, 0];
        let index_buffer =
            support::upload(device, &support::bytes_of(&indices), BufferUsage::INDEX);
        let target = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        run(device, |list| {
            target.begin(list, [0.0; 4]);
            let full = Viewport::full(4, 4);
            list.set_viewports(&[full, Viewport { width: 2.0, ..full }]);
            let rect = Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            };
            list.set_scissors(&[rect, rect]);
            list.set_graphics_pipeline(&strip);
            list.set_index_buffer(&index_buffer, 0, IndexFormat::U16);
            list.push_constants(0, &solid_constants(WHITE, 0, 0.5, [0.0; 4]));
            list.draw_indexed(7, 1, 0, 0, 0);
            target.finish(list);
        });
        assert_eq!(target.texel(3, 3), 0xffff_ffff);
        // Restart needs a strip.
        let error = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |d| {
                d.primitive_restart = Some(IndexFormat::U16);
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), marlin_rhi::ErrorKind::InvalidInput);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn multisampled_textures_have_rules() {
    let device = support::hardware_device();
    let invalid = |desc: TextureDesc| {
        assert_eq!(
            device.create_texture(&desc).unwrap_err().kind(),
            marlin_rhi::ErrorKind::InvalidInput,
            "{desc:?}"
        );
    };
    let base = TextureDesc {
        sample_count: 4,
        ..TextureDesc::new_2d(8, 8, Format::Rgba8Unorm, TextureUsage::RENDER_TARGET)
    };
    invalid(TextureDesc {
        sample_count: 3,
        ..base
    });
    invalid(TextureDesc {
        mip_levels: 2,
        ..base
    });
    invalid(TextureDesc {
        usage: TextureUsage::RENDER_TARGET | TextureUsage::STORAGE,
        ..base
    });
    invalid(TextureDesc {
        usage: TextureUsage::SAMPLED,
        ..base
    });
    // Depth multisamples too.
    device
        .create_texture(&TextureDesc {
            sample_count: 4,
            ..TextureDesc::new_2d(8, 8, Format::D32Float, TextureUsage::DEPTH_STENCIL)
        })
        .unwrap();
    let _ = SamplerDesc::default();
    assert_eq!(device.validation_errors(), 0);
}
