//! [`Document`] to canonical text.
//!
//! One layout for any given tree, so saving never produces spurious diffs: 4-space indents,
//! Allman braces for objects and multi-line values, a blank line between objects. A value stays
//! on its line when it fits in [`MAX_WIDTH`] columns, else it breaks one element per line.
//! Inline sequences separate elements with `, `; broken ones put one per line without commas.
//! Comments are not kept: files are written by tools.

use std::fmt::Write;

use crate::ast::{Attr, Document, FileRef, LinkRef, Node, NodeKind, Object};

/// Column limit for keeping a value on one line.
pub const MAX_WIDTH: usize = 100;

const INDENT: &str = "    ";

/// The canonical text of `doc`.
pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "!Version {}", doc.version);
    for object in &doc.objects {
        out.push('\n');
        write_object(&mut out, object);
    }
    out
}

/// Appends one object block.
pub fn write_object(out: &mut String, object: &Object) {
    out.push_str(&object.type_name);
    out.push_str("\n{\n");
    let _ = writeln!(out, "{INDENT}!Uuid = \"{}\"", object.uuid);
    let _ = write!(out, "{INDENT}!Name = ");
    write_string(out, &object.name);
    out.push('\n');
    if object.type_version != 0 {
        let _ = writeln!(out, "{INDENT}!Version = {}", object.type_version);
    }
    for attr in &object.attrs {
        write_attr(out, attr, 1);
    }
    out.push_str("}\n");
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str(INDENT);
    }
}

fn write_attr(out: &mut String, attr: &Attr, level: usize) {
    indent(out, level);
    out.push_str(&attr.key);
    out.push_str(" =");
    let used = level * INDENT.len() + attr.key.len() + 3;
    let inline = inline(&attr.value);
    if used + inline.len() <= MAX_WIDTH || !breakable(&attr.value) {
        out.push(' ');
        out.push_str(&inline);
        out.push('\n');
    } else {
        write_broken(out, &attr.value, level, true);
    }
}

/// Whether a node has a multi-line form.
fn breakable(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Call(_, items) | NodeKind::Tuple(items) | NodeKind::List(items) => {
            !items.is_empty()
        }
        NodeKind::Record(_, attrs) => !attrs.is_empty(),
        _ => false,
    }
}

/// A value as an element of a broken sequence: its own line(s) at `level`.
fn write_element(out: &mut String, node: &Node, level: usize) {
    let inline = inline(node);
    if level * INDENT.len() + inline.len() <= MAX_WIDTH || !breakable(node) {
        indent(out, level);
        out.push_str(&inline);
        out.push('\n');
    } else {
        indent(out, level);
        write_broken(out, node, level, false);
    }
}

/// The multi-line form. With `after_eq` the caller has written `Key =` and nothing else on the
/// line; otherwise the caller has indented the start of the line.
fn write_broken(out: &mut String, node: &Node, level: usize, after_eq: bool) {
    // Where the line stands: a tag continues it, a bare bracket goes on the next line.
    let head = |out: &mut String, tag: Option<&str>| match (tag, after_eq) {
        (Some(tag), true) => {
            out.push(' ');
            out.push_str(tag);
            out.push('\n');
        }
        (Some(tag), false) => {
            out.push_str(tag);
            out.push('\n');
        }
        (None, true) => out.push('\n'),
        (None, false) => {}
    };
    let open = |out: &mut String, bracket: char, tagged_or_after_eq: bool| {
        if tagged_or_after_eq {
            indent(out, level);
        }
        out.push(bracket);
        out.push('\n');
    };
    match &node.kind {
        NodeKind::Record(tag, attrs) => {
            head(out, tag.as_deref());
            open(out, '{', tag.is_some() || after_eq);
            for attr in attrs {
                write_attr(out, attr, level + 1);
            }
            indent(out, level);
            out.push_str("}\n");
        }
        NodeKind::Call(tag, items) => {
            head(out, Some(tag));
            open(out, '(', true);
            for item in items {
                write_element(out, item, level + 1);
            }
            indent(out, level);
            out.push_str(")\n");
        }
        NodeKind::Tuple(items) | NodeKind::List(items) => {
            let (o, c) = match node.kind {
                NodeKind::List(_) => ('[', ']'),
                _ => ('(', ')'),
            };
            head(out, None);
            open(out, o, after_eq);
            for item in items {
                write_element(out, item, level + 1);
            }
            indent(out, level);
            out.push(c);
            out.push('\n');
        }
        _ => {
            if after_eq {
                out.push(' ');
            }
            out.push_str(&inline(node));
            out.push('\n');
        }
    }
}

/// The one-line form of any value.
pub fn inline(node: &Node) -> String {
    let mut out = String::new();
    write_inline(&mut out, node);
    out
}

fn write_inline(out: &mut String, node: &Node) {
    let seq = |out: &mut String, items: &[Node], open: char, close: char| {
        out.push(open);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_inline(out, item);
        }
        out.push(close);
    };
    match &node.kind {
        NodeKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        NodeKind::Int(v) => {
            let _ = write!(out, "{v}");
        }
        NodeKind::Float(v) => write_float(out, *v),
        NodeKind::Str(s) => write_string(out, s),
        NodeKind::Ident(word) => out.push_str(word),
        NodeKind::Call(tag, items) => {
            out.push_str(tag);
            seq(out, items, '(', ')');
        }
        NodeKind::Record(tag, attrs) => {
            if let Some(tag) = tag {
                out.push_str(tag);
                out.push(' ');
            }
            if attrs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, attr) in attrs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&attr.key);
                out.push_str(" = ");
                write_inline(out, &attr.value);
            }
            out.push_str(" }");
        }
        NodeKind::Tuple(items) => seq(out, items, '(', ')'),
        NodeKind::List(items) => seq(out, items, '[', ']'),
        NodeKind::Link(link) => write_link(out, link),
        NodeKind::File(file) => write_file(out, file),
    }
}

/// Shortest text that reads back as the same `f64`, always marked as a float.
fn write_float(out: &mut String, v: f64) {
    if v.is_nan() {
        out.push_str("nan");
    } else if v.is_infinite() {
        out.push_str(if v > 0.0 { "inf" } else { "-inf" });
    } else {
        // `Debug` is shortest round-trip and keeps `.0` on whole numbers; it switches to exponent
        // form for very large and small magnitudes, which the lexer reads too.
        let _ = write!(out, "{v:?}");
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_link(out: &mut String, link: &LinkRef) {
    out.push('<');
    out.push_str(&link.path);
    out.push(':');
    out.push_str(&link.name);
    if let Some(uuid) = link.uuid {
        let _ = write!(out, " #{uuid}");
    }
    out.push('>');
}

fn write_file(out: &mut String, file: &FileRef) {
    let _ = write!(out, "`{}:{}`", file.scheme, file.path);
}
