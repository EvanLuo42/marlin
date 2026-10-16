//! `#[derive(Error)]` and `#[derive(ErrorKind)]`: the engine's `thiserror`.
//!
//! The generated code names only `::core`, so it works in every no_std crate, `marlin-pal`
//! included. There is no runtime half: printing source chains and reporting fatal errors belong
//! to `marlin-diag` (`chain`, `fatal!`, `OrFatal`).
//!
//! # `#[derive(Error)]`
//!
//! Implements `core::fmt::Display` and `core::error::Error` (with `source`), and `From` for
//! `#[from]` fields. `Debug` is up to the type.
//!
//! | where             | attribute                  | meaning                                         |
//! |-------------------|----------------------------|-------------------------------------------------|
//! | struct or variant | `#[error("fmt", args...)]` | the message, a `write!` format string           |
//! | struct or variant | `#[error(transparent)]`    | `Display` and `source` of the only field        |
//! | struct            | `#[error(code = "fmt")]`   | code error: `kind`, plus ` (fmt)` of `raw` if nonzero |
//! | field             | `#[source]`                | what `source()` returns; implied for a field named `source` |
//! | field             | `#[from]`                  | `#[source]` plus `From<FieldType>`; must be the only field |
//!
//! In format strings `{name}` and `{0}` are the fields (by reference), also as `name$` widths.
//! Other placeholders take the extra args, which see the fields as bindings: named fields by name,
//! tuple fields as `_0`, `_1`, ....
//!
//! A message describes its own level only. Don't interpolate the source into it: `source()`
//! already exposes it, and `marlin_diag::chain` prints `outer: inner: root`.
//!
//! ```ignore
//! #[derive(Debug, marlin_error::Error)]
//! enum LoadError {
//!     #[error("reading {path}")]
//!     Read { path: &'static str, source: marlin_io::Error },
//!     #[error("bad header: version {0}, expected {}", CURRENT)]
//!     Version(u32),
//!     #[error("gpu")]
//!     Gpu(#[from] marlin_rhi::Error),
//! }
//! ```
//!
//! A *code error* is the convention for errors that come straight from an OS or API call: a
//! `Copy` struct with fields `kind` (a `#[derive(ErrorKind)]` enum) and `raw` (an integer code,
//! 0 when there is none). `#[error(code = "os error {}")]` displays it as `not found` or
//! `not found (os error 2)`.
//!
//! # `#[derive(ErrorKind)]`
//!
//! For the kind enums of code errors. Every variant is a unit variant with `#[error("text")]`
//! (plain text, not a format string). Generates `const fn as_str(self) -> &'static str`, with
//! the enum's visibility, and a `Display` that writes it. The enum must be `Copy`.
//!
//! Neither derive infers bounds for generic parameters: write them on the type.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Error, Fields, Ident, LitStr, Member, Meta, Result, Token, Type,
};

#[proc_macro_derive(Error, attributes(error, source, from))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_error(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

#[proc_macro_derive(ErrorKind, attributes(error))]
pub fn derive_error_kind(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_error_kind(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// The contents of one `#[error(...)]`.
enum Display {
    Format {
        lit: LitStr,
        args: Vec<TokenStream2>,
    },
    Transparent(Span),
    Code(LitStr),
}

fn parse_display(attrs: &[Attribute]) -> Result<Option<(Display, Span)>> {
    let mut found = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("error")) {
        if found.is_some() {
            return Err(Error::new(attr.span(), "duplicate #[error(...)] attribute"));
        }
        found = Some((attr.parse_args_with(parse_display_args)?, attr.span()));
    }
    Ok(found)
}

fn parse_display_args(input: ParseStream) -> Result<Display> {
    if input.peek(LitStr) {
        let lit: LitStr = input.parse()?;
        let mut args = Vec::new();
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
            args = split_args(input.parse()?);
        }
        return Ok(Display::Format { lit, args });
    }
    if input.peek(Ident) {
        let ident: Ident = input.parse()?;
        if ident == "transparent" && input.is_empty() {
            return Ok(Display::Transparent(ident.span()));
        }
        if ident == "code" {
            input.parse::<Token![=]>()?;
            let lit: LitStr = input.parse()?;
            if !input.is_empty() {
                return Err(input.error("`code` takes only a format string for `raw`"));
            }
            return Ok(Display::Code(lit));
        }
    }
    Err(input.error("expected a format string, `transparent` or `code = \"...\"`"))
}

/// Splits format arguments at top-level commas. Token trees keep delimited groups whole, so
/// only turbofish and closure parameter lists could be split wrongly; bind those to a name first.
fn split_args(tokens: TokenStream2) -> Vec<TokenStream2> {
    let mut args = vec![TokenStream2::new()];
    for tt in tokens {
        match &tt {
            TokenTree::Punct(p) if p.as_char() == ',' => args.push(TokenStream2::new()),
            _ => args.last_mut().unwrap().extend([tt]),
        }
    }
    args.retain(|a| !a.is_empty());
    args
}

/// The name of a `name = value` format argument.
fn named_arg(arg: &TokenStream2) -> Option<String> {
    let mut tts = arg.clone().into_iter();
    match (tts.next(), tts.next()) {
        (Some(TokenTree::Ident(name)), Some(TokenTree::Punct(eq)))
            if eq.as_char() == '=' && eq.spacing() == proc_macro2::Spacing::Alone =>
        {
            Some(name.to_string())
        }
        _ => None,
    }
}

struct FieldInfo {
    member: Member,
    /// Pattern binding: the field name, or `_0`, `_1`, ... for tuple fields.
    binding: Ident,
    ty: Type,
    span: Span,
    /// `#[source]` or `#[from]`.
    explicit_source: bool,
    from: Option<Span>,
}

fn collect_fields(fields: &Fields) -> Result<Vec<FieldInfo>> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut explicit_source = false;
            let mut from = None;
            for attr in &f.attrs {
                let path = attr.path();
                if path.is_ident("error") {
                    return Err(Error::new(
                        attr.span(),
                        "#[error(...)] goes on the type or variant, not on a field",
                    ));
                }
                if !(path.is_ident("source") || path.is_ident("from")) {
                    continue;
                }
                if !matches!(attr.meta, Meta::Path(_)) {
                    return Err(Error::new(attr.span(), "this attribute takes no arguments"));
                }
                explicit_source = true;
                if path.is_ident("from") {
                    from = Some(attr.span());
                }
            }
            let (member, binding) = match &f.ident {
                Some(ident) => (Member::Named(ident.clone()), ident.clone()),
                None => (Member::Unnamed(i.into()), format_ident!("_{}", i)),
            };
            Ok(FieldInfo {
                member,
                binding,
                ty: f.ty.clone(),
                span: f.span(),
                explicit_source,
                from,
            })
        })
        .collect()
}

/// One struct or enum variant.
struct Case {
    /// `Self` or `Self::Variant`.
    path: TokenStream2,
    display: Display,
    fields: Vec<FieldInfo>,
    /// Index of the field `source()` returns.
    source: Option<usize>,
}

impl Case {
    fn new(
        path: TokenStream2,
        display: Option<(Display, Span)>,
        fields: &Fields,
        span: Span,
    ) -> Result<Self> {
        let Some((display, attr_span)) = display else {
            return Err(Error::new(
                span,
                "missing #[error(\"...\")], #[error(transparent)] or #[error(code = \"...\")]",
            ));
        };
        let fields = collect_fields(fields)?;

        let mut explicit = fields.iter().enumerate().filter(|(_, f)| f.explicit_source);
        let source = match (explicit.next(), explicit.next()) {
            (_, Some((_, second))) => {
                return Err(Error::new(second.span, "only one field can be the source"));
            }
            (Some((i, _)), None) => Some(i),
            (None, None) => fields
                .iter()
                .position(|f| matches!(&f.member, Member::Named(n) if n == "source")),
        };
        if let Some(from) = fields.iter().find_map(|f| f.from)
            && fields.len() != 1
        {
            return Err(Error::new(
                from,
                "a #[from] field must be the only field: `From` cannot fill the others",
            ));
        }

        match &display {
            Display::Transparent(span) => {
                if fields.len() != 1 {
                    return Err(Error::new(
                        *span,
                        "#[error(transparent)] needs exactly one field",
                    ));
                }
                if fields[0].explicit_source && fields[0].from.is_none() {
                    return Err(Error::new(
                        fields[0].span,
                        "#[error(transparent)] forwards `source` already; drop #[source]",
                    ));
                }
            }
            Display::Code(_) => {
                for name in ["kind", "raw"] {
                    if !fields
                        .iter()
                        .any(|f| matches!(&f.member, Member::Named(n) if n == name))
                    {
                        return Err(Error::new(
                            attr_span,
                            format_args!("a code error needs a field named `{name}`"),
                        ));
                    }
                }
                if let Some(f) = fields.iter().find(|f| f.explicit_source) {
                    return Err(Error::new(
                        f.span,
                        "code errors come from an OS or API call and have no source",
                    ));
                }
            }
            Display::Format { .. } => {}
        }

        Ok(Self {
            path,
            display,
            fields,
            source,
        })
    }

    /// `Self::Variant { a, 0: _0 }`, binding every field by reference.
    fn pattern(&self) -> TokenStream2 {
        let path = &self.path;
        let binds = self.fields.iter().map(|f| match &f.member {
            Member::Named(name) => quote!(#name),
            Member::Unnamed(index) => {
                let binding = &f.binding;
                quote!(#index: #binding)
            }
        });
        quote!(#path { #(#binds),* })
    }

    fn display_body(&self) -> TokenStream2 {
        match &self.display {
            Display::Format { lit, args } => {
                let tuple = matches!(
                    self.fields.first(),
                    Some(FieldInfo {
                        member: Member::Unnamed(_),
                        ..
                    })
                );
                let (rewritten, used) = rewrite_format(&lit.value(), &self.fields, tuple);
                let lit = LitStr::new(&rewritten, lit.span());
                let explicit: Vec<String> = args.iter().filter_map(named_arg).collect();
                let implicit = used
                    .iter()
                    .filter(|b| !explicit.contains(&b.to_string()))
                    .map(|b| quote!(#b = #b));
                quote!(::core::write!(__formatter, #lit #(, #args)* #(, #implicit)*))
            }
            Display::Transparent(_) => {
                let inner = &self.fields[0].binding;
                quote!(::core::fmt::Display::fmt(#inner, __formatter))
            }
            Display::Code(lit) => quote! {
                if *raw == 0 {
                    ::core::fmt::Display::fmt(kind, __formatter)
                } else {
                    ::core::write!(__formatter, "{} ({})", kind, ::core::format_args!(#lit, raw))
                }
            },
        }
    }

    fn source_body(&self) -> TokenStream2 {
        if let Display::Transparent(_) = self.display {
            let inner = &self.fields[0].binding;
            return quote!(::core::error::Error::source(#inner));
        }
        match self.source {
            Some(i) => {
                let source = &self.fields[i].binding;
                quote!(::core::option::Option::Some(
                    #source as &(dyn ::core::error::Error + 'static)
                ))
            }
            None => quote!(::core::option::Option::None),
        }
    }

    fn has_source(&self) -> bool {
        self.source.is_some() || matches!(self.display, Display::Transparent(_))
    }

    /// `From<FieldType>` for a `#[from]` field.
    fn impl_from(&self, input: &DeriveInput) -> Option<TokenStream2> {
        let field = self.fields.iter().find(|f| f.from.is_some())?;
        let (ident, path, member, ty) = (&input.ident, &self.path, &field.member, &field.ty);
        let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
        Some(quote! {
            impl #impl_generics ::core::convert::From<#ty> for #ident #ty_generics #where_clause {
                #[inline]
                fn from(source: #ty) -> Self {
                    #path { #member: source }
                }
            }
        })
    }
}

/// Rewrites tuple field references `{0}` to their bindings `{_0}` and returns the bindings the
/// string refers to, which become named arguments.
fn rewrite_format(fmt: &str, fields: &[FieldInfo], tuple: bool) -> (String, Vec<Ident>) {
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut used: Vec<Ident> = Vec::new();
    let use_field = |name: &str, used: &mut Vec<Ident>| -> bool {
        let Some(field) = fields.iter().find(|f| f.binding == name) else {
            return false;
        };
        if !used.contains(&field.binding) {
            used.push(field.binding.clone());
        }
        true
    };

    let mut rest = fmt;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let brace = rest.as_bytes()[i];
        rest = &rest[i + 1..];
        if rest.as_bytes().first() == Some(&brace) {
            // `{{` or `}}`.
            out.push(brace as char);
            out.push(brace as char);
            rest = &rest[1..];
            continue;
        }
        if brace == b'}' {
            out.push('}');
            continue;
        }
        let Some(end) = rest.find('}') else {
            // Unterminated: rustc reports it on the original string.
            out.push('{');
            break;
        };
        let inner = &rest[..end];
        rest = &rest[end + 1..];
        let (arg, spec) = match inner.find(':') {
            Some(colon) => (&inner[..colon], Some(&inner[colon + 1..])),
            None => (inner, None),
        };

        out.push('{');
        if tuple && !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
            let binding = format!("_{arg}");
            if use_field(&binding, &mut used) {
                out.push_str(&binding);
            } else {
                out.push_str(arg);
            }
        } else {
            use_field(arg, &mut used);
            out.push_str(arg);
        }
        if let Some(spec) = spec {
            // `name$` widths and precisions may name fields too.
            for (dollar, _) in spec.match_indices('$') {
                let start = spec[..dollar]
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(0, |p| p + 1);
                let name = &spec[start..dollar];
                if name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                    use_field(name, &mut used);
                }
            }
            out.push(':');
            out.push_str(spec);
        }
        out.push('}');
    }
    out.push_str(rest);
    (out, used)
}

fn expand_error(input: &DeriveInput) -> Result<TokenStream2> {
    let top = parse_display(&input.attrs)?;
    let cases = match &input.data {
        Data::Struct(data) => vec![Case::new(quote!(Self), top, &data.fields, input.span())?],
        Data::Enum(data) => {
            if let Some((_, span)) = top {
                return Err(Error::new(
                    span,
                    "put #[error(...)] on each variant, not on the enum",
                ));
            }
            data.variants
                .iter()
                .map(|v| {
                    let ident = &v.ident;
                    let display = parse_display(&v.attrs)?;
                    if let Some((Display::Code(_), span)) = display {
                        return Err(Error::new(span, "code errors are structs"));
                    }
                    Case::new(quote!(Self::#ident), display, &v.fields, v.span())
                })
                .collect::<Result<Vec<_>>>()?
        }
        Data::Union(data) => {
            return Err(Error::new(
                data.union_token.span,
                "Error cannot be derived for unions",
            ));
        }
    };

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let is_enum = matches!(input.data, Data::Enum(_));

    let display = if is_enum && cases.is_empty() {
        quote!(match *self {})
    } else if is_enum {
        let arms = cases.iter().map(|c| {
            let (pattern, body) = (c.pattern(), c.display_body());
            quote!(#pattern => #body,)
        });
        quote!(match self { #(#arms)* })
    } else {
        let (pattern, body) = (cases[0].pattern(), cases[0].display_body());
        quote!(let #pattern = self; #body)
    };

    let source = cases.iter().any(Case::has_source).then(|| {
        let body = if is_enum {
            let arms = cases.iter().map(|c| {
                let (pattern, body) = (c.pattern(), c.source_body());
                quote!(#[allow(unused_variables)] #pattern => #body,)
            });
            quote!(match self { #(#arms)* })
        } else {
            let (pattern, body) = (cases[0].pattern(), cases[0].source_body());
            quote!(#[allow(unused_variables)] let #pattern = self; #body)
        };
        quote! {
            fn source(&self) -> ::core::option::Option<&(dyn ::core::error::Error + 'static)> {
                #body
            }
        }
    });

    let from_impls = cases.iter().filter_map(|c| c.impl_from(input));

    Ok(quote! {
        impl #impl_generics ::core::fmt::Display for #ident #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn fmt(&self, __formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                #display
            }
        }

        impl #impl_generics ::core::error::Error for #ident #ty_generics #where_clause {
            #source
        }

        #(#from_impls)*
    })
}

fn expand_error_kind(input: &DeriveInput) -> Result<TokenStream2> {
    let Data::Enum(data) = &input.data else {
        return Err(Error::new(
            input.ident.span(),
            "ErrorKind is derived for enums of unit variants",
        ));
    };
    if let Some(attr) = input.attrs.iter().find(|a| a.path().is_ident("error")) {
        return Err(Error::new(
            attr.span(),
            "put #[error(\"...\")] on each variant, not on the enum",
        ));
    }

    let mut arms = Vec::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(Error::new(
                variant.fields.span(),
                "ErrorKind variants carry no data",
            ));
        }
        let text = match parse_display(&variant.attrs)? {
            Some((Display::Format { lit, args }, _)) if args.is_empty() => lit,
            Some((_, span)) => {
                return Err(Error::new(
                    span,
                    "an ErrorKind variant takes plain text: #[error(\"...\")]",
                ));
            }
            None => {
                return Err(Error::new(
                    variant.span(),
                    "missing #[error(\"...\")] describing this kind",
                ));
            }
        };
        let ident = &variant.ident;
        arms.push(quote!(Self::#ident => #text,));
    }

    let (ident, vis) = (&input.ident, &input.vis);
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #ident #ty_generics #where_clause {
            /// What this kind displays as.
            #vis const fn as_str(self) -> &'static str {
                match self { #(#arms)* }
            }
        }

        impl #impl_generics ::core::fmt::Display for #ident #ty_generics #where_clause {
            fn fmt(&self, __formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                __formatter.write_str(Self::as_str(*self))
            }
        }
    })
}
