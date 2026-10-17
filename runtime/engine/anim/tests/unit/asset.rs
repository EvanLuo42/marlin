//! Rotation quantization and clip sampling.

use marlin_anim::asset::{decode_rotation, encode_rotation, nlerp};
use marlin_math::scalar::abs_diff_eq;
use marlin_math::{Rng, Vec4};

#[test]
fn rotations_quantize_to_48_bits_within_hundredths_of_a_degree() {
    let mut rng = Rng::new(11);
    let mut next = || rng.range_f32(-1.0, 1.0);
    let mut worst = 0.0f64;
    for _ in 0..50_000 {
        let q = Vec4::new(next(), next(), next(), next())
            .normalize()
            .to_array();
        let d = decode_rotation(encode_rotation(q));
        // The angle between nearby unit quaternions is twice their distance (q and -q being the
        // same rotation). A dot product would drown in the inputs' own f32 normalization error.
        let s = if Vec4::from_array(q).dot(Vec4::from_array(d)) < 0.0 {
            -1.0
        } else {
            1.0
        };
        let distance = (0..4)
            .map(|i| f64::from(q[i] - s * d[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        worst = worst.max((2.0 * distance).to_degrees());
        let length_squared = Vec4::from_array(d).length_squared();
        assert!(
            abs_diff_eq(length_squared, 1.0, 1e-5),
            "{q:?} decodes to {d:?}"
        );
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
