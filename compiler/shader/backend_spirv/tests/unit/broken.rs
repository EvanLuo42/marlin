//! The validator on broken modules: valid ones the backend wrote, each broken one way. Every
//! break must be reported (the golden file lists what is said); with `MARLIN_SPIRV_VAL`, Khronos'
//! `spirv-val` must reject each one too, so the rules agree with theirs.

use marlin_shaderc_backend_spirv::spirv::{capability, decoration, execution_mode, op};
use marlin_shaderc_backend_spirv::{disassemble, from_bytes, to_bytes, validate, write_module};
use marlin_shaderc_ir::parse_module;
use marlin_test_support::{data_path, golden};

/// A module as instructions, to break.
#[derive(Clone)]
struct Spv {
    header: Vec<u32>,
    insts: Vec<(u32, Vec<u32>)>,
}

impl Spv {
    fn parse(words: &[u32]) -> Spv {
        let mut insts = Vec::new();
        let mut at = 5;
        while at < words.len() {
            let n = (words[at] >> 16) as usize;
            insts.push((words[at] & 0xffff, words[at + 1..at + n].to_vec()));
            at += n;
        }
        Spv {
            header: words[..5].to_vec(),
            insts,
        }
    }

    fn words(&self) -> Vec<u32> {
        let mut out = self.header.clone();
        for (opcode, ops) in &self.insts {
            out.push(((ops.len() as u32 + 1) << 16) | opcode);
            out.extend(ops);
        }
        out
    }

    /// The position of the `n`th instruction with `opcode` for which `pick` holds.
    fn find(&self, opcode: u32, pick: impl Fn(&[u32]) -> bool) -> usize {
        self.insts
            .iter()
            .position(|(o, ops)| *o == opcode && pick(ops))
            .unwrap_or_else(|| panic!("no instruction {opcode} to break"))
    }

    fn new_id(&mut self) -> u32 {
        let id = self.header[3];
        self.header[3] += 1;
        id
    }
}

fn module(name: &str) -> Spv {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), &format!("compute/{name}.ir"));
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let m = parse_module(&text).unwrap();
    Spv::parse(&write_module(&m, m.entries[0].func).unwrap())
}

type Break = fn(&mut Spv);

/// (name, module, how to break it).
fn breaks() -> Vec<(&'static str, &'static str, Break)> {
    vec![
        ("capability left out", "mirror", |s| {
            let k = s.find(op::Capability, |o| o[0] == capability::DescriptorHeapEXT);
            s.insts.remove(k);
        }),
        ("extension left out", "mirror", |s| {
            let k = s.find(op::Extension, |_| true);
            s.insts.remove(k);
        }),
        ("two memory models", "mirror", |s| {
            let k = s.find(op::MemoryModel, |_| true);
            let m = s.insts[k].clone();
            s.insts.insert(k, m);
        }),
        ("an id nothing defines", "mirror", |s| {
            let k = s.find(op::IMul, |_| true);
            s.insts[k].1[3] = 9999;
            s.header[3] = 10_000;
        }),
        ("a bound under the ids", "mirror", |s| s.header[3] = 3),
        ("a type declared twice", "mirror", |s| {
            let k = s.find(op::TypeInt, |_| true);
            let id = s.new_id();
            let mut ops = s.insts[k].1.clone();
            ops[0] = id;
            s.insts.insert(k + 1, (op::TypeInt, ops));
        }),
        ("an offset left out", "mirror", |s| {
            let k = s.find(op::MemberDecorate, |o| o[2] == decoration::Offset);
            s.insts.remove(k);
        }),
        ("an offset that overlaps", "mirror", |s| {
            let k = s.find(op::MemberDecorate, |o| {
                o[1] == 1 && o[2] == decoration::Offset
            });
            s.insts[k].1[3] = 2;
        }),
        ("layout in groupshared memory", "mirror", |s| {
            // The groupshared array's type: the pointee of the `Workgroup` pointer.
            let p = s.find(op::TypePointer, |o| o[1] == 4);
            let array = s.insts[p].1[2];
            let d = s.find(op::Decorate, |_| true);
            s.insts
                .insert(d, (op::Decorate, vec![array, decoration::ArrayStride, 4]));
        }),
        ("a variable missing from the interface", "mirror", |s| {
            let k = s.find(op::EntryPoint, |_| true);
            s.insts[k].1.pop();
        }),
        ("no LocalSize", "mirror", |s| {
            let k = s.find(op::ExecutionMode, |o| o[1] == execution_mode::LocalSize);
            s.insts.remove(k);
        }),
        ("a built-in on the wrong type", "mirror", |s| {
            // GroupIndex (a scalar) as GlobalInvocationId (a vector of three).
            let k = s.find(op::Decorate, |o| o[1] == decoration::BuiltIn && o[2] == 29);
            s.insts[k].1[2] = 28;
        }),
        ("a load of the wrong type", "mirror", |s| {
            let k = s.find(op::Load, |_| true);
            let other = s.find(op::TypeFloat, |_| true);
            s.insts[k].1[0] = s.insts[other].1[0];
            let _ = other;
        }),
        ("a float added as integers", "move", |s| {
            let k = s.find(op::FAdd, |_| true);
            s.insts[k].0 = op::IAdd;
        }),
        ("no loop merge", "move", |s| {
            let k = s.find(op::LoopMerge, |_| true);
            s.insts.remove(k);
        }),
        ("a loop header's branch that does not leave", "move", |s| {
            let k = s.find(op::BranchConditional, |_| true);
            let body = s.insts[k].1[2];
            s.insts[k].1[1] = body;
            let other = s.new_id();
            let _ = other;
        }),
        ("a second back edge", "move", |s| {
            // The loop body's break goes back to the header instead.
            let header = s.insts[s.find(op::LoopMerge, |_| true) - 1..]
                .iter()
                .find(|(o, _)| *o == op::BranchConditional)
                .map(|_| ())
                .and(None::<u32>);
            let _ = header;
            let lm = s.find(op::LoopMerge, |_| true);
            let merge = s.insts[lm].1[0];
            let label = s.insts[..lm]
                .iter()
                .rev()
                .find(|(o, _)| *o == op::Label)
                .unwrap()
                .1[0];
            let k = s
                .insts
                .iter()
                .rposition(|(o, ops)| *o == op::Branch && ops[0] == merge)
                .unwrap();
            s.insts[k].1[0] = label;
        }),
        (
            "a break out of a selection, not to its merge",
            "branches",
            |s| {
                // The if's then block jumps to the loop's merge instead of the if's.
                let lm = s.find(op::LoopMerge, |_| true);
                let loop_merge = s.insts[lm].1[0];
                let k = s.find(op::Branch, |_| true);
                s.insts[k].1[0] = loop_merge;
            },
        ),
        ("a phi missing a predecessor", "branches", |s| {
            let k = s.find(op::Phi, |_| true);
            s.insts[k].1.truncate(4);
        }),
        ("a block without its terminator", "move", |s| {
            let k = s.find(op::Return, |_| true);
            s.insts.remove(k);
        }),
        ("a branch to something that is not a block", "move", |s| {
            let k = s.find(op::Branch, |_| true);
            let t = s.find(op::TypeInt, |_| true);
            s.insts[k].1[0] = s.insts[t].1[0];
        }),
        ("an instruction after the terminator", "move", |s| {
            let k = s.find(op::Return, |_| true);
            s.insts.insert(k + 1, (op::Return, vec![]));
        }),
    ]
}

/// With `MARLIN_SPIRV_VAL`, whether Khronos' `spirv-val` accepts `words` (`None` without it).
fn spirv_val_accepts(words: &[u32], what: &str) -> Option<bool> {
    let tool = std::env::var("MARLIN_SPIRV_VAL").ok()?;
    let dir = std::env::temp_dir().join(format!("marlin-spirv-broken-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.spv", what.replace(' ', "-")));
    std::fs::write(&path, to_bytes(words)).unwrap();
    let out = std::process::Command::new(tool)
        .args(["--target-env", "vulkan1.3", "--scalar-block-layout"])
        .arg(&path)
        .output()
        .expect("runs spirv-val");
    let _ = std::fs::remove_file(&path);
    Some(out.status.success())
}

#[test]
fn every_break_is_reported() {
    let mut text = String::new();
    let mut silent = Vec::new();
    for (name, base, how) in breaks() {
        let mut s = module(base);
        how(&mut s);
        let words = s.words();
        let problems = validate(&words);
        text.push_str(&format!("== {name}\n"));
        for p in &problems {
            text.push_str(&format!("{p}\n"));
        }
        if problems.is_empty() {
            silent.push(name);
        }
        if spirv_val_accepts(&words, name) == Some(true) {
            panic!(
                "{name}: spirv-val accepts this, so it is not broken\n{}",
                disassemble(&words)
            );
        }
    }
    golden::assert_text(&data_path(env!("CARGO_MANIFEST_DIR"), "broken.txt"), &text);
    assert!(silent.is_empty(), "not reported: {silent:?}");
}

#[test]
fn what_is_not_spirv_is_an_error_not_a_panic() {
    let mut s = module("mirror");
    let words = s.words();
    for cut in [0, 3, 5, 6, words.len() - 1] {
        assert!(!validate(&words[..cut]).is_empty(), "cut at {cut}");
    }
    s.header[0] = 0x0302_2307;
    assert!(!validate(&s.words()).is_empty());
    assert!(from_bytes(&[1, 2, 3]).is_none());
}

/// Words of the fixtures, to break at random.
fn fixtures() -> Vec<Vec<u32>> {
    ["mirror", "move", "branches"]
        .iter()
        .map(|n| module(n).words())
        .collect()
}

/// `words` with a few of them changed (`seed` picks which and how).
fn mutate(words: &[u32], seed: u64) -> Vec<u32> {
    let mut rng = marlin_math::Rng::new(seed);
    let mut w = words.to_vec();
    for _ in 0..1 + rng.below(3) {
        let k = 5 + rng.below(w.len() - 5);
        w[k] = match rng.below(4) {
            0 => w[k] ^ (1 << rng.below(32)),
            1 => w[k].wrapping_add(1),
            2 => rng.next_u32() % 128,
            _ => w[rng.below(w.len())],
        };
    }
    w
}

#[test]
fn broken_words_never_panic() {
    let base = marlin_test_support::seed(0x09_b001);
    for (f, words) in fixtures().iter().enumerate() {
        for case in 0..marlin_test_support::scale(300) as u64 {
            let seed = base.wrapping_add(case * 7 + f as u64);
            let broken = mutate(words, seed);
            let result = std::panic::catch_unwind(|| validate(&broken));
            assert!(
                result.is_ok(),
                "seed {seed}: the validator panicked\n{}",
                disassemble(&broken)
            );
        }
    }
}

/// How often our verdict and `spirv-val`'s differ on randomly broken modules (opt-in with
/// `MARLIN_SPIRV_VAL`; run with `--nocapture` to read it). Ours checks a subset of the rules,
/// so it may accept what spirv-val rejects; it must not reject what spirv-val accepts.
#[test]
fn verdicts_agree_with_spirv_val() {
    if std::env::var("MARLIN_SPIRV_VAL").is_err() {
        return;
    }
    let base = marlin_test_support::seed(0x09_b002);
    let (mut both, mut only_theirs, mut only_ours) = (0, 0, Vec::new());
    for (f, words) in fixtures().iter().enumerate() {
        for case in 0..marlin_test_support::scale(100) as u64 {
            let seed = base.wrapping_add(case * 7 + f as u64);
            let broken = mutate(words, seed);
            let problems = validate(&broken);
            // It knows the instructions the backend writes, and says so about others.
            if problems
                .iter()
                .any(|p| p.contains("not one this validator knows"))
            {
                continue;
            }
            // Vulkan's rule, which spirv-val does not check.
            if problems.iter().any(|p| p.contains("Vulkan's memory model")) {
                continue;
            }
            let ours = problems.is_empty();
            let theirs = spirv_val_accepts(&broken, &format!("agree-{seed}")).unwrap_or(true);
            match (ours, theirs) {
                (true, false) => only_theirs += 1,
                (false, true) => only_ours.push((seed, validate(&broken))),
                _ => both += 1,
            }
        }
    }
    eprintln!(
        "verdicts: {both} agree, {only_theirs} only spirv-val rejects, {} only ours rejects",
        only_ours.len()
    );
    assert!(
        only_ours.is_empty(),
        "we reject what spirv-val accepts: {only_ours:#?}"
    );
}

#[test]
fn fuzz_finds_stay_fixed() {
    marlin_test_support::regress::for_each(
        env!("CARGO_MANIFEST_DIR"),
        "shaderc_backend_spirv_validate",
        |bytes| {
            if let Some(words) = marlin_shaderc_backend_spirv::from_bytes(bytes) {
                let result = std::panic::catch_unwind(|| {
                    if validate(&words).is_empty() {
                        disassemble(&words);
                    }
                });
                assert!(result.is_ok(), "the validator panicked on a fuzz find");
            }
        },
    );
}
