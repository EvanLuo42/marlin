//! A model test: a graph of queries whose dependencies depend on input values, checked against
//! direct evaluation after every random change.

use std::collections::HashMap;

use marlin_lang_query::{Cancelled, Cx, Db, Durability, Input, Query};
use marlin_math::Rng;
use marlin_test_support::{scale, seed};

pub const INPUTS: u32 = 6;
pub const NODES: u32 = 24;

pub struct In;
impl Input for In {
    type Key = u32;
    type Value = u64;
}

/// Reads one input, then one or two earlier nodes chosen by that input's value. Values are
/// kept small so that recomputations often produce the same value (backdating).
pub struct Node;
impl Query for Node {
    type Key = u32;
    type Value = u64;
    fn execute(cx: &Cx<'_>, &i: &u32) -> Result<u64, Cancelled> {
        node(i, &mut |k| cx.input::<In>(&k), &mut |j| cx.get::<Node>(&j))
    }
}

fn node<E>(
    i: u32,
    input: &mut dyn FnMut(u32) -> Result<u64, E>,
    get: &mut dyn FnMut(u32) -> Result<u64, E>,
) -> Result<u64, E> {
    let x = input(i % INPUTS)?;
    let mut v = x;
    if i > 0 {
        let j = (x as u32 + i) % i;
        v = v.wrapping_mul(31).wrapping_add(get(j)?);
        if x % 2 == 0 && i > 1 {
            v ^= get((x as u32 * 7) % i)?;
        }
    }
    Ok(v % 5)
}

/// Direct evaluation without the engine.
pub fn reference(inputs: &HashMap<u32, u64>, i: u32) -> u64 {
    fn eval(inputs: &HashMap<u32, u64>, i: u32, memo: &mut HashMap<u32, u64>) -> u64 {
        if let Some(&v) = memo.get(&i) {
            return v;
        }
        let v = node::<()>(i, &mut |k| Ok(inputs[&k]), &mut |j| {
            Ok(eval(inputs, j, memo))
        })
        .unwrap();
        memo.insert(i, v);
        v
    }
    eval(inputs, i, &mut HashMap::new())
}

pub fn random_db(rng: &mut Rng) -> (Db, HashMap<u32, u64>) {
    let db = Db::new();
    let mut inputs = HashMap::new();
    db.update(|u| {
        for k in 0..INPUTS {
            let value = rng.below(6) as u64;
            inputs.insert(k, value);
            u.set::<In>(k, value, durability(rng));
        }
    });
    (db, inputs)
}

fn durability(rng: &mut Rng) -> Durability {
    if rng.bool() {
        Durability::Low
    } else {
        Durability::High
    }
}

#[test]
fn incremental_results_match_direct_evaluation() {
    let seed = seed(0x0de1_0001);
    let mut rng = Rng::new(seed);
    for round in 0..scale(20) {
        let (db, mut inputs) = random_db(&mut rng);
        for step in 0..60 {
            // Change one to three inputs in one revision.
            let changes: Vec<(u32, u64, Durability)> = (0..1 + rng.below(3))
                .map(|_| {
                    (
                        rng.below(INPUTS as usize) as u32,
                        rng.below(6) as u64,
                        durability(&mut rng),
                    )
                })
                .collect();
            db.update(|u| {
                for &(k, v, d) in &changes {
                    u.set::<In>(k, v, d);
                }
            });
            for &(k, v, _) in &changes {
                inputs.insert(k, v);
            }
            let snapshot = db.snapshot();
            for _ in 0..8 {
                let i = rng.below(NODES as usize) as u32;
                assert_eq!(
                    snapshot.get::<Node>(&i).unwrap(),
                    reference(&inputs, i),
                    "seed {seed}, round {round}, step {step}, node {i}"
                );
            }
        }
    }
}
