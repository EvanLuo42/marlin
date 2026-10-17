//! Semantic types.
//!
//! Every named type, built-in ones included, is an [`Ty::Adt`]: the declaration plus its type
//! arguments. What a built-in declaration means (a scalar, `Vector<T, N>`, a texture) comes
//! from the [`Builtins`](crate::Builtins) registry, not from the type itself.

use std::sync::Arc;

use marlin_front_hir_def::{ItemId, Name};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// A type that could not be determined; the reason has been reported.
    Error,
    Void,
    /// A struct, enum, interface (as a constraint), class or record, with its arguments.
    Adt {
        item: ItemId,
        args: Arc<[GenericArgTy]>,
    },
    Array {
        element: Arc<Ty>,
        len: ConstTy,
    },
    Tuple(Arc<[Ty]>),
    /// The `index`th type parameter of `owner`.
    Param {
        owner: ItemId,
        index: u32,
    },
    /// An associated type of a type parameter, `M.Params`, resolved through the parameter's
    /// interface bounds during type checking.
    Assoc {
        base: Arc<Ty>,
        name: Name,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GenericArgTy {
    Type(Ty),
    Const(ConstTy),
}

/// The value of a constant generic argument or array length.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConstTy {
    Error,
    Value(i128),
    /// The `index`th (constant) generic parameter of `owner`.
    Param {
        owner: ItemId,
        index: u32,
    },
    /// A `const` field, evaluated at compile time later (0006 §11).
    Item(ItemId),
    /// Arithmetic that involves parameters or items: `N * 2`.
    Expr(Arc<ConstExpr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    Neg,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConstExpr {
    pub op: ConstOp,
    pub lhs: ConstTy,
    /// `None` for `Neg`.
    pub rhs: Option<ConstTy>,
}

impl ConstTy {
    /// Folds `op` over constants when both are known; otherwise keeps it symbolic.
    pub fn apply(op: ConstOp, lhs: ConstTy, rhs: Option<ConstTy>) -> ConstTy {
        use ConstTy::Value;
        match (&lhs, &rhs) {
            (ConstTy::Error, _) | (_, Some(ConstTy::Error)) => ConstTy::Error,
            (Value(a), None) if op == ConstOp::Neg => a.checked_neg().map_or(ConstTy::Error, Value),
            (Value(a), Some(Value(b))) => {
                let (a, b) = (*a, *b);
                let folded = match op {
                    ConstOp::Add => a.checked_add(b),
                    ConstOp::Sub => a.checked_sub(b),
                    ConstOp::Mul => a.checked_mul(b),
                    ConstOp::Div => a.checked_div(b),
                    ConstOp::Rem => a.checked_rem(b),
                    ConstOp::Shl => u32::try_from(b).ok().and_then(|b| a.checked_shl(b)),
                    ConstOp::Shr => u32::try_from(b).ok().and_then(|b| a.checked_shr(b)),
                    ConstOp::Neg => None,
                };
                folded.map_or(ConstTy::Error, Value)
            }
            _ => ConstTy::Expr(Arc::new(ConstExpr { op, lhs, rhs })),
        }
    }

    pub fn value(&self) -> Option<i128> {
        match self {
            ConstTy::Value(v) => Some(*v),
            _ => None,
        }
    }
}

impl Ty {
    pub fn adt(item: ItemId) -> Ty {
        Ty::Adt {
            item,
            args: Arc::new([]),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }
}
