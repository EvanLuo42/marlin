use marlin_shaderc_ir::Val;

use crate::support::{World, world};

fn basics() -> World {
    world(&["game/Basics.mshader"])
}

fn int(w: &World, path: &str, args: &[i32]) -> i32 {
    let args: Vec<Val> = args.iter().map(|&a| Val::i32(a)).collect();
    w.run(path, &args)[0].as_i32().unwrap()
}

fn float(w: &World, path: &str, args: &[f32]) -> f32 {
    let args: Vec<Val> = args.iter().map(|&a| Val::f32(a)).collect();
    w.run(path, &args)[0].as_f32().unwrap()
}

fn vec3(v: [f32; 3]) -> Val {
    Val::Agg(v.iter().map(|&x| Val::f32(x)).collect())
}

#[test]
fn arithmetic_follows_the_sign_of_the_types() {
    let w = basics();
    // (7 + 2) * 3 - 7 / 2 + 7 % 2 = 27 - 3 + 1
    assert_eq!(int(&w, "Game.Arith", &[7, 2]), 25);
    assert_eq!(int(&w, "Game.Arith", &[-7, 2]), -15 + 3 - 1);
    // Dividing by zero gives 0 (0004 §9.6).
    assert_eq!(int(&w, "Game.Arith", &[5, 0]), 15);
    let r = w.run("Game.Unsigned", &[Val::u32(u32::MAX), Val::u32(2)]);
    let want = (u32::MAX / 2).wrapping_add(u32::MAX >> 1);
    assert_eq!(r[0], Val::u32(want), "unsigned division and logical shift");
    assert_eq!(float(&w, "Game.Mix", &[1.0, 3.0, 0.25]), 1.5);
}

#[test]
fn vectors_swizzles_and_intrinsics() {
    let w = basics();
    let r = w.run("Game.Vec", &[vec3([1.0, 2.0, 3.0]), Val::f32(2.0)]);
    // w = (2, 4, 6); w.y = 10; w.xz = (w.z, w.x) = (6, 2); + 1
    assert_eq!(r[0], vec3([7.0, 11.0, 3.0]));
    let r = w.run("Game.Length", &[vec3([2.0, 3.0, 6.0])]);
    assert_eq!(r[0], Val::f32(7.0));
    let m = w.run(
        "Game.Mat",
        &[
            Val::Agg(
                (0..4)
                    .map(|c| {
                        Val::Agg(
                            (0..4)
                                .map(|r| Val::f32(if r == c { 2.0 } else { 0.0 }))
                                .collect(),
                        )
                    })
                    .collect(),
            ),
            Val::Agg(
                [1.0f32, 2.0, 3.0, 4.0]
                    .iter()
                    .map(|&x| Val::f32(x))
                    .collect(),
            ),
        ],
    );
    assert_eq!(m[0], Val::f32(2.0));
}

#[test]
fn loops_break_continue_and_foreach() {
    let w = basics();
    assert_eq!(
        int(&w, "Game.Loops", &[10]),
        1 + 2 + 4 + 5 + 6,
        "skips 3, stops at 7"
    );
    assert_eq!(int(&w, "Game.Loops", &[2]), 1);
    assert_eq!(int(&w, "Game.Collatz", &[6]), 8);
    assert_eq!(int(&w, "Game.Collatz", &[1]), 0);
    assert_eq!(int(&w, "Game.Do", &[0]), 1, "the body runs once");
    assert_eq!(int(&w, "Game.Do", &[3]), 3);
    assert_eq!(int(&w, "Game.Range", &[5]), 10);
    assert_eq!(float(&w, "Game.Array", &[]), 20.0);
}

#[test]
fn switches_patterns_and_enums() {
    let w = basics();
    // Mode.A = 0, Mode.B = 5, Mode.C = 6.
    assert_eq!(int(&w, "Game.Switch", &[0]), 1);
    assert_eq!(int(&w, "Game.Switch", &[5]), 2);
    assert_eq!(int(&w, "Game.Switch", &[6]), 3);
    assert_eq!(int(&w, "Game.ModeValue", &[6]), 6);
    for (x, want) in [(0, 10), (3, 20), (5, 30), (-1, 30)] {
        assert_eq!(int(&w, "Game.SwitchExpr", &[x]), want, "SwitchExpr({x})");
    }
}

#[test]
fn structs_methods_constructors_and_defaults() {
    let w = basics();
    // Intensity starts at its default 2, times 3; Color.y is 2.
    assert_eq!(float(&w, "Game.Struct", &[3.0]), 8.0);
    assert_eq!(int(&w, "Game.Methods", &[]), 14);
    assert_eq!(int(&w, "Game.Ctor", &[]), 12);
    assert_eq!(int(&w, "Game.UseDefaults", &[]), 8 + 3);
    assert_eq!(int(&w, "Game.ConstLen", &[]), 3);
}

#[test]
fn ref_out_tuples_and_short_circuits() {
    let w = basics();
    assert_eq!(int(&w, "Game.UseSwap", &[1, 2]), 21);
    assert_eq!(int(&w, "Game.UseOut", &[10]), 5);
    assert_eq!(int(&w, "Game.UseOut", &[4]), -1, "h = 2 is not > 2");
    assert_eq!(int(&w, "Game.UseOut", &[7]), -1, "odd");
    let logic = |a: i32, b: i32| w.run("Game.Logic", &[Val::i32(a), Val::i32(b)])[0].clone();
    assert_eq!(logic(1, 5), Val::bool(true));
    assert_eq!(logic(2, 1), Val::bool(false));
    assert_eq!(logic(0, 9), Val::bool(true));
    assert_eq!(int(&w, "Game.Tuple", &[4]), 20);
}

#[test]
fn what_is_not_lowered_yet_is_a_located_error() {
    use marlin_shaderc_lower::Lowering;
    let w = world(&["game/Later.mshader"]);
    let snapshot = w.db.snapshot();
    for (path, want) in [
        ("Game.UsesProperty", "a property is not lowered yet"),
        (
            "Game.Apply",
            "`Func` and `Action` parameters are not lowered yet",
        ),
    ] {
        let mut l = Lowering::new(&snapshot, w.game).unwrap();
        let err = l
            .function(w.item(path))
            .and_then(|_| l.finish().map(|_| ()))
            .unwrap_err();
        let d = err.to_diagnostic(&snapshot).unwrap().expect("a diagnostic");
        let shown = marlin_lang_diag::render_all(&[d], &w);
        assert!(
            shown.contains("E1301") && shown.contains(want),
            "{path}: {shown}"
        );
        assert!(
            shown.contains("game/Later.mshader:"),
            "{path}: located: {shown}"
        );
    }
}
