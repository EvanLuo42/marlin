//! Capability queries: the baseline a device must meet, its optional [`Features`], and what it
//! does with each [`Format`].

use super::com::{self, *};
use super::ffi::*;
use super::resource::dxgi_format;
use super::{Device, error, unsupported};
use crate::{
    DeviceDesc, Features, Format, FormatFeatures, FormatSupport, Result, VariableRateShading,
};

/// Queries a feature-support struct; `false` if the runtime does not know the feature.
///
/// # Safety
/// `T` is the struct `feature` reports.
pub(super) unsafe fn feature<T>(device: &Com<ID3D12Device>, feature: i32, data: &mut T) -> bool {
    // SAFETY: valid interface; `data` matches the feature (caller).
    let hr = unsafe {
        (device.vtbl().CheckFeatureSupport)(
            device.as_raw(),
            feature,
            (data as *mut T).cast(),
            size_of::<T>() as u32,
        )
    };
    hr >= 0
}

/// The highest shader model the device supports, as `0xMm`.
fn shader_model(device: &Com<ID3D12Device>) -> i32 {
    // Runtimes reject models newer than they know: step down until one is accepted.
    (D3D_SHADER_MODEL_6_6..=D3D_SHADER_MODEL_HIGHEST)
        .rev()
        .find_map(|requested| {
            let mut data = D3D12_FEATURE_DATA_SHADER_MODEL {
                HighestShaderModel: requested,
            };
            // SAFETY: the struct matches the feature.
            unsafe { feature(device, D3D12_FEATURE_SHADER_MODEL, &mut data) }
                .then_some(data.HighestShaderModel)
        })
        .unwrap_or(0)
}

/// Creates a device on `adapter` if it meets the baseline: feature level 12_0, shader model 6.6,
/// resource binding tier 3 (bindless through directly indexed heaps), mesh shader tier 1, wave
/// intrinsics and `ID3D12Device5` / `ID3D12GraphicsCommandList6` (Windows 10 2004).
pub(super) fn create_d3d12_device(
    adapter: &Com<IDXGIAdapter1>,
) -> Result<(Com<ID3D12Device>, i32)> {
    let create = d3d12_create_device().ok_or_else(unsupported)?;
    // SAFETY: valid adapter, IID and out pointer.
    let device = com::create::<ID3D12Device>(|iid, out| unsafe {
        create(adapter.as_raw(), D3D_FEATURE_LEVEL_12_0, iid, out)
    })
    .map_err(error)?;
    let model = shader_model(&device);
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    // SAFETY: the struct matches the feature.
    let tier = match unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS, &mut options) } {
        true => options.ResourceBindingTier,
        false => 0,
    };
    let mut options1 = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: the struct matches the feature.
    unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS1, &mut options1) };
    let mut options7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
    // SAFETY: the struct matches the feature. Older runtimes reject it, leaving tier 0.
    unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS7, &mut options7) };
    let interfaces = device.cast::<ID3D12Device5>().is_ok();
    if model < D3D_SHADER_MODEL_6_6
        || tier < D3D12_RESOURCE_BINDING_TIER_3
        || options7.MeshShaderTier == 0
        || options1.WaveOps == 0
        || !interfaces
    {
        marlin_diag::warn!(
            "adapter lacks the D3D12 baseline: shader model {:#x}, resource binding tier {tier}, \
             mesh shader tier {}, wave ops {}, ID3D12Device5 {interfaces}",
            model,
            options7.MeshShaderTier,
            options1.WaveOps != 0
        );
        return Err(unsupported());
    }
    Ok((device, model))
}

/// The device's optional features (the baseline was checked by [`create_d3d12_device`]).
pub(super) fn query_features(
    device: &Com<ID3D12Device>,
    model: i32,
    tearing: bool,
    desc: &DeviceDesc,
) -> Features {
    macro_rules! query {
        ($($name:ident: $ty:ident = $id:ident),* $(,)?) => {$(
            let mut $name = $ty::default();
            // SAFETY: the struct matches the feature. Older runtimes reject newer features,
            // leaving them zeroed (unsupported).
            unsafe { feature(device, $id, &mut $name) };
        )*};
    }
    query!(
        options: D3D12_FEATURE_DATA_D3D12_OPTIONS = D3D12_FEATURE_D3D12_OPTIONS,
        options1: D3D12_FEATURE_DATA_D3D12_OPTIONS1 = D3D12_FEATURE_D3D12_OPTIONS1,
        options2: D3D12_FEATURE_DATA_D3D12_OPTIONS2 = D3D12_FEATURE_D3D12_OPTIONS2,
        options3: D3D12_FEATURE_DATA_D3D12_OPTIONS3 = D3D12_FEATURE_D3D12_OPTIONS3,
        options4: D3D12_FEATURE_DATA_D3D12_OPTIONS4 = D3D12_FEATURE_D3D12_OPTIONS4,
        options5: D3D12_FEATURE_DATA_D3D12_OPTIONS5 = D3D12_FEATURE_D3D12_OPTIONS5,
        options6: D3D12_FEATURE_DATA_D3D12_OPTIONS6 = D3D12_FEATURE_D3D12_OPTIONS6,
        options7: D3D12_FEATURE_DATA_D3D12_OPTIONS7 = D3D12_FEATURE_D3D12_OPTIONS7,
        options9: D3D12_FEATURE_DATA_D3D12_OPTIONS9 = D3D12_FEATURE_D3D12_OPTIONS9,
        options12: D3D12_FEATURE_DATA_D3D12_OPTIONS12 = D3D12_FEATURE_D3D12_OPTIONS12,
        options16: D3D12_FEATURE_DATA_D3D12_OPTIONS16 = D3D12_FEATURE_D3D12_OPTIONS16,
    );
    let markers = D3D12_COMMAND_LIST_SUPPORT_FLAG_DIRECT
        | D3D12_COMMAND_LIST_SUPPORT_FLAG_COMPUTE
        | D3D12_COMMAND_LIST_SUPPORT_FLAG_COPY;
    let vrs = match options6.VariableShadingRateTier {
        D3D12_VARIABLE_SHADING_RATE_TIER_2.. => VariableRateShading::Image,
        D3D12_VARIABLE_SHADING_RATE_TIER_1 => VariableRateShading::PerDraw,
        _ => VariableRateShading::None,
    };
    Features {
        shader_model: ((model >> 4) as u8, (model & 0xf) as u8),
        enhanced_barriers: options12.EnhancedBarriersSupported != 0
            && !desc.disable_enhanced_barriers,
        tearing,
        mesh_shader: options7.MeshShaderTier != 0,
        ray_query: options5.RaytracingTier >= D3D12_RAYTRACING_TIER_1_1,
        // Shader model 6.6 has 64-bit atomics on raw buffers wherever it has 64-bit integers.
        atomic_int64: options1.Int64ShaderOps != 0
            && options9.AtomicInt64OnGroupSharedSupported != 0,
        texture_atomic_int64: options9.AtomicInt64OnTypedResourceSupported != 0,
        copy_queue_timestamps: options3.CopyQueueTimestampQueriesSupported != 0,
        mixed_heaps: options.ResourceHeapTier >= D3D12_RESOURCE_HEAP_TIER_2,
        wave_size_range: (options1.WaveLaneCountMin, options1.WaveLaneCountMax),
        shader_float16_int16: options4.Native16BitShaderOpsSupported != 0,
        shader_int64: options1.Int64ShaderOps != 0,
        barycentrics: options3.BarycentricsSupported != 0,
        sampler_min_max: options.TiledResourcesTier >= D3D12_TILED_RESOURCES_TIER_2,
        conservative_rasterization: options.ConservativeRasterizationTier >= 1,
        depth_bounds: options2.DepthBoundsTestSupported != 0,
        variable_rate_shading: vrs,
        shading_rate_tile_size: match vrs {
            VariableRateShading::Image => options6.ShadingRateImageTileSize,
            _ => 0,
        },
        pipeline_statistics: options9.MeshShaderPipelineStatsSupported != 0,
        buffer_markers: options3.WriteBufferImmediateSupportFlags & markers == markers,
        sparse_resources: options.TiledResourcesTier >= D3D12_TILED_RESOURCES_TIER_2,
        sparse_volumes: options.TiledResourcesTier >= D3D12_TILED_RESOURCES_TIER_3,
        gpu_upload_heap: options16.GPUUploadHeapSupported != 0,
    }
}

/// The format depth textures are read through, which carries their sampling support.
fn view_format(format: Format) -> u32 {
    match format {
        Format::D16Unorm => DXGI_FORMAT_R16_UNORM,
        Format::D32Float => DXGI_FORMAT_R32_FLOAT,
        Format::D32FloatS8 => DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS,
        f => dxgi_format(f),
    }
}

fn support(device: &Com<ID3D12Device>, format: u32) -> (u32, u32) {
    let mut data = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: the struct matches the feature.
    match unsafe { feature(device, D3D12_FEATURE_FORMAT_SUPPORT, &mut data) } {
        true => (data.Support1, data.Support2),
        false => (0, 0),
    }
}

pub(super) fn format_support(device: &Device, format: Format) -> FormatSupport {
    let dxgi = dxgi_format(format);
    if dxgi == DXGI_FORMAT_UNKNOWN {
        return FormatSupport::default();
    }
    let d = &device.device;
    let (s1, s2) = support(d, dxgi);
    let (v1, v2) = match format.is_depth() {
        true => support(d, view_format(format)),
        false => (s1, s2),
    };
    let textures = D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_TEXTURE3D;
    if (s1 | v1) & textures == 0 {
        return FormatSupport::default();
    }
    let mut features = FormatFeatures::default();
    let mut set = |condition: bool, flag| {
        if condition {
            features |= flag;
        }
    };
    set(
        v1 & D3D12_FORMAT_SUPPORT1_SHADER_LOAD != 0,
        FormatFeatures::SAMPLED,
    );
    set(
        v1 & D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE != 0 && !format.is_integer(),
        FormatFeatures::FILTER,
    );
    set(
        v1 & D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE_COMPARISON != 0,
        FormatFeatures::COMPARISON,
    );
    set(
        s2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE != 0
            && s1 & D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW != 0,
        FormatFeatures::STORAGE_WRITE,
    );
    set(
        s2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD != 0,
        FormatFeatures::STORAGE_READ,
    );
    set(
        s2 & D3D12_FORMAT_SUPPORT2_UAV_ATOMIC_ADD != 0,
        FormatFeatures::STORAGE_ATOMIC,
    );
    set(
        s1 & D3D12_FORMAT_SUPPORT1_RENDER_TARGET != 0,
        FormatFeatures::RENDER_TARGET,
    );
    set(
        s1 & D3D12_FORMAT_SUPPORT1_BLENDABLE != 0,
        FormatFeatures::BLEND,
    );
    set(
        s1 & D3D12_FORMAT_SUPPORT1_DEPTH_STENCIL != 0,
        FormatFeatures::DEPTH_STENCIL,
    );
    set(
        s1 & D3D12_FORMAT_SUPPORT1_MULTISAMPLE_RESOLVE != 0,
        FormatFeatures::RESOLVE,
    );
    set(
        v1 & D3D12_FORMAT_SUPPORT1_MULTISAMPLE_LOAD != 0,
        FormatFeatures::MULTISAMPLE_LOAD,
    );
    set(
        (s2 | v2) & D3D12_FORMAT_SUPPORT2_TILED != 0 && device.features.sparse_resources,
        FormatFeatures::SPARSE,
    );
    let mut sample_counts = 0;
    if features.intersects(FormatFeatures::RENDER_TARGET | FormatFeatures::DEPTH_STENCIL) {
        sample_counts |= 1 << 1;
        for count in [2, 4, 8, 16] {
            let mut levels = D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS {
                Format: dxgi,
                SampleCount: count,
                ..Default::default()
            };
            // SAFETY: the struct matches the feature.
            let ok = unsafe { feature(d, D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS, &mut levels) };
            if ok && levels.NumQualityLevels > 0 {
                sample_counts |= 1 << count;
            }
        }
    }
    FormatSupport {
        features,
        sample_counts,
    }
}
