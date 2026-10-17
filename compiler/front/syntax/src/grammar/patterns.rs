//! Patterns for `is`, switch expressions and `case` labels: constant, relational, `and`/`or`/
//! `not`, parenthesized, discard, `var x`, property `{ X: p }` and positional `(p, q)`.
//! There are no type patterns.

use super::{exprs, name, name_ref};
use crate::SyntaxKind::*;
use crate::T;
use crate::parser::{CompletedMarker, Parser};

pub(super) fn pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if !p.enter() {
        return None;
    }
    let result = or_pattern(p);
    p.leave();
    result
}

fn or_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = and_pattern(p)?;
    while p.at(T![or]) {
        let m = lhs.precede(p);
        p.bump(T![or]);
        and_pattern(p);
        lhs = m.complete(p, BINARY_PATTERN);
    }
    Some(lhs)
}

fn and_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = not_pattern(p)?;
    while p.at(T![and]) {
        let m = lhs.precede(p);
        p.bump(T![and]);
        not_pattern(p);
        lhs = m.complete(p, BINARY_PATTERN);
    }
    Some(lhs)
}

fn not_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(T![not]) {
        let m = p.start();
        p.bump(T![not]);
        not_pattern(p);
        return Some(m.complete(p, NOT_PATTERN));
    }
    primary_pattern(p)
}

fn primary_pattern(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    match p.current() {
        T![<] | T![<=] | T![>=] => Some(relational(p)),
        T![>] if !p.at(T![>>]) => Some(relational(p)),
        IDENT if p.raw_text(0) == "_" && !matches!(p.nth(1), DOT | L_PAREN | L_BRACK) => {
            let m = p.start();
            p.bump(IDENT);
            Some(m.complete(p, DISCARD_PATTERN))
        }
        IDENT if p.at(T![var]) && p.nth(1) == IDENT => {
            let m = p.start();
            p.bump(T![var]);
            name(p);
            Some(m.complete(p, VAR_PATTERN))
        }
        T!['{'] => Some(property_pattern(p)),
        T!['('] => Some(paren_pattern(p)),
        _ => {
            let m = p.start();
            if exprs::binary(p, exprs::SHIFT_PRECEDENCE).is_none() {
                m.abandon(p);
                return None;
            }
            Some(m.complete(p, CONSTANT_PATTERN))
        }
    }
}

/// `< 3`, `>= x`
fn relational(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump_any();
    exprs::binary(p, exprs::SHIFT_PRECEDENCE);
    m.complete(p, RELATIONAL_PATTERN)
}

/// `{ Kind: Lobe.Diffuse, Weight: > 0 }`
fn property_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T!['{']);
    while !p.at(T!['}']) && !p.at(EOF) {
        let s = p.start();
        if p.at(IDENT) {
            // A member path: `Committed.Kind`.
            let path = p.start();
            name_ref(p);
            let mut path = path.complete(p, NAME_EXPR);
            while p.at(T![.]) {
                let member = path.precede(p);
                p.bump(T![.]);
                name_ref(p);
                path = member.complete(p, MEMBER_EXPR);
            }
            p.expect(T![:]);
        }
        if pattern(p).is_none() {
            s.abandon(p);
            break;
        }
        s.complete(p, SUBPATTERN);
        if !p.at(T!['}']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T!['}']);
    m.complete(p, PROPERTY_PATTERN)
}

/// `(p)` or `(p, q, ...)`
fn paren_pattern(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T!['(']);
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    let mut count = 0;
    while !p.at(T![')']) && !p.at(EOF) {
        let s = p.start();
        if pattern(p).is_none() {
            s.abandon(p);
            break;
        }
        count += 1;
        if count == 1 && p.at(T![')']) {
            s.abandon(p);
        } else {
            s.complete(p, SUBPATTERN);
        }
        if !p.at(T![')']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![')']);
    p.no_gt = no_gt;
    m.complete(
        p,
        if count == 1 {
            PAREN_PATTERN
        } else {
            POSITIONAL_PATTERN
        },
    )
}
