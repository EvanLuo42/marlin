//! The module level: ids, instruction encoding, types, constants and module variables, in the
//! order SPIR-V wants its sections.
//!
//! Types come in two forms. Memory the API lays out (storage buffers, push constants, uniform
//! buffers) needs `Offset`, `ArrayStride` and `MatrixStride` (scalar layout, 0004 §16); other
//! memory and values must not carry them in Vulkan. So a struct or array has a plain id and a
//! laid-out id, and loads and stores of aggregates convert with `OpCopyLogical`.

use std::collections::{BTreeSet, HashMap};

use marlin_shaderc_ir::{
    Binding, Builtin, Const, ConstValue, GlobalId, Interp, Module, ResKind, ResourceKind, Scalar,
    Space, Stage, StructId, Type, TypeData, scalar_field_offset, scalar_layout,
};
use marlin_shaderc_target::BackendError;

use crate::spirv::{capability as cap, decoration, image_format, op, storage_class as sc};

pub(crate) type R<T> = Result<T, BackendError>;

pub(crate) fn unsupported<T>(what: impl Into<String>) -> R<T> {
    Err(BackendError {
        message: format!("{} is not written for SPIR-V yet", what.into()),
    })
}

/// One instruction's words: the opcode with the word count, then the operands.
pub(crate) fn inst(opcode: u32, operands: &[u32]) -> Vec<u32> {
    let mut words = Vec::with_capacity(operands.len() + 1);
    words.push(((operands.len() as u32 + 1) << 16) | opcode);
    words.extend_from_slice(operands);
    words
}

/// A literal string: UTF-8, nul-terminated, padded to whole words.
pub(crate) fn string(s: &str) -> Vec<u32> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

/// The storage class of an address space.
pub(crate) fn storage_class(space: Space) -> R<u32> {
    Ok(match space {
        Space::Function => sc::Function,
        Space::GroupShared => sc::Workgroup,
        Space::Buffer => sc::StorageBuffer,
        Space::Uniform => sc::Uniform,
        Space::Constant => sc::Private,
        Space::Push => sc::PushConstant,
        Space::Input => sc::Input,
        Space::Output => sc::Output,
        // A mesh entry's outputs are arrays of `Output` variables (SPV_EXT_mesh_shader).
        Space::MeshVertex | Space::MeshPrimitive | Space::MeshIndex => sc::Output,
        Space::Payload => sc::TaskPayloadWorkgroupEXT,
    })
}

/// Whether the API lays out memory of this storage class (so its types carry offsets).
pub(crate) fn laid_out(class: u32) -> bool {
    matches!(class, sc::StorageBuffer | sc::Uniform | sc::PushConstant)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Ir(Type, bool),
    Words(&'static str, Vec<u32>),
}

/// A module-level variable as the function sees it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Global {
    /// A variable whose id is the IR value.
    Var(u32),
    /// The push constants: a block whose member 0 is the IR's struct.
    Push(u32),
}

pub(crate) struct Writer<'m> {
    pub m: &'m Module,
    next: u32,
    capabilities: BTreeSet<u32>,
    extensions: BTreeSet<&'static str>,
    glsl: Option<u32>,
    /// The `NonSemantic.DebugPrintf` import, once something prints.
    printf: Option<u32>,
    /// `OpString`s (the debug section, before the names).
    strings: Vec<u32>,
    pub names: Vec<u32>,
    pub annotations: Vec<u32>,
    /// Types, constants and module-level variables, each after what it refers to.
    pub globals: Vec<u32>,
    types: HashMap<Key, u32>,
    consts: HashMap<Const, u32>,
    module_vars: HashMap<GlobalId, Global>,
    /// The variables the entry point uses (its interface).
    pub interface: Vec<u32>,
    heap: Option<u32>,
    sampler_heap: Option<u32>,
    accel_heap: Option<u32>,
    slot: Option<u32>,
    builtins: HashMap<u32, u32>,
    /// The stage of the entry point being written.
    pub stage: Stage,
    /// The entry writes `FragDepth`.
    depth: bool,
}

impl<'m> Writer<'m> {
    pub fn new(m: &'m Module, stage: Stage) -> Writer<'m> {
        let mut w = Writer {
            m,
            stage,
            depth: false,
            next: 1,
            capabilities: BTreeSet::new(),
            extensions: BTreeSet::new(),
            glsl: None,
            printf: None,
            strings: Vec::new(),
            names: Vec::new(),
            annotations: Vec::new(),
            globals: Vec::new(),
            types: HashMap::new(),
            consts: HashMap::new(),
            module_vars: HashMap::new(),
            interface: Vec::new(),
            heap: None,
            sampler_heap: None,
            accel_heap: None,
            slot: None,
            builtins: HashMap::new(),
        };
        w.capability(cap::Shader);
        w
    }

    pub fn id(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    pub fn capability(&mut self, c: u32) {
        self.capabilities.insert(c);
    }

    pub fn extension(&mut self, e: &'static str) {
        self.extensions.insert(e);
    }

    pub fn glsl(&mut self) -> u32 {
        if let Some(id) = self.glsl {
            return id;
        }
        let id = self.id();
        self.glsl = Some(id);
        id
    }

    /// The `NonSemantic.DebugPrintf` instruction set, for Development `debug.print` (0005 §4.4).
    pub fn printf(&mut self) -> u32 {
        if let Some(id) = self.printf {
            return id;
        }
        self.extension("SPV_KHR_non_semantic_info");
        let id = self.id();
        self.printf = Some(id);
        id
    }

    /// An `OpString`.
    pub fn op_string(&mut self, s: &str) -> u32 {
        let id = self.id();
        let mut ops = vec![id];
        ops.extend(string(s));
        self.strings.extend(inst(op::String, &ops));
        id
    }

    pub fn name(&mut self, id: u32, name: &str) {
        let mut ops = vec![id];
        ops.extend(string(name));
        self.names.extend(inst(op::Name, &ops));
    }

    pub fn decorate(&mut self, id: u32, d: u32, extra: &[u32]) {
        let mut ops = vec![id, d];
        ops.extend_from_slice(extra);
        self.annotations.extend(inst(op::Decorate, &ops));
    }

    /// A type (or other global instruction) made from `words` once: `make` gets the new id and
    /// returns the operands after it.
    fn once(&mut self, tag: &'static str, opcode: u32, operands: Vec<u32>) -> u32 {
        let key = Key::Words(tag, {
            let mut k = vec![opcode];
            k.extend(&operands);
            k
        });
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let id = self.id();
        let mut ops = vec![id];
        ops.extend(operands);
        self.globals.extend(inst(opcode, &ops));
        self.types.insert(key, id);
        id
    }

    /// Like [`Self::once`] for instructions with a result type: `result_type id operands`.
    fn once_typed(
        &mut self,
        tag: &'static str,
        opcode: u32,
        result_type: u32,
        operands: Vec<u32>,
    ) -> u32 {
        let key = Key::Words(tag, {
            let mut k = vec![opcode, result_type];
            k.extend(&operands);
            k
        });
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let id = self.id();
        let mut ops = vec![result_type, id];
        ops.extend(operands);
        self.globals.extend(inst(opcode, &ops));
        self.types.insert(key, id);
        id
    }

    pub fn void(&mut self) -> u32 {
        self.once("", op::TypeVoid, vec![])
    }

    pub fn bool_ty(&mut self) -> u32 {
        self.once("", op::TypeBool, vec![])
    }

    pub fn uint_ty(&mut self, bits: u32) -> u32 {
        match bits {
            16 => self.capability(cap::Int16),
            64 => self.capability(cap::Int64),
            _ => {}
        }
        self.once("", op::TypeInt, vec![bits, 0])
    }

    pub fn fn_ty(&mut self, ret: u32, params: &[u32]) -> u32 {
        let mut ops = vec![ret];
        ops.extend_from_slice(params);
        self.once("", op::TypeFunction, ops)
    }

    pub fn ptr_ty(&mut self, class: u32, pointee: u32) -> u32 {
        self.once("", op::TypePointer, vec![class, pointee])
    }

    pub fn untyped_ptr_ty(&mut self, class: u32) -> u32 {
        self.untyped();
        self.once("", op::TypeUntypedPointerKHR, vec![class])
    }

    fn untyped(&mut self) {
        self.capability(cap::UntypedPointersKHR);
        self.extension("SPV_KHR_untyped_pointers");
    }

    /// A 32-bit unsigned constant.
    pub fn uint(&mut self, v: u32) -> u32 {
        self.lit(Scalar::I32, 1, u64::from(v))
    }

    pub fn scalar(&mut self, s: Scalar) -> u32 {
        match s {
            Scalar::I1 => self.bool_ty(),
            Scalar::I16 => self.uint_ty(16),
            Scalar::I32 => self.uint_ty(32),
            Scalar::I64 => self.uint_ty(64),
            Scalar::F16 => {
                self.capability(cap::Float16);
                self.once("", op::TypeFloat, vec![16])
            }
            Scalar::F32 => self.once("", op::TypeFloat, vec![32]),
            Scalar::F64 => {
                self.capability(cap::Float64);
                self.once("", op::TypeFloat, vec![64])
            }
        }
    }

    fn layout(&self, t: Type) -> R<(u64, u64)> {
        scalar_layout(self.m, t).ok_or_else(|| BackendError {
            message: "a type without a memory layout in laid-out memory".into(),
        })
    }

    /// The SPIR-V type of IR type `t`; `laid` for laid-out memory.
    pub fn ty(&mut self, t: Type, laid: bool) -> R<u32> {
        let laid = laid && matches!(self.m.data(t), TypeData::Array(..) | TypeData::Struct(_));
        if let Some(&id) = self.types.get(&Key::Ir(t, laid)) {
            return Ok(id);
        }
        let id = match self.m.data(t).clone() {
            TypeData::Scalar(s) => self.scalar(s),
            TypeData::Res(_) => self.uint_ty(32),
            TypeData::Vector(s, n) => {
                let e = self.scalar(s);
                self.once("", op::TypeVector, vec![e, n as u32])
            }
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => {
                let e = self.scalar(element);
                let col = self.once("", op::TypeVector, vec![e, rows as u32]);
                self.once("", op::TypeMatrix, vec![col, cols as u32])
            }
            TypeData::Array(e, n) => {
                let et = self.ty(e, laid)?;
                let len = self.uint(n);
                if laid {
                    let id = self.once("laid", op::TypeArray, vec![et, len]);
                    let (size, align) = self.layout(e)?;
                    let stride = size.div_ceil(align) * align;
                    self.decorate(id, decoration::ArrayStride, &[stride as u32]);
                    id
                } else {
                    self.once("", op::TypeArray, vec![et, len])
                }
            }
            TypeData::Struct(s) => self.struct_ty(t, s, laid)?,
            TypeData::Ptr { space, pointee, .. } => {
                let class = storage_class(space)?;
                let p = self.ty(pointee, laid_out(class))?;
                self.ptr_ty(class, p)
            }
            // Textures and samplers are loaded from the heap; buffers are pointers into it.
            TypeData::Handle(k) if k.kind.texture().is_some() => self.image_ty(&k)?,
            TypeData::Handle(k) if k.kind == ResourceKind::AccelerationStructure => self.accel_ty(),
            TypeData::Handle(k)
                if matches!(
                    k.kind,
                    ResourceKind::Sampler | ResourceKind::ComparisonSampler
                ) =>
            {
                self.sampler_ty()
            }
            TypeData::Handle(_) => self.untyped_ptr_ty(sc::UniformConstant),
            TypeData::RayQuery => {
                self.ray_query_ext();
                self.once("", op::TypeRayQueryKHR, vec![])
            }
        };
        self.types.insert(Key::Ir(t, laid), id);
        Ok(id)
    }

    fn struct_ty(&mut self, t: Type, s: StructId, laid: bool) -> R<u32> {
        let fields = self.m.structs[s].fields.clone();
        let mut members = Vec::new();
        for (_, f) in &fields {
            members.push(self.ty(*f, laid)?);
        }
        // Each IR struct is its own SPIR-V struct (members may be decorated differently).
        let id = self.id();
        let mut ops = vec![id];
        ops.extend(&members);
        self.globals.extend(inst(op::TypeStruct, &ops));
        let name = self.m.structs[s].name.clone();
        self.name(id, &name);
        for (i, (field, _)) in fields.iter().enumerate() {
            let mut ops = vec![id, i as u32];
            ops.extend(string(field));
            self.names.extend(inst(op::MemberName, &ops));
        }
        if laid {
            for (i, (_, f)) in fields.iter().enumerate() {
                let offset =
                    scalar_field_offset(self.m, t, i as u32).ok_or_else(|| BackendError {
                        message: "a struct field without an offset".into(),
                    })?;
                self.member_decorate(id, i as u32, decoration::Offset, &[offset as u32]);
                self.matrix_layout(id, i as u32, *f);
            }
        }
        Ok(id)
    }

    pub fn member_decorate(&mut self, id: u32, member: u32, d: u32, extra: &[u32]) {
        let mut ops = vec![id, member, d];
        ops.extend_from_slice(extra);
        self.annotations.extend(inst(op::MemberDecorate, &ops));
    }

    /// Matrix members (or arrays of them) say they are column-major and their column stride.
    fn matrix_layout(&mut self, s: u32, member: u32, mut t: Type) {
        while let TypeData::Array(e, _) = self.m.data(t) {
            t = *e;
        }
        if let TypeData::Matrix { element, rows, .. } = self.m.data(t) {
            let bytes = match element {
                Scalar::I16 | Scalar::F16 => 2,
                Scalar::I64 | Scalar::F64 => 8,
                _ => 4,
            };
            let stride = bytes * *rows as u32;
            self.member_decorate(s, member, decoration::ColMajor, &[]);
            self.member_decorate(s, member, decoration::MatrixStride, &[stride]);
        }
    }

    /// A Block struct around `inner` at offset 0, for push constants and buffers.
    pub fn block(&mut self, tag: &'static str, inner: u32) -> u32 {
        let key = Key::Words(tag, vec![inner]);
        if let Some(&id) = self.types.get(&key) {
            return id;
        }
        let id = self.id();
        self.globals.extend(inst(op::TypeStruct, &[id, inner]));
        self.decorate(id, decoration::Block, &[]);
        self.member_decorate(id, 0, decoration::Offset, &[0]);
        self.types.insert(key, id);
        id
    }

    /// A byte buffer: a Block holding a runtime array of 32-bit words.
    pub fn word_block(&mut self) -> u32 {
        let e = self.uint_ty(32);
        let key = Key::Words("rta", vec![e]);
        let rta = match self.types.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.id();
                self.globals.extend(inst(op::TypeRuntimeArray, &[id, e]));
                self.decorate(id, decoration::ArrayStride, &[4]);
                self.types.insert(key, id);
                id
            }
        };
        self.block("buffer", rta)
    }

    /// A buffer of `element`s: a Block holding a runtime array, laid out.
    pub fn buffer_block(&mut self, element: Type) -> R<u32> {
        let e = self.ty(element, true)?;
        let key = Key::Words("rta", vec![e]);
        let rta = match self.types.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.id();
                self.globals.extend(inst(op::TypeRuntimeArray, &[id, e]));
                let (size, align) = self.layout(element)?;
                let stride = size.div_ceil(align) * align;
                self.decorate(id, decoration::ArrayStride, &[stride as u32]);
                self.types.insert(key, id);
                id
            }
        };
        Ok(self.block("buffer", rta))
    }

    // ---- constants ----

    pub fn konst(&mut self, c: Const) -> R<u32> {
        if let Some(&id) = self.consts.get(&c) {
            return Ok(id);
        }
        let data = self.m.consts[c].clone();
        let t = self.ty(data.ty, false)?;
        let id = match &data.value {
            ConstValue::Int(bits) | ConstValue::Float(bits) => match self.m.data(data.ty) {
                // Scalars share the literal table, so each value is declared once.
                TypeData::Scalar(s) => self.lit(*s, 1, *bits),
                _ => self.lit(Scalar::I32, 1, *bits),
            },
            ConstValue::Zero | ConstValue::Any => {
                let id = self.id();
                let opcode = if data.value == ConstValue::Zero {
                    op::ConstantNull
                } else {
                    op::Undef
                };
                self.globals.extend(inst(opcode, &[t, id]));
                id
            }
            ConstValue::Composite(items) => {
                let mut parts = Vec::new();
                for &item in items {
                    parts.push(self.konst(item)?);
                }
                let id = self.id();
                let mut ops = vec![t, id];
                ops.extend(parts);
                self.globals.extend(inst(op::ConstantComposite, &ops));
                id
            }
        };
        self.consts.insert(c, id);
        Ok(id)
    }

    // ---- module variables ----

    pub fn module_var(&mut self, g: GlobalId) -> R<Global> {
        if let Some(&v) = self.module_vars.get(&g) {
            return Ok(v);
        }
        let data = self.m.globals[g].clone();
        let TypeData::Ptr { space, pointee, .. } = *self.m.data(data.ty) else {
            return unsupported("a module variable that is not a pointer");
        };
        let class = storage_class(space)?;
        let v = match space {
            Space::Push => {
                let inner = self.ty(pointee, true)?;
                let block = self.block("push", inner);
                let ptr = self.ptr_ty(class, block);
                let var = self.id();
                self.globals.extend(inst(op::Variable, &[ptr, var, class]));
                self.name(var, &data.name);
                Global::Push(var)
            }
            _ => {
                let t = self.ty(pointee, false)?;
                let ptr = self.ptr_ty(class, t);
                let var = self.id();
                let mut ops = vec![ptr, var, class];
                if let Some(init) = data.init {
                    ops.push(self.konst(init)?);
                }
                self.globals.extend(inst(op::Variable, &ops));
                self.name(var, &data.name);
                match data.binding {
                    Binding::Builtin(b) => {
                        let builtin = crate::body::builtin_of(b, self.stage, space)?;
                        self.decorate(var, decoration::BuiltIn, &[builtin]);
                        match b {
                            Builtin::InstanceId => self.capability(cap::DrawParameters),
                            Builtin::SampleIndex => self.capability(cap::SampleRateShading),
                            // A pixel entry's primitive id (DXC's choice for `SV_PrimitiveID` too;
                            // mesh entries have `MeshShadingEXT`).
                            Builtin::PrimitiveId if self.stage == Stage::Pixel => {
                                self.capability(cap::Geometry)
                            }
                            Builtin::Depth => self.depth = true,
                            _ => {}
                        }
                        // A pixel entry's integer inputs are `Flat`, built-ins too.
                        if self.stage == Stage::Pixel
                            && space == Space::Input
                            && matches!(b, Builtin::PrimitiveId | Builtin::SampleIndex)
                        {
                            self.decorate(var, decoration::Flat, &[]);
                        }
                    }
                    Binding::Location(l) => self.decorate(var, decoration::Location, &[l]),
                    // Dual-source blending: target 0's second source is location 0, index 1.
                    Binding::Target(t, source) => {
                        self.decorate(var, decoration::Location, &[t]);
                        if source != 0 {
                            self.decorate(var, decoration::Index, &[source]);
                        }
                    }
                    Binding::None => {}
                }
                match space {
                    // Per-primitive outputs of a mesh entry.
                    Space::MeshPrimitive => self.decorate(var, decoration::PerPrimitiveEXT, &[]),
                    // The index list: its built-in follows the topology.
                    Space::MeshIndex => {
                        let width = match self.m.data(pointee) {
                            TypeData::Array(e, _) => self.m.width(*e).unwrap_or(3),
                            _ => 3,
                        };
                        let b = match width {
                            1 => crate::spirv::builtin::PrimitivePointIndicesEXT,
                            2 => crate::spirv::builtin::PrimitiveLineIndicesEXT,
                            _ => crate::spirv::builtin::PrimitiveTriangleIndicesEXT,
                        };
                        self.decorate(var, decoration::BuiltIn, &[b]);
                    }
                    _ => {}
                }
                if let Some(i) = data.interp {
                    let d = match i {
                        Interp::Perspective => None,
                        Interp::NoPerspective => Some(decoration::NoPerspective),
                        Interp::Flat => Some(decoration::Flat),
                        Interp::Centroid => Some(decoration::Centroid),
                        Interp::Sample => {
                            self.capability(cap::SampleRateShading);
                            Some(decoration::Sample)
                        }
                    };
                    if let Some(d) = d {
                        self.decorate(var, d, &[]);
                    }
                }
                Global::Var(var)
            }
        };
        let id = match v {
            Global::Var(id) | Global::Push(id) => id,
        };
        self.interface.push(id);
        self.module_vars.insert(g, v);
        Ok(v)
    }

    /// A built-in input of type `u32` (subgroup size, lane index), made once.
    pub fn builtin_input(&mut self, builtin: u32) -> u32 {
        if let Some(&v) = self.builtins.get(&builtin) {
            return v;
        }
        let t = self.uint_ty(32);
        let ptr = self.ptr_ty(sc::Input, t);
        let var = self.id();
        self.globals
            .extend(inst(op::Variable, &[ptr, var, sc::Input]));
        self.decorate(var, decoration::BuiltIn, &[builtin]);
        self.interface.push(var);
        self.builtins.insert(builtin, var);
        var
    }

    // ---- the descriptor heap (0002 §6, 0016 §4.2) ----

    fn heap_ext(&mut self) {
        self.untyped();
        self.capability(cap::DescriptorHeapEXT);
        self.extension("SPV_EXT_descriptor_heap");
    }

    /// The resource heap variable.
    pub fn heap(&mut self) -> u32 {
        if let Some(h) = self.heap {
            return h;
        }
        self.heap_ext();
        let ptr = self.untyped_ptr_ty(sc::UniformConstant);
        let var = self.id();
        self.globals.extend(inst(
            op::UntypedVariableKHR,
            &[ptr, var, sc::UniformConstant],
        ));
        self.decorate(
            var,
            decoration::BuiltIn,
            &[crate::spirv::builtin::ResourceHeapEXT],
        );
        self.name(var, "resource_heap");
        self.interface.push(var);
        self.heap = Some(var);
        var
    }

    /// The width of a resource heap slot: the larger of an image's and a buffer's descriptor,
    /// as the driver sizes them (0002 §6).
    fn slot(&mut self) -> u32 {
        if let Some(s) = self.slot {
            return s;
        }
        self.heap_ext();
        let u32t = self.uint_ty(32);
        let float = self.scalar(Scalar::F32);
        // An image type stands for every image descriptor (they share one size).
        let image = self.once("", op::TypeImage, vec![float, 1, 0, 0, 0, 1, 0]);
        let buffer = self.once("", op::TypeBufferEXT, vec![sc::StorageBuffer]);
        let image_size = self.once_typed("", op::ConstantSizeOfEXT, u32t, vec![image]);
        let buffer_size = self.once_typed("", op::ConstantSizeOfEXT, u32t, vec![buffer]);
        let bool_t = self.bool_ty();
        let bigger = self.id();
        self.globals.extend(inst(
            op::SpecConstantOp,
            &[bool_t, bigger, op::UGreaterThan, image_size, buffer_size],
        ));
        let slot = self.id();
        self.globals.extend(inst(
            op::SpecConstantOp,
            &[u32t, slot, op::Select, bigger, image_size, buffer_size],
        ));
        self.slot = Some(slot);
        slot
    }

    /// The heap seen as an array of `kind` descriptors.
    pub fn heap_array(&mut self, kind: &ResKind) -> R<u32> {
        self.heap_ext();
        // Images and samplers are descriptors of their own type; buffers `OpTypeBufferEXT`.
        let (desc, sampler) = match kind.kind {
            k if k.texture().is_some() => (self.image_ty(kind)?, false),
            ResourceKind::Sampler | ResourceKind::ComparisonSampler => (self.sampler_ty(), true),
            ResourceKind::Buffer
            | ResourceKind::ReadOnlyBuffer
            | ResourceKind::ByteBuffer
            | ResourceKind::ReadOnlyByteBuffer => (
                self.once("", op::TypeBufferEXT, vec![sc::StorageBuffer]),
                false,
            ),
            ResourceKind::Uniform => (self.once("", op::TypeBufferEXT, vec![sc::Uniform]), false),
            other => return unsupported(format!("a `{other:?}` resource")),
        };
        let key = Key::Words("heap", vec![desc]);
        if let Some(&id) = self.types.get(&key) {
            return Ok(id);
        }
        let slot = if sampler {
            self.sampler_slot()
        } else {
            self.slot()
        };
        let id = self.id();
        self.globals.extend(inst(op::TypeRuntimeArray, &[id, desc]));
        self.annotations.extend(inst(
            op::DecorateId,
            &[id, decoration::ArrayStrideIdEXT, slot],
        ));
        self.types.insert(key, id);
        Ok(id)
    }

    /// The scalar type images of `kind` hold: a signed integer for `signed` textures, the IR's
    /// (unsigned) one otherwise.
    pub fn image_scalar(&mut self, kind: &ResKind) -> R<u32> {
        let texel = kind.element.ok_or_else(|| BackendError {
            message: "a texture without a texel type".into(),
        })?;
        let s = self.m.element_scalar(texel).ok_or_else(|| BackendError {
            message: "a texel that is not a scalar or vector".into(),
        })?;
        Ok(if kind.signed {
            let bits = s.bits();
            self.once("", op::TypeInt, vec![bits, 1])
        } else {
            self.scalar(s)
        })
    }

    /// Four components of what images of `kind` hold: what reads return and writes take.
    pub fn image_texel4(&mut self, kind: &ResKind) -> R<u32> {
        let s = self.image_scalar(kind)?;
        Ok(self.once("", op::TypeVector, vec![s, 4]))
    }

    /// The image type of a texture kind (0006 §8.6): its texel's scalar (signed for `signed`
    /// textures), dimension, array and multisample bits; storage textures without a format,
    /// except 64-bit ones, which are `R64ui` or `R64i` (their only formats, which Vulkan
    /// reads and writes only when the shader names them).
    pub fn image_ty(&mut self, kind: &ResKind) -> R<u32> {
        let format = match (self.texel_bits(kind)?, kind.signed) {
            (64, true) => image_format::R64i,
            (64, false) => image_format::R64ui,
            _ => image_format::Unknown,
        };
        self.image_ty_with(kind, format)
    }

    /// The bits of the scalar images of `kind` hold.
    pub fn texel_bits(&mut self, kind: &ResKind) -> R<u32> {
        let texel = kind.element.ok_or_else(|| BackendError {
            message: "a texture without a texel type".into(),
        })?;
        Ok(self.m.element_scalar(texel).map_or(0, |s| s.bits()))
    }

    /// The image type of a texture kind with image format `format` (a storage texture's for
    /// atomics, which need one).
    pub fn image_ty_with(&mut self, kind: &ResKind, format: u32) -> R<u32> {
        let shape = kind.kind.texture().ok_or_else(|| BackendError {
            message: format!("`{:?}` is not a texture", kind.kind),
        })?;
        let sampled_type = self.image_scalar(kind)?;
        let dim = match (shape.cube, shape.dims) {
            (true, _) => 3,
            (false, 3) => 2,
            _ => 1,
        };
        if shape.cube && shape.array {
            self.capability(cap::SampledCubeArray);
        }
        if shape.multisampled && shape.array {
            self.capability(cap::ImageMSArray);
        }
        let sampled = if shape.storage { 2 } else { 1 };
        if self.texel_bits(kind)? == 64 {
            self.capability(cap::Int64ImageEXT);
            self.extension("SPV_EXT_shader_image_int64");
        }
        Ok(self.once(
            "",
            op::TypeImage,
            vec![
                sampled_type,
                dim,
                0,
                shape.array as u32,
                shape.multisampled as u32,
                sampled,
                format,
            ],
        ))
    }

    pub fn sampler_ty(&mut self) -> u32 {
        self.once("", op::TypeSampler, vec![])
    }

    /// The sampler heap variable.
    pub fn sampler_heap(&mut self) -> u32 {
        if let Some(h) = self.sampler_heap {
            return h;
        }
        self.heap_ext();
        let ptr = self.untyped_ptr_ty(sc::UniformConstant);
        let var = self.id();
        self.globals.extend(inst(
            op::UntypedVariableKHR,
            &[ptr, var, sc::UniformConstant],
        ));
        self.decorate(
            var,
            decoration::BuiltIn,
            &[crate::spirv::builtin::SamplerHeapEXT],
        );
        self.name(var, "sampler_heap");
        self.interface.push(var);
        self.sampler_heap = Some(var);
        var
    }

    /// The width of a sampler heap slot: a sampler descriptor's size (0002 §6).
    fn sampler_slot(&mut self) -> u32 {
        let u32t = self.uint_ty(32);
        let sampler = self.sampler_ty();
        self.once_typed("", op::ConstantSizeOfEXT, u32t, vec![sampler])
    }

    /// The whole module: header and sections, around the entry point's code.
    pub fn finish(mut self, entry: u32, name: &str, code: Vec<u32>) -> Vec<u32> {
        if matches!(self.stage, Stage::Mesh(_) | Stage::Amplification(_)) {
            self.capability(cap::MeshShadingEXT);
            self.extension("SPV_EXT_mesh_shader");
        }
        let glsl = self.glsl;
        let mut out = vec![
            crate::spirv::MAGIC,
            crate::spirv::VERSION_1_6,
            0,
            self.next,
            0,
        ];
        for &c in &self.capabilities.clone() {
            out.extend(inst(op::Capability, &[c]));
        }
        for e in &self.extensions {
            out.extend(inst(op::Extension, &string(e)));
        }
        if let Some(g) = glsl {
            let mut ops = vec![g];
            ops.extend(string("GLSL.std.450"));
            out.extend(inst(op::ExtInstImport, &ops));
        }
        if let Some(p) = self.printf {
            let mut ops = vec![p];
            ops.extend(string("NonSemantic.DebugPrintf"));
            out.extend(inst(op::ExtInstImport, &ops));
        }
        out.extend(inst(op::MemoryModel, &[0, 1]));
        use crate::spirv::{execution_mode as mode, execution_model as model};
        let execution_model = match self.stage {
            Stage::Vertex => model::Vertex,
            Stage::Pixel => model::Fragment,
            Stage::Mesh(_) => model::MeshEXT,
            Stage::Amplification(_) => model::TaskEXT,
            Stage::Compute(_) => model::GLCompute,
        };
        let mut ops = vec![execution_model, entry];
        ops.extend(string("main"));
        ops.extend(&self.interface);
        out.extend(inst(op::EntryPoint, &ops));
        if let Some(size) = self.stage.group_size() {
            let mut ops = vec![entry, mode::LocalSize];
            ops.extend(size);
            out.extend(inst(op::ExecutionMode, &ops));
        }
        match self.stage {
            // The capacity and topology, from the output arrays.
            Stage::Mesh(_) => {
                let array = |space: Space| {
                    self.m
                        .globals
                        .iter()
                        .find_map(|(_, g)| match self.m.data(g.ty) {
                            TypeData::Ptr {
                                space: s, pointee, ..
                            } if *s == space => match self.m.data(*pointee) {
                                TypeData::Array(e, n) => Some((*e, *n)),
                                _ => None,
                            },
                            _ => None,
                        })
                };
                let vertices = array(Space::MeshVertex).map_or(0, |(_, n)| n);
                let (primitives, width) = array(Space::MeshIndex)
                    .map_or((0, 3), |(e, n)| (n, self.m.width(e).unwrap_or(3)));
                let topology = match width {
                    1 => mode::OutputPoints,
                    2 => mode::OutputLinesEXT,
                    _ => mode::OutputTrianglesEXT,
                };
                out.extend(inst(
                    op::ExecutionMode,
                    &[entry, mode::OutputVertices, vertices],
                ));
                out.extend(inst(
                    op::ExecutionMode,
                    &[entry, mode::OutputPrimitivesEXT, primitives],
                ));
                out.extend(inst(op::ExecutionMode, &[entry, topology]));
            }
            Stage::Pixel => {
                // Window coordinates from the top left (0002 §4).
                out.extend(inst(op::ExecutionMode, &[entry, mode::OriginUpperLeft]));
                if self.depth {
                    out.extend(inst(op::ExecutionMode, &[entry, mode::DepthReplacing]));
                }
            }
            _ => {}
        }
        self.name(entry, name);
        out.extend(&self.strings);
        out.extend(&self.names);
        out.extend(&self.annotations);
        out.extend(&self.globals);
        out.extend(code);
        out[3] = self.next;
        out
    }
}

impl Writer<'_> {
    /// A bool, or a vector of `n` bools.
    pub fn bool_vec(&mut self, n: u8) -> u32 {
        let b = self.bool_ty();
        if n <= 1 {
            b
        } else {
            self.once("", op::TypeVector, vec![b, n as u32])
        }
    }

    /// A struct of two `t`s, as `OpUMulExtended` returns.
    pub fn struct_pair(&mut self, t: u32) -> u32 {
        self.once("pair", op::TypeStruct, vec![t, t])
    }

    /// A constant of scalar `s` with these bits, or a vector of `n` of them.
    pub fn lit(&mut self, s: Scalar, n: u8, bits: u64) -> u32 {
        let st = self.scalar(s);
        let scalar = if s == Scalar::I1 {
            let opcode = if bits & 1 == 1 {
                op::ConstantTrue
            } else {
                op::ConstantFalse
            };
            self.once_typed("", opcode, st, vec![])
        } else {
            let words = if matches!(s, Scalar::I64 | Scalar::F64) {
                vec![bits as u32, (bits >> 32) as u32]
            } else {
                vec![bits as u32]
            };
            self.once_typed("lit", op::Constant, st, words)
        };
        if n <= 1 {
            return scalar;
        }
        let vt = self.once("", op::TypeVector, vec![st, n as u32]);
        self.once_typed("lit", op::ConstantComposite, vt, vec![scalar; n as usize])
    }
}

impl Writer<'_> {
    /// `OpUndef` of type id `t`, made once.
    pub fn undef(&mut self, t: u32) -> u32 {
        self.once_typed("undef", op::Undef, t, vec![])
    }
}

impl Writer<'_> {
    /// A vector of `n` `s`s, or `s` itself for 1.
    pub fn vec_ty(&mut self, s: Scalar, n: u8) -> u32 {
        let e = self.scalar(s);
        if n <= 1 {
            e
        } else {
            self.once("", op::TypeVector, vec![e, n as u32])
        }
    }

    pub fn sampled_image_ty(&mut self, image: u32) -> u32 {
        self.once("", op::TypeSampledImage, vec![image])
    }
}

impl Writer<'_> {
    pub fn ray_query_ext(&mut self) {
        self.capability(cap::RayQueryKHR);
        self.extension("SPV_KHR_ray_query");
    }

    pub fn accel_ty(&mut self) -> u32 {
        self.ray_query_ext();
        self.once("", op::TypeAccelerationStructureKHR, vec![])
    }

    /// The resource heap seen as acceleration structures, through set 0 binding 0, which the
    /// RHI maps onto the heap with the slot width as the stride (0016 §4.2). Acceleration
    /// structures are not loaded from `ResourceHeapEXT` itself: a ray query on one loaded that
    /// way finds nothing on NVIDIA's driver (2026-10-18), though validation accepts it.
    pub fn accel_heap(&mut self) -> u32 {
        if let Some(h) = self.accel_heap {
            return h;
        }
        let accel = self.accel_ty();
        self.capability(cap::RuntimeDescriptorArray);
        let array = self.once("accel_heap", op::TypeRuntimeArray, vec![accel]);
        let ptr = self.ptr_ty(sc::UniformConstant, array);
        let var = self.id();
        self.globals
            .extend(inst(op::Variable, &[ptr, var, sc::UniformConstant]));
        self.decorate(var, decoration::DescriptorSet, &[0]);
        self.decorate(var, decoration::Binding, &[0]);
        self.name(var, "acceleration_structures");
        self.interface.push(var);
        self.accel_heap = Some(var);
        var
    }
}
