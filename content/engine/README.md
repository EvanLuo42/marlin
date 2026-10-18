# content/engine

The engine's own content: an asset root like a game's (`docs/design/0003-asset-pipeline.md`),
cooked and packaged on its own. Games mount the engine package below their own, so a game can
override any of it.

- `shaders/`: the `Marlin.Engine` shader package (`package.masset`). Each directory is one
  namespace: `shaders/` is `Marlin.Engine`, `shaders/Post/` would be `Marlin.Engine.Post`. The
  cooker compiles every public entry point into one library, which the engine finds by the
  package's name and then by entry path (`Marlin.Engine.ForwardVs`).

```
cargo run -p marlin-cooker --bin marlin-cook -- cook content/engine
cargo run -p marlin-cooker --bin marlin-cook -- package content/engine
```

The package lands in `content/engine/.marlin/build/<platform>.mpak`; `samples/basic/boot.toml`
mounts it from there.
