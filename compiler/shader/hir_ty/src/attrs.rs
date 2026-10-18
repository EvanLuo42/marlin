//! Attributes the compiler gives meaning to: `[Flags]` (0004 §3.5), stages (§11.1), system
//! values and interpolation modes (§11.2), `[Push]` (§11.3), `[GroupShared]` (§12.1) and
//! `[Requires]` (§12.3). Like `[Builtin]`, they are structs of the core library's `Marlin`
//! namespace and are recognized by declaration, not by spelling: a user's own
//! `FlagsAttribute` is an ordinary attribute.

use std::sync::Arc;

use marlin_front_hir_def::store::ExprId;
use marlin_front_hir_def::{FilePackage, ItemId, ItemSignature, ItemTreeQuery, Name, Resolver};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::builtins::{Builtins, BuiltinsQuery, resolve_attribute};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Compute,
    Vertex,
    Pixel,
    Mesh,
    Amplification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemValue {
    DispatchThreadId,
    GroupId,
    GroupThreadId,
    GroupIndex,
    VertexId,
    InstanceId,
    Position,
    ClipDistance,
    PrimitiveId,
    CullPrimitive,
    IsFrontFace,
    SampleIndex,
    Target,
    Depth,
    Coverage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownAttr {
    Flags,
    Stage(Stage),
    SystemValue(SystemValue),
    /// `[Flat]`, `[NoPerspective]`, `[Centroid]`, `[Sample]`.
    Interpolation,
    Push,
    GroupShared,
    Requires,
    /// `[Uniform]` on a local: its values are the same in every lane (0004 §10.3).
    Uniform,
}

impl KnownAttr {
    /// The attribute named `name` (without the `Attribute` suffix).
    pub fn from_name(name: &str) -> Option<KnownAttr> {
        use SystemValue::*;
        Some(match name {
            "Flags" => KnownAttr::Flags,
            "Compute" => KnownAttr::Stage(Stage::Compute),
            "Vertex" => KnownAttr::Stage(Stage::Vertex),
            "Pixel" => KnownAttr::Stage(Stage::Pixel),
            "Mesh" => KnownAttr::Stage(Stage::Mesh),
            "Amplification" => KnownAttr::Stage(Stage::Amplification),
            "Push" => KnownAttr::Push,
            "GroupShared" => KnownAttr::GroupShared,
            "Requires" => KnownAttr::Requires,
            "Uniform" => KnownAttr::Uniform,
            "Flat" | "NoPerspective" | "Centroid" | "Sample" => KnownAttr::Interpolation,
            _ => KnownAttr::SystemValue(match name {
                "DispatchThreadId" => DispatchThreadId,
                "GroupId" => GroupId,
                "GroupThreadId" => GroupThreadId,
                "GroupIndex" => GroupIndex,
                "VertexId" => VertexId,
                "InstanceId" => InstanceId,
                "Position" => Position,
                "ClipDistance" => ClipDistance,
                "PrimitiveId" => PrimitiveId,
                "CullPrimitive" => CullPrimitive,
                "IsFrontFace" => IsFrontFace,
                "SampleIndex" => SampleIndex,
                "Target" => Target,
                "Depth" => Depth,
                "Coverage" => Coverage,
                _ => return None,
            }),
        })
    }
}

/// One attribute written on a declaration or a parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AttributeTy {
    /// The attribute's struct.
    pub ty: ItemId,
    pub known: Option<KnownAttr>,
    /// The arguments (named ones with their name), in the declaration's signature store.
    pub args: Vec<(Option<Name>, ExprId)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ItemAttributes {
    pub item: Vec<AttributeTy>,
    pub params: Vec<Vec<AttributeTy>>,
}

impl ItemAttributes {
    pub fn find(&self, known: KnownAttr) -> Option<&AttributeTy> {
        self.item.iter().find(|a| a.known == Some(known))
    }

    pub fn has(&self, known: KnownAttr) -> bool {
        self.find(known).is_some()
    }
}

/// Which compiler-known attribute the struct `ty` is, if any.
pub fn known_attribute(
    cx: &Cx<'_>,
    builtins: &Builtins,
    ty: ItemId,
) -> Result<Option<KnownAttr>, Cancelled> {
    if builtins.core_package.is_none()
        || builtins.core_package != Some(cx.input::<FilePackage>(&ty.file)?)
    {
        return Ok(None);
    }
    let tree = cx.get::<ItemTreeQuery>(&ty.file)?;
    let data = &tree.items[ty.local];
    if data.parent.is_some() {
        return Ok(None);
    }
    Ok(data
        .name
        .as_ref()
        .and_then(|n| n.as_str().strip_suffix("Attribute"))
        .and_then(KnownAttr::from_name))
}

/// The attributes of a declaration and of its parameters, resolved. Attributes that do not
/// resolve are left out (type lowering reports them).
pub struct ItemAttributesQuery;
impl Query for ItemAttributesQuery {
    type Key = ItemId;
    type Value = Arc<ItemAttributes>;
    fn execute(cx: &Cx<'_>, item: &ItemId) -> Result<Arc<ItemAttributes>, Cancelled> {
        let sig = cx.get::<ItemSignature>(item)?;
        let package = cx.input::<FilePackage>(&item.file)?;
        let builtins = cx.get::<BuiltinsQuery>(&package)?;
        let resolver = Resolver::for_item(cx, *item)?;
        let resolve = |attributes: &[marlin_front_hir_def::store::Attribute]| {
            let mut out = Vec::new();
            for attribute in attributes {
                let Some(ty) = resolve_attribute(cx, &resolver, &attribute.path.to_def_path())?
                else {
                    continue;
                };
                out.push(AttributeTy {
                    ty,
                    known: known_attribute(cx, &builtins, ty)?,
                    args: attribute
                        .args
                        .iter()
                        .map(|a| (a.name.clone(), a.expr))
                        .collect(),
                });
            }
            Ok::<_, Cancelled>(out)
        };
        let mut out = ItemAttributes {
            item: resolve(&sig.attributes)?,
            params: Vec::new(),
        };
        for param in &sig.params {
            out.params.push(resolve(&param.attributes)?);
        }
        Ok(Arc::new(out))
    }
}

/// The compiler-known attributes among `attributes`, written inside `item`'s body (on a local
/// declaration, say). Attributes that do not resolve are left out.
pub fn body_attributes(
    cx: &Cx<'_>,
    item: ItemId,
    attributes: &[marlin_front_hir_def::store::Attribute],
) -> Result<Vec<KnownAttr>, Cancelled> {
    let package = cx.input::<FilePackage>(&item.file)?;
    let builtins = cx.get::<BuiltinsQuery>(&package)?;
    let resolver = Resolver::for_item(cx, item)?;
    let mut out = Vec::new();
    for attribute in attributes {
        let Some(ty) = resolve_attribute(cx, &resolver, &attribute.path.to_def_path())? else {
            continue;
        };
        if let Some(known) = known_attribute(cx, &builtins, ty)? {
            out.push(known);
        }
    }
    Ok(out)
}
