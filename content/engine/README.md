# content/engine

The engine's own content, as sources: an asset root like a game's
(`docs/design/0003-asset-pipeline.md`). It is never cooked on its own. Every project mounts it as
`/Engine`, below its own `/Game`, and cooks it from source with the project's platform and
profile into the project's one build (§4.5). An installed editor ships this directory read-only
next to its `bin/`. Tools write nothing in it: each project keeps its index of it under its own
`.marlin/index/Engine`.

- `shaders/`: the `Marlin.Engine` shader package (`package.masset`). Each directory is one
  namespace: `shaders/` is `Marlin.Engine`, `shaders/Post/` would be `Marlin.Engine.Post`. The
  cooker compiles every public entry point into one library, which the engine finds by the
  package's name and then by entry path (`Marlin.Engine.ForwardVs`). Package names `Marlin` and
  `Marlin.…` belong to this mount only.

Game content links into it with qualified paths: `</Engine/shaders/package:Marlin.Engine #…>`.
