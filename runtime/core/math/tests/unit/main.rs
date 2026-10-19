//! marlin-math's tests (quick tier, docs/design/0014-testing.md §5): vectors, matrices,
//! quaternions, double-precision positions, geometry, the RNG's reference outputs and the transcendental functions' accuracy.
//! Platform-independent. Run alone with `cargo test -p marlin-math --test unit`.

mod dvec;
mod geometry;
mod linear;
mod random;
mod scalar;
