//! A plain disassembly, for golden files and messages: one instruction per line, results as
//! `%n`, other ids as `%n`, literals as numbers, strings quoted.

use std::fmt::Write as _;

use crate::spirv::op;

/// Opcodes without a result id.
fn has_result(opcode: u32) -> bool {
    !matches!(
        opcode,
        op::Nop
            | op::Name
            | op::MemberName
            | op::Extension
            | op::MemoryModel
            | op::EntryPoint
            | op::ExecutionMode
            | op::ExecutionModeId
            | op::Capability
            | op::Decorate
            | op::DecorateId
            | op::MemberDecorate
            | op::Store
            | op::FunctionEnd
            | op::LoopMerge
            | op::SelectionMerge
            | op::Branch
            | op::BranchConditional
            | op::Switch
            | op::Return
            | op::ReturnValue
            | op::Unreachable
            | op::ControlBarrier
            | op::MemoryBarrier
            | op::AtomicStore
    )
}

/// Opcodes whose result comes without a result type (types, labels, imports, strings).
fn result_only(opcode: u32) -> bool {
    matches!(opcode, op::ExtInstImport | op::String | op::Label)
        || crate::spirv::op::name(opcode).is_some_and(|n| n.starts_with("Type"))
}

/// Opcodes with a literal string operand, and where it starts.
fn string_at(opcode: u32) -> Option<usize> {
    match opcode {
        op::Extension => Some(0),
        op::ExtInstImport | op::String | op::Name => Some(1),
        op::MemberName => Some(2),
        op::EntryPoint => Some(2),
        _ => None,
    }
}

fn decode(words: &[u32]) -> (String, usize) {
    let mut bytes = Vec::new();
    let mut used = 0;
    for &w in words {
        used += 1;
        let b = w.to_le_bytes();
        let end = b.iter().position(|&x| x == 0);
        bytes.extend_from_slice(&b[..end.unwrap_or(4)]);
        if end.is_some() {
            break;
        }
    }
    (String::from_utf8_lossy(&bytes).into_owned(), used)
}

/// The module as text; malformed words end it with a note.
pub fn disassemble(words: &[u32]) -> String {
    let mut out = String::new();
    if words.len() < 5 {
        return "; not a SPIR-V module\n".into();
    }
    let _ = writeln!(
        out,
        "; SPIR-V {}.{}, bound {}",
        words[1] >> 16,
        (words[1] >> 8) & 0xff,
        words[3]
    );
    let mut at = 5;
    while at < words.len() {
        let count = (words[at] >> 16) as usize;
        let opcode = words[at] & 0xffff;
        if count == 0 || at + count > words.len() {
            let _ = writeln!(out, "; truncated instruction at word {at}");
            break;
        }
        let ops = &words[at + 1..at + count];
        let name = op::name(opcode).map_or_else(|| format!("Op{opcode}"), |n| format!("Op{n}"));
        let mut line = String::new();
        let mut rest: &[u32] = ops;
        if has_result(opcode) {
            if result_only(opcode) {
                let _ = write!(line, "%{} = {name}", rest.first().copied().unwrap_or(0));
                rest = &rest[1.min(rest.len())..];
            } else if rest.len() >= 2 {
                let _ = write!(line, "%{} = {name} %{}", rest[1], rest[0]);
                rest = &rest[2..];
            } else {
                line.push_str(&name);
            }
        } else {
            line.push_str(&name);
        }
        let consumed = ops.len() - rest.len();
        let string = string_at(opcode).and_then(|s| s.checked_sub(consumed));
        let mut i = 0;
        while i < rest.len() {
            if Some(i) == string {
                let (s, used) = decode(&rest[i..]);
                let _ = write!(line, " {s:?}");
                i += used;
                continue;
            }
            let _ = write!(line, " {}", rest[i]);
            i += 1;
        }
        if opcode == op::Function || opcode == op::Label {
            out.push('\n');
        }
        let _ = writeln!(out, "{line}");
        at += count;
    }
    out
}
