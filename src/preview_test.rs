//! Offscreen previews: bake a mesh, render the real 3D viewport from several
//! cameras in several view modes and write the frames to
//! `target/bake_test/preview/*.png`. Nothing touches a window. Without
//! `MP_PREVIEW_MESH` the rounded cube is used.
//!
//! `MP_PREVIEW_MESH=path.fbx cargo test --bin material_painter preview -- --ignored --nocapture`

use crate::bake::BakeSettings;
use crate::bake::tests::{gpu_with_adapter, run_bake};
use crate::engine::{Engine, MAPS};
use crate::gpu::{GpuMesh, read_texture};
use crate::mesh::Mesh;
use crate::presets;
use crate::viewport::{OrbitCam, ViewMode, ViewRenderer, ViewSettings};
use eframe::{egui, egui_wgpu, wgpu};
use glam::Vec3;
use std::path::Path;
use std::sync::Arc;

fn render_state() -> egui_wgpu::RenderState {
    let (instance, adapter, device, queue) = gpu_with_adapter();
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = egui_wgpu::Renderer::new(&device, target_format, Default::default());
    egui_wgpu::RenderState {
        available_adapters: vec![adapter.clone()],
        adapter,
        instance,
        device,
        queue,
        target_format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
    }
}

fn map_mode(name: &str) -> ViewMode {
    ViewMode::Map(MAPS.iter().position(|m| m.name == name).unwrap_or_else(|| panic!("no map {name}")))
}

#[test]
#[ignore]
fn preview_mesh() {
    let mesh = match std::env::var("MP_PREVIEW_MESH").as_deref() {
        Ok("ball") => crate::mesh::Primitive::MaterialBall.build(),
        Ok(path) => Mesh::load_file(Path::new(path)).unwrap_or_else(|e| panic!("{path}: {e}")),
        Err(_) => crate::mesh::Primitive::RoundedCube.build(),
    };
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 2048;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &BakeSettings::default(), res);
    engine.set_maps(maps, report);

    // A smart material that leans on the edge, cavity and AO maps.
    let smart_name = std::env::var("MP_PREVIEW_SMART").unwrap_or_else(|_| "Painted Steel Worn".into());
    let smart = presets::SMART_MATERIALS.iter().find(|s| s.name == smart_name).unwrap_or_else(|| panic!("no smart material {smart_name}"));
    crate::app::add_smart_material(&mut engine, &mut crate::undo::UndoStack::default(), smart, 0, None);
    engine.composite();

    // Cameras: the whole mesh, then close-ups of both ends of its long axis.
    let (lo, hi) = (mesh.bounds_min, mesh.bounds_max);
    let ext = hi - lo;
    let axis = if ext.x >= ext.y && ext.x >= ext.z {
        Vec3::X
    } else if ext.y >= ext.z {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let center = (lo + hi) * 0.5;
    let half = ext.dot(axis) * 0.5;
    let side_yaw = if axis == Vec3::X { 0.0 } else { std::f32::consts::FRAC_PI_2 };
    let cams = [
        ("full", OrbitCam { target: center, dist: 3.4, yaw: side_yaw + 0.3, pitch: 0.25, ..Default::default() }),
        ("end_a", OrbitCam { target: center + axis * half * 0.75, dist: 1.1, yaw: side_yaw + 0.6, pitch: 0.35, ..Default::default() }),
        ("end_b", OrbitCam { target: center - axis * half * 0.75, dist: 1.1, yaw: side_yaw - 0.5, pitch: 0.3, ..Default::default() }),
    ];
    let modes = [
        ("material", ViewMode::Material),
        ("ao", map_mode("Ambient Occlusion")),
        ("curvature", map_mode("Curvature")),
        ("edge", map_mode("Edge Distance")),
        ("cavity", map_mode("Cavity")),
        ("normal", map_mode("Normal")),
    ];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/preview");
    std::fs::create_dir_all(&dir).unwrap();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let size = [960, 640];
    for (cname, cam) in &cams {
        for (mname, mode) in &modes {
            let settings = ViewSettings { mode: *mode, ..Default::default() };
            views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, cam, &settings, None);
            let img = views.view3d.image();
            let bytes = read_texture(&rs.device, &rs.queue, img);
            let file = dir.join(format!("{cname}_{mname}.png"));
            image::save_buffer(&file, &bytes, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
        }
    }
    println!("  previews in {}", dir.display());
}


/// Every kind of edit, undone back to the start and redone to the end, must
/// reproduce the exact layer stack at each step.
#[test]
#[ignore]
fn undo_redo_roundtrip() {
    use crate::engine::{Blend, Content, FillParams, GenKind};
    use crate::undo::UndoStack;

    let rs = render_state();
    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 256);
    let mut undo = UndoStack::default();

    // (ids, names, opacity, blend, has mask, has generator, is fill)
    let snapshot = |e: &Engine| -> Vec<(u64, String, u32, Blend, bool, bool, bool)> {
        e.layers
            .iter()
            .map(|l| {
                let m = l.mask.as_ref();
                (l.id, l.name.clone(), l.opacity.to_bits(), l.blend, m.is_some(), m.is_some_and(|m| m.generator.is_some()), matches!(l.content, Content::Fill(_)))
            })
            .collect()
    };
    let mut states = vec![snapshot(&engine)];
    let mut step = |e: &mut Engine, u: &mut UndoStack, f: &dyn Fn(&mut Engine, &mut UndoStack)| {
        f(e, u);
        e.commit_props(u);
        states.push(snapshot(e));
    };
    step(&mut engine, &mut undo, &|e, u| {
        e.add_paint_layer(0, None, None, u);
    });
    step(&mut engine, &mut undo, &|e, u| {
        e.add_fill_layer(1, None, "Fill".into(), FillParams::default(), u);
    });
    let smart = &presets::SMART_MATERIALS[0];
    step(&mut engine, &mut undo, &|e, u| {
        crate::app::add_smart_material(e, u, smart, 2, None);
    });
    step(&mut engine, &mut undo, &|e, _| {
        e.layers[0].opacity = 0.25;
        e.layers[0].blend = Blend::Multiply;
        e.layers[1].name = "Renamed".into();
    });
    step(&mut engine, &mut undo, &|e, u| {
        e.move_layer(0, 3, u);
    });
    step(&mut engine, &mut undo, &|e, u| e.add_mask(0, true, None, u));
    step(&mut engine, &mut undo, &|e, u| e.add_mask(1, true, Some(GenKind::ALL[0].default_generator()), u));
    step(&mut engine, &mut undo, &|e, u| e.remove_mask(0, u));
    step(&mut engine, &mut undo, &|e, u| {
        e.duplicate_layer(2, u);
    });
    step(&mut engine, &mut undo, &|e, u| e.clear_layer(4, u));
    step(&mut engine, &mut undo, &|e, u| e.delete_layer(1, u));
    step(&mut engine, &mut undo, &|e, u| e.delete_layer(0, u));

    let n = states.len() - 1;
    println!("  labels: {:?}", undo.undo.iter().map(|e| e.label.as_str()).collect::<Vec<_>>());
    assert_eq!(undo.undo.len(), n, "one history entry per step: {:?}", undo.undo.iter().map(|e| &e.label).collect::<Vec<_>>());
    for i in (0..n).rev() {
        engine.undo(&mut undo);
        assert_eq!(snapshot(&engine), states[i], "undo back to step {i}");
    }
    for (i, state) in states.iter().enumerate().skip(1) {
        engine.redo(&mut undo);
        assert_eq!(&snapshot(&engine), state, "redo to step {i}");
    }
    println!("  {} steps undone and redone: {:?}", n, undo.undo.iter().map(|e| e.label.as_str()).collect::<Vec<_>>());
}

/// Every built-in export template, in every format, from a baked and
/// painted rounded cube: files exist, decode, have the right size and
/// channel count, and padding fills the space between UV islands.
#[test]
#[ignore]
fn export_templates() {
    use crate::export::{self, Depth, Format, Padding, Settings};
    let mesh = Arc::new(crate::mesh::Primitive::RoundedCube.build());
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 512;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &BakeSettings::default(), res);
    engine.set_maps(maps, report);
    let smart = presets::SMART_MATERIALS.iter().find(|s| s.name == "Painted Steel Worn").unwrap();
    crate::app::add_smart_material(&mut engine, &mut crate::undo::UndoStack::default(), smart, 0, None);
    engine.composite();
    let snap = export::snapshot(&engine, &rs.device, &rs.queue, mesh.clone());

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/export");
    let _ = std::fs::remove_dir_all(&root);
    for (fi, format) in Format::ALL.into_iter().enumerate() {
        for (ti, t) in export::builtin_templates().iter().enumerate() {
            // Feature templates need tagged layers (see `feature_material_export`).
            if t.maps.iter().any(|m| m.chans.iter().any(|c| c.src == export::Source::Features)) {
                continue;
            }
            // Every template in PNG; the other formats with two templates.
            if format != Format::Png && ti > 1 {
                continue;
            }
            let settings = Settings {
                folder: root.join(format.ext()).join(format!("{ti}")),
                mesh_name: "Cube".into(),
                project_name: "Test".into(),
                set_name: "Cube".into(),
                set_count: 1,
                format,
                depth: *format.depths().last().unwrap(),
                size: (fi % 2 == 1).then_some(256),
                padding: if ti == 1 { Padding::Transparent } else { Padding::Infinite },
                normal_strength: 1.0,
                use_baked_normal: true,
            };
            let files = export::export(&snap, t, &settings).unwrap_or_else(|e| panic!("{} {}: {e}", t.name, format.name()));
            assert_eq!(files.len(), t.maps.len());
            for (f, m) in files.iter().zip(&t.maps) {
                let img = image::open(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
                let size = settings.size.unwrap_or(res);
                assert_eq!((img.width(), img.height()), (size, size), "{}", f.display());
                let want = match (m.layout, format) {
                    (export::Layout::Rgba, Format::Jpeg) => 3,
                    (export::Layout::Gray, Format::Exr) => 3,
                    (l, _) => l.channels(),
                };
                assert_eq!(img.color().channel_count() as usize, want, "{}", f.display());
            }
        }
    }
    // Infinite padding leaves no untouched black between islands of the
    // base color (the cube's islands are surrounded by empty UV space).
    let base = image::open(root.join("png/0/T_Cube_BaseColor.png")).unwrap().to_rgb8();
    let black = base.pixels().filter(|p| p.0 == [0, 0, 0]).count();
    assert!(black < (res * res / 200) as usize, "{black} black texels after padding");
    let _ = Depth::U8;
}

/// A ground tile made of three features exports the game layout: masks say
/// where each tagged layer still shows (layers above cover those below,
/// untagged layers change nothing), and the manifest names the features.
#[test]
#[ignore]
fn feature_material_export() {
    use crate::engine::{FillParams, MatValues};
    use crate::export::{self, Format, Padding, Settings};
    let mesh = Arc::new(crate::mesh::Primitive::Plane.build());
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 256;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let mut undo = crate::undo::UndoStack::default();
    while !engine.layers.is_empty() {
        engine.delete_layer(0, &mut undo);
    }
    let fill = |color: [f32; 3], height: f32| FillParams {
        mat: MatValues { enabled: [true, true, false, true], color, rough: 0.7, metal: 0.0, height },
        ..FillParams::default()
    };
    // (name, color, opacity, feature)
    let stack = [
        ("Rock", [0.4, 0.4, 0.42], 1.0, Some(0)),
        ("Moss", [0.25, 0.4, 0.15], 0.6, Some(1)),
        ("Pebbles", [0.5, 0.45, 0.4], 0.5, Some(2)),
        ("Tint", [0.6, 0.5, 0.4], 1.0, None),
    ];
    for (name, color, opacity, feature) in stack {
        let i = engine.add_fill_layer(engine.layers.len(), None, name.into(), fill(color, 0.2), &mut undo);
        engine.layers[i].opacity = opacity;
        engine.layers[i].feature = feature;
    }
    engine.features = crate::engine::Features { names: ["rock".into(), "moss".into(), "pebbles".into()], tile_m: 2.0, relief_m: 0.05 };
    engine.mark_dirty();
    engine.composite();
    let snap = export::snapshot(&engine, &rs.device, &rs.queue, mesh.clone());

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/features");
    let _ = std::fs::remove_dir_all(&root);
    let template = export::builtin_templates().into_iter().find(|t| t.name == "Game material (features)").expect("built in");
    let settings = Settings {
        folder: root.clone(),
        mesh_name: "Plane".into(),
        project_name: "RockMoss".into(),
        set_name: "Plane".into(),
        set_count: 1,
        format: Format::Png,
        depth: crate::export::Depth::U8,
        size: None,
        padding: Padding::Infinite,
        normal_strength: 1.0,
        use_baked_normal: false,
    };
    let files = export::export(&snap, &template, &settings).expect("exports");
    assert_eq!(files.len(), 3, "two maps and the manifest: {files:?}");
    let b = image::open(root.join("RockMoss_NormalMasks.png")).unwrap().to_rgba8();
    let p = b.get_pixel(res / 2, res / 2).0;
    // Pebbles (0.5) cover half of the moss (0.6): moss 0.3, pebbles 0.5; the
    // untagged tint on top changes nothing.
    let near = |v: u8, want: f32| (v as f32 / 255.0 - want).abs() < 0.02;
    assert!(near(p[2], 0.3) && near(p[3], 0.5), "masks at the center: {p:?}");
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("RockMoss.material.json")).unwrap()).unwrap();
    assert_eq!(manifest["features"]["mask2"], "pebbles");
    assert_eq!(manifest["maps"][1]["channels"][2], "mask1");
    assert_eq!(manifest["relief_m"], 0.05);

    // Without tags the template refuses rather than writing empty masks.
    for l in &mut engine.layers {
        l.feature = None;
    }
    engine.mark_dirty();
    engine.composite();
    let snap = export::snapshot(&engine, &rs.device, &rs.queue, mesh);
    assert!(export::export(&snap, &template, &settings).is_err());

    // Smart masks: moss and pebbles in clouds. Where both show, pebbles
    // cover moss, so the masks never add up past 1.
    engine.layers[1].feature = Some(1);
    engine.layers[2].feature = Some(2);
    for (i, seed) in [(1, 3.0), (2, 11.0)] {
        engine.layers[i].opacity = 1.0;
        let mut g = crate::engine::GenKind::Clouds.default_generator();
        g.seed = seed;
        engine.add_mask(i, true, Some(g), &mut undo);
    }
    engine.mark_dirty();
    engine.composite();
    let snap = export::snapshot(&engine, &rs.device, &rs.queue, Arc::new(crate::mesh::Primitive::Plane.build()));
    // At a smaller size too: resizing must not bleed mask 2 (alpha) into mask 1.
    let settings = Settings { size: Some(res / 2), ..settings };
    export::export(&snap, &template, &settings).expect("exports");
    let b = image::open(root.join("RockMoss_NormalMasks.png")).unwrap().to_rgba8();
    let (mut m1, mut m2, mut worst) = (0.0, 0.0, 0u32);
    for p in b.pixels() {
        m1 += p.0[2] as f32;
        m2 += p.0[3] as f32;
        worst = worst.max(p.0[2] as u32 + p.0[3] as u32);
    }
    let n = (res * res / 4) as f32 * 255.0;
    println!("  clouds: mask1 {:.3}, mask2 {:.3}, largest sum {}", m1 / n, m2 / n, worst);
    assert!(m1 / n > 0.05 && m2 / n > 0.05, "both masks show somewhere");
    assert!(worst <= 257, "masks overlap: sum {worst}/255");
}

/// Height blend: a fill shows only where its height rises above the stack
/// below, and its feature mask follows exactly what composited (checked by
/// recovering each texel's coverage from the color).
#[test]
#[ignore]
fn height_blend_features() {
    use crate::engine::{FillParams, MatValues};
    use crate::export::{self, Format, Padding, Settings};
    let mesh = Arc::new(crate::mesh::Primitive::Plane.build());
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 256;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let mut undo = crate::undo::UndoStack::default();
    while !engine.layers.is_empty() {
        engine.delete_layer(0, &mut undo);
    }
    let base = FillParams { mat: MatValues { enabled: [true, false, false, true], color: [0.0, 0.0, 0.0], height: 0.0, ..MatValues::default() }, ..FillParams::default() };
    let top = FillParams {
        mat: MatValues { enabled: [true, false, false, true], color: [1.0, 1.0, 1.0], height: 0.0, ..MatValues::default() },
        height_var: 0.6,
        var_scale: 6.0,
        height_blend: 0.05,
        ..FillParams::default()
    };
    engine.add_fill_layer(0, None, "Soil".into(), base, &mut undo);
    engine.add_fill_layer(1, None, "Grass".into(), top, &mut undo);
    engine.layers[0].feature = Some(0);
    engine.layers[1].feature = Some(1);
    engine.features = crate::engine::Features { names: ["soil".into(), "grass".into(), String::new()], tile_m: 1.0, relief_m: 0.05 };
    engine.mark_dirty();
    engine.composite();
    let snap = export::snapshot(&engine, &rs.device, &rs.queue, mesh);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/height_blend");
    let _ = std::fs::remove_dir_all(&root);
    let template = export::builtin_templates().into_iter().find(|t| t.name == "Game material (features)").expect("built in");
    let settings = Settings {
        folder: root.clone(),
        mesh_name: "Plane".into(),
        project_name: "SoilGrass".into(),
        set_name: "Plane".into(),
        set_count: 1,
        format: Format::Png,
        depth: crate::export::Depth::U8,
        size: None,
        padding: Padding::None,
        normal_strength: 1.0,
        use_baked_normal: false,
    };
    export::export(&snap, &template, &settings).expect("exports");
    let a = image::open(root.join("SoilGrass_ColorHeight.png")).unwrap().to_rgba8();
    let b = image::open(root.join("SoilGrass_NormalMasks.png")).unwrap().to_rgba8();
    let (mut shown, mut worst) = (0.0f32, 0.0f32);
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        // Black below, white above, mixed as stored (sRGB values): the
        // color is the coverage.
        let cover = pa.0[0] as f32 / 255.0;
        let mask1 = pb.0[2] as f32 / 255.0;
        shown += mask1;
        worst = worst.max((cover - mask1).abs());
    }
    let shown = shown / (res * res) as f32;
    println!("  grass shows on {:.0}% of the tile, mask vs composite differ by at most {worst:.3}", shown * 100.0);
    assert!(shown > 0.2 && shown < 0.8, "height blend shows grass only where it rises: {shown}");
    assert!(worst < 0.03, "the feature mask follows the composite: off by {worst}");
}

/// The color nodes from the feature-grading workflow, on a real pre-blended
/// scan (grass and rubble, from `MP_WORKSPACE/library/_incoming`; skipped
/// when it isn't there): Color to Mask finds the grass, HCL Adjust turns hue
/// without changing lightness, and the mean tools land on their targets.
#[test]
#[ignore]
fn color_nodes_on_scan() {
    use crate::graph::{Graph, Library, Runtime, Value};
    let workspace = std::env::var("MP_WORKSPACE").map(std::path::PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join(".."));
    let scan = workspace.join("library/_incoming/grass_and_rubble_pjwey0_4k/Grass_And_Rubble_pjwey0_4K_BaseColor.jpg");
    let soil = workspace.join("library/_incoming/soil_dirt_pkbpp20_4k/Soil_Dirt_pkbpp20_4K_BaseColor.jpg");
    if !scan.exists() || !soil.exists() {
        println!("  scans not found under {}: skipped", workspace.display());
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/color_nodes");
    std::fs::create_dir_all(&dir).unwrap();
    // Small copies: the nodes see 256 texels either way.
    let small = |from: &Path, name: &str| {
        let p = dir.join(name);
        image::open(from).unwrap().resize_exact(256, 256, image::imageops::FilterType::Triangle).to_rgb8().save(&p).unwrap();
        p
    };
    let (scan, soil) = (small(&scan, "grass_rubble.png"), small(&soil, "soil.png"));

    let rs = render_state();
    let mesh = crate::mesh::Primitive::Plane.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 64);
    let maps = |m: usize| engine.mesh_map(m);
    let lib = Library::load(None);
    let mut rt = Runtime::new(&rs.device, &rs.queue);
    let half = |b: &[u8]| {
        let h = u16::from_le_bytes([b[0], b[1]]);
        let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 31) as i32, (h & 1023) as f32);
        let v = if e == 0 { m / 1024.0 * 2f32.powi(-14) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        if s == 1 { -v } else { v }
    };
    let mut g = Graph::new("Color nodes");
    g.res = 256;
    let bitmap = |g: &mut Graph, path: &Path| {
        let n = g.add("bitmap", [0.0; 2]);
        g.node_mut(n).unwrap().params.insert("image".into(), Value::Image(Some(path.to_path_buf())));
        n
    };
    let src = bitmap(&mut g, &scan);
    let reference = bitmap(&mut g, &soil);
    let mask = g.add("color_to_mask", [0.0; 2]);
    g.connect(src, mask, "input");
    // The scan's grass color (its greenest tenth, averaged); everything
    // here is low-chroma olive, so the tolerance is tight.
    g.node_mut(mask).unwrap().params.insert("color".into(), Value::Color([0.31, 0.357, 0.177]));
    g.node_mut(mask).unwrap().params.insert("tolerance".into(), Value::Float(0.012));
    g.node_mut(mask).unwrap().params.insert("softness".into(), Value::Float(0.01));
    g.node_mut(mask).unwrap().params.insert("lightness_weight".into(), Value::Float(0.1));
    let turned = g.add("hcl_adjust", [0.0; 2]);
    g.connect(src, turned, "input");
    g.node_mut(turned).unwrap().params.insert("hue".into(), Value::Float(0.3));
    let flat = g.add("hcl_flatten", [0.0; 2]);
    g.connect(src, flat, "input");
    g.node_mut(flat).unwrap().params.insert("chroma".into(), Value::Float(1.0));
    let matched = g.add("match_mean", [0.0; 2]);
    g.connect(src, matched, "input");
    g.connect(reference, matched, "reference");
    let gray = g.add("grayscale", [0.0; 2]);
    g.connect(src, gray, "input");
    let level = g.add("auto_level", [0.0; 2]);
    g.connect(gray, level, "input");
    rt.evaluate(&rs.device, &rs.queue, &g, &lib, &maps, 0);
    assert!(rt.errors.is_empty(), "{:?}", rt.errors);
    let read = |n: u64| -> Vec<[f32; 3]> {
        let bytes = read_texture(&rs.device, &rs.queue, rt.output(n).unwrap());
        bytes.chunks(8).map(|px| [0, 1, 2].map(|j| half(&px[j * 2..]))).collect()
    };
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    // OKLab lightness and chroma of an sRGB color (as the nodes compute them).
    let lab = |c: [f32; 3]| {
        let [r, g, b] = c.map(lin);
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).max(0.0).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).max(0.0).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).max(0.0).cbrt();
        let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
        let bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
        [0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s, a, bb]
    };
    let mean = |v: &[[f32; 3]]| v.iter().fold([0.0f32; 3], |a, c| [a[0] + c[0], a[1] + c[1], a[2] + c[2]]).map(|x| x / v.len() as f32);
    let input = read(src);
    let n = input.len() as f32;

    let m = read(mask);
    let covered = m.iter().map(|c| c[0]).sum::<f32>() / n;
    let green = |c: &[f32; 3]| c[1] - (c[0] + c[2]) * 0.5;
    let (mut g_in, mut w_in, mut g_out, mut w_out) = (0.0, 0.0, 0.0, 0.0);
    for (c, k) in input.iter().zip(&m) {
        g_in += green(c) * k[0];
        w_in += k[0];
        g_out += green(c) * (1.0 - k[0]);
        w_out += 1.0 - k[0];
    }
    println!("  color to mask: {:.0}% grass; greenness inside {:.3}, outside {:.3}", covered * 100.0, g_in / w_in, g_out / w_out);
    assert!(covered > 0.05 && covered < 0.7, "grass coverage {covered}");
    assert!(g_in / w_in > g_out / w_out + 0.03, "the mask picks greener texels");

    let t = read(turned);
    let dl = input.iter().zip(&t).map(|(a, b)| (lab(*a)[0] - lab(*b)[0]).abs()).sum::<f32>() / n;
    println!("  hcl adjust: hue turned 0.3, mean lightness change {dl:.4}");
    assert!(dl < 0.02, "turning hue keeps lightness: {dl}");

    let chroma = |v: &[[f32; 3]]| {
        let cs: Vec<f32> = v.iter().map(|c| { let l = lab(*c); (l[1] * l[1] + l[2] * l[2]).sqrt() }).collect();
        let mu = cs.iter().sum::<f32>() / cs.len() as f32;
        (cs.iter().map(|c| (c - mu) * (c - mu)).sum::<f32>() / cs.len() as f32).sqrt()
    };
    let (c_in, c_flat) = (chroma(&input), chroma(&read(flat)));
    println!("  hcl flatten: chroma spread {c_in:.4} -> {c_flat:.4}");
    assert!(c_flat < c_in * 0.25, "chroma flattened");

    let (want, got) = (lab(mean(&read(reference))), lab(mean(&read(matched))));
    println!("  match mean: reference L {:.3} a {:.3} b {:.3}, matched L {:.3} a {:.3} b {:.3}", want[0], want[1], want[2], got[0], got[1], got[2]);
    assert!((want[0] - got[0]).abs() < 0.03 && (want[1] - got[1]).abs() < 0.015 && (want[2] - got[2]).abs() < 0.015, "means match");

    let lv = mean(&read(level))[0];
    println!("  auto level: mean {lv:.3}");
    assert!((lv - 0.5).abs() < 0.03, "balanced around mid grey: {lv}");

    let mut sheet = image::RgbImage::new(256 * 3, 256);
    for (k, v) in [input, m, t].iter().enumerate() {
        for (i, c) in v.iter().enumerate() {
            sheet.put_pixel(k as u32 * 256 + (i % 256) as u32, (i / 256) as u32, image::Rgb(c.map(|x| (x.clamp(0.0, 1.0) * 255.0) as u8)));
        }
    }
    sheet.save(dir.join("sheet.png")).unwrap();
}

/// A stroke across the rounded cube with each brush projection (and once
/// with grain), then every fill projection: each must put paint on the mesh
/// and comparable amounts of it. Frames go to `target/bake_test/paint/`.
#[test]
#[ignore]
fn paint_projections() {
    use crate::app::{BrushProjection, resolve_stamps};
    use crate::engine::{BrushPaint, FillParams, MatValues, Projection, StampUniform};
    use crate::undo::{StrokeTarget, UndoStack};
    use glam::Vec2;

    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let bvh = crate::bvh::Bvh::from_mesh(&mesh);
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 512;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let mut undo = UndoStack::default();
    engine.add_paint_layer(0, None, None, &mut undo);

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/paint");
    std::fs::create_dir_all(&dir).unwrap();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let cam = OrbitCam { target: (mesh.bounds_min + mesh.bounds_max) * 0.5, ..Default::default() };
    let size = [640u32, 480u32];
    let settings = ViewSettings::default();
    let save = |views: &ViewRenderer, name: &str| {
        let img = views.view3d.image();
        let bytes = read_texture(&rs.device, &rs.queue, img);
        image::save_buffer(dir.join(format!("{name}.png")), &bytes, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
    };
    let red_texels = |engine: &Engine| {
        let bytes = read_texture(&rs.device, &rs.queue, engine.final_tex(0));
        bytes.chunks(4).filter(|p| p[0] > 150 && p[1] < 90).count()
    };
    let extent = (mesh.bounds_max - mesh.bounds_min).max_element();

    let mut counts = Vec::new();
    for (name, projection, grain) in [
        ("screen", BrushProjection::Screen, false),
        ("surface", BrushProjection::Surface, false),
        ("uv", BrushProjection::Uv, false),
        ("surface_grain", BrushProjection::Surface, true),
    ] {
        engine.clear_layer(0, &mut undo);
        if grain {
            let px = crate::textures::TEXTURES[0].render(256);
            engine.set_brush_grain(Some((&px, 256)));
        }
        views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, &cam, &settings, None);
        assert!(engine.begin_stroke(0, StrokeTarget::Content, [true, false, false, false], false));
        let radius = 28.0;
        let raw: Vec<([f32; 4], f32)> = (0..60).map(|i| ([180.0 + i as f32 * 5.0, 250.0, 1.0, 0.0], 1.0)).collect();
        let fsize = Vec2::new(size[0] as f32, size[1] as f32);
        let stamps = resolve_stamps(&mesh, &bvh, &cam, fsize, projection, 1, radius, &raw);
        assert!(!stamps.is_empty(), "{name}: no stamps hit the mesh");
        let su = StampUniform {
            view_proj: cam.view_proj(fsize.x / fsize.y).to_cols_array_2d(),
            cam_pos: cam.eye().extend(1.0).to_array(),
            view: [fsize.x, fsize.y, 0.0, 0.0],
            view2d: [0.0; 4],
            brush: [radius, 0.6, 1.0, 1.0],
            clip: [crate::viewport::NEAR, crate::viewport::FAR, 0.0, 0.0],
            proj: [projection as u32 as f32, 0.0, 85f32.to_radians().cos(), 0.0],
        };
        let paint = BrushPaint {
            mat: MatValues { enabled: [true, false, false, false], color: [0.9, 0.1, 0.05], ..Default::default() },
            mask_value: 1.0,
            opacity: 1.0,
            erase: false,
            grain: if grain { [1.0, 3.0 / extent, 0.0, 0.0] } else { [0.0; 4] },
        };
        engine.stroke_step(&stamps, su, views.view3d.depth_view(), &paint, None);
        engine.end_stroke();
        engine.composite();
        views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, &cam, &settings, None);
        save(&views, name);
        let n = red_texels(&engine);
        println!("  {name}: {} stamps, {n} painted texels", stamps.len());
        counts.push(n);
    }
    let (screen, surface, uv, grained) = (counts[0], counts[1], counts[2], counts[3]);
    assert!(screen > 500, "screen projection painted {screen} texels");
    // Same brush circle on screen: the surface and UV discs cover a
    // similar area (the surface one more, as it wraps the bevels).
    for (name, n) in [("surface", surface), ("uv", uv)] {
        assert!(n * 3 > screen && n < screen * 4, "{name} painted {n} texels vs screen {screen}");
    }
    assert!(grained < surface, "grain should hold back some paint ({grained} vs {surface})");

    // Fill projections with an image.
    engine.delete_layer(0, &mut undo);
    let png = {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&crate::textures::TEXTURES[0].render(256), 256, 256, image::ExtendedColorType::L8)
            .unwrap();
        png
    };
    let img = engine.decode_fill_image("grain.png".into(), Arc::new(png)).unwrap();
    for p in Projection::ALL {
        let fill = FillParams { projection: p, ..FillParams::default() };
        let idx = engine.add_fill_layer(0, None, p.name().into(), fill, &mut undo);
        engine.set_fill_image(idx, 0, Some(img.clone()), &mut undo);
        engine.composite();
        views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, &cam, &settings, None);
        save(&views, &format!("fill_{}", p.key()));
        let bytes = read_texture(&rs.device, &rs.queue, engine.final_tex(0));
        let lit = bytes.chunks(4).filter(|px| px[0] > 20).count();
        assert!(lit > 1000, "{}: image didn't show ({lit} texels)", p.name());
        engine.delete_layer(idx, &mut undo);
    }
    println!("  frames in {}", dir.display());
}

/// GPU time of one painting frame at 4K: stamping + applying a stroke step,
/// then recompositing a smart material stack with a paint layer on top.
/// `cargo test --bin material_painter perf_4k -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_4k() {
    use crate::app::{BrushProjection, resolve_stamps};
    use crate::engine::{BrushPaint, MatValues, StampUniform};
    use crate::undo::{StrokeTarget, UndoStack};
    use glam::Vec2;
    use std::time::Instant;

    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let bvh = crate::bvh::Bvh::from_mesh(&mesh);
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 4096;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let mut undo = UndoStack::default();
    let smart = presets::SMART_MATERIALS.iter().find(|s| s.name == "Painted Steel Worn").unwrap();
    crate::app::add_smart_material(&mut engine, &mut undo, smart, 0, None);
    let top = engine.layers.len();
    engine.add_paint_layer(top, None, None, &mut undo);
    // A second paint layer under the smart material: painting it keeps the
    // stack below in a cache, as more passes follow it.
    engine.add_paint_layer(0, None, None, &mut undo);
    let top = top + 1;
    println!("  {} layers", engine.layers.len());
    let wait = |rs: &egui_wgpu::RenderState| {
        rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    };
    engine.composite();
    wait(&rs);

    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let cam = OrbitCam { target: (mesh.bounds_min + mesh.bounds_max) * 0.5, ..Default::default() };
    let size = [1600u32, 1000u32];
    let settings = ViewSettings::default();
    views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, &cam, &settings, None);
    wait(&rs);

    let fsize = Vec2::new(size[0] as f32, size[1] as f32);
    let su = StampUniform {
        view_proj: cam.view_proj(fsize.x / fsize.y).to_cols_array_2d(),
        cam_pos: cam.eye().extend(1.0).to_array(),
        view: [fsize.x, fsize.y, 0.0, 0.0],
        view2d: [0.0; 4],
        brush: [35.0, 0.6, 1.0, 0.0],
        clip: [crate::viewport::NEAR, crate::viewport::FAR, 0.0, 0.0],
        proj: [0.0, 0.0, 85f32.to_radians().cos(), 0.0],
    };
    let paint = BrushPaint {
        mat: MatValues { enabled: [true, true, true, false], color: [0.9, 0.1, 0.05], ..Default::default() },
        mask_value: 1.0,
        opacity: 1.0,
        erase: false,
        grain: [0.0; 4],
    };
    for (name, layer) in [("top layer", top), ("bottom layer", 0)] {
    assert!(engine.begin_stroke(layer, StrokeTarget::Content, [true, true, true, false], false));
    let frames = 30;
    let (mut t_stroke, mut t_comp, mut t_view) = (0.0, 0.0, 0.0);
    for f in 0..frames {
        let raw: Vec<([f32; 4], f32)> = (0..6).map(|i| ([500.0 + (f * 6 + i) as f32 * 3.0, 500.0, 1.0, 0.0], 1.0)).collect();
        let stamps = resolve_stamps(&mesh, &bvh, &cam, fsize, BrushProjection::Screen, 0, 35.0, &raw);
        let t = Instant::now();
        let region = crate::app::stroke_region(&mesh, &bvh, &cam, fsize, BrushProjection::Screen, 0, 35.0, &stamps);
        engine.stroke_step(&stamps, su, views.view3d.depth_view(), &paint, region);
        wait(&rs);
        t_stroke += t.elapsed().as_secs_f64();
        let t = Instant::now();
        engine.composite();
        wait(&rs);
        t_comp += t.elapsed().as_secs_f64();
        let t = Instant::now();
        views.render_3d(&rs, size, &[&engine], &env, &gpu_mesh, &cam, &settings, None);
        wait(&rs);
        t_view += t.elapsed().as_secs_f64();
    }
    // Stroke frames recomposite from the painted layer up: the result must
    // match a full composite exactly.
    let partial: Vec<Vec<u8>> = (0..4).map(|c| read_texture(&rs.device, &rs.queue, engine.final_tex(c))).collect();
    engine.mark_dirty();
    engine.composite();
    for c in 0..4 {
        assert!(read_texture(&rs.device, &rs.queue, engine.final_tex(c)) == partial[c], "{name}: channel {c} differs from a full composite");
    }
    engine.end_stroke();
    let ms = |t: f64| t * 1000.0 / frames as f64;
    println!(
        "  {name}, per frame at {res}: stroke {:.2} ms, composite {:.2} ms, 3D view {:.2} ms",
        ms(t_stroke),
        ms(t_comp),
        ms(t_view)
    );
    }
}

/// Every built-in graph node compiles and renders, fed by noise where it has
/// inputs; the starter graph produces its outputs. Thumbnails go to
/// `target/bake_test/graph/`.
#[test]
#[ignore]
fn graph_nodes_render() {
    use crate::graph::{Graph, Library, Runtime, Value};

    let rs = render_state();
    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 256);
    let lib = Library::load(None);
    let mut rt = Runtime::new(&rs.device, &rs.queue);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/graph");
    std::fs::create_dir_all(&dir).unwrap();
    let picture = dir.join("input.png");
    image::save_buffer(&picture, &crate::textures::TEXTURES[0].render(64), 64, 64, image::ExtendedColorType::L8).unwrap();
    let maps = |m: usize| engine.mesh_map(m);

    let half = |b: &[u8]| {
        let h = u16::from_le_bytes([b[0], b[1]]);
        let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 31) as i32, (h & 1023) as f32);
        let v = if e == 0 { m / 1024.0 * 2f32.powi(-14) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        if s == 1 { -v } else { v }
    };
    let texture_defs: Vec<_> = lib.defs.iter().filter(|d| !d.shading).collect();
    let mut sheet = image::RgbaImage::new(128 * 6, 128 * texture_defs.len().div_ceil(6) as u32);
    for (k, def) in texture_defs.into_iter().enumerate() {
        let mut g = Graph::new(&def.name);
        g.res = 128;
        let noise = g.add("noise", [0.0; 2]);
        let n = g.add(&def.id, [0.0; 2]);
        for inp in &def.inputs {
            g.connect(noise, n, &inp.name);
        }
        if def.params.iter().any(|p| p.kind == crate::graph::ParamKind::Image) {
            g.node_mut(n).unwrap().params.insert("image".into(), Value::Image(Some(picture.clone())));
        }
        rt.evaluate(&rs.device, &rs.queue, &g, &lib, &maps, 0);
        assert!(rt.errors.is_empty(), "{}: {:?}", def.id, rt.errors);
        let tex = rt.output(n).unwrap_or_else(|| panic!("{} rendered nothing", def.id));
        let bytes = read_texture(&rs.device, &rs.queue, tex);
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for (i, px) in bytes.chunks(8).enumerate() {
            let c = [0, 1, 2, 3].map(|j| half(&px[j * 2..]));
            assert!(c.iter().all(|v| v.is_finite()), "{}: non-finite output", def.id);
            lo = lo.min(c[0]);
            hi = hi.max(c[0]);
            let (x, y) = ((i % 128) as u32, (i / 128) as u32);
            let u8c = c.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8);
            sheet.put_pixel((k % 6) as u32 * 128 + x, (k / 6) as u32 * 128 + y, image::Rgba([u8c[0], u8c[1], u8c[2], 255]));
        }
        println!("  {:<18} red range {lo:.3}..{hi:.3}", def.id);
    }
    sheet.save(dir.join("nodes.png")).unwrap();

    // The starter graph feeds base color and roughness.
    let g = Graph::starter("Starter");
    rt.evaluate(&rs.device, &rs.queue, &g, &lib, &maps, 0);
    assert!(rt.errors.is_empty(), "{:?}", rt.errors);
    let outs = g.outputs(&lib);
    assert!(rt.output(outs[0].unwrap()).is_some() && rt.output(outs[1].unwrap()).is_some());
    // Nothing changed: nothing re-renders.
    assert!(!rt.evaluate(&rs.device, &rs.queue, &g, &lib, &maps, 0));

    // Through a fill layer: the composite shows the graph's base color, and
    // a graph's output written into a mask varies across it.
    let mut engine = engine;
    let mut undo = crate::undo::UndoStack::default();
    let fill = crate::engine::FillParams { graph: Some(1), ..Default::default() };
    let idx = engine.add_fill_layer(0, None, "Graph".into(), fill, &mut undo);
    let out = |u: usize| outs[u].and_then(|n| rt.output(n).cloned());
    engine.set_graph_images(idx, [out(0), out(1), out(2), out(3)], "Starter");
    engine.composite();
    let color = read_texture(&rs.device, &rs.queue, engine.final_tex(0));
    let distinct: std::collections::HashSet<[u8; 3]> = color.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
    assert!(distinct.len() > 50, "base color shows {} distinct colors", distinct.len());
    engine.add_mask(idx, true, None, &mut undo);
    engine.set_graph_mask(idx, rt.output(outs[1].unwrap()).unwrap());
    let mask = read_texture(&rs.device, &rs.queue, engine.layers[idx].mask.as_ref().unwrap().paint.as_ref().unwrap());
    let (lo, hi) = mask.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(hi > lo + 40, "mask range {lo}..{hi}");
    // Custom nodes: a valid one compiles; a broken one reports the
    // compiler's message instead of failing the device.
    let user = dir.join("user_nodes");
    let _ = std::fs::remove_dir_all(&user);
    let mut ulib = Library::load(Some(user.clone()));
    let id = ulib.save_user("tint", &crate::graph::template("Tint")).unwrap();
    assert!(rt.compile(&rs.device, ulib.find(&id).unwrap()).is_ok());
    let broken = crate::graph::template("Broken").replace("c.rgb * params.amount", "c.rgb * undefined_thing");
    let id = ulib.save_user("broken", &broken).unwrap();
    let err = rt.compile(&rs.device, ulib.find(&id).unwrap()).unwrap_err();
    assert!(err.contains("undefined_thing"), "{err}");
    // The location reads as a line of the user's file.
    let def = ulib.find(&id).unwrap();
    let want = broken.lines().position(|l| l.contains("undefined_thing")).unwrap() + 1;
    let mapped = def.map_error(&err);
    assert!(mapped.contains(&format!("line {want}:")), "expected line {want} in: {mapped}");
    println!("  broken node reports: {}", err.lines().find(|l| l.contains("undefined_thing")).unwrap_or(""));
    let img = engine.final_tex(0);
    image::save_buffer(dir.join("composited.png"), &color, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
}

/// The ready-made material graphs render cleanly; their base colors go to
/// `target/bake_test/graph/presets.png`.
#[test]
#[ignore]
fn graph_presets_render() {
    use crate::graph::{Library, Runtime, presets::PRESETS};
    let rs = render_state();
    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 256);
    let lib = Library::load(None);
    let maps = |m: usize| engine.mesh_map(m);
    let mut sheet = image::RgbaImage::new(256 * PRESETS.len() as u32, 256);
    for (k, p) in PRESETS.iter().enumerate() {
        let mut g = (p.build)();
        g.res = 256;
        let mut rt = Runtime::new(&rs.device, &rs.queue);
        rt.evaluate(&rs.device, &rs.queue, &g, &lib, &maps, 0);
        assert!(rt.errors.is_empty(), "{}: {:?}", p.name, rt.errors);
        let base = g.outputs(&lib)[0].unwrap();
        let thumb = rt.thumb(base).unwrap();
        let bytes = read_texture(&rs.device, &rs.queue, thumb);
        let img = image::RgbaImage::from_raw(thumb.width, thumb.height, bytes).unwrap();
        image::imageops::overlay(&mut sheet, &img, k as i64 * 256, 0);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/graph");
    std::fs::create_dir_all(&dir).unwrap();
    sheet.save(dir.join("presets.png")).unwrap();
}

/// Screenshots of the real UI, rendered offscreen by egui_kittest (no
/// window): every workspace, written to `target/bake_test/ui/`.
#[test]
#[ignore]
fn ui_screenshots() {
    use crate::app::{PainterApp, Workspace};
    // A hung frame would otherwise leave the test (and its GPU memory) running.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(600));
        eprintln!("ui_screenshots: still running after 10 minutes, giving up");
        std::process::exit(101);
    });
    use egui_kittest::kittest::Queryable;
    // The same device request as the app: the adapter's full limits.
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("ui test device"),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size([1600.0, 1300.0])
        .with_pixels_per_point(1.0)
        // A screenshot test, not an accessibility audit.
        .with_accessibility_check(false)
        .wgpu_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
        .build_eframe(|cc| PainterApp::new(cc).expect("app starts"));
    harness.run_steps(8);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/ui");
    std::fs::create_dir_all(&dir).unwrap();
    // The welcome window opens at startup.
    harness.render().expect("renders").save(dir.join("welcome.png")).unwrap();
    // Its entries light up under the pointer.
    harness.get_by_label_contains("Open project").hover();
    harness.run_steps(2);
    harness.render().expect("renders").save(dir.join("welcome_hover.png")).unwrap();
    harness.state_mut().test_close_welcome();
    harness.run_steps(2);
    harness.state_mut().test_show_seams();
    harness.run_steps(3);
    harness.render().expect("renders").save(dir.join("seams.png")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    for (name, ws) in [("paint", Workspace::Paint), ("designer", Workspace::Designer), ("shader", Workspace::ShaderMaker)] {
        harness.state_mut().set_workspace(ws);
        harness.run_steps(6);
        harness.render().expect("renders").save(dir.join(format!("{name}.png"))).unwrap();
    }
    // The shelf's plain materials, grouped by type.
    harness.state_mut().set_workspace(Workspace::Paint);
    harness.state_mut().test_shelf_materials();
    harness.run_steps(6);
    harness.render().expect("renders").save(dir.join("shelf_types.png")).unwrap();
    // A graph open on the canvas, and the paint workspace showing its layer.
    harness.state_mut().set_workspace(Workspace::Designer);
    harness.state_mut().test_add_preset(1);
    harness.run_steps(10);
    harness.render().expect("renders").save(dir.join("designer_graph.png")).unwrap();
    // The export window keeps its size over many frames (it used to grow
    // to the full width of the screen).
    harness.state_mut().set_workspace(Workspace::Paint);
    harness.state_mut().test_open_export();
    harness.run_steps(30);
    harness.render().expect("renders").save(dir.join("export.png")).unwrap();
    // HDRI pictures in the Preview tab.
    let ctx = harness.ctx.clone();
    harness.state_mut().test_show_hdris(&ctx);
    harness.run_steps(4);
    harness.render().expect("renders").save(dir.join("hdri.png")).unwrap();
    // The asset library (network: waits for the catalog, then thumbnails).
    let start = std::time::Instant::now();
    while !harness.state_mut().test_open_library() && start.elapsed().as_secs() < 60 {
        harness.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 8 {
        harness.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    harness.render().expect("renders").save(dir.join("library.png")).unwrap();
    // A downloaded material dropped on the mesh.
    harness.state_mut().test_add_downloaded();
    harness.run_steps(10);
    harness.render().expect("renders").save(dir.join("library_layer.png")).unwrap();
    // A smart material saved from the stack and applied again keeps its structure.
    let ctx = harness.ctx.clone();
    let [before, after] = harness.state_mut().test_smart_roundtrip(&ctx);
    println!("  smart material (layers, masks, generators, filters, images, folders): saved {before:?}, applied {after:?}");
    assert_eq!(before, after, "the smart material round-trips");
    harness.run_steps(10);
    harness.render().expect("renders").save(dir.join("smart_roundtrip.png")).unwrap();
    // The path tool: a stitch path in each view, previewed, then applied.
    for (name, in_3d) in [("path_3d", true), ("path_2d", false)] {
        harness.state_mut().test_path_setup(in_3d);
        harness.run_steps(4);
        harness.render().expect("renders").save(dir.join(format!("{name}_preview.png"))).unwrap();
        let status = harness.state_mut().test_path_apply();
        println!("  {name}: {status}");
        assert!(status.starts_with("Painted"), "{status}");
        harness.run_steps(6);
        harness.render().expect("renders").save(dir.join(format!("{name}.png"))).unwrap();
    }
    // A preview shader: Toon on the 3D view, and its graph in the Material Designer.
    harness.state_mut().set_workspace(Workspace::Paint);
    let toon = harness.state_mut().test_preview_preset(0);
    harness.run_steps(6);
    assert_eq!(harness.state_mut().test_preview_error(), None, "the Toon preview shader compiles in the app");
    harness.render().expect("renders").save(dir.join("preview_shader.png")).unwrap();
    harness.state_mut().test_open_graph(toon);
    harness.run_steps(8);
    harness.render().expect("renders").save(dir.join("preview_shader_graph.png")).unwrap();
    // A bake that leaves out AO and edge distance: shelf badges and mask notes.
    harness.state_mut().set_workspace(Workspace::Paint);
    let missing = (1 << 0) | (1 << 5);
    let start = std::time::Instant::now();
    while !harness.state_mut().test_bake_without(&ctx, missing) && start.elapsed().as_secs() < 120 {
        harness.run_steps(1);
    }
    harness.state_mut().test_show_missing();
    harness.run_steps(8);
    harness.render().expect("renders").save(dir.join("missing_maps.png")).unwrap();
    harness.state_mut().test_show_custom();
    harness.run_steps(8);
    harness.render().expect("renders").save(dir.join("bake_custom.png")).unwrap();
    // A mask reading a custom map: the map picker and the short inputs list.
    harness.state_mut().test_custom_mask();
    harness.run_steps(8);
    harness.render().expect("renders").save(dir.join("mask_custom.png")).unwrap();
}

/// Mask filters change what a layer covers: a white mask shows the layer
/// everywhere, Invert hides it, noise breakup eats into it, blur on a flat
/// mask changes nothing.
#[test]
#[ignore]
fn mask_filters_apply() {
    use crate::engine::{FillParams, FilterKind, MaskFilter, MatValues};
    let rs = render_state();
    let mesh = crate::mesh::Primitive::RoundedCube.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 256);
    let mut undo = crate::undo::UndoStack::default();
    let fill = FillParams { mat: MatValues { enabled: [true, false, false, false], color: [1.0, 0.0, 0.0], ..Default::default() }, ..Default::default() };
    let idx = engine.add_fill_layer(0, None, "Red".into(), fill, &mut undo);
    engine.add_mask(idx, true, None, &mut undo);
    let red = |e: &mut Engine| {
        e.composite();
        let px = read_texture(&rs.device, &rs.queue, e.final_tex(0));
        let red = px.chunks(4).filter(|p| p[0] > 200 && p[1] < 60).count();
        red as f32 / (px.len() / 4) as f32
    };
    let full = red(&mut engine);
    assert!(full > 0.95, "white mask: {full}");
    let set = |e: &mut Engine, filters: Vec<MaskFilter>| {
        e.layers[idx].mask.as_mut().unwrap().filters = filters;
        e.mark_dirty();
    };
    set(&mut engine, vec![MaskFilter::new(FilterKind::Blur)]);
    let blurred = red(&mut engine);
    assert!(blurred > 0.95, "blurred white mask: {blurred}");
    set(&mut engine, vec![MaskFilter::new(FilterKind::Invert)]);
    let inverted = red(&mut engine);
    assert!(inverted < 0.01, "inverted: {inverted}");
    let mut noise = MaskFilter::new(FilterKind::Noise);
    noise.a = 1.0;
    set(&mut engine, vec![noise]);
    let broken = red(&mut engine);
    assert!(broken > 0.05 && broken < 0.9, "noise breakup: {broken}");
    // Switched off, the filter does nothing.
    let mut off = MaskFilter::new(FilterKind::Invert);
    off.enabled = false;
    set(&mut engine, vec![off]);
    assert!(red(&mut engine) > 0.95);
    println!("  red coverage: white {full:.2}, blur {blurred:.2}, invert {inverted:.2}, noise {broken:.2}");

    // Mask layers blend over the base mask.
    use crate::engine::{GenKind, MaskBlend, MaskLayer, MaskSource};
    let stack = |e: &mut Engine, layers: Vec<MaskLayer>| {
        let m = e.layers[idx].mask.as_mut().unwrap();
        m.filters.clear();
        m.layers = layers;
        e.mark_dirty();
    };
    let ml = |source, blend| MaskLayer { source, blend, opacity: 1.0, enabled: true };
    stack(&mut engine, vec![ml(MaskSource::Fill(0.0), MaskBlend::Multiply)]);
    assert!(red(&mut engine) < 0.01, "multiplied by black");
    stack(&mut engine, vec![ml(MaskSource::Fill(1.0), MaskBlend::Subtract)]);
    assert!(red(&mut engine) < 0.01, "white subtracted");
    stack(&mut engine, vec![ml(MaskSource::Fill(0.0), MaskBlend::Normal), ml(MaskSource::Generator(GenKind::Clouds.default_generator()), MaskBlend::Add)]);
    let clouds = red(&mut engine);
    assert!(clouds > 0.05 && clouds < 0.95, "clouds added over black: {clouds}");
    let mut off = ml(MaskSource::Fill(0.0), MaskBlend::Multiply);
    off.enabled = false;
    stack(&mut engine, vec![off]);
    assert!(red(&mut engine) > 0.95, "a switched-off mask layer does nothing");
    println!("  mask layers: clouds over black covers {clouds:.2}");
}

/// Every built-in shading node compiles into a preview shader on the GPU
/// (one small graph each), and every preset renders: images in
/// `target/bake_test/shading/`.
#[test]
#[ignore]
fn preview_shaders_compile_and_render() {
    use crate::graph::{Graph, Library, shading};
    let rs = render_state();
    let mesh = crate::mesh::Primitive::MaterialBall.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 1024;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &BakeSettings::default(), res);
    engine.set_maps(maps, report);
    let smart = presets::SMART_MATERIALS.iter().find(|s| s.name == "Painted Steel Worn").expect("smart material");
    crate::app::add_smart_material(&mut engine, &mut crate::undo::UndoStack::default(), smart, 0, None);
    engine.composite();
    let lib = Library::load(None);
    let mut views = ViewRenderer::new(&rs);

    // Each node alone, its output into the Shader Output.
    let mut failures = Vec::new();
    for def in lib.defs.iter().filter(|d| d.shading && d.id != shading::OUTPUT) {
        let mut g = Graph::new(&def.name);
        let n = g.add(&def.id, [0.0, 0.0]);
        let o = g.add(shading::OUTPUT, [200.0, 0.0]);
        g.connect(n, o, "color");
        let compiled = shading::compile(&g, &lib).unwrap_or_else(|e| panic!("{}: {e}", def.name));
        if let Err(e) = views.set_custom_shader(&rs.device, Some(&compiled.code)) {
            failures.push(format!("{}: {e}", def.name));
        }
    }
    assert!(failures.is_empty(), "shading nodes that don't compile:\n{}", failures.join("\n"));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/shading");
    std::fs::create_dir_all(&dir).unwrap();
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let cam = OrbitCam { dist: 3.2, yaw: 0.5, pitch: 0.3, ..Default::default() };
    let settings = ViewSettings::default();
    let mut shots: Vec<(String, Option<Graph>)> = vec![("Default".into(), None)];
    shots.extend(shading::presets().into_iter().map(|g| (g.name.clone(), Some(g))));
    for (name, graph) in &shots {
        match graph {
            Some(g) => {
                let compiled = shading::compile(g, &lib).unwrap();
                views.set_custom_shader(&rs.device, Some(&compiled.code)).unwrap_or_else(|e| panic!("{name}: {e}"));
                views.set_custom_params(&rs.queue, &shading::param_values(&compiled, g, &lib));
            }
            None => views.set_custom_shader(&rs.device, None).unwrap(),
        }
        views.render_3d(&rs, [640, 640], &[&engine], &env, &gpu_mesh, &cam, &settings, None);
        let img = views.view3d.image();
        let bytes = read_texture(&rs.device, &rs.queue, img);
        let file = dir.join(format!("{}.png", name.replace(' ', "_").replace('+', "and")));
        image::save_buffer(&file, &bytes, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
        println!("  {name} -> {}", file.display());
    }
}

/// Reads a project's mesh (`MP_PROJECT=path.mpp`), reports what's wrong with
/// it (degenerate and flipped UV triangles, overlapping UVs, ...), bakes it
/// with the project's bake settings and writes every map to
/// `target/bake_test/project/`. For diagnosing bake artifacts.
#[test]
#[ignore]
fn project_bake() {
    use glam::Vec2;
    let Ok(path) = std::env::var("MP_PROJECT") else {
        println!("set MP_PROJECT to a .mpp file");
        return;
    };
    let (mesh, manifest) = load_project_mesh(&path);

    // ---- mesh health
    let tris = mesh.indices.len() / 3;
    let (mut degenerate, mut uv_degenerate, mut outside) = (0, 0, 0);
    let mut uv_area = 0.0f64;
    let mut signs = [0usize; 2];
    let res = 2048usize;
    let mut cover = vec![0u16; res * res];
    for t in mesh.indices.chunks_exact(3) {
        let v = [t[0], t[1], t[2]].map(|i| &mesh.vertices[i as usize]);
        let p = v.map(|v| Vec3::from_array(v.pos));
        let uv = v.map(|v| Vec2::from_array(v.uv));
        if (p[1] - p[0]).cross(p[2] - p[0]).length() < 1e-10 {
            degenerate += 1;
        }
        let a = (uv[1] - uv[0]).perp_dot(uv[2] - uv[0]);
        uv_area += (a.abs() * 0.5) as f64;
        if a.abs() < 1e-12 {
            uv_degenerate += 1;
        } else {
            signs[(a < 0.0) as usize] += 1;
        }
        if uv.iter().any(|u| u.x < 0.0 || u.y < 0.0 || u.x > 1.0 || u.y > 1.0) {
            outside += 1;
        }
        // Rasterize to count texels covered more than once (overlapping UVs).
        let px = uv.map(|u| u * res as f32);
        let lo = px[0].min(px[1]).min(px[2]).max(Vec2::ZERO);
        let hi = px[0].max(px[1]).max(px[2]).min(Vec2::splat(res as f32 - 1.0));
        if lo.x > hi.x || lo.y > hi.y {
            continue;
        }
        for y in lo.y as usize..=hi.y as usize {
            for x in lo.x as usize..=hi.x as usize {
                let q = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let e = |a: Vec2, b: Vec2| (b - a).perp_dot(q - a);
                let (w0, w1, w2) = (e(px[1], px[2]), e(px[2], px[0]), e(px[0], px[1]));
                if (w0 > 0.0 && w1 > 0.0 && w2 > 0.0) || (w0 < 0.0 && w1 < 0.0 && w2 < 0.0) {
                    cover[y * res + x] = cover[y * res + x].saturating_add(1);
                }
            }
        }
    }
    let flipped = signs[0].min(signs[1]);
    let covered = cover.iter().filter(|&&c| c > 0).count();
    let overlapped = cover.iter().filter(|&&c| c > 1).count();
    // Seams: vertices sharing a position but not a UV.
    let mut by_pos: std::collections::HashMap<[u32; 3], Vec<[u32; 2]>> = Default::default();
    for v in &mesh.vertices {
        by_pos.entry(v.pos.map(f32::to_bits)).or_default().push(v.uv.map(f32::to_bits));
    }
    let seam_verts = by_pos.values().filter(|uvs| uvs.iter().any(|u| *u != uvs[0])).count();
    println!("  mesh: {tris} tris, {} verts ({} unique positions, {seam_verts} on UV seams)", mesh.vertices.len(), by_pos.len());
    println!("  degenerate tris: {degenerate} (3D), {uv_degenerate} (UV); flipped UV tris: {flipped} of {}; tris with UVs outside 0-1: {outside}", signs[0] + signs[1]);
    println!(
        "  UV area used: {:.1}%; texels covered {:.1}%, of which overlapping {:.2}% ({overlapped} texels at {res}^2)",
        uv_area * 100.0,
        covered as f64 / (res * res) as f64 * 100.0,
        overlapped as f64 / covered.max(1) as f64 * 100.0
    );
    let ov: Vec<u8> = cover.iter().map(|&c| match c {
        0 => 0,
        1 => 90,
        _ => 255,
    }).collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/project");
    std::fs::create_dir_all(&dir).unwrap();
    image::save_buffer(dir.join("uv_coverage.png"), &ov, res as u32, res as u32, image::ExtendedColorType::L8).unwrap();

    // ---- bake with the project's settings
    let bv = &manifest["bake"];
    let mut settings = BakeSettings::default();
    if let Some(q) = bv["quality"].as_str().and_then(|q| crate::bake::Quality::ALL.into_iter().find(|x| format!("{x:?}") == q)) {
        settings.apply_quality(q);
    }
    let fl = |k: &str, d: f32| bv[k].as_f64().map_or(d, |v| v as f32);
    settings.ao_distance = fl("ao_distance", settings.ao_distance);
    settings.cavity_distance = fl("cavity_distance", settings.cavity_distance);
    settings.thickness_distance = fl("thickness_distance", settings.thickness_distance);
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let bake_res: u32 = std::env::var("MP_BAKE_RES").ok().and_then(|r| r.parse().ok()).unwrap_or(2048);
    let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &settings, bake_res);
    let packed: Vec<Vec<u8>> = maps.maps.iter().map(|t| read_texture(&rs.device, &rs.queue, t)).collect();
    let fmt: Vec<_> = maps.maps.iter().map(|t| t.format).collect();
    for mi in MAPS.iter() {
        let (bytes, f) = (&packed[mi.tex], fmt[mi.tex]);
        let px = |i: usize| -> [f32; 4] {
            match f {
                wgpu::TextureFormat::Rgba16Float => {
                    let b = &bytes[i * 8..i * 8 + 8];
                    [0, 1, 2, 3].map(|k| crate::gpu::f16_to_f32(u16::from_le_bytes([b[k * 2], b[k * 2 + 1]])))
                }
                _ => [0, 1, 2, 3].map(|k| bytes[i * 4 + k] as f32 / 255.0),
            }
        };
        let n = (bake_res * bake_res) as usize;
        let img: Vec<u8> = (0..n).map(|i| (px(i)[mi.comp].clamp(0.0, 1.0) * 255.0) as u8).collect();
        let name = mi.name.to_lowercase().replace(' ', "_");
        image::save_buffer(dir.join(format!("map_{name}.png")), &img, bake_res, bake_res, image::ExtendedColorType::L8).unwrap();
    }
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), bake_res);
    engine.set_maps(maps, report);
    engine.composite();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    for (cname, yaw) in [("front", 0.0f32), ("back", std::f32::consts::PI), ("side", 1.3)] {
        let cam = OrbitCam { target: center, dist: 3.0, yaw, pitch: 0.15, ..Default::default() };
        for mname in ["Ambient Occlusion", "Curvature", "Normal", "Cavity", "Edge Distance", "Thickness"] {
            let mode = ViewMode::all().into_iter().find(|v| v.name() == mname).unwrap_or(ViewMode::Material);
            views.render_3d(&rs, [900, 900], &[&engine], &env, &gpu_mesh, &cam, &ViewSettings { mode, ..Default::default() }, None);
            let img = views.view3d.image();
            let bytes = read_texture(&rs.device, &rs.queue, img);
            let file = dir.join(format!("view_{cname}_{}.png", mname.to_lowercase().replace(' ', "_")));
            image::save_buffer(&file, &bytes, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
        }
    }
    println!("  wrote {}", dir.display());
}

/// Custom bake maps, as many as a project wants: eight fluid flows from
/// different directions next to one of each other kind (twelve maps, three
/// textures). Each bakes something plausible, flows from opposite
/// directions differ, a mask reads one of the later maps by id, and the views
/// show them by id. Images in `target/bake_test/custom/`.
/// `MP_PREVIEW_MESH=path` bakes that mesh instead of the material ball.
#[test]
#[ignore]
fn custom_maps_bake() {
    use crate::bake::CustomKind;
    use crate::engine::{CustomInput, FillParams, GenKind, MatValues};
    let mesh = match std::env::var("MP_PREVIEW_MESH") {
        Ok(path) => Mesh::load_file(Path::new(&path)).unwrap_or_else(|e| panic!("{path}: {e}")),
        Err(_) => crate::mesh::Primitive::MaterialBall.build(),
    };
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 512;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/custom");
    std::fs::create_dir_all(&dir).unwrap();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;

    let mut settings = BakeSettings::default();
    settings.apply_quality(crate::bake::Quality::Draft);
    // Eight fluid flows: from above, then from seven directions around and below.
    for (k, (az, el)) in [(0.0, 90.0), (0.0, 30.0), (90.0, 30.0), (180.0, 30.0), (270.0, 30.0), (45.0, 0.0), (225.0, 0.0), (0.0, -90.0)].into_iter().enumerate() {
        let i = settings.add_custom(CustomKind::Flow);
        let c = &mut settings.custom[i];
        (c.azimuth, c.elevation) = (az, el);
        c.name = format!("Rain {}", k + 1);
    }
    for kind in [CustomKind::Exposure, CustomKind::Deposit, CustomKind::Contact, CustomKind::Point] {
        let i = settings.add_custom(kind);
        if kind == CustomKind::Point {
            settings.custom[i].point = [mesh.bounds_max.x, center.y, mesh.bounds_max.z];
        }
    }
    let ids: Vec<u32> = settings.custom.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 12);
    assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), 12, "ids are unique");

    let start = std::time::Instant::now();
    let (baked, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &settings, res);
    println!("  12 custom maps at {res}²: {:.2}s", start.elapsed().as_secs_f32());
    assert_eq!(report.custom, ids, "every map baked, in order");
    assert_eq!(baked.custom.len(), 3, "four maps a texture");

    // Every map, from its texture channel.
    let textures: Vec<Vec<[f32; 4]>> = baked
        .custom
        .iter()
        .map(|t| {
            read_texture(&rs.device, &rs.queue, t)
                .chunks_exact(8)
                .map(|b| [0, 1, 2, 3].map(|k| crate::gpu::f16_to_f32(u16::from_le_bytes([b[k * 2], b[k * 2 + 1]]))))
                .collect()
        })
        .collect();
    let values = |i: usize| -> Vec<f32> { textures[i / 4].iter().map(|t| t[i % 4]).collect() };
    for (i, c) in settings.custom.iter().enumerate() {
        let v = values(i);
        let max = v.iter().copied().fold(0.0f32, f32::max);
        let lit = v.iter().filter(|&&x| x > 0.02).count() as f32 / v.len() as f32;
        println!("    {:>2} {:<22} max {max:.2}, {:.1}% lit", i + 1, c.name, lit * 100.0);
        assert!(max > 0.3, "{} reaches bright values (max {max})", c.name);
        assert!(lit < 0.98, "{} isn't uniformly lit ({lit})", c.name);
    }
    // Rain from above and fluid from below wet different places.
    let (above, below) = (values(0), values(7));
    let differ = above.iter().zip(&below).filter(|(a, b)| (*a - *b).abs() > 0.2).count() as f32 / above.len() as f32;
    println!("    above vs below differ on {:.0}%", differ * 100.0);
    assert!(differ > 0.05, "flows from opposite directions differ");

    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    engine.set_maps(baked, report);
    // A red fill masked by the eleventh map (Contact, in the third texture).
    let contact = ids[10];
    let mut undo = crate::undo::UndoStack::default();
    let fill = FillParams { mat: MatValues { color: [0.8, 0.1, 0.05], ..Default::default() }, ..Default::default() };
    let idx = engine.add_fill_layer(0, None, "Custom".into(), fill, &mut undo);
    let mut g = GenKind::CustomMap.default_generator();
    g.custom[0] = CustomInput { id: contact, weight: 1.0 };
    engine.add_mask(idx, true, Some(g), &mut undo);
    assert!(engine.missing_custom(&g).is_empty(), "the mask's map was baked");
    let mut gone = g;
    gone.custom[0].id = 999;
    assert_eq!(engine.missing_custom(&gone), vec![999], "a removed map is reported missing");
    engine.composite();
    let px = read_texture(&rs.device, &rs.queue, engine.final_tex(0));
    let red = px.chunks(4).filter(|p| p[0] > 150 && p[1] < 90).count() as f32 / (px.len() / 4) as f32;
    println!("    contact mask covers {:.1}% in red", red * 100.0);
    assert!(red > 0.002 && red < 0.6, "the mask reads the eleventh map ({red})");

    let mut modes: Vec<(String, ViewMode)> = vec![("material".into(), ViewMode::Material)];
    for c in &settings.custom {
        modes.push((c.name.to_lowercase().replace(' ', "_"), ViewMode::Custom(c.id)));
    }
    let cam = OrbitCam { target: center, dist: 3.2, yaw: 0.35, pitch: 0.25, ..Default::default() };
    let mut sheet = image::RgbaImage::new(4 * 300, 4 * 300);
    for (k, (mname, mode)) in modes.iter().enumerate() {
        views.render_3d(&rs, [600, 600], &[&engine], &env, &gpu_mesh, &cam, &ViewSettings { mode: *mode, ..Default::default() }, None);
        let img = views.view3d.image();
        let px = read_texture(&rs.device, &rs.queue, img);
        image::save_buffer(dir.join(format!("many_{k:02}_{mname}.png")), &px, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
        let small = image::imageops::resize(&image::RgbaImage::from_raw(img.width, img.height, px).unwrap(), 300, 300, image::imageops::FilterType::Triangle);
        image::imageops::replace(&mut sheet, &small, (k as i64 % 4) * 300, (k as i64 / 4) * 300);
    }
    sheet.save(dir.join("many_sheet.png")).unwrap();
    println!("  wrote {}", dir.display());
}

/// The live bake preview: adding custom maps bakes a quick preview, then a
/// sharper one (no restarts while nothing changes), a settings change bakes
/// again, and
/// closing the bake window ends it. Waits are time-bounded so a regression fails
/// instead of hanging.
#[test]
#[ignore]
fn live_bake_preview() {
    use crate::app::PainterApp;
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("ui test device"),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size([1600.0, 1300.0])
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .wgpu_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
        .build_eframe(|cc| PainterApp::new(cc).expect("app starts"));
    harness.run_steps(4);
    harness.state_mut().test_close_welcome();
    // A full bake first (leaving AO out), as when tuning an existing bake.
    let ctx = harness.ctx.clone();
    let mut frames = 0;
    while !harness.state_mut().test_bake_without(&ctx, 1) {
        harness.run_steps(1);
        frames += 1;
        assert!(frames < 2000, "the full bake finishes");
    }
    println!("  full bake after {frames} frames");
    harness.state_mut().test_show_custom();
    let wait = |harness: &mut egui_kittest::Harness<'_, PainterApp>, what: &str| {
        // Bounded by time: the sharpest preview step takes seconds, however
        // fast offscreen frames go.
        let start = std::time::Instant::now();
        let mut frames = 0;
        while start.elapsed().as_secs() < 60 {
            if harness.state().test_preview_shown() {
                println!("  {what}: preview after {frames} frames ({:.1}s), {} starts", start.elapsed().as_secs_f32(), harness.state().test_preview_starts());
                return;
            }
            harness.run_steps(1);
            frames += 1;
        }
        panic!("{what}: no preview after 60 s ({} starts)", harness.state().test_preview_starts());
    };
    wait(&mut harness, "custom maps added");
    let starts = harness.state().test_preview_starts();
    assert!(starts <= 4, "one preparation, a quick preview and two sharper ones ({starts} starts)");
    harness.run_steps(20);
    assert_eq!(harness.state().test_preview_starts(), starts, "no restarts while nothing changes");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/ui");
    std::fs::create_dir_all(&dir).unwrap();
    harness.render().expect("renders").save(dir.join("bake_live_preview.png")).unwrap();

    // Dragging the gizmo's handle sideways tips the fluid's direction away
    // from straight above, and the preview follows.
    let before = harness.state().test_custom(0).unwrap();
    let from = harness.state().test_gizmo_handle().expect("the flow gizmo is on screen");
    let to = from + egui::vec2(160.0, 40.0);
    let pointer = |harness: &mut egui_kittest::Harness<'_, PainterApp>, event: egui::Event| {
        harness.input_mut().events.push(event);
        harness.run_steps(1);
    };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    pointer(&mut harness, egui::Event::PointerMoved(from));
    pointer(&mut harness, button(from, true));
    for i in 1..=8 {
        pointer(&mut harness, egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
    }
    pointer(&mut harness, button(to, false));
    let after = harness.state().test_custom(0).unwrap();
    println!("  gizmo drag: elevation {:.1} -> {:.1}, compass {:.1} -> {:.1}", before.elevation, after.elevation, before.azimuth, after.azimuth);
    assert!(after.elevation < before.elevation - 5.0, "dragging the handle tipped the direction");
    wait(&mut harness, "direction dragged");
    harness.render().expect("renders").save(dir.join("bake_live_gizmo.png")).unwrap();

    harness.state_mut().test_set_bake_window(false);
    harness.run_steps(2);
    assert!(harness.state().test_preview_ended(), "closing the bake window ends the preview");
}

/// Seams in custom maps: the live preview's quick (512²) and refined
/// (1024²) bakes and a full bake of the same Wind Deposits setup, rendered close up on the material ball's
/// base. Images in `target/bake_test/custom/seams_*.png`.
#[test]
#[ignore]
fn custom_map_seams() {
    use crate::bake::CustomKind;
    // MP_PROJECT=path.mpp: a real mesh, usually cut into many UV islands.
    let mesh = match std::env::var("MP_PROJECT") {
        Ok(path) => load_project_mesh(&path).0,
        Err(_) => crate::mesh::Primitive::MaterialBall.build(),
    };
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/custom");
    std::fs::create_dir_all(&dir).unwrap();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let mut full = BakeSettings::default();
    full.apply_quality(crate::bake::Quality::Ultra);
    let i = full.add_custom(CustomKind::Deposit);
    let c = &mut full.custom[i];
    (c.azimuth, c.spread, c.distance) = (60.0, 50.0, 0.6);
    full.add_custom(CustomKind::Flow);
    let ids: Vec<u32> = full.custom.iter().map(|c| c.id).collect();
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    for (name, settings, res) in [("preview", full.preview(), 512), ("refined", full.preview(), 1024), ("full", full.clone(), 2048)] {
        let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &settings, res);
        seam_report(&rs, &mesh, &maps.maps, res, name);
        let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 2048);
        engine.set_maps(maps, report);
        engine.composite();
        for (cname, yaw, pitch) in [("right", 0.9f32, 0.35f32), ("left", -0.6, 0.35)] {
            let cam = OrbitCam { target: center - Vec3::Y * 0.4, dist: 2.2, yaw, pitch, ..Default::default() };
            for (slot, map) in [(0, "deposit"), (1, "flow")] {
                views.render_3d(&rs, [900, 700], &[&engine], &env, &gpu_mesh, &cam, &ViewSettings { mode: ViewMode::Custom(ids[slot]), ..Default::default() }, None);
                let img = views.view3d.image();
                let px = read_texture(&rs.device, &rs.queue, img);
                image::save_buffer(dir.join(format!("seams_{map}_{name}_{cname}.png")), &px, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
            }
        }
    }
}

/// A project file's mesh and its manifest (for tests that bake a real project).
fn load_project_mesh(path: &str) -> (Mesh, serde_json::Value) {
    let data = std::fs::read(&path).expect("project file");
    let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap()) as usize;
    let len = u64_at(8);
    let manifest: serde_json::Value = serde_json::from_slice(&data[16..16 + len]).unwrap();
    let mut at = 16 + len;
    let count = u64_at(at);
    at += 8;
    let mut blobs = Vec::new();
    for _ in 0..count {
        let n = u64_at(at);
        blobs.push(lz4_flex::decompress_size_prepended(&data[at + 8..at + 8 + n]).unwrap());
        at += 8 + n;
    }
    let m = &manifest["mesh"];
    let blob = |k: &str| &blobs[m[k].as_u64().unwrap() as usize];
    let vertices: Vec<crate::mesh::Vertex> = bytemuck::pod_collect_to_vec(blob("vertices"));
    let indices: Vec<u32> = bytemuck::pod_collect_to_vec(blob("indices"));
    let c: Vec<f32> = m["center"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
    let mesh = Mesh::from_saved("project".into(), vertices, indices, Vec3::new(c[0], c[1], c[2]), m["scale"].as_f64().unwrap() as f32);

    (mesh, manifest)
}

/// How much each scalar map jumps across UV seams: along every seam edge,
/// the value just inside the island on one side against the other side
/// (the same surface point), next to how much it changes between two texels
/// inside one island. A map without seams has both about equal.
fn seam_report(rs: &egui_wgpu::RenderState, mesh: &Mesh, maps: &[crate::gpu::Tex], res: u32, name: &str) {
    use crate::engine::MapKind;
    use glam::Vec2;
    let planes: Vec<Vec<[f32; 4]>> = maps
        .iter()
        .map(|t| {
            let bytes = read_texture(&rs.device, &rs.queue, t);
            match t.format {
                wgpu::TextureFormat::Rgba16Float => bytes
                    .chunks_exact(8)
                    .map(|b| [0, 1, 2, 3].map(|k| crate::gpu::f16_to_f32(u16::from_le_bytes([b[k * 2], b[k * 2 + 1]]))))
                    .collect(),
                _ => bytes.chunks_exact(4).map(|b| [0, 1, 2, 3].map(|k| b[k] as f32 / 255.0)).collect(),
            }
        })
        .collect();
    let at = |plane: &[[f32; 4]], comp: usize, uv: Vec2| -> f32 {
        let x = ((uv.x * res as f32) as i32).clamp(0, res as i32 - 1) as usize;
        let y = ((uv.y * res as f32) as i32).clamp(0, res as i32 - 1) as usize;
        plane[y * res as usize + x][comp]
    };
    // Edges by their two (quantized) end positions, with the UVs each
    // triangle gives them and the triangle's UV centroid.
    let key = |p: [f32; 3]| (Vec3::from_array(p) * 1e4).round().as_ivec3().to_array();
    let mut edges: std::collections::HashMap<([i32; 3], [i32; 3]), Vec<(Vec2, Vec2, Vec2)>> = Default::default();
    for t in mesh.indices.chunks_exact(3) {
        let v = [t[0], t[1], t[2]].map(|i| &mesh.vertices[i as usize]);
        let centroid = (Vec2::from_array(v[0].uv) + Vec2::from_array(v[1].uv) + Vec2::from_array(v[2].uv)) / 3.0;
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            let (ka, kb) = (key(a.pos), key(b.pos));
            let (ua, ub) = (Vec2::from_array(a.uv), Vec2::from_array(b.uv));
            let entry = if ka <= kb { ((ka, kb), (ua, ub)) } else { ((kb, ka), (ub, ua)) };
            edges.entry(entry.0).or_default().push((entry.1.0, entry.1.1, centroid));
        }
    }
    let texel = 1.0 / res as f32;
    let mut samples: Vec<(Vec2, Vec2, Vec2)> = Vec::new(); // side A, side B, A further in
    for list in edges.values() {
        if list.len() != 2 {
            continue;
        }
        let ((a0, a1, ca), (b0, b1, cb)) = (list[0], list[1]);
        if a0.distance(b0) < texel && a1.distance(b1) < texel {
            continue; // not a seam
        }
        if a0.distance(a1) < 4.0 * texel || b0.distance(b1) < 4.0 * texel {
            continue; // too short to sample
        }
        for t in [0.3f32, 0.5, 0.7] {
            let (pa, pb) = (a0.lerp(a1, t), b0.lerp(b1, t));
            let ina = (ca - pa).normalize_or_zero();
            let inb = (cb - pb).normalize_or_zero();
            samples.push((pa + ina * 2.0 * texel, pb + inb * 2.0 * texel, pa + ina * 4.0 * texel));
        }
    }
    println!("  {name}: {} seam samples at {res}^2; mean |jump| across seams vs between texels inside an island:", samples.len());
    for m in MAPS.iter().filter(|m| m.kind == MapKind::Scalar) {
        let plane = &planes[m.tex];
        let (mut across, mut inside) = (0.0f64, 0.0f64);
        for &(a, b, a2) in &samples {
            across += (at(plane, m.comp, a) - at(plane, m.comp, b)).abs() as f64;
            inside += (at(plane, m.comp, a) - at(plane, m.comp, a2)).abs() as f64;
        }
        let n = samples.len().max(1) as f64;
        println!("    {:<18} across {:.4}  inside {:.4}  ratio {:.1}", m.name, across / n, inside / n, across / inside.max(1e-9));
    }
}

/// Close-ups of the material ball's base with UV seams drawn over the
/// custom maps and AO, to see whether lines in a map sit on the seams.
/// Images in `target/bake_test/custom/closeup_*.png`.
#[test]
#[ignore]
fn custom_map_closeups() {
    use crate::bake::CustomKind;
    let mesh = crate::mesh::Primitive::MaterialBall.build();
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/custom");
    std::fs::create_dir_all(&dir).unwrap();
    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let mut s = BakeSettings::default();
    s.apply_quality(crate::bake::Quality::Standard);
    let i = s.add_custom(CustomKind::Deposit);
    let d = &mut s.custom[i];
    (d.azimuth, d.spread, d.distance) = (270.0, 50.0, 0.6);
    s.add_custom(CustomKind::Flow);
    s.add_custom(CustomKind::Exposure);
    let ids: Vec<u32> = s.custom.iter().map(|c| c.id).collect();
    let (maps, report, low_pos) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &s, 2048);
    // The flow map (second channel of the first custom texture) and the island mask, in UV space.
    let f = read_texture(&rs.device, &rs.queue, &maps.custom[0]);
    let flow: Vec<u8> = f.chunks_exact(8).map(|b| (crate::gpu::f16_to_f32(u16::from_le_bytes([b[2], b[3]])).clamp(0.0, 1.0) * 255.0) as u8).collect();
    image::save_buffer(dir.join("uv_flow.png"), &flow, 2048, 2048, image::ExtendedColorType::L8).unwrap();
    let lp = read_texture(&rs.device, &rs.queue, &low_pos);
    let mask: Vec<u8> = lp.chunks_exact(16).map(|b| (f32::from_le_bytes([b[12], b[13], b[14], b[15]]).clamp(0.0, 1.0) * 255.0) as u8).collect();
    image::save_buffer(dir.join("uv_islands.png"), &mask, 2048, 2048, image::ExtendedColorType::L8).unwrap();
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 2048);
    engine.set_maps(maps, report);
    engine.composite();
    let base = Vec3::new(mesh.bounds_max.x * 0.8, mesh.bounds_min.y + (mesh.bounds_max.y - mesh.bounds_min.y) * 0.2, mesh.bounds_max.z * 0.8);
    let cam = OrbitCam { target: base, dist: 1.1, yaw: 0.75, pitch: 0.45, ..Default::default() };
    for (name, mode) in [
        ("deposit", ViewMode::Custom(ids[0])),
        ("flow", ViewMode::Custom(ids[1])),
        ("exposure", ViewMode::Custom(ids[2])),
        ("ao", map_mode("Ambient Occlusion")),
        ("sky", map_mode("Sky Exposure")),
    ] {
        for seams in [false, true] {
            let settings = ViewSettings { mode, show_seams_3d: seams, ..Default::default() };
            views.render_3d(&rs, [800, 800], &[&engine], &env, &gpu_mesh, &cam, &settings, None);
            let img = views.view3d.image();
            let px = read_texture(&rs.device, &rs.queue, img);
            let file = dir.join(format!("closeup_{name}{}.png", if seams { "_seams" } else { "" }));
            image::save_buffer(&file, &px, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
        }
    }
}

/// Layer thumbnails show what the layer and its mask really do: a fill's
/// color, a mask that inverts or breaks up with noise, a painted mask, and
/// they follow changes. Images in `target/bake_test/thumbs/`.
#[test]
#[ignore]
fn layer_thumbnails() {
    use crate::engine::{FillParams, FilterKind, GenKind, MaskFilter, MatValues};
    let rs = render_state();
    let mesh = crate::mesh::Primitive::MaterialBall.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 512);
    let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &BakeSettings::default(), 512);
    engine.set_maps(maps, report);
    let mut undo = crate::undo::UndoStack::default();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/thumbs");
    std::fs::create_dir_all(&dir).unwrap();
    let stats = |engine: &Engine, tex: &crate::gpu::Tex, name: &str| -> (f32, f32, [f32; 3]) {
        let px = read_texture(&rs.device, &rs.queue, tex);
        image::save_buffer(dir.join(format!("{name}.png")), &px, tex.width, tex.height, image::ExtendedColorType::Rgba8).unwrap();
        let n = (px.len() / 4) as f32;
        let lum: Vec<f32> = px.chunks(4).map(|p| (p[0] as f32 + p[1] as f32 + p[2] as f32) / (3.0 * 255.0)).collect();
        let mean = lum.iter().sum::<f32>() / n;
        let var = lum.iter().map(|l| (l - mean) * (l - mean)).sum::<f32>() / n;
        let rgb = [0, 1, 2].map(|k| px.chunks(4).map(|p| p[k] as f32 / 255.0).sum::<f32>() / n);
        let _ = engine;
        (mean, var.sqrt(), rgb)
    };

    // A red fill with color variation, masked by edge wear.
    let fill = FillParams { mat: MatValues { color: [0.8, 0.1, 0.05], ..Default::default() }, color_var: 0.4, ..Default::default() };
    let idx = engine.add_fill_layer(0, None, "Red".into(), fill, &mut undo);
    engine.add_mask(idx, true, Some(GenKind::EdgeWear.default_generator()), &mut undo);
    let id = engine.layers[idx].id;
    engine.composite();
    let (content, mask) = engine.thumb_textures(id);
    let (_, spread, rgb) = stats(&engine, content.expect("fill thumbnail"), "fill_content");
    println!("  fill: rgb {rgb:?}, spread {spread:.3}");
    assert!(rgb[0] > rgb[1] * 2.0 && rgb[0] > rgb[2] * 2.0, "the fill thumbnail is red: {rgb:?}");
    assert!(spread > 0.01, "color variation shows ({spread})");
    let (wear, wear_spread, _) = stats(&engine, mask.expect("mask thumbnail"), "mask_edge_wear");
    println!("  edge wear mask: mean {wear:.3}, spread {wear_spread:.3}");
    assert!(wear > 0.01 && wear < 0.6 && wear_spread > 0.05, "edge wear covers the edges, not everything");

    // An Invert filter flips it.
    engine.layers[idx].mask.as_mut().unwrap().filters = vec![MaskFilter::new(FilterKind::Invert)];
    engine.mark_dirty();
    engine.composite();
    let (inverted, _, _) = stats(&engine, engine.thumb_textures(id).1.unwrap(), "mask_inverted");
    // Off the islands both are the same dark gray (0.16); on them they sum to
    // white. The fill thumbnail's alpha says how much is on them.
    let covered = {
        let px = read_texture(&rs.device, &rs.queue, engine.thumb_textures(id).0.unwrap());
        px.chunks(4).map(|p| p[3] as f32 / 255.0).sum::<f32>() / (px.len() / 4) as f32
    };
    let expected = covered + 2.0 * 0.16 * (1.0 - covered);
    println!("  inverted: mean {inverted:.3}; islands cover {:.0}%", covered * 100.0);
    assert!((wear + inverted - expected).abs() < 0.05, "inverting the mask inverts its thumbnail ({wear:.3} + {inverted:.3}, expected {expected:.3})");

    // A new fill color shows.
    if let crate::engine::Content::Fill(f) = &mut engine.layers[idx].content {
        f.mat.color = [0.05, 0.2, 0.9];
    }
    engine.mark_dirty();
    engine.composite();
    let (_, _, blue) = stats(&engine, engine.thumb_textures(id).0.unwrap(), "fill_blue");
    assert!(blue[2] > blue[0] * 2.0, "the thumbnail follows the fill's color: {blue:?}");
}

/// What darkens crevices in the 3D view: the same gray material ball with
/// the baked AO at full and at the default view strength, without AO, and
/// without the key light.
/// Images in `target/bake_test/viewport/`.
#[test]
#[ignore]
fn viewport_crevices() {
    use crate::engine::{FillParams, MatValues};
    let mesh = crate::mesh::Primitive::MaterialBall.build();
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/viewport");
    std::fs::create_dir_all(&dir).unwrap();
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    let mut views = ViewRenderer::new(&rs);
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    let cam = OrbitCam { target: center - Vec3::Y * 0.3, dist: 2.4, yaw: 0.5, pitch: 0.3, ..Default::default() };
    let make = |baked: bool| {
        let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), 1024);
        if baked {
            let (maps, report, _) = run_bake(&rs.device, &rs.queue, &mesh, &gpu_mesh, &BakeSettings::default(), 1024);
            engine.set_maps(maps, report);
        }
        let fill = FillParams { mat: MatValues { color: [0.7, 0.7, 0.7], rough: 0.5, ..Default::default() }, ..Default::default() };
        engine.add_fill_layer(0, None, "Gray".into(), fill, &mut crate::undo::UndoStack::default());
        engine.composite();
        engine
    };
    let (baked, plain) = (make(true), make(false));
    for (name, engine, key, ao) in [
        ("baked_full", &baked, true, 1.0),
        ("baked_default", &baked, true, ViewSettings::default().ao_strength),
        ("noao", &plain, true, 1.0),
        ("baked_nokey", &baked, false, 1.0),
    ] {
        let mut s = ViewSettings::default();
        s.key.on = key;
        s.ao_strength = ao;
        views.render_3d(&rs, [900, 900], &[engine], &env, &gpu_mesh, &cam, &s, None);
        let img = views.view3d.image();
        let px = read_texture(&rs.device, &rs.queue, img);
        let mean = px.chunks(4).map(|p| p[0] as f32).sum::<f32>() / (px.len() / 4) as f32;
        println!("  {name}: mean {mean:.1}");
        image::save_buffer(dir.join(format!("{name}.png")), &px, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
    }
}

/// Opening Substance 3D Painter projects of your own (`MP_SPP=a.spp;b.spp`): the flattened
/// result as a visible paint layer, the rebuilt stack hidden next to it, the report up.
/// Screenshots in `target/bake_test/spp/`. Nothing runs without the variable (no Painter file
/// ships with the tests; `spp_import`'s unit tests use a synthetic one).
#[test]
#[ignore]
fn open_spp_projects() {
    use crate::app::PainterApp;
    let Ok(paths) = std::env::var("MP_SPP") else {
        println!("set MP_SPP to .spp files of your own");
        return;
    };
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("ui test device"),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size([1600.0, 1000.0])
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .wgpu_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
        .build_eframe(|cc| PainterApp::new(cc).expect("app starts"));
    harness.run_steps(4);
    harness.state_mut().test_close_welcome();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/spp");
    std::fs::create_dir_all(&dir).unwrap();
    for (k, path) in paths.split(';').filter(|p| !p.is_empty()).enumerate() {
        let ctx = harness.ctx.clone();
        harness.state_mut().test_open_spp(Path::new(path), &ctx);
        let start = std::time::Instant::now();
        while !harness.state().test_spp_done() && start.elapsed().as_secs() < 120 {
            harness.run_steps(1);
        }
        assert!(harness.state().test_spp_done(), "{path}: the import finished");
        let layers = harness.state().test_layers();
        println!("  {path}:");
        for (name, kind, visible) in &layers {
            println!("    {kind:<12} {} {name}", if *visible { "shown " } else { "hidden" });
        }
        let flat = layers.iter().find(|(n, ..)| n == "Painter result (flattened)").expect("the flattened layer");
        assert_eq!((flat.1, flat.2), ("paint", true), "the flattened result is a visible paint layer");
        if let Some(folder) = layers.iter().find(|(n, ..)| n == "Painter layers (rebuilt)") {
            assert!(!folder.2, "the rebuilt stack starts hidden");
        }
        // Every texture set came in, each with its flattened result.
        let sets = harness.state().test_sets();
        for (name, res, layers) in &sets {
            println!("    set {name}: {res}², {} layers", layers.len());
            assert!(layers.iter().any(|l| l == "Painter result (flattened)"), "{name} has its flattened layer");
        }
        // Let the bakes land (every set's, one after another), then picture it with the report.
        let mut steps = 0;
        while harness.state().test_baking() && steps < 20_000 {
            harness.run_steps(1);
            steps += 1;
        }
        assert!(!harness.state().test_baking(), "{path}: the bakes finished");
        harness.run_steps(2);
        harness.render().expect("renders").save(dir.join(format!("spp_{k}.png"))).unwrap();
        // The path tracer reads every set.
        harness.state_mut().test_ray_trace(true);
        harness.run_steps(8);
        harness.render().expect("renders").save(dir.join(format!("spp_{k}_raytraced.png"))).unwrap();
        harness.state_mut().test_ray_trace(false);
        // Saved and opened again, every set keeps its size and layers.
        let ctx = harness.ctx.clone();
        let project = dir.join(format!("spp_{k}.mpp"));
        harness.state_mut().test_save_and_reopen(&project, &ctx).expect("saves and reopens");
        harness.run_steps(2);
        assert_eq!(harness.state().test_sets(), sets, "{path}: the project round-trips");
        let _ = std::fs::remove_file(&project);
    }
}

/// Two texture sets on one plane (left and right halves of the UV square):
/// a stroke across the whole plane paints only the edited set's half, and
/// the 3D view draws each half with its own set. The frame goes to
/// `target/bake_test/texture_sets.png`.
#[test]
#[ignore]
fn texture_sets_paint_apart() {
    use crate::app::{BrushProjection, resolve_stamps};
    use crate::engine::{BrushPaint, FillParams, MatValues, StampUniform};
    use crate::undo::{StrokeTarget, UndoStack};
    use glam::Vec2;

    let mesh = crate::mesh::set_tests::split_plane();
    let bvh = crate::bvh::Bvh::from_mesh(&mesh);
    let rs = render_state();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    let res = 256;
    let mut left = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), res);
    left.set_triangles(mesh.sets[0].index_range(), &gpu_mesh);
    let mut right = left.sibling(&gpu_mesh, mesh.sets[1].index_range(), res);
    let mut undo = UndoStack::default();
    left.add_paint_layer(0, None, None, &mut undo);
    let blue = MatValues { color: [0.05, 0.1, 0.9], ..Default::default() };
    right.add_fill_layer(0, None, "Blue".into(), FillParams { mat: blue, ..Default::default() }, &mut undo);

    let mut views = ViewRenderer::new(&rs);
    let env = crate::environment::EnvBaker::new(&rs.device)
        .upload(&rs, &crate::environment::decode(&crate::environment::EnvSource::Builtin(0)).unwrap());
    // Looking straight down at the plane.
    let cam = OrbitCam { pitch: 1.5, dist: 3.2, ..Default::default() };
    let size = [512u32, 512u32];
    let settings = ViewSettings::default();
    views.render_3d(&rs, size, &[&left, &right], &env, &gpu_mesh, &cam, &settings, None);

    // A stroke across the middle of the view, over both halves.
    assert!(left.begin_stroke(0, StrokeTarget::Content, [true, false, false, false], false));
    let radius = 40.0;
    let raw: Vec<([f32; 4], f32)> = (0..90).map(|i| ([30.0 + i as f32 * 5.0, 256.0, 1.0, 0.0], 1.0)).collect();
    let fsize = Vec2::new(size[0] as f32, size[1] as f32);
    let stamps = resolve_stamps(&mesh, &bvh, &cam, fsize, BrushProjection::Screen, 0, radius, &raw);
    assert!(!stamps.is_empty(), "the stroke hits the plane");
    let su = StampUniform {
        view_proj: cam.view_proj(fsize.x / fsize.y).to_cols_array_2d(),
        cam_pos: cam.eye().extend(1.0).to_array(),
        view: [fsize.x, fsize.y, 0.0, 0.0],
        view2d: [0.0; 4],
        // No mirroring (brush.w).
        brush: [radius, 0.6, 1.0, 0.0],
        clip: [crate::viewport::NEAR, crate::viewport::FAR, 0.0, 0.0],
        proj: [BrushProjection::Screen as u32 as f32, 0.0, 85f32.to_radians().cos(), 0.0],
    };
    let paint = BrushPaint {
        mat: MatValues { enabled: [true, false, false, false], color: [0.9, 0.1, 0.05], ..Default::default() },
        mask_value: 1.0,
        opacity: 1.0,
        erase: false,
        grain: [0.0; 4],
    };
    left.stroke_step(&stamps, su, views.view3d.depth_view(), &paint, None);
    left.end_stroke();
    left.composite();
    right.composite();

    // Red texels of the left set, by side of the UV square (past the
    // dilation margin on the right).
    let bytes = read_texture(&rs.device, &rs.queue, left.final_tex(0));
    let (mut on_left, mut on_right) = (0, 0);
    for (i, p) in bytes.chunks(4).enumerate() {
        let x = (i as u32 % res) as f32 / res as f32;
        if p[0] > 150 && p[1] < 90 {
            if x < 0.5 {
                on_left += 1;
            } else if x > 0.5 + 16.0 / res as f32 {
                on_right += 1;
            }
        }
    }
    println!("  painted texels: {on_left} in the left set's half, {on_right} past it");
    assert!(on_left > 500, "the left set took paint ({on_left})");
    assert_eq!(on_right, 0, "paint stays in the edited set");

    views.render_3d(&rs, size, &[&left, &right], &env, &gpu_mesh, &cam, &settings, None);
    let img = views.view3d.image();
    let px = read_texture(&rs.device, &rs.queue, img);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test");
    std::fs::create_dir_all(&dir).unwrap();
    image::save_buffer(dir.join("texture_sets.png"), &px, img.width, img.height, image::ExtendedColorType::Rgba8).unwrap();
    let rgb = |p: &[u8]| (p[0] as i32, p[1] as i32, p[2] as i32);
    let bluish = px.chunks(4).map(rgb).filter(|&(r, g, b)| b > r + 40 && b > g + 40).count();
    let reddish = px.chunks(4).map(rgb).filter(|&(r, g, b)| r > g + 60 && r > b + 60).count();
    println!("  view: {bluish} blue pixels (right set), {reddish} red (the stroke)");
    assert!(bluish > 5000, "the right set draws with its own textures ({bluish})");
    assert!(reddish > 500, "the stroke shows ({reddish})");
}

/// A clean picture of a Painter project opened in Material Painter (`MP_SPP_SHOT=a.spp`): no
/// dialogs, the bakes done, rasterized and fully path traced, in `target/bake_test/spp/shot*.png`.
#[test]
#[ignore]
fn spp_screenshot() {
    use crate::app::PainterApp;
    let Ok(path) = std::env::var("MP_SPP_SHOT") else {
        println!("set MP_SPP_SHOT to a .spp file of your own");
        return;
    };
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("ui test device"),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size([1600.0, 1000.0])
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .wgpu_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
        .build_eframe(|cc| PainterApp::new(cc).expect("app starts"));
    harness.run_steps(4);
    let ctx = harness.ctx.clone();
    harness.state_mut().test_open_spp(Path::new(&path), &ctx);
    let mut steps = 0;
    while !(harness.state().test_spp_done() && !harness.state().test_baking()) && steps < 50_000 {
        harness.run_steps(1);
        steps += 1;
    }
    assert!(harness.state().test_spp_done() && !harness.state().test_baking(), "imported and baked");
    harness.state_mut().test_close_dialogs();
    harness.run_steps(3);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bake_test/spp");
    std::fs::create_dir_all(&dir).unwrap();
    harness.render().expect("renders").save(dir.join("shot.png")).unwrap();
    harness.state_mut().test_ray_trace(true);
    let mut steps = 0;
    while !harness.state().test_ray_trace_done() && steps < 20_000 {
        harness.run_steps(1);
        steps += 1;
    }
    harness.run_steps(1);
    harness.render().expect("renders").save(dir.join("shot_raytraced.png")).unwrap();
}

/// Normal and emission channels: a dome normal image stays a dome under UV
/// and tri-planar projection and under rotation, tri-planar agrees with UV
/// where the two map the same way, and emission comes out linear times
/// strength.
#[test]
#[ignore]
fn normal_and_emission_channels() {
    use crate::engine::{Content, EMISSION_CH, FillParams, MatValues, NORMAL_CH, Projection};
    let rs = render_state();
    // The plane: u along +x, v along +z, facing +y, so its tri-planar top
    // projection (image right +x, up -z) matches its UVs.
    let mesh = crate::mesh::Primitive::Plane.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    const RES: u32 = 256;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), RES);
    let mut undo = crate::undo::UndoStack::default();
    // A dome: normals point away from the image center (OpenGL, green up).
    let n = 128u32;
    let dome = image::RgbaImage::from_fn(n, n, |x, y| {
        let (dx, dy) = ((x as f32 + 0.5) / n as f32 - 0.5, (y as f32 + 0.5) / n as f32 - 0.5);
        let (mut nx, mut ny) = (dx * 1.6, -dy * 1.6);
        if nx * nx + ny * ny > 0.64 {
            (nx, ny) = (0.0, 0.0);
        }
        let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
        image::Rgba([nx, ny, nz, 1.0].map(|v| ((v * 0.5 + 0.5) * 255.0).round() as u8))
    });
    let mut png = Vec::new();
    dome.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let dome = engine.decode_fill_image("dome".into(), Arc::new(png)).unwrap();
    let fill = FillParams { mat: MatValues { enabled: [false; 4], ..Default::default() }, normal_on: true, ..Default::default() };
    let idx = engine.add_fill_layer(0, None, "Dome".into(), fill, &mut undo);
    engine.set_fill_image(idx, NORMAL_CH, Some(dome), &mut undo);

    let normals = |e: &mut Engine| -> Vec<[f32; 2]> {
        e.composite();
        let px = read_texture(&rs.device, &rs.queue, e.final_tex(NORMAL_CH));
        px.chunks_exact(2).map(|c| [c[0] as f32 / 255.0 * 2.0 - 1.0, c[1] as f32 / 255.0 * 2.0 - 1.0]).collect()
    };
    let at = |v: &[[f32; 2]], u: f32, w: f32| v[((w * RES as f32) as usize).min(RES as usize - 1) * RES as usize + ((u * RES as f32) as usize).min(RES as usize - 1)];
    // Average slope along the outward direction (tangent x right, y up) on a
    // circle around the dome's center, and across it.
    let dome_check = |v: &[[f32; 2]]| {
        let (mut out, mut across) = (0.0, 0.0);
        for k in 0..16 {
            let a = k as f32 / 16.0 * std::f32::consts::TAU;
            let (dx, dy) = (a.cos(), a.sin());
            let s = at(v, 0.5 + 0.15 * dx, 0.5 - 0.15 * dy);
            out += (s[0] * dx + s[1] * dy) / 16.0;
            across += (s[0] * -dy + s[1] * dx).abs() / 16.0;
        }
        (out, across)
    };
    let set = |e: &mut Engine, proj: Projection, tiling: f32, rot: f32, offset: [f32; 2]| {
        if let Content::Fill(f) = &mut e.layers[idx].content {
            (f.projection, f.tiling, f.rotation, f.offset) = (proj, tiling, rot, offset);
        }
        e.mark_dirty();
    };
    let diff = |a: &[[f32; 2]], b: &[[f32; 2]]| {
        let r = RES as usize;
        let mut sum = 0.0;
        let mut cnt = 0.0;
        for y in r / 8..r * 7 / 8 {
            for x in r / 8..r * 7 / 8 {
                let (p, q) = (a[y * r + x], b[y * r + x]);
                sum += (p[0] - q[0]).abs() + (p[1] - q[1]).abs();
                cnt += 2.0;
            }
        }
        sum / cnt
    };
    for rot in [0.0, 90.0, 37.0] {
        set(&mut engine, Projection::Uv, 1.0, rot, [0.0, 0.0]);
        let uv = normals(&mut engine);
        let (out, across) = dome_check(&uv);
        println!("  uv rotation {rot}: outward {out:.3}, across {across:.3}");
        assert!(out > 0.15 && across < 0.06, "UV dome at {rot} degrees: outward {out}, across {across}");
        set(&mut engine, Projection::Triplanar, 0.5, rot, [0.5, 0.5]);
        let tri = normals(&mut engine);
        let d = diff(&uv, &tri);
        println!("  tri-planar vs uv at {rot}: mean difference {d:.4}");
        assert!(d < 0.03, "tri-planar should match UV on the plane at {rot} degrees: {d}");
    }
    // Strength scales the slopes; DirectX flips green.
    if let Content::Fill(f) = &mut engine.layers[idx].content {
        (f.projection, f.tiling, f.rotation, f.offset, f.normal_dx) = (Projection::Uv, 1.0, 0.0, [0.0, 0.0], true);
    }
    engine.mark_dirty();
    let dx = normals(&mut engine);
    let s = at(&dx, 0.5, 0.35);
    assert!(s[1] < -0.15, "DirectX flips the slope above the center: {s:?}");

    // Emission: linear color times strength, on top of the normal layer.
    let glow = FillParams {
        mat: MatValues { enabled: [false; 4], ..Default::default() },
        emission_on: true,
        emission: [1.0, 0.5, 0.0],
        emission_strength: 2.0,
        ..Default::default()
    };
    engine.add_fill_layer(idx + 1, None, "Glow".into(), glow, &mut undo);
    engine.composite();
    let px = read_texture(&rs.device, &rs.queue, engine.final_tex(EMISSION_CH));
    let h = |b: &[u8]| {
        let h = u16::from_le_bytes([b[0], b[1]]);
        let (s, e, m) = ((h >> 15) as i32, ((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f32);
        let v = if e == 0 { m / 1024.0 * 2f32.powi(-14) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        if s == 1 { -v } else { v }
    };
    let mid = ((RES / 2) * RES + RES / 2) as usize * 8;
    let (r, g, b) = (h(&px[mid..]), h(&px[mid + 2..]), h(&px[mid + 4..]));
    println!("  emission at the center: {r:.3} {g:.3} {b:.3}");
    assert!((r - 2.0).abs() < 0.02 && (g - 0.428).abs() < 0.02 && b.abs() < 0.01, "emission {r} {g} {b}");
}

/// Where a real document's composite time goes: opens `MP_PERF_PROJECT`,
/// times a full composite, then each visible layer's share (hidden one at a
/// time). Prints the costliest layers.
///
/// `MP_PERF_PROJECT=path/to/ship.json cargo test --bin material_painter project_composite_profile -- --ignored --nocapture`
#[test]
#[ignore]
fn project_composite_profile() {
    use crate::app::PainterApp;
    let Ok(path) = std::env::var("MP_PERF_PROJECT") else {
        println!("set MP_PERF_PROJECT to a project of your own");
        return;
    };
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("perf test device"),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size([1200.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .wgpu_setup(egui_wgpu::WgpuSetup::CreateNew(setup))
        .build_eframe(|cc| PainterApp::new(cc).expect("app starts"));
    harness.run_steps(2);
    harness.state_mut().test_close_welcome();
    let ctx = harness.ctx.clone();
    harness.state_mut().test_open_project(Path::new(&path), &ctx).expect("opens");
    let mut steps = 0;
    while harness.state().test_baking() && steps < 20_000 {
        harness.run_steps(1);
        steps += 1;
    }
    harness.run_steps(2);
    let rs = harness.state().test_render_state();
    let engine = harness.state_mut().test_engine();
    let time = |e: &mut Engine| {
        let mut best = f64::MAX;
        for _ in 0..3 {
            e.mark_dirty();
            rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let t = std::time::Instant::now();
            e.composite();
            rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        best
    };
    let full = time(engine);
    println!("  {} layers at {}²: full composite {full:.1} ms", engine.layers.len(), engine.res);
    // Editing one layer's settings: after the first composite records the
    // stack below it, later edits start at that layer.
    let visible: Vec<usize> = (0..engine.layers.len()).filter(|&i| engine.layers[i].visible && !engine.layers[i].is_folder()).collect();
    for (label, i) in [("top", *visible.last().unwrap()), ("middle", visible[visible.len() / 2]), ("bottom", visible[0])] {
        engine.mark_layer_dirty(i);
        engine.composite();
        let mut best = f64::MAX;
        for _ in 0..3 {
            engine.mark_layer_dirty(i);
            rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let t = std::time::Instant::now();
            engine.composite();
            rs.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("  editing the {label} layer ({}): {best:.1} ms per change", engine.layers[i].name);
    }
    let mut costs = Vec::new();
    for i in 0..engine.layers.len() {
        if !engine.layers[i].visible || engine.layers[i].is_folder() {
            continue;
        }
        engine.layers[i].visible = false;
        let without = time(engine);
        engine.layers[i].visible = true;
        let l = &engine.layers[i];
        let mask_kind = l.mask.as_ref().and_then(|m| m.generator.as_ref()).map_or("-".to_string(), |g| g.kind.name().to_string());
        let filters = l.mask.as_ref().map_or(0, |m| m.filters.iter().filter(|f| f.enabled).count());
        costs.push((full - without, l.name.clone(), mask_kind, filters));
    }
    costs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (ms, name, generator, filters) in costs.iter().take(20) {
        println!("    {ms:6.1} ms  {name:<34} mask: {generator}{}", if *filters > 0 { format!(" + {filters} filters") } else { String::new() });
    }
    let total: f64 = costs.iter().map(|c| c.0).sum();
    println!("  sum of layer shares {total:.1} ms");
}

/// A decal fill shows only inside its box on faces turned toward it, and a
/// text decal only where its letters are.
#[test]
#[ignore]
fn decal_projection_covers_its_box() {
    use crate::engine::{Content, FillParams, MatValues, Projection};
    let rs = render_state();
    // The plane: u along +x, v along +z, facing +y.
    let mesh = crate::mesh::Primitive::Plane.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    const RES: u32 = 256;
    let (lo, hi) = (mesh.bounds_min, mesh.bounds_max);
    let mut engine = Engine::new(&rs, &gpu_mesh, (lo, hi), RES);
    let mut undo = crate::undo::UndoStack::default();
    let black = FillParams { mat: MatValues { enabled: [true, false, false, false], color: [0.0; 3], ..Default::default() }, ..Default::default() };
    engine.add_fill_layer(0, None, "Base".into(), black, &mut undo);
    let mut fill = FillParams { mat: MatValues { enabled: [true, false, false, false], color: [1.0; 3], ..Default::default() }, ..Default::default() };
    fill.projection = Projection::Decal;
    fill.decal = crate::decal::Decal { center: [0.0, 0.0, 0.0], normal: [0.0, 1.0, 0.0], size: [0.5, 0.25], depth: 0.1, facing: 80.0, soft: 0.0, mirror: false };
    let idx = engine.add_fill_layer(1, None, "Decal".into(), fill, &mut undo);

    let shot = |e: &mut Engine| -> Vec<u8> {
        e.mark_dirty();
        e.composite();
        read_texture(&rs.device, &rs.queue, e.final_tex(0)).chunks_exact(4).map(|c| c[0]).collect()
    };
    // The texel at a point (x, z) of the plane.
    let at = |v: &[u8], x: f32, z: f32| {
        let u = ((x - lo.x) / (hi.x - lo.x) * RES as f32) as usize;
        let w = ((z - lo.z) / (hi.z - lo.z) * RES as f32) as usize;
        v[w.min(RES as usize - 1) * RES as usize + u.min(RES as usize - 1)]
    };
    let v = shot(&mut engine);
    println!("  box: center {} inside x {} inside z {} | outside x {} outside z {}", at(&v, 0.0, 0.0), at(&v, 0.2, 0.0), at(&v, 0.0, 0.1), at(&v, 0.35, 0.0), at(&v, 0.0, 0.2));
    assert!(at(&v, 0.0, 0.0) > 240 && at(&v, 0.2, 0.0) > 240 && at(&v, 0.0, 0.1) > 240, "inside the box");
    assert!(at(&v, 0.35, 0.0) < 10 && at(&v, 0.0, 0.2) < 10 && at(&v, -0.35, 0.0) < 10, "outside the box");

    // Turned away from the surface: nothing.
    if let Content::Fill(f) = &mut engine.layers[idx].content {
        f.decal.normal = [0.0, -1.0, 0.0];
    }
    let v = shot(&mut engine);
    assert!(at(&v, 0.0, 0.0) < 10, "a decal facing away leaves the surface alone");
    // Unless it wraps around (facing 180).
    if let Content::Fill(f) = &mut engine.layers[idx].content {
        f.decal.facing = 180.0;
    }
    let v = shot(&mut engine);
    assert!(at(&v, 0.0, 0.0) > 240, "facing 180 takes every face in the box");

    // Rotated 90 degrees: the box's long side runs along z.
    if let Content::Fill(f) = &mut engine.layers[idx].content {
        f.decal.normal = [0.0, 1.0, 0.0];
        f.decal.facing = 80.0;
        f.rotation = 90.0;
    }
    let v = shot(&mut engine);
    assert!(at(&v, 0.0, 0.2) > 240 && at(&v, 0.2, 0.0) < 10, "rotation turns the box");

    // Text: covered only where the letters are, never outside the box.
    if crate::decal::font_path("bahnschrift").or_else(|| crate::decal::font_path("arialbd")).is_some() {
        if let Content::Fill(f) = &mut engine.layers[idx].content {
            f.rotation = 0.0;
        }
        let spec = crate::decal::TextSpec { text: "HI".into(), ..Default::default() };
        let (img, aspect) = engine.text_fill_image(&spec).unwrap();
        engine.set_fill_image(idx, 0, Some(img), &mut undo);
        if let Content::Fill(f) = &mut engine.layers[idx].content {
            f.decal.size = [0.25 * aspect, 0.25];
        }
        let v = shot(&mut engine);
        let (mut inside, mut lit, mut outside_lit) = (0, 0, 0);
        let half = [0.125 * aspect, 0.125];
        for k in 0..RES as usize * RES as usize {
            let (u, w) = ((k % RES as usize) as f32 + 0.5, (k / RES as usize) as f32 + 0.5);
            let x = lo.x + u / RES as f32 * (hi.x - lo.x);
            let z = lo.z + w / RES as f32 * (hi.z - lo.z);
            let is_in = x.abs() < half[0] && z.abs() < half[1];
            if is_in {
                inside += 1;
                lit += (v[k] > 128) as u32;
            } else if v[k] > 128 {
                outside_lit += 1;
            }
        }
        let frac = lit as f32 / inside.max(1) as f32;
        println!("  text: {frac:.2} of the box lit, {outside_lit} texels outside");
        assert!(frac > 0.08 && frac < 0.7, "letters, not the whole box: {frac}");
        assert_eq!(outside_lit, 0);
    }
}

/// A draft composite matches the full one at the texels it samples.
#[test]
#[ignore]
fn draft_composite_matches_full() {
    use crate::engine::{FillParams, GenKind, MatValues};
    let rs = render_state();
    let mesh = crate::mesh::Primitive::Sphere.build();
    let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
    const RES: u32 = 256;
    let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), RES);
    let mut undo = crate::undo::UndoStack::default();
    let base = FillParams { mat: MatValues { color: [0.2, 0.3, 0.4], ..Default::default() }, ..Default::default() };
    engine.add_fill_layer(0, None, "Base".into(), base, &mut undo);
    let mut spots = FillParams { mat: MatValues { color: [0.9, 0.6, 0.1], ..Default::default() }, ..Default::default() };
    spots.color_var = 0.3;
    let i = engine.add_fill_layer(1, None, "Spots".into(), spots, &mut undo);
    engine.add_mask(i, true, Some(GenKind::Clouds.default_generator()), &mut undo);
    engine.layers[i].opacity = 0.7;
    let read = |e: &Engine| -> (u32, Vec<u8>) {
        let t = e.view_tex(0);
        (t.width, read_texture(&rs.device, &rs.queue, t).chunks_exact(4).map(|c| c[0]).collect())
    };
    engine.mark_dirty();
    engine.composite();
    let (fw, full) = read(&engine);
    engine.draft = true;
    engine.mark_dirty();
    engine.composite();
    let (dw, draft) = read(&engine);
    assert_eq!(dw * 2, fw, "the draft is half the size at 256");
    let (mut sum, mut n, mut worst) = (0.0f64, 0, 0u8);
    for y in 0..dw as usize {
        for x in 0..dw as usize {
            let a = draft[y * dw as usize + x];
            let b = full[(2 * y) * fw as usize + 2 * x];
            let d = a.abs_diff(b);
            sum += d as f64;
            n += 1;
            worst = worst.max(d);
        }
    }
    let mean = sum / n as f64;
    println!("  draft vs full: mean {mean:.2}, worst {worst} (of 255)");
    assert!(mean < 3.0, "draft differs from full: mean {mean}");
    // Back to full size once the draft ends.
    engine.draft = false;
    engine.mark_dirty();
    engine.composite();
    assert_eq!(read(&engine).0, fw);
}
