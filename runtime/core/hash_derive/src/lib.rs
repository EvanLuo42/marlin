//! `#[derive(StableHash)]` for `marlin-hash`.
//!
//! Structs hash their fields in declaration order. Enums hash the variant's discriminant (as
//! `i64`), then its fields; every variant must spell out its discriminant (`A = 0`, with a
//! `#[repr(u8)]` or similar when variants carry fields), so that reordering variants is a
//! compile-time-visible change rather than a silent change of every hash.
//!
//! Attributes, all under `#[stable_hash(...)]`:
//!
//! | where | key              | meaning                                                    |
//! |-------|------------------|------------------------------------------------------------|
//! | type  | `crate = "path"` | path to `marlin_hash` when it is re-exported               |
//! | field | `skip`           | not hashed (caches, debug names)                           |

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, Fields, Ident, Member, Path, Result, parse_quote,
};

#[proc_macro_derive(StableHash, attributes(stable_hash))]
pub fn derive_stable_hash(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

#[derive(Default)]
struct Attrs {
    krate: Option<Path>,
    skip: bool,
}

fn parse_attrs(attrs: &[Attribute], on_type: bool) -> Result<Attrs> {
    let mut out = Attrs::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("stable_hash")) {
        attr.parse_nested_meta(|meta| {
            if on_type && meta.path.is_ident("crate") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                out.krate = Some(lit.parse()?);
            } else if !on_type && meta.path.is_ident("skip") {
                out.skip = true;
            } else {
                let expected = if on_type { "crate" } else { "skip" };
                return Err(meta.error(format_args!(
                    "unsupported stable_hash attribute here; expected `{expected}`"
                )));
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// The integer type of the enum's discriminants: its `#[repr]`, else `isize`.
fn discriminant_type(input: &DeriveInput) -> Result<Ident> {
    const INTS: [&str; 10] = [
        "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
    ];
    let mut ty = format_ident!("isize");
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("repr")) {
        attr.parse_nested_meta(|meta| {
            if let Some(ident) = meta.path.get_ident() {
                let name = ident.to_string();
                if INTS.contains(&name.as_str()) {
                    ty = ident.clone();
                } else if name == "u128" || name == "i128" {
                    return Err(meta.error(
                        "StableHash hashes discriminants as i64; 128-bit reprs are not supported",
                    ));
                }
            }
            if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                content.parse::<TokenStream2>()?;
            }
            Ok(())
        })?;
    }
    Ok(ty)
}

/// `(pattern, statements)` for one set of fields: the pattern binds the hashed fields as
/// `__f0, __f1, ...`; the statements hash them in order.
fn fields(krate: &Path, fields: &Fields) -> Result<(TokenStream2, TokenStream2)> {
    let mut binds = Vec::new();
    let mut hashes = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let attrs = parse_attrs(&f.attrs, false)?;
        let member = match &f.ident {
            Some(ident) => Member::Named(ident.clone()),
            None => Member::Unnamed(i.into()),
        };
        if attrs.skip {
            continue;
        }
        let binding = format_ident!("__f{}", i);
        binds.push(quote!(#member: #binding));
        hashes.push(quote!(#krate::StableHash::stable_hash(#binding, __h);));
    }
    let pattern = match fields {
        Fields::Unit => quote!(),
        _ => quote!({ #(#binds,)* .. }),
    };
    Ok((pattern, quote!(#(#hashes)*)))
}

fn expand(input: &DeriveInput) -> Result<TokenStream2> {
    let attrs = parse_attrs(&input.attrs, true)?;
    let krate = attrs.krate.unwrap_or_else(|| parse_quote!(::marlin_hash));

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
            .push(parse_quote!(#param: #krate::StableHash));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => {
            let (pattern, hashes) = fields(&krate, &data.fields)?;
            quote! {
                let Self #pattern = self;
                #hashes
            }
        }
        Data::Enum(data) => {
            let repr = discriminant_type(input)?;
            let mut arms = Vec::new();
            for v in &data.variants {
                let Some((_, expr)) = &v.discriminant else {
                    return Err(Error::new(
                        v.span(),
                        "StableHash needs an explicit discriminant on every variant (`Name = 0`), \
                         so that reordering variants cannot change hashes silently",
                    ));
                };
                let expr: &Expr = expr;
                let name = &v.ident;
                let (pattern, hashes) = fields(&krate, &v.fields)?;
                arms.push(quote! {
                    Self::#name #pattern => {
                        const __D: #repr = #expr;
                        #krate::StableHasher::write_discriminant(__h, __D as i64);
                        #hashes
                    }
                });
            }
            if arms.is_empty() {
                // No values exist; `match self {}` would not see that through the reference.
                quote!(match *self {})
            } else {
                quote! {
                    match self {
                        #(#arms)*
                    }
                }
            }
        }
        Data::Union(_) => {
            return Err(Error::new(
                input.ident.span(),
                "StableHash cannot be derived for unions: the active field is unknown",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics #krate::StableHash for #ident #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn stable_hash<__H: #krate::StableHasher + ?Sized>(&self, __h: &mut __H) {
                #body
            }
        }
    })
}
