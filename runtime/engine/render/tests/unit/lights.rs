//! Light tiles on the CPU: the alias table they are drawn from gives each light its share of the
//! power, and the power of a light.

use marlin_render::lights::{alias_table, light_power};
use marlin_render::scene::{LightDesc, LightKind};

/// Each candidate's probability under the table: kept below its threshold, plus the others'
/// remainders that alias to it.
fn probabilities(table: &[marlin_render::lights::AliasEntry]) -> Vec<f64> {
    let n = table.len() as f64;
    let mut p = vec![0.0; table.len()];
    for e in table {
        p[e.alias as usize] += (1.0 - f64::from(e.threshold)) / n;
    }
    for (i, e) in table.iter().enumerate() {
        p[i] += f64::from(e.threshold) / n;
    }
    p
}

#[test]
fn the_alias_table_draws_each_light_by_its_power() {
    let weights: Vec<(u32, f32)> = (0..257)
        .map(|i| (i * 3 + 1, ((i * 7919) % 101) as f32 + 0.5))
        .collect();
    let table = alias_table(&weights);
    assert_eq!(table.len(), weights.len());
    let total: f64 = weights.iter().map(|&(_, w)| f64::from(w)).sum();
    for ((entry, p), &(row, w)) in table.iter().zip(probabilities(&table)).zip(&weights) {
        let want = f64::from(w) / total;
        assert_eq!(entry.row, row);
        assert!(
            (f64::from(entry.pdf) - want).abs() < 1e-6,
            "pdf of row {row}"
        );
        assert!(
            (p - want).abs() < 1e-6,
            "row {row} drawn {p}, wanted {want}"
        );
        assert!((0.0..=1.0).contains(&entry.threshold));
    }
}

#[test]
fn lights_without_power_are_never_drawn() {
    let table = alias_table(&[(0, 0.0), (1, 2.0), (2, -1.0), (3, f32::NAN), (4, 6.0)]);
    let rows: Vec<u32> = table.iter().map(|e| e.row).collect();
    assert_eq!(rows, [1, 4]);
    let p = probabilities(&table);
    assert!(
        (p[0] - 0.25).abs() < 1e-6 && (p[1] - 0.75).abs() < 1e-6,
        "{p:?}"
    );
    assert!(alias_table(&[(0, 0.0)]).is_empty());
    assert!(alias_table(&[]).is_empty());
}

#[test]
fn a_lights_power_is_its_flux() {
    let mut light = LightDesc {
        kind: LightKind::Point,
        radiance: [100.0; 3],
        radius: 0.0,
        position: [0.0; 3],
        direction: [0.0, -1.0, 0.0],
        range: 10.0,
        cone: [1.0, 0.0],
    };
    // 100 cd of white: 400π lumens.
    assert!((light_power(&light) - 400.0 * core::f32::consts::PI).abs() < 1e-2);
    light.kind = LightKind::Directional;
    assert_eq!(light_power(&light), 0.0);
}
