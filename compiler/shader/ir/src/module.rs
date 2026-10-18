//! A module: interned types and constants, structs, module-level variables (entry I/O,
//! groupshared, constant data), functions and entry points (0006 §6.3, §12).

use marlin_lang_arena::{Arena, Idx, Interner};

use crate::func::{FuncId, Function};
use crate::ty::{ResKind, Scalar, Space, StructId, Type, TypeData};

pub type Const = Idx<ConstData>;
pub type GlobalId = Idx<GlobalData>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StructData {
    /// The full name, as the source wrote it or as monomorphization made it (`Game.Grid<4>`).
    pub name: Box<str>,
    pub fields: Vec<(Box<str>, Type)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConstValue {
    /// The bits, zero-extended from the type's width; `i1` is 0 or 1.
    Int(u64),
    /// The bits of the IEEE value at the type's width.
    Float(u64),
    /// All zero bits.
    Zero,
    /// Some value, fixed but unspecified (0006 §10).
    Any,
    /// The elements of a vector, matrix (column by column), array or struct.
    Composite(Vec<Const>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConstData {
    pub ty: Type,
    pub value: ConstValue,
}

words! {
    /// System values (0004 §11.2).
    Builtin {
        DispatchThreadId = "DispatchThreadId", GroupId = "GroupId",
        GroupThreadId = "GroupThreadId", GroupIndex = "GroupIndex", VertexId = "VertexId",
        InstanceId = "InstanceId", Position = "Position", ClipDistance = "ClipDistance",
        PrimitiveId = "PrimitiveId", CullPrimitive = "CullPrimitive",
        IsFrontFace = "IsFrontFace", SampleIndex = "SampleIndex", Depth = "Depth",
        Coverage = "Coverage",
    }
}

words! {
    Interp {
        Perspective = "perspective", NoPerspective = "no_perspective", Flat = "flat",
        Centroid = "centroid", Sample = "sample",
    }
}

/// How an entry I/O variable connects to the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binding {
    None,
    Builtin(Builtin),
    Location(u32),
    /// A render target.
    Target(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GlobalData {
    pub name: Box<str>,
    /// Always a pointer type: the variable's address.
    pub ty: Type,
    pub binding: Binding,
    pub interp: Option<Interp>,
    /// Initial contents, for `constant` data.
    pub init: Option<Const>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Compute([u32; 3]),
    Vertex,
    Pixel,
    Mesh([u32; 3]),
    Amplification([u32; 3]),
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Compute(_) => "compute",
            Stage::Vertex => "vertex",
            Stage::Pixel => "pixel",
            Stage::Mesh(_) => "mesh",
            Stage::Amplification(_) => "amplification",
        }
    }

    pub fn group_size(self) -> Option<[u32; 3]> {
        match self {
            Stage::Compute(s) | Stage::Mesh(s) | Stage::Amplification(s) => Some(s),
            Stage::Vertex | Stage::Pixel => None,
        }
    }
}

/// An entry point: a function without parameters or results, its stage, and the module-level
/// variables it uses.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub func: FuncId,
    pub stage: Stage,
    pub uses: Vec<GlobalId>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: Interner<TypeData>,
    pub consts: Interner<ConstData>,
    pub structs: Arena<StructData>,
    pub globals: Arena<GlobalData>,
    pub funcs: Arena<Function>,
    pub entries: Vec<Entry>,
    /// Format strings of `debug.print` and `debug.assert`, by index.
    pub formats: Vec<Box<str>>,
}

impl Module {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ty(&mut self, data: TypeData) -> Type {
        self.types.intern(&data)
    }

    pub fn scalar(&mut self, s: Scalar) -> Type {
        self.ty(TypeData::Scalar(s))
    }

    /// A vector, or the scalar itself for length 1.
    pub fn vector(&mut self, s: Scalar, n: u8) -> Type {
        if n == 1 {
            self.scalar(s)
        } else {
            self.ty(TypeData::Vector(s, n))
        }
    }

    pub fn ptr(&mut self, space: Space, readonly: bool, pointee: Type) -> Type {
        self.ty(TypeData::Ptr {
            space,
            readonly,
            pointee,
        })
    }

    pub fn res(&mut self, kind: ResKind) -> Type {
        self.ty(TypeData::Res(kind))
    }

    pub fn struct_ty(&mut self, id: StructId) -> Type {
        self.ty(TypeData::Struct(id))
    }

    pub fn data(&self, t: Type) -> &TypeData {
        &self.types[t]
    }

    /// The scalar of a scalar, vector or matrix type.
    pub fn element_scalar(&self, t: Type) -> Option<Scalar> {
        match self.data(t) {
            TypeData::Scalar(s) | TypeData::Vector(s, _) => Some(*s),
            TypeData::Matrix { element, .. } => Some(*element),
            _ => None,
        }
    }

    /// The number of components of a scalar (1) or vector.
    pub fn width(&self, t: Type) -> Option<u8> {
        match self.data(t) {
            TypeData::Scalar(_) => Some(1),
            TypeData::Vector(_, n) => Some(*n),
            _ => None,
        }
    }

    pub fn constant(&mut self, ty: Type, value: ConstValue) -> Const {
        self.consts.intern(&ConstData { ty, value })
    }

    /// An integer constant; `value` is truncated to the type's width.
    pub fn int(&mut self, ty: Type, value: i64) -> Const {
        let bits = match self.element_scalar(ty) {
            Some(s) if s.bits() < 64 => (value as u64) & ((1u64 << s.bits()) - 1),
            _ => value as u64,
        };
        self.constant(ty, ConstValue::Int(bits))
    }

    pub fn bool(&mut self, value: bool) -> Const {
        let ty = self.scalar(Scalar::I1);
        self.constant(ty, ConstValue::Int(value as u64))
    }

    /// A float constant of an `f16`, `f32` or `f64` type, rounded to it.
    pub fn float(&mut self, ty: Type, value: f64) -> Const {
        let bits = match self.element_scalar(ty) {
            Some(Scalar::F16) => u64::from(crate::num::f64_to_f16(value)),
            Some(Scalar::F32) => u64::from((value as f32).to_bits()),
            _ => value.to_bits(),
        };
        self.constant(ty, ConstValue::Float(bits))
    }

    pub fn add_struct(&mut self, name: &str, fields: Vec<(Box<str>, Type)>) -> StructId {
        self.structs.alloc(StructData {
            name: name.into(),
            fields,
        })
    }

    pub fn add_global(&mut self, global: GlobalData) -> GlobalId {
        self.globals.alloc(global)
    }

    pub fn add_function(&mut self, func: Function) -> FuncId {
        self.funcs.alloc(func)
    }

    pub fn find_function(&self, name: &str) -> Option<FuncId> {
        self.funcs
            .iter()
            .find(|(_, f)| &*f.name == name)
            .map(|(id, _)| id)
    }

    pub fn entry_of(&self, func: FuncId) -> Option<&Entry> {
        self.entries.iter().find(|e| e.func == func)
    }

    /// Keeps only the functions and module variables `keep_func` and `keep_global` accept; ids
    /// change, and calls, entries and the functions' references follow. Nothing kept may call a
    /// removed function or use a removed variable.
    pub fn retain(
        &mut self,
        keep_func: impl Fn(FuncId) -> bool,
        keep_global: impl Fn(GlobalId) -> bool,
    ) {
        let mut globals = Arena::new();
        let mut global_map = std::collections::HashMap::new();
        for (id, g) in std::mem::take(&mut self.globals).iter() {
            if keep_global(id) {
                global_map.insert(id, globals.alloc(g.clone()));
            }
        }
        self.globals = globals;
        let mut funcs = Arena::new();
        let mut func_map = std::collections::HashMap::new();
        for (id, f) in std::mem::take(&mut self.funcs).iter() {
            if keep_func(id) {
                func_map.insert(id, funcs.alloc(f.clone()));
            }
        }
        self.funcs = funcs;
        for (_, f) in self.funcs.iter_mut() {
            f.remap(&func_map, &global_map);
        }
        self.entries.retain(|e| func_map.contains_key(&e.func));
        for e in &mut self.entries {
            e.func = func_map[&e.func];
            e.uses = e
                .uses
                .iter()
                .filter_map(|g| global_map.get(g).copied())
                .collect();
        }
    }
}
