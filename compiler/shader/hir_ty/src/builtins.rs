//! The built-in declarations of the core library (0004 §17): types marked
//! `[Builtin("name")]` in namespace `Marlin`, and the two attributes themselves.

use std::collections::BTreeMap;
use std::sync::Arc;

use marlin_front_hir_def::store::{Expr, Literal};
use marlin_front_hir_def::{
    FilePackage, ItemId, ItemKind, ItemSignature, ItemTreeQuery, Name, NamespacePath,
    PackageDefMap, PackageId, Path, PathSegment, Resolution, Resolver, VisiblePackages,
};
use marlin_lang_query::{Cancelled, Cx, Query};

use crate::diagnostics::{TyDiagnostic, TyDiagnosticKind, TyLoc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scalar {
    Bool,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F16,
    F32,
    F64,
}

impl Scalar {
    pub const ALL: [Scalar; 10] = [
        Scalar::Bool,
        Scalar::I16,
        Scalar::U16,
        Scalar::I32,
        Scalar::U32,
        Scalar::I64,
        Scalar::U64,
        Scalar::F16,
        Scalar::F32,
        Scalar::F64,
    ];

    /// The language keyword for this scalar.
    pub fn keyword(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::I16 => "short",
            Scalar::U16 => "ushort",
            Scalar::I32 => "int",
            Scalar::U32 => "uint",
            Scalar::I64 => "long",
            Scalar::U64 => "ulong",
            Scalar::F16 => "half",
            Scalar::F32 => "float",
            Scalar::F64 => "double",
        }
    }

    /// The name in `[Builtin("...")]`.
    pub fn builtin_name(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::I16 => "i16",
            Scalar::U16 => "u16",
            Scalar::I32 => "i32",
            Scalar::U32 => "u32",
            Scalar::I64 => "i64",
            Scalar::U64 => "u64",
            Scalar::F16 => "f16",
            Scalar::F32 => "f32",
            Scalar::F64 => "f64",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resource {
    Uniform,
    Buffer,
    ReadOnlyBuffer,
    ByteBuffer,
    ReadOnlyByteBuffer,
    Texture2D,
    Texture2DArray,
    TextureCube,
    StorageTexture2D,
    StorageTexture2DArray,
    Sampler,
    ComparisonSampler,
    AccelerationStructure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinKind {
    Scalar(Scalar),
    Vector,
    Matrix,
    Resource(Resource),
    /// `Func<..., TResult>` of any arity.
    Func,
    /// `Action<...>` of any arity.
    Action,
    /// `string`, which exists only in attribute declarations (0004 §1).
    String,
}

impl BuiltinKind {
    pub fn from_name(name: &str) -> Option<BuiltinKind> {
        use Resource::*;
        if let Some(s) = Scalar::ALL.into_iter().find(|s| s.builtin_name() == name) {
            return Some(BuiltinKind::Scalar(s));
        }
        Some(match name {
            "vector" => BuiltinKind::Vector,
            "matrix" => BuiltinKind::Matrix,
            "func" => BuiltinKind::Func,
            "action" => BuiltinKind::Action,
            "string" => BuiltinKind::String,
            "uniform" => BuiltinKind::Resource(Uniform),
            "buffer" => BuiltinKind::Resource(Buffer),
            "readonly_buffer" => BuiltinKind::Resource(ReadOnlyBuffer),
            "byte_buffer" => BuiltinKind::Resource(ByteBuffer),
            "readonly_byte_buffer" => BuiltinKind::Resource(ReadOnlyByteBuffer),
            "texture2d" => BuiltinKind::Resource(Texture2D),
            "texture2d_array" => BuiltinKind::Resource(Texture2DArray),
            "texture_cube" => BuiltinKind::Resource(TextureCube),
            "storage_texture2d" => BuiltinKind::Resource(StorageTexture2D),
            "storage_texture2d_array" => BuiltinKind::Resource(StorageTexture2DArray),
            "sampler" => BuiltinKind::Resource(Sampler),
            "comparison_sampler" => BuiltinKind::Resource(ComparisonSampler),
            "acceleration_structure" => BuiltinKind::Resource(AccelerationStructure),
            _ => return None,
        })
    }
}

/// The built-ins a package can see.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Builtins {
    /// The package that declares `Marlin.BuiltinAttribute`: the core library.
    pub core_package: Option<PackageId>,
    pub builtin_attribute: Option<ItemId>,
    pub intrinsic_attribute: Option<ItemId>,
    /// `Marlin.IAttribute`, which attribute structs implement.
    pub attribute_interface: Option<ItemId>,
    pub kinds: BTreeMap<ItemId, BuiltinKind>,
    pub items: BTreeMap<BuiltinKind, Vec<ItemId>>,
    pub diagnostics: Vec<TyDiagnostic>,
}

impl Builtins {
    pub fn kind(&self, item: ItemId) -> Option<BuiltinKind> {
        self.kinds.get(&item).copied()
    }

    /// The declaration of a non-generic built-in such as a scalar.
    pub fn item(&self, kind: BuiltinKind) -> Option<ItemId> {
        self.items
            .get(&kind)
            .and_then(|items| items.first().copied())
    }

    pub fn scalar(&self, item: ItemId) -> Option<Scalar> {
        match self.kind(item)? {
            BuiltinKind::Scalar(s) => Some(s),
            _ => None,
        }
    }
}

fn marlin() -> NamespacePath {
    NamespacePath::new([Name::new("Marlin")])
}

/// The first type named `name` in namespace `Marlin` of the visible packages.
fn marlin_type(cx: &Cx<'_>, package: PackageId, name: &str) -> Result<Option<ItemId>, Cancelled> {
    for &p in cx.get::<VisiblePackages>(&package)?.iter() {
        let map = cx.get::<PackageDefMap>(&p)?;
        if let Some(items) = map
            .namespace(&marlin())
            .and_then(|ns| ns.types.get(&(Name::new(name), 0)))
        {
            return Ok(Some(items[0]));
        }
    }
    Ok(None)
}

/// Resolves an attribute's path like C#: `X` names `XAttribute` if there is one, else `X`.
pub(crate) fn resolve_attribute(
    cx: &Cx<'_>,
    resolver: &Resolver,
    path: &Path,
) -> Result<Option<ItemId>, Cancelled> {
    let mut suffixed = path.clone();
    if let Some(last) = suffixed.segments.last_mut() {
        *last = PathSegment {
            name: Name::new(&format!("{}Attribute", last.name)),
            arity: last.arity,
        };
    }
    for candidate in [&suffixed, path] {
        if let Resolution::Type(item) = resolver.resolve(cx, candidate, false)? {
            return Ok(Some(item));
        }
    }
    Ok(None)
}

/// The string literal argument of an attribute, unquoted.
pub(crate) fn string_arg(
    store: &marlin_front_hir_def::store::ExprStore,
    expr: marlin_front_hir_def::store::ExprId,
) -> Option<String> {
    match &store.exprs[expr] {
        Expr::Literal(Literal::String(text)) => {
            let text = text.strip_prefix('@').unwrap_or(text);
            Some(text.strip_prefix('"')?.strip_suffix('"')?.to_string())
        }
        _ => None,
    }
}

pub struct BuiltinsQuery;
impl Query for BuiltinsQuery {
    type Key = PackageId;
    type Value = Arc<Builtins>;
    fn execute(cx: &Cx<'_>, package: &PackageId) -> Result<Arc<Builtins>, Cancelled> {
        let mut builtins = Builtins {
            builtin_attribute: marlin_type(cx, *package, "BuiltinAttribute")?,
            intrinsic_attribute: marlin_type(cx, *package, "IntrinsicAttribute")?,
            attribute_interface: marlin_type(cx, *package, "IAttribute")?,
            ..Builtins::default()
        };
        let Some(builtin_attribute) = builtins.builtin_attribute else {
            return Ok(Arc::new(builtins));
        };
        builtins.core_package = Some(cx.input::<FilePackage>(&builtin_attribute.file)?);

        for &p in cx.get::<VisiblePackages>(package)?.iter() {
            let map = cx.get::<PackageDefMap>(&p)?;
            let Some(ns) = map.namespace(&marlin()) else {
                continue;
            };
            for items in ns.types.values() {
                let item = items[0];
                let tree = cx.get::<ItemTreeQuery>(&item.file)?;
                if tree.items[item.local].kind == ItemKind::TypeAlias {
                    continue;
                }
                let sig = cx.get::<ItemSignature>(&item)?;
                if sig.attributes.is_empty() {
                    continue;
                }
                let resolver = Resolver::for_item(cx, item)?;
                for attribute in &sig.attributes {
                    let path = attribute.path.to_def_path();
                    if resolve_attribute(cx, &resolver, &path)? != Some(builtin_attribute) {
                        continue;
                    }
                    let name = attribute
                        .args
                        .first()
                        .and_then(|a| string_arg(&sig.store, a.expr));
                    match name.as_deref().and_then(BuiltinKind::from_name) {
                        Some(kind) => {
                            builtins.kinds.insert(item, kind);
                            builtins.items.entry(kind).or_default().push(item);
                        }
                        None => builtins.diagnostics.push(TyDiagnostic {
                            item,
                            loc: TyLoc::Item,
                            kind: TyDiagnosticKind::UnknownBuiltin {
                                name: name.unwrap_or_default(),
                            },
                        }),
                    }
                }
            }
        }
        Ok(Arc::new(builtins))
    }
}
