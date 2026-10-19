//! Our own SPIR-V validator (0005 §4.6; step 9b): the rules of SPIR-V 1.6 and its Vulkan
//! environment that the backend's code relies on, checked without outside tools. It knows the
//! instructions the backend writes and says so about others.
//!
//! - The module: the header, section order, ids (each defined once, under the bound, defined
//!   before use outside the places SPIR-V lets refer forward), types (well formed, no
//!   duplicate non-aggregate types), constants, one memory model, entry points (execution
//!   mode, an interface listing every module variable they reach), capabilities and
//!   extensions for what is used.
//! - Layout (Vulkan): blocks in laid-out memory are `Block` structs whose members have
//!   offsets, arrays strides, matrices strides, all in scalar layout (naturally aligned, no
//!   overlap); types in other memory have none of these; built-ins have the types their
//!   variables need; the descriptor heaps are untyped `UniformConstant` variables.
//! - Functions ([`func`]): blocks and terminators, `OpPhi` and `OpVariable` where they may
//!   go, operand and result types, definitions dominating uses, blocks after their
//!   dominators, and structured control flow as SPIR-V 2.11 defines it (constructs by
//!   structural dominance; exits only to a selection's merge, the innermost loop's merge or
//!   continue target, or the innermost switch's merge; one back edge per loop, from its
//!   continue construct).

mod func;
mod operands;
mod types;

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::spirv::{builtin, capability as cap, decoration, op, storage_class as sc};
use types::{Ty, Types};

/// One instruction: where it is, its opcode, its operand words, and what they are.
#[derive(Clone, Debug)]
pub(crate) struct Inst {
    pub at: usize,
    pub opcode: u32,
    pub ops: Vec<u32>,
    pub result_type: Option<u32>,
    pub result: Option<u32>,
    /// Operand positions that are ids (not the result type or result).
    pub id_at: Vec<usize>,
}

impl Inst {
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.id_at.iter().map(|&k| self.ops[k])
    }

    pub fn name(&self) -> String {
        let n = op::name(self.opcode)
            .map_or_else(|| format!("Op{}", self.opcode), |n| format!("Op{n}"));
        match self.result {
            Some(r) => format!("%{r} = {n}"),
            None => format!("{n} (word {})", self.at),
        }
    }
}

/// The problems in `words`, a SPIR-V module (empty: valid).
pub fn validate(words: &[u32]) -> Vec<String> {
    let mut v = Validator::default();
    v.run(words);
    // A type reached two ways is reported once.
    let mut seen = HashSet::new();
    v.problems.retain(|p| seen.insert(p.clone()));
    v.problems
}

/// Decorations: (decoration, extra operands).
type Decorations = Vec<(u32, Vec<u32>)>;

#[derive(Default)]
pub(crate) struct Validator {
    pub problems: Vec<String>,
    pub insts: Vec<Inst>,
    pub bound: u32,
    /// The instruction that defines each id.
    pub defs: HashMap<u32, usize>,
    pub types: Types,
    /// Decorations of ids, and of (struct, member) pairs.
    pub decorations: HashMap<u32, Decorations>,
    pub member_decorations: HashMap<(u32, u32), Decorations>,
    pub capabilities: BTreeSet<u32>,
    pub extensions: BTreeSet<String>,
    /// Module variables: id to (storage class, pointer type).
    pub globals: HashMap<u32, (u32, u32)>,
}

impl Validator {
    pub fn error(&mut self, message: impl Into<String>) {
        self.problems.push(message.into());
    }

    /// The execution models of the entry points.
    pub fn stage_models(&self) -> Vec<u32> {
        self.insts
            .iter()
            .filter(|i| i.opcode == op::EntryPoint)
            .map(|i| i.ops[0])
            .collect()
    }

    fn run(&mut self, words: &[u32]) {
        if words.len() < 5 {
            return self.error("too short for a SPIR-V header");
        }
        if words[0] != crate::spirv::MAGIC {
            return self.error("not SPIR-V: the magic number is wrong");
        }
        if words[1] > crate::spirv::VERSION_1_6 || words[1] & 0xff00_00ff != 0 {
            self.error(format!(
                "version {:#x} is not one this validator checks (up to 1.6)",
                words[1]
            ));
        }
        if words[4] != 0 {
            self.error("the header's schema word is not 0");
        }
        self.bound = words[3];
        if !self.parse(&words[5..]) {
            return;
        }
        self.ids();
        if !self.problems.is_empty() {
            // Later checks look ids up; with broken ids they would only repeat it.
            return;
        }
        self.sections();
        self.collect();
        self.capability_rules();
        self.memory_model();
        self.entry_points();
        self.layout_rules();
        self.builtins();
        self.stage_io();
        func::functions(self);
    }

    /// Splits the words into instructions and decodes each one's operands.
    fn parse(&mut self, words: &[u32]) -> bool {
        let mut at = 0;
        while at < words.len() {
            let count = (words[at] >> 16) as usize;
            let opcode = words[at] & 0xffff;
            if count == 0 || at + count > words.len() {
                self.error(format!(
                    "word {}: an instruction that runs past the end",
                    at + 5
                ));
                return false;
            }
            let ops = words[at + 1..at + count].to_vec();
            let Some(layout) = operands::layout(opcode) else {
                self.error(format!(
                    "word {}: opcode {opcode} is not one this validator knows",
                    at + 5
                ));
                at += count;
                continue;
            };
            match decode(layout, &ops, |id| self.const_width(id)) {
                Ok((result_type, result, id_at)) => self.insts.push(Inst {
                    at: at + 5,
                    opcode,
                    ops,
                    result_type,
                    result,
                    id_at,
                }),
                Err(e) => {
                    let n = op::name(opcode).unwrap_or("?");
                    self.error(format!("word {}: `Op{n}`: {e}", at + 5));
                }
            }
            // Types are known as they come, for literals whose width depends on them.
            if let Some(i) = self.insts.last()
                && i.at == at + 5
            {
                let i = i.clone();
                types::declare(self, &i);
            }
            at += count;
        }
        true
    }

    /// How many words a literal of the type of id `id` (a constant's type, a switch's
    /// selector) takes.
    fn const_width(&self, id: u32) -> usize {
        let t = self
            .defs
            .get(&id)
            .and_then(|&k| self.insts[k].result_type)
            .unwrap_or(id);
        match self.types.get(t) {
            Some(Ty::Int { width, .. } | Ty::Float { width }) if *width == 64 => 2,
            _ => 1,
        }
    }

    /// Results unique and under the bound; ids used defined (or allowed to come later).
    fn ids(&mut self) {
        let mut seen = HashMap::new();
        let mut over: Vec<String> = Vec::new();
        for (k, i) in self.insts.iter().enumerate() {
            if let Some(r) = i.result {
                if r == 0 || r >= self.bound {
                    over.push(i.name());
                }
                if seen.insert(r, k).is_some() {
                    self.problems
                        .push(format!("{}: id {r} is defined twice", i.name()));
                }
            }
        }
        if let Some(first) = over.first() {
            self.problems.push(format!(
                "{} result id(s) are not under the bound {} (the first: {first})",
                over.len(),
                self.bound
            ));
        }
        self.defs = seen;
        // Outside functions, an id is defined before it is used, except where SPIR-V lets
        // names, decorations and entry points refer ahead (this keeps types acyclic).
        for (k, i) in self.insts.iter().enumerate() {
            if i.opcode == op::Function {
                break;
            }
            let ahead_ok = matches!(
                i.opcode,
                op::EntryPoint
                    | op::ExecutionMode
                    | op::ExecutionModeId
                    | op::Name
                    | op::MemberName
                    | op::Decorate
                    | op::DecorateId
                    | op::MemberDecorate
            );
            if ahead_ok {
                continue;
            }
            let mut used: Vec<u32> = i.ids().collect();
            used.extend(i.result_type);
            for id in used {
                if self.defs.get(&id).is_some_and(|&d| d >= k) {
                    self.problems
                        .push(format!("{}: %{id} is used before it is defined", i.name()));
                }
            }
        }
        for i in &self.insts {
            for (pos, id) in i.id_at.iter().map(|&p| (p, i.ops[p])) {
                if !self.defs.contains_key(&id) {
                    self.problems.push(format!(
                        "{}: operand {pos} names id {id}, which nothing defines",
                        i.name()
                    ));
                }
            }
            if let Some(t) = i.result_type
                && !self.defs.contains_key(&t)
            {
                self.problems
                    .push(format!("{}: its result type {t} is not defined", i.name()));
            }
        }
    }

    /// The order of the logical layout (2.4).
    fn sections(&mut self) {
        let mut last = 0;
        let mut in_function = false;
        for i in &self.insts {
            let rank = match i.opcode {
                op::Capability => 1,
                op::Extension => 2,
                op::ExtInstImport => 3,
                op::MemoryModel => 4,
                op::EntryPoint => 5,
                op::ExecutionMode | op::ExecutionModeId => 6,
                op::String | op::Name | op::MemberName => 7,
                op::Decorate | op::MemberDecorate | op::DecorateId => 8,
                op::Function => {
                    in_function = true;
                    10
                }
                op::FunctionEnd => {
                    in_function = false;
                    10
                }
                _ if in_function => 10,
                _ => 9,
            };
            if rank < last {
                self.problems.push(format!(
                    "{}: out of the order SPIR-V's layout has (2.4)",
                    i.name()
                ));
            }
            last = last.max(rank);
            let global_var = matches!(i.opcode, op::Variable | op::UntypedVariableKHR);
            if global_var && in_function && i.ops.get(2) != Some(&sc::Function) {
                self.problems.push(format!(
                    "{}: only `Function` variables are made inside functions",
                    i.name()
                ));
            }
            if global_var && !in_function && i.ops.get(2) == Some(&sc::Function) {
                self.problems.push(format!(
                    "{}: a `Function` variable outside a function",
                    i.name()
                ));
            }
        }
    }

    /// Capabilities, extensions, decorations and module variables.
    fn collect(&mut self) {
        for i in self.insts.clone() {
            match i.opcode {
                op::Capability => {
                    self.capabilities.insert(i.ops[0]);
                }
                op::Extension => {
                    self.extensions.insert(literal_string(&i.ops));
                }
                op::Decorate | op::DecorateId => {
                    self.decorations
                        .entry(i.ops[0])
                        .or_default()
                        .push((i.ops[1], i.ops[2..].to_vec()));
                }
                op::MemberDecorate => {
                    self.member_decorations
                        .entry((i.ops[0], i.ops[1]))
                        .or_default()
                        .push((i.ops[2], i.ops[3..].to_vec()));
                }
                op::Variable | op::UntypedVariableKHR if i.ops[2] != sc::Function => {
                    let (t, r, class) = (i.ops[0], i.ops[1], i.ops[2]);
                    match self.types.get(t).cloned() {
                        Some(Ty::Pointer { class: pc, .. } | Ty::Untyped { class: pc })
                            if pc == class => {}
                        _ => self.error(format!(
                            "{}: its type is not a pointer of its storage class",
                            i.name()
                        )),
                    }
                    if i.opcode == op::Variable
                        && i.ops.len() > 3
                        && !matches!(class, sc::Private | sc::Output)
                    {
                        self.error(format!(
                            "{}: only `Private`, `Function` and `Output` variables have initializers",
                            i.name()
                        ));
                    }
                    self.globals.insert(r, (class, t));
                }
                _ => {}
            }
        }
    }

    pub fn decorated(&self, id: u32, d: u32) -> Option<&[u32]> {
        self.decorations
            .get(&id)?
            .iter()
            .find(|(x, _)| *x == d)
            .map(|(_, e)| e.as_slice())
    }

    pub fn member_decorated(&self, s: u32, m: u32, d: u32) -> Option<&[u32]> {
        self.member_decorations
            .get(&(s, m))?
            .iter()
            .find(|(x, _)| *x == d)
            .map(|(_, e)| e.as_slice())
    }

    /// What is used needs its capability and extension.
    fn capability_rules(&mut self) {
        let mut need: Vec<(u32, &'static str, String)> = Vec::new();
        let mut ext: Vec<(&str, String)> = Vec::new();
        // The type of an id, and the format word of an image's type.
        let type_of = |id: u32| self.defs.get(&id).and_then(|&k| self.insts[k].result_type);
        let image_format = |id: u32| match type_of(id).and_then(|t| self.types.get(t)) {
            Some(Ty::Image(words)) => words.get(6).copied(),
            _ => None,
        };
        for i in &self.insts {
            let name = i.name();
            let mut req = |c: u32, why: &'static str| need.push((c, why, name.clone()));
            match i.opcode {
                op::TypeInt => match i.ops[1] {
                    16 => req(cap::Int16, "16-bit integers"),
                    64 => req(cap::Int64, "64-bit integers"),
                    _ => {}
                },
                op::TypeFloat => match i.ops[1] {
                    16 => req(cap::Float16, "16-bit floats"),
                    64 => req(cap::Float64, "64-bit floats"),
                    _ => {}
                },
                op::TypeUntypedPointerKHR
                | op::UntypedVariableKHR
                | op::UntypedAccessChainKHR
                | op::UntypedArrayLengthKHR => {
                    req(cap::UntypedPointersKHR, "untyped pointers");
                    ext.push(("SPV_KHR_untyped_pointers", name.clone()));
                }
                op::TypeBufferEXT | op::BufferPointerEXT | op::ConstantSizeOfEXT => {
                    req(cap::DescriptorHeapEXT, "descriptor heaps");
                    ext.push(("SPV_EXT_descriptor_heap", name.clone()));
                }
                // (result, sampled type, dim, depth, arrayed, ms, sampled, format)
                op::TypeImage => {
                    let (dim, arrayed, ms, sampled) = (i.ops[2], i.ops[4], i.ops[5], i.ops[6]);
                    if dim == 3 && arrayed == 1 && sampled == 1 {
                        req(cap::SampledCubeArray, "sampled cube arrays");
                    }
                    if ms == 1 && arrayed == 1 {
                        req(cap::ImageMSArray, "multisampled arrays");
                    }
                    if matches!(self.types.get(i.ops[1]), Some(Ty::Int { width: 64, .. })) {
                        req(cap::Int64ImageEXT, "64-bit integer images");
                        ext.push(("SPV_EXT_shader_image_int64", name.clone()));
                    }
                }
                op::TypeRayQueryKHR | op::TypeAccelerationStructureKHR => {
                    req(cap::RayQueryKHR, "ray queries");
                    ext.push(("SPV_KHR_ray_query", name.clone()));
                }
                op::ImageRead if image_format(i.ops[2]) == Some(0) => req(
                    cap::StorageImageReadWithoutFormat,
                    "reading storage images without a format",
                ),
                op::ImageWrite if image_format(i.ops[0]) == Some(0) => req(
                    cap::StorageImageWriteWithoutFormat,
                    "writing storage images without a format",
                ),
                op::UntypedImageTexelPointerEXT => {
                    req(
                        cap::DescriptorHeapEXT,
                        "texel pointers into descriptor heaps",
                    );
                    ext.push(("SPV_EXT_descriptor_heap", name.clone()));
                }
                op::AtomicLoad
                | op::AtomicStore
                | op::AtomicExchange
                | op::AtomicCompareExchange
                | op::AtomicIAdd
                | op::AtomicISub
                | op::AtomicSMin
                | op::AtomicUMin
                | op::AtomicSMax
                | op::AtomicUMax
                | op::AtomicAnd
                | op::AtomicOr
                | op::AtomicXor => {
                    // The value's type: the result's, or a store's value.
                    let ty = match i.result_type {
                        Some(t) => Some(t),
                        None => type_of(i.ops[3]),
                    };
                    if ty.is_some_and(|t| {
                        matches!(self.types.get(t), Some(Ty::Int { width: 64, .. }))
                    }) {
                        req(cap::Int64Atomics, "64-bit atomics");
                    }
                }
                op::DPdxFine
                | op::DPdyFine
                | op::FwidthFine
                | op::DPdxCoarse
                | op::DPdyCoarse
                | op::FwidthCoarse => req(cap::DerivativeControl, "fine and coarse derivatives"),
                op::DemoteToHelperInvocation | op::IsHelperInvocationEXT => req(
                    cap::DemoteToHelperInvocation,
                    "demoting to a helper invocation",
                ),
                op::ImageQuerySizeLod
                | op::ImageQuerySize
                | op::ImageQueryLevels
                | op::ImageQuerySamples => req(cap::ImageQuery, "image queries"),
                op::EntryPoint
                    if matches!(
                        i.ops[0],
                        crate::spirv::execution_model::MeshEXT
                            | crate::spirv::execution_model::TaskEXT
                    ) =>
                {
                    req(cap::MeshShadingEXT, "mesh and task shaders");
                    ext.push(("SPV_EXT_mesh_shader", name.clone()));
                }
                op::GroupNonUniformElect => req(cap::GroupNonUniform, "wave operations"),
                op::GroupNonUniformAll | op::GroupNonUniformAny | op::GroupNonUniformAllEqual => {
                    req(cap::GroupNonUniformVote, "wave votes")
                }
                op::GroupNonUniformBallot
                | op::GroupNonUniformBroadcast
                | op::GroupNonUniformBroadcastFirst => req(cap::GroupNonUniformBallot, "ballots"),
                op::GroupNonUniformShuffle => req(cap::GroupNonUniformShuffle, "wave shuffles"),
                op::GroupNonUniformQuadBroadcast | op::GroupNonUniformQuadSwap => {
                    req(cap::GroupNonUniformQuad, "quad operations")
                }
                o if (op::GroupNonUniformIAdd..=op::GroupNonUniformLogicalXor).contains(&o) => {
                    if i.ops.get(3) == Some(&crate::spirv::group_operation::ClusteredReduce) {
                        req(cap::GroupNonUniformClustered, "clustered reductions");
                    } else {
                        req(cap::GroupNonUniformArithmetic, "wave arithmetic");
                    }
                }
                _ => {}
            }
            if (op::GroupNonUniformElect..=op::GroupNonUniformQuadSwap).contains(&i.opcode) {
                need.push((cap::GroupNonUniform, "wave operations", name.clone()));
            }
        }
        let shader = self.capabilities.contains(&cap::Shader)
            || self.capabilities.iter().any(|c| IMPLY_SHADER.contains(c));
        if !shader {
            self.error("the `Shader` capability is not declared (Vulkan needs it)");
        }
        let mut told = HashSet::new();
        for (c, why, at) in need {
            let implied =
                c == cap::UntypedPointersKHR && self.capabilities.contains(&cap::DescriptorHeapEXT);
            if !self.capabilities.contains(&c) && !implied && told.insert(c) {
                let n = cap::name(c).unwrap_or("?");
                self.error(format!(
                    "{at}: {why} need the `{n}` capability, which is not declared"
                ));
            }
        }
        for (e, at) in ext {
            if !self.extensions.contains(e) && told.insert(u32::MAX - e.len() as u32) {
                self.error(format!(
                    "{at}: needs extension `{e}`, which is not declared"
                ));
            }
        }
    }

    fn memory_model(&mut self) {
        let models: Vec<Inst> = self
            .insts
            .iter()
            .filter(|i| i.opcode == op::MemoryModel)
            .cloned()
            .collect();
        match models.as_slice() {
            [m] => {
                if m.ops[0] != 0 {
                    self.error("Vulkan uses `Logical` addressing");
                }
                if m.ops[1] != 1 && m.ops[1] != 3 {
                    self.error("Vulkan's memory model is `GLSL450` or `Vulkan`");
                }
            }
            [] => self.error("the module has no `OpMemoryModel`"),
            _ => self.error("the module has more than one `OpMemoryModel`"),
        }
    }

    /// The module variables function `f` reaches (directly and through calls).
    pub fn reached_globals(&self, f: u32) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        let mut seen = HashSet::new();
        let mut stack = vec![f];
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            let Some(&start) = self.defs.get(&f) else {
                continue;
            };
            for i in self.insts[start..]
                .iter()
                .take_while(|i| i.opcode != op::FunctionEnd)
            {
                for id in i.ids() {
                    if self.globals.contains_key(&id) {
                        out.insert(id);
                    }
                }
                if i.opcode == op::FunctionCall {
                    stack.push(i.ops[2]);
                }
            }
        }
        out
    }

    fn entry_points(&mut self) {
        let entries: Vec<Inst> = self
            .insts
            .iter()
            .filter(|i| i.opcode == op::EntryPoint)
            .cloned()
            .collect();
        if entries.is_empty() {
            self.error("the module has no entry point");
        }
        let modes: Vec<Inst> = self
            .insts
            .iter()
            .filter(|i| i.opcode == op::ExecutionMode || i.opcode == op::ExecutionModeId)
            .cloned()
            .collect();
        for e in entries {
            let f = e.ops[1];
            let name = literal_string(&e.ops[2..]);
            let words = (name.len() + 1).div_ceil(4);
            let interface: Vec<u32> = e.ops[2 + words..].to_vec();
            if self.defs.get(&f).map(|&k| self.insts[k].opcode) != Some(op::Function) {
                self.error(format!("entry point `{name}`: %{f} is not a function"));
                continue;
            }
            // Implicit levels come from derivatives, which only fragments have (Vulkan; the
            // modules here hold one entry point).
            if e.ops[0] != crate::spirv::execution_model::Fragment
                && self.insts.iter().any(|i| {
                    matches!(
                        i.opcode,
                        op::ImageSampleImplicitLod | op::ImageSampleDrefImplicitLod
                    )
                })
            {
                self.error(format!(
                    "entry point `{name}`: an implicit-level sample outside a fragment entry point"
                ));
            }
            if e.ops[0] == crate::spirv::execution_model::Fragment
                && !modes.iter().any(|m| {
                    m.ops[0] == f && m.ops[1] == crate::spirv::execution_mode::OriginUpperLeft
                })
            {
                self.error(format!(
                    "entry point `{name}`: a fragment entry point needs `OriginUpperLeft` (Vulkan)"
                ));
            }
            let model = e.ops[0];
            use crate::spirv::execution_model as em;
            if matches!(model, em::GLCompute | em::MeshEXT | em::TaskEXT)
                && !modes
                    .iter()
                    .any(|m| m.ops[0] == f && m.ops[1] == crate::spirv::execution_mode::LocalSize)
            {
                self.error(format!(
                    "entry point `{name}`: a compute, mesh or task entry point needs `LocalSize`"
                ));
            }
            let listed: HashSet<u32> = interface.iter().copied().collect();
            if listed.len() != interface.len() {
                self.error(format!(
                    "entry point `{name}`: its interface names a variable twice"
                ));
            }
            for id in &interface {
                if !self.globals.contains_key(id) {
                    self.error(format!(
                        "entry point `{name}`: interface %{id} is not a module variable"
                    ));
                }
            }
            for g in self.reached_globals(f) {
                if !listed.contains(&g) {
                    self.error(format!(
                        "entry point `{name}`: it uses %{g}, which its interface does not list (SPIR-V 1.4)"
                    ));
                }
            }
        }
    }

    /// Explicit layout where the API lays memory out, and nowhere else (Vulkan).
    fn layout_rules(&mut self) {
        let mut laid = Vec::new();
        let mut plain = Vec::new();
        for (&id, ty) in self.types.iter() {
            if let Ty::Pointer { class, pointee } = ty {
                match *class {
                    sc::StorageBuffer | sc::Uniform | sc::PushConstant => {
                        laid.push((id, *class, *pointee))
                    }
                    sc::Function
                    | sc::Private
                    | sc::Workgroup
                    | sc::Input
                    | sc::Output
                    | sc::TaskPayloadWorkgroupEXT => plain.push((id, *class, *pointee)),
                    _ => {}
                }
            }
        }
        laid.sort();
        plain.sort();
        for (ptr, class, pointee) in laid {
            if !matches!(self.types.get(pointee), Some(Ty::Struct(_)))
                || self.decorated(pointee, decoration::Block).is_none()
            {
                // A pointer into a block, rather than to one, is fine (access chains make
                // those); only variables and buffer pointers need the block itself.
                let to_block = self.globals.values().any(|&(_, t)| t == ptr)
                    || self
                        .insts
                        .iter()
                        .any(|i| i.opcode == op::BufferPointerEXT && i.result_type == Some(ptr));
                if to_block {
                    let n = sc::name(class).unwrap_or("?");
                    self.error(format!("%{ptr}: memory of class `{n}` is a `Block` struct"));
                }
            }
            types::check_laid_out(self, pointee);
        }
        for (_, class, pointee) in plain {
            types::check_plain(self, pointee, class);
        }
    }

    fn builtins(&mut self) {
        for (&id, ds) in &self.decorations.clone() {
            for (d, extra) in ds {
                if *d != decoration::BuiltIn {
                    continue;
                }
                let b = extra.first().copied().unwrap_or(u32::MAX);
                let Some(&(class, ptr)) = self.globals.get(&id) else {
                    self.error(format!("%{id}: `BuiltIn` is on module variables here"));
                    continue;
                };
                let pointee = match self.types.get(ptr) {
                    Some(Ty::Pointer { pointee, .. }) => Some(*pointee),
                    _ => None,
                };
                // A mesh entry's outputs are arrays: one element per vertex or primitive.
                let mesh = self.insts.iter().any(|i| {
                    i.opcode == op::EntryPoint
                        && i.ops.first() == Some(&crate::spirv::execution_model::MeshEXT)
                });
                let pointee = match pointee.and_then(|t| self.types.get(t)) {
                    Some(Ty::Array { element, .. }) if mesh && class == sc::Output => {
                        Some(*element)
                    }
                    _ => pointee,
                };
                let uint = |v: &Self, t: Option<u32>, n: u32| match t.and_then(|t| v.types.get(t)) {
                    Some(Ty::Int { width: 32, .. }) => n == 1,
                    Some(Ty::Vector { component, count }) => {
                        *count == n
                            && matches!(v.types.get(*component), Some(Ty::Int { width: 32, .. }))
                    }
                    _ => false,
                };
                let float = |v: &Self, t: Option<u32>, n: u32| match t.and_then(|t| v.types.get(t))
                {
                    Some(Ty::Float { width: 32 }) => n == 1,
                    Some(Ty::Vector { component, count }) => {
                        *count == n
                            && matches!(v.types.get(*component), Some(Ty::Float { width: 32 }))
                    }
                    _ => false,
                };
                let ok = match b {
                    builtin::GlobalInvocationId
                    | builtin::LocalInvocationId
                    | builtin::WorkgroupId => class == sc::Input && uint(self, pointee, 3),
                    builtin::LocalInvocationIndex
                    | builtin::SubgroupSize
                    | builtin::SubgroupLocalInvocationId => {
                        class == sc::Input && uint(self, pointee, 1)
                    }
                    builtin::ResourceHeapEXT | builtin::SamplerHeapEXT => {
                        class == sc::UniformConstant
                            && matches!(self.types.get(ptr), Some(Ty::Untyped { .. }))
                    }
                    builtin::VertexIndex | builtin::InstanceIndex | builtin::BaseInstance => {
                        self.needs_capability(b == builtin::BaseInstance, cap::DrawParameters, id);
                        class == sc::Input && uint(self, pointee, 1)
                    }
                    builtin::SampleId => {
                        self.needs_capability(true, cap::SampleRateShading, id);
                        class == sc::Input && uint(self, pointee, 1)
                    }
                    builtin::Position => class == sc::Output && float(self, pointee, 4),
                    builtin::PrimitiveTriangleIndicesEXT => {
                        mesh && class == sc::Output && uint(self, pointee, 3)
                    }
                    builtin::PrimitiveLineIndicesEXT => {
                        mesh && class == sc::Output && uint(self, pointee, 2)
                    }
                    builtin::PrimitivePointIndicesEXT => {
                        mesh && class == sc::Output && uint(self, pointee, 1)
                    }
                    builtin::CullPrimitiveEXT => {
                        mesh && class == sc::Output
                            && matches!(pointee.and_then(|t| self.types.get(t)), Some(Ty::Bool))
                    }
                    builtin::PrimitiveId => {
                        if !self.capabilities.contains(&cap::Geometry)
                            && !self.capabilities.contains(&cap::MeshShadingEXT)
                        {
                            self.error(format!(
                                "%{id}: its built-in needs the `Geometry` or `MeshShadingEXT` \
                                 capability"
                            ));
                        }
                        uint(self, pointee, 1)
                    }
                    builtin::FragCoord => class == sc::Input && float(self, pointee, 4),
                    builtin::FragDepth => class == sc::Output && float(self, pointee, 1),
                    builtin::FrontFacing => {
                        class == sc::Input
                            && matches!(pointee.and_then(|t| self.types.get(t)), Some(Ty::Bool))
                    }
                    _ => {
                        self.error(format!(
                            "%{id}: built-in {b} is not one this validator checks"
                        ));
                        true
                    }
                };
                if !ok {
                    let n = builtin::name(b).unwrap_or("?");
                    self.error(format!(
                        "%{id}: `{n}` is not on a variable of the type it needs"
                    ));
                }
            }
        }
    }

    fn needs_capability(&mut self, when: bool, c: u32, id: u32) {
        if when && !self.capabilities.contains(&c) {
            let n = cap::name(c).unwrap_or("?");
            self.error(format!("%{id}: its built-in needs the `{n}` capability"));
        }
    }

    /// Vulkan's rules for stage I/O: every `Input`/`Output` variable that is not a built-in has
    /// a `Location`; a fragment shader's integer inputs are `Flat`.
    fn stage_io(&mut self) {
        let fragment = self.insts.iter().any(|i| {
            i.opcode == op::EntryPoint
                && i.ops.first() == Some(&crate::spirv::execution_model::Fragment)
        });
        let globals: Vec<(u32, u32, u32)> = self
            .globals
            .iter()
            .map(|(&id, &(class, ptr))| (id, class, ptr))
            .collect();
        for (id, class, ptr) in globals {
            if class != sc::Input && class != sc::Output {
                continue;
            }
            let has = |v: &Self, d: u32| v.decorated(id, d).is_some();
            let builtin = has(self, decoration::BuiltIn);
            if !builtin && !has(self, decoration::Location) {
                self.error(format!("%{id}: a stage input or output has no `Location`"));
            }
            let pointee = match self.types.get(ptr) {
                Some(Ty::Pointer { pointee, .. }) => Some(*pointee),
                _ => None,
            };
            let int = pointee
                .and_then(|t| self.types.component(t))
                .is_some_and(|(_, c)| matches!(c, Ty::Int { .. }));
            if fragment && class == sc::Input && int && !has(self, decoration::Flat) {
                self.error(format!(
                    "%{id}: an integer input of a fragment shader is not `Flat`"
                ));
            }
        }
    }
}

/// The result type, result and id positions of `ops` laid out as `layout`.
#[allow(clippy::type_complexity)]
fn decode(
    layout: &str,
    ops: &[u32],
    width: impl Fn(u32) -> usize,
) -> Result<(Option<u32>, Option<u32>, Vec<usize>), String> {
    let kinds: Vec<char> = layout.chars().collect();
    let (mut result_type, mut result, mut ids) = (None, None, Vec::new());
    let mut k = 0;
    let mut at = 0;
    while k < kinds.len() {
        let kind = kinds[k];
        let next = kinds.get(k + 1).copied();
        let repeat = next == Some('*');
        let optional = next == Some('?');
        let step = if repeat || optional { 2 } else { 1 };
        if at >= ops.len() {
            if repeat || optional || matches!(kind, 'M' | 'C' | 'L') {
                k += step;
                continue;
            }
            return Err("missing operands".into());
        }
        match kind {
            't' => result_type = Some(ops[at]),
            'r' => result = Some(ops[at]),
            'i' => ids.push(at),
            'l' => {}
            's' => {
                let words = string_words(&ops[at..]).ok_or("an unterminated string")?;
                at += words;
                k += step;
                if repeat {
                    continue;
                }
                continue;
            }
            'L' => {
                at = ops.len();
                k += 1;
                continue;
            }
            'M' => {
                // A mask, then a literal per `Aligned` (0x2).
                let mask = ops[at];
                at += 1 + usize::from(mask & 0x2 != 0);
                k += 1;
                continue;
            }
            'C' => {
                // (literal, label) pairs, the literal as wide as the selector.
                let w = width(ops[0]);
                while at < ops.len() {
                    at += w;
                    if at < ops.len() {
                        ids.push(at);
                    }
                    at += 1;
                }
                k += step;
                continue;
            }
            _ => return Err(format!("bad layout `{layout}`")),
        }
        at += 1;
        if !repeat {
            k += step;
        }
    }
    if at < ops.len() {
        return Err("more operands than it takes".into());
    }
    Ok((result_type, result, ids))
}

/// How many words a nul-terminated string takes.
fn string_words(words: &[u32]) -> Option<usize> {
    words
        .iter()
        .position(|w| w.to_le_bytes().contains(&0))
        .map(|p| p + 1)
}

pub(crate) fn literal_string(words: &[u32]) -> String {
    let mut bytes = Vec::new();
    for &w in words {
        let b = w.to_le_bytes();
        match b.iter().position(|&x| x == 0) {
            Some(end) => {
                bytes.extend_from_slice(&b[..end]);
                break;
            }
            None => bytes.extend_from_slice(&b),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The capabilities that declare `Shader` with them (directly or through others), from
/// `spirv.core.grammar.json`.
const IMPLY_SHADER: &[u32] = &[
    2, 3, 21, 23, 24, 25, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 40, 41, 42, 45, 48, 49, 50,
    51, 52, 53, 54, 55, 56, 57, 4422, 4427, 4428, 4429, 4430, 4439, 4441, 4442, 4445, 4471, 4472,
    4478, 4479, 4495, 5008, 5009, 5010, 5013, 5015, 5067, 5249, 5251, 5254, 5255, 5259, 5260, 5265,
    5266, 5283, 5288, 5291, 5301, 5302, 5303, 5306, 5307, 5308, 5309, 5310, 5311, 5312, 5336, 5340,
    5341, 5347, 5350, 5353, 5357, 5363, 5372, 5373, 5378, 5379, 5380, 5381, 5383, 5388, 5391, 5409,
    5423, 5437, 6032,
];
