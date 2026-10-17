# Fuzz targets

The long-run fuzz layer of [0014 §7.3](../docs/design/0014-testing.md): libFuzzer targets
(`libfuzzer-sys`, in cargo-fuzz's layout) that feed untrusted bytes to the runtime's parsers and
decoders. Each must return an error on bad input, never panic, read out of bounds or run away with
memory.

```text
cargo xtask test --tier fuzz                          every target, 60 s each
cargo xtask test --tier fuzz --target compress_zstd --time 600
cargo xtask test --tier fuzz -p marlin-asset          the targets of one crate
cargo xtask test --tier fuzz --seed 7 -- -max_len=512 more libFuzzer flags after --
```

xtask builds the targets with AddressSanitizer and the platform's gen-cfg flags into
`target/fuzz/`, caps every run at 2048 MB and 10 s per input, and kills a run that outlives its
time. cargo-fuzz is not needed, and `cargo fuzz run` would not work: its `RUSTFLAGS` replace the
gen-cfg flags of `.cargo/config.toml`, so `marlin_platform` would be unset.

- Targets are named `<crate>_<what>` after the crate they test (`marlin-` dropped).
- This crate has its own `[workspace]`: `cargo test --workspace` never builds it.
- Seeds are in `corpus/<target>/`, read-only during a run; what the fuzzer finds accumulates in
  `target/fuzz/corpus/<target>/`. `cargo run --release --manifest-path fuzz/Cargo.toml --bin
  seed-corpus` rewrites the seeds (rerun it when a format changes); the compress seeds include
  the crate's reference-codec fixtures.
- A crashing, out-of-memory or hanging input is minimized and copied to
  `<crate>/tests/data/regress/<target>/`; xtask prints the `unit` test that replays that
  directory with `marlin_test_support::regress::for_each`, so the fix stays covered by the quick
  tier.

## Windows

AddressSanitizer uses MSVC's runtime (`clang_rt.asan_dynamic-x86_64.dll`, from the "C++
AddressSanitizer" component of the build tools); xtask finds it with vswhere. Two more things
xtask takes care of: MSVC's linker needs `/include:main` to take libFuzzer's `main` from the
rlib, and a panic must end the process through the C runtime's `abort` (`marlin_fuzz::init`),
since `std::process::abort` there is `__fastfail`, which libFuzzer cannot catch to save the input.
`cl.exe` fails with C1083 when the path of libFuzzer's object files nears 260 characters, so a
checkout with a long path cannot build the targets.
