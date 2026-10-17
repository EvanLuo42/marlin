//! Memory layout (0004 §16): one layout everywhere, scalar layout. Every value is aligned to its
//! largest scalar, fields follow in declaration order, and a struct's size is rounded up to
//! its alignment. Resources are 4-byte indices (§10); an enum is its underlying type.

use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, Name};
use marlin_lang_query::{Cancelled, Cx};

use crate::builtins::{BuiltinKind, Builtins, Scalar};
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{GenericArgTy, Ty};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

/// A field of a struct or tuple, at its offset.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldLayout {
    pub name: Name,
    pub ty: Ty,
    pub offset: u64,
    pub layout: Layout,
}

/// Deep enough for any real struct; a struct that contains itself has no layout.
const MAX_DEPTH: u32 = 64;

pub fn scalar_size(s: Scalar) -> u64 {
    match s {
        Scalar::I16 | Scalar::U16 | Scalar::F16 => 2,
        Scalar::Bool | Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
        Scalar::I64 | Scalar::U64 | Scalar::F64 => 8,
    }
}

fn round_up(x: u64, align: u64) -> u64 {
    x.div_ceil(align.max(1)) * align.max(1)
}

/// The layout of `ty`; `None` when it has none or it is not known yet (type parameters,
/// lengths given by `const` fields, `Func`).
pub fn layout_of(cx: &Cx<'_>, builtins: &Builtins, ty: &Ty) -> Result<Option<Layout>, Cancelled> {
    layout(cx, builtins, ty, 0)
}

fn layout(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    depth: u32,
) -> Result<Option<Layout>, Cancelled> {
    if depth > MAX_DEPTH {
        return Ok(None);
    }
    Ok(match ty {
        Ty::Adt { item, args } => match builtins.kind(*item) {
            Some(BuiltinKind::Scalar(s)) => Some(Layout {
                size: scalar_size(s),
                align: scalar_size(s),
            }),
            Some(BuiltinKind::Vector) => match &args[..] {
                [GenericArgTy::Type(t), GenericArgTy::Const(n)] => {
                    match (layout(cx, builtins, t, depth + 1)?, n.value()) {
                        (Some(e), Some(n)) if n > 0 => Some(Layout {
                            size: e.size * n as u64,
                            align: e.align,
                        }),
                        _ => None,
                    }
                }
                _ => None,
            },
            Some(BuiltinKind::Matrix) => match &args[..] {
                [
                    GenericArgTy::Type(t),
                    GenericArgTy::Const(r),
                    GenericArgTy::Const(c),
                ] => match (layout(cx, builtins, t, depth + 1)?, r.value(), c.value()) {
                    (Some(e), Some(r), Some(c)) if r > 0 && c > 0 => Some(Layout {
                        size: e.size * (r * c) as u64,
                        align: e.align,
                    }),
                    _ => None,
                },
                _ => None,
            },
            Some(BuiltinKind::Resource(_)) => Some(Layout { size: 4, align: 4 }),
            Some(_) => None,
            None => {
                let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                match tree.items[item.local].kind {
                    ItemKind::Enum => {
                        let types = cx.get::<ItemTypesQuery>(item)?;
                        match types.bases.first() {
                            Some(t) => layout(cx, builtins, t, depth + 1)?,
                            None => Some(Layout { size: 4, align: 4 }),
                        }
                    }
                    ItemKind::Struct => {
                        let fields = struct_fields(cx, builtins, *item, args, depth)?;
                        fields.map(|f| aggregate(&f))
                    }
                    _ => None,
                }
            }
        },
        Ty::Array { element, len } => {
            match (layout(cx, builtins, element, depth + 1)?, len.value()) {
                (Some(e), Some(n)) if n > 0 => Some(Layout {
                    size: round_up(e.size, e.align) * n as u64,
                    align: e.align,
                }),
                _ => None,
            }
        }
        Ty::Tuple(items) => {
            let mut fields = Vec::new();
            let mut offset = 0;
            for (i, t) in items.iter().enumerate() {
                let Some(l) = layout(cx, builtins, t, depth + 1)? else {
                    return Ok(None);
                };
                offset = round_up(offset, l.align);
                fields.push(FieldLayout {
                    name: Name::new(&format!("Item{}", i + 1)),
                    ty: t.clone(),
                    offset,
                    layout: l,
                });
                offset += l.size;
            }
            Some(aggregate(&fields))
        }
        Ty::Error | Ty::Void | Ty::Param { .. } | Ty::Assoc { .. } => None,
    })
}

fn aggregate(fields: &[FieldLayout]) -> Layout {
    let align = fields.iter().map(|f| f.layout.align).max().unwrap_or(1);
    let end = fields
        .iter()
        .map(|f| f.offset + f.layout.size)
        .max()
        .unwrap_or(0);
    Layout {
        size: round_up(end, align),
        align,
    }
}

/// The instance fields of struct `item` with type arguments `args`, laid out; `None` when one
/// of them has no layout.
pub fn fields_of(
    cx: &Cx<'_>,
    builtins: &Builtins,
    item: ItemId,
    args: &[GenericArgTy],
) -> Result<Option<Vec<FieldLayout>>, Cancelled> {
    struct_fields(cx, builtins, item, args, 0)
}

fn struct_fields(
    cx: &Cx<'_>,
    builtins: &Builtins,
    item: ItemId,
    args: &[GenericArgTy],
    depth: u32,
) -> Result<Option<Vec<FieldLayout>>, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
    let subst = Subst::new().with_args(item, args);
    let mut out = Vec::new();
    let mut offset = 0;
    for &m in &tree.items[item.local].members {
        let data = &tree.items[m];
        if data.kind != ItemKind::Field || data.is_static || data.is_const {
            continue;
        }
        let field = ItemId {
            file: item.file,
            local: m,
        };
        let types = cx.get::<ItemTypesQuery>(&field)?;
        let ty = subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
        let Some(l) = layout(cx, builtins, &ty, depth + 1)? else {
            return Ok(None);
        };
        offset = round_up(offset, l.align);
        out.push(FieldLayout {
            name: data.name.clone().unwrap_or_else(|| Name::new("_")),
            ty,
            offset,
            layout: l,
        });
        offset += l.size;
    }
    Ok(Some(out))
}
