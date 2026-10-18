//! The cluster DAG's cook stages (0009 §3.1) on bumpy spheres of 82k and 328k triangles:
//! clustering, the DAG, page packing and the RT LOD. `cargo xtask test --tier bench -p
//! marlin-cook-mesh`, or `cargo bench -p marlin-cook-mesh --bench cluster_dag -- [filter]`.

#[path = "../tests/support/mod.rs"]
#[allow(unused_imports, reason = "the support module also serves the cook tests")]
mod support;

use marlin_bench::{Bench, Duration};
use marlin_cook_mesh::{cluster, dag, page, quantize, rt};

/// A stage takes up to seconds; a few samples are enough.
fn slow(name: String, triangles: usize) -> Bench {
    Bench::new(name)
        .throughput_items(triangles as u64, "tri")
        .samples(5)
        .warmup(Duration::ZERO)
}

fn stages() {
    for subdivisions in [6, 7] {
        let s = support::icosphere(subdivisions);
        let triangles = s.indices.len() / 3;
        let sections = vec![0u16; triangles];
        let attributes: Vec<f32> = s
            .normals
            .iter()
            .zip(&s.uvs)
            .flat_map(|(n, uv)| {
                [
                    n[0] * 0.05,
                    n[1] * 0.05,
                    n[2] * 0.05,
                    uv[0] * 0.1,
                    uv[1] * 0.1,
                ]
            })
            .collect();
        let input = dag::Input {
            positions: &s.positions,
            attributes: &attributes,
            attribute_count: 5,
            indices: &s.indices,
            sections: &sections,
        };
        let id = |stage: &str| format!("{stage}/{}k", triangles / 1000);

        slow(id("cluster"), triangles).run(|| cluster::split(&s.indices, &sections, &s.positions));

        let built = dag::build(&input);
        slow(id("dag"), triangles)
            .metric("levels", f64::from(built.levels))
            .metric("clusters", built.clusters.len() as f64)
            .run(|| dag::build(&input));

        let tangents: Vec<[f32; 4]> = vec![[1.0, 0.0, 0.0, 1.0]; s.positions.len()];
        let uvs = vec![s.uvs.clone()];
        let vertices = quantize::Vertices {
            positions: &s.positions,
            normals: &s.normals,
            tangents: &tangents,
            uvs: &uvs,
            colors: None,
            skin: None,
        };
        let positions = quantize::Positions::Quantized {
            min: [-1.1; 3],
            scale: [2.2 / 65535.0; 3],
        };
        let packed = page::pack(&built, &vertices, positions);
        slow(id("pack"), triangles)
            .metric("pages", packed.pages.len() as f64)
            .run(|| page::pack(&built, &vertices, positions));

        slow(id("rt_lod"), triangles).run(|| rt::build(&built, triangles, rt::DEFAULT_FRACTION, 1));
    }
}

marlin_bench::main!(stages);
