# samples/lights

A hall at night lit by 1024 local lights (ReSTIR DI, docs/design/0010-lighting.md §2): a floor,
a grid of pillars, point lights of 200 to 2000 lumens and, every eighth, a spot light shining
down. No sun; a faint sky. R3's acceptance scene: the frame time does not depend on the number
of lights.

```
cargo run -p marlin-cooker --bin marlin-cook -- cook samples/lights
cargo run -p marlin-cooker --bin marlin-cook -- package samples/lights
cargo run -p marlin-game -- samples/lights/boot.toml
```

Right mouse button + mouse, or the right stick: look. WASD or the left stick: move. Q/E or the
triggers: down/up. Shift: faster.
