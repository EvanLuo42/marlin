//! Pipelines: shader stages (SPIR-V plus the descriptor heap mapping, 0016 §4.2), compute
//! pipelines, the pipeline cache, and graphics and mesh pipelines (draw area). No pipeline has a
//! `VkPipelineLayout`: all are created with `VK_PIPELINE_CREATE_2_DESCRIPTOR_HEAP_BIT_EXT`.

use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::device::{Device, Shared};
use super::diag::c_name;
use super::ffi::*;
use super::{check, invalid};
use crate::{ComputePipelineDesc, GraphicsPipelineDesc, MeshPipelineDesc, Result, ShaderModule};

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// One shader stage of a pipeline being created: the SPIR-V (copied to `u32` alignment), its
/// entry point, and the set 0 mapping onto the descriptor heaps.
pub(crate) struct ShaderStage {
    code: Vec<u32>,
    entry: Vec<u8>,
    stage: VkShaderStageFlags,
    module: VkShaderModuleCreateInfo,
    mappings: [VkDescriptorSetAndBindingMappingEXT; 3],
    mapping: VkShaderDescriptorSetAndBindingMappingInfoEXT,
}

impl ShaderStage {
    /// `InvalidInput` unless `module` is SPIR-V.
    pub(crate) fn new(
        shared: &Shared,
        module: &ShaderModule<'_>,
        stage: VkShaderStageFlags,
    ) -> Result<Self> {
        let bytes = module.code;
        if bytes.len() < 20 || !bytes.len().is_multiple_of(4) {
            return Err(invalid());
        }
        let code: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        if code[0] != SPIRV_MAGIC {
            return Err(invalid());
        }
        Ok(Self {
            code,
            entry: c_name(module.entry),
            stage,
            module: VkShaderModuleCreateInfo::default(),
            mappings: shared.bindless.mappings(),
            mapping: VkShaderDescriptorSetAndBindingMappingInfoEXT::default(),
        })
    }

    /// The stage's create info. It points into `self`, which must not move or drop until the
    /// pipeline is created.
    pub(crate) fn info(&mut self) -> VkPipelineShaderStageCreateInfo {
        self.module.codeSize = self.code.len() * 4;
        self.module.pCode = self.code.as_ptr();
        self.mapping.pNext = (&raw const self.module).cast();
        self.mapping.mappingCount = self.mappings.len() as u32;
        self.mapping.pMappings = self.mappings.as_ptr();
        VkPipelineShaderStageCreateInfo {
            pNext: (&raw const self.mapping).cast(),
            stage: self.stage as VkShaderStageFlagBits,
            module: 0,
            pName: self.entry.as_ptr().cast(),
            ..Default::default()
        }
    }
}

/// The create flags every pipeline takes, to chain into its create info.
pub(crate) fn heap_flags() -> VkPipelineCreateFlags2CreateInfo {
    VkPipelineCreateFlags2CreateInfo {
        flags: VK_PIPELINE_CREATE_2_DESCRIPTOR_HEAP_BIT_EXT,
        ..Default::default()
    }
}

/// A `VkPipeline`, destroyed on drop.
pub(crate) struct Pipeline {
    shared: Arc<Shared>,
    raw: VkPipeline,
}

impl Pipeline {
    pub(crate) fn new(shared: &Arc<Shared>, raw: VkPipeline) -> Self {
        Self {
            shared: shared.clone(),
            raw,
        }
    }

    pub(crate) fn raw(&self) -> VkPipeline {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: no pending submission uses the pipeline (the public API's contract).
        unsafe { (self.shared.fns.vkDestroyPipeline)(self.shared.raw, self.raw, ptr::null()) };
    }
}

pub(crate) type ComputePipeline = Pipeline;

pub(crate) fn create_compute_pipeline(
    device: &Device,
    desc: &ComputePipelineDesc<'_>,
) -> Result<ComputePipeline> {
    let s = &device.shared;
    s.lost.check()?;
    let mut stage = ShaderStage::new(s, &desc.shader, VK_SHADER_STAGE_COMPUTE_BIT)?;
    let flags = heap_flags();
    let info = VkComputePipelineCreateInfo {
        pNext: (&raw const flags).cast(),
        stage: stage.info(),
        layout: 0,
        basePipelineIndex: -1,
        ..Default::default()
    };
    let cache = desc.cache.map_or(0, |c| c.raw.raw);
    let mut raw = 0;
    // SAFETY: a valid create info chain (pointing into `stage` and `flags`, alive here) and out
    // pointer.
    s.lost.result(unsafe {
        (s.fns.vkCreateComputePipelines)(s.raw, cache, 1, &info, ptr::null(), &mut raw)
    })?;
    Ok(Pipeline::new(s, raw))
}

// --- Graphics and mesh pipelines (draw area) ---

pub(crate) type GraphicsPipeline = Pipeline;
pub(crate) type MeshPipeline = Pipeline;

pub(crate) fn create_graphics_pipeline(
    device: &Device,
    desc: &GraphicsPipelineDesc<'_>,
) -> Result<GraphicsPipeline> {
    let _ = (device, desc);
    todo!("vulkan: create_graphics_pipeline")
}

pub(crate) fn create_mesh_pipeline(
    device: &Device,
    desc: &MeshPipelineDesc<'_>,
) -> Result<MeshPipeline> {
    let _ = (device, desc);
    todo!("vulkan: create_mesh_pipeline")
}

// --- Pipeline cache ---

pub(crate) struct PipelineCache {
    shared: Arc<Shared>,
    pub(crate) raw: VkPipelineCache,
}

impl PipelineCache {
    /// Seeds the cache with `data` only if its header names this device and driver; anything
    /// else starts empty.
    pub(crate) fn new(device: &Device, data: &[u8]) -> Result<Self> {
        let s = &device.shared;
        let p = &s.properties;
        let header_len = size_of::<VkPipelineCacheHeaderVersionOne>();
        let matches = data.len() >= header_len && {
            // SAFETY: at least a header's worth of bytes; read unaligned.
            let h = unsafe {
                ptr::read_unaligned(data.as_ptr().cast::<VkPipelineCacheHeaderVersionOne>())
            };
            h.headerVersion == VK_PIPELINE_CACHE_HEADER_VERSION_ONE
                && h.vendorID == p.vendor_id
                && h.deviceID == p.device_id
                && h.pipelineCacheUUID == p.pipeline_cache_uuid
        };
        let initial: &[u8] = if matches { data } else { &[] };
        let info = VkPipelineCacheCreateInfo {
            initialDataSize: initial.len(),
            pInitialData: initial.as_ptr().cast(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info and out pointer.
        s.lost.result(unsafe {
            (s.fns.vkCreatePipelineCache)(s.raw, &info, ptr::null(), &mut raw)
        })?;
        Ok(Self {
            shared: s.clone(),
            raw,
        })
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        let s = &self.shared;
        let mut len = 0;
        // SAFETY: a live cache; size query.
        check(unsafe {
            (s.fns.vkGetPipelineCacheData)(s.raw, self.raw, &mut len, ptr::null_mut())
        })?;
        let mut data = alloc::vec![0u8; len];
        // SAFETY: `data` holds `len` bytes.
        check(unsafe {
            (s.fns.vkGetPipelineCacheData)(s.raw, self.raw, &mut len, data.as_mut_ptr().cast())
        })?;
        data.truncate(len);
        Ok(data)
    }
}

impl Drop for PipelineCache {
    fn drop(&mut self) {
        // SAFETY: no pipeline creation uses the cache any more (`&mut self`).
        unsafe { (self.shared.fns.vkDestroyPipelineCache)(self.shared.raw, self.raw, ptr::null()) };
    }
}
