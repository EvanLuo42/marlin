//! Compile-time evaluation (0006 §11): the initializer of a `const` field, an enum member or a
//! field default is lowered into a scratch module and run in the IR interpreter. Enum members
//! without an initializer count up from the previous one (from 0). Values are kept apart from
//! any module (as interpreter values with their front-end type) and made into constants of the
//! module that uses them. A local `const` is evaluated the same way, as a function of its
//! body (with the body's generic arguments) that only computes the initializer.

use std::collections::HashMap;

use marlin_front_hir_def::store::BindingId;
use marlin_front_hir_def::{
    BodyOwner, BodyPart, BodyQuery, ItemId, ItemKind, ItemTreeQuery, body_owners, item_path,
};
use marlin_shaderc_hir_ty::{InferQuery, ItemTypesQuery, Subst, Ty};
use marlin_shaderc_ir::{
    Const, ConstValue as IrConst, Interpreter, Type, TypeData, Val, verify_module,
};

use crate::closures::Inner;
use crate::types::Num;
use crate::{Instance, LowerError, Lowering, R};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstValue {
    pub val: Val,
    pub ty: Ty,
}

impl Lowering<'_, '_> {
    /// The compile-time value of a constant, enum member or field default.
    pub(crate) fn const_value(&mut self, item: ItemId) -> R<ConstValue> {
        if let Some(v) = self.consts.get(&item) {
            return Ok(v.clone());
        }
        if self.evaluating.contains(&item) {
            let path = item_path(self.cx, item)?;
            return Err(LowerError::new(
                "E1306",
                format!("the value of `{path}` depends on itself"),
            ));
        }
        let v = self.evaluate(item, &Subst::new())?;
        self.consts.insert(item, v.clone());
        Ok(v)
    }

    /// The value of an interface's constant `item` for the type whose arguments for the interface
    /// `subst` holds: its default initializer, evaluated with them (not cached: it may depend on
    /// them).
    pub(crate) fn const_value_in(&mut self, item: ItemId, subst: &Subst) -> R<ConstValue> {
        if self.evaluating.contains(&item) {
            let path = item_path(self.cx, item)?;
            return Err(LowerError::new(
                "E1306",
                format!("the value of `{path}` depends on itself"),
            ));
        }
        self.evaluate(item, subst)
    }

    fn evaluate(&mut self, item: ItemId, subst: &Subst) -> R<ConstValue> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        let ty = types.ty.clone().unwrap_or(Ty::Error);
        if data.kind == ItemKind::EnumMember && body_owners(self.cx, item)?.is_empty() {
            // One more than the member before it.
            let parent = data
                .parent
                .ok_or_else(|| LowerError::new("E1306", "an enum member outside an enum"))?;
            let members: Vec<_> = tree.items[parent]
                .members
                .iter()
                .copied()
                .filter(|&m| tree.items[m].kind == ItemKind::EnumMember)
                .collect();
            let at = members.iter().position(|&m| m == item.local).unwrap_or(0);
            let num = self.num(&ty)?.unwrap_or(Num::Signed(32));
            let value = if at == 0 {
                0
            } else {
                let prev = ItemId {
                    file: item.file,
                    local: members[at - 1],
                };
                let p = self.const_value(prev)?;
                p.val.bits().unwrap_or(0).wrapping_add(1)
            };
            let mask = if num.bits() >= 64 {
                u64::MAX
            } else {
                (1 << num.bits()) - 1
            };
            return Ok(ConstValue {
                val: Val::Bits(value & mask),
                ty,
            });
        }
        if body_owners(self.cx, item)?.is_empty() {
            let path = item_path(self.cx, item)?;
            return Err(LowerError::new("E1306", format!("`{path}` has no value")));
        }
        // Lower the initializer into a module of its own and run it.
        let mut scratch = Lowering {
            cx: self.cx,
            builtins: self.builtins.clone(),
            module: marlin_shaderc_ir::Module::new(),
            types: HashMap::new(),
            structs: HashMap::new(),
            funcs: HashMap::new(),
            queue: Vec::new(),
            names: HashMap::new(),
            consts: self.consts.clone(),
            evaluating: self.evaluating.iter().copied().chain([item]).collect(),
            local_consts: self.local_consts.clone(),
            exports: Vec::new(),
            evaluating_locals: self.evaluating_locals.clone(),
            envs: HashMap::new(),
            statics: self.statics.clone(),
            shared: HashMap::new(),
            ctfe: true,
            locs: HashMap::new(),
            mesh: None,
            payload_field: None,
        };
        let f = scratch.instance(Instance {
            owner: BodyOwner {
                item,
                part: BodyPart::Main,
            },
            subst: subst.clone(),
            closures: Vec::new(),
            inner: None,
        })?;
        scratch.run()?;
        let errors = verify_module(&scratch.module);
        if let Some(e) = errors.first() {
            return Err(LowerError::new(
                "E1304",
                format!(
                    "evaluating `{}` lowered to bad IR: {e}",
                    item_path(self.cx, item)?
                ),
            ));
        }
        let mut results = Interpreter::new(&scratch.module)
            .call(f, &[])
            .map_err(|e| {
                LowerError::new(
                    "E1306",
                    format!(
                        "evaluating `{}`: {e}",
                        item_path(self.cx, item)
                            .map(|p| p.to_string())
                            .unwrap_or_default()
                    ),
                )
            })?;
        for (k, v) in scratch.consts {
            self.consts.entry(k).or_insert(v);
        }
        let val = results
            .pop()
            .ok_or_else(|| LowerError::new("E1306", "an initializer without a value"))?;
        Ok(ConstValue { val, ty })
    }

    /// An integer constant's value, sign-extended by its type.
    pub(crate) fn const_int(&mut self, item: ItemId) -> R<i128> {
        let v = self.const_value(item)?;
        let bits = v
            .val
            .bits()
            .ok_or_else(|| LowerError::new("E1306", "not an integer constant"))?;
        Ok(match self.num(&v.ty)? {
            Some(Num::Signed(w)) => {
                let shift = 64 - w;
                (((bits << shift) as i64) >> shift) as i128
            }
            _ => bits as i128,
        })
    }

    /// A constant of this module with the value of `item`, as type `ty`.
    pub(crate) fn const_operand(&mut self, item: ItemId, ty: &Ty) -> R<Const> {
        let v = self.const_value(item)?;
        let t = self.ty(ty)?;
        self.val_const(&v.val, t)
    }

    pub(crate) fn val_const(&mut self, val: &Val, t: Type) -> R<Const> {
        let bad = || LowerError::new("E1306", "a compile-time value of the wrong shape");
        let data = self.module.data(t).clone();
        Ok(match (data, val) {
            (TypeData::Scalar(s), Val::Bits(b)) => {
                let value = if s.is_float() {
                    IrConst::Float(*b)
                } else {
                    IrConst::Int(*b)
                };
                self.module.constant(t, value)
            }
            (TypeData::Res(_), Val::Bits(b)) => self.module.constant(t, IrConst::Int(*b)),
            (TypeData::Vector(s, _), Val::Agg(items)) => {
                let e = self.module.scalar(s);
                let parts = items
                    .iter()
                    .map(|v| self.val_const(v, e))
                    .collect::<R<_>>()?;
                self.module.constant(t, IrConst::Composite(parts))
            }
            (TypeData::Matrix { element, rows, .. }, Val::Agg(items)) => {
                let col = self.module.vector(element, rows);
                let parts = items
                    .iter()
                    .map(|v| self.val_const(v, col))
                    .collect::<R<_>>()?;
                self.module.constant(t, IrConst::Composite(parts))
            }
            (TypeData::Array(e, _), Val::Agg(items)) => {
                let parts = items
                    .iter()
                    .map(|v| self.val_const(v, e))
                    .collect::<R<_>>()?;
                self.module.constant(t, IrConst::Composite(parts))
            }
            (TypeData::Struct(s), Val::Agg(items)) => {
                let fields: Vec<Type> = self.module.structs[s].fields.iter().map(|f| f.1).collect();
                if fields.len() != items.len() {
                    return Err(bad());
                }
                let parts = items
                    .iter()
                    .zip(fields)
                    .map(|(v, f)| self.val_const(v, f))
                    .collect::<R<_>>()?;
                self.module.constant(t, IrConst::Composite(parts))
            }
            _ => return Err(bad()),
        })
    }

    /// A value of `ty` with every struct field at its default (zero where it has none).
    pub(crate) fn default_const(&mut self, ty: &Ty, t: Type) -> R<Const> {
        let TypeData::Struct(_) = self.module.data(t) else {
            return Ok(self.module.constant(t, IrConst::Zero));
        };
        let (s, fields) = self.struct_of(ty)?;
        let field_tys: Vec<Type> = self.module.structs[s].fields.iter().map(|f| f.1).collect();
        let mut parts = Vec::new();
        let mut any_default = false;
        for (f, ft) in fields.iter().zip(field_tys) {
            if body_owners(self.cx, *f)?.is_empty() {
                // A struct field without a default may hold a struct with defaults.
                let types = self.cx.get::<ItemTypesQuery>(f)?;
                let fty = match ty {
                    Ty::Adt { item, args } => marlin_shaderc_hir_ty::Subst::new()
                        .with_args(*item, args)
                        .ty(types.ty.as_ref().unwrap_or(&Ty::Error)),
                    _ => Ty::Error,
                };
                let c = self.default_const(&fty, ft)?;
                any_default |= self.module.consts[c].value != IrConst::Zero;
                parts.push(c);
            } else {
                let v = self.const_value(*f)?;
                parts.push(self.val_const(&v.val, ft)?);
                any_default = true;
            }
        }
        Ok(if any_default {
            self.module.constant(t, IrConst::Composite(parts))
        } else {
            self.module.constant(t, IrConst::Zero)
        })
    }
}

impl Lowering<'_, '_> {
    /// A constant of this module as an interpreter value.
    pub(crate) fn const_val(&self, c: Const) -> R<Val> {
        let data = &self.module.consts[c];
        Ok(match &data.value {
            IrConst::Int(b) | IrConst::Float(b) => Val::Bits(*b),
            IrConst::Zero | IrConst::Any => Val::zero(&self.module, data.ty)
                .map_err(|e| LowerError::new("E1306", e.to_string()))?,
            IrConst::Composite(items) => {
                Val::Agg(items.iter().map(|&i| self.const_val(i)).collect::<R<_>>()?)
            }
        })
    }
}

impl Lowering<'_, '_> {
    /// The compile-time value of the local `const` `b` of `owner`'s body, with the generic
    /// arguments `subst`: its initializer lowered into a module of its own and run.
    pub(crate) fn local_const(
        &mut self,
        owner: BodyOwner,
        subst: &Subst,
        b: BindingId,
    ) -> R<ConstValue> {
        let key = (owner, subst.clone(), b);
        if let Some(v) = self.local_consts.get(&key) {
            return Ok(v.clone());
        }
        let body = self.cx.get::<BodyQuery>(&owner)?;
        let name = body.store.bindings[b].name.to_string();
        if self.evaluating_locals.contains(&key) {
            return Err(LowerError::new(
                "E1306",
                format!("the value of `{name}` depends on itself"),
            ));
        }
        let infer = self.cx.get::<InferQuery>(&owner)?;
        let ty = self
            .resolve_assoc(&subst.ty(&infer.binding_types.get(b).cloned().unwrap_or(Ty::Error)));
        let mut scratch = Lowering {
            cx: self.cx,
            builtins: self.builtins.clone(),
            module: marlin_shaderc_ir::Module::new(),
            types: HashMap::new(),
            structs: HashMap::new(),
            funcs: HashMap::new(),
            queue: Vec::new(),
            names: HashMap::new(),
            consts: self.consts.clone(),
            evaluating: self.evaluating.clone(),
            local_consts: self.local_consts.clone(),
            exports: Vec::new(),
            evaluating_locals: self
                .evaluating_locals
                .iter()
                .cloned()
                .chain([key.clone()])
                .collect(),
            envs: HashMap::new(),
            statics: self.statics.clone(),
            shared: HashMap::new(),
            ctfe: true,
            locs: HashMap::new(),
            mesh: None,
            payload_field: None,
        };
        let f = scratch.instance(Instance {
            owner,
            subst: subst.clone(),
            closures: Vec::new(),
            inner: Some(Inner::Const(b)),
        })?;
        scratch.run()?;
        if let Some(e) = verify_module(&scratch.module).first() {
            return Err(LowerError::new(
                "E1304",
                format!("evaluating `{name}` lowered to bad IR: {e}"),
            ));
        }
        let mut results = Interpreter::new(&scratch.module)
            .call(f, &[])
            .map_err(|e| LowerError::new("E1306", format!("evaluating `{name}`: {e}")))?;
        for (k, v) in scratch.consts {
            self.consts.entry(k).or_insert(v);
        }
        for (k, v) in scratch.local_consts {
            self.local_consts.entry(k).or_insert(v);
        }
        let val = results
            .pop()
            .ok_or_else(|| LowerError::new("E1306", "an initializer without a value"))?;
        let v = ConstValue { val, ty };
        self.local_consts.insert(key, v.clone());
        Ok(v)
    }
}
