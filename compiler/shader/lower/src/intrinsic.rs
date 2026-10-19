//! `[Intrinsic("op")]` names (0004 §17.2): the IR operation's own name (0006 §8), so the core
//! library says exactly which operation each overload is (`fmax` for floats, `smax` for `int`,
//! `umax` for `uint`). Operations with immediates spell them with dots: `wave.reduce.fadd`,
//! `wave.scan.add.exclusive`, `barrier.group`. A few names pick the operation by the operand's
//! kind instead (`min`, `max`, `wave.reduce.sum`, ...: `BY_KIND` in `expr.rs`), so one generic
//! declaration covers every scalar and vector type.

use marlin_shaderc_ir::{
    Axis, BinaryOp, MathOp, Op, QuadOp, ReduceOp, Scope, Space, UnaryOp, VoteOp, WaveOp,
};

pub(crate) fn op(name: &str) -> Option<Op> {
    if let Some(m) = MathOp::from_name(name) {
        return Some(Op::Math(m));
    }
    if let Some(b) = BinaryOp::from_name(name) {
        return Some(Op::Binary(b));
    }
    if let Some(u) = UnaryOp::from_name(name) {
        return Some(Op::Unary(u));
    }
    Some(match name {
        "select" => Op::Select,
        "diff.detach" => Op::DiffDetach,
        "deriv.seed" => Op::DerivSeed,
        "assume.uniform" => Op::AssumeUniform,
        "assume.nonuniform" => Op::AssumeNonUniform,
        "matmul" => Op::MatMul,
        "transpose" => Op::Transpose,
        "discard" => Op::Discard,
        "is_helper" => Op::IsHelper,
        "barrier.group" => Op::Barrier {
            exec: Some(Scope::Group),
            mem: Scope::Group,
            spaces: vec![Space::GroupShared],
        },
        "barrier.device" => Op::Barrier {
            exec: Some(Scope::Group),
            mem: Scope::Device,
            spaces: vec![Space::Buffer, Space::GroupShared],
        },
        "barrier.memory" => Op::Barrier {
            exec: None,
            mem: Scope::Device,
            spaces: vec![Space::Buffer],
        },
        _ => return deriv(name).or_else(|| wave(name)),
    })
}

/// `deriv.ddx.coarse`, `deriv.ddy.fine` and the like.
fn deriv(name: &str) -> Option<Op> {
    let (axis, precision) = name.strip_prefix("deriv.")?.split_once('.')?;
    let fine = match precision {
        "fine" => true,
        "coarse" => false,
        _ => return None,
    };
    Some(Op::Deriv {
        axis: Axis::from_name(axis)?,
        fine,
    })
}

fn wave(name: &str) -> Option<Op> {
    let rest = name.strip_prefix("wave.")?;
    let mut parts = rest.split('.');
    let op = match parts.next()? {
        "reduce" => WaveOp::Reduce(ReduceOp::from_name(parts.next()?)?),
        "scan" => {
            let op = ReduceOp::from_name(parts.next()?)?;
            let exclusive = match parts.next()? {
                "exclusive" => true,
                "inclusive" => false,
                _ => return None,
            };
            WaveOp::Scan { op, exclusive }
        }
        "ballot" => WaveOp::Ballot,
        "broadcast" => WaveOp::Broadcast,
        "shuffle" => WaveOp::Shuffle,
        "quad" => WaveOp::Quad(QuadOp::from_name(parts.next()?)?),
        "vote" => WaveOp::Vote(VoteOp::from_name(parts.next()?)?),
        "match" => WaveOp::Match,
        "lane_index" => WaveOp::LaneIndex,
        "lane_count" => WaveOp::LaneCount,
        "is_first" => WaveOp::IsFirst,
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(Op::Wave { op, cluster: None })
}
