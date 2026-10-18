//! `Debug.Print` and `Debug.Assert` (0004 §12.5): the message's text goes to the module's format
//! table, each hole as `{u}`, `{i}`, `{f}` or `{b}` (the IR is signless, so the kind is written
//! here, where the types are known), literal braces as `{{` and `}}`. `Debug.Assert(c, m)` is
//! `if (!c) debug.print("assertion failed: <c as written>: m")`, so backends only print.

use marlin_front_hir_def::store::{Arg, Expr, ExprId, InterpolationPart};
use marlin_front_hir_def::{BodyWithSourceMap, FileText};
use marlin_shaderc_hir_ty::InferLoc;
use marlin_shaderc_ir::{Op, UnaryOp, Value};

use crate::R;
use crate::func::{FnCx, Term};
use crate::types::{Num, Shape};

impl FnCx<'_, '_, '_> {
    /// `debug.print` (the message) and `debug.assert` (the condition, then an optional message).
    pub(crate) fn debug_op(&mut self, e: ExprId, name: &str, args: &[Arg]) -> R<()> {
        let assert = name == "debug.assert";
        let (cond, message) = match (assert, args) {
            (true, [c, rest @ ..]) => (Some(c.expr), rest.first().map(|a| a.expr)),
            (false, [m]) => (None, Some(m.expr)),
            _ => return Err(self.error("E1305", "a debug call's arguments", InferLoc::Expr(e))),
        };
        let mut format = String::new();
        if let Some(c) = cond {
            format.push_str("assertion failed: ");
            push_text(&mut format, &self.source_text(c)?);
        }
        let Some(cond) = cond else {
            let (text, values) = self.message(e, message)?;
            format.push_str(&text);
            return self.print(format, values);
        };
        let c = self.value(cond)?;
        if self.const_bits(c).is_some_and(|b| b & 1 == 1) {
            return Ok(());
        }
        let failed = self.op(Op::Unary(UnaryOp::Not), vec![c], None)?;
        let (pb, merge) = (self.new_block(), self.new_block());
        self.terminate(Term::CondBr {
            cond: failed,
            then_block: pb,
            else_block: merge,
            merge,
        });
        self.seal(pb);
        self.enter(pb);
        // The message's values are computed only when the assertion fails.
        if message.is_some() {
            format.push_str(": ");
        }
        let (text, values) = self.message(e, message)?;
        format.push_str(&text);
        self.print(format, values)?;
        self.terminate(Term::Br(merge));
        self.continue_in(merge);
        Ok(())
    }

    /// The format text and values of an interpolated string (`$"n = {n}"`).
    fn message(&mut self, e: ExprId, message: Option<ExprId>) -> R<(String, Vec<Value>)> {
        let Some(m) = message else {
            return Ok((String::new(), Vec::new()));
        };
        let Expr::Interpolated(parts) = self.body.store.exprs[m].clone() else {
            return Err(self.error(
                "E1309",
                "a debug message is an interpolated string (`$\"...\"`)",
                InferLoc::Expr(e),
            ));
        };
        let (mut text, mut values) = (String::new(), Vec::new());
        for part in parts {
            match part {
                // As written: escapes are decoded here, `{{` and `}}` stay.
                InterpolationPart::Text(t) => text.push_str(&unescape(&t)),
                InterpolationPart::Hole {
                    expr,
                    alignment,
                    format,
                } => {
                    if alignment.is_some() || format.is_some() {
                        return Err(self.error(
                            "E1309",
                            "debug output takes no alignment or format specifiers",
                            InferLoc::Expr(expr),
                        ));
                    }
                    let ty = self.concrete(&self.infer.type_of(expr));
                    let kind = match self.l.num_shape(&ty)? {
                        Some((num, Shape::Scalar | Shape::Vector(_))) => match num {
                            Num::Bool => 'b',
                            Num::Signed(_) => 'i',
                            Num::Unsigned(_) => 'u',
                            Num::Float(_) => 'f',
                        },
                        _ => {
                            return Err(self.error(
                                "E1309",
                                format!(
                                    "debug output prints scalars and vectors, not `{}`",
                                    self.l.show(&ty)
                                ),
                                InferLoc::Expr(expr),
                            ));
                        }
                    };
                    text.push('{');
                    text.push(kind);
                    text.push('}');
                    values.push(self.value(expr)?);
                }
            }
        }
        Ok((text, values))
    }

    fn print(&mut self, format: String, values: Vec<Value>) -> R<()> {
        let index = self.l.module.formats.len() as u32;
        self.l.module.formats.push(format.into());
        self.emit(Op::DebugPrint(index), values, None)?;
        Ok(())
    }

    /// The source text of `e`, as written.
    fn source_text(&mut self, e: ExprId) -> R<String> {
        let owner = self.owner();
        let (_, map) = self.l.cx.get::<BodyWithSourceMap>(&owner)?;
        let Some(ptr) = map.exprs.get(e).copied() else {
            return Ok("condition".into());
        };
        let text = self.l.cx.input::<FileText>(&owner.item.file)?;
        let range = ptr.range.start().0 as usize..ptr.range.end().0 as usize;
        Ok(text.get(range).unwrap_or("condition").to_string())
    }
}

/// `text` with its braces doubled, as format text.
fn push_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            c => out.push(c),
        }
    }
}

/// C#'s simple escapes (`\n`, `\t`, `\\`, `\"`, `\'`, `\0`) decoded; others kept as written.
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(c @ ('\\' | '"' | '\'')) => out.push(c),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}
