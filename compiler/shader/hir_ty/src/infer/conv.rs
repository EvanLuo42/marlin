//! Conversions (0004 §9.3, §9.4) and C#'s "better conversion" ordering (§9.8).

use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery};
use marlin_lang_query::Cancelled;

use super::{InferCtx, Lit};
use crate::builtins::Scalar;
use crate::item::ItemTypesQuery;
use crate::ty::{ConstTy, Ty};

/// C#'s implicit numeric conversions, plus `half` → `float`/`double`.
pub(crate) fn scalar_implicit(from: Scalar, to: Scalar) -> bool {
    use Scalar::*;
    from == to
        || matches!(
            (from, to),
            (I16, I32 | I64 | F32 | F64)
                | (U16, I32 | U32 | I64 | U64 | F32 | F64)
                | (I32, I64 | F32 | F64)
                | (U32, I64 | U64 | F32 | F64)
                | (I64, F32 | F64)
                | (U64, F32 | F64)
                | (F16, F32 | F64)
                | (F32, F64)
        )
}

pub(crate) fn is_integer(s: Scalar) -> bool {
    use Scalar::*;
    matches!(s, I16 | U16 | I32 | U32 | I64 | U64)
}

pub(crate) fn is_float(s: Scalar) -> bool {
    matches!(s, Scalar::F16 | Scalar::F32 | Scalar::F64)
}

pub(crate) fn is_signed(s: Scalar) -> bool {
    use Scalar::*;
    matches!(s, I16 | I32 | I64 | F16 | F32 | F64)
}

fn fits(v: i128, s: Scalar) -> bool {
    use Scalar::*;
    let (lo, hi): (i128, i128) = match s {
        I16 => (i16::MIN.into(), i16::MAX.into()),
        U16 => (0, u16::MAX.into()),
        I32 => (i32::MIN.into(), i32::MAX.into()),
        U32 => (0, u32::MAX.into()),
        I64 => (i64::MIN.into(), i64::MAX.into()),
        U64 => (0, u64::MAX.into()),
        F16 | F32 | F64 => return true,
        Bool => return false,
    };
    (lo..=hi).contains(&v)
}

/// Whether an unsuffixed literal converts to scalar `to` by context (§9.3).
fn literal_fits(lit: Lit, to: Scalar) -> bool {
    match lit {
        Lit::None => false,
        Lit::Int(v) => fits(v, to),
        Lit::Float => is_float(to),
    }
}

/// The natural type of an integer literal's value, as in C#.
pub(crate) fn natural_int(v: i128) -> Scalar {
    if fits(v, Scalar::I32) {
        Scalar::I32
    } else if fits(v, Scalar::U32) {
        Scalar::U32
    } else if fits(v, Scalar::I64) {
        Scalar::I64
    } else {
        Scalar::U64
    }
}

impl InferCtx<'_, '_> {
    /// Whether a value of type `from` (an expression with literal flexibility `lit`) converts
    /// implicitly to `to`.
    pub(crate) fn implicit(&self, from: &Ty, to: &Ty, lit: Lit) -> Result<bool, Cancelled> {
        if from == to || from.is_error() || to.is_error() {
            return Ok(true);
        }
        if let (Some(a), Some(b)) = (self.scalar_of(from), self.scalar_of(to)) {
            return Ok(scalar_implicit(a, b) || literal_fits(lit, b));
        }
        // Scalar → vector: broadcast.
        if self.scalar_of(from).is_some()
            && let Some((element, _)) = self.vector_of(to)
        {
            return self.implicit(from, &element, lit);
        }
        // Vector → vector and matrix → matrix of the same shape, element by element.
        if let (Some((fe, fnn)), Some((te, tn))) = (self.vector_of(from), self.vector_of(to)) {
            return Ok(same_const(&fnn, &tn) && self.implicit(&fe, &te, lit)?);
        }
        if let (Some((fe, fr, fc)), Some((te, tr, tc))) = (self.matrix_of(from), self.matrix_of(to))
        {
            return Ok(same_const(&fr, &tr)
                && same_const(&fc, &tc)
                && self.implicit(&fe, &te, lit)?);
        }
        Ok(self.user_conversion(from, to, false)?.is_some())
    }

    /// Whether `(to)value` is allowed: implicit conversions, all numeric conversions,
    /// vector truncation, enum ↔ integer, and declared conversions.
    pub(crate) fn explicit(&self, from: &Ty, to: &Ty) -> Result<bool, Cancelled> {
        if self.implicit(from, to, Lit::None)? {
            return Ok(true);
        }
        let numeric = |s: Scalar| s != Scalar::Bool;
        if let (Some(a), Some(b)) = (self.scalar_of(from), self.scalar_of(to)) {
            return Ok(numeric(a) && numeric(b));
        }
        if let (Some((fe, fnn)), Some((te, tn))) = (self.vector_of(from), self.vector_of(to)) {
            let wide_enough = match (fnn.value(), tn.value()) {
                (Some(f), Some(t)) => f >= t,
                _ => same_const(&fnn, &tn),
            };
            return Ok(wide_enough && self.explicit(&fe, &te)?);
        }
        if let (Some((fe, fr, fc)), Some((te, tr, tc))) = (self.matrix_of(from), self.matrix_of(to))
        {
            return Ok(same_const(&fr, &tr) && same_const(&fc, &tc) && self.explicit(&fe, &te)?);
        }
        let is_enum = |ty: &Ty| -> Result<bool, Cancelled> {
            Ok(match ty {
                Ty::Adt { item, .. } => self.item_kind(*item)? == ItemKind::Enum,
                _ => false,
            })
        };
        let integer = |ty: &Ty| self.scalar_of(ty).is_some_and(is_integer);
        if (is_enum(from)? && integer(to)) || (integer(from) && is_enum(to)?) {
            return Ok(true);
        }
        Ok(self.user_conversion(from, to, true)?.is_some())
    }

    /// A declared `implicit` (or, with `explicit`, also `explicit`) conversion operator from
    /// `from` to `to`, declared in either type.
    pub(crate) fn user_conversion(
        &self,
        from: &Ty,
        to: &Ty,
        explicit: bool,
    ) -> Result<Option<ItemId>, Cancelled> {
        for ty in [from, to] {
            let Ty::Adt { item, args } = ty else {
                continue;
            };
            if self.builtins.kind(*item).is_some() {
                continue;
            }
            let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
            let subst = crate::subst::Subst::new().with_args(*item, args);
            for &member in &tree.items[item.local].members {
                if tree.items[member].kind != ItemKind::Conversion {
                    continue;
                }
                let conversion = ItemId {
                    file: item.file,
                    local: member,
                };
                let types = self.cx.get::<ItemTypesQuery>(&conversion)?;
                let Some(param) = types.params.first() else {
                    continue;
                };
                let source = subst.ty(&param.ty);
                let target = subst.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                let is_implicit = self.conversion_is_implicit(conversion)?;
                if &source == from && &target == to && (is_implicit || explicit) {
                    return Ok(Some(conversion));
                }
            }
        }
        Ok(None)
    }

    fn conversion_is_implicit(&self, conversion: ItemId) -> Result<bool, Cancelled> {
        let (_, map) = self
            .cx
            .get::<marlin_front_hir_def::SignatureWithSourceMap>(&conversion)?;
        let _ = map;
        let tree = self.cx.get::<ItemTreeQuery>(&conversion.file)?;
        let ids = self
            .cx
            .get::<marlin_front_hir_def::AstIdMap>(&conversion.file)?;
        let parse = self
            .cx
            .get::<marlin_front_hir_def::ParseFile>(&conversion.file)?;
        let node = ids
            .ptr(tree.items[conversion.local].ast)
            .to_node(&parse.syntax_node());
        Ok(node
            .child_tokens()
            .any(|t| t.kind() == marlin_front_syntax::SyntaxKind::IMPLICIT_KW))
    }

    /// C#'s better conversion target: whether converting to `a` beats converting to `b` for a
    /// value of type `from` (with literal flexibility `lit`). `None` when neither is better.
    pub(crate) fn better_target(
        &self,
        from: &Ty,
        lit: Lit,
        a: &Ty,
        b: &Ty,
    ) -> Result<Option<bool>, Cancelled> {
        if a == b {
            return Ok(None);
        }
        if from == a {
            return Ok(Some(true));
        }
        if from == b {
            return Ok(Some(false));
        }
        let ab = self.implicit(a, b, Lit::None)?;
        let ba = self.implicit(b, a, Lit::None)?;
        if ab && !ba {
            return Ok(Some(true));
        }
        if ba && !ab {
            return Ok(Some(false));
        }
        // A signed integer beats an unsigned one (C#'s tie-break for literals).
        if let (Some(x), Some(y)) = (self.element_scalar(a), self.element_scalar(b))
            && is_integer(x)
            && is_integer(y)
            && is_signed(x) != is_signed(y)
        {
            return Ok(Some(is_signed(x)));
        }
        // For a literal, the narrower float (its context type) beats the wider one only
        // through the rules above; otherwise there is no winner.
        let _ = lit;
        Ok(None)
    }

    /// The scalar of a scalar type or the element scalar of a vector or matrix.
    pub(crate) fn element_scalar(&self, ty: &Ty) -> Option<Scalar> {
        self.scalar_of(ty)
            .or_else(|| self.vector_of(ty).and_then(|(e, _)| self.scalar_of(&e)))
            .or_else(|| self.matrix_of(ty).and_then(|(e, _, _)| self.scalar_of(&e)))
    }
}

/// Whether two constant lengths are known to be equal.
pub(crate) fn same_const(a: &ConstTy, b: &ConstTy) -> bool {
    a == b || matches!(a, ConstTy::Error) || matches!(b, ConstTy::Error)
}
