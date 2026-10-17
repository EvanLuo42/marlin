//! `#[derive(Pod)]` for `marlin-serial` cooked layouts; use it as `marlin_serial::cooked::Pod`.
//!
//! The derive implements the `unsafe` trait `Pod` only after checking, at compile time, what its
//! safety contract requires:
//!
//! - the struct is `#[repr(C)]` or `#[repr(transparent)]` (optionally with `align(N)` or
//!   `packed`), so its layout is defined;
//! - every field is `Pod` (which also rules out references, pointers, `bool`, enums and interior
//!   mutability);
//! - there are no padding bytes: the struct's size is the sum of its fields' sizes. Add explicit
//!   padding fields (`_pad: [u8; 4]`) where the compiler would insert padding.
//!
//! It also generates the type's `LAYOUT` (field names, offsets and types), from which the cooked
//! header's layout hash is computed. Type parameters get a `T: Pod` bound; for generic types the
//! padding check runs when an instantiation is first used through `CookedBuilder` or
//! `CookedView`, for other types immediately.
//!
//! `#[pod(crate = "path")]` names `marlin_serial` when it is re-exported.
//!
//! ```
//! use marlin_serial::cooked::{Pod, RelSlice};
//!
//! #[derive(Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Mesh {
//!     vertices: RelSlice<[f32; 3]>,
//!     lod_bias: f32,
//!     _pad: [u8; 4],
//! }
//! ```
//!
//! # Errors
//!
//! Each of these fails to compile. Implicit padding (here 3 bytes after `a`):
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Padded { a: u8, b: u32 }
//! ```
//! No `repr(C)`:
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! struct Unspecified { a: u32 }
//! ```
//! A field that is not `Pod`:
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Flag { on: bool }
//! ```
//! Enums and unions:
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(u32)]
//! enum Kind { A, B }
//! ```
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(C)]
//! union Bits { f: f32, u: u32 }
//! ```
//! Lifetimes:
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Borrowed<'a> { a: &'a u32 }
//! ```
//! A generic type with padding, once it is used:
//! ```compile_fail
//! #[derive(marlin_serial::cooked::Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Pair<T> { a: u8, b: T }
//! let mut b = marlin_serial::cooked::CookedBuilder::new();
//! b.push(&Pair { a: 1u8, b: 2u32 });
//! ```

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Error, Fields, Ident, LitStr, Member, Path, Result, parse_quote};

#[proc_macro_derive(Pod, attributes(pod))]
pub fn derive_pod(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn crate_path(input: &DeriveInput) -> Result<Path> {
    let mut krate = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("pod")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let lit: LitStr = meta.value()?.parse()?;
                krate = Some(lit.parse()?);
                Ok(())
            } else {
                Err(meta.error("unsupported pod attribute; expected `crate = \"path\"`"))
            }
        })?;
    }
    Ok(krate.unwrap_or_else(|| parse_quote!(::marlin_serial)))
}

/// Requires `repr(C)` or `repr(transparent)`; `align` and `packed` may be added.
fn check_repr(input: &DeriveInput) -> Result<()> {
    let mut defined = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("repr")) {
        attr.parse_nested_meta(|meta| {
            defined |= meta.path.is_ident("C") || meta.path.is_ident("transparent");
            if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                content.parse::<TokenStream2>()?;
            }
            Ok(())
        })?;
    }
    if defined {
        Ok(())
    } else {
        Err(Error::new(
            input.ident.span(),
            "Pod requires `#[repr(C)]` or `#[repr(transparent)]`: the layout of other structs is \
             unspecified and may change between builds",
        ))
    }
}

fn expand(input: &DeriveInput) -> Result<TokenStream2> {
    let krate = crate_path(input)?;
    let ident = &input.ident;
    let data = match &input.data {
        Data::Struct(data) => data,
        Data::Enum(e) => {
            return Err(Error::new(
                e.enum_token.span,
                "Pod cannot be derived for enums: not every bit pattern is a valid variant; store \
                 the discriminant as an integer field instead",
            ));
        }
        Data::Union(u) => {
            return Err(Error::new(
                u.union_token.span,
                "Pod cannot be derived for unions: the derive cannot check their fields; implement \
                 `Pod` by hand if every field is `Pod` and they all have the union's size",
            ));
        }
    };
    if let Some(lt) = input.generics.lifetimes().next() {
        return Err(Error::new(
            lt.span(),
            "Pod types must be 'static: cooked data cannot hold references",
        ));
    }
    check_repr(input)?;

    let mut generics = input.generics.clone();
    let type_params: Vec<Ident> = generics.type_params().map(|p| p.ident.clone()).collect();
    let where_clause = generics.make_where_clause();
    for param in &type_params {
        where_clause
            .predicates
            .push(parse_quote!(#param: #krate::cooked::Pod));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let fields: Vec<(Member, String, &syn::Type)> = match &data.fields {
        Fields::Named(f) => f
            .named
            .iter()
            .map(|f| {
                let ident = f.ident.clone().expect("named field");
                let name = ident.to_string();
                (Member::Named(ident), name, &f.ty)
            })
            .collect(),
        Fields::Unnamed(f) => f
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| (Member::Unnamed(i.into()), i.to_string(), &f.ty))
            .collect(),
        Fields::Unit => Vec::new(),
    };
    let types: Vec<&syn::Type> = fields.iter().map(|(_, _, ty)| *ty).collect();
    let layout_fields = fields.iter().map(|(member, name, ty)| {
        quote! {
            #krate::cooked::PodField {
                name: #name,
                offset: ::core::mem::offset_of!(Self, #member),
                ty: #krate::cooked::pod_layout::<#ty>,
            }
        }
    });
    let padding_message = format!(
        "`{ident}` has padding bytes: add explicit padding fields (such as `_pad: [u8; N]`) so \
         that every byte belongs to a field"
    );
    let check = quote! {
        ::core::assert!(
            ::core::mem::size_of::<Self>() == 0 #(+ ::core::mem::size_of::<#types>())*,
            #padding_message
        )
    };
    // Field types must be `Pod`; for generic types this is checked against the declared bounds.
    let field_check = quote! {
        const _: () = {
            #[allow(dead_code)]
            fn __assert_fields_are_pod #impl_generics () #where_clause {
                fn pod<T: #krate::cooked::Pod>() {}
                #(pod::<#types>();)*
            }
        };
    };
    let eager_check = generics
        .params
        .is_empty()
        .then(|| quote!(const _: () = <#ident as #krate::cooked::Pod>::CHECK;));

    Ok(quote! {
        // SAFETY: `repr(C)`/`repr(transparent)` (checked above), every field is `Pod` (checked by
        // `__assert_fields_are_pod`), and `CHECK` rejects padding; `Copy` is a supertrait.
        #[automatically_derived]
        unsafe impl #impl_generics #krate::cooked::Pod for #ident #ty_generics #where_clause {
            const LAYOUT: #krate::cooked::PodLayout = #krate::cooked::PodLayout {
                kind: #krate::cooked::PodKind::Struct(&[#(#layout_fields),*]),
                ..#krate::cooked::PodLayout::opaque::<Self>()
            };
            const CHECK: () = #check;
        }
        #field_check
        #eager_check
    })
}
