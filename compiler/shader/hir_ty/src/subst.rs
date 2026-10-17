//! Generic substitution and unification.

use marlin_front_hir_def::ItemId;

use crate::ty::{ConstTy, GenericArgTy, Ty};

/// Arguments for the generic parameters of one or more declarations (a struct and its method).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Subst {
    pub entries: Vec<(ItemId, Vec<Option<GenericArgTy>>)>,
}

impl Subst {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `owner`'s parameters, all unknown.
    pub fn with_unknowns(mut self, owner: ItemId, count: usize) -> Self {
        self.entries.push((owner, vec![None; count]));
        self
    }

    /// Adds `owner`'s parameters with known arguments.
    pub fn with_args(mut self, owner: ItemId, args: &[GenericArgTy]) -> Self {
        self.entries
            .push((owner, args.iter().cloned().map(Some).collect()));
        self
    }

    fn slot(&mut self, owner: ItemId, index: u32) -> Option<&mut Option<GenericArgTy>> {
        self.entries
            .iter_mut()
            .find(|(o, _)| *o == owner)
            .and_then(|(_, args)| args.get_mut(index as usize))
    }

    pub fn get(&self, owner: ItemId, index: u32) -> Option<&GenericArgTy> {
        self.entries
            .iter()
            .find(|(o, _)| *o == owner)
            .and_then(|(_, args)| args.get(index as usize))
            .and_then(Option::as_ref)
    }

    /// Whether every parameter of `owner` has an argument.
    pub fn complete(&self, owner: ItemId) -> bool {
        self.entries
            .iter()
            .filter(|(o, _)| *o == owner)
            .all(|(_, args)| args.iter().all(Option::is_some))
    }

    pub fn args_of(&self, owner: ItemId) -> Vec<GenericArgTy> {
        self.entries
            .iter()
            .find(|(o, _)| *o == owner)
            .map(|(_, args)| {
                args.iter()
                    .map(|a| a.clone().unwrap_or(GenericArgTy::Type(Ty::Error)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn ty(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Param { owner, index } => match self.get(*owner, *index) {
                Some(GenericArgTy::Type(t)) => t.clone(),
                _ => ty.clone(),
            },
            Ty::Adt { item, args } => Ty::Adt {
                item: *item,
                args: args.iter().map(|a| self.arg(a)).collect(),
            },
            Ty::Array { element, len } => Ty::Array {
                element: self.ty(element).into(),
                len: self.konst(len),
            },
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(|t| self.ty(t)).collect()),
            Ty::Assoc { base, name } => Ty::Assoc {
                base: self.ty(base).into(),
                name: name.clone(),
            },
            Ty::Error | Ty::Void => ty.clone(),
        }
    }

    pub fn arg(&self, arg: &GenericArgTy) -> GenericArgTy {
        match arg {
            GenericArgTy::Type(t) => GenericArgTy::Type(self.ty(t)),
            GenericArgTy::Const(c) => GenericArgTy::Const(self.konst(c)),
        }
    }

    pub fn konst(&self, c: &ConstTy) -> ConstTy {
        match c {
            ConstTy::Param { owner, index } => match self.get(*owner, *index) {
                Some(GenericArgTy::Const(v)) => v.clone(),
                _ => c.clone(),
            },
            ConstTy::Expr(e) => ConstTy::apply(
                e.op,
                self.konst(&e.lhs),
                e.rhs.as_ref().map(|r| self.konst(r)),
            ),
            _ => c.clone(),
        }
    }

    /// Infers parameters of the substitution's owners from `param` (written with them) and the
    /// argument type `arg`. Inference only collects: where the two do not line up there is
    /// nothing to learn (whether `arg` converts is checked later). Returns `false` only when a
    /// parameter would get two different arguments.
    pub fn unify(&mut self, param: &Ty, arg: &Ty) -> bool {
        match (param, arg) {
            (_, Ty::Error) | (Ty::Error, _) => true,
            (Ty::Param { owner, index }, _) => {
                let (owner, index) = (*owner, *index);
                if let Some(slot) = self.slot(owner, index) {
                    match slot {
                        None => {
                            *slot = Some(GenericArgTy::Type(arg.clone()));
                            true
                        }
                        Some(GenericArgTy::Type(t)) => t == arg,
                        Some(GenericArgTy::Const(_)) => false,
                    }
                } else {
                    param == arg
                }
            }
            (Ty::Adt { item: a, args: pa }, Ty::Adt { item: b, args: aa }) => {
                if a != b || pa.len() != aa.len() {
                    return true;
                }
                pa.iter().zip(aa.iter()).all(|(p, a)| match (p, a) {
                    (GenericArgTy::Type(p), GenericArgTy::Type(a)) => self.unify(p, a),
                    (GenericArgTy::Const(p), GenericArgTy::Const(a)) => self.unify_const(p, a),
                    _ => true,
                })
            }
            (
                Ty::Array {
                    element: pe,
                    len: pl,
                },
                Ty::Array {
                    element: ae,
                    len: al,
                },
            ) => self.unify(pe, ae) && self.unify_const(pl, al),
            (Ty::Tuple(p), Ty::Tuple(a)) => {
                p.len() != a.len() || p.iter().zip(a.iter()).all(|(p, a)| self.unify(p, a))
            }
            _ => true,
        }
    }

    fn unify_const(&mut self, param: &ConstTy, arg: &ConstTy) -> bool {
        match (param, arg) {
            (_, ConstTy::Error) | (ConstTy::Error, _) => true,
            (ConstTy::Param { owner, index }, _) => {
                let (owner, index) = (*owner, *index);
                match self.slot(owner, index) {
                    Some(slot @ None) => {
                        *slot = Some(GenericArgTy::Const(arg.clone()));
                        true
                    }
                    Some(Some(GenericArgTy::Const(c))) => c == arg,
                    Some(Some(GenericArgTy::Type(_))) => false,
                    None => param == arg,
                }
            }
            (ConstTy::Value(_), ConstTy::Value(_)) => true,
            // `N * 2` against 6: not inverted; checked after substitution.
            _ => true,
        }
    }
}
