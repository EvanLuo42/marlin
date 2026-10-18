//! The autodiff pass (0005 §4.4 pass 0.3): derivatives against central differences in the
//! interpreter, and what it makes of screen-space derivatives and samples (golden files).

use marlin_math::Rng;
use marlin_shaderc_ir::{
    FuncId, Interpreter, Module, Op, TexArg, Type, TypeData, Val, parse_module, print_module,
    type_name, verify_module,
};
use marlin_shaderc_opt::{Autodiff, Inline, Mem2Reg, PassManager, SimplifyCfg, Sroa};
use marlin_test_support::{data_path, golden, seed};

fn read(name: &str) -> String {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), &format!("autodiff/{name}"));
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Stage 1 up to the pass, and the pass: what the pipeline runs it after.
fn stage1() -> PassManager {
    let mut pm = PassManager::new(true);
    pm.add(Inline)
        .add(SimplifyCfg)
        .add(Sroa)
        .add(Mem2Reg)
        .add(Autodiff::default());
    pm
}

/// `ForwardDiff` of `@name` in all its parameters, as lowering writes it: the parameters,
/// then their tangents; the tangent of the result.
fn wrapper(m: &Module, id: FuncId) -> String {
    let f = &m.funcs[id];
    let types: Vec<String> = f.params().iter().map(|&p| type_name(m, f.ty(p))).collect();
    let result = type_name(m, f.results[0]);
    let mut params = Vec::new();
    for (i, t) in types.iter().enumerate() {
        params.push(format!("%x{i}: {t}"));
    }
    for (i, t) in types.iter().enumerate() {
        params.push(format!("%t{i}: {t}"));
    }
    let mut body = String::from("    %c = diff.channel\n");
    let mut seeded = Vec::new();
    for i in 0..types.len() {
        body.push_str(&format!("    %s{i} = diff.seed %c, %x{i}, %t{i}\n"));
        seeded.push(format!("%s{i}"));
    }
    body.push_str(&format!(
        "    %r = call @{}({})\n    %d = diff.tangent %c, %r\n    return %d\n",
        f.name,
        seeded.join(", ")
    ));
    format!(
        "func @{}.d({}) -> {result} {{\nblock0:\n{body}}}\n",
        f.name,
        params.join(", ")
    )
}

/// A random value of an `f64` scalar or vector type: components from `pick`.
fn random(m: &Module, t: Type, pick: &mut dyn FnMut() -> f64) -> Val {
    match m.data(t) {
        TypeData::Vector(_, n) => Val::Agg((0..*n).map(|_| Val::f64(pick())).collect()),
        _ => Val::f64(pick()),
    }
}

fn components(v: &Val) -> Vec<f64> {
    match v {
        Val::Agg(parts) => parts.iter().flat_map(components).collect(),
        Val::Bits(b) => vec![f64::from_bits(*b)],
        other => panic!("not a number: {other:?}"),
    }
}

/// `x + k t`, component by component.
fn step(x: &Val, t: &Val, k: f64) -> Val {
    match (x, t) {
        (Val::Agg(a), Val::Agg(b)) => {
            Val::Agg(a.iter().zip(b).map(|(a, b)| step(a, b, k)).collect())
        }
        _ => {
            let (a, b) = (components(x)[0], components(t)[0]);
            Val::f64(a + k * b)
        }
    }
}

fn unit(rng: &mut Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[test]
fn derivatives_match_finite_differences() {
    let text = read("fd.ir");
    let fixture = parse_module(&text).unwrap();
    let mut all = text.clone();
    let mut tested = Vec::new();
    for (id, f) in fixture.funcs.iter() {
        if matches!(&*f.name, "square" | "detach") {
            continue;
        }
        all.push_str(&wrapper(&fixture, id));
        tested.push(f.name.to_string());
    }
    let before = parse_module(&all).unwrap_or_else(|e| panic!("{e}\n{all}"));
    let errors = verify_module(&before);
    assert!(errors.is_empty(), "{errors:#?}");
    let mut after = before.clone();
    stage1().run(&mut after).unwrap();
    let printed = print_module(&after);
    assert!(!printed.contains("diff."), "marks are left:\n{printed}");

    let mut rng = Rng::new(seed(0x0b_0001));
    let h = 1e-6;
    for name in &tested {
        let g = before.find_function(name).unwrap();
        let d = after.find_function(&format!("{name}.d")).unwrap();
        let params: Vec<Type> = before.funcs[g]
            .params()
            .iter()
            .map(|&p| before.funcs[g].ty(p))
            .collect();
        for _ in 0..16 {
            let xs: Vec<Val> = params
                .iter()
                .map(|&t| random(&before, t, &mut || 0.1 + 0.8 * unit(&mut rng)))
                .collect();
            let ts: Vec<Val> = params
                .iter()
                .map(|&t| random(&before, t, &mut || 2.0 * unit(&mut rng) - 1.0))
                .collect();
            let call = |m: &Module, f: FuncId, args: &[Val]| {
                Interpreter::new(m)
                    .call(f, args)
                    .unwrap_or_else(|e| panic!("@{name}: {e}\n{}", print_module(m)))
                    .remove(0)
            };
            let mut args = xs.clone();
            args.extend(ts.iter().cloned());
            let ad = components(&call(&after, d, &args));
            let plus: Vec<Val> = xs.iter().zip(&ts).map(|(x, t)| step(x, t, h)).collect();
            let minus: Vec<Val> = xs.iter().zip(&ts).map(|(x, t)| step(x, t, -h)).collect();
            let (p, q) = (
                components(&call(&before, g, &plus)),
                components(&call(&before, g, &minus)),
            );
            for (k, a) in ad.iter().enumerate() {
                let fd = (p[k] - q[k]) / (2.0 * h);
                assert!(
                    (a - fd).abs() <= 1e-5 * fd.abs().max(1.0),
                    "@{name} at {xs:?} along {ts:?}: component {k} is {a}, finite differences \
                     give {fd}\n{}",
                    print_module(&after)
                );
            }
        }
    }
}

#[test]
fn detach_has_no_derivative() {
    let text = read("fd.ir");
    let fixture = parse_module(&text).unwrap();
    let id = fixture.find_function("detach").unwrap();
    let all = format!("{text}{}", wrapper(&fixture, id));
    let mut m = parse_module(&all).unwrap();
    stage1().run(&mut m).unwrap();
    let d = m.find_function("detach.d").unwrap();
    for x in [0.25, 0.5, 2.0] {
        let got = Interpreter::new(&m)
            .call(d, &[Val::f64(x), Val::f64(1.0)])
            .unwrap();
        // d/dx (x · detach(x)) = detach(x).
        assert_eq!(components(&got[0]), vec![x]);
    }
}

/// Fixture `name` through stage 1 and the pass, against `name.out.ir`; its warnings.
fn check(name: &str) -> Vec<String> {
    let before = parse_module(&read(&format!("{name}.ir"))).unwrap();
    let errors = verify_module(&before);
    assert!(errors.is_empty(), "{name}: {errors:#?}");
    let mut after = before.clone();
    let report = stage1()
        .run(&mut after)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let text = print_module(&after);
    golden::assert_text(
        &data_path(
            env!("CARGO_MANIFEST_DIR"),
            &format!("autodiff/{name}.out.ir"),
        ),
        &text,
    );
    assert_eq!(print_module(&parse_module(&text).unwrap()), text, "{name}");
    report
        .warnings
        .iter()
        .map(|w| format!("{}: {}", w.code, w.message))
        .collect()
}

#[test]
fn screen_derivatives_come_from_seeds() {
    let warnings = check("screen");
    assert_eq!(
        warnings,
        vec![
            "E1402: a texture sample has no derivative: `Ddx`, `Ddy` and samples with an \
             implicit level take the derivative through it as 0"
                .to_string()
        ]
    );
    // Nothing is left for the hardware outside the pixel entry that seeds nothing.
    let m = parse_module(&read("screen.out.ir")).unwrap();
    for (_, f) in m.funcs.iter() {
        if &*f.name == "Hardware" {
            continue;
        }
        for &b in &f.layout {
            for i in f.insts(b) {
                let op = &f.insts[i].op;
                let implicit = matches!(op, Op::TexSample(a)
                    if !a.contains(&TexArg::Lod) && !a.contains(&TexArg::Grad));
                assert!(!implicit && !matches!(op, Op::Deriv { .. }), "@{}", f.name);
            }
        }
    }
}

#[test]
fn nested_forward_derivatives_differentiate_each_other() {
    let warnings = check("nested");
    assert!(warnings.is_empty(), "{warnings:?}");
    let m = parse_module(&read("nested.out.ir")).unwrap();
    // d²/dx² of x³ is 6x; the screen-space derivative of d/dx (x²) with x seeded by dx is 2 dx.
    let second = m.find_function("second").unwrap();
    let got = Interpreter::new(&m).call(second, &[Val::f64(1.5)]).unwrap();
    assert_eq!(components(&got[0]), vec![9.0]);
    let screen = m.find_function("screen").unwrap();
    let got = Interpreter::new(&m)
        .call(screen, &[Val::f64(3.0), Val::f64(0.25)])
        .unwrap();
    assert_eq!(components(&got[0]), vec![0.5]);
}
