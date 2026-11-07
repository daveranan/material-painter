//! Renders a graph on the GPU: one pipeline per node definition, one
//! texture per node. A node only re-renders when its definition, its
//! parameters or anything upstream changed (tracked by a hash).

use super::def::{self, MAX_IMAGES, MAX_INPUTS, NodeDef, PREV_BINDING, ParamKind, Passes, Value};
use super::{Graph, Library};
use crate::gpu::{self, LAYER_USAGE, Tex};
use eframe::wgpu;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

/// Every node renders in this format: colors and grays (in all of rgb)
/// alike, so any output can feed any input.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NodeUniform {
    p: [[f32; 4]; def::MAX_PARAMS],
    defaults: [[f32; 4]; MAX_INPUTS],
    info: [f32; 4],
}

struct Compiled {
    /// Hash of the source it was compiled from.
    source: u64,
    pipeline: Result<wgpu::RenderPipeline, String>,
}

struct Rendered {
    hash: u64,
    tex: Arc<Tex>,
    /// 8-bit copy for the editor (egui shows Rgba8Unorm textures).
    thumb: Arc<Tex>,
}

/// Thumbnail edge in texels.
pub const THUMB: u32 = 256;

const BLIT: &str = "
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@fragment
fn fs_blit(in: FsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(clamp(textureSampleLevel(src, smp, in.uv, 0.0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
// One mip level from the one above: bilinear at the center of each 2x2 block.
@fragment
fn fs_down(in: FsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, in.uv, 0.0);
}
// Stats of a grey image down a mip chain: min in r, max in g, average in b
// (level 0 holds the grey in all three).
@fragment
fn fs_down_stats(in: FsOut) -> @location(0) vec4<f32> {
    let n = vec2<i32>(textureDimensions(src));
    let o = vec2<i32>(in.pos.xy) * 2;
    var lo = 1e30;
    var hi = -1e30;
    var sum = 0.0;
    for (var k = 0; k < 4; k++) {
        let t = textureLoad(src, min(o + vec2<i32>(k % 2, k / 2), n - 1), 0);
        lo = min(lo, t.r);
        hi = max(hi, t.g);
        sum += t.b;
    }
    return vec4<f32>(lo, hi, sum * 0.25, 1.0);
}
";

pub struct Runtime {
    blit: wgpu::RenderPipeline,
    /// Downsamples one level of a mip chain into the next (`@mean` inputs).
    down: wgpu::RenderPipeline,
    /// The same, keeping min, max and average (`@minmax` inputs).
    down_stats: wgpu::RenderPipeline,
    blit_bgl: wgpu::BindGroupLayout,
    bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    dummy: Tex,
    pipelines: HashMap<String, Compiled>,
    rendered: HashMap<u64, Rendered>,
    images: HashMap<PathBuf, Result<Arc<Tex>, String>>,
    /// Node id → why it didn't render.
    pub errors: HashMap<u64, String>,
}

fn hash_of(v: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Runs `f` and returns the first validation error it caused, as text.
pub(crate) fn validation_scope<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> (T, Option<String>) {
    use std::future::Future;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = f();
    let mut pop = std::pin::pin!(scope.pop());
    let err = match pop.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(Some(e)) => Some(e.to_string()),
        _ => None,
    };
    (value, err)
}

/// The mesh maps a graph can read: a Mesh Map value's texture and channel
/// (see `Engine::mesh_map`), `None` when it isn't baked.
pub type MeshMaps<'a> = &'a dyn Fn(usize) -> Option<(&'a Tex, usize)>;

impl Runtime {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Runtime {
        let mut entries = vec![gpu::uniform_entry(0, false), gpu::sampler_entry(1)];
        entries.extend((0..(MAX_INPUTS + MAX_IMAGES) as u32).map(|i| gpu::tex_entry(2 + i, true)));
        entries.push(gpu::tex_entry(PREV_BINDING as u32, true));
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("graph node"), entries: &entries });
        let layout = gpu::layout(device, &bgl);
        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("graph thumb"),
            entries: &[gpu::tex_entry(0, true), gpu::sampler_entry(1)],
        });
        let blit_module = gpu::shader(device, "graph thumb", BLIT);
        let blit = gpu::fullscreen_pipeline(
            device,
            "graph thumb",
            &blit_module,
            "fs_blit",
            &gpu::layout(device, &blit_bgl),
            &[gpu::target(wgpu::TextureFormat::Rgba8Unorm)],
        );
        let down = gpu::fullscreen_pipeline(device, "graph mip", &blit_module, "fs_down", &gpu::layout(device, &blit_bgl), &[gpu::target(FORMAT)]);
        let down_stats =
            gpu::fullscreen_pipeline(device, "graph stats", &blit_module, "fs_down_stats", &gpu::layout(device, &blit_bgl), &[gpu::target(FORMAT)]);
        let dummy = Tex::new(device, "graph dummy", 1, 1, FORMAT, LAYER_USAGE);
        queue.write_texture(dummy.copy_info(), bytemuck::cast_slice(&[0u16; 4]), Default::default(), dummy.extent());
        Runtime {
            blit,
            down,
            down_stats,
            blit_bgl,
            bgl,
            layout,
            sampler: gpu::linear_sampler(device, wgpu::AddressMode::Repeat),
            dummy,
            pipelines: HashMap::new(),
            rendered: HashMap::new(),
            images: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    /// The last render of a node.
    pub fn output(&self, node: u64) -> Option<&Arc<Tex>> {
        self.rendered.get(&node).map(|r| &r.tex)
    }

    /// A node's 8-bit thumbnail.
    pub fn thumb(&self, node: u64) -> Option<&Arc<Tex>> {
        self.rendered.get(&node).map(|r| &r.thumb)
    }

    /// Checks a definition compiles, without rendering. `Err` holds the
    /// compiler's message.
    #[cfg(test)]
    pub fn compile(&mut self, device: &wgpu::Device, def: &NodeDef) -> Result<(), String> {
        self.pipeline(device, def).map(|_| ())
    }

    fn pipeline(&mut self, device: &wgpu::Device, def: &NodeDef) -> Result<&wgpu::RenderPipeline, String> {
        let source = hash_of(&def.source);
        let fresh = self.pipelines.get(&def.id).is_some_and(|c| c.source == source);
        if !fresh {
            let (pipeline, err) = validation_scope(device, || {
                let module = gpu::shader(device, &def.id, &def.shader());
                gpu::fullscreen_pipeline(device, &def.id, &module, "fs_node", &self.layout, &[gpu::target(FORMAT)])
            });
            let pipeline = match err {
                Some(e) => Err(e),
                None => Ok(pipeline),
            };
            self.pipelines.insert(def.id.clone(), Compiled { source, pipeline });
        }
        self.pipelines[&def.id].pipeline.as_ref().map_err(Clone::clone)
    }

    fn image(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, path: &PathBuf) -> Result<Arc<Tex>, String> {
        self.images
            .entry(path.clone())
            .or_insert_with(|| {
                let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.into_rgba8();
                let tex = Tex::new(device, "graph image", img.width(), img.height(), wgpu::TextureFormat::Rgba8Unorm, LAYER_USAGE);
                queue.write_texture(
                    tex.copy_info(),
                    &img,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width() * 4), rows_per_image: Some(img.height()) },
                    tex.extent(),
                );
                Ok(Arc::new(tex))
            })
            .clone()
    }

    /// Renders every node whose inputs changed. `maps_key` changes whenever
    /// the mesh maps do. Returns whether anything re-rendered.
    pub fn evaluate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        graph: &Graph,
        lib: &Library,
        maps: MeshMaps,
        maps_key: u64,
    ) -> bool {
        let res = graph.res.clamp(16, 4096);
        self.rendered.retain(|id, _| graph.node(*id).is_some());
        self.errors.retain(|id, _| graph.node(*id).is_some());
        let mut changed = false;
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut hashes: HashMap<u64, u64> = HashMap::new();
        for id in graph.order() {
            let node = graph.node(id).expect("ordered from the graph");
            let Some(def) = lib.find(&node.def) else {
                self.errors.insert(id, format!("unknown node '{}'", node.def));
                self.rendered.remove(&id);
                continue;
            };
            let values: Vec<Value> = def.params.iter().map(|p| lib.value(node, &p.name).unwrap_or_else(|| p.default.clone())).collect();
            let inputs: Vec<Option<u64>> =
                def.inputs.iter().map(|i| node.links.get(&i.name).copied().filter(|from| hashes.contains_key(from))).collect();
            let uses_maps = def.params.iter().any(|p| p.kind == ParamKind::MeshMap);
            let hash = hash_of((
                &def.source,
                format!("{values:?}"),
                res,
                inputs.iter().map(|i| i.map(|f| hashes[&f])).collect::<Vec<_>>(),
                if uses_maps { maps_key } else { 0 },
            ));
            if self.rendered.get(&id).is_some_and(|r| r.hash == hash) && !self.errors.contains_key(&id) {
                hashes.insert(id, hash);
                continue;
            }
            match self.render(device, queue, &mut encoder, def, &values, &inputs, res, maps) {
                Ok(tex) => {
                    let thumb = Arc::new(Tex::new(device, "graph thumb", THUMB, THUMB, wgpu::TextureFormat::Rgba8Unorm, LAYER_USAGE));
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("graph thumb"),
                        layout: &self.blit_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&tex.view) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        ],
                    });
                    {
                        let mut pass =
                            gpu::pass(&mut encoder, "graph thumb", &[gpu::color_attachment(&thumb.view, wgpu::LoadOp::Clear(wgpu::Color::BLACK))]);
                        pass.set_pipeline(&self.blit);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.draw(0..3, 0..1);
                    }
                    self.errors.remove(&id);
                    self.rendered.insert(id, Rendered { hash, tex, thumb });
                    hashes.insert(id, hash);
                    changed = true;
                }
                Err(e) => {
                    self.errors.insert(id, e);
                    self.rendered.remove(&id);
                    changed = true;
                }
            }
        }
        queue.submit([encoder.finish()]);
        changed
    }

    #[allow(clippy::too_many_arguments)]
    /// A copy of `src` with a full mip chain, down to 1x1: its last level is
    /// the whole image's average (`mean_<input>()` in `@mean` nodes).
    fn mipped(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, src: &Tex, stats: bool) -> Tex {
        let mips = 32 - src.width.max(src.height).leading_zeros();
        let t = Tex::new_mipped(device, "graph mean", src.width, src.height, FORMAT, LAYER_USAGE, mips);
        encoder.copy_texture_to_texture(src.copy_info(), t.copy_info(), src.extent());
        for level in 1..mips {
            let from = t.mip_view(level - 1, false);
            let to = t.mip_view(level, false);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("graph mip"),
                layout: &self.blit_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&from) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            let mut pass = gpu::pass(encoder, "graph mip", &[gpu::color_attachment(&to, wgpu::LoadOp::Clear(wgpu::Color::BLACK))]);
            pass.set_pipeline(if stats { &self.down_stats } else { &self.down });
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        t
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        def: &NodeDef,
        values: &[Value],
        inputs: &[Option<u64>],
        res: u32,
        maps: MeshMaps,
    ) -> Result<Arc<Tex>, String> {
        // Image and mesh map slots.
        let mut slots: Vec<Option<Arc<Tex>>> = vec![None; MAX_IMAGES];
        let mut map_slots: Vec<Option<usize>> = vec![None; MAX_IMAGES];
        let mut comp_bits = 0u32;
        for (pi, v) in values.iter().enumerate() {
            let Some(slot) = def.image_slot(pi) else { continue };
            match v {
                Value::Image(Some(path)) => slots[slot] = Some(self.image(device, queue, path)?),
                Value::MeshMap(m) => {
                    if let Some((_, comp)) = maps(*m) {
                        map_slots[slot] = Some(*m);
                        comp_bits |= (comp as u32) << (slot * 2);
                    }
                }
                _ => {}
            }
        }
        let mut u = NodeUniform { p: [[0.0; 4]; def::MAX_PARAMS], defaults: [[0.0; 4]; MAX_INPUTS], info: [0.0; 4] };
        for (i, v) in values.iter().enumerate() {
            u.p[i] = def::pack(v);
        }
        let mut bits = 0u32;
        for (i, inp) in def.inputs.iter().enumerate() {
            u.defaults[i] = inp.default;
            if inputs[i].is_some() {
                bits |= 1 << i;
            }
        }
        u.info = [res as f32, bits as f32, comp_bits as f32, 0.0];
        let pipeline = self.pipeline(device, def)?.clone();

        let ubuf = {
            use wgpu::util::DeviceExt;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("graph node"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        let mut input_tex: Vec<Option<Arc<Tex>>> = inputs.iter().map(|i| i.and_then(|f| self.rendered.get(&f).map(|r| r.tex.clone()))).collect();
        // Nodes that read whole-image averages get their inputs with mips.
        if def.mean || def.minmax {
            for t in input_tex.iter_mut().flatten() {
                *t = Arc::new(self.mipped(device, encoder, t, def.minmax));
            }
        }
        let passes = match &def.passes {
            Passes::One => 1,
            Passes::Log2 => 32 - res.max(2).leading_zeros() + 1,
            Passes::Fixed(n) => *n,
            Passes::Param(name) => {
                let i = def.params.iter().position(|p| &p.name == name).ok_or(format!("@passes names no parameter '{name}'"))?;
                match values[i] {
                    Value::Int(n) => n.clamp(1, 1024) as u32,
                    _ => return Err(format!("@passes parameter '{name}' must be an int")),
                }
            }
        };
        let mut prev: Option<Arc<Tex>> = None;
        for pass_i in 0..passes {
            // A fresh texture each pass: layers may still hold the previous one.
            let tex = Arc::new(Tex::new(device, &def.id, res, res, FORMAT, LAYER_USAGE));
            let pass_buf;
            let ubuf = if passes == 1 {
                &ubuf
            } else {
                let mut pu = u;
                pu.info[3] = (pass_i + passes * 4096) as f32;
                use wgpu::util::DeviceExt;
                pass_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("graph node pass"),
                    contents: bytemuck::bytes_of(&pu),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                &pass_buf
            };
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ];
            for i in 0..MAX_INPUTS {
                let view = input_tex.get(i).and_then(|t| t.as_ref()).map_or(&self.dummy.view, |t| &t.view);
                entries.push(wgpu::BindGroupEntry { binding: 2 + i as u32, resource: wgpu::BindingResource::TextureView(view) });
            }
            for i in 0..MAX_IMAGES {
                let view = match (&slots[i], map_slots[i]) {
                    (Some(t), _) => &t.view,
                    (None, Some(m)) => maps(m).map_or(&self.dummy.view, |(t, _)| &t.view),
                    _ => &self.dummy.view,
                };
                entries.push(wgpu::BindGroupEntry {
                    binding: (2 + MAX_INPUTS + i) as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: PREV_BINDING as u32,
                resource: wgpu::BindingResource::TextureView(prev.as_ref().map_or(&self.dummy.view, |t| &t.view)),
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("graph node"), layout: &self.bgl, entries: &entries });
            let mut pass = gpu::pass(encoder, "graph node", &[gpu::color_attachment(&tex.view, wgpu::LoadOp::Clear(wgpu::Color::BLACK))]);
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
            drop(pass);
            prev = Some(tex);
        }
        prev.ok_or_else(|| "no passes".to_string())
    }
}
