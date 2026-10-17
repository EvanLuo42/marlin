//! `#[gpu_test(api = ...)]`: a test that only makes sense on some graphics APIs (docs/design/
//! 0016-rhi-vulkan.md §9). Use it through `marlin_gpu_test_support::gpu_test`.
//!
//! ```ignore
//! #[gpu_test(api = d3d12)]
//! fn d3d12_has_dedicated_compute_and_copy_queues() { ... }
//!
//! #[gpu_test(api = vulkan | d3d12)]
//! fn ...() { ... }
//! ```
//!
//! It stands in for `#[test]` and starts the body with a run-time check of
//! `marlin_rhi::GRAPHICS_API` that `skip!`s on every other API, so the skip is recorded. The
//! check is at run time because the backend is a constant, not a `cfg`; platforms keep their
//! `cfg(marlin_platform)` gates (docs/design/0014-testing.md §3). API names are
//! `GraphicsApi::name()`'s: `vulkan`, `d3d12`, `metal`, `agc`, `nvn`. Other attributes
//! (`#[ignore]`, ...) stay; a skip returns normally, so `#[should_panic]` tests cannot use it.

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Error, Ident, Result, Token};

#[proc_macro_attribute]
pub fn gpu_test(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(args as Args);
    expand(&args, item.into())
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// `api = a | b | ...`.
struct Args {
    apis: Vec<Ident>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> Result<Self> {
        let key: Ident = input.parse()?;
        if key != "api" {
            return Err(Error::new(key.span(), "expected `api = ...`"));
        }
        input.parse::<Token![=]>()?;
        let mut apis = vec![input.parse::<Ident>()?];
        while input.peek(Token![|]) {
            input.parse::<Token![|]>()?;
            apis.push(input.parse()?);
        }
        if !input.is_empty() {
            return Err(input.error("expected `|` or the end of the arguments"));
        }
        Ok(Self { apis })
    }
}

/// `GraphicsApi`'s variant and display name for `api` (a `GraphicsApi::name()`).
fn variant(api: &Ident) -> Result<(Ident, &'static str)> {
    let (variant, display) = match api.to_string().as_str() {
        "vulkan" => ("Vulkan", "Vulkan"),
        "d3d12" => ("D3D12", "D3D12"),
        "metal" => ("Metal", "Metal"),
        "agc" => ("Agc", "AGC"),
        "nvn" => ("Nvn", "NVN"),
        _ => {
            return Err(Error::new(
                api.span(),
                "unknown graphics API: expected vulkan, d3d12, metal, agc or nvn",
            ));
        }
    };
    Ok((format_ident!("{variant}", span = api.span()), display))
}

fn expand(args: &Args, item: TokenStream2) -> Result<TokenStream2> {
    let mut variants = Vec::new();
    let mut names = Vec::new();
    for api in &args.apis {
        let (variant, name) = variant(api)?;
        variants.push(variant);
        names.push(name);
    }
    let reason = format!("{} only", names.join(" and "));
    let support = quote!(::marlin_gpu_test_support);
    // A test function is its attributes and signature, then its body: the last token, in braces.
    let mut tokens: Vec<TokenTree> = item.into_iter().collect();
    let body = match tokens.pop() {
        Some(TokenTree::Group(body)) if body.delimiter() == Delimiter::Brace => body,
        _ => {
            return Err(Error::new(
                proc_macro2::Span::call_site(),
                "#[gpu_test] goes on a test function",
            ));
        }
    };
    let head: TokenStream2 = tokens.into_iter().collect();
    let body = body.stream();
    Ok(quote! {
        #[test]
        #head {
            if !::core::matches!(
                #support::__rhi::GRAPHICS_API,
                #(#support::__rhi::GraphicsApi::#variants)|*
            ) {
                #support::skip!(#reason);
            }
            #body
        }
    })
}
