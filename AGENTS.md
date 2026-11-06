# Guide for AI coding agents

Read this before changing code in this repository. Humans: see
[CONTRIBUTING.md](CONTRIBUTING.md) too.

## What this is

Material Painter is a PBR texture painter (in the spirit of Substance
Painter) in **Rust**, with **eframe/egui 0.37** for the UI and **wgpu 30** for
all GPU work. One binary crate, edition 2024. Goals: fast, low memory, and
artist-first UI.

## Commands

```sh
cargo build                     # debug build (dependencies are optimized, see Cargo.toml)
cargo test                      # unit tests; CI runs these with RUSTFLAGS="-D warnings"
cargo test --bin material_painter -- --ignored --test-threads 1   # GPU tests, offscreen
cargo test --bin material_painter <name> -- --ignored               # one GPU test
```

GPU tests render without a window and write images to `target/bake_test/`.
Look at the images after changing rendering code; a passing test alone does
not prove the picture is right. `ui_screenshots` renders the real UI
offscreen with egui_kittest into `target/bake_test/ui/`.

## Map of the code

| Where | What |
|---|---|
| `src/main.rs` | Entry point, window options, panic hook (`crash.rs`) |
| `src/app.rs` + `src/app/` | The UI shell: workspaces (Material Paint, Material Designer, Shader Maker), docking (`dock.rs`), panels, input, actions (`Action` enum, processed once per frame), projects (`project_io.rs`), preferences, export window, welcome window |
| `src/engine.rs` | One texture set: layer stack (paint, fill, folder), masks and mask filters, GPU painting and compositing, undo hooks |
| `src/app/texture_sets.rs` | Texture sets: one `Engine` per material of the mesh (`Mesh::sets`); the edited one is `PainterApp::engine`, the others are parked. Views, bakes, export and projects cover every set |
| `src/app/mesh_set.rs` | Mesh sets: several single-set meshes sharing one layer stack, one shown at a time. Switching swaps the mesh (re-bake, usually from the bake cache) and the UV-bound data (`engine/uv_data.rs`: paint channels, paint masks, map fixes); fills, generators and settings stay shared. Saved under `mesh_set` in projects; MCP `mesh_set_add/switch/remove` |
| `src/bake.rs` | Mesh-map baking: CPU scene prep on a worker thread, then GPU passes pumped a few tiles per frame |
| `src/bake_cache.rs` | On-disk cache of baked maps, keyed by mesh, settings and the bake code itself |
| `src/viewport.rs`, `src/post.rs`, `src/raytrace.rs` | 3D and 2D view rendering, post-processing, path-traced preview |
| `src/graph/` | Material graph: node definitions (`def.rs`, WGSL files with a comment header), GPU runtime, presets; built-in nodes live in `src/graph/nodes/*.wgsl` |
| `src/shaders/*.wgsl` | Engine shaders (compositing, painting stamps, baking, views) |
| `src/presets.rs` | Smart materials, materials and mask generators on the shelf |
| `src/mesh.rs`, `src/import.rs` | Meshes, procedural primitives, OBJ/FBX import, MikkTSpace tangents |
| `src/graph/shading.rs`, `src/graph/nodes/shading/`, `src/app/preview_shader.rs` | Preview shaders: shading graphs compiled into one `custom_shade(c: Shade)` for the 3D view (`Shade` and the hook are in `view.wgsl`); parameters are a uniform array |
| `src/app/path_tool.rs` | Path tool: editable curves on the surface or UV map, stamped through the normal stroke pipeline |
| `src/library.rs`, `src/app/library_ui.rs` | Asset library: Poly Haven / ambientCG catalogs and downloads, the community library (`alla2001/material-painter-community`), sharing graphs; network work on worker threads |
| `src/export.rs` | Texture export templates (engine presets) and file writing |
| `src/undo.rs` | Undo/redo of every document edit |
| `src/mcp.rs`, `src/app/mcp_tools.rs` | Optional MCP server so AI agents can drive the app (off by default) |
| `src/theme.rs`, `src/icons.rs` | UI theme (sharp corners, grey palette, blue accent) and Phosphor icon glyphs |
| `tools/`, `.claude/skills/` | Agent workflow outside the app: Python helpers (ID maps from AI-generated meshes, livery and strata/deposit masks, cliff kits, Blender parity renders) and the `material-painter-agent` / `pbr-texturing-craft` skills that explain driving MP through `mp` |

## Conventions (follow these)

- **No timing hacks.** Never fix ordering problems with sleeps, frame
  counting or timeouts. Use the existing patterns: worker threads that send
  results over `mpsc` channels, `queue.on_submitted_work_done` callbacks,
  `ctx.request_repaint()`, and the `Action` queue in `app.rs`.
- **Never block the UI thread** on disk, decoding or GPU readback. Heavy work
  goes to a worker thread; GPU work is split so a frame stays fast.
- **Every document edit is undoable.** Go through the engine's existing edit
  functions (they record undo entries) rather than mutating layers directly.
- **Partial updates.** Painting and compositing redraw only dirty regions; use
  `mark_dirty()` / stroke regions as the surrounding code does instead of
  forcing full recomposites.
- **Projects must stay loadable.** If you change what a project saves, keep
  reading old files (see the version handling in `project_io.rs`).
- **WGSL must compile on every backend** (DX12 via FXC/DXC, Vulkan, Metal).
  FXC rejects some things Naga accepts, for example writing to a vector with
  a dynamic index; use `select` or a local array. Test with the GPU tests.
- **egui 0.37 API.** Many examples online are for older egui, and several
  names changed (for example `ctx.global_style()` instead of `ctx.style()`,
  and `egui::Panel` for side and top panels). When in doubt, copy the pattern
  from this codebase and let the compiler's deprecation warnings guide you.
- **UI text** is short, plain and specific: say what a control does, not how
  it works. Tooltips explain the non-obvious.
- **Comments explain why**, not what. Doc comments on modules and public
  items, like the existing ones. No commented-out code.
- **Warning-free** under `-D warnings`. No `#[allow(dead_code)]` to silence
  unfinished work.
- **No new dependencies** without a clear need; say why in the PR.
- **Cross-platform:** Windows, Linux and macOS. Don't hardcode paths; data
  lives in the settings folder (`crash::data_dir()`), assets are found with
  `environment::asset_dir`.

## Common agent mistakes here

- Rewriting or reformatting unrelated code. Change only what the task needs.
- Adding a new abstraction layer for a one-off. Prefer a small function next
  to the code that uses it.
- Re-implementing something that exists: check `gpu.rs` helpers, `theme.rs`
  widgets (`theme::selectable`, `theme::pill`, `theme::card`), and the engine's
  edit functions first.
- Assuming a shader change works because it compiles: run the GPU tests and
  look at the output images.
- Forgetting the 2D (UV) view or the path tracer when changing how the 3D view
  draws something.

## Before you finish

1. `cargo build` with no warnings, and `cargo test` passes.
2. GPU tests pass if you touched rendering, baking, painting, nodes or export.
3. You tried the change in the running app.
4. The diff contains only your change.
