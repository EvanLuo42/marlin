//! Mesh shading on the hardware adapter, through each barrier path: mesh pipelines with and
//! without an amplification stage, direct and indirect (counted) mesh dispatches.

use marlin_gpu_test_support as support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferUsage, CullMode, Device, DispatchIndirectArgs,
    INDIRECT_CONSTANT_WORD, IndirectArgs, MemoryLocation, MeshPipeline, MeshPipelineDesc,
    RasterState,
};
use support::{COLUMNS, ColumnTarget, Stage, buffer, bytes_of, devices, run, shader, upload};

fn mesh_pipeline(device: &Device, amplified: bool) -> MeshPipeline {
    device
        .create_mesh_pipeline(&MeshPipelineDesc {
            amplification: amplified.then(|| shader("mesh", Stage::Amplification)),
            mesh: match amplified {
                true => shader("mesh.amplified", Stage::Mesh),
                false => shader("mesh", Stage::Mesh),
            },
            pixel: Some(shader("mesh", Stage::Pixel)),
            raster: RasterState {
                cull: CullMode::None,
                ..RasterState::default()
            },
            depth: None,
            color_targets: &ColumnTarget::color_targets(),
            multisample: marlin_rhi::MultisampleState::NONE,
            cache: None,
        })
        .unwrap()
}

/// Column `c` holds `c + 1` where a mesh group covered it, else 0.
fn expect_columns(target: &ColumnTarget, drawn: &[u32]) {
    let columns = target.columns();
    for c in 0..COLUMNS {
        let expected = if drawn.contains(&c) { c + 1 } else { 0 };
        assert_eq!(columns[c as usize], expected, "column {c}: {columns:?}");
    }
}

#[test]
fn mesh_shaders_are_baseline() {
    let device = support::hardware_device();
    assert!(device.features().mesh_shader);
}

#[test]
fn dispatches_mesh_groups() {
    for device in devices() {
        let pipeline = mesh_pipeline(device, false);
        let target = ColumnTarget::new(device);
        run(device, |list| {
            target.begin(list);
            list.set_mesh_pipeline(&pipeline);
            list.push_constants(0, &[COLUMNS]);
            list.push_constants(INDIRECT_CONSTANT_WORD, &[4]);
            list.dispatch_mesh(3, 1, 1);
            target.finish(list);
        });
        expect_columns(&target, &[4, 5, 6]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn amplification_launches_mesh_groups() {
    for device in devices() {
        let pipeline = mesh_pipeline(device, true);
        let target = ColumnTarget::new(device);
        run(device, |list| {
            target.begin(list);
            list.set_mesh_pipeline(&pipeline);
            list.push_constants(0, &[COLUMNS]);
            list.push_constants(INDIRECT_CONSTANT_WORD, &[9]);
            // Two amplification groups, two mesh groups each: columns 9 through 12.
            list.dispatch_mesh(2, 1, 1);
            target.finish(list);
        });
        expect_columns(&target, &[9, 10, 11, 12]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn dispatches_mesh_indirect_with_a_count() {
    for device in devices() {
        let pipeline = mesh_pipeline(device, false);
        let target = ColumnTarget::new(device);
        let records = [
            DispatchIndirectArgs {
                constant: 10,
                x: 2,
                y: 1,
                z: 1,
            },
            DispatchIndirectArgs {
                constant: 0,
                x: 1,
                y: 1,
                z: 1,
            },
            DispatchIndirectArgs {
                constant: 13,
                x: 3,
                y: 1,
                z: 1,
            },
        ];
        let staging = upload(device, &bytes_of(&records), BufferUsage::default());
        let count_staging = upload(device, &2u32.to_le_bytes(), BufferUsage::default());
        let args = buffer(
            device,
            staging.desc().size,
            BufferUsage::INDIRECT | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let count = buffer(
            device,
            4,
            BufferUsage::INDIRECT | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        run(device, |list| {
            list.copy_buffer(&staging, 0, &args, 0, staging.desc().size);
            list.copy_buffer(&count_staging, 0, &count, 0, 4);
            list.buffer_barriers(&[&args, &count].map(|buffer| BufferBarrier {
                buffer,
                before: BufferState::COPY_DST,
                after: BufferState::INDIRECT,
            }));
            target.begin(list);
            list.set_mesh_pipeline(&pipeline);
            list.push_constants(0, &[COLUMNS]);
            list.dispatch_mesh_indirect(&IndirectArgs {
                buffer: &args,
                offset: 0,
                max_count: 3,
                count: Some((&count, 0)),
            });
            target.finish(list);
        });
        expect_columns(&target, &[10, 11, 0]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
#[should_panic(expected = "a mesh dispatch needs a graphics list")]
fn mesh_dispatches_need_a_graphics_list() {
    let device = support::hardware_device();
    let mut pool = device
        .create_command_pool_for(marlin_rhi::QueueType::Compute)
        .unwrap();
    let mut list = device.create_command_list(&pool).unwrap();
    list.begin(&mut pool).unwrap();
    list.dispatch_mesh(1, 1, 1);
}
