use marlin_shaderc_ir::Val;

use crate::support::{World, world};

fn closures() -> World {
    world(&["game/Closures.mshader"])
}

#[test]
fn lambdas_capture_locals_by_value() {
    let w = closures();
    // The mean of 3x over the midpoints of four quarters.
    assert_eq!(
        w.run("Game.MeanOfScaled", &[Val::f32(3.0)])[0],
        Val::f32(1.5)
    );
    // 1 + 1 with `seen` as it was, then 0 + 100.
    assert_eq!(w.run("Game.CapturesByValue", &[])[0], Val::i32(102));
}

#[test]
fn funcs_forward_and_method_groups_bind() {
    let w = closures();
    assert_eq!(
        w.run("Game.ForwardsFunc", &[Val::f32(2.5)])[0],
        Val::f32(5.0)
    );
    // Midpoints 0.25 and 0.75: (0.0625 + 0.5625) / 2.
    assert_eq!(w.run("Game.PassesMethodGroup", &[])[0], Val::f32(0.3125));
}

#[test]
fn local_functions_and_this_captures() {
    let w = closures();
    // Both(1) = 1 + 2a, AddA(10) = 10 + a.
    assert_eq!(
        w.run("Game.LocalFunctions", &[Val::i32(3)])[0],
        Val::i32(20)
    );
    assert_eq!(w.run("Game.UsesThis", &[Val::i32(4)])[0], Val::i32(9));
    assert_eq!(
        w.run("Game.UsesMethodGroupOfThis", &[Val::i32(4)])[0],
        Val::i32(5)
    );
}
