//! Types: `float3`, `A.B<T, 4>`, `float[N * 2]`, `(float3 Wi, float Pdf)`.

use super::{exprs, name, name_ref, scan};
use crate::SyntaxKind::*;
use crate::T;
use crate::parser::{CompletedMarker, Parser};

pub(super) fn at_type_start(p: &Parser<'_>) -> bool {
    let kind = p.current();
    kind.is_predefined_type()
        || kind == IDENT
        || (kind == T!['('] && scan::scan_type(p, 0).is_some())
}

/// Parses a type, or reports that one is missing (consuming nothing).
pub(super) fn type_ref(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if !p.enter() {
        return None;
    }
    let result = type_inner(p);
    p.leave();
    result
}

fn type_inner(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let kind = p.current();
    let mut ty = if kind.is_predefined_type() {
        let m = p.start();
        p.bump_any();
        m.complete(p, PREDEFINED_TYPE)
    } else if kind == IDENT {
        let m = p.start();
        path(p);
        m.complete(p, PATH_TYPE)
    } else if kind == T!['('] {
        tuple_type(p)
    } else {
        p.error_expected("a type");
        return None;
    };

    while p.at(T!['[']) {
        let m = ty.precede(p);
        p.bump(T!['[']);
        if p.at(T![']']) {
            p.error(
                "an array type needs a length: `float[4]`; variable-length data goes in a buffer",
            );
        } else {
            let no_gt = std::mem::replace(&mut p.no_gt, false);
            exprs::expr(p);
            p.no_gt = no_gt;
        }
        p.expect(T![']']);
        ty = m.complete(p, ARRAY_TYPE);
    }
    Some(ty)
}

/// `global::A.B<C>.D`: a `PATH` of `PATH_SEGMENT`s.
pub(super) fn path(p: &mut Parser<'_>) {
    let m = p.start();
    if p.at(T![global]) && p.nth_at(1, T![::]) {
        p.bump(T![global]);
        p.bump(T![::]);
    }
    loop {
        let s = p.start();
        name_ref(p);
        if p.at(T![<]) {
            generic_arg_list(p);
        }
        s.complete(p, PATH_SEGMENT);
        if p.at(T![.]) && p.nth(1) == IDENT {
            p.bump(T![.]);
        } else {
            break;
        }
    }
    m.complete(p, PATH);
}

/// `<A, B<C>, N - 1>` in a type or after a name in an expression.
pub(super) fn generic_arg_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![<]);
    while !p.at(T![>]) && !p.at(EOF) {
        let a = p.start();
        if is_const_arg(p) {
            let no_gt = std::mem::replace(&mut p.no_gt, true);
            exprs::expr(p);
            p.no_gt = no_gt;
            a.complete(p, CONST_ARG);
        } else if type_ref(p).is_some() {
            a.complete(p, TYPE_ARG);
        } else {
            a.abandon(p);
            break;
        }
        if !p.at(T![>]) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![>]);
    m.complete(p, GENERIC_ARG_LIST);
}

/// Whether the generic argument at the cursor is a constant expression rather than a type:
/// a literal, a parenthesized expression, a negation, or a name followed by an arithmetic
/// operator (`N - 1`).
fn is_const_arg(p: &Parser<'_>) -> bool {
    match p.raw(0) {
        INT_NUMBER | FLOAT_NUMBER | TRUE_KW | FALSE_KW | MINUS => true,
        L_PAREN => scan::scan_type(p, 0).is_none(),
        IDENT => scan::scan_type(p, 0)
            .is_some_and(|end| matches!(p.raw(end), PLUS | MINUS | STAR | SLASH | PERCENT | SHL)),
        _ => false,
    }
}

/// `(float3 Wi, float Pdf)`
fn tuple_type(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T!['(']);
    while !p.at(T![')']) && !p.at(EOF) {
        let e = p.start();
        if type_ref(p).is_none() {
            e.abandon(p);
            break;
        }
        if p.at(IDENT) {
            name(p);
        }
        e.complete(p, TUPLE_TYPE_ELEMENT);
        if !p.at(T![')']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![')']);
    m.complete(p, TUPLE_TYPE)
}
