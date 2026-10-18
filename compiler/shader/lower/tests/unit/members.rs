use marlin_shaderc_ir::Val;

use crate::support::{World, world};

fn members() -> World {
    world(&["game/Members.mshader"])
}

#[test]
fn properties_and_indexers_call_their_accessors() {
    let w = members();
    // Width 2 -> Side = 3 -> Area 9 + this[2] = 6.
    assert_eq!(w.run("Game.UseProps", &[Val::f32(2.0)])[0], Val::f32(15.0));
    // b[2] = 8 sets Width to 8 / 2.
    assert_eq!(w.run("Game.UseIndexerSet", &[])[0], Val::f32(4.0));
}

#[test]
fn extension_members_take_their_receiver() {
    let w = members();
    let v = Val::Agg([1.0f32, 2.0, 3.0].iter().map(|&x| Val::f32(x)).collect());
    // Sum 6 + Scaled(2) 12 + First 1.
    assert_eq!(w.run("Game.UseExtensions", &[v])[0], Val::f32(19.0));
}
