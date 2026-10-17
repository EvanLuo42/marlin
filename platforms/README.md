# platforms/

One directory per platform. Each has a `platform.toml` manifest, read by `cargo xtask` (targets,
capabilities) and compiled into the cook tools (its `[cook]` table: texture formats and layout,
package codecs; see `tools/cook_core/src/platform.rs`). Tools and games default to the platform
they were built for (`marlin_rt::PLATFORM`).

- `windows/`, `linux/`, `macos/` are public and tracked. Their backends live inside the runtime
  crates (`runtime/*/<crate>/src/backend/<platform>/`).
- `ps5/`, `xbox/`, `switch2/` are under NDA. Each is a private repository cloned here and ignored
  by git. Runtime crates pull their backends in with `#[path]` modules that only exist under
  `cfg(marlin_platform = "<name>")`, so the public tree builds without them.

## Console repository layout

The layout mirrors the public crates:

```
platforms/<console>/
├── platform.toml      # targets, ext list, target-spec, build-std, [cook]
├── target/*.json      # custom target spec
├── pal/mod.rs         # included by runtime/base/pal/src/backend/mod.rs
│   └── sys/           # SDK FFI bindings, re-exported as marlin_pal::os::<console>::sys
├── app/ input/ audio_device/ net/ services/ rhi/
├── cook/              # cooker platform plugins (texture swizzle, ...)
└── package/           # packaging, driven by xtask
```

Every backend must implement the same contract traits as the public ones. Missing items fail to
compile.

## Setup

```bash
git clone <private-url>/marlin-platform-ps5 platforms/ps5
cargo xtask build --platform ps5
```
