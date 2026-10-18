use marlin_math::Rng;
use marlin_shaderc_ir::{FuncId, Interpreter, Module, Val, print_module, verify_module};
use marlin_shaderc_opt::{Dce, Options, PassManager, Profile, Sccp, SimplifyCfg, pipeline};
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
        before += m.funcs[f].insts.len();
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
