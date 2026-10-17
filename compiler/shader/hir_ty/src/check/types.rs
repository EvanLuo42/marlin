//! Where a type may be used, wherever it is written (signatures and locals):
//! - interfaces only as constraints (0004 §8.5);
//! - `Func` and `Action` only as the type of a parameter (§9.2);
//! - no `bool` in what a buffer, `Uniform` or push constants hold (§3.1, §16);
//! - `Uniform<T>`: no field of `T` crosses a 16-byte row (§16).

use marlin_front_hir_def::{ItemKind, ItemTreeQuery};
use marlin_lang_query::{Cancelled, Cx};

use crate::builtins::{BuiltinKind, Builtins, Resource, Scalar};
use crate::layout::{fields_of, layout_of};
use crate::ty::{GenericArgTy, Ty};

pub(crate) struct Problem {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

/// What is wrong with writing `ty` here; `is_param` for the type of a parameter.
pub(crate) fn type_problems(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    is_param: bool,
) -> Result<Vec<Problem>, Cancelled> {
    let mut out = Vec::new();
    walk(cx, builtins, ty, is_param, &mut out)?;
    Ok(out)
}

fn show(cx: &Cx<'_>, builtins: &Builtins, ty: &Ty) -> Result<String, Cancelled> {
    crate::display_ty(cx, builtins, ty)
}

fn walk(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    top_param: bool,
    out: &mut Vec<Problem>,
) -> Result<(), Cancelled> {
    match ty {
        Ty::Adt { item, args } => {
            match builtins.kind(*item) {
                Some(BuiltinKind::Func | BuiltinKind::Action) => {
                    if !top_param {
                        out.push(Problem {
                            code: "E1202",
                            message: format!(
                                "`{}` can only be the type of a parameter, not of a field, \
                                 local or result",
                                show(cx, builtins, ty)?
                            ),
                        });
                    }
                    // What a lambda parameter takes and gives is checked where it is written.
                    return Ok(());
                }
                Some(BuiltinKind::Resource(
                    r @ (Resource::Uniform | Resource::Buffer | Resource::ReadOnlyBuffer),
                )) => {
                    if let Some(GenericArgTy::Type(element)) = args.first() {
                        if holds_bool(cx, builtins, element, 0)? {
                            out.push(Problem {
                                code: "E1203",
                                message: format!(
                                    "`{}` holds a `bool`, which cannot be stored in a buffer; \
                                     use `uint`",
                                    show(cx, builtins, ty)?
                                ),
                            });
                        } else if r == Resource::Uniform
                            && let Some(problem) = uniform_rows(cx, builtins, element)?
                        {
                            out.push(problem);
                        }
                    }
                }
                Some(_) => {}
                None => {
                    let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                    if tree.items[item.local].kind == ItemKind::Interface {
                        out.push(Problem {
                            code: "E1201",
                            message: format!(
                                "`{}` is an interface; interfaces can only be constraints \
                                 (`where T : {0}`)",
                                show(cx, builtins, ty)?
                            ),
                        });
                        return Ok(());
                    }
                }
            }
            for arg in args.iter() {
                if let GenericArgTy::Type(t) = arg {
                    walk(cx, builtins, t, false, out)?;
                }
            }
        }
        Ty::Array { element, .. } => walk(cx, builtins, element, false, out)?,
        Ty::Tuple(items) => {
            for t in items.iter() {
                walk(cx, builtins, t, false, out)?;
            }
        }
        Ty::Error | Ty::Void | Ty::Param { .. } | Ty::Assoc { .. } => {}
    }
    Ok(())
}

/// Whether a value of `ty` contains a `bool` somewhere.
pub(crate) fn holds_bool(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    depth: u32,
) -> Result<bool, Cancelled> {
    if depth > 64 {
        return Ok(false);
    }
    Ok(match ty {
        Ty::Adt { item, args } => match builtins.kind(*item) {
            Some(BuiltinKind::Scalar(s)) => s == Scalar::Bool,
            Some(BuiltinKind::Vector | BuiltinKind::Matrix) => match args.first() {
                Some(GenericArgTy::Type(t)) => holds_bool(cx, builtins, t, depth + 1)?,
                _ => false,
            },
            Some(_) => false,
            None => {
                let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                if tree.items[item.local].kind != ItemKind::Struct {
                    return Ok(false);
                }
                let subst = crate::Subst::new().with_args(*item, args);
                for &m in &tree.items[item.local].members {
                    let data = &tree.items[m];
                    if data.kind != ItemKind::Field || data.is_static || data.is_const {
                        continue;
                    }
                    let field = marlin_front_hir_def::ItemId {
                        file: item.file,
                        local: m,
                    };
                    let types = cx.get::<crate::ItemTypesQuery>(&field)?;
                    let t = subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                    if holds_bool(cx, builtins, &t, depth + 1)? {
                        return Ok(true);
                    }
                }
                false
            }
        },
        Ty::Array { element, .. } => holds_bool(cx, builtins, element, depth + 1)?,
        Ty::Tuple(items) => {
            for t in items.iter() {
                if holds_bool(cx, builtins, t, depth + 1)? {
                    return Ok(true);
                }
            }
            false
        }
        _ => false,
    })
}

/// The first part of `ty` that crosses a 16-byte row, as a problem.
fn uniform_rows(cx: &Cx<'_>, builtins: &Builtins, ty: &Ty) -> Result<Option<Problem>, Cancelled> {
    let mut found = None;
    rows(cx, builtins, ty, 0, &mut String::new(), &mut found, 0)?;
    Ok(found.map(|(path, offset, size): (String, u64, u64)| {
        let next = offset.div_ceil(16) * 16;
        let shown = show(cx, builtins, ty).unwrap_or_default();
        Problem {
            code: "E1215",
            message: format!(
                "in `Uniform<{shown}>`, `{path}` ({size} bytes at offset {offset}) crosses a \
                 16-byte row; reorder the fields or add padding so that it starts at offset \
                 {next}"
            ),
        }
    }))
}

/// Walks the leaves (scalars, vectors, matrix rows, resources) of `ty` at `base`.
fn rows(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: &Ty,
    base: u64,
    path: &mut String,
    found: &mut Option<(String, u64, u64)>,
    depth: u32,
) -> Result<(), Cancelled> {
    if found.is_some() || depth > 64 {
        return Ok(());
    }
    let Some(layout) = layout_of(cx, builtins, ty)? else {
        return Ok(());
    };
    match ty {
        Ty::Adt { item, args } if builtins.kind(*item).is_none() => {
            let tree = cx.get::<ItemTreeQuery>(&item.file)?;
            if tree.items[item.local].kind == ItemKind::Struct {
                for f in fields_of(cx, builtins, *item, args)?.unwrap_or_default() {
                    let len = path.len();
                    if !path.is_empty() {
                        path.push('.');
                    }
                    path.push_str(f.name.as_str());
                    rows(cx, builtins, &f.ty, base + f.offset, path, found, depth + 1)?;
                    path.truncate(len);
                }
                return Ok(());
            }
        }
        Ty::Adt { item, args } if builtins.kind(*item) == Some(BuiltinKind::Matrix) => {
            // Row by row.
            if let [
                GenericArgTy::Type(_),
                GenericArgTy::Const(r),
                GenericArgTy::Const(_),
            ] = &args[..]
                && let Some(r) = r.value()
                && r > 0
            {
                let row = layout.size / r as u64;
                for i in 0..r as u64 {
                    leaf(base + i * row, row, path, found);
                }
            }
            return Ok(());
        }
        Ty::Array { element, len } => {
            if let (Some(e), Some(n)) = (layout_of(cx, builtins, element)?, len.value()) {
                let stride = e.size.div_ceil(e.align) * e.align;
                // Offsets repeat modulo 16 after at most 16 elements.
                for i in 0..(n as u64).min(16) {
                    let len = path.len();
                    path.push_str(&format!("[{i}]"));
                    rows(
                        cx,
                        builtins,
                        element,
                        base + i * stride,
                        path,
                        found,
                        depth + 1,
                    )?;
                    path.truncate(len);
                }
            }
            return Ok(());
        }
        _ => {}
    }
    leaf(base, layout.size, path, found);
    Ok(())
}

fn leaf(offset: u64, size: u64, path: &str, found: &mut Option<(String, u64, u64)>) {
    let crosses = if size <= 16 {
        size > 0 && offset / 16 != (offset + size - 1) / 16
    } else {
        offset % 16 != 0
    };
    if crosses && found.is_none() {
        *found = Some((path.to_string(), offset, size));
    }
}
