//! Binding between syntax trees and reflected values.
//!
//! Reading starts from the type's default and overwrites what the text gives, so missing fields
//! keep their defaults. It keeps going past bad values, reporting each as a [`Diagnostic`]. Unknown
//! top-level attributes are returned untouched so tools can write them back: data written by a
//! newer schema survives a round trip through an older one.
//!
//! Writing omits every field equal to its default (relative to the enclosing value's default, as
//! reading reconstructs it), so text holds only what differs from code.
//!
//! Types marked `meta(text = "tuple")` (vectors, quaternions) are written as tuples of their
//! fields in order: `Translation = (1.0, 2.0, 3.0)`.
//!
//! Opaque values are written through their type data: a [`ReflectDynamic`] holder as the held
//! value tagged with its type's name (`Light { Intensity = 800.0 }`; the type must be in the
//! schema), a [`ReflectProxy`] type as its proxy.

use marlin_asset::Uuid;
use marlin_reflect::{
    Mut, Primitive, Ref, ReflectDynamic, ReflectProxy, StructStyle, TypeInfo, TypeKind, TypeName,
    Value, find_data,
};

use crate::ast::{Attr, Document, FileRef, LinkRef, Node, NodeKind, Object, SOURCE_SCHEME, Span};
use crate::case::{short_type_name, to_pascal};
use crate::error::Diagnostic;
use crate::schema::{LINK_TYPE, SOURCE_FILE_TYPE, Schema, UUID_TYPE, path_of};

/// How links map between UUIDs and the `path:Name` written beside them. Backed by the asset
/// index in tools; [`DocumentLinks`] covers objects in the same file.
pub trait Links {
    /// The UUID of object `name` in file `path` (relative to the asset root, no extension), for
    /// links written by hand without one.
    fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        let _ = (path, name);
        None
    }

    /// The file path and name of `target`, for writing links.
    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        let _ = target;
        None
    }
}

/// Knows nothing: links keep the path and name they were written with.
pub struct NoLinks;

impl Links for NoLinks {}

/// Objects of one document, at `path`, then `fallback` for everything else.
pub struct DocumentLinks<'a> {
    pub document: &'a Document,
    pub path: &'a str,
    pub fallback: &'a dyn Links,
}

impl Links for DocumentLinks<'_> {
    fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        if path.is_empty() || path == self.path {
            return self.document.object(name).map(|o| o.uuid);
        }
        self.fallback.resolve(path, name)
    }

    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        match self.document.object_by_uuid(target) {
            Some(o) => Some((self.path.to_owned(), o.name.clone())),
            None => self.fallback.describe(target),
        }
    }
}

/// A link met while reading: where it points and the type the field expects there.
#[derive(Clone, PartialEq, Debug)]
pub struct FoundLink {
    pub target: Uuid,
    /// Text name of `T` in the field's `Link<T>`.
    pub expected_type: String,
    pub span: Span,
}

/// An object read into its Rust type.
pub struct ReadObject {
    pub value: Value,
    pub uuid: Uuid,
    pub name: String,
    /// Top-level attributes no field matched; give them back to [`Schema::write_object`].
    pub unknown: Vec<Attr>,
    pub links: Vec<FoundLink>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A value that cannot be written as text.
#[derive(Clone, PartialEq, Eq, Debug, marlin_error::Error)]
pub enum WriteError {
    #[error("`{0}` is not a registered object type")]
    UnknownType(String),
    #[error("`{0}` has no visible structure and cannot be written")]
    Opaque(String),
    #[error("`{0}` is a map or set, which text does not support yet")]
    Collection(String),
}

struct Reader<'a> {
    schema: &'a Schema,
    links: &'a dyn Links,
    found: Vec<FoundLink>,
    diagnostics: Vec<Diagnostic>,
}

fn describe(node: &Node) -> &'static str {
    match node.kind {
        NodeKind::Bool(_) => "a boolean",
        NodeKind::Int(_) => "an integer",
        NodeKind::Float(_) => "a float",
        NodeKind::Str(_) => "a string",
        NodeKind::Ident(_) => "a name",
        NodeKind::Call(..) => "a tuple variant",
        NodeKind::Record(Some(_), _) => "a struct variant",
        NodeKind::Record(None, _) => "a struct",
        NodeKind::Tuple(_) => "a tuple",
        NodeKind::List(_) => "a list",
        NodeKind::Link(_) => "a link",
        NodeKind::File(_) => "a file link",
    }
}

fn link_target_name(info: &TypeInfo) -> String {
    match info.name {
        TypeName::Path { args: [target], .. } => match target {
            TypeName::Path { path, .. } => short_type_name(path).to_owned(),
            other => other.to_string(),
        },
        _ => String::new(),
    }
}

impl Reader<'_> {
    fn mismatch(&mut self, node: &Node, expected: impl core::fmt::Display) {
        self.diagnostics.push(Diagnostic::error(
            node.span,
            format!("expected {expected}, found {}", describe(node)),
        ));
    }

    fn read(&mut self, mut m: Mut<'_>, node: &Node) {
        let info = m.info();
        match path_of(info) {
            UUID_TYPE => {
                match &node.kind {
                    NodeKind::Str(s) => match s.parse::<Uuid>() {
                        Ok(uuid) => {
                            let _ = m.set(uuid);
                        }
                        Err(_) => self
                            .diagnostics
                            .push(Diagnostic::error(node.span, "malformed UUID")),
                    },
                    _ => self.mismatch(node, "a UUID string"),
                }
                return;
            }
            LINK_TYPE => {
                self.read_link(m, node);
                return;
            }
            SOURCE_FILE_TYPE => {
                match &node.kind {
                    NodeKind::File(FileRef { scheme, path }) if scheme == SOURCE_SCHEME => {
                        if let Some(mut field) = m.field("path") {
                            field.set_str(path);
                        }
                    }
                    _ => self.mismatch(node, "a `source:` file link"),
                }
                return;
            }
            _ => {}
        }
        match &info.kind {
            TypeKind::Primitive(p) => self.read_primitive(m, *p, node),
            TypeKind::String(_) => match &node.kind {
                NodeKind::Str(s) => {
                    m.set_str(s);
                }
                _ => self.mismatch(node, "a string"),
            },
            TypeKind::Struct(s) => match s.style {
                StructStyle::Named => match &node.kind {
                    NodeKind::Record(None, attrs) => self.read_fields(m, attrs, None),
                    NodeKind::Tuple(items)
                        if is_tuple_text(info) && items.len() == s.fields.len() =>
                    {
                        self.read_items(m, items);
                    }
                    _ if is_tuple_text(info) => {
                        self.mismatch(node, format_args!("a tuple of {}", s.fields.len()));
                    }
                    _ => self.mismatch(node, "a struct `{ … }`"),
                },
                StructStyle::Tuple if s.fields.len() == 1 => {
                    // Newtypes are transparent.
                    let inner = match &node.kind {
                        NodeKind::Tuple(items) if items.len() == 1 => &items[0],
                        _ => node,
                    };
                    if let Some((_, field)) = m.field_at(0) {
                        self.read(field, inner);
                    }
                }
                StructStyle::Tuple => match &node.kind {
                    NodeKind::Tuple(items) if items.len() == s.fields.len() => {
                        self.read_items(m, items);
                    }
                    _ => self.mismatch(node, format_args!("a tuple of {}", s.fields.len())),
                },
                StructStyle::Unit => match &node.kind {
                    NodeKind::Record(None, attrs) if attrs.is_empty() => {}
                    NodeKind::Ident(_) => {}
                    _ => self.mismatch(node, "`{}`"),
                },
            },
            TypeKind::Enum(e) => {
                let (tag, payload) = match &node.kind {
                    NodeKind::Ident(tag) => (tag, None),
                    NodeKind::Call(tag, items) => (tag, Some(Payload::Items(items))),
                    NodeKind::Record(Some(tag), attrs) => (tag, Some(Payload::Attrs(attrs))),
                    _ => {
                        self.mismatch(node, "an enum variant");
                        return;
                    }
                };
                let Some(index) = e.variants.iter().position(|v| to_pascal(v.name) == *tag) else {
                    let names: Vec<String> = e.variants.iter().map(|v| to_pascal(v.name)).collect();
                    self.diagnostics.push(Diagnostic::error(
                        node.span,
                        format!(
                            "unknown variant `{tag}`; expected one of {}",
                            names.join(", ")
                        ),
                    ));
                    return;
                };
                let variant = &e.variants[index];
                if !m.set_variant(index) {
                    self.diagnostics.push(Diagnostic::error(
                        node.span,
                        format!("variant `{tag}` cannot be default-constructed"),
                    ));
                    return;
                }
                match (variant.style, payload) {
                    (StructStyle::Unit, None) => {}
                    (StructStyle::Tuple, Some(Payload::Items(items)))
                        if items.len() == variant.fields.len() =>
                    {
                        self.read_items(m, items);
                    }
                    (StructStyle::Named, Some(Payload::Attrs(attrs))) => {
                        self.read_fields(m, attrs, None);
                    }
                    (style, _) => {
                        let expected = match style {
                            StructStyle::Unit => format!("`{tag}` without fields"),
                            StructStyle::Tuple => {
                                format!("`{tag}(…)` with {} values", variant.fields.len())
                            }
                            StructStyle::Named => format!("`{tag} {{ … }}`"),
                        };
                        self.mismatch(node, expected);
                    }
                }
            }
            TypeKind::Array(a) => match &node.kind {
                NodeKind::Tuple(items) | NodeKind::List(items) if items.len() == a.len => {
                    for (i, item) in items.iter().enumerate() {
                        if let Some(element) = m.reborrow().index(i) {
                            self.read(element, item);
                        }
                    }
                }
                _ => self.mismatch(node, format_args!("a tuple of {}", a.len)),
            },
            TypeKind::List(_) => match &node.kind {
                NodeKind::List(items) => {
                    // Every item starts from the element type's default, as the writer assumes,
                    // not from what the enclosing default held at that index.
                    if !(m.clear() && m.resize(items.len())) {
                        self.diagnostics.push(Diagnostic::error(
                            node.span,
                            "list items cannot be default-constructed",
                        ));
                        return;
                    }
                    for (i, item) in items.iter().enumerate() {
                        if let Some(element) = m.reborrow().index(i) {
                            self.read(element, item);
                        }
                    }
                }
                _ => self.mismatch(node, "a list `[ … ]`"),
            },
            TypeKind::Option(_) => match &node.kind {
                NodeKind::Ident(word) if word == "None" => {
                    m.set_none();
                }
                _ => {
                    if !m.set_some_default() {
                        self.diagnostics.push(Diagnostic::error(
                            node.span,
                            "the value cannot be default-constructed",
                        ));
                        return;
                    }
                    if let Some(Some(inner)) = m.option() {
                        self.read(inner, node);
                    }
                }
            },
            TypeKind::Tuple(t) => match &node.kind {
                NodeKind::Tuple(items) if items.len() == t.fields.len() => {
                    self.read_items(m, items);
                }
                _ => self.mismatch(node, format_args!("a tuple of {}", t.fields.len())),
            },
            // Pointers are transparent; a shared `Rc` is unshared first.
            TypeKind::Pointer(_) => match m.pointee_make_mut() {
                Some(inner) => self.read(inner, node),
                None => self.diagnostics.push(Diagnostic::error(
                    node.span,
                    format!("`{}` is shared and cannot be cloned", info.name),
                )),
            },
            TypeKind::Map(_) | TypeKind::Set(_) => self.diagnostics.push(Diagnostic::error(
                node.span,
                format!(
                    "`{}` is a map or set, which text does not support yet",
                    info.name
                ),
            )),
            TypeKind::Opaque => {
                if let Some(dynamic) = find_data::<ReflectDynamic>(info) {
                    self.read_held(m, dynamic, node);
                } else if let Some(proxy) = find_data::<ReflectProxy>(info) {
                    self.read_proxied(m, proxy, node);
                } else {
                    self.diagnostics.push(Diagnostic::error(
                        node.span,
                        format!("`{}` cannot be read from text", info.name),
                    ));
                }
            }
        }
    }

    /// A dynamic holder's value: `Type { … }`, `Type(…)`, `Type` (a unit struct, or all
    /// defaults) or `None` (empty).
    fn read_held(&mut self, m: Mut<'_>, dynamic: &ReflectDynamic, node: &Node) {
        let (tag, inner) = match &node.kind {
            NodeKind::Ident(word) if word == "None" => return,
            NodeKind::Ident(tag) => (tag, NodeKind::Record(None, Vec::new())),
            NodeKind::Record(Some(tag), attrs) => (tag, NodeKind::Record(None, attrs.clone())),
            NodeKind::Call(tag, items) if items.len() == 1 => (tag, items[0].kind.clone()),
            NodeKind::Call(tag, items) => (tag, NodeKind::Tuple(items.clone())),
            _ => {
                self.mismatch(node, "a typed value `Type { … }`");
                return;
            }
        };
        let Some(held) = self
            .schema
            .object_type(tag)
            .filter(|&info| dynamic.accepts(info))
        else {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                format!("`{tag}` is not a type that can be used here"),
            ));
            return;
        };
        let Some(mut value) = Value::default_of(held) else {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                format!("`{tag}` cannot be default-constructed"),
            ));
            return;
        };
        self.read(
            value.as_mut(),
            &Node {
                kind: inner,
                span: node.span,
            },
        );
        if dynamic.hold(m, value).is_err() {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                format!("`{tag}` cannot be held here"),
            ));
        }
    }

    fn read_proxied(&mut self, m: Mut<'_>, proxy: &ReflectProxy, node: &Node) {
        let info = m.info();
        // Start from the current value, so a struct proxy keeps fields the text omits.
        let Some(mut value) = proxy
            .to_proxy(m.as_ref())
            .or_else(|| Value::default_of((proxy.proxy)()))
        else {
            return;
        };
        self.read(value.as_mut(), node);
        if !proxy.from_proxy(m, value.as_ref()) {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                format!("not a valid `{}`", info.name),
            ));
        }
    }

    fn read_link(&mut self, mut m: Mut<'_>, node: &Node) {
        let expected_type = link_target_name(m.info());
        let target = match &node.kind {
            NodeKind::Ident(word) if word == "None" => Uuid::NIL,
            NodeKind::Link(LinkRef { path, name, uuid }) => match uuid {
                Some(uuid) => *uuid,
                None => match self.links.resolve(path, name) {
                    Some(uuid) => uuid,
                    None => {
                        self.diagnostics.push(Diagnostic::error(
                            node.span,
                            format!("link to `{path}:{name}` has no UUID and does not resolve"),
                        ));
                        return;
                    }
                },
            },
            _ => {
                self.mismatch(node, "a link `<path:Name #uuid>` or `None`");
                return;
            }
        };
        if let Some(mut field) = m.reborrow().field("target") {
            let _ = field.set(target);
        }
        if !target.is_nil() {
            self.found.push(FoundLink {
                target,
                expected_type,
                span: node.span,
            });
        }
    }

    fn read_items(&mut self, mut m: Mut<'_>, items: &[Node]) {
        for (i, item) in items.iter().enumerate() {
            if let Some((_, field)) = m.reborrow().field_at(i) {
                self.read(field, item);
            }
        }
    }

    /// Named fields of a struct or struct variant. `unknown` collects unmatched attributes;
    /// without it they are reported.
    fn read_fields(&mut self, mut m: Mut<'_>, attrs: &[Attr], mut unknown: Option<&mut Vec<Attr>>) {
        let fields = m.as_ref().fields_info();
        for attr in attrs {
            let index = self
                .schema
                .find_field(fields, &attr.key)
                .filter(|&i| !fields[i].attrs.transient);
            match index {
                Some(i) => {
                    if let Some((_, field)) = m.reborrow().field_at(i) {
                        self.read(field, &attr.value);
                    }
                }
                None => match unknown.as_deref_mut() {
                    Some(list) => {
                        self.diagnostics.push(Diagnostic::warning(
                            attr.span,
                            format!("unknown attribute `{}` (kept as is)", attr.key),
                        ));
                        list.push(attr.clone());
                    }
                    None => self.diagnostics.push(Diagnostic::error(
                        attr.span,
                        format!("unknown attribute `{}`", attr.key),
                    )),
                },
            }
        }
    }

    fn read_primitive(&mut self, mut m: Mut<'_>, p: Primitive, node: &Node) {
        macro_rules! int {
            ($ty:ty) => {{
                let value = match &node.kind {
                    NodeKind::Int(v) => <$ty>::try_from(*v).ok(),
                    // Values beyond i128 are written as decimal strings.
                    NodeKind::Str(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                        s.parse::<$ty>().ok()
                    }
                    _ => {
                        self.mismatch(node, "an integer");
                        return;
                    }
                };
                match value {
                    Some(v) => {
                        let _ = m.set(v);
                    }
                    None => self.diagnostics.push(Diagnostic::error(
                        node.span,
                        format!("out of range for {}", stringify!($ty)),
                    )),
                }
            }};
        }
        let float = |node: &Node| match node.kind {
            NodeKind::Float(v) => Some(v),
            NodeKind::Int(v) => Some(v as f64),
            _ => None,
        };
        match p {
            Primitive::Bool => match node.kind {
                NodeKind::Bool(b) => {
                    let _ = m.set(b);
                }
                _ => self.mismatch(node, "`true` or `false`"),
            },
            Primitive::Char => match &node.kind {
                NodeKind::Str(s) if s.chars().count() == 1 => {
                    let _ = m.set(s.chars().next().unwrap_or_default());
                }
                _ => self.mismatch(node, "a one-character string"),
            },
            Primitive::U8 => int!(u8),
            Primitive::U16 => int!(u16),
            Primitive::U32 => int!(u32),
            Primitive::U64 => int!(u64),
            Primitive::U128 => int!(u128),
            Primitive::Usize => int!(usize),
            Primitive::I8 => int!(i8),
            Primitive::I16 => int!(i16),
            Primitive::I32 => int!(i32),
            Primitive::I64 => int!(i64),
            Primitive::I128 => int!(i128),
            Primitive::Isize => int!(isize),
            Primitive::F32 => match float(node) {
                Some(v) => {
                    let _ = m.set(v as f32);
                }
                None => self.mismatch(node, "a number"),
            },
            Primitive::F64 => match float(node) {
                Some(v) => {
                    let _ = m.set(v);
                }
                None => self.mismatch(node, "a number"),
            },
        }
    }
}

enum Payload<'a> {
    Items(&'a [Node]),
    Attrs(&'a [Attr]),
}

struct Writer<'a> {
    schema: &'a Schema,
    links: &'a dyn Links,
}

impl Writer<'_> {
    /// `default` is the value reading would start from; fields equal to it are left out.
    fn node(&self, r: Ref<'_>, default: Option<Ref<'_>>) -> Result<Node, WriteError> {
        let info = r.info();
        match path_of(info) {
            UUID_TYPE => {
                let uuid = r.downcast::<Uuid>().copied().unwrap_or_default();
                return Ok(Node::new(NodeKind::Str(uuid.to_string())));
            }
            LINK_TYPE => {
                let target = r
                    .field("target")
                    .and_then(|t| t.downcast::<Uuid>().copied())
                    .unwrap_or_default();
                if target.is_nil() {
                    return Ok(Node::new(NodeKind::Ident("None".into())));
                }
                let (path, name) = self
                    .links
                    .describe(target)
                    .unwrap_or_else(|| ("?".into(), "?".into()));
                return Ok(Node::new(NodeKind::Link(LinkRef {
                    path,
                    name,
                    uuid: Some(target),
                })));
            }
            SOURCE_FILE_TYPE => {
                let path = r.field("path").and_then(Ref::as_str).unwrap_or_default();
                return Ok(Node::new(NodeKind::File(FileRef {
                    scheme: SOURCE_SCHEME.into(),
                    path: path.to_owned(),
                })));
            }
            _ => {}
        }
        let kind = match &info.kind {
            TypeKind::Primitive(p) => primitive(r, *p),
            TypeKind::String(_) => NodeKind::Str(r.as_str().unwrap_or_default().to_owned()),
            TypeKind::Struct(s) => match s.style {
                StructStyle::Named if is_tuple_text(info) => NodeKind::Tuple(self.items(r, None)?),
                StructStyle::Named => NodeKind::Record(None, self.fields(r, default)?),
                StructStyle::Tuple if s.fields.len() == 1 => {
                    let field_default = default.and_then(|d| d.field_at(0)).map(|(_, d)| d);
                    match r.field_at(0) {
                        Some((_, field)) => return self.node(field, field_default),
                        None => NodeKind::Tuple(Vec::new()),
                    }
                }
                StructStyle::Tuple => NodeKind::Tuple(self.items(r, default)?),
                StructStyle::Unit => NodeKind::Record(None, Vec::new()),
            },
            TypeKind::Enum(_) => {
                let Some((index, variant)) = r.variant() else {
                    return Err(WriteError::Opaque(info.name.to_string()));
                };
                let tag = to_pascal(variant.name);
                // Reading switches to the variant with default fields; compare against that.
                let mut variant_default = Value::default_of(info);
                if let Some(v) = variant_default.as_mut()
                    && !v.as_mut().set_variant(index)
                {
                    variant_default = None;
                }
                let default = variant_default.as_ref().map(Value::as_ref);
                match variant.style {
                    StructStyle::Unit => NodeKind::Ident(tag),
                    StructStyle::Tuple => NodeKind::Call(tag, self.items(r, default)?),
                    StructStyle::Named => NodeKind::Record(Some(tag), self.fields(r, default)?),
                }
            }
            TypeKind::Array(a) => {
                let mut items = Vec::with_capacity(a.len);
                for i in 0..a.len {
                    if let Some(element) = r.index(i) {
                        let d = default.and_then(|d| d.index(i));
                        items.push(self.node(element, d)?);
                    }
                }
                NodeKind::Tuple(items)
            }
            TypeKind::List(l) => {
                // New items start from the element type's default.
                let element_default = Value::default_of((l.element)());
                let d = element_default.as_ref().map(Value::as_ref);
                let len = r.len().unwrap_or(0);
                let mut items = Vec::with_capacity(len);
                for i in 0..len {
                    if let Some(element) = r.index(i) {
                        items.push(self.node(element, d)?);
                    }
                }
                NodeKind::List(items)
            }
            TypeKind::Option(o) => match r.option().flatten() {
                None => NodeKind::Ident("None".into()),
                Some(inner) => {
                    let inner_default = Value::default_of((o.some)());
                    return self.node(inner, inner_default.as_ref().map(Value::as_ref));
                }
            },
            TypeKind::Tuple(_) => NodeKind::Tuple(self.items(r, default)?),
            TypeKind::Pointer(_) => match r.pointee() {
                Some(inner) => return self.node(inner, default.and_then(Ref::pointee)),
                None => return Err(WriteError::Opaque(info.name.to_string())),
            },
            TypeKind::Map(_) | TypeKind::Set(_) => {
                return Err(WriteError::Collection(info.name.to_string()));
            }
            TypeKind::Opaque => {
                if let Some(dynamic) = find_data::<ReflectDynamic>(info) {
                    return self.held_node(dynamic, r);
                }
                if let Some(proxy) = find_data::<ReflectProxy>(info) {
                    let value = proxy
                        .to_proxy(r)
                        .ok_or_else(|| WriteError::Opaque(info.name.to_string()))?;
                    let default = default.and_then(|d| proxy.to_proxy(d));
                    return self.node(value.as_ref(), default.as_ref().map(Value::as_ref));
                }
                return Err(WriteError::Opaque(info.name.to_string()));
            }
        };
        Ok(Node::new(kind))
    }

    /// A dynamic holder: the held value tagged with its type's text name, its fields relative to
    /// that type's default.
    fn held_node(&self, dynamic: &ReflectDynamic, r: Ref<'_>) -> Result<Node, WriteError> {
        let Some(held) = dynamic.held(r) else {
            return Ok(Node::new(NodeKind::Ident("None".into())));
        };
        let info = held.info();
        let name = Schema::text_name(info);
        if !self
            .schema
            .object_type(name)
            .is_some_and(|t| t.same_type(info))
        {
            return Err(WriteError::UnknownType(info.name.to_string()));
        }
        let default = Value::default_of(info);
        let node = self.node(held, default.as_ref().map(Value::as_ref))?;
        let name = name.to_owned();
        Ok(Node::new(match node.kind {
            NodeKind::Record(None, attrs) if attrs.is_empty() => NodeKind::Ident(name),
            NodeKind::Record(None, attrs) => NodeKind::Record(Some(name), attrs),
            NodeKind::Tuple(items) => NodeKind::Call(name, items),
            other => NodeKind::Call(name, vec![Node::new(other)]),
        }))
    }

    fn items(&self, r: Ref<'_>, default: Option<Ref<'_>>) -> Result<Vec<Node>, WriteError> {
        let mut items = Vec::new();
        for (i, (_, field)) in r.fields().enumerate() {
            let d = default.and_then(|d| d.field_at(i)).map(|(_, d)| d);
            items.push(self.node(field, d)?);
        }
        Ok(items)
    }

    fn fields(&self, r: Ref<'_>, default: Option<Ref<'_>>) -> Result<Vec<Attr>, WriteError> {
        let fields = r.fields_info();
        let mut attrs = Vec::new();
        for (i, (info, field)) in r.fields().enumerate() {
            if info.attrs.transient {
                continue;
            }
            let field_default = default.and_then(|d| d.field_at(i)).map(|(_, d)| d);
            let node = self.node(field, field_default)?;
            if let Some(d) = field_default
                && self.node(d, None)? == node
            {
                continue;
            }
            attrs.push(Attr {
                key: self.schema.field_name(fields, i),
                value: node,
                span: Span::default(),
            });
        }
        Ok(attrs)
    }
}

fn primitive(r: Ref<'_>, p: Primitive) -> NodeKind {
    macro_rules! get {
        ($ty:ty) => {
            r.downcast::<$ty>().copied().unwrap_or_default()
        };
    }
    match p {
        Primitive::Bool => NodeKind::Bool(get!(bool)),
        Primitive::Char => NodeKind::Str(get!(char).to_string()),
        Primitive::U8 => NodeKind::Int(get!(u8).into()),
        Primitive::U16 => NodeKind::Int(get!(u16).into()),
        Primitive::U32 => NodeKind::Int(get!(u32).into()),
        Primitive::U64 => NodeKind::Int(get!(u64).into()),
        Primitive::U128 => {
            let v = get!(u128);
            match i128::try_from(v) {
                Ok(v) => NodeKind::Int(v),
                Err(_) => NodeKind::Str(v.to_string()),
            }
        }
        Primitive::Usize => NodeKind::Int(get!(usize) as i128),
        Primitive::I8 => NodeKind::Int(get!(i8).into()),
        Primitive::I16 => NodeKind::Int(get!(i16).into()),
        Primitive::I32 => NodeKind::Int(get!(i32).into()),
        Primitive::I64 => NodeKind::Int(get!(i64).into()),
        Primitive::I128 => NodeKind::Int(get!(i128)),
        Primitive::Isize => NodeKind::Int(get!(isize) as i128),
        // Through f32's shortest text, so 0.1f32 is written `0.1`, not its f64 expansion.
        Primitive::F32 => NodeKind::Float(
            format!("{:?}", get!(f32))
                .parse()
                .unwrap_or(f64::from(get!(f32))),
        ),
        Primitive::F64 => NodeKind::Float(get!(f64)),
    }
}

impl Schema {
    /// Reads `object` into a value of its registered type, applying schema upgrades first.
    ///
    /// Fails only when the object cannot be read at all: an unknown type, a version newer than
    /// the schema, a missing upgrade, or a type without a default. Bad values are diagnostics.
    pub fn read_object(
        &self,
        object: &Object,
        links: &dyn Links,
    ) -> Result<ReadObject, Diagnostic> {
        let ty = self.types.get(&object.type_name).ok_or_else(|| {
            Diagnostic::error(
                object.span,
                format!("unknown object type `{}`", object.type_name),
            )
        })?;
        let upgraded;
        let object = if object.type_version == ty.version {
            object
        } else if object.type_version > ty.version {
            return Err(Diagnostic::error(
                object.span,
                format!(
                    "`{}` is at schema version {}, newer than this tool's {}",
                    object.type_name, object.type_version, ty.version
                ),
            ));
        } else {
            let mut copy = object.clone();
            for from in object.type_version..ty.version {
                let upgrade = ty.upgrades[from as usize].ok_or_else(|| {
                    Diagnostic::error(
                        object.span,
                        format!("no upgrade for `{}` from version {from}", object.type_name),
                    )
                })?;
                upgrade(&mut copy).map_err(|e| {
                    Diagnostic::error(
                        object.span,
                        format!("upgrading `{}` from version {from}: {e}", object.type_name),
                    )
                })?;
                copy.type_version = from + 1;
            }
            upgraded = copy;
            &upgraded
        };

        let mut value = Value::default_of(ty.info).ok_or_else(|| {
            Diagnostic::error(
                object.span,
                format!("`{}` cannot be default-constructed", object.type_name),
            )
        })?;
        let mut reader = Reader {
            schema: self,
            links,
            found: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut unknown = Vec::new();
        reader.read_fields(value.as_mut(), &object.attrs, Some(&mut unknown));
        Ok(ReadObject {
            value,
            uuid: object.uuid,
            name: object.name.clone(),
            unknown,
            links: reader.found,
            diagnostics: reader.diagnostics,
        })
    }

    /// The object block for `value`, a registered object type. `unknown` (from
    /// [`read_object`](Self::read_object)) is appended unchanged. Links are written with the path
    /// and name `links` gives; same-file targets get an empty path when `links` reports
    /// `this_path`.
    pub fn write_object(
        &self,
        value: Ref<'_>,
        uuid: Uuid,
        name: &str,
        unknown: &[Attr],
        links: &dyn Links,
        this_path: &str,
    ) -> Result<Object, WriteError> {
        let info = value.info();
        let type_name = Schema::text_name(info);
        let ty = self
            .types
            .get(type_name)
            .filter(|t| t.info.same_type(info))
            .ok_or_else(|| WriteError::UnknownType(info.name.to_string()))?;
        let local = LocalPaths {
            inner: links,
            this_path,
        };
        let writer = Writer {
            schema: self,
            links: &local,
        };
        let default = Value::default_of(info);
        let mut attrs = writer.fields(value, default.as_ref().map(Value::as_ref))?;
        attrs.extend(unknown.iter().cloned());
        Ok(Object {
            type_name: type_name.to_owned(),
            uuid,
            name: name.to_owned(),
            type_version: ty.version,
            attrs,
            span: Span::default(),
        })
    }

    /// The text form of any reflected value (no default omission at the top), for tools that
    /// edit single attributes.
    pub fn write_value(&self, value: Ref<'_>, links: &dyn Links) -> Result<Node, WriteError> {
        Writer {
            schema: self,
            links,
        }
        .node(value, None)
    }

    /// Reads one value of `value`'s type from `node`, in place.
    pub fn read_value(&self, value: Mut<'_>, node: &Node, links: &dyn Links) -> Vec<Diagnostic> {
        let mut reader = Reader {
            schema: self,
            links,
            found: Vec::new(),
            diagnostics: Vec::new(),
        };
        reader.read(value, node);
        reader.diagnostics
    }
}

/// Turns links into the current file into local (`<:Name …>`) links.
struct LocalPaths<'a> {
    inner: &'a dyn Links,
    this_path: &'a str,
}

impl Links for LocalPaths<'_> {
    fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        self.inner.resolve(path, name)
    }

    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        let (path, name) = self.inner.describe(target)?;
        Some((
            if path == self.this_path {
                String::new()
            } else {
                path
            },
            name,
        ))
    }
}

/// Types marked `meta(text = "tuple")` (vectors, quaternions) are written as a tuple of their
/// fields, `(1.0, 2.0, 3.0)`; reading takes that form too.
fn is_tuple_text(info: &TypeInfo) -> bool {
    info.attrs.meta_value("text") == Some("tuple")
}
