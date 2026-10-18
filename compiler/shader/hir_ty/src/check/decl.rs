//! Checks of one declaration (step 5c, E12xx):
//! - the types it writes (`types`);
//! - namespace-level variables are `[GroupShared]` (0004 §12.1);
//! - structs and extension blocks implement every required member of their interfaces (§8);
//! - entry points: one stage, its arguments, system values by stage and direction, their
//!   types, at most one `[Push]` of at most 128 bytes (§11);
//! - a `[Requires]` contract covers what the function needs (§12.3);
//! - no `[Export]` on declarations: it goes on an entry's local constants (§13.4, E1134).

use std::collections::HashSet;
use std::sync::Arc;

use marlin_front_hir_def::store::{Expr, Literal, RefKind, TypeRefId};
use marlin_front_hir_def::{
    FilePackage, ItemId, ItemKind, ItemSignature, ItemTreeQuery, Name, body_owners,
};
use marlin_lang_query::{Cancelled, Cx, Query};

use super::types::type_problems;
use crate::attrs::{AttributeTy, ItemAttributesQuery, KnownAttr, Stage, SystemValue};
use crate::builtins::{BuiltinKind, Builtins, BuiltinsQuery, Scalar};
use crate::caps::{CapabilitiesQuery, declared_requirements};
use crate::diagnostics::{TyDiagnostic, TyDiagnosticKind, TyLoc};
use crate::item::ItemTypesQuery;
use crate::layout::layout_of;
use crate::subst::Subst;
use crate::ty::{GenericArgTy, Ty};

/// The most push-constant data an entry point takes (0002 §2.1).
const MAX_PUSH_BYTES: u64 = 128;

/// The diagnostics of the declaration checks of one item.
pub struct ItemChecksQuery;
impl Query for ItemChecksQuery {
    type Key = ItemId;
    type Value = Arc<[TyDiagnostic]>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Arc<[TyDiagnostic]>, Cancelled> {
        let package = cx.input::<FilePackage>(&item.file)?;
        let mut c = Decl {
            cx,
            item: *item,
            builtins: cx.get::<BuiltinsQuery>(&package)?,
            out: Vec::new(),
        };
        c.run()?;
        Ok(c.out.into())
    }
}

struct Decl<'a, 'b> {
    cx: &'a Cx<'b>,
    item: ItemId,
    builtins: Arc<Builtins>,
    out: Vec<TyDiagnostic>,
}

/// Where a value crosses a stage boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    In,
    Out,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Compute => "compute",
            Stage::Vertex => "vertex",
            Stage::Pixel => "pixel",
            Stage::Mesh => "mesh",
            Stage::Amplification => "amplification",
        }
    }
}

impl SystemValue {
    fn name(self) -> &'static str {
        use SystemValue::*;
        match self {
            DispatchThreadId => "DispatchThreadId",
            GroupId => "GroupId",
            GroupThreadId => "GroupThreadId",
            GroupIndex => "GroupIndex",
            VertexId => "VertexId",
            InstanceId => "InstanceId",
            Position => "Position",
            ClipDistance => "ClipDistance",
            PrimitiveId => "PrimitiveId",
            CullPrimitive => "CullPrimitive",
            IsFrontFace => "IsFrontFace",
            SampleIndex => "SampleIndex",
            Target => "Target",
            Depth => "Depth",
            Coverage => "Coverage",
        }
    }

    /// The table of 0004 §11.2.
    fn allowed(self, stage: Stage, dir: Direction) -> bool {
        use Direction::*;
        use SystemValue::*;
        let compute_like = matches!(stage, Stage::Compute | Stage::Mesh | Stage::Amplification);
        match self {
            DispatchThreadId | GroupId | GroupThreadId | GroupIndex => dir == In && compute_like,
            VertexId | InstanceId => dir == In && stage == Stage::Vertex,
            Position | ClipDistance => match dir {
                Out => matches!(stage, Stage::Vertex | Stage::Mesh),
                In => stage == Stage::Pixel,
            },
            PrimitiveId => match dir {
                In => stage == Stage::Pixel,
                Out => stage == Stage::Mesh,
            },
            CullPrimitive => dir == Out && stage == Stage::Mesh,
            IsFrontFace | SampleIndex => dir == In && stage == Stage::Pixel,
            Target | Depth | Coverage => dir == Out && stage == Stage::Pixel,
        }
    }

    /// The type it must have, as written in §11.2.
    fn expected(self) -> &'static str {
        use SystemValue::*;
        match self {
            DispatchThreadId | GroupId | GroupThreadId => "`uint3`",
            GroupIndex | VertexId | InstanceId | PrimitiveId | SampleIndex | Coverage => "`uint`",
            Position => "`float4`",
            ClipDistance => "`float[N]`",
            CullPrimitive | IsFrontFace => "`bool`",
            Target => "a `float`, `int` or `uint` scalar or vector",
            Depth => "`float`",
        }
    }
}

impl Decl<'_, '_> {
    fn report(&mut self, loc: TyLoc, code: &'static str, message: String) {
        self.out.push(TyDiagnostic {
            item: self.item,
            loc,
            kind: TyDiagnosticKind::Check { code, message },
        });
    }

    fn show(&self, ty: &Ty) -> Result<String, Cancelled> {
        crate::display_ty(self.cx, &self.builtins, ty)
    }

    fn run(&mut self) -> Result<(), Cancelled> {
        let cx = self.cx;
        let tree = cx.get::<ItemTreeQuery>(&self.item.file)?;
        let data = tree.items[self.item.local].clone();
        // The compiler implements built-in declarations itself.
        if self.builtins.kind(self.item).is_some() {
            return Ok(());
        }
        let sig = cx.get::<ItemSignature>(&self.item)?;
        let types = cx.get::<ItemTypesQuery>(&self.item)?;
        let attrs = cx.get::<ItemAttributesQuery>(&self.item)?;
        if attrs.has(KnownAttr::Export) {
            self.report(
                TyLoc::Item,
                "E1134",
                "`[Export]` goes on a `const` local in an entry point's body, not on a declaration"
                    .into(),
            );
        }

        // Types as written.
        for (param, ty) in sig.params.iter().zip(&types.params) {
            self.type_use(&ty.ty, true, param.ty)?;
        }
        if let (Some(receiver), Some(ty)) = (&sig.receiver, &types.receiver) {
            self.type_use(ty, true, receiver.ty)?;
        }
        let writes_ty = matches!(
            data.kind,
            ItemKind::Field
                | ItemKind::Function
                | ItemKind::Operator
                | ItemKind::Conversion
                | ItemKind::Property
                | ItemKind::Indexer
        );
        if writes_ty && let (Some(t), Some(ty)) = (sig.ty, &types.ty) {
            self.type_use(ty, false, t)?;
        }

        let parent_kind = data.parent.map(|p| tree.items[p].kind);
        let at_namespace = matches!(parent_kind, None | Some(ItemKind::Namespace));

        // Fields: namespace-level variables, system values.
        if data.kind == ItemKind::Field {
            let shared = attrs.has(KnownAttr::GroupShared);
            if at_namespace && !data.is_const && !shared {
                self.report(
                    TyLoc::Item,
                    "E1205",
                    "a namespace-level variable must be `[GroupShared]`; a constant is `const`"
                        .into(),
                );
            }
            if shared && !at_namespace {
                self.report(
                    TyLoc::Item,
                    "E1205",
                    "`[GroupShared]` is for namespace-level fields".into(),
                );
            }
            if let (Some(t), Some(ty)) = (sig.ty, &types.ty) {
                self.system_value_types(&attrs.item, ty, t, &sig)?;
            }
        }

        if matches!(data.kind, ItemKind::Struct | ItemKind::Extension) {
            self.implementations(&data, &types.bases, &sig.bases)?;
        }

        if matches!(data.kind, ItemKind::Function | ItemKind::Constructor) {
            for (i, (param, ty)) in sig.params.iter().zip(&types.params).enumerate() {
                let param_attrs = attrs.params.get(i).map_or(&[][..], |a| &a[..]);
                self.system_value_types(param_attrs, &ty.ty, param.ty, &sig)?;
            }
            self.entry_point(&data, at_namespace, &attrs, &sig, &types)?;
            self.contract()?;
        }
        Ok(())
    }

    fn type_use(&mut self, ty: &Ty, is_param: bool, at: TypeRefId) -> Result<(), Cancelled> {
        for p in type_problems(self.cx, &self.builtins, ty, is_param)? {
            self.report(TyLoc::Type(at), p.code, p.message);
        }
        Ok(())
    }

    // ---- interfaces ----

    /// Every member an interface requires (no default body), with the substitution of the
    /// interface's parameters; through the interfaces it extends.
    fn required_members(
        &self,
        iface: ItemId,
        subst: Subst,
        seen: &mut HashSet<ItemId>,
        out: &mut Vec<(ItemId, ItemId, Subst)>,
    ) -> Result<(), Cancelled> {
        if !seen.insert(iface) {
            return Ok(());
        }
        let tree = self.cx.get::<ItemTreeQuery>(&iface.file)?;
        for &m in &tree.items[iface.local].members {
            let member = ItemId {
                file: iface.file,
                local: m,
            };
            let required = match tree.items[m].kind {
                ItemKind::Function
                | ItemKind::Operator
                | ItemKind::Property
                | ItemKind::Indexer => body_owners(self.cx, member)?.is_empty(),
                ItemKind::AssocType => self.cx.get::<crate::AliasTargetQuery>(&member)?.is_none(),
                _ => false,
            };
            if required {
                out.push((iface, member, subst.clone()));
            }
        }
        let types = self.cx.get::<ItemTypesQuery>(&iface)?;
        for base in &types.bases {
            if let Ty::Adt { item, args } = subst.ty(base) {
                let inner = Subst::new().with_args(item, &args);
                self.required_members(item, inner, seen, out)?;
            }
        }
        Ok(())
    }

    fn implementations(
        &mut self,
        data: &marlin_front_hir_def::Item,
        bases: &[Ty],
        written: &[TypeRefId],
    ) -> Result<(), Cancelled> {
        let tree = self.cx.get::<ItemTreeQuery>(&self.item.file)?;
        let own: Vec<ItemId> = data
            .members
            .iter()
            .map(|&m| ItemId {
                file: self.item.file,
                local: m,
            })
            .collect();
        for (i, base) in bases.iter().enumerate() {
            let Ty::Adt { item: iface, args } = base else {
                continue;
            };
            let itree = self.cx.get::<ItemTreeQuery>(&iface.file)?;
            if itree.items[iface.local].kind != ItemKind::Interface {
                continue;
            }
            let mut required = Vec::new();
            let subst = Subst::new().with_args(*iface, args);
            self.required_members(*iface, subst, &mut HashSet::new(), &mut required)?;
            for (owner, member, subst) in required {
                let otree = self.cx.get::<ItemTreeQuery>(&member.file)?;
                let mdata = &otree.items[member.local];
                let candidates: Vec<ItemId> = own
                    .iter()
                    .copied()
                    .filter(|o| {
                        let d = &tree.items[o.local];
                        let same_kind = match mdata.kind {
                            ItemKind::AssocType => {
                                d.kind == ItemKind::AssocType || d.kind.is_type()
                            }
                            kind => d.kind == kind && d.is_static == mdata.is_static,
                        };
                        same_kind && d.name == mdata.name
                    })
                    .collect();
                let mut matched = false;
                for c in &candidates {
                    if self.same_signature(member, &subst, *c)? {
                        matched = true;
                        break;
                    }
                }
                if matched {
                    continue;
                }
                let iname = otree.items[owner.local]
                    .name
                    .as_ref()
                    .map_or(String::new(), |n| n.to_string());
                let mname = match (&mdata.name, mdata.kind) {
                    (Some(n), _) => n.to_string(),
                    (None, ItemKind::Operator) => {
                        let msig = self.cx.get::<ItemSignature>(&member)?;
                        format!("operator {}", msig.operator.as_deref().unwrap_or("?"))
                    }
                    (None, _) => "this[]".to_string(),
                };
                let what = if candidates.is_empty() {
                    "does not implement"
                } else {
                    "has a member that does not match"
                };
                let me = data
                    .name
                    .as_ref()
                    .map_or("this extension".to_string(), |n| format!("`{n}`"));
                let at = written.get(i).map_or(TyLoc::Item, |t| TyLoc::Type(*t));
                self.report(at, "E1204", format!("{me} {what} `{iname}.{mname}`"));
            }
        }
        Ok(())
    }

    /// Whether `candidate` implements interface member `member` (whose interface parameters
    /// `subst` gives).
    fn same_signature(
        &self,
        member: ItemId,
        subst: &Subst,
        candidate: ItemId,
    ) -> Result<bool, Cancelled> {
        let mtree = self.cx.get::<ItemTreeQuery>(&member.file)?;
        if mtree.items[member.local].kind == ItemKind::AssocType {
            return Ok(true);
        }
        if mtree.items[member.local].kind == ItemKind::Operator {
            let a = self.cx.get::<ItemSignature>(&member)?;
            let b = self.cx.get::<ItemSignature>(&candidate)?;
            if a.operator != b.operator {
                return Ok(false);
            }
        }
        let ctree = self.cx.get::<ItemTreeQuery>(&candidate.file)?;
        let (m_generics, c_generics) = (
            mtree.items[member.local].generic_params.len(),
            ctree.items[candidate.local].generic_params.len(),
        );
        if m_generics != c_generics {
            return Ok(false);
        }
        // The member's own type parameters stand for the candidate's.
        let own: Vec<GenericArgTy> = match crate::self_ty(self.cx, candidate)? {
            Ty::Adt { args, .. } => args.to_vec(),
            _ => Vec::new(),
        };
        let subst = subst.clone().with_args(member, &own);
        let m = self.cx.get::<ItemTypesQuery>(&member)?;
        let c = self.cx.get::<ItemTypesQuery>(&candidate)?;
        if m.params.len() != c.params.len() {
            return Ok(false);
        }
        for (a, b) in m.params.iter().zip(&c.params) {
            if a.ref_kind != b.ref_kind || subst.ty(&a.ty) != b.ty {
                return Ok(false);
            }
        }
        Ok(match (&m.ty, &c.ty) {
            (Some(a), Some(b)) => subst.ty(a) == *b,
            (None, None) => true,
            _ => false,
        })
    }

    // ---- system values and entry points ----

    fn system_values(attrs: &[AttributeTy]) -> impl Iterator<Item = SystemValue> + '_ {
        attrs.iter().filter_map(|a| match a.known {
            Some(KnownAttr::SystemValue(sv)) => Some(sv),
            _ => None,
        })
    }

    /// The type a system value is written on.
    fn system_value_types(
        &mut self,
        attrs: &[AttributeTy],
        ty: &Ty,
        at: TypeRefId,
        sig: &marlin_front_hir_def::Signature,
    ) -> Result<(), Cancelled> {
        if ty.is_error() {
            return Ok(());
        }
        for attr in attrs {
            let Some(KnownAttr::SystemValue(sv)) = attr.known else {
                continue;
            };
            if !self.system_value_type_ok(sv, ty) {
                let shown = self.show(ty)?;
                self.report(
                    TyLoc::Type(at),
                    "E1212",
                    format!("`[{}]` must be {}, not `{shown}`", sv.name(), sv.expected()),
                );
            }
            if sv == SystemValue::Target {
                let index = attr
                    .args
                    .first()
                    .and_then(|(_, e)| int_literal(&sig.store, *e));
                if !matches!(index, Some(0..=7)) {
                    self.report(
                        TyLoc::Type(at),
                        "E1212",
                        "`[Target(i)]` takes the render target's index, 0 to 7".into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn system_value_type_ok(&self, sv: SystemValue, ty: &Ty) -> bool {
        use SystemValue::*;
        let scalar = |t: &Ty| match t {
            Ty::Adt { item, .. } => self.builtins.scalar(*item),
            _ => None,
        };
        let vector = |t: &Ty| match t {
            Ty::Adt { item, args } if self.builtins.kind(*item) == Some(BuiltinKind::Vector) => {
                match &args[..] {
                    [GenericArgTy::Type(e), GenericArgTy::Const(n)] => scalar(e).zip(n.value()),
                    _ => None,
                }
            }
            _ => None,
        };
        match sv {
            DispatchThreadId | GroupId | GroupThreadId => vector(ty) == Some((Scalar::U32, 3)),
            GroupIndex | VertexId | InstanceId | PrimitiveId | SampleIndex | Coverage => {
                scalar(ty) == Some(Scalar::U32)
            }
            Position => vector(ty) == Some((Scalar::F32, 4)),
            ClipDistance => {
                matches!(ty, Ty::Array { element, .. } if scalar(element) == Some(Scalar::F32))
            }
            CullPrimitive | IsFrontFace => scalar(ty) == Some(Scalar::Bool),
            Depth => scalar(ty) == Some(Scalar::F32),
            Target => {
                let ok = |s| matches!(s, Scalar::F32 | Scalar::I32 | Scalar::U32);
                scalar(ty).is_some_and(ok)
                    || vector(ty).is_some_and(|(s, n)| ok(s) && (1..=4).contains(&n))
            }
        }
    }

    fn entry_point(
        &mut self,
        data: &marlin_front_hir_def::Item,
        at_namespace: bool,
        attrs: &crate::ItemAttributes,
        sig: &marlin_front_hir_def::Signature,
        types: &crate::ItemTypes,
    ) -> Result<(), Cancelled> {
        let stages: Vec<(Stage, &AttributeTy)> = attrs
            .item
            .iter()
            .filter_map(|a| match a.known {
                Some(KnownAttr::Stage(s)) => Some((s, a)),
                _ => None,
            })
            .collect();
        let Some(&(stage, stage_attr)) = stages.first() else {
            // Not an entry point: no system values or push constants on its parameters.
            for (i, param) in sig.params.iter().enumerate() {
                let param_attrs = attrs.params.get(i).map_or(&[][..], |a| &a[..]);
                let misplaced = param_attrs.iter().find_map(|a| match a.known {
                    Some(KnownAttr::SystemValue(sv)) => Some(format!("[{}]", sv.name())),
                    Some(KnownAttr::Push) => Some("[Push]".to_string()),
                    _ => None,
                });
                if let Some(attr) = misplaced {
                    self.report(
                        TyLoc::Type(param.ty),
                        "E1213",
                        format!("`{attr}` belongs on a parameter of an entry point"),
                    );
                }
            }
            return Ok(());
        };
        if stages.len() > 1 {
            self.report(
                TyLoc::Item,
                "E1210",
                "an entry point has exactly one stage attribute".into(),
            );
        }
        if data.kind != ItemKind::Function || !at_namespace {
            self.report(
                TyLoc::Item,
                "E1210",
                "an entry point is a namespace-level function".into(),
            );
            return Ok(());
        }

        // The stage's own arguments: a thread-group size, or nothing.
        // `MaxVertices = 64` sets a field, as C# attributes do: not a positional argument.
        let positional: Vec<_> = stage_attr
            .args
            .iter()
            .filter(|(name, e)| {
                name.is_none() && !matches!(sig.store.exprs[*e], Expr::Assign { op: None, .. })
            })
            .map(|(_, e)| *e)
            .collect();
        let group_size = matches!(stage, Stage::Compute | Stage::Mesh | Stage::Amplification);
        let size_ok = positional.len() == 3
            && positional.iter().all(|&e| match &sig.store.exprs[e] {
                Expr::Literal(_) => int_literal(&sig.store, e).is_some_and(|v| v > 0),
                // A constant, checked when constants are evaluated.
                Expr::Path(_) => true,
                _ => false,
            });
        if group_size && !size_ok {
            self.report(
                TyLoc::Item,
                "E1210",
                format!(
                    "`[{}(x, y, z)]` takes the thread-group size: three positive integer \
                     constants",
                    capitalized(stage.name())
                ),
            );
        }
        if !group_size && !stage_attr.args.is_empty() {
            self.report(
                TyLoc::Item,
                "E1210",
                format!("`[{}]` takes no arguments", capitalized(stage.name())),
            );
        }
        if group_size
            && types
                .ty
                .as_ref()
                .is_some_and(|t| *t != Ty::Void && !t.is_error())
        {
            let at = sig.ty.map_or(TyLoc::Item, TyLoc::Type);
            self.report(
                at,
                "E1210",
                format!("a {} entry point returns `void`", stage.name()),
            );
        }

        // Parameters: inputs, push constants.
        let mut pushes = 0;
        for (i, (param, ty)) in sig.params.iter().zip(&types.params).enumerate() {
            let param_attrs = attrs.params.get(i).map_or(&[][..], |a| &a[..]);
            let at = TyLoc::Type(param.ty);
            let is_push = param_attrs.iter().any(|a| a.known == Some(KnownAttr::Push));
            if is_push {
                pushes += 1;
                if pushes > 1 {
                    self.report(
                        at,
                        "E1214",
                        "an entry point has at most one `[Push]` parameter".into(),
                    );
                } else {
                    self.push(&ty.ty, ty.ref_kind, at)?;
                }
                continue;
            }
            let svs: Vec<SystemValue> = Self::system_values(param_attrs).collect();
            if svs.is_empty() {
                self.io_fields(&ty.ty, stage, Direction::In, at)?;
            }
            for sv in svs {
                if !sv.allowed(stage, Direction::In) {
                    self.report(
                        at,
                        "E1211",
                        format!(
                            "`[{}]` is not an input of a {} entry point",
                            sv.name(),
                            stage.name()
                        ),
                    );
                }
            }
        }
        if let (Some(t), Some(ty)) = (sig.ty, &types.ty) {
            self.io_fields(ty, stage, Direction::Out, TyLoc::Type(t))?;
        }
        Ok(())
    }

    /// The system values on the fields of a stage I/O struct.
    fn io_fields(
        &mut self,
        ty: &Ty,
        stage: Stage,
        dir: Direction,
        at: TyLoc,
    ) -> Result<(), Cancelled> {
        let Ty::Adt { item, .. } = ty else {
            return Ok(());
        };
        if self.builtins.kind(*item).is_some() {
            return Ok(());
        }
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        if tree.items[item.local].kind != ItemKind::Struct {
            return Ok(());
        }
        let struct_name = tree.items[item.local]
            .name
            .as_ref()
            .map_or(String::new(), |n| n.to_string());
        for &m in &tree.items[item.local].members {
            let data = &tree.items[m];
            if data.kind != ItemKind::Field || data.is_static || data.is_const {
                continue;
            }
            let field = ItemId {
                file: item.file,
                local: m,
            };
            let attrs = self.cx.get::<ItemAttributesQuery>(&field)?;
            for sv in Self::system_values(&attrs.item) {
                if !sv.allowed(stage, dir) {
                    let field_name = data.name.as_ref().map_or(String::new(), Name::to_string);
                    let role = match dir {
                        Direction::In => "an input",
                        Direction::Out => "an output",
                    };
                    self.report(
                        at,
                        "E1211",
                        format!(
                            "`{struct_name}.{field_name}`: `[{}]` is not {role} of a {} entry \
                             point",
                            sv.name(),
                            stage.name()
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    fn push(&mut self, ty: &Ty, ref_kind: Option<RefKind>, at: TyLoc) -> Result<(), Cancelled> {
        if ty.is_error() {
            return Ok(());
        }
        let is_struct = match ty {
            Ty::Adt { item, .. } if self.builtins.kind(*item).is_none() => {
                let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
                tree.items[item.local].kind == ItemKind::Struct
            }
            _ => false,
        };
        if !is_struct || matches!(ref_kind, Some(RefKind::Out | RefKind::Ref)) {
            self.report(
                at,
                "E1214",
                "a `[Push]` parameter is a struct, passed by value or `in`".into(),
            );
            return Ok(());
        }
        let shown = self.show(ty)?;
        if super::types::holds_bool(self.cx, &self.builtins, ty, 0)? {
            self.report(
                at,
                "E1203",
                format!("`{shown}` holds a `bool`, which cannot be pushed; use `uint`"),
            );
        }
        if let Some(layout) = layout_of(self.cx, &self.builtins, ty)?
            && layout.size > MAX_PUSH_BYTES
        {
            self.report(
                at,
                "E1214",
                format!(
                    "push constants are at most {MAX_PUSH_BYTES} bytes; `{shown}` is {} bytes",
                    layout.size
                ),
            );
        }
        Ok(())
    }

    // ---- capabilities ----

    /// What the function needs, against its `[Requires]` contract.
    fn contract(&mut self) -> Result<(), Cancelled> {
        let Some(declared) = declared_requirements(self.cx, self.item)? else {
            return Ok(());
        };
        if body_owners(self.cx, self.item)?.is_empty() {
            return Ok(());
        }
        let caps = self.cx.get::<CapabilitiesQuery>(&self.item)?;
        for (cap, why) in &caps.required {
            if !declared.contains(cap) {
                self.report(
                    TyLoc::Item,
                    "E1220",
                    format!(
                        "this function needs `{cap}` (it {why}), which its `[Requires]` does \
                         not list"
                    ),
                );
            }
        }
        Ok(())
    }
}

fn capitalized(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// An integer literal's value.
fn int_literal(
    store: &marlin_front_hir_def::store::ExprStore,
    e: marlin_front_hir_def::store::ExprId,
) -> Option<i128> {
    match &store.exprs[e] {
        Expr::Literal(Literal::Number(text)) => crate::lower::parse_int(text),
        _ => None,
    }
}
