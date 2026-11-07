//! Terrain blend preview: up to four feature materials on a heightmap,
//! placed by the game's splat pattern and blended by the game's height rule
//! with anti-tiling (see `shaders/terrain.wgsl`). It renders only while its
//! pane is shown, into its own small target, so the painter's views and
//! render times are untouched.
//!
//! Materials come from game exports (`<name>.material.json` beside its
//! `ColorHeight` and `NormalMasks` PNGs) or from the open document, built in
//! memory with the same "Game material (features)" template.

use crate::gpu::{self, Tex};
use eframe::{egui, egui_wgpu, wgpu};
use glam::Vec3;
use image::{RgbaImage, imageops::FilterType};
use std::path::{Path, PathBuf};

pub const SLOTS: usize = 4;
/// Material texture size in the preview (each slot, with a full mip chain).
pub const MAT_SIZE: u32 = 1024;
const MAT_MIPS: u32 = 11;
/// Heightmap texels per side once loaded.
const HEIGHT_SIZE: u32 = 1024;
const MSAA: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The built-in sample heightmap.
const SAMPLE_HEIGHT: &[u8] = include_bytes!("../assets/terrain/sample_height.png");

/// What the splat pattern puts in each slot (the game's temperate ground).
pub const SLOT_ROLES: [&str; SLOTS] = ["Base (soil)", "Drifts", "Patches", "Slopes and outcrops"];

#[derive(Clone, Debug, PartialEq)]
pub enum SlotSource {
    Empty,
    /// The open document, built with the game template.
    Live,
    /// A game export's `material.json`.
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct SlotInfo {
    pub name: String,
    pub tile_m: f32,
    pub relief_m: f32,
}

#[derive(Clone, Debug)]
pub struct Ground {
    pub name: String,
    pub footprint_m: f32,
    pub amplitude_m: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub blend_depth: f32,
    pub anti_tiling: bool,
    pub show_weights: bool,
    /// Multiplies the splat pattern's sizes (1 = the game's).
    pub splat_scale: f32,
    pub sun_azimuth: f32,
    pub sun_elevation: f32,
    pub exposure: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { blend_depth: 0.12, anti_tiling: true, show_weights: false, splat_scale: 1.0, sun_azimuth: 35.0, sun_elevation: 38.0, exposure: 1.0 }
    }
}

/// An orbit camera over the terrain.
#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

impl Cam {
    pub fn eye(&self) -> Vec3 {
        let d = Vec3::new(self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), self.yaw.cos() * self.pitch.cos());
        self.target + d * self.dist
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    sun: [f32; 4],
    ground: [f32; 4],
    blend: [f32; 4],
    tiles: [f32; 4],
    present: [f32; 4],
    info: [f32; 4],
}

struct Targets {
    msaa: Tex,
    depth: Tex,
    resolve: Tex,
    size: [u32; 2],
}

pub struct TerrainPreview {
    pipeline: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    height: Tex,
    color: wgpu::Texture,
    data: wgpu::Texture,
    bind: wgpu::BindGroup,
    targets: Targets,
    pub egui_id: egui::TextureId,
    pub sources: [SlotSource; SLOTS],
    pub slots: [Option<SlotInfo>; SLOTS],
    pub ground: Ground,
    pub settings: Settings,
    pub cam: Cam,
    /// Grid cells per side.
    pub grid: u32,
    /// The heightmap on the CPU (0..1), to keep the camera on the ground.
    heights: Vec<f32>,
}

fn array_texture(device: &wgpu::Device, label: &str, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: MAT_SIZE, height: MAT_SIZE, depth_or_array_layers: SLOTS as u32 },
        mip_level_count: MAT_MIPS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn array_view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() })
}

fn make_targets(device: &wgpu::Device, size: [u32; 2]) -> Targets {
    let [w, h] = [size[0].clamp(16, 8192), size[1].clamp(16, 8192)];
    let usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
    Targets {
        msaa: Tex::new_ms(device, "terrain msaa", w, h, COLOR_FORMAT, usage, MSAA),
        depth: Tex::new_ms(device, "terrain depth", w, h, DEPTH_FORMAT, usage, MSAA),
        resolve: Tex::new(device, "terrain view", w, h, COLOR_FORMAT, usage | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC),
        size: [w, h],
    }
}

/// A greyscale heightmap as R16Float texels, resized to `HEIGHT_SIZE`.
fn height_texels(img: &image::DynamicImage) -> Vec<u16> {
    let grey = img.to_luma32f();
    let grey = image::imageops::resize(&grey, HEIGHT_SIZE, HEIGHT_SIZE, FilterType::Triangle);
    grey.pixels().map(|p| gpu::f32_to_f16(p[0])).collect()
}

/// A full mip chain of an RGBA image (straight alpha, each channel filtered on its own).
fn mip_chain(img: &RgbaImage) -> Vec<RgbaImage> {
    let mut out = vec![image::imageops::resize(img, MAT_SIZE, MAT_SIZE, FilterType::Triangle)];
    for level in 1..MAT_MIPS {
        let s = (MAT_SIZE >> level).max(1);
        let prev = out.last().expect("level 0 first");
        out.push(image::imageops::resize(prev, s, s, FilterType::Triangle));
    }
    out
}

/// A game export: its two maps and what it is.
pub struct LoadedMaterial {
    pub color_height: RgbaImage,
    pub normal_masks: RgbaImage,
    pub info: SlotInfo,
}

/// Reads `<name>.material.json` and the two maps it lists.
pub fn load_material(manifest: &Path) -> Result<LoadedMaterial, String> {
    let text = std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let m: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let dir = manifest.parent().unwrap_or(Path::new("."));
    let maps = m["maps"].as_array().ok_or("material.json: no 'maps'")?;
    let find = |first: &str| {
        maps.iter()
            .find(|mv| mv["channels"].as_array().and_then(|c| c.first()).and_then(|c| c.as_str()).is_some_and(|c| c.starts_with(first)))
            .and_then(|mv| mv["file"].as_str())
            .map(|f| dir.join(f))
            .ok_or_else(|| format!("material.json: no map starting with {first}"))
    };
    let load = |p: PathBuf| image::open(&p).map(|i| i.to_rgba8()).map_err(|e| format!("{}: {e}", p.display()));
    Ok(LoadedMaterial {
        color_height: load(find("base_color")?)?,
        normal_masks: load(find("normal")?)?,
        info: SlotInfo {
            name: m["name"].as_str().unwrap_or("material").to_string(),
            tile_m: m["tile_m"].as_f64().unwrap_or(2.0) as f32,
            relief_m: m["relief_m"].as_f64().unwrap_or(0.1) as f32,
        },
    })
}

/// A heightmap stamp from a stamp library (a game's `Library.json`).
#[derive(Clone, Debug)]
pub struct Stamp {
    pub name: String,
    pub category: String,
    pub path: PathBuf,
    pub footprint_m: f32,
    pub amplitude_m: f32,
}

/// The stamps of a landscape stamp folder: `Library.json` names each
/// stamp's heightmap by asset id, and each PNG's `.meta` holds its id.
pub fn read_stamps(folder: &Path) -> Vec<Stamp> {
    let Ok(text) = std::fs::read_to_string(folder.join("Library.json")) else { return Vec::new() };
    let Ok(lib) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let mut by_guid = std::collections::HashMap::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "meta") {
                let png = p.with_extension("");
                if png.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) {
                    let guid = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["guid"].as_str().map(str::to_string));
                    if let Some(g) = guid {
                        by_guid.insert(g, png);
                    }
                }
            }
        }
    }
    let mut out: Vec<Stamp> = lib["data"]["Sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let path = by_guid.get(s["Heightmap"].as_str()?)?.clone();
            Some(Stamp {
                name: s["Name"].as_str().unwrap_or("stamp").to_string(),
                category: s["Category"].as_str().unwrap_or("").trim_start_matches("Stamps: ").to_string(),
                path,
                footprint_m: s["Footprint"].as_f64().unwrap_or(100.0) as f32,
                amplitude_m: s["Amplitude"].as_f64().unwrap_or(20.0) as f32,
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    out
}

impl TerrainPreview {
    pub fn new(rs: &egui_wgpu::RenderState) -> TerrainPreview {
        let device = &rs.device;
        let module = gpu::shader(device, "terrain", include_str!("shaders/terrain.wgsl"));
        let tex2d = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension, multisampled: false },
            count: None,
        };
        let samp = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex2d(1, wgpu::TextureViewDimension::D2),
                tex2d(2, wgpu::TextureViewDimension::D2Array),
                tex2d(3, wgpu::TextureViewDimension::D2Array),
                samp(4),
                samp(5),
            ],
        });
        let layout = gpu::layout(device, &bgl);
        let ms = wgpu::MultisampleState { count: MSAA, ..Default::default() };
        let depth = |write: bool, compare| Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: Default::default(),
            bias: Default::default(),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs"), targets: &[gpu::target(COLOR_FORMAT)], compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: depth(true, wgpu::CompareFunction::Less),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        let sky = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain sky"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_sky"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_sky"), targets: &[gpu::target(COLOR_FORMAT)], compilation_options: Default::default() }),
            primitive: Default::default(),
            depth_stencil: depth(false, wgpu::CompareFunction::Always),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let clamp = gpu::linear_sampler(device, wgpu::AddressMode::ClampToEdge);
        let repeat = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terrain repeat"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let height = Tex::new(device, "terrain height", HEIGHT_SIZE, HEIGHT_SIZE, wgpu::TextureFormat::R16Float, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        let color = array_texture(device, "terrain colour", wgpu::TextureFormat::Rgba8UnormSrgb);
        let data = array_texture(device, "terrain data", wgpu::TextureFormat::Rgba8Unorm);
        let bind = Self::bind_group(device, &bgl, &uniforms, &height, &color, &data, &clamp, &repeat);
        let targets = make_targets(device, [64, 64]);
        let egui_id = rs.renderer.write().register_native_texture(device, &targets.resolve.view, wgpu::FilterMode::Linear);
        let mut t = TerrainPreview {
            pipeline,
            sky,
            uniforms,
            height,
            color,
            data,
            bind,
            targets,
            egui_id,
            sources: [SlotSource::Empty, SlotSource::Empty, SlotSource::Empty, SlotSource::Empty],
            slots: [None, None, None, None],
            ground: Ground { name: String::new(), footprint_m: 0.0, amplitude_m: 0.0 },
            settings: Settings::default(),
            cam: Cam { target: Vec3::ZERO, yaw: 0.7, pitch: 0.45, dist: 40.0 },
            grid: 512,
            heights: Vec::new(),
        };
        let sample = image::load_from_memory(SAMPLE_HEIGHT).expect("bundled heightmap decodes");
        t.set_height(&rs.queue, &sample, Ground { name: "Sample".into(), footprint_m: 160.0, amplitude_m: 9.0 });
        t
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_group(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        uniforms: &wgpu::Buffer,
        height: &Tex,
        color: &wgpu::Texture,
        data: &wgpu::Texture,
        clamp: &wgpu::Sampler,
        repeat: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&height.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&array_view(color)) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&array_view(data)) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(clamp) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(repeat) },
            ],
        })
    }

    /// Replaces the ground: a greyscale heightmap covering `footprint_m`,
    /// white = `amplitude_m` above black. The camera re-frames it.
    pub fn set_height(&mut self, queue: &wgpu::Queue, img: &image::DynamicImage, ground: Ground) {
        let texels = height_texels(img);
        queue.write_texture(
            self.height.copy_info(),
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(HEIGHT_SIZE * 2), rows_per_image: None },
            self.height.extent(),
        );
        self.heights = texels.iter().map(|&h| gpu::f16_to_f32(h)).collect();
        self.ground = ground;
        // Frame the middle of the ground at a walking-ish distance.
        self.cam.target = Vec3::new(0.0, self.ground_y(0.0, 0.0), 0.0);
        self.cam.dist = (self.ground.footprint_m * 0.12).clamp(8.0, 60.0);
    }

    /// The ground's height (m) at world x, z (nearest texel).
    pub fn ground_y(&self, x: f32, z: f32) -> f32 {
        let n = HEIGHT_SIZE as f32;
        let f = |v: f32| ((v / self.ground.footprint_m.max(1e-3) + 0.5) * n).clamp(0.0, n - 1.0) as usize;
        self.heights.get(f(z) * HEIGHT_SIZE as usize + f(x)).map_or(0.0, |h| h * self.ground.amplitude_m)
    }

    /// Puts a material in slot `k`.
    pub fn set_slot(&mut self, queue: &wgpu::Queue, k: usize, m: &LoadedMaterial) {
        for (tex, img) in [(&self.color, &m.color_height), (&self.data, &m.normal_masks)] {
            for (level, mip) in mip_chain(img).iter().enumerate() {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: level as u32,
                        origin: wgpu::Origin3d { x: 0, y: 0, z: k as u32 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    mip.as_raw(),
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(mip.width() * 4), rows_per_image: None },
                    wgpu::Extent3d { width: mip.width(), height: mip.height(), depth_or_array_layers: 1 },
                );
            }
        }
        self.slots[k] = Some(m.info.clone());
    }

    pub fn clear_slot(&mut self, k: usize) {
        self.slots[k] = None;
        self.sources[k] = SlotSource::Empty;
    }

    /// Renders at `size` (pixels) and returns the egui texture showing it.
    pub fn render(&mut self, rs: &egui_wgpu::RenderState, size: [u32; 2]) -> egui::TextureId {
        let device = &rs.device;
        if self.targets.size != [size[0].clamp(16, 8192), size[1].clamp(16, 8192)] {
            self.targets = make_targets(device, size);
            rs.renderer.write().update_egui_texture_from_wgpu_texture(device, &self.targets.resolve.view, wgpu::FilterMode::Linear, self.egui_id);
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode(&rs.queue, &mut encoder);
        rs.queue.submit([encoder.finish()]);
        self.egui_id
    }

    /// Renders offscreen at `size` and reads the image back (RGBA8, sRGB).
    pub fn render_image(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 2]) -> (Vec<u8>, u32, u32) {
        let shown = std::mem::replace(&mut self.targets, make_targets(device, size));
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode(queue, &mut encoder);
        queue.submit([encoder.finish()]);
        let px = gpu::read_texture(device, queue, &self.targets.resolve);
        let (w, h) = (self.targets.size[0], self.targets.size[1]);
        self.targets = shown;
        (px, w, h)
    }

    fn encode(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder) {
        let [w, h] = self.targets.size;
        let s = &self.settings;
        let eye = self.cam.eye();
        let proj = crate::viewport::perspective_rh(50f32.to_radians(), w as f32 / h as f32, 0.05, 2000.0);
        let view = crate::viewport::look_at_rh(eye, self.cam.target, Vec3::Y);
        let (az, el) = (s.sun_azimuth.to_radians(), s.sun_elevation.to_radians());
        let sun = Vec3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos());
        let tile = |k: usize| self.slots[k].as_ref().map_or(2.0, |i| i.tile_m.max(0.05));
        let u = Uniforms {
            view_proj: (proj * view).to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 1.0],
            sun: [sun.x, sun.y, sun.z, 1.7],
            ground: [self.ground.footprint_m, self.ground.amplitude_m, self.grid as f32, s.splat_scale.max(0.05)],
            blend: [s.blend_depth.max(0.001), f32::from(u8::from(s.anti_tiling)), f32::from(u8::from(s.show_weights)), s.exposure],
            tiles: [tile(0), tile(1), tile(2), tile(3)],
            present: std::array::from_fn(|k| if self.slots[k].is_some() { 1.0 } else { 0.0 }),
            info: [HEIGHT_SIZE as f32, 0.0, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("terrain"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets.msaa.view,
                resolve_target: Some(&self.targets.resolve.view),
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Discard },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.targets.depth.view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_pipeline(&self.sky);
        pass.draw(0..3, 0..1);
        if self.slots.iter().any(Option::is_some) {
            pass.set_pipeline(&self.pipeline);
            pass.draw(0..self.grid * self.grid * 6, 0..1);
        }
    }

    /// Orbit (drag), pan (right or middle drag), zoom (scroll) from a pane's response.
    pub fn handle_input(&mut self, ui: &egui::Ui, resp: &egui::Response) {
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.cam.yaw -= d.x * 0.006;
            self.cam.pitch = (self.cam.pitch + d.y * 0.006).clamp(0.03, 1.5);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            let right = Vec3::new(self.cam.yaw.cos(), 0.0, -self.cam.yaw.sin());
            let fwd = Vec3::new(-self.cam.yaw.sin(), 0.0, -self.cam.yaw.cos());
            let k = self.cam.dist * 0.0025;
            self.cam.target += (-right * d.x + fwd * d.y) * k;
            self.cam.target.y = self.ground_y(self.cam.target.x, self.cam.target.z);
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.dist = (self.cam.dist * (1.0 - scroll * 0.0015)).clamp(0.5, 600.0);
            }
        }
    }

    /// Camera presets: a player's eye on the ground, or an overview.
    pub fn camera_preset(&mut self, name: &str) {
        match name {
            "player" => {
                self.cam.pitch = 0.32;
                self.cam.dist = 5.0;
            }
            "close" => {
                self.cam.pitch = 0.9;
                self.cam.dist = 2.5;
            }
            _ => {
                self.cam.pitch = 0.5;
                self.cam.dist = (self.ground.footprint_m * 0.35).clamp(10.0, 300.0);
            }
        }
    }
}

/// A heightmap file as the ground.
pub fn load_height(path: &Path) -> Result<image::DynamicImage, String> {
    image::open(path).map_err(|e| format!("{}: {e}", path.display()))
}
