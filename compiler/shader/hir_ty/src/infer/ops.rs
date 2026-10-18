//! Operators: user-defined `operator` declarations (also through interface bounds of type
//! parameters), and the built-in operators on scalars, vectors and matrices.

use marlin_front_hir_def::store::{BinaryOp, ExprId, UnaryOp};
use marlin_front_hir_def::{ItemId, ItemKind, ItemSignature, ItemTreeQuery};
use marlin_lang_query::Cancelled;

use super::call::{Candidate, Pick};
use super::conv::{is_integer, is_signed, same_const};
use super::{Callee, InferCtx, InferLoc, Lit};
use crate::builtins::Scalar;
use crate::item::ItemTypesQuery;
use crate::subst::Subst;
use crate::ty::{ConstTy, Ty};

fn unary_symbol(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Plus => "+",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
        UnaryOp::PreInc | UnaryOp::PostInc => "++",
        UnaryOp::PreDec | UnaryOp::PostDec => "--",
    }
}

/// The scalar signatures of a built-in binary operator: `(operand, result)` pairs.
fn builtin_binary(op: BinaryOp) -> Vec<(Scalar, Scalar)> {
    use BinaryOp::*;
    use Scalar::*;
    let numeric = [I32, U32, I64, U64, F16, F32, F64];
    match op {
        Add | Sub | Mul | Div | Rem => numeric.iter().map(|&s| (s, s)).collect(),
        Lt | Le | Gt | Ge => numeric.iter().map(|&s| (s, Bool)).collect(),
        Eq | Ne => numeric.iter().chain([&Bool]).map(|&s| (s, Bool)).collect(),
        BitAnd | BitOr | BitXor => [I32, U32, I64, U64, Bool].iter().map(|&s| (s, s)).collect(),
        Shl | Shr | UShr => [I32, U32, I64, U64].iter().map(|&s| (s, s)).collect(),
        And | Or => vec![(Bool, Bool)],
    }
}

/// A value's shape for element-wise operators.
#[derive(Clone)]
enum Shape {
    Scalar,
    Vector(ConstTy),
    Matrix(ConstTy, ConstTy),
}

impl InferCtx<'_, '_> {
    fn shape(&self, ty: &Ty) -> Option<(Ty, Shape)> {
        if self.scalar_of(ty).is_some() {
            return Some((ty.clone(), Shape::Scalar));
        }
        if let Some((e, n)) = self.vector_of(ty) {
            return Some((e, Shape::Vector(n)));
        }
        if let Some((e, r, c)) = self.matrix_of(ty) {
            return Some((e, Shape::Matrix(r, c)));
        }
        None
    }

    fn reshape(&self, element: Ty, shape: &Shape) -> Ty {
        match shape {
            Shape::Scalar => element,
            Shape::Vector(n) => self.vector_ty(element, n.clone()),
            Shape::Matrix(r, c) => self.matrix_ty(element, r.clone(), c.clone()),
        }
    }

    /// `operator` declarations for `symbol` in the operand types and their bounds.
    fn user_operators(
        &self,
        symbol: &str,
        operand_tys: &[&Ty],
    ) -> Result<Vec<(ItemId, Subst)>, Cancelled> {
        let mut found: Vec<(ItemId, Subst)> = Vec::new();
        let search = |item: ItemId,
                      subst: Subst,
                      found: &mut Vec<(ItemId, Subst)>|
         -> Result<(), Cancelled> {
            let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
            for &m in &tree.items[item.local].members {
                if tree.items[m].kind != ItemKind::Operator {
                    continue;
                }
                let op = ItemId {
                    file: item.file,
                    local: m,
                };
                let sig = self.cx.get::<ItemSignature>(&op)?;
                if sig.operator.as_deref() == Some(symbol) && !found.iter().any(|(i, _)| *i == op) {
                    found.push((op, subst.clone()));
                }
            }
            Ok(())
        };
        for ty in operand_tys {
            match ty {
                Ty::Adt { item, args } if self.builtins.kind(*item).is_none() => {
                    search(*item, Subst::new().with_args(*item, args), &mut found)?;
                }
                Ty::Param { owner, index } => {
                    let types = self.cx.get::<ItemTypesQuery>(owner)?;
                    if let Some(param) = types.generics.get(*index as usize) {
                        // The bounds and the interfaces they extend.
                        for (item, subst) in crate::item::bound_interfaces(self.cx, &param.bounds)?
                        {
                            search(item, subst, &mut found)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(found)
    }

    pub(crate) fn binary(
        &mut self,
        e: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Ty, Cancelled> {
        let lt = self.infer_expr(lhs, None)?;
        let rt = self.infer_expr(rhs, None)?;
        if lt.is_error() || rt.is_error() {
            return Ok(Ty::Error);
        }
        let (ll, rl) = (self.literal_of(lhs), self.literal_of(rhs));

        // User-defined operators first.
        let users = self.user_operators(op.symbol(), &[&lt, &rt])?;
        if !users.is_empty() {
            let args = vec![
                super::call::ArgInfo {
                    expr: lhs,
                    name: None,
                    modifier: None,
                    ty: Some(lt.clone()),
                    lit: ll,
                },
                super::call::ArgInfo {
                    expr: rhs,
                    name: None,
                    modifier: None,
                    ty: Some(rt.clone()),
                    lit: rl,
                },
            ];
            let mut mapped = Vec::new();
            for (item, outer) in &users {
                if let Some(m) = self.map_call(*item, outer, None, &args)? {
                    mapped.push(m);
                }
            }
            let candidates: Vec<Candidate> = mapped
                .iter()
                .map(|m| Candidate {
                    params: m.arg_params.clone(),
                    generic: m.generic,
                    defaults_used: 0,
                })
                .collect();
            if let Pick::One(i) = self.pick(&candidates, &[(lt.clone(), ll), (rt.clone(), rl)])? {
                let chosen = mapped.swap_remove(i);
                self.coerce(lhs, &lt, &chosen.arg_params[0])?;
                self.coerce(rhs, &rt, &chosen.arg_params[1])?;
                let ret = self.cx.get::<ItemTypesQuery>(&chosen.item)?.ty.clone();
                let ret = chosen.subst.ty(&ret.unwrap_or(Ty::Error));
                self.result.callees.insert(
                    e,
                    Callee::Item {
                        item: chosen.item,
                        subst: chosen.subst,
                    },
                );
                return Ok(ret);
            }
        }

        // Enums (0004 §3.5): `==` and `!=` between values of one enum, and for a `[Flags]` enum
        // `&`, `|` and `^`, which give the enum (as C# has them).
        if let (Ty::Adt { item: a, .. }, Ty::Adt { item: b, .. }) = (&lt, &rt)
            && a == b
        {
            let tree = self.cx.get::<ItemTreeQuery>(&a.file)?;
            if tree.items[a.local].kind == ItemKind::Enum {
                let flags = self
                    .cx
                    .get::<crate::attrs::ItemAttributesQuery>(a)?
                    .has(crate::attrs::KnownAttr::Flags);
                let result = match op {
                    BinaryOp::Eq | BinaryOp::Ne => Some(self.scalar_ty(Scalar::Bool)),
                    BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor if flags => {
                        Some(lt.clone())
                    }
                    _ => None,
                };
                if let Some(r) = result {
                    self.result.callees.insert(e, Callee::BuiltinOp);
                    return Ok(r);
                }
            }
        }

        let (Some((le, ls)), Some((re, rs))) = (self.shape(&lt), self.shape(&rt)) else {
            return Ok(self.no_operator(e, op.symbol(), &lt, &rt));
        };

        // Matrix products.
        if op == BinaryOp::Mul
            && let Some(ty) = self.matrix_product(e, (&le, &ls), (&re, &rs))?
        {
            return Ok(ty);
        }

        // Element-wise: broadcast scalars, require equal lengths otherwise.
        let shape = match (&ls, &rs) {
            (Shape::Scalar, s) | (s, Shape::Scalar) => s.clone(),
            (Shape::Vector(a), Shape::Vector(b)) if same_const(a, b) => ls.clone(),
            (Shape::Matrix(ar, ac), Shape::Matrix(br, bc))
                if same_const(ar, br) && same_const(ac, bc) && !matches!(op, BinaryOp::Mul) =>
            {
                ls.clone()
            }
            _ => return Ok(self.no_operator(e, op.symbol(), &lt, &rt)),
        };

        // `int` and `uint` variables do not mix (0004 §9.3).
        if let (Some(a), Some(b)) = (self.scalar_of(&le), self.scalar_of(&re))
            && is_integer(a)
            && is_integer(b)
            && is_signed(a) != is_signed(b)
            && ll == Lit::None
            && rl == Lit::None
            && !matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr)
        {
            let (a, b) = (self.show(&lt), self.show(&rt));
            self.report(
                InferLoc::Expr(e),
                "E1102",
                format!("`{a}` and `{b}` do not mix; convert one of them explicitly"),
            );
            return Ok(Ty::Error);
        }

        let Some((operand, result)) = self.element_op(op, (&le, ll), (&re, rl))? else {
            return Ok(self.no_operator(e, op.symbol(), &lt, &rt));
        };
        let (lop, rop) = match op {
            // The right operand of a shift is an `int` (or `uint`), whatever the left is.
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr => {
                let count = if self.scalar_of(&re) == Some(Scalar::U32) {
                    Scalar::U32
                } else {
                    Scalar::I32
                };
                (operand.clone(), self.scalar_ty(count))
            }
            _ => (operand.clone(), operand.clone()),
        };
        let lt_target = self.reshape(
            lop,
            &match ls {
                Shape::Scalar => Shape::Scalar,
                _ => shape.clone(),
            },
        );
        let rt_target = self.reshape(
            rop,
            &match rs {
                Shape::Scalar => Shape::Scalar,
                _ => shape.clone(),
            },
        );
        self.coerce(lhs, &lt, &lt_target)?;
        self.coerce(rhs, &rt, &rt_target)?;
        self.result.callees.insert(e, Callee::BuiltinOp);
        Ok(self.reshape(result, &shape))
    }

    /// The built-in scalar operator for element types `l` and `r`: `(operand type, result)`.
    fn element_op(
        &mut self,
        op: BinaryOp,
        (l, ll): (&Ty, Lit),
        (r, rl): (&Ty, Lit),
    ) -> Result<Option<(Ty, Ty)>, Cancelled> {
        // Elements that are type parameters use their bounds' operators.
        if matches!(l, Ty::Param { .. }) || matches!(r, Ty::Param { .. }) {
            let users = self.user_operators(op.symbol(), &[l, r])?;
            for (item, outer) in users {
                let types = self.cx.get::<ItemTypesQuery>(&item)?;
                let params: Vec<Ty> = types.params.iter().map(|p| outer.ty(&p.ty)).collect();
                if params.len() == 2
                    && self.implicit(l, &params[0], ll)?
                    && self.implicit(r, &params[1], rl)?
                {
                    let ret = outer.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                    return Ok(Some((params[0].clone(), ret)));
                }
            }
            return Ok(None);
        }
        // A flexible literal next to a typed operand takes that operand's type when it fits:
        // `h * 0.5` is `half`, `id.x + 1` is `uint` (0004 §9.3).
        let (mut l, mut ll, mut r, mut rl) = (l.clone(), ll, r.clone(), rl);
        if ll != Lit::None
            && rl == Lit::None
            && self.scalar_of(&r).is_some()
            && self.implicit(&l, &r, ll)?
        {
            (l, ll) = (r.clone(), Lit::None);
        } else if rl != Lit::None
            && ll == Lit::None
            && self.scalar_of(&l).is_some()
            && self.implicit(&r, &l, rl)?
        {
            (r, rl) = (l.clone(), Lit::None);
        }
        let (l, r) = (&l, &r);
        // Only the scalars the core library declares have operators.
        let signatures: Vec<(Scalar, Scalar)> = builtin_binary(op)
            .into_iter()
            .filter(|(o, r)| {
                self.builtins.item(crate::BuiltinKind::Scalar(*o)).is_some()
                    && self.builtins.item(crate::BuiltinKind::Scalar(*r)).is_some()
            })
            .collect();
        let operand_of = |s: &Self, (o, _): &(Scalar, Scalar)| s.scalar_ty(*o);
        let candidates: Vec<Candidate> = signatures
            .iter()
            .map(|sig| {
                let t = operand_of(self, sig);
                Candidate {
                    params: vec![t.clone(), t],
                    generic: false,
                    defaults_used: 0,
                }
            })
            .collect();
        Ok(
            match self.pick(&candidates, &[(l.clone(), ll), (r.clone(), rl)])? {
                Pick::One(i) => {
                    let (operand, result) = signatures[i];
                    Some((self.scalar_ty(operand), self.scalar_ty(result)))
                }
                _ => None,
            },
        )
    }

    /// `matrix * matrix`, `matrix * vector` (column vectors, 0004 §9.5), and `scalar * matrix`
    /// in either order.
    fn matrix_product(
        &mut self,
        e: ExprId,
        (le, ls): (&Ty, &Shape),
        (re, rs): (&Ty, &Shape),
    ) -> Result<Option<Ty>, Cancelled> {
        let result_shape = match (ls, rs) {
            (Shape::Matrix(r, k1), Shape::Matrix(k2, c)) => {
                if !same_const(k1, k2) {
                    return Ok(None);
                }
                Shape::Matrix(r.clone(), c.clone())
            }
            (Shape::Matrix(r, k), Shape::Vector(n)) => {
                if !same_const(k, n) {
                    return Ok(None);
                }
                Shape::Vector(r.clone())
            }
            _ => return Ok(None),
        };
        let Some((operand, result)) =
            self.element_op(BinaryOp::Mul, (le, Lit::None), (re, Lit::None))?
        else {
            return Ok(None);
        };
        let _ = operand;
        self.result.callees.insert(e, Callee::BuiltinOp);
        Ok(Some(self.reshape(result, &result_shape)))
    }

    fn no_operator(&mut self, e: ExprId, symbol: &str, l: &Ty, r: &Ty) -> Ty {
        let (l, r) = (self.show(l), self.show(r));
        self.report(
            InferLoc::Expr(e),
            "E1101",
            format!("no operator `{symbol}` for `{l}` and `{r}`"),
        );
        Ty::Error
    }

    pub(crate) fn unary(
        &mut self,
        e: ExprId,
        op: UnaryOp,
        operand: ExprId,
    ) -> Result<Ty, Cancelled> {
        let ty = self.infer_expr(operand, None)?;
        if ty.is_error() {
            return Ok(Ty::Error);
        }
        if matches!(
            op,
            UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec
        ) {
            self.check_place(operand)?;
        }
        let users = self.user_operators(unary_symbol(op), &[&ty])?;
        for (item, outer) in users {
            let types = self.cx.get::<ItemTypesQuery>(&item)?;
            if types.params.len() == 1 && outer.ty(&types.params[0].ty) == ty {
                let ret = outer.ty(types.ty.as_ref().unwrap_or(&Ty::Error));
                self.result
                    .callees
                    .insert(e, Callee::Item { item, subst: outer });
                return Ok(ret);
            }
        }
        let Some((element, shape)) = self.shape(&ty) else {
            return Ok(self.no_unary(e, op, &ty));
        };
        let Some(s) = self.scalar_of(&element) else {
            return Ok(self.no_unary(e, op, &ty));
        };
        use Scalar::*;
        let ok = match op {
            UnaryOp::Neg => {
                matches!(s, I16 | I32 | I64 | F16 | F32 | F64)
                    || (matches!(self.literal_of(operand), Lit::Int(_)))
            }
            UnaryOp::Plus
            | UnaryOp::PreInc
            | UnaryOp::PreDec
            | UnaryOp::PostInc
            | UnaryOp::PostDec => s != Bool,
            UnaryOp::Not => s == Bool && matches!(shape, Shape::Scalar),
            UnaryOp::BitNot => is_integer(s),
        };
        if !ok {
            return Ok(self.no_unary(e, op, &ty));
        }
        self.result.callees.insert(e, Callee::BuiltinOp);
        // Short operands are promoted to int, as in C#.
        let promoted = match (op, s) {
            (UnaryOp::Neg | UnaryOp::Plus | UnaryOp::BitNot, I16 | U16) => {
                self.reshape(self.scalar_ty(I32), &shape)
            }
            _ => ty,
        };
        Ok(promoted)
    }

    fn no_unary(&mut self, e: ExprId, op: UnaryOp, ty: &Ty) -> Ty {
        let shown = self.show(ty);
        self.report(
            InferLoc::Expr(e),
            "E1101",
            format!("no operator `{}` for `{shown}`", unary_symbol(op)),
        );
        Ty::Error
    }
}
