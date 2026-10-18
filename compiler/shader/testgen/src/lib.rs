//! Random IR programs for differential tests (0005 §4.6), shared by the passes' and the
//! backends' tests: [`random::function`] makes structured functions; [`compute_entry`] puts one
//! in a compute entry point, so a program can run in the interpreter and on a GPU and the two
//! compared.

pub mod random;

use marlin_shaderc_ir::{
    Binding, Builtin, FuncBuilder, FuncId, GlobalData, Module, Op, ResKind, ResourceKind, Scalar,
    Space, Stage, Val,
};

pub use random::{arg, function};

/// The thread-group size of [`compute_entry`].
pub const GROUP: u32 = 64;

/// A compute entry point `@test` that calls `f` (`(int, int, int) -> int`) once per thread
/// with the push constants' three arguments, the first plus the thread's index in the group,
/// and stores the result at that index of the buffer whose resource index the push constants
/// hold last: `struct { int a, b, c; Buffer<int> out; }`.
pub fn compute_entry(m: &mut Module, f: FuncId) -> FuncId {
    let int = m.scalar(Scalar::I32);
    let out_ty = m.res(ResKind {
        kind: ResourceKind::Buffer,
        element: Some(int),
    });
    let args = m.add_struct(
        "Inputs",
        vec![
            ("a".into(), int),
            ("b".into(), int),
            ("c".into(), int),
            ("out".into(), out_ty),
        ],
    );
    let args_ty = m.struct_ty(args);
    let push_ptr = m.ptr(Space::Push, false, args_ty);
    let push = m.add_global(GlobalData {
        name: "inputs".into(),
        ty: push_ptr,
        binding: Binding::None,
        interp: None,
        init: None,
    });
    let gi_ptr = m.ptr(Space::Input, false, int);
    let gi = m.add_global(GlobalData {
        name: "gi".into(),
        ty: gi_ptr,
        binding: Binding::Builtin(Builtin::GroupIndex),
        interp: None,
        init: None,
    });
    let mut b = FuncBuilder::new(m, "test", Vec::new());
    let block = b.add_block();
    b.switch_to(block);
    let pp = b.global(push);
    let inputs = b.value(Op::Load { aligned: None }, vec![pp]);
    let gp = b.global(gi);
    let lane = b.value(Op::Load { aligned: None }, vec![gp]);
    let a0 = b.value(Op::ExtractField(0), vec![inputs]);
    let a = b.binary(marlin_shaderc_ir::BinaryOp::Add, a0, lane);
    let bb = b.value(Op::ExtractField(1), vec![inputs]);
    let c = b.value(Op::ExtractField(2), vec![inputs]);
    let r = b.value(Op::Call(f), vec![a, bb, c]);
    let out = b.value(Op::ExtractField(3), vec![inputs]);
    let h = b.value(Op::DescGet { nonuniform: false }, vec![out]);
    let p = b.value(Op::BufferElem, vec![h, lane]);
    b.ins(Op::Store { aligned: None }, vec![p, r]);
    b.ret(Vec::new());
    let entry = b.func;
    m.entries.push(marlin_shaderc_ir::Entry {
        func: entry,
        stage: Stage::Compute([GROUP, 1, 1]),
        uses: vec![push, gi],
    });
    entry
}

/// The push constants [`compute_entry`] takes: the three arguments and buffer 0.
pub fn inputs(a: i32, b: i32, c: i32) -> Val {
    Val::Agg(vec![Val::i32(a), Val::i32(b), Val::i32(c), Val::u32(0)])
}
