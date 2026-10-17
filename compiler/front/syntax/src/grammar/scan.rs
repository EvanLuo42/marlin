//! Bounded speculative scans that settle C#'s ambiguities without backtracking:
//! generic argument lists in expressions (`F<T>(x)` vs `a < b`), local declarations
//! (`A<B> c;`, `T[4] x;`), casts (`(T)x` vs `(a) + b`), and parenthesized lambdas.
//!
//! The scans only look at token kinds; they never push events. Each is limited to a fixed
//! number of tokens and nesting levels, so pathological input stays linear.

use crate::SyntaxKind::{self, *};
use crate::parser::Parser;

/// How far a scan may look ahead.
const LIMIT: usize = 512;
/// How deep generic and tuple types may nest inside a scan.
const MAX_DEPTH: u32 = 16;

/// The index just past a type starting `i` tokens ahead.
pub(super) fn scan_type(p: &Parser<'_>, i: usize) -> Option<usize> {
    scan_type_at(p, i, 0)
}

fn scan_type_at(p: &Parser<'_>, i: usize, depth: u32) -> Option<usize> {
    if depth > MAX_DEPTH || i > LIMIT {
        return None;
    }
    let kind = p.raw(i);
    let mut i = if kind.is_predefined_type() {
        i + 1
    } else if kind == IDENT {
        scan_path(p, i, depth)?
    } else if kind == L_PAREN {
        scan_tuple_type(p, i, depth)?
    } else {
        return None;
    };
    while p.raw(i) == L_BRACK {
        i = skip_balanced(p, i)?;
    }
    Some(i)
}

/// `global::A.B<C>.D`
fn scan_path(p: &Parser<'_>, mut i: usize, depth: u32) -> Option<usize> {
    if p.raw(i) == IDENT && p.raw_text(i) == "global" && p.raw(i + 1) == COLON2 {
        i += 2;
    }
    loop {
        if p.raw(i) != IDENT {
            return None;
        }
        i += 1;
        if p.raw(i) == LT {
            i = scan_generic_args(p, i, depth)?;
        }
        if p.raw(i) == DOT && p.raw(i + 1) == IDENT {
            i += 1;
            continue;
        }
        return Some(i);
    }
}

/// `<A, B<C>, N - 1>` starting at the `<`.
fn scan_generic_args(p: &Parser<'_>, mut i: usize, depth: u32) -> Option<usize> {
    debug_assert_eq!(p.raw(i), LT);
    if depth > MAX_DEPTH {
        return None;
    }
    i += 1;
    loop {
        if i > LIMIT {
            return None;
        }
        i = scan_generic_arg(p, i, depth + 1)?;
        match p.raw(i) {
            COMMA => i += 1,
            GT => return Some(i + 1),
            _ => return None,
        }
    }
}

fn scan_generic_arg(p: &Parser<'_>, i: usize, depth: u32) -> Option<usize> {
    if let Some(end) = scan_type_at(p, i, depth)
        && matches!(p.raw(end), COMMA | GT)
    {
        return Some(end);
    }
    scan_const_arg(p, i)
}

/// A constant generic argument: operands joined by arithmetic operators, such as `N * 2`.
/// Names in it carry no type arguments, which keeps the scan linear: a failed type scan never
/// re-enters nested argument lists through here.
fn scan_const_arg(p: &Parser<'_>, mut i: usize) -> Option<usize> {
    loop {
        if i > LIMIT {
            return None;
        }
        while p.raw(i) == MINUS {
            i += 1;
        }
        i = match p.raw(i) {
            INT_NUMBER | FLOAT_NUMBER | TRUE_KW | FALSE_KW => i + 1,
            IDENT => scan_dotted_name(p, i)?,
            L_PAREN => skip_balanced(p, i)?,
            _ => return None,
        };
        if matches!(p.raw(i), PLUS | MINUS | STAR | SLASH | PERCENT | SHL) {
            i += 1;
        } else {
            return Some(i);
        }
    }
}

/// `(A, B b)`: two or more elements.
fn scan_tuple_type(p: &Parser<'_>, mut i: usize, depth: u32) -> Option<usize> {
    debug_assert_eq!(p.raw(i), L_PAREN);
    i += 1;
    let mut count = 0;
    loop {
        i = scan_type_at(p, i, depth + 1)?;
        if p.raw(i) == IDENT {
            i += 1;
        }
        count += 1;
        match p.raw(i) {
            COMMA => i += 1,
            R_PAREN if count >= 2 => return Some(i + 1),
            _ => return None,
        }
    }
}

/// The index just past the bracket that matches the opening bracket at `i`.
pub(super) fn skip_balanced(p: &Parser<'_>, i: usize) -> Option<usize> {
    let mut stack: Vec<SyntaxKind> = Vec::new();
    let mut j = i;
    loop {
        if j - i > LIMIT {
            return None;
        }
        match p.raw(j) {
            L_PAREN => stack.push(R_PAREN),
            L_BRACK => stack.push(R_BRACK),
            L_CURLY => stack.push(R_CURLY),
            close @ (R_PAREN | R_BRACK | R_CURLY) => {
                if stack.pop() != Some(close) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(j + 1);
                }
            }
            EOF | SEMICOLON => return None,
            _ => {}
        }
        j += 1;
    }
}

/// At `<` after a name in an expression: whether it opens type arguments. Follows C#'s rule
/// (§12.8.9.2 of the C# spec): the list must scan as type arguments and be followed by one of
/// `( ) ] } : ; , . ? == != | ^ && || & [`, to which we add `{` for object initializers.
pub(super) fn is_generic_args_in_expr(p: &Parser<'_>) -> bool {
    let Some(end) = scan_generic_args(p, 0, 0) else {
        return false;
    };
    matches!(
        p.raw(end),
        L_PAREN
            | R_PAREN
            | R_BRACK
            | R_CURLY
            | COLON
            | SEMICOLON
            | COMMA
            | DOT
            | QUESTION
            | EQ2
            | NEQ
            | PIPE
            | CARET
            | AMP2
            | PIPE2
            | AMP
            | L_BRACK
            | L_CURLY
            | EOF
    )
}

/// At the start of a statement: whether it declares locals or a local function (`T x`).
/// Returns the index of the name.
pub(super) fn local_decl_name(p: &Parser<'_>) -> Option<usize> {
    let end = scan_type(p, 0)?;
    (p.raw(end) == IDENT).then_some(end)
}

/// After `out`/`ref`/`in`: whether an inline declaration follows (`out var x`,
/// `out float3 n`, `out var (a, b)`).
pub(super) fn is_declaration_expr(p: &Parser<'_>) -> bool {
    local_decl_name(p).is_some()
        || (p.raw(0) == IDENT && p.raw_text(0) == "var" && p.raw(1) == L_PAREN)
}

/// At `(`: whether this is a cast `(T)x`. A predefined type in the parentheses is always a
/// cast; any other type only when the next token cannot continue a binary expression.
pub(super) fn is_cast(p: &Parser<'_>) -> bool {
    let Some(end) = scan_type(p, 1) else {
        return false;
    };
    if p.raw(end) != R_PAREN {
        return false;
    }
    let after = p.raw(end + 1);
    if p.raw(1).is_predefined_type() {
        return super::exprs::can_start_expr(after) && after != L_CURLY;
    }
    matches!(
        after,
        IDENT
            | INT_NUMBER
            | FLOAT_NUMBER
            | STRING
            | CHAR
            | INTERP_START
            | L_PAREN
            | TILDE
            | BANG
            | THIS_KW
            | DEFAULT_KW
            | TRUE_KW
            | FALSE_KW
    ) || after.is_predefined_type()
}

/// At `(`: whether this is a lambda's parameter list (`(a, b) =>`).
pub(super) fn is_paren_lambda(p: &Parser<'_>) -> bool {
    skip_balanced(p, 0).is_some_and(|end| p.raw(end) == FAT_ARROW)
}

/// `A.B.C` without type arguments.
fn scan_dotted_name(p: &Parser<'_>, mut i: usize) -> Option<usize> {
    loop {
        if p.raw(i) != IDENT {
            return None;
        }
        i += 1;
        if p.raw(i) == DOT && p.raw(i + 1) == IDENT {
            i += 1;
        } else {
            return Some(i);
        }
    }
}
