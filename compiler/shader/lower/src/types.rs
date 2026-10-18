//! Front-end types to IR types (0006 §3). Integers lose their sign here; the lowering of each
//! operation reads it from the front-end type ([`Num`]).

use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, item_path};
use marlin_shaderc_hir_ty::{
    BuiltinKind, ConstTy, GenericArgTy, ItemTypesQuery, Resource, Scalar, Subst, Ty, display_ty,
};
use marlin_shaderc_ir::{ResKind, ResourceKind, Scalar as IrScalar, StructId, Type, TypeData};

use crate::{LowerError, Lowering, R};

/// How a scalar's bits are read: what picks `sdiv` or `udiv`, `slt` or `ult`, `sext` or `zext`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Num {
    Bool,
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

impl Num {
    pub(crate) fn of(s: Scalar) -> Num {
        match s {
            Scalar::Bool => Num::Bool,
            Scalar::I16 => Num::Signed(16),
            Scalar::U16 => Num::Unsigned(16),
            Scalar::I32 => Num::Signed(32),
            Scalar::U32 => Num::Unsigned(32),
            Scalar::I64 => Num::Signed(64),
            Scalar::U64 => Num::Unsigned(64),
            Scalar::F16 => Num::Float(16),
            Scalar::F32 => Num::Float(32),
            Scalar::F64 => Num::Float(64),
        }
    }

    pub(crate) fn ir(self) -> IrScalar {
        match self {
            Num::Bool => IrScalar::I1,
            Num::Signed(16) | Num::Unsigned(16) => IrScalar::I16,
            Num::Signed(64) | Num::Unsigned(64) => IrScalar::I64,
            Num::Signed(_) | Num::Unsigned(_) => IrScalar::I32,
            Num::Float(16) => IrScalar::F16,
            Num::Float(64) => IrScalar::F64,
            Num::Float(_) => IrScalar::F32,
        }
    }

    pub(crate) fn is_float(self) -> bool {
        matches!(self, Num::Float(_))
    }

    pub(crate) fn is_signed(self) -> bool {
        matches!(self, Num::Signed(_))
    }

    pub(crate) fn bits(self) -> u32 {
        match self {
            Num::Bool => 1,
            Num::Signed(b) | Num::Unsigned(b) | Num::Float(b) => b,
        }
    }
}

/// A value's shape for element-wise lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Scalar,
    Vector(u8),
    Matrix(u8, u8),
}

impl Lowering<'_, '_> {
    pub(crate) fn show(&self, ty: &Ty) -> String {
        display_ty(self.cx, &self.builtins, ty).unwrap_or_else(|_| "?".into())
    }

    fn const_value_of(&mut self, c: &ConstTy) -> R<u64> {
        match c {
            ConstTy::Value(v) if *v >= 0 => Ok(*v as u64),
            ConstTy::Item(item) => {
                let v = self.const_int(*item)?;
                u64::try_from(v)
                    .map_err(|_| LowerError::new("E1302", "a length or count is negative"))
            }
            other => Err(LowerError::new(
                "E1302",
                format!("a constant that is not known here: {other:?}"),
            )),
        }
    }

    /// The element type, numeric kind and shape of a scalar, vector or matrix type.
    pub(crate) fn num_shape(&mut self, ty: &Ty) -> R<Option<(Num, Shape)>> {
        let Ty::Adt { item, args } = ty else {
            return Ok(None);
        };
        Ok(match self.builtins.kind(*item) {
            Some(BuiltinKind::Scalar(s)) => Some((Num::of(s), Shape::Scalar)),
            Some(BuiltinKind::Vector) => match &args[..] {
                [GenericArgTy::Type(e), GenericArgTy::Const(n)] => {
                    let Some((num, Shape::Scalar)) = self.num_shape(e)? else {
                        return Ok(None);
                    };
                    let n = self.const_value_of(n)?;
                    Some((
                        num,
                        if n == 1 {
                            Shape::Scalar
                        } else {
                            Shape::Vector(n as u8)
                        },
                    ))
                }
                _ => None,
            },
            Some(BuiltinKind::Matrix) => match &args[..] {
                [
                    GenericArgTy::Type(e),
                    GenericArgTy::Const(r),
                    GenericArgTy::Const(c),
                ] => {
                    let Some((num, Shape::Scalar)) = self.num_shape(e)? else {
                        return Ok(None);
                    };
                    let (r, c) = (self.const_value_of(r)?, self.const_value_of(c)?);
                    Some((num, Shape::Matrix(r as u8, c as u8)))
                }
                _ => None,
            },
            _ => {
                // An enum reads as its underlying type.
                let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
                if tree.items[item.local].kind == ItemKind::Enum {
                    let under = self.enum_underlying(*item)?;
                    return self.num_shape(&under);
                }
                None
            }
        })
    }

    pub(crate) fn num(&mut self, ty: &Ty) -> R<Option<Num>> {
        Ok(self.num_shape(ty)?.map(|(n, _)| n))
    }

    pub(crate) fn enum_underlying(&mut self, item: ItemId) -> R<Ty> {
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        Ok(match types.bases.first() {
            Some(t) => t.clone(),
            None => self
                .builtins
                .item(BuiltinKind::Scalar(Scalar::I32))
                .map_or(Ty::Error, Ty::adt),
        })
    }

    /// The IR type of a concrete front-end type.
    pub(crate) fn ty(&mut self, ty: &Ty) -> R<Type> {
        if let Some(&t) = self.types.get(ty) {
            return Ok(t);
        }
        let t = self.ty_uncached(ty)?;
        self.types.insert(ty.clone(), t);
        Ok(t)
    }

    fn ty_uncached(&mut self, ty: &Ty) -> R<Type> {
        if let Some((num, shape)) = self.num_shape(ty)? {
            let s = num.ir();
            return Ok(match shape {
                Shape::Scalar => self.module.scalar(s),
                Shape::Vector(n) => self.module.vector(s, n),
                Shape::Matrix(rows, cols) => self.module.ty(TypeData::Matrix {
                    element: s,
                    rows,
                    cols,
                }),
            });
        }
        match ty {
            Ty::Adt { item, args } => {
                if let Some(BuiltinKind::Resource(r)) = self.builtins.kind(*item) {
                    let kind = resource_kind(r);
                    let element = match (kind.has_element(), args.first()) {
                        (true, Some(GenericArgTy::Type(e))) => Some(self.ty(e)?),
                        (true, _) => {
                            return Err(LowerError::new(
                                "E1303",
                                "a resource without its element type",
                            ));
                        }
                        (false, _) => None,
                    };
                    // Image types carry the sign of integer texels, which the IR's signless
                    // integers do not: the texture type says it.
                    let signed = kind.texture().is_some()
                        && matches!(args.first(), Some(GenericArgTy::Type(e))
                            if matches!(self.num(e)?, Some(Num::Signed(_))));
                    return Ok(self.module.res(ResKind {
                        kind,
                        element,
                        signed,
                    }));
                }
                // A mesh entry's outputs are module variables; these values name them.
                if matches!(
                    self.builtins.kind(*item),
                    Some(BuiltinKind::MeshOutput | BuiltinKind::MeshElements)
                ) {
                    return Ok(self.module.scalar(marlin_shaderc_ir::Scalar::I32));
                }
                // A ray query is the address of its own variable (0006 §8.9).
                if self.builtins.kind(*item) == Some(BuiltinKind::RayQuery) {
                    let rq = self.module.ty(TypeData::RayQuery);
                    return Ok(self
                        .module
                        .ptr(marlin_shaderc_ir::Space::Function, false, rq));
                }
                if matches!(
                    self.builtins.kind(*item),
                    Some(BuiltinKind::Func | BuiltinKind::Action)
                ) {
                    return Err(LowerError::new(
                        "E1301",
                        "`Func` and `Action` parameters are not lowered yet",
                    ));
                }
                if self.builtins.kind(*item).is_some() {
                    return Err(LowerError::new(
                        "E1303",
                        format!("`{}` has no run-time representation", self.show(ty)),
                    ));
                }
                let (id, _) = self.struct_of(ty)?;
                Ok(self.module.struct_ty(id))
            }
            Ty::Array { element, len } => {
                let e = self.ty(element)?;
                let n = self.const_value_of(len)?;
                let n =
                    u32::try_from(n).map_err(|_| LowerError::new("E1302", "an array too long"))?;
                Ok(self.module.ty(TypeData::Array(e, n)))
            }
            Ty::Tuple(items) => {
                let mut fields = Vec::new();
                for (i, t) in items.iter().enumerate() {
                    fields.push((format!("Item{}", i + 1).into(), self.ty(t)?));
                }
                let name = self.show(ty);
                let id = self.module.add_struct(&name, fields);
                Ok(self.module.struct_ty(id))
            }
            Ty::Void => Err(LowerError::new("E1303", "`void` is not a value")),
            Ty::Error | Ty::Param { .. } | Ty::Assoc { .. } => Err(LowerError::new(
                "E1303",
                format!("`{}` is not a concrete type", self.show(ty)),
            )),
        }
    }

    /// The IR struct of a struct type, and its instance fields in IR order.
    pub(crate) fn struct_of(&mut self, ty: &Ty) -> R<(StructId, Vec<ItemId>)> {
        if let Some(s) = self.structs.get(ty) {
            return Ok(s.clone());
        }
        let Ty::Adt { item, args } = ty else {
            return Err(LowerError::new("E1303", "not a struct"));
        };
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        if tree.items[item.local].kind != ItemKind::Struct {
            return Err(LowerError::new(
                "E1303",
                format!("`{}` has no run-time representation", self.show(ty)),
            ));
        }
        let field_items: Vec<ItemId> = tree.items[item.local]
            .members
            .iter()
            .filter(|&&m| {
                let d = &tree.items[m];
                d.kind == ItemKind::Field && !d.is_static && !d.is_const
            })
            .map(|&m| ItemId {
                file: item.file,
                local: m,
            })
            .collect();
        // The name: the full path, with type arguments for instances of generic structs.
        let name = if args.is_empty() {
            item_path(self.cx, *item)?.to_string()
        } else {
            let mut shown = item_path(self.cx, *item)?.to_string();
            let parts: Vec<String> = args
                .iter()
                .map(|a| match a {
                    GenericArgTy::Type(t) => self.show(t),
                    GenericArgTy::Const(c) => marlin_shaderc_hir_ty::display_const(self.cx, c)
                        .unwrap_or_else(|_| "?".into()),
                })
                .collect();
            shown.push_str(&format!("<{}>", parts.join(", ")));
            shown
        };
        // Declare first, so fields may refer back through arrays of other structs.
        let id = self.module.add_struct(&name, Vec::new());
        self.structs.insert(ty.clone(), (id, field_items.clone()));
        let subst = Subst::new().with_args(*item, args);
        let mut fields = Vec::new();
        for f in &field_items {
            let types = self.cx.get::<ItemTypesQuery>(f)?;
            let fty = subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
            let name = tree.items[f.local]
                .name
                .as_ref()
                .map_or_else(|| "_".to_string(), |n| n.to_string());
            fields.push((name.into(), self.ty(&fty)?));
        }
        self.module.structs[id].fields = fields;
        Ok((id, field_items))
    }

    /// The IR field index of field `field` of struct type `ty`.
    pub(crate) fn field_index(&mut self, ty: &Ty, field: ItemId) -> R<u32> {
        let (_, fields) = self.struct_of(ty)?;
        fields
            .iter()
            .position(|f| *f == field)
            .map(|i| i as u32)
            .ok_or_else(|| LowerError::new("E1303", "not an instance field of this struct"))
    }
}

fn resource_kind(r: Resource) -> ResourceKind {
    match r {
        Resource::Uniform => ResourceKind::Uniform,
        Resource::Buffer => ResourceKind::Buffer,
        Resource::ReadOnlyBuffer => ResourceKind::ReadOnlyBuffer,
        Resource::ByteBuffer => ResourceKind::ByteBuffer,
        Resource::ReadOnlyByteBuffer => ResourceKind::ReadOnlyByteBuffer,
        Resource::Texture2D => ResourceKind::Texture2D,
        Resource::Texture2DArray => ResourceKind::Texture2DArray,
        Resource::Texture3D => ResourceKind::Texture3D,
        Resource::TextureCube => ResourceKind::TextureCube,
        Resource::TextureCubeArray => ResourceKind::TextureCubeArray,
        Resource::Texture2DMS => ResourceKind::Texture2DMS,
        Resource::Texture2DMSArray => ResourceKind::Texture2DMSArray,
        Resource::StorageTexture3D => ResourceKind::StorageTexture3D,
        Resource::StorageTexture2D => ResourceKind::StorageTexture2D,
        Resource::StorageTexture2DArray => ResourceKind::StorageTexture2DArray,
        Resource::Sampler => ResourceKind::Sampler,
        Resource::ComparisonSampler => ResourceKind::ComparisonSampler,
        Resource::AccelerationStructure => ResourceKind::AccelerationStructure,
    }
}

impl Lowering<'_, '_> {
    /// `ty` with every associated type of a concrete type replaced by what the type says it is
    /// (`M.Params` with `M = Rock` becomes `Rock.Params`' target).
    pub(crate) fn resolve_assoc(&self, ty: &Ty) -> Ty {
        marlin_shaderc_hir_ty::normalize_assoc(self.cx, ty).unwrap_or_else(|_| ty.clone())
    }
}
