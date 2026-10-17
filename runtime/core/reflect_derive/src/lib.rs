//! `#[derive(Reflect)]` for `marlin-reflect`.
//!
//! Generates a `Reflect` impl (plus `ReflectEnum` for enums) whose `TYPE_INFO` is a constant, so
//! the whole description lives in read-only data and costs nothing at startup beyond registration.
//!
//! Attributes, all under `#[reflect(...)]`:
//!
//! | where     | key                    | meaning                                                      |
//! |-----------|------------------------|--------------------------------------------------------------|
//! | type      | `name = "a::B"`        | stable name (and so type id) instead of `module_path!()::B`  |
//! | type      | `default`              | default-construct with `Default::default()`, not field-wise  |
//! | type      | `clone`                | clone with `Clone::clone`, not field-wise                    |
//! | type      | `crate = "path"`       | path to `marlin_reflect` when it is re-exported              |
//! | variant   | `default`              | variant used by field-wise default construction (else first) |
//! | variant   | `rename`, `hidden`, `tooltip`                                                          |||
//! | field     | `rename = "n"`         | reflected (and serialized) name                              |
//! | field     | `range = 0.0..=1.0`    | editor range; bounds may be omitted (`..10`, `0..`)          |
//! | field     | `hidden`, `read_only`, `tooltip = "..."`                    | editor hints           |
//! | field     | `transient`            | reflected but never serialized                               |
//! | field     | `skip`                 | not reflected; built with `Default`, copied with `Clone`     |

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Error, Fields, Ident, LitFloat, LitInt,
    LitStr, Member, Path, Result, Token, Type, parse_quote,
};

#[proc_macro_derive(Reflect, attributes(reflect))]
pub fn derive_reflect(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    Type,
    Variant,
    Field,
}

impl Site {
    fn keys(self) -> &'static [&'static str] {
        match self {
            Site::Type => &["name", "default", "clone", "crate"],
            Site::Variant => &["rename", "hidden", "tooltip", "default"],
            Site::Field => &[
                "rename",
                "range",
                "hidden",
                "read_only",
                "tooltip",
                "transient",
                "skip",
            ],
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Site::Type => "type",
            Site::Variant => "variant",
            Site::Field => "field",
        }
    }
}

#[derive(Default)]
struct Attrs {
    name: Option<LitStr>,
    default: bool,
    clone: bool,
    krate: Option<Path>,
    rename: Option<LitStr>,
    hidden: bool,
    read_only: bool,
    tooltip: Option<LitStr>,
    transient: bool,
    skip: bool,
    range: Option<RangeAttr>,
}

struct RangeAttr {
    min: Option<f64>,
    max: Option<f64>,
    max_inclusive: bool,
}

fn parse_attrs(attrs: &[Attribute], site: Site) -> Result<Attrs> {
    let mut out = Attrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            let key = meta
                .path
                .get_ident()
                .map(Ident::to_string)
                .unwrap_or_default();
            if !site.keys().contains(&key.as_str()) {
                return Err(meta.error(format_args!(
                    "unsupported reflect attribute on a {}; expected one of: {}",
                    site.describe(),
                    site.keys().join(", ")
                )));
            }
            match key.as_str() {
                "name" => out.name = Some(meta.value()?.parse()?),
                "default" => out.default = true,
                "clone" => out.clone = true,
                "crate" => {
                    let lit: LitStr = meta.value()?.parse()?;
                    out.krate = Some(lit.parse()?);
                }
                "rename" => out.rename = Some(meta.value()?.parse()?),
                "hidden" => out.hidden = true,
                "read_only" => out.read_only = true,
                "tooltip" => out.tooltip = Some(meta.value()?.parse()?),
                "transient" => out.transient = true,
                "skip" => out.skip = true,
                "range" => out.range = Some(parse_range(meta.value()?)?),
                _ => unreachable!(),
            }
            Ok(())
        })?;
    }
    Ok(out)
}

fn parse_range(input: ParseStream) -> Result<RangeAttr> {
    let span = input.span();
    let min = parse_bound(input)?;
    let max_inclusive = if input.peek(Token![..=]) {
        input.parse::<Token![..=]>()?;
        true
    } else if input.peek(Token![..]) {
        input.parse::<Token![..]>()?;
        false
    } else {
        return Err(input.error("expected a range such as `0.0..=1.0`, `0..10` or `1..`"));
    };
    let max = parse_bound(input)?;
    if max_inclusive && max.is_none() {
        return Err(Error::new(span, "`..=` needs an upper bound"));
    }
    if let (Some(min), Some(max)) = (min, max)
        && (min > max || (min == max && !max_inclusive))
    {
        return Err(Error::new(span, "range is empty"));
    }
    Ok(RangeAttr {
        min,
        max,
        max_inclusive,
    })
}

fn parse_bound(input: ParseStream) -> Result<Option<f64>> {
    let negative = input.peek(Token![-]);
    if negative {
        input.parse::<Token![-]>()?;
    }
    let value = if input.peek(LitFloat) {
        input.parse::<LitFloat>()?.base10_parse::<f64>()?
    } else if input.peek(LitInt) {
        input.parse::<LitInt>()?.base10_parse::<f64>()?
    } else if negative {
        return Err(input.error("expected a number after `-`"));
    } else {
        return Ok(None);
    };
    Ok(Some(if negative { -value } else { value }))
}

/// Same hash as `marlin_reflect::name_hash`; used here only to reject collisions at compile time.
fn name_hash(name: &str) -> u32 {
    let h = marlin_hash::fnv1a_64(name.as_bytes());
    (h ^ (h >> 32)) as u32
}

struct FieldDef {
    member: Member,
    binding: Ident,
    ty: Type,
    name: String,
    span: Span,
    attrs: Attrs,
}

fn collect_fields(fields: &Fields) -> Result<Vec<FieldDef>> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let attrs = parse_attrs(&f.attrs, Site::Field)?;
            let member = match &f.ident {
                Some(ident) => Member::Named(ident.clone()),
                None => Member::Unnamed(i.into()),
            };
            let name = match (&attrs.rename, &f.ident) {
                (Some(rename), _) => rename.value(),
                (None, Some(ident)) => ident.to_string(),
                (None, None) => i.to_string(),
            };
            if attrs.skip && (attrs.rename.is_some() || attrs.range.is_some()) {
                return Err(Error::new(
                    f.span(),
                    "a `skip` field is not reflected; other reflect attributes have no effect",
                ));
            }
            Ok(FieldDef {
                member,
                binding: format_ident!("__f{}", i),
                ty: f.ty.clone(),
                name,
                span: f.span(),
                attrs,
            })
        })
        .collect()
}

/// Serialized data identifies fields and variants by name hash, so names must be unique and their
/// hashes must not collide.
fn check_names<'a>(items: impl Iterator<Item = (&'a str, Span)>, what: &str) -> Result<()> {
    let mut seen: Vec<(&str, u32)> = Vec::new();
    for (name, span) in items {
        let hash = name_hash(name);
        if let Some((other, _)) = seen.iter().find(|(n, h)| *n == name || *h == hash) {
            let msg = if *other == name {
                format!("duplicate reflected {what} name `{name}`")
            } else {
                format!(
                    "{what} names `{other}` and `{name}` have the same hash; rename one with \
                     `#[reflect(rename = \"...\")]`"
                )
            };
            return Err(Error::new(span, msg));
        }
        seen.push((name, hash));
    }
    Ok(())
}

fn check_generics(input: &DeriveInput) -> Result<()> {
    if let Some(lt) = input.generics.lifetimes().next() {
        return Err(Error::new(
            lt.span(),
            "Reflect requires 'static types; lifetime parameters are not supported",
        ));
    }
    if let Some(c) = input.generics.const_params().next() {
        return Err(Error::new(
            c.span(),
            "Reflect does not support const generic parameters",
        ));
    }
    Ok(())
}

fn check_repr(input: &DeriveInput) -> Result<()> {
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("repr")) {
        let mut packed = false;
        attr.parse_nested_meta(|meta| {
            packed |= meta.path.is_ident("packed");
            if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                content.parse::<TokenStream2>()?;
            }
            Ok(())
        })?;
        if packed {
            return Err(Error::new(
                attr.span(),
                "Reflect cannot be derived for packed types: references to their fields may be \
                 unaligned",
            ));
        }
    }
    Ok(())
}

fn opt_str(lit: &Option<LitStr>) -> TokenStream2 {
    match lit {
        Some(lit) => quote!(::core::option::Option::Some(#lit)),
        None => quote!(::core::option::Option::None),
    }
}

fn f64_tokens(value: Option<f64>, infinity: TokenStream2) -> TokenStream2 {
    match value {
        Some(v) => {
            let lit = proc_macro2::Literal::f64_suffixed(v);
            quote!(#lit)
        }
        None => infinity,
    }
}

fn field_info(krate: &Path, field: &FieldDef, in_struct: bool) -> TokenStream2 {
    let name = &field.name;
    let ident = match &field.member {
        Member::Named(ident) => ident.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    };
    let member = &field.member;
    let ty = &field.ty;
    let offset = if in_struct {
        quote!(::core::mem::offset_of!(Self, #member))
    } else {
        quote!(#krate::VARIANT_FIELD)
    };
    let a = &field.attrs;
    let (hidden, read_only, transient) = (a.hidden, a.read_only, a.transient);
    let tooltip = opt_str(&a.tooltip);
    let range = match &a.range {
        Some(r) => {
            let min = f64_tokens(r.min, quote!(::core::primitive::f64::NEG_INFINITY));
            let max = f64_tokens(r.max, quote!(::core::primitive::f64::INFINITY));
            let inclusive = r.max_inclusive;
            quote!(::core::option::Option::Some(#krate::RangeAttr {
                min: #min,
                max: #max,
                max_inclusive: #inclusive,
            }))
        }
        None => quote!(::core::option::Option::None),
    };
    quote! {
        #krate::FieldInfo {
            name: #name,
            ident: #ident,
            name_hash: #krate::name_hash(#name),
            offset: #offset,
            ty: #krate::type_info::<#ty>,
            attrs: #krate::FieldAttrs {
                hidden: #hidden,
                read_only: #read_only,
                transient: #transient,
                tooltip: #tooltip,
                range: #range,
            },
        }
    }
}

fn style(krate: &Path, fields: &Fields) -> TokenStream2 {
    match fields {
        Fields::Named(_) => quote!(#krate::StructStyle::Named),
        Fields::Unnamed(_) => quote!(#krate::StructStyle::Tuple),
        Fields::Unit => quote!(#krate::StructStyle::Unit),
    }
}

/// `member: value` initializers that build each field with its default.
fn default_inits(krate: &Path, fields: &[FieldDef]) -> Vec<TokenStream2> {
    fields
        .iter()
        .map(|f| {
            let (member, ty) = (&f.member, &f.ty);
            if f.attrs.skip {
                quote!(#member: ::core::default::Default::default())
            } else {
                quote!(#member: <#ty as #krate::Reflect>::reflect_default()?)
            }
        })
        .collect()
}

/// `member: value` initializers that clone each field from its pattern binding.
fn clone_inits(krate: &Path, fields: &[FieldDef]) -> Vec<TokenStream2> {
    fields
        .iter()
        .map(|f| {
            let (member, binding) = (&f.member, &f.binding);
            if f.attrs.skip {
                quote!(#member: ::core::clone::Clone::clone(#binding))
            } else {
                quote!(#member: #krate::Reflect::reflect_clone(#binding)?)
            }
        })
        .collect()
}

/// A braced pattern binding every field: `{ a: __f0, 1: __f1 }` works for all struct styles.
fn bind_all(fields: &[FieldDef]) -> TokenStream2 {
    let binds = fields.iter().map(|f| {
        let (member, binding) = (&f.member, &f.binding);
        quote!(#member: #binding)
    });
    quote!({ #(#binds),* })
}

fn expand(input: &DeriveInput) -> Result<TokenStream2> {
    let attrs = parse_attrs(&input.attrs, Site::Type)?;
    check_generics(input)?;
    check_repr(input)?;
    let krate = attrs
        .krate
        .clone()
        .unwrap_or_else(|| parse_quote!(::marlin_reflect));

    let ident = &input.ident;
    let type_params: Vec<Ident> = input
        .generics
        .type_params()
        .map(|p| p.ident.clone())
        .collect();
    let mut generics = input.generics.clone();
    let where_clause = generics.make_where_clause();
    for param in &type_params {
        where_clause
            .predicates
            .push(parse_quote!(#param: #krate::Reflect));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let path = match &attrs.name {
        Some(name) => quote!(#name),
        None => {
            let ident = ident.to_string();
            quote!(::core::concat!(::core::module_path!(), "::", #ident))
        }
    };
    let type_name = quote! {
        #krate::TypeName::Path {
            path: #path,
            args: &[#(&<#type_params as #krate::Reflect>::TYPE_NAME),*],
        }
    };

    let (kind, default_body, clone_body, extra) = match &input.data {
        Data::Struct(data) => expand_struct(&krate, &attrs, data)?,
        Data::Enum(data) => expand_enum(&krate, &attrs, input, data)?,
        Data::Union(data) => {
            return Err(Error::new(
                data.union_token.span,
                "Reflect cannot be derived for unions: the active field is not known",
            ));
        }
    };

    let extra = extra.map(|body| {
        quote! {
            #[automatically_derived]
            impl #impl_generics #krate::ReflectEnum for #ident #ty_generics #where_clause {
                #body
            }
        }
    });

    Ok(quote! {
        // SAFETY: the layout data comes from `size_of`, `align_of` and `offset_of!` on this very
        // type, and the field types are the declared ones.
        #[automatically_derived]
        unsafe impl #impl_generics #krate::Reflect for #ident #ty_generics #where_clause {
            const TYPE_NAME: #krate::TypeName = #type_name;
            const TYPE_INFO: #krate::TypeInfo = #krate::TypeInfo::new::<Self>(#kind);

            fn reflect_default() -> ::core::option::Option<Self> {
                #default_body
            }

            fn reflect_clone(&self) -> ::core::option::Option<Self> {
                #clone_body
            }
        }
        #extra
    })
}

type Expansion = (
    TokenStream2,
    TokenStream2,
    TokenStream2,
    Option<TokenStream2>,
);

fn expand_struct(krate: &Path, attrs: &Attrs, data: &DataStruct) -> Result<Expansion> {
    let fields = collect_fields(&data.fields)?;
    let reflected: Vec<&FieldDef> = fields.iter().filter(|f| !f.attrs.skip).collect();
    check_names(reflected.iter().map(|f| (f.name.as_str(), f.span)), "field")?;

    let infos = reflected.iter().map(|f| field_info(krate, f, true));
    let style = style(krate, &data.fields);
    let kind = quote! {
        #krate::TypeKind::Struct(#krate::StructInfo {
            style: #style,
            fields: &[#(#infos),*],
        })
    };

    let default_body = if attrs.default {
        quote!(::core::option::Option::Some(
            <Self as ::core::default::Default>::default()
        ))
    } else {
        let inits = default_inits(krate, &fields);
        quote!(::core::option::Option::Some(Self { #(#inits),* }))
    };
    let clone_body = if attrs.clone {
        quote!(::core::option::Option::Some(
            <Self as ::core::clone::Clone>::clone(self)
        ))
    } else {
        let pattern = bind_all(&fields);
        let inits = clone_inits(krate, &fields);
        quote! {
            let Self #pattern = self;
            ::core::option::Option::Some(Self { #(#inits),* })
        }
    };
    Ok((kind, default_body, clone_body, None))
}

fn expand_enum(
    krate: &Path,
    attrs: &Attrs,
    input: &DeriveInput,
    data: &DataEnum,
) -> Result<Expansion> {
    if data.variants.is_empty() {
        return Err(Error::new(
            input.ident.span(),
            "Reflect cannot be derived for an enum without variants: it has no values",
        ));
    }

    struct VariantDef {
        ident: Ident,
        name: String,
        span: Span,
        attrs: Attrs,
        fields: Vec<FieldDef>,
        style: TokenStream2,
    }
    let mut variants = Vec::new();
    for v in &data.variants {
        let attrs = parse_attrs(&v.attrs, Site::Variant)?;
        let fields = collect_fields(&v.fields)?;
        check_names(
            fields
                .iter()
                .filter(|f| !f.attrs.skip)
                .map(|f| (f.name.as_str(), f.span)),
            "field",
        )?;
        variants.push(VariantDef {
            ident: v.ident.clone(),
            name: attrs
                .rename
                .as_ref()
                .map_or_else(|| v.ident.to_string(), LitStr::value),
            span: v.span(),
            fields,
            style: style(krate, &v.fields),
            attrs,
        });
    }
    check_names(
        variants.iter().map(|v| (v.name.as_str(), v.span)),
        "variant",
    )?;

    let defaults: Vec<usize> = (0..variants.len())
        .filter(|&i| variants[i].attrs.default)
        .collect();
    if defaults.len() > 1 {
        return Err(Error::new(
            variants[defaults[1]].span,
            "only one variant can be `#[reflect(default)]`",
        ));
    }
    if attrs.default && !defaults.is_empty() {
        return Err(Error::new(
            variants[defaults[0]].span,
            "`#[reflect(default)]` on the type already uses `Default::default()`",
        ));
    }
    let default_index = defaults.first().copied().unwrap_or(0);

    // Discriminants can only be read with `as` when no variant carries data.
    let fieldless = data
        .variants
        .iter()
        .all(|v| matches!(v.fields, Fields::Unit));

    let infos = variants.iter().map(|v| {
        let (ident, name, style) = (&v.ident, &v.name, &v.style);
        let ident_str = ident.to_string();
        let discriminant = if fieldless {
            quote!(::core::option::Option::Some(Self::#ident as i64))
        } else {
            quote!(::core::option::Option::None)
        };
        let fields = v
            .fields
            .iter()
            .filter(|f| !f.attrs.skip)
            .map(|f| field_info(krate, f, false));
        let hidden = v.attrs.hidden;
        let tooltip = opt_str(&v.attrs.tooltip);
        quote! {
            #krate::VariantInfo {
                name: #name,
                ident: #ident_str,
                name_hash: #krate::name_hash(#name),
                discriminant: #discriminant,
                style: #style,
                fields: &[#(#fields),*],
                hidden: #hidden,
                tooltip: #tooltip,
            }
        }
    });
    let kind = quote! {
        #krate::TypeKind::Enum(#krate::EnumInfo::of::<Self>(&[#(#infos),*]))
    };

    let index_arms = variants.iter().enumerate().map(|(i, v)| {
        let ident = &v.ident;
        quote!(Self::#ident { .. } => #i)
    });
    let field_arms = |mutable: bool| {
        let arms = variants.iter().flat_map(|v| {
            let ident = &v.ident;
            v.fields
                .iter()
                .filter(|f| !f.attrs.skip)
                .enumerate()
                .map(move |(j, f)| {
                    let member = &f.member;
                    let convert = if mutable {
                        quote!(::core::ptr::from_mut(__f).cast::<u8>())
                    } else {
                        quote!(::core::ptr::from_ref(__f).cast::<u8>())
                    };
                    quote!((Self::#ident { #member: __f, .. }, #j) => #convert)
                })
        });
        let null = if mutable {
            quote!(::core::ptr::null_mut())
        } else {
            quote!(::core::ptr::null())
        };
        quote! {
            match (self, field) {
                #(#arms,)*
                _ => #null,
            }
        }
    };
    let field_ptr = field_arms(false);
    let field_ptr_mut = field_arms(true);
    let variant_arms = variants.iter().enumerate().map(|(i, v)| {
        let ident = &v.ident;
        let inits = default_inits(krate, &v.fields);
        quote!(#i => ::core::option::Option::Some(Self::#ident { #(#inits),* }))
    });
    let extra = quote! {
        fn variant_index(&self) -> usize {
            match self {
                #(#index_arms,)*
            }
        }

        fn field_ptr(&self, field: usize) -> *const u8 {
            #field_ptr
        }

        fn field_ptr_mut(&mut self, field: usize) -> *mut u8 {
            #field_ptr_mut
        }

        fn default_variant(index: usize) -> ::core::option::Option<Self> {
            match index {
                #(#variant_arms,)*
                _ => ::core::option::Option::None,
            }
        }
    };

    let default_body = if attrs.default {
        quote!(::core::option::Option::Some(
            <Self as ::core::default::Default>::default()
        ))
    } else {
        quote!(<Self as #krate::ReflectEnum>::default_variant(#default_index))
    };
    let clone_body = if attrs.clone {
        quote!(::core::option::Option::Some(
            <Self as ::core::clone::Clone>::clone(self)
        ))
    } else {
        let arms = variants.iter().map(|v| {
            let ident = &v.ident;
            let pattern = bind_all(&v.fields);
            let inits = clone_inits(krate, &v.fields);
            quote!(Self::#ident #pattern => Self::#ident { #(#inits),* })
        });
        quote! {
            ::core::option::Option::Some(match self {
                #(#arms,)*
            })
        }
    };
    Ok((kind, default_body, clone_body, Some(extra)))
}
