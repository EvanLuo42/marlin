//! The superset grammar of the C#-family languages (`docs/design/0015-language-frontend.md`
//! §2): every construct any dialect uses gets a proper node, whether it comes from C# (`new`,
//! `class`, `null`, `as`, `typeof`, ...) or from the Marlin shader language (value generic
//! parameters, associated types, extension blocks with interface conformance, `const if`).
//!
//! The grammar never looks at the dialect. Validation rejects what a dialect lacks; the parser
//! itself only reports syntax errors and C# constructs no dialect supports yet.

mod exprs;
mod items;
mod patterns;
mod scan;
mod stmts;
mod types;

use crate::SyntaxKind::*;
use crate::T;
use crate::parser::{Parser, TokenSet};

pub(crate) use items::source_file;

/// `Foo` where something is declared.
fn name(p: &mut Parser<'_>) {
    if p.at(IDENT) {
        let m = p.start();
        p.bump(IDENT);
        m.complete(p, NAME);
    } else {
        p.error_expected("a name");
    }
}

/// `Foo` where something is referenced.
fn name_ref(p: &mut Parser<'_>) {
    if p.at(IDENT) {
        let m = p.start();
        p.bump(IDENT);
        m.complete(p, NAME_REF);
    } else {
        p.error_expected("an identifier");
    }
}

/// `[A, B(1, X = 2)]*`
fn attribute_lists(p: &mut Parser<'_>) {
    while p.at(T!['[']) {
        attribute_list(p);
    }
}

fn attribute_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T!['[']);
    while !p.at(T![']']) && !p.at(EOF) {
        if !p.at(IDENT) && !p.at(T![global]) {
            p.err_recover("expected an attribute", TokenSet::new(&[T![']'], T![,]]));
            if !p.eat(T![,]) {
                break;
            }
            continue;
        }
        let a = p.start();
        types::path(p);
        if p.at(T!['(']) {
            arg_list(p);
        }
        a.complete(p, ATTRIBUTE);
        if !p.eat(T![,]) {
            break;
        }
    }
    p.expect(T![']']);
    m.complete(p, ATTRIBUTE_LIST);
}

const MODIFIER_KINDS: TokenSet = TokenSet::new(&[
    PUBLIC_KW,
    INTERNAL_KW,
    PRIVATE_KW,
    PROTECTED_KW,
    STATIC_KW,
    READONLY_KW,
    CONST_KW,
    ABSTRACT_KW,
    VIRTUAL_KW,
    OVERRIDE_KW,
    SEALED_KW,
    EXTERN_KW,
    UNSAFE_KW,
    VOLATILE_KW,
]);

/// `public static readonly ...` in front of a declaration. `const if` is a statement, so a
/// `const` followed by `if` is left alone.
fn modifiers(p: &mut Parser<'_>) {
    if !p.at_ts(MODIFIER_KINDS) || (p.at(T![const]) && p.nth_at(1, T![if])) {
        return;
    }
    let m = p.start();
    while p.at_ts(MODIFIER_KINDS) && !(p.at(T![const]) && p.nth_at(1, T![if])) {
        p.bump_any();
    }
    m.complete(p, MODIFIERS);
}

/// `(a, lod: 2, out var x, ref w)`
fn arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T!['(']);
    args_until(p, T![')']);
    p.expect(T![')']);
    m.complete(p, ARG_LIST);
}

/// `[i]`, `[r, c]`
fn bracket_arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T!['[']);
    args_until(p, T![']']);
    p.expect(T![']']);
    m.complete(p, BRACKET_ARG_LIST);
}

fn args_until(p: &mut Parser<'_>, close: crate::SyntaxKind) {
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    while !p.at(close) && !p.at(EOF) {
        if !arg(p) {
            p.err_recover(
                "expected an argument",
                TokenSet::new(&[close, T![,], T![;]]),
            );
        }
        if !p.at(close) && !p.expect(T![,]) {
            break;
        }
    }
    p.no_gt = no_gt;
}

/// One argument: optional `name:`, optional `in`/`out`/`ref`, then an expression or an inline
/// declaration (`out var x`). Returns whether anything was parsed.
fn arg(p: &mut Parser<'_>) -> bool {
    let m = p.start();
    if p.at(IDENT) && p.nth_at(1, T![:]) {
        let n = p.start();
        name_ref(p);
        p.bump(T![:]);
        n.complete(p, NAME_COLON);
    }
    let by_ref = p.eat(T![out]) || p.eat(T![ref]) || p.eat(T![in]);
    let parsed = if by_ref && scan::is_declaration_expr(p) {
        exprs::declaration_expr(p);
        true
    } else {
        exprs::expr(p).is_some()
    };
    if !parsed && !by_ref {
        m.abandon(p);
        return false;
    }
    m.complete(p, ARG);
    true
}

/// `(int a, in float b = 1)` for functions, constructors and extension receivers; the
/// bracketed form for indexers.
fn param_list(p: &mut Parser<'_>, open: crate::SyntaxKind, close: crate::SyntaxKind) {
    if !p.at(open) {
        p.error_expected(&open.describe());
        return;
    }
    let m = p.start();
    p.bump(open);
    while !p.at(close) && !p.at(EOF) {
        if !param(p) {
            p.err_recover(
                "expected a parameter",
                TokenSet::new(&[close, T![,], T!['{'], T![;], T![=>]]),
            );
            if !p.at(T![,]) {
                break;
            }
        }
        if !p.at(close) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(close);
    m.complete(p, PARAM_LIST);
}

const PARAM_MODIFIERS: TokenSet =
    TokenSet::new(&[IN_KW, OUT_KW, REF_KW, THIS_KW, READONLY_KW, PARAMS_KW]);

fn param(p: &mut Parser<'_>) -> bool {
    if !p.at(T!['[']) && !p.at_ts(PARAM_MODIFIERS) && !types::at_type_start(p) {
        return false;
    }
    let m = p.start();
    attribute_lists(p);
    if p.at_ts(PARAM_MODIFIERS) {
        let mods = p.start();
        while p.at_ts(PARAM_MODIFIERS) {
            p.bump_any();
        }
        mods.complete(p, MODIFIERS);
    }
    types::type_ref(p);
    // A receiver of an extension block may be unnamed: `extension(float3)`.
    if p.at(IDENT) {
        name(p);
    }
    if p.eat(T![=]) {
        exprs::expr(p);
    }
    m.complete(p, PARAM);
    true
}
