//! Automatic differentiation through the whole pipeline (0004 §15): `ForwardDiff` and
//! `Derivatives.Seed` in Marlin source, lowered, optimized and run in the interpreter, against
//! the derivatives worked out by hand or by central differences of the function itself.

use marlin_shaderc_ir::Val;
use marlin_test_support::data_path;

use crate::support::{World, world};

fn source() -> World {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), "Differentiation.mshader");
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    world(&text)
}

fn f(v: &[Val]) -> f32 {
    v[0].as_f32().unwrap()
}

fn fs(v: &[Val]) -> Vec<f32> {
    match &v[0] {
        Val::Agg(items) => items.iter().map(|x| x.as_f32().unwrap()).collect(),
        other => panic!("not a vector: {other:?}"),
    }
}

fn v2(x: f32, y: f32) -> Val {
    Val::Agg(vec![Val::f32(x), Val::f32(y)])
}

fn v3(v: [f32; 3]) -> Val {
    Val::Agg(v.iter().map(|&x| Val::f32(x)).collect())
}

fn close(a: f32, b: f32, tolerance: f32, what: &str) {
    assert!(
        (a - b).abs() <= tolerance * b.abs().max(1.0),
        "{what}: {a} is not {b}"
    );
}

#[test]
fn source_type_checks() {
    assert_eq!(source().diagnostics(), "");
}

#[test]
fn forward_derivatives_of_scalar_functions() {
    let w = source();
    for x in [-1.5f32, -0.25, 0.0, 0.3, 2.0] {
        close(
            f(&w.run_optimized("Game.DPoly", &[Val::f32(x)])),
            3.0 * x * x - 2.0,
            1e-5,
            "DPoly",
        );
        close(
            f(&w.run_optimized("Game.VPoly", &[Val::f32(x)])),
            x * x * x - 2.0 * x,
            1e-5,
            "VPoly",
        );
        close(
            f(&w.run_optimized("Game.DSin", &[Val::f32(x)])),
            x.cos(),
            1e-5,
            "DSin",
        );
        assert_eq!(
            f(&w.run_optimized("Game.DDetached", &[Val::f32(x)])),
            x,
            "DDetached"
        );
        close(
            f(&w.run_optimized("Game.D2Cube", &[Val::f32(x)])),
            6.0 * x,
            1e-5,
            "D2Cube",
        );
    }
    for (x, n) in [(0.5f32, 4), (1.25, 3), (-0.75, 6), (2.0, 0)] {
        let want: f32 = (1..=n).map(|k| x.powi(k - 1)).sum();
        let got = f(&w.run_optimized("Game.DSeries", &[Val::f32(x), Val::i32(n)]));
        close(got, want, 1e-5, &format!("DSeries({x}, {n})"));
    }
    for x in [0.1f32, 0.4, 0.6, 1.5] {
        let want = if x < 0.5 {
            x.cos()
        } else {
            // e^x (1.25 x) differentiated.
            x.exp() * 1.25 * x + x.exp() * 1.25
        };
        close(
            f(&w.run_optimized("Game.DPiecewise", &[Val::f32(x)])),
            want,
            1e-5,
            "DPiecewise",
        );
    }
}

#[test]
fn forward_derivatives_match_central_differences() {
    let w = source();
    let shade = |n: [f32; 3], l: [f32; 3], r: f32| -> Vec<f32> {
        fs(&w.run("Game.Shade", &[v3(n), v3(l), Val::f32(r)]))
    };
    let cases = [
        (
            [0.3f32, 0.8, 0.5],
            [0.6f32, 0.2, 0.7],
            [0.1f32, -0.4, 0.2],
            [0.3f32, 0.1, -0.2],
        ),
        (
            [1.0, 0.2, -0.1],
            [0.4, 0.4, 0.8],
            [-0.2, 0.5, 0.3],
            [0.0, 0.0, 0.0],
        ),
    ];
    for (n, l, dn, dl) in cases {
        let got = fs(&w.run_optimized(
            "Game.DShade",
            &[v3(n), v3(dn), v3(l), v3(dl), Val::f32(0.75)],
        ));
        let h = 1e-3f32;
        let at = |k: f32| {
            let n2 = [n[0] + k * dn[0], n[1] + k * dn[1], n[2] + k * dn[2]];
            let l2 = [l[0] + k * dl[0], l[1] + k * dl[1], l[2] + k * dl[2]];
            shade(n2, l2, 0.75)
        };
        let (p, q) = (at(h), at(-h));
        for c in 0..3 {
            close(
                got[c],
                (p[c] - q[c]) / (2.0 * h),
                2e-3,
                &format!("DShade[{c}]"),
            );
        }
    }
}

#[test]
fn screen_derivatives_come_from_seeds() {
    let w = source();
    let (u, v) = (0.4f32, 0.7f32);
    let (dx, dy) = ((0.01f32, -0.02f32), (0.03f32, 0.005f32));
    let got = fs(&w.run_optimized("Game.Screen", &[v2(u, v), v2(dx.0, dx.1), v2(dy.0, dy.1)]));
    close(
        got[0],
        3.0 * (3.0 * u).cos() * dx.0 + dx.0.abs() + dy.0.abs(),
        1e-5,
        "Screen.x",
    );
    close(got[1], 3.0 * (3.0 * v).cos() * dy.1, 1e-5, "Screen.y");
    let got = f(&w.run_optimized(
        "Game.ScreenSurface",
        &[v2(u, v), v2(dx.0, dx.1), v2(dy.0, dy.1)],
    ));
    close(got, 4.0 * (4.0 * u).cos() * dx.0, 1e-5, "ScreenSurface");
}
