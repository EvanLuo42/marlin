//! Types as the author writes them: `float3` rather than `Vector<float, 3>`.

use marlin_front_hir_def::{ItemTreeQuery, item_path};
use marlin_lang_query::{Cancelled, Cx};

use crate::builtins::{BuiltinKind, Builtins};
use crate::ty::{ConstOp, ConstTy, GenericArgTy, Ty};

pub fn display_ty(cx: &Cx<'_>, builtins: &Builtins, ty: &Ty) -> Result<String, Cancelled> {
    Ok(match ty {
        Ty::Error => "{error}".into(),
        Ty::Void => "void".into(),
        Ty::Adt { item, args } => {
            let scalar_of = |t: &GenericArgTy| match t {
                GenericArgTy::Type(Ty::Adt { item, .. }) => builtins.scalar(*item),
                _ => None,
            };
            let small = |c: &GenericArgTy| match c {
                GenericArgTy::Const(ConstTy::Value(n)) if (2..=4).contains(n) => Some(*n),
                _ => None,
            };
            match builtins.kind(*item) {
                Some(BuiltinKind::Scalar(s)) => return Ok(s.keyword().into()),
                Some(BuiltinKind::String) => return Ok("string".into()),
                Some(BuiltinKind::Vector) => {
                    if let [elem, n] = &args[..]
                        && let (Some(s), Some(n)) = (scalar_of(elem), small(n))
                    {
                        return Ok(format!("{}{n}", s.keyword()));
                    }
                }
                Some(BuiltinKind::Matrix) => {
                    if let [elem, r, c] = &args[..]
                        && let (Some(s), Some(r), Some(c)) = (scalar_of(elem), small(r), small(c))
                    {
                        return Ok(format!("{}{r}x{c}", s.keyword()));
                    }
                }
                _ => {}
            }
            let tree = cx.get::<ItemTreeQuery>(&item.file)?;
            let name = tree.items[item.local]
                .name
                .as_ref()
                .map_or_else(|| "<anonymous>".to_string(), ToString::to_string);
            if args.is_empty() {
                name
            } else {
                let mut shown = Vec::new();
                for a in args.iter() {
                    shown.push(match a {
                        GenericArgTy::Type(t) => display_ty(cx, builtins, t)?,
                        GenericArgTy::Const(c) => display_const(cx, c)?,
                    });
                }
                format!("{name}<{}>", shown.join(", "))
            }
        }
        Ty::Array { element, len } => {
            format!(
                "{}[{}]",
                display_ty(cx, builtins, element)?,
                display_const(cx, len)?
            )
        }
        Ty::Tuple(items) => {
            let mut shown = Vec::new();
            for t in items.iter() {
                shown.push(display_ty(cx, builtins, t)?);
            }
            format!("({})", shown.join(", "))
        }
        Ty::Param { owner, index } => param_name(cx, *owner, *index)?,
        Ty::Assoc { base, name } => format!("{}.{name}", display_ty(cx, builtins, base)?),
    })
}

fn param_name(
    cx: &Cx<'_>,
    owner: marlin_front_hir_def::ItemId,
    index: u32,
) -> Result<String, Cancelled> {
    let tree = cx.get::<ItemTreeQuery>(&owner.file)?;
    Ok(tree.items[owner.local]
        .generic_params
        .get(index as usize)
        .map_or_else(|| format!("<param {index}>"), |p| p.name.to_string()))
}

pub fn display_const(cx: &Cx<'_>, c: &ConstTy) -> Result<String, Cancelled> {
    Ok(match c {
        ConstTy::Error => "{error}".into(),
        ConstTy::Value(v) => v.to_string(),
        ConstTy::Param { owner, index } => param_name(cx, *owner, *index)?,
        ConstTy::Item(item) => item_path(cx, *item)?,
        ConstTy::Expr(e) => {
            let lhs = display_const(cx, &e.lhs)?;
            match &e.rhs {
                None => format!("-{lhs}"),
                Some(rhs) => {
                    let op = match e.op {
                        ConstOp::Add => "+",
                        ConstOp::Sub => "-",
                        ConstOp::Mul => "*",
                        ConstOp::Div => "/",
                        ConstOp::Rem => "%",
                        ConstOp::Shl => "<<",
                        ConstOp::Shr => ">>",
                        ConstOp::Neg => "-",
                    };
                    format!("({lhs} {op} {})", display_const(cx, rhs)?)
                }
            }
        }
    })
}
