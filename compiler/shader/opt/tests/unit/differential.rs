use marlin_math::Rng;
use marlin_shaderc_ir::{FuncId, Interpreter, Module, Val, print_module, verify_module};
use marlin_shaderc_opt::{
    Dce, DivGuard, GlobalDce, HoistConstArrays, Inline, Mem2Reg, Options, PassManager,
    PrecisePropagate, Profile, Sccp, SimplifyCfg, Sroa, pipeline,
};
use marlin_test_support::{scale, seed};

use crate::random;

fn run(m: &Module, f: FuncId, args: &[i32]) -> Vec<Val> {
    let args: Vec<Val> = args.iter().map(|&a| Val::i32(a)).collect();
    Interpreter::new(m)
        .call(f, &args)
        .unwrap_or_else(|e| panic!("{e}\n{}", print_module(m)))
}

/// Runs `pm` on generated functions and checks each still verifies and computes what it did.
fn check(what: &str, pm: &PassManager, cases: usize) -> (usize, usize) {
    let base = seed(0x09_0001);
    let mut rng = Rng::new(base);
    let (mut before, mut after) = (0, 0);
    for case in 0..cases {
        let s = base.wrapping_add(case as u64);
        let (m, f) = random::function(s, 6 + rng.below(40));
        let errors = verify_module(&m);
        assert!(
            errors.is_empty(),
            "seed {s}: the generator made bad IR: {errors:#?}\n{}",
            print_module(&m)
        );
        let mut opt = m.clone();
        if let Err(e) = pm.run(&mut opt) {
            panic!(
                "{what}, seed {s}: {e}\nbefore:\n{}\nafter:\n{}",
                print_module(&m),
                print_module(&opt)
            );
        }
        // Sizes against the function with its calls inlined, which is what the rest works on.
        let mut inlined = m.clone();
        let mut inline = PassManager::new(false);
        inline.add(Inline);
        inline.run(&mut inlined).unwrap();
        before += inlined.funcs[f]
            .layout
            .iter()
            .map(|&b| inlined.funcs[f].insts(b).count())
            .sum::<usize>();
        after += opt.funcs[f]
            .layout
            .iter()
            .map(|&b| opt.funcs[f].insts(b).count())
            .sum::<usize>();
        for _ in 0..8 {
            let args = [
                random::arg(&mut rng),
                random::arg(&mut rng),
                random::arg(&mut rng),
            ];
            let want = run(&m, f, &args);
            let got = run(&opt, f, &args);
            assert_eq!(
                got,
                want,
                "{what}, seed {s}, args {args:?}\nbefore:\n{}\nafter:\n{}",
                print_module(&m),
                print_module(&opt)
            );
        }
    }
    (before, after)
}

fn alone(pass: impl marlin_shaderc_opt::Pass + 'static) -> PassManager {
    let mut pm = PassManager::new(true);
    pm.add(pass);
    pm
}

#[test]
fn each_pass_keeps_what_functions_compute() {
    let n = scale(60);
    check("simplify-cfg", &alone(SimplifyCfg), n);
    check("sccp", &alone(Sccp), n);
    check("dce", &alone(Dce), n);
    check("div-guard", &alone(DivGuard), n);
    check("inline", &alone(Inline), n);
    check("sroa", &alone(Sroa), n);
    check("mem2reg", &alone(Mem2Reg), n);
    check("precise", &alone(PrecisePropagate), n);
    check("hoist-const-arrays", &alone(HoistConstArrays), n);
    check("global-dce", &alone(GlobalDce), n);
}

#[test]
fn the_pipeline_keeps_what_functions_compute_and_shrinks_them() {
    let options = Options {
        profile: Profile::Development,
        verify_each: true,
    };
    let (before, after) = check("pipeline", &pipeline(&options), scale(150));
    // Usually about a quarter of what the generator writes goes; at least a tenth must.
    assert!(
        after * 10 < before * 9,
        "the pipeline left {after} of {before} instructions"
    );
}

/// Calls, variables and constant tables in all of a module's functions.
fn count(m: &Module, which: impl Fn(&marlin_shaderc_ir::Op) -> bool) -> usize {
    m.funcs
        .iter()
        .map(|(_, f)| {
            f.layout
                .iter()
                .map(|&b| f.insts(b).filter(|&i| which(&f.insts[i].op)).count())
                .sum::<usize>()
        })
        .sum()
}

#[test]
fn the_pipeline_inlines_promotes_and_hoists() {
    use marlin_shaderc_ir::Op;
    let options = Options {
        profile: Profile::Development,
        verify_each: true,
    };
    let base = seed(0x09_0002);
    let (mut calls, mut vars, mut vars_left, mut tables) = (0, 0, 0, 0);
    for case in 0..scale(100) as u64 {
        let (m, _) = random::function(base.wrapping_add(case), 30);
        let mut opt = m.clone();
        pipeline(&options).run(&mut opt).unwrap();
        calls += count(&m, |o| matches!(o, Op::Call(_)));
        assert_eq!(
            count(&opt, |o| matches!(o, Op::Call(_))),
            0,
            "every call is inlined"
        );
        vars += count(&m, |o| *o == Op::Var);
        vars_left += count(&opt, |o| *o == Op::Var);
        tables += opt.globals.len();
    }
    assert!(calls > 0, "the generator makes calls");
    // What stays is arrays indexed with values.
    assert!(vars_left * 2 < vars, "{vars_left} of {vars} variables stay");
    assert!(tables > 0, "constant tables become module data");
}
