//! Rotation quantization and clip sampling.

use marlin_anim::asset::{decode_rotation, encode_rotation, nlerp};

fn normalize(q: [f32; 4]) -> [f32; 4] {
    let l = q.iter().map(|c| c * c).sum::<f32>().sqrt();
    q.map(|c| c / l)
}

#[test]
fn rotations_quantize_to_48_bits_within_hundredths_of_a_degree() {
    let mut state = 11u64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    let mut worst = 0.0f64;
    for _ in 0..50_000 {
        let q = normalize([next(), next(), next(), next()]);
        let d = decode_rotation(encode_rotation(q));
        // The angle between nearby unit quaternions is twice their distance (q and -q being the
        // same rotation). A dot product would drown in the inputs' own f32 normalization error.
        let s = if (0..4).map(|i| q[i] * d[i]).sum::<f32>() < 0.0 {
            -1.0
        } else {
            1.0
        };
        let distance = (0..4)
            .map(|i| f64::from(q[i] - s * d[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        worst = worst.max((2.0 * distance).to_degrees());
        assert!((d.iter().map(|c| c * c).sum::<f32>() - 1.0).abs() < 1e-5);
    }
    assert!(worst < 0.01, "worst error {worst}°");
    // Identity and its negation are the same rotation and encode the same.
    assert_eq!(
        encode_rotation([0.0, 0.0, 0.0, 1.0]),
        encode_rotation([0.0, 0.0, 0.0, -1.0])
    );
}

#[test]
fn nlerp_takes_the_short_way() {
    let a = [0.0, 0.0, 0.0, 1.0];
    let b = [0.0, 0.0, 0.0, -1.0];
    assert_eq!(nlerp(a, b, 0.5), [0.0, 0.0, 0.0, 1.0]);
}
