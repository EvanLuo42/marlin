//! Entry points (0004 §11, 0006 §6.3) and static parameters (0004 §13.3).
//!
//! The entry function itself is lowered like any other instance, with its system values and
//! `[Push]` struct as ordinary parameters. The IR entry is a function without parameters
//! around it: it loads each parameter from its module-level variable (`input` with a
//! `builtin(...)` binding, or `push`) and calls the instance. Inlining removes the call.
//! A module holds one entry; its `uses` are the module-level variables its functions reach.

use marlin_front_hir_def::store::{Expr, Literal, UnaryOp};
use marlin_front_hir_def::{
    BodyOwner, BodyPart, ItemId, ItemKind, ItemSignature, ItemTreeQuery, Resolution, Resolver,
    item_path,
};
use marlin_shaderc_hir_ty::{
    GenericArgTy, InferLoc, ItemAttributesQuery, KnownAttr, Stage, Subst, parse_int,
};
use marlin_shaderc_ir::{
    Binding, Builtin, Entry, FuncBuilder, FuncId, GlobalData, Op, Space, Stage as IrStage, Val,
};

use crate::func::{Pass, signature};
use crate::{Instance, LowerError, Lowering, R};

impl Lowering<'_, '_> {
    /// Sets the static parameter `field`, a `const` field of a struct (0004 §13.3), to `value`
    /// for everything lowered by this `Lowering`. Static parameters are set before anything is
    /// lowered.
    pub fn set_static(&mut self, field: ItemId, value: Val) -> R<()> {
        let path = item_path(self.cx, field)?;
        let tree = self.cx.get::<ItemTreeQuery>(&field.file)?;
        let data = &tree.items[field.local];
        let in_struct = data
            .parent
            .is_some_and(|p| tree.items[p].kind == ItemKind::Struct);
        if data.kind != ItemKind::Field || !data.is_const || !in_struct {
            return Err(LowerError::new(
                "E1308",
                format!("`{path}` is not a static parameter (a `const` field of a struct)"),
            ));
        }
        if !self.funcs.is_empty() || self.consts.keys().any(|k| !self.statics.contains(k)) {
            return Err(LowerError::new(
                "E1308",
                format!("`{path}` is set after lowering started"),
            ));
        }
        let types = self
            .cx
            .get::<marlin_shaderc_hir_ty::ItemTypesQuery>(&field)?;
        let ty = types.ty.clone().unwrap_or(marlin_shaderc_hir_ty::Ty::Error);
        let t = self.ty(&ty)?;
        // The value has to fit the field's type.
        self.val_const(&value, t).map_err(|_| {
            LowerError::new(
                "E1308",
                format!("this value does not fit `{path}` (`{}`)", self.show(&ty)),
            )
        })?;
        self.statics.insert(field);
        self.consts
            .insert(field, crate::ctfe::ConstValue { val: value, ty });
        Ok(())
    }

    /// Lowers the entry point `item` with `type_args` for its own generic parameters (0004
    /// §11.4) into an IR entry; the function it returns is the entry's.
    pub fn entry(&mut self, item: ItemId, type_args: &[GenericArgTy]) -> R<FuncId> {
        let owner = BodyOwner {
            item,
            part: BodyPart::Main,
        };
        self.entry_inner(item, type_args)
            .map_err(|e| e.located(owner, InferLoc::Owner))
    }

    fn entry_inner(&mut self, item: ItemId, type_args: &[GenericArgTy]) -> R<FuncId> {
        let path = item_path(self.cx, item)?;
        let attrs = self.cx.get::<ItemAttributesQuery>(&item)?;
        let Some((stage, stage_attr)) = attrs.item.iter().find_map(|a| match a.known {
            Some(KnownAttr::Stage(s)) => Some((s, a.clone())),
            _ => None,
        }) else {
            return Err(LowerError::new(
                "E1308",
                format!("`{path}` is not an entry point"),
            ));
        };
        if stage != Stage::Compute {
            return Err(LowerError::new(
                "E1301",
                format!("{stage:?} entry points are not lowered yet"),
            ));
        }
        let mut size = [1u32; 3];
        let positional: Vec<_> = stage_attr
            .args
            .iter()
            .filter(|(n, _)| n.is_none())
            .map(|(_, e)| *e)
            .collect();
        for (k, e) in positional.into_iter().enumerate().take(3) {
            let v = self.attribute_int(item, e)?;
            size[k] = u32::try_from(v).ok().filter(|&v| v > 0).ok_or_else(|| {
                LowerError::new("E1308", "a thread-group size is a positive integer")
            })?;
        }

        // The instance, with the requested type arguments.
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let generics = tree.items[item.local].generic_params.len();
        if type_args.len() != generics {
            return Err(LowerError::new(
                "E1307",
                format!(
                    "`{path}` takes {generics} generic argument(s); {} given",
                    type_args.len()
                ),
            ));
        }
        let mut subst = Subst::new();
        if generics > 0 {
            subst = subst.with_args(item, type_args);
        }
        let instance = Instance {
            owner: BodyOwner {
                item,
                part: BodyPart::Main,
            },
            subst,
            closures: Vec::new(),
            inner: None,
        };
        // The entry keeps the source name; the instance it calls gets told apart.
        let name = self.instance_base(&instance)?;
        let func = self
            .module
            .add_function(marlin_shaderc_ir::Function::new(&name, Vec::new()));
        if self.names.insert(name.clone(), func).is_some() {
            return Err(LowerError::new(
                "E1308",
                format!("two functions are named `{name}`"),
            ));
        }
        let sig = signature(self, &instance)?;
        let callee = self.instance(instance)?;

        // The parameters' module-level variables.
        let mut globals = Vec::new();
        for (i, (ty, pass)) in sig.params.iter().enumerate() {
            let param_attrs = attrs.params.get(i).map_or(&[][..], |a| &a[..]);
            let t = self.ty(ty)?;
            if *pass != Pass::Value {
                return Err(LowerError::new(
                    "E1308",
                    "an entry point's parameters are inputs, passed by value or `in`",
                ));
            }
            let pname = self
                .cx
                .get::<ItemSignature>(&item)?
                .params
                .get(i)
                .and_then(|p| p.name.clone())
                .map_or_else(|| format!("arg{i}"), |n| n.to_string());
            let (space, binding) = if param_attrs.iter().any(|a| a.known == Some(KnownAttr::Push)) {
                (Space::Push, Binding::None)
            } else {
                let sv = param_attrs.iter().find_map(|a| match a.known {
                    Some(KnownAttr::SystemValue(sv)) => Some(sv),
                    _ => None,
                });
                let Some(builtin) = sv.and_then(|sv| Builtin::from_name(&format!("{sv:?}"))) else {
                    return Err(LowerError::new(
                        "E1301",
                        "stage inputs other than system values are not lowered yet",
                    ));
                };
                (Space::Input, Binding::Builtin(builtin))
            };
            let ptr = self.module.ptr(space, false, t);
            let g = self.module.add_global(GlobalData {
                name: format!("{name}.{pname}").into(),
                ty: ptr,
                binding,
                interp: None,
                init: None,
            });
            globals.push(g);
        }

        // The entry function: load the inputs, call.
        let mut b = FuncBuilder::edit(&mut self.module, func);
        let block = b.add_block();
        b.switch_to(block);
        let mut args = Vec::new();
        for &g in &globals {
            let p = b.global(g);
            args.push(b.value(Op::Load { aligned: None }, vec![p]));
        }
        b.ins(Op::Call(callee), args);
        b.ret(Vec::new());
        self.module.entries.push(Entry {
            func,
            stage: IrStage::Compute(size),
            uses: Vec::new(),
        });
        Ok(func)
    }

    /// An integer attribute argument: a literal or a constant.
    fn attribute_int(&mut self, item: ItemId, e: marlin_front_hir_def::store::ExprId) -> R<i128> {
        let sig = self.cx.get::<ItemSignature>(&item)?;
        let (negate, x) = match &sig.store.exprs[e] {
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
            } => (true, *operand),
            _ => (false, e),
        };
        let v = match &sig.store.exprs[x] {
            Expr::Literal(Literal::Number(text)) => parse_int(text),
            Expr::Path(p) => {
                let resolver = Resolver::for_item(self.cx, item)?;
                match resolver.resolve(self.cx, &p.to_def_path(), true)? {
                    Resolution::Values(items) if items.len() == 1 => {
                        Some(self.const_int(items[0])?)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let v = v.ok_or_else(|| {
            LowerError::new(
                "E1308",
                "an attribute argument that is not an integer constant",
            )
        })?;
        Ok(if negate { -v } else { v })
    }

    /// Each entry's `uses`: the module-level variables of the functions it reaches.
    pub(crate) fn entry_uses(&mut self) {
        self.module.recompute_uses();
    }
}
