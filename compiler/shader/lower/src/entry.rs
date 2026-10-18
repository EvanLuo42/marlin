//! Entry points (0004 §11, 0006 §6.3) and static parameters (0004 §13.3).
//!
//! The entry function itself is lowered like any other instance, with its system values and
//! `[Push]` struct as ordinary parameters. The IR entry is a function without parameters
//! around it: it loads each parameter from its module-level variable (`input` with a
//! `builtin(...)` binding, or `push`; a stage I/O struct from one variable per field), calls
//! the instance and stores its result the same way (`output`, per field or as target 0).
//! Inlining removes the call.
//! A module holds one entry; its `uses` are the module-level variables its functions reach.

use marlin_front_hir_def::store::{Expr, Literal, UnaryOp};
use marlin_front_hir_def::{
    BodyOwner, BodyPart, ItemId, ItemKind, ItemSignature, ItemTreeQuery, Resolution, Resolver,
    item_path,
};
use marlin_shaderc_hir_ty::{
    GenericArgTy, InferLoc, ItemAttributesQuery, KnownAttr, Stage, Subst, SystemValue, Ty,
    parse_int,
};
use marlin_shaderc_ir::{
    Binding, Builtin, Entry, FuncBuilder, FuncId, GlobalData, GlobalId, Interp, Op, Space,
    Stage as IrStage, Type, TypeData, Val,
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
        let ir_stage = match stage {
            Stage::Compute => {
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
                IrStage::Compute(size)
            }
            Stage::Vertex => IrStage::Vertex,
            Stage::Pixel => IrStage::Pixel,
            _ => {
                return Err(LowerError::new(
                    "E1301",
                    format!("{stage:?} entry points are not lowered yet"),
                ));
            }
        };

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

        // The parameters' module-level variables: one per parameter, or one per field of a
        // stage I/O struct (built into the struct after loading).
        let mut inputs: Vec<Input> = Vec::new();
        let mut location = 0u32;
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
            let sv = param_attrs.iter().find_map(|a| match a.known {
                Some(KnownAttr::SystemValue(sv)) => Some(sv),
                _ => None,
            });
            let one = |l: &mut Self, space: Space, binding: Binding| -> Input {
                let ptr = l.module.ptr(space, false, t);
                Input::One(l.module.add_global(GlobalData {
                    name: format!("{name}.{pname}").into(),
                    ty: ptr,
                    binding,
                    interp: None,
                    init: None,
                }))
            };
            if param_attrs.iter().any(|a| a.known == Some(KnownAttr::Push)) {
                inputs.push(one(self, Space::Push, Binding::None));
            } else if let Some(sv) = sv {
                let builtin = Builtin::from_name(&format!("{sv:?}")).ok_or_else(|| {
                    LowerError::new("E1301", format!("`[{sv:?}]` is not lowered yet"))
                })?;
                inputs.push(one(self, Space::Input, Binding::Builtin(builtin)));
            } else if let Some(fields) = self.io_fields(ty)? {
                let mut gs = Vec::new();
                for f in fields {
                    gs.push(self.io_global(
                        &format!("{name}.{pname}"),
                        Space::Input,
                        f,
                        &mut location,
                    )?);
                }
                inputs.push(Input::Fields(t, gs));
            } else {
                return Err(LowerError::new(
                    "E1308",
                    format!(
                        "`{pname}`: an entry's input is a system value, its `[Push]` struct or a \
                         stage I/O struct"
                    ),
                ));
            }
        }

        // The result: a render target, or one variable per field of a stage I/O struct.
        let mut outputs = Vec::new();
        let mut out_fields = false;
        if let Some(rt) = &sig.result {
            let t = self.ty(rt)?;
            if let Some(fields) = self.io_fields(rt)? {
                out_fields = true;
                let mut loc = 0;
                for f in fields {
                    outputs.push(self.io_global(
                        &format!("{name}.out"),
                        Space::Output,
                        f,
                        &mut loc,
                    )?);
                }
            } else if ir_stage == IrStage::Pixel {
                let ptr = self.module.ptr(Space::Output, false, t);
                outputs.push(self.module.add_global(GlobalData {
                    name: format!("{name}.target").into(),
                    ty: ptr,
                    binding: Binding::Target(0, 0),
                    interp: None,
                    init: None,
                }));
            } else {
                return Err(LowerError::new(
                    "E1308",
                    format!("a {} entry returns a stage I/O struct", ir_stage.name()),
                ));
            }
        }

        // The entry function: load the inputs, call, store the outputs.
        let mut b = FuncBuilder::edit(&mut self.module, func);
        let block = b.add_block();
        b.switch_to(block);
        let mut args = Vec::new();
        for input in &inputs {
            match input {
                Input::One(g) => {
                    let p = b.global(*g);
                    args.push(b.value(Op::Load { aligned: None }, vec![p]));
                }
                Input::Fields(t, gs) => {
                    let mut parts = Vec::new();
                    for &g in gs {
                        let p = b.global(g);
                        parts.push(b.value(Op::Load { aligned: None }, vec![p]));
                    }
                    let inst = b
                        .ins_with(Op::Construct, parts, Some(*t))
                        .map_err(|e| LowerError::new("E1305", e))?;
                    args.push(b.f().result(inst));
                }
            }
        }
        let call = b.ins(Op::Call(callee), args);
        if !outputs.is_empty() {
            let r = b.f().result(call);
            for (k, &g) in outputs.iter().enumerate() {
                let v = if out_fields {
                    b.value(Op::ExtractField(k as u32), vec![r])
                } else {
                    r
                };
                let p = b.global(g);
                b.ins(Op::Store { aligned: None }, vec![p, v]);
            }
        }
        b.ret(Vec::new());
        self.module.entries.push(Entry {
            func,
            stage: ir_stage,
            uses: Vec::new(),
        });
        Ok(func)
    }

    /// The fields of a stage I/O struct `ty` (in IR order) with their bindings: a system value,
    /// a render target, or a location to assign; `None` if `ty` is not a struct.
    fn io_fields(&mut self, ty: &Ty) -> R<Option<Vec<IoField>>> {
        let t = self.ty(ty)?;
        let TypeData::Struct(s) = self.module.data(t).clone() else {
            return Ok(None);
        };
        let Some((_, items)) = self.structs.get(ty).cloned() else {
            return Ok(None);
        };
        let field_tys: Vec<(Box<str>, Type)> = self.module.structs[s].fields.clone();
        let mut out = Vec::new();
        for (item, (fname, ft)) in items.into_iter().zip(field_tys) {
            let attrs = self.cx.get::<ItemAttributesQuery>(&item)?;
            let mut binding = None;
            let mut interp = None;
            for a in &attrs.item {
                match a.known {
                    Some(KnownAttr::SystemValue(SystemValue::Target)) => {
                        let (positional, named) = self.attribute_args(item, &a.args)?;
                        let index = match positional.first() {
                            Some(&e) => self.attribute_int(item, e)?,
                            None => 0,
                        };
                        let source = match named.iter().find(|(n, _)| n == "Source") {
                            Some(&(_, e)) => self.attribute_int(item, e)?,
                            None => 0,
                        };
                        if !(0..=7).contains(&index) || !(0..=1).contains(&source) {
                            return Err(LowerError::new(
                                "E1308",
                                "a render target is 0 to 7, its source 0 or 1",
                            ));
                        }
                        binding = Some(Binding::Target(index as u32, source as u32));
                    }
                    Some(KnownAttr::SystemValue(sv)) => {
                        let b = Builtin::from_name(&format!("{sv:?}")).ok_or_else(|| {
                            LowerError::new("E1301", format!("`[{sv:?}]` is not lowered yet"))
                        })?;
                        binding = Some(Binding::Builtin(b));
                    }
                    Some(KnownAttr::Interpolation) => {
                        let tree = self.cx.get::<ItemTreeQuery>(&a.ty.file)?;
                        let n = tree.items[a.ty.local].name.as_ref().map(|n| n.to_string());
                        interp = match n.as_deref() {
                            Some("FlatAttribute") => Some(Interp::Flat),
                            Some("NoPerspectiveAttribute") => Some(Interp::NoPerspective),
                            Some("CentroidAttribute") => Some(Interp::Centroid),
                            Some("SampleAttribute") => Some(Interp::Sample),
                            _ => interp,
                        };
                    }
                    _ => {}
                }
            }
            out.push(IoField {
                name: fname.to_string(),
                ty: ft,
                binding,
                interp,
            });
        }
        Ok(Some(out))
    }

    /// The module variable of one stage I/O field; a field without a system value takes the
    /// next location (0004 §11.2: declaration order).
    fn io_global(
        &mut self,
        prefix: &str,
        space: Space,
        f: IoField,
        location: &mut u32,
    ) -> R<GlobalId> {
        let binding = match f.binding {
            Some(b) => b,
            None => {
                let l = *location;
                *location += 1;
                // An integer varying is not interpolated (Vulkan and D3D12 both require it).
                let int = self
                    .module
                    .element_scalar(f.ty)
                    .is_some_and(|s| !s.is_float());
                if int && f.interp != Some(Interp::Flat) {
                    return Err(LowerError::new(
                        "E1308",
                        format!(
                            "`{}` is an integer stage I/O field, which is `[Flat]`",
                            f.name
                        ),
                    ));
                }
                Binding::Location(l)
            }
        };
        let ptr = self.module.ptr(space, false, f.ty);
        Ok(self.module.add_global(GlobalData {
            name: format!("{prefix}.{}", f.name).into(),
            ty: ptr,
            binding,
            interp: f.interp,
            init: None,
        }))
    }

    /// An attribute's positional arguments, and its named ones (`Source = 1`, as C# writes
    /// fields set by an attribute, or `source: 1`) with their values.
    #[allow(clippy::type_complexity)]
    fn attribute_args(
        &self,
        item: ItemId,
        args: &[(
            Option<marlin_front_hir_def::Name>,
            marlin_front_hir_def::store::ExprId,
        )],
    ) -> R<(
        Vec<marlin_front_hir_def::store::ExprId>,
        Vec<(String, marlin_front_hir_def::store::ExprId)>,
    )> {
        let sig = self.cx.get::<ItemSignature>(&item)?;
        let (mut positional, mut named) = (Vec::new(), Vec::new());
        for (name, e) in args {
            if let Some(n) = name {
                named.push((n.to_string(), *e));
                continue;
            }
            match &sig.store.exprs[*e] {
                Expr::Assign {
                    op: None,
                    target,
                    value,
                } => match &sig.store.exprs[*target] {
                    Expr::Path(p) if p.segments.len() == 1 => {
                        named.push((p.segments[0].name.to_string(), *value));
                    }
                    _ => positional.push(*e),
                },
                _ => positional.push(*e),
            }
        }
        Ok((positional, named))
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

/// An entry parameter's module variables.
enum Input {
    One(GlobalId),
    /// A stage I/O struct of this type: one variable per field.
    Fields(Type, Vec<GlobalId>),
}

/// A field of a stage I/O struct.
struct IoField {
    name: String,
    ty: Type,
    /// `None`: a location, assigned in declaration order.
    binding: Option<Binding>,
    interp: Option<Interp>,
}
