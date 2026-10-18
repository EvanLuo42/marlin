use marlin_shaderc_ir::{Val, print_module};

use crate::support::{World, world};

fn generics() -> World {
    world(&["game/Generics.mshader"])
}

fn ints(vals: &[i32]) -> Vec<Val> {
    vals.iter().map(|&a| Val::i32(a)).collect()
}

#[test]
fn generic_functions_get_one_instance_per_type() {
    let w = generics();
    assert_eq!(w.run("Game.TwiceInt", &ints(&[21]))[0], Val::i32(42));
    assert_eq!(
        w.run("Game.TwiceFloat", &[Val::f32(1.25)])[0],
        Val::f32(2.5)
    );
    let v = |xs: [f32; 3]| Val::Agg(xs.iter().map(|&x| Val::f32(x)).collect());
    assert_eq!(
        w.run("Game.TwiceVec", &[v([1.0, 2.0, 3.0])])[0],
        v([2.0, 4.0, 6.0])
    );
    let m = w.lower(&["Game.TwiceInt", "Game.TwiceFloat"]);
    let text = print_module(&m);
    assert!(
        text.contains("@\"Game.Twice<int>\"") && text.contains("@\"Game.Twice<float>\""),
        "{text}"
    );
}

#[test]
fn constant_parameters_and_compile_time_recursion() {
    let w = generics();
    assert_eq!(w.run("Game.SumOf3", &ints(&[1, 2, 3]))[0], Val::i32(6));
    assert_eq!(w.run("Game.Fact5", &[])[0], Val::i32(120));
    // Each N is its own instance; `if (N <= 1)` folds, so the recursion ends at 1.
    let text = print_module(&w.lower(&["Game.Fact5"]));
    assert!(
        text.contains("@\"Game.Fact<1>\"") && !text.contains("Fact<0>"),
        "{text}"
    );
}

#[test]
fn generic_structs_interfaces_and_associated_types() {
    let w = generics();
    assert_eq!(w.run("Game.UseBox", &[Val::f32(1.5)])[0], Val::f32(3.5));
    assert_eq!(
        w.run("Game.Areas", &[Val::f32(2.0), Val::f32(3.0)])[0],
        Val::f32(10.0)
    );
    assert_eq!(w.run("Game.TwiceMoney", &ints(&[7]))[0], Val::i32(14));
    assert_eq!(w.run("Game.MakeFloat", &[])[0], Val::f32(1.0));
}
