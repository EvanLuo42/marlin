use marlin_shaderc_ir::{
    BinaryOp, FuncBuilder, IntCmp, Module, Op, Scalar, ValueDef, parse_module, print_module,
};
use marlin_test_support::{data_path, golden};

/// `Sum(n)`: 0 + 1 + ... + (n - 1), as MIR lowering would build it.
fn sum_module() -> Module {
    let mut m = Module::new();
    let i32_ty = m.scalar(Scalar::I32);
    let mut b = FuncBuilder::new(&mut m, "Game.Sum", vec![i32_ty]);
    let entry = b.add_block();
    let header = b.add_block();
    let body = b.add_block();
    let cont = b.add_block();
    let exit = b.add_block();
    let n = b.param(entry, i32_ty, Some("n"));

    b.switch_to(entry);
    let zero = b.int(Scalar::I32, 0);
    b.ins(
        Op::Loop {
            header,
            continuing: cont,
            merge: exit,
        },
        vec![zero, zero],
    );

    b.switch_to(header);
    let i = b.param(header, i32_ty, Some("i"));
    let acc = b.param(header, i32_ty, Some("acc"));
    let done = b.value(Op::Icmp(IntCmp::Sge), vec![i, n]);
    b.ins(
        Op::CondBr {
            then_block: exit,
            then_args: 1,
            else_block: body,
            merge: body,
        },
        vec![done, acc],
    );

    b.switch_to(body);
    let next_acc = b.binary(BinaryOp::Add, acc, i);
    b.br(cont, vec![next_acc]);

    b.switch_to(cont);
    let acc2 = b.param(cont, i32_ty, Some("acc"));
    let one = b.int(Scalar::I32, 1);
    let next_i = b.binary(BinaryOp::Add, i, one);
    b.br(header, vec![next_i, acc2]);

    b.switch_to(exit);
    let result = b.param(exit, i32_ty, Some("result"));
    b.ret(vec![result]);
    m
}

#[test]
fn built_functions_print_and_parse_back() {
    let m = sum_module();
    let printed = print_module(&m);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "builder/sum.txt"),
        &printed,
    );
    let again = parse_module(&printed).unwrap();
    assert_eq!(print_module(&again), printed);
}

#[test]
fn use_lists_follow_edits() {
    let mut m = sum_module();
    let fid = m.find_function("Game.Sum").unwrap();
    let f = &mut m.funcs[fid];
    let body = f.layout[2];
    let add = f.first_inst(body).unwrap();
    let (acc, i) = (f.operands(add)[0], f.operands(add)[1]);
    let i_uses = f.uses(i).len();

    // `acc + i` → `i + i`: `acc` loses a use, `i` gains one.
    let acc_uses = f.uses(acc).len();
    f.set_operand(add, 0, i);
    assert_eq!(f.uses(acc).len(), acc_uses - 1);
    assert_eq!(f.uses(i).len(), i_uses + 1);

    // Every use of the sum now reads `i` directly; the add is dead and goes away.
    let sum = f.result(add);
    f.replace_all_uses(sum, i);
    assert!(f.uses(sum).is_empty());
    f.remove_inst(add);
    assert_eq!(
        f.uses(i).len(),
        i_uses,
        "the add's two uses of `i` left; the branch's arrived in place of the old `acc + i`"
    );
    assert_eq!(f.insts(body).count(), 1);
    for (v, data) in f.values.iter() {
        for u in f.uses(v) {
            assert_eq!(f.operands(u.inst)[u.index as usize], v, "{data:?}");
        }
    }

    // Insert before the branch: a constant operand is the same value everywhere.
    let br = f.first_inst(body).unwrap();
    let ty = f.ty(i);
    let c = m.int(ty, 1);
    let f = &mut m.funcs[fid];
    let one = f.const_value(c, ty);
    assert!(matches!(f.values[one].def, ValueDef::Const(_)));
    let inc = f.create_inst(Op::Binary(BinaryOp::Add), vec![i, one], &[ty]);
    f.insert_before(br, inc);
    let insts: Vec<_> = f.insts(body).collect();
    assert_eq!(insts, vec![inc, br]);
    assert_eq!(f.prev_inst(br), Some(inc));
    let printed = print_module(&m);
    assert!(
        printed.contains("    %1 = add %i, i32 1\n    br block3(%i)"),
        "{printed}"
    );
}

#[test]
fn built_functions_verify() {
    let errors = marlin_shaderc_ir::verify_module(&sum_module());
    assert!(errors.is_empty(), "{errors:?}");
}
