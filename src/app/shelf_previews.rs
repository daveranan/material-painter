//! Shelf thumbnails rendered by the real engine on a baked material ball, so
//! a smart material's swatch shows how its masks read edges, cavities, creases
//! and occlusion. Until the ball is baked the shelf shows the CPU spheres.

use super::{Shelf, add_smart_material};
use crate::bake::{self, BakeRun, BakeSettings, GpuBaker, PreparedScene, Quality};
use crate::engine::{Engine, FillParams, GenKind, MatValues};
use crate::environment::{self, EnvBaker, EnvSource, Environment};
use crate::gpu::GpuMesh;
use crate::mesh::Primitive;
use crate::presets;
use crate::undo::UndoStack;
use crate::viewport::{Background, OrbitCam, ViewRenderer, ViewSettings};
use eframe::{egui, egui_wgpu};
use std::sync::mpsc;

/// Texture resolution of the ball; thumbnails are small.
const RES: u32 = 512;
/// Rendered thumbnail size in pixels: sharp up to the largest shelf size,
/// and for the 2x hover preview at the default size.
const THUMB: u32 = 192;

#[derive(Clone, Copy)]
pub(super) enum Item {
    Smart(usize),
    Material(usize),
    Generator(usize),
}

impl Item {
    fn all() -> Vec<Item> {
        let mut items: Vec<Item> = (0..presets::SMART_MATERIALS.len()).map(Item::Smart).collect();
        items.extend((0..presets::MATERIALS.len()).map(Item::Material));
        items.extend((0..GenKind::ALL.len()).map(Item::Generator));
        items
    }

    /// Where the rendered thumbnail is kept between runs. The folder carries
    /// the app version, so a new version (new presets) renders them again.
    fn cache_path(self) -> Option<std::path::PathBuf> {
        let (kind, i, name) = match self {
            Item::Smart(i) => ("smart", i, presets::SMART_MATERIALS[i].name),
            Item::Material(i) => ("material", i, presets::MATERIALS[i].name),
            Item::Generator(i) => ("generator", i, GenKind::ALL[i].name()),
        };
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        let dir = super::project_io::data_dir()?.join("shelf_previews").join(env!("CARGO_PKG_VERSION"));
        Some(dir.join(format!("{kind}-{i}-{name}.png")))
    }

    fn handle(self, shelf: &mut Shelf) -> &mut egui::TextureHandle {
        match self {
            Item::Smart(i) => &mut shelf.smart[i],
            Item::Material(i) => &mut shelf.materials[i],
            Item::Generator(i) => &mut shelf.generators[i],
        }
    }
}

/// Puts the thumbnails rendered on an earlier run in place. Returns the
/// items still missing.
pub(super) fn load_cached(shelf: &mut Shelf) -> Vec<Item> {
    Item::all()
        .into_iter()
        .filter(|item| {
            let img = item.cache_path().and_then(|p| image::open(p).ok()).map(|i| i.into_rgba8());
            match img {
                Some(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    item.handle(shelf).set(color, egui::TextureOptions::LINEAR);
                    false
                }
                None => true,
            }
        })
        .collect()
}

fn save_cached(item: Item, img: &egui::ColorImage) {
    let Some(path) = item.cache_path() else { return };
    let _ = std::fs::create_dir_all(path.parent().expect("a file in a folder"));
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    if let Some(buf) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, bytes) {
        let _ = buf.save(path);
    }
}

enum Stage {
    Preparing(mpsc::Receiver<Result<PreparedScene, String>>),
    Baking(BakeRun),
    /// Rendering one thumbnail per frame, so the UI never stalls.
    Rendering(Vec<Item>),
}

pub(super) struct ShelfPreviews {
    mesh: GpuMesh,
    bounds: (glam::Vec3, glam::Vec3),
    baker: GpuBaker,
    engine: Option<Engine>,
    views: ViewRenderer,
    /// Fixed studio lighting, so thumbnails don't change with the viewport HDRI.
    env: Environment,
    stage: Stage,
    /// The thumbnails no earlier run left behind.
    missing: Vec<Item>,
}

impl ShelfPreviews {
    /// Starts rendering the thumbnails in `missing` (see `load_cached`).
    pub fn start(rs: &egui_wgpu::RenderState, ctx: &egui::Context, env_baker: &EnvBaker, missing: Vec<Item>) -> ShelfPreviews {
        let mesh = Primitive::MaterialBall.build();
        let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
        let bounds = (mesh.bounds_min, mesh.bounds_max);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let settings = BakeSettings { quality: Quality::Draft, ..Default::default() };
            let _ = tx.send(bake::prepare(&mesh, &settings, 0));
            ctx.request_repaint();
        });
        ShelfPreviews {
            mesh: gpu_mesh,
            bounds,
            baker: GpuBaker::new(&rs.device),
            engine: None,
            views: ViewRenderer::new(rs),
            env: env_baker.upload(rs, &environment::decode(&EnvSource::Builtin(0)).expect("the procedural studio always decodes")),
            stage: Stage::Preparing(rx),
            missing,
        }
    }

    /// Advances the bake or renders the next thumbnail. Returns false once
    /// every thumbnail is in place (or the bake failed) and this can be dropped.
    pub fn pump(&mut self, rs: &egui_wgpu::RenderState, ctx: &egui::Context, shelf: &mut Shelf) -> bool {
        match &mut self.stage {
            Stage::Preparing(rx) => match rx.try_recv() {
                Ok(Ok(scene)) => {
                    let settings = BakeSettings { quality: Quality::Draft, ..Default::default() };
                    let run = self.baker.start(&rs.device, &rs.queue, &self.mesh, &scene, &settings, RES);
                    self.stage = Stage::Baking(run);
                    ctx.request_repaint();
                    true
                }
                Ok(Err(e)) => {
                    eprintln!("shelf preview bake failed: {e}");
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
                Err(mpsc::TryRecvError::Disconnected) => false,
            },
            Stage::Baking(run) => {
                if let Some(maps) = self.baker.pump(run, &rs.device, &rs.queue, ctx) {
                    let report = run.report.clone();
                    let mut engine = Engine::new(rs, &self.mesh, self.bounds, RES);
                    engine.set_maps(maps, report);
                    self.engine = Some(engine);
                    let mut items = std::mem::take(&mut self.missing);
                    items.reverse();
                    self.stage = Stage::Rendering(items);
                    ctx.request_repaint();
                }
                true
            }
            Stage::Rendering(items) => {
                let Some(item) = items.pop() else { return false };
                let engine = self.engine.as_mut().expect("engine exists while rendering");
                let image = render(rs, engine, &self.env, &mut self.views, &self.mesh, item);
                save_cached(item, &image);
                item.handle(shelf).set(image, egui::TextureOptions::LINEAR);
                ctx.request_repaint();
                !items.is_empty()
            }
        }
    }
}

fn render(
    rs: &egui_wgpu::RenderState,
    engine: &mut Engine,
    env: &Environment,
    views: &mut ViewRenderer,
    mesh: &GpuMesh,
    item: Item,
) -> egui::ColorImage {
    let mut undo = UndoStack::default();
    while !engine.layers.is_empty() {
        engine.delete_layer(0, &mut undo);
    }
    match item {
        Item::Smart(i) => {
            add_smart_material(engine, &mut undo, &presets::SMART_MATERIALS[i], 0, None);
        }
        Item::Material(i) => {
            let m = &presets::MATERIALS[i];
            engine.add_fill_layer(0, None, m.name.to_string(), m.fill(), &mut undo);
        }
        Item::Generator(i) => {
            // The mask in white over black, like the generator's own preview.
            let flat = |c: f32| FillParams {
                mat: MatValues { enabled: [true, true, true, false], color: [c; 3], rough: 0.6, metal: 0.0, height: 0.0 },
                ..Default::default()
            };
            engine.add_fill_layer(0, None, "Black".into(), flat(0.06), &mut undo);
            let top = engine.add_fill_layer(1, None, "Mask".into(), flat(0.92), &mut undo);
            engine.add_mask(top, true, Some(GenKind::ALL[i].default_generator()), &mut undo);
        }
    }
    engine.composite();
    let cam = OrbitCam { yaw: 0.5, pitch: 0.32, dist: 4.05, target: glam::Vec3::new(0.0, -0.2, 0.0), ..OrbitCam::default() };
    let mut settings = ViewSettings { show_wire: false, background: Background::Transparent, ..ViewSettings::default() };
    settings.post.vignette = 0.0;
    settings.post.bloom = 0.0;
    views.render_3d(rs, [THUMB, THUMB], &[engine], env, mesh, &cam, &settings, None);
    let img = views.view3d.image();
    let rgba = crate::gpu::read_texture(&rs.device, &rs.queue, img);
    // MSAA resolve averages edge samples with the transparent clear, so the
    // silhouette comes out premultiplied.
    egui::ColorImage::from_rgba_premultiplied([img.width as usize, img.height as usize], &rgba)
}
