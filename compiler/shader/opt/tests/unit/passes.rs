use marlin_shaderc_ir::{Interpreter, Module, Val, parse_module, print_module, verify_module};
use marlin_shaderc_opt::{Dce, Options, PassManager, Profile, Sccp, SimplifyCfg, pipeline};
use marlin_test_support::{data_path, golden};

fn fixture(name: &str) -> Module {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), &format!("passes/{name}.ir"));
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let m = parse_module(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let errors = verify_module(&m);
    assert!(errors.is_empty(), "{name}: {errors:#?}");
    m
}

/// Every function of `before` and `after` returns the same for a few `int` arguments.
fn same_results(name: &str, before: &Module, after: &Module) {
    for (id, f) in before.funcs.iter() {
        for x in [-5, -1, 0, 1, 2, 3, 10, 100] {
            let args = vec![Val::i32(x); f.params().len()];
            let want = Interpreter::new(before).call(id, &args).unwrap();
            let got = Interpreter::new(after).call(id, &args).unwrap();
            assert_eq!(got, want, "{name}: @{} with {x}", f.name);
        }
    }
}

/// Runs `pm` on fixture `name` and compares the result with `name.out.ir`.
fn check(name: &str, pm: &PassManager) {
    let before = fixture(name);
    let mut after = before.clone();
    pm.run(&mut after).unwrap_or_else(|e| panic!("{name}: {e}"));
    let text = print_module(&after);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), &format!("passes/{name}.out.ir")),
        &text,
    );
    // What the pass makes reads back as it prints.
    assert_eq!(print_module(&parse_module(&text).unwrap()), text, "{name}");
    same_results(name, &before, &after);
}

fn alone(pass: impl marlin_shaderc_opt::Pass + 'static) -> PassManager {
    let mut pm = PassManager::new(true);
    pm.add(pass);
    pm
}

#[test]
fn simplify_cfg_keeps_regions_that_are_broken_out_of() {
    check("simplify_cfg", &alone(SimplifyCfg));
}

#[test]
fn dce_removes_what_nothing_live_reads() {
    check("dce", &alone(Dce));
}

#[test]
fn sccp_finds_constants_through_loops_and_branches() {
    check("sccp", &alone(Sccp));
    let options = Options {
        profile: Profile::Shipping,
        verify_each: false,
    };
    // With the rest of the pipeline, `one_way` is a `return`; the loop, which may run any
    // number of times, stays (deleting loops is pass 3.3), but returns a constant.
    let mut m = fixture("sccp");
    pipeline(&options).run(&mut m).unwrap();
    let text = print_module(&m);
    assert!(
        text.contains(
            "block0:
    return i32 8
"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    return i32 1
"
        ),
        "{text}"
    );
}

#[test]
fn the_report_times_every_pass_and_check() {
    let pm = chain(vec![Box::new(SimplifyCfg), Box::new(Sccp), Box::new(Dce)]);
    let mut m = fixture("sccp");
    let report = pm.run(&mut m).unwrap();
    // Each pass, then `verify` after each one that changed the module (`verify_each`).
    let mut want = Vec::new();
    for &(name, changed) in &report.ran {
        want.push(name);
        if changed {
            want.push("verify");
        }
    }
    let steps: Vec<&str> = report.timings.iter().map(|(name, _)| *name).collect();
    assert_eq!(steps, want);
    assert_eq!(&want[..1], ["simplify-cfg"]);

    // Without `verify_each`, the module is checked once, at the end.
    let options = Options {
        profile: Profile::Shipping,
        verify_each: false,
    };
    let pm = pipeline(&options);
    let mut m = fixture("sccp");
    let report = pm.run(&mut m).unwrap();
    let steps: Vec<&str> = report.timings.iter().map(|(name, _)| *name).collect();
    let mut want = pm.names();
    want.push("verify");
    assert_eq!(steps, want);
}

/// What `fuzz/fuzz_targets/shaderc_opt_pipeline.rs` checks.
fn pipeline_on_text(text: &str) {
    let Ok(module) = parse_module(text) else {
        return;
    };
    if !verify_module(&module).is_empty() {
        return;
    }
    let options = Options {
        profile: Profile::Development,
        verify_each: true,
    };
    let mut opt = module.clone();
    match pipeline(&options).run(&mut opt) {
        Ok(_) => {}
        Err(marlin_shaderc_opt::PassError::Rejected { .. }) => return,
        Err(e) => panic!("{e}"),
    }
    for (id, f) in module.funcs.iter() {
        if module.entry_of(id).is_some() {
            continue;
        }
        let Some(args) = f
            .params()
            .iter()
            .map(|&p| Val::zero(&module, f.ty(p)).ok())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let mut a = Interpreter::new(&module);
        a.options.step_limit = 20_000;
        let mut b = Interpreter::new(&opt);
        b.options.step_limit = 20_000;
        if let (Ok(x), Ok(y)) = (a.call(id, &args), b.call(id, &args)) {
            assert_eq!(x, y, "@{}", f.name);
        }
    }
}

#[test]
fn fuzz_finds_stay_fixed() {
    marlin_test_support::regress::for_each(
        env!("CARGO_MANIFEST_DIR"),
        "shaderc_opt_pipeline",
        |bytes| {
            if let Ok(text) = std::str::from_utf8(bytes) {
                pipeline_on_text(text);
            }
        },
    );
}

fn chain(passes: Vec<Box<dyn marlin_shaderc_opt::Pass>>) -> PassManager {
    let mut pm = PassManager::new(true);
    for p in passes {
        pm.add_boxed(p);
    }
    pm
}

#[test]
fn inline_turns_returns_into_breaks_out_of_regions() {
    check("inline", &chain(vec![Box::new(marlin_shaderc_opt::Inline)]));
}

#[test]
fn sroa_mem2reg_and_hoisting_take_variables_out_of_memory() {
    check(
        "memory",
        &chain(vec![
            Box::new(marlin_shaderc_opt::Sroa),
            Box::new(marlin_shaderc_opt::Mem2Reg),
            Box::new(marlin_shaderc_opt::HoistConstArrays),
            Box::new(marlin_shaderc_opt::GlobalDce),
        ]),
    );
}

#[test]
fn stage_0_guards_divisions_strips_debug_and_spreads_precise() {
    check(
        "stage0",
        &chain(vec![
            Box::new(marlin_shaderc_opt::DivGuard),
            Box::new(marlin_shaderc_opt::StripDebug),
            Box::new(marlin_shaderc_opt::PrecisePropagate),
        ]),
    );
}

/// A function of `segments` pieces, each a selection on a constant (either way) or on the
/// argument, with empty arms, or a loop that runs once; then a chain of blocks. About six
/// blocks a piece.
fn long_function(segments: usize) -> String {
    let mut s = String::from("func @long(%a: i32) -> i32 {\nblock0:\n    br block1(%a)\n");
    let mut b = 1;
    for i in 0..segments {
        let (h, m) = (b, b + 3);
        s += &format!("block{h}(%x{i}: i32):\n");
        if i % 4 == 3 {
            s += &format!(
                "    loop header block{}(%x{i}) continue block{} merge block{m}\n\
                 block{}(%y{i}: i32):\n    %z{i} = add %y{i}, i32 1\n    br block{m}(%z{i})\n\
                 block{}:\n    br block{}(i32 0)\n",
                h + 1,
                h + 2,
                h + 1,
                h + 2,
                h + 1
            );
        } else {
            let cond = match i % 4 {
                0 => "i1 true".to_string(),
                1 => "i1 false".to_string(),
                _ => {
                    s += &format!("    %c{i} = icmp slt %x{i}, i32 {i}\n");
                    format!("%c{i}")
                }
            };
            s += &format!(
                "    cond_br {cond}, block{}, block{} merge block{m}\n\
                 block{}:\n    br block{m}(i32 {i})\n\
                 block{}:\n    br block{m}(%x{i})\n",
                h + 1,
                h + 2,
                h + 1,
                h + 2
            );
        }
        s += &format!(
            "block{m}(%m{i}: i32):\n    %s{i} = add %m{i}, i32 1\n    br block{}\n\
             block{}:\n    %t{i} = mul %s{i}, i32 3\n    br block{}\n\
             block{}:\n    br block{}(%t{i})\n",
            m + 1,
            m + 1,
            m + 2,
            m + 2,
            m + 3
        );
        b = m + 3;
    }
    s += &format!("block{b}(%r: i32):\n    return %r\n}}\n");
    s
}

#[test]
fn simplify_cfg_takes_long_functions_in_few_rounds() {
    let segments = 600;
    // The generated function is well formed by construction: the verifier is not yet quick on
    // functions this long, and the pass's own check verifies what it makes.
    let before = parse_module(&long_function(segments)).unwrap();
    let blocks = |m: &Module| m.funcs.iter().map(|(_, f)| f.layout.len()).sum::<usize>();
    assert!(blocks(&before) > 3500);

    let mut after = before.clone();
    let report = alone(SimplifyCfg).run(&mut after).unwrap();
    assert_eq!(report.ran, [("simplify-cfg", true)]);
    // About 0.15 s in a debug build; one change per round, with the analyses rebuilt after
    // each, had not finished after five minutes.
    let (_, took) = report.timings[0];
    assert!(took.as_secs_f64() < 1.0, "simplify-cfg took {took:?}");
    // Only the selections on the argument are left: a header, the arm the `cond_br` still
    // jumps to and the merge block each.
    let kept = (0..segments).filter(|i| i % 4 == 2).count();
    assert!(blocks(&after) <= 3 * kept + 2, "{} blocks", blocks(&after));
    // Nothing is left to do.
    let mut again = after.clone();
    let report = alone(SimplifyCfg).run(&mut again).unwrap();
    assert_eq!(report.ran, [("simplify-cfg", false)]);
    same_results("long", &before, &after);
}
