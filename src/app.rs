//! Application shell: Substance-Painter-style layout, input handling and
//! the per-frame GPU flow (stamps -> composite -> viewport render).

use crate::bake::{self, BakeRun, BakeSettings, GpuBaker, IdSource, PreparedScene, Quality};
use crate::bake_cache;
use crate::bvh::Bvh;
use crate::cage::{CageProbe, CageStats, ProbeKey};
use crate::environment::{self, EnvBaker, EnvImage, EnvSource, Environment};
use crate::engine::{Drop, Folder, Projection};
use crate::engine::{
    Blend, BrushPaint, Content, Engine, FillParams, GEN_INPUTS, GenKind, Levels, MAPS, MapKind, MatValues, Stamp,
    StampUniform,
};
use crate::gpu::GpuMesh;
use crate::icons;
use crate::mesh::{Mesh, Primitive};
use crate::polyfill::{FillMode, Topology};
use crate::post::Tonemap;
use crate::presets::{self, PreviewLayer};
use crate::raytrace::{RayTracer, RtSettings};
use crate::theme;
use crate::undo::{StrokeTarget, UndoStack};
use crate::viewport::{Background, Cam2d, FAR, NEAR, OrbitCam, OverlaySlot, Overlays, ViewMode, ViewRenderer, ViewSettings};
use eframe::{egui, egui_wgpu, wgpu};
use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};
use glam::{Vec2, Vec3};
use std::sync::{Arc, mpsc};
use std::time::Instant;

mod export_ui;
mod dock;
mod gallery;
mod graph_ui;
mod shader_ui;
mod library_ui;
mod path_tool;
mod decal_gizmo;
mod bake_live;
mod spp_open;
mod preview_shader;
mod smart_io;
mod welcome;
pub(crate) mod live_reload;
mod mcp_tools;
pub(crate) mod prefs;
mod project_io;
mod shelf_previews;
mod texture_sets;

use export_ui::{ExportDialog, open_in_file_browser};
use graph_ui::GraphStore;
use prefs::Preferences;

/// Preferences window state.
struct PrefsUi {
    open: bool,
    page: prefs::Page,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Paint,
    Eraser,
    /// Picks the brush material from the surface under the cursor.
    Picker,
    /// Fills whole triangles, quads, mesh parts or UV islands.
    PolyFill,
    /// Lays the brush along an editable path (stitches, seams, panel lines).
    Path,
    /// Clicks pick what's under them: a decal selects its layer.
    Select,
}

/// Bake cage preview (see `crate::cage`): probe results and what's drawn.
struct CagePreview {
    show: bool,
    probe: Option<Arc<CageProbe>>,
    job: Option<(ProbeKey, mpsc::Receiver<Result<CageProbe, String>>)>,
    /// A probe that failed, so it isn't retried until the inputs change.
    failed: Option<(ProbeKey, String)>,
    /// (front, rear) the overlay was built for.
    drawn: Option<(f32, f32)>,
    stats: Option<CageStats>,
}

/// Polygon fill tool state.
struct PolyFillState {
    mode: FillMode,
    /// Removes paint instead of adding it (Ctrl inverts while clicking).
    erase: bool,
    /// Connectivity of the current mesh, built on first use.
    topo: Option<Arc<Topology>>,
    /// The triangle under the cursor this frame, from whichever view has it.
    frame_hover: Option<u32>,
    /// What the selection overlay currently shows.
    shown: Option<(u32, FillMode, bool)>,
    /// Triangles filled during the current drag, so each fills once.
    filled: std::collections::HashSet<u32>,
    stroke_erase: bool,
    /// Shift+drag box selection in progress.
    box_drag: Option<BoxDrag>,
}

/// A box drawn with Shift+drag; the faces inside fill on release.
#[derive(Clone, Copy)]
struct BoxDrag {
    view: ViewKind,
    start: egui::Pos2,
    end: egui::Pos2,
    erase: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Split,
    View3d,
    View2d,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewKind {
    V3d,
    V2d,
}

/// The top-level workspaces, switched by the tabs in the menu bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workspace {
    /// Painting and layers on the mesh.
    Paint,
    /// Material graphs: the node canvas next to the material on the mesh.
    Designer,
    /// Writing custom node shaders with a live preview.
    ShaderMaker,
}

impl Workspace {
    const ALL: [Workspace; 3] = [Workspace::Paint, Workspace::Designer, Workspace::ShaderMaker];
    fn name(self) -> &'static str {
        match self {
            Workspace::Paint => "Material Paint",
            Workspace::Designer => "Material Designer",
            Workspace::ShaderMaker => "Shader Maker",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Workspace::Paint => icons::PAINT_BRUSH,
            Workspace::Designer => icons::SPARKLE,
            Workspace::ShaderMaker => icons::MAGIC_WAND,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShelfCat {
    SmartMaterials,
    Materials,
    Generators,
    Brushes,
    Textures,
    Graphs,
    /// Materials downloaded from the asset library.
    Downloaded,
}

struct Brush {
    /// Diameter in screen points.
    size: f32,
    hardness: f32,
    flow: f32,
    opacity: f32,
    /// Stamp spacing as a fraction of the diameter.
    spacing: f32,
    mat: MatValues,
    mask_value: f32,
    /// Value painted onto a baked map when fixing it.
    fix_value: f32,
    /// Mirror strokes across the mesh's center plane: 0 off, 1 X, 2 Y, 3 Z.
    symmetry: u8,
    /// Brush tip from `brushes::BRUSH_ALPHAS`; `None` is the round brush.
    tip: Option<usize>,
    /// Tip rotation in degrees.
    angle: f32,
    /// Turn the tip along the stroke direction.
    follow: bool,
    /// Random rotation per stamp, 0..1 of a full turn.
    angle_jitter: f32,
    /// Random shrink per stamp, 0..1.
    size_jitter: f32,
    /// Random offset per stamp, in brush radii.
    scatter: f32,
    /// Jitter random state.
    rng: u32,
    /// How stamps reach the surface in the 3D view.
    projection: BrushProjection,
    /// Screen projection: also paint surfaces hidden behind others.
    paint_through: bool,
    /// Paint fades out on surfaces turned further than this from the brush (degrees).
    angle_limit: f32,
    /// Lazy mouse: the brush trails the cursor by this many points, smoothing strokes.
    stabilize: f32,
    /// Pen pressure scales the stamp size / flow.
    pressure_size: bool,
    pressure_flow: bool,
    /// Grain texture from `textures::TEXTURES` that breaks up the paint.
    grain: Option<usize>,
    /// Grain repeats across the mesh (or the UV square).
    grain_scale: f32,
    grain_strength: f32,
    grain_invert: bool,
}

/// How brush stamps are projected onto the mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BrushProjection {
    /// From the camera: what you see under the brush gets paint.
    Screen,
    /// A disc lying on the surface under the cursor: no stretching on
    /// slopes, wraps around edges and onto hidden faces.
    Surface,
    /// A disc in texture space around the cursor's UV.
    Uv,
}

impl BrushProjection {
    pub const ALL: [BrushProjection; 3] = [BrushProjection::Screen, BrushProjection::Surface, BrushProjection::Uv];
    pub fn name(self) -> &'static str {
        match self {
            BrushProjection::Screen => "Screen",
            BrushProjection::Surface => "Surface",
            BrushProjection::Uv => "UV",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            BrushProjection::Screen => "screen",
            BrushProjection::Surface => "surface",
            BrushProjection::Uv => "uv",
        }
    }
    fn tip(self) -> &'static str {
        match self {
            BrushProjection::Screen => "Paint from the camera, like a spray can: only what you see gets paint",
            BrushProjection::Surface => "The brush lies on the surface: even dots on slopes, wraps around edges",
            BrushProjection::Uv => "The brush is a circle in texture space around the cursor",
        }
    }
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            size: 70.0,
            hardness: 0.55,
            flow: 1.0,
            opacity: 1.0,
            spacing: 0.08,
            mat: MatValues { enabled: [true, true, true, false], color: [0.78, 0.2, 0.12], rough: 0.4, metal: 0.0, height: 0.5 },
            mask_value: 1.0,
            fix_value: 0.5,
            symmetry: 0,
            tip: None,
            angle: 0.0,
            follow: false,
            angle_jitter: 0.0,
            size_jitter: 0.0,
            scatter: 0.0,
            rng: 0x9e37_79b9,
            projection: BrushProjection::Screen,
            paint_through: false,
            angle_limit: 85.0,
            stabilize: 0.0,
            pressure_size: true,
            pressure_flow: false,
            grain: None,
            grain_scale: 4.0,
            grain_strength: 1.0,
            grain_invert: false,
        }
    }
}

struct StrokeInput {
    view: ViewKind,
    /// Where the last stamp went.
    last: Vec2,
    /// The stabilized brush position trailing the cursor.
    lazy: Vec2,
    /// Pen pressure at the last stamp.
    pressure: f32,
}

/// Where the last brush stroke ended, for Shift+click straight lines. Only
/// valid while the view hasn't moved.
#[derive(Clone, Copy)]
struct LineAnchor {
    view: ViewKind,
    at: Vec2,
    key: [f32; 8],
}

enum BakeJob {
    /// Worker thread is looking the bake up in the cache, or else loading the
    /// source and building the BVH.
    Preparing(mpsc::Receiver<Result<Prepared, String>>),
    /// GPU passes in progress; the result is cached under the key.
    Running(BakeRun, bake_cache::Key),
}

enum Prepared {
    Scene(PreparedScene, bake_cache::Key),
    Cached(bake_cache::Entry),
}

struct Shelf {
    cat: ShelfCat,
    search: String,
    /// Only materials of this type (smart, plain and downloaded materials); `None`: all,
    /// grouped by type.
    kind: Option<presets::MaterialType>,
    /// Thumbnail edge in points (Ctrl+scroll over the shelf, or the slider).
    icon_size: f32,
    smart: Vec<egui::TextureHandle>,
    materials: Vec<egui::TextureHandle>,
    generators: Vec<egui::TextureHandle>,
    brushes: Vec<egui::TextureHandle>,
    textures: Vec<egui::TextureHandle>,
}

enum Action {
    NewProject(Primitive),
    /// `None` asks for a file.
    OpenProject(Option<std::path::PathBuf>),
    /// Opens a Substance 3D Painter project as a new project.
    OpenSpp(Option<std::path::PathBuf>),
    SaveProject,
    SaveProjectAs,
    /// Saves, then runs the action if the save went through.
    SaveThen(Box<Action>),
    /// Runs an action that would discard unsaved changes, without asking.
    Confirmed(Box<Action>),
    Exit,
    ChangeMesh(Primitive),
    ImportMesh,
    Undo,
    Redo,
    AddPaint,
    AddFill(FillParams, String),
    /// A decal fill layer placed where the view looks: a band (no image) or text.
    AddDecal { text: bool },
    /// Renders a text decal's image again (or removes it with `None`).
    SetText { idx: usize, spec: Option<crate::decal::TextSpec> },
    /// A run-off layer under the text decal at `idx`, following it.
    AddRunoff { idx: usize },
    /// Moves every selected layer to `drop`, keeping their order.
    MoveSelection { drop: Drop },
    /// Copies the selected layers (folders with their contents).
    CopyLayers,
    /// Pastes the copied layers above the selected one.
    PasteLayers,
    AddMask { white: bool },
    AddGenerator(GenKind),
    RemoveMask,
    Duplicate,
    Delete,
    Clear,
    MoveTo { from: usize, drop: Drop },
    /// Loads (or with `None` clears) the image on a fill layer channel.
    SetFillImage { idx: usize, ch: usize, path: Option<std::path::PathBuf> },
    /// A shelf item dropped at a spot in the layer stack.
    ShelfDrop { item: ShelfItem, at: usize, parent: Option<u64> },
    /// A fill layer driven by a new material graph.
    AddGraphLayer,
    /// A graph fill layer from a ready-made graph (`graph::presets::PRESETS`).
    AddGraphPreset(usize),
    /// A mask on the selected layer driven by a new material graph.
    AddGraphMask,
    /// Opens the selected layer's graph in the Material Graph window.
    EditGraph,
    GraphUndo,
    GraphRedo,
    /// Opens a custom node in the code editor.
    GraphEditCode(String),
    /// Copies a node into the user folder as a custom node and opens it.
    GraphCopyNode(String),
    /// A graph renders again (a node it uses was saved).
    GraphRefresh(u64),
    /// Puts a node into the graph open in the Material Designer and shows it.
    GraphAddNode(String),
    AddOnTop(ShelfItem),
    /// Saves the selected layer (with its folder contents) as a smart material.
    SaveSmart,
    AddFolder,
    /// Puts the selected layer into a new folder.
    GroupIntoFolder,
    SetResolution(u32),
    /// Bakes the texture set being edited.
    Bake,
    /// Bakes every texture set.
    BakeAll,
    /// Selects a brush tip (`None`: round) and switches to painting.
    SetBrushTip(Option<usize>),
    /// Puts a shelf texture on a channel of the selected fill layer.
    TextureToChannel { texture: usize, ch: usize },
}

pub struct PainterApp {
    rs: egui_wgpu::RenderState,
    mesh: Arc<Mesh>,
    bvh: Arc<Bvh>,
    gpu_mesh: GpuMesh,
    /// The texture set being edited (see `texture_sets`).
    engine: Engine,
    sets: texture_sets::TextureSets,
    views: ViewRenderer,
    raytracer: RayTracer,
    rt: RtSettings,
    undo: UndoStack,
    selected: usize,
    target: StrokeTarget,
    tool: Tool,
    brush: Brush,
    cam: OrbitCam,
    cam2d: Cam2d,
    layout: Layout,
    view: ViewSettings,
    /// Lighting environment for the viewports.
    env: Environment,
    env_baker: EnvBaker,
    /// An HDRI decoding on a worker thread.
    env_load: Option<mpsc::Receiver<Result<EnvImage, String>>>,
    /// Built-in HDRIs whose files are installed.
    env_builtins: Vec<usize>,
    /// HDRI preview pictures by environment name.
    hdri_thumbs: std::collections::HashMap<String, egui::TextureHandle>,
    /// Built-in HDRI pictures arriving from the worker that makes them.
    hdri_thumb_rx: Option<mpsc::Receiver<(String, Vec<u8>)>>,
    /// The picture of an environment that just loaded.
    hdri_new_thumb: Option<(String, Vec<u8>)>,
    stroke: Option<StrokeInput>,
    bake_settings: BakeSettings,
    bake_job: Option<BakeJob>,
    baker: GpuBaker,
    show_bake: bool,
    shelf: Shelf,
    /// Renders the shelf thumbnails on a baked material ball; gone once done.
    shelf_previews: Option<shelf_previews::ShelfPreviews>,
    export: ExportDialog,
    prefs: Preferences,
    /// The preferences in effect (and saved); `prefs` is what the UI edits.
    prefs_applied: Preferences,
    prefs_ui: PrefsUi,
    /// Material graphs: the node library, the graphs layers use, the editor.
    graphs: GraphStore,
    workspace: Workspace,
    /// The docked panel layouts of the workspaces.
    docks: dock::Docks,
    /// A 3D view drew this frame (else its depth is rendered offscreen).
    drew_3d: bool,
    pending: Vec<Action>,
    status: String,
    show_help: bool,
    show_about: bool,
    /// The welcome window (startup, or Help → Welcome).
    show_welcome: bool,
    welcome_tex: Option<egui::TextureHandle>,
    library: library_ui::LibraryUi,
    user_smarts: Vec<smart_io::UserSmart>,
    path: path_tool::PathTool,
    bake_preview: bake_live::LivePreview,
    custom_ui: bake_live::CustomUi,
    spp: spp_open::SppOpen,
    preview: preview_shader::PreviewShader,
    /// A crash report the last run left, shown once.
    crash_report: Option<std::path::PathBuf>,
    frame_ms: f32,
    /// Placing this decal layer (by id) in the 3D view: drag moves it, scroll scales.
    decal_place: Option<u64>,
    /// The decal gizmo: the handle being dragged, the one under the pointer,
    /// and whether moving a decal turns it to face the surface.
    decal_drag: Option<decal_gizmo::GizmoDrag>,
    decal_hot: Option<decal_gizmo::Handle>,
    decal_align: bool,
    /// The selection highlight: which layer it shows and when it started.
    highlight: Option<(u64, Instant)>,
    /// Layers selected together (Ctrl / Shift click), by id; `selected` is
    /// the one the properties show. Ignored once it no longer holds it.
    multi: Vec<u64>,
    /// Where a Shift click range starts.
    sel_anchor: Option<usize>,
    /// The layer deselected in the view (Select tool: a click on nothing, or
    /// Esc): its gizmo is hidden until it's picked again.
    pub(super) gizmo_off: Option<u64>,
    /// Image files dropped on the window, waiting for a pointer position in
    /// the 3D view (Windows sends none while a file drag is over the window),
    /// and when they arrived.
    file_drop: Option<(Vec<std::path::PathBuf>, Instant)>,
    /// Copied layers, bottom to top (a folder after its contents).
    layer_clip: Vec<crate::engine::Layer>,
    /// Slow-frame profiling (MP_PROFILE=1): where this frame's time went.
    prof: Vec<(String, f32)>,
    profile: bool,
    /// When the last frame started (profiling: the gap between frames).
    last_frame: Option<Instant>,
    ppp: f32,
    mcp: Option<crate::mcp::McpServer>,
    /// Follows the open text project's files on disk.
    live: live_reload::LiveProject,
    /// MCP calls answered when the running bake completes.
    mcp_bake_waiters: Vec<mpsc::Sender<crate::mcp::ToolResult>>,
    /// The open project file, if it has been saved or opened.
    project_path: Option<std::path::PathBuf>,
    /// Undo state at the last save (see `UndoStack::state_serial`).
    saved_serial: u64,
    /// An action waiting for "save changes?" to be answered.
    confirm: Option<Action>,
    /// The window may close: changes were saved or discarded.
    allow_close: bool,
    recent: Vec<std::path::PathBuf>,
    /// Undo state at the last autosave.
    autosaved_serial: u64,
    /// An autosave from a session that didn't close cleanly, offered at startup.
    recovery: Option<std::path::PathBuf>,
    window_title: String,
    /// MCP calls answered once the loading HDRI is in use.
    mcp_env_waiters: Vec<mpsc::Sender<crate::mcp::ToolResult>>,
    /// Offscreen renderer for MCP previews, created on first use.
    mcp_views: Option<ViewRenderer>,
    /// Geometry drawn over the 3D view: bake cage, polygon fill selection.
    overlays: Overlays,
    cage_preview: CagePreview,
    poly: PolyFillState,
    /// The brush tip the engine holds (`None` until the first upload).
    uploaded_tip: Option<Option<usize>>,
    /// The brush grain the engine holds (`None` until the first upload).
    uploaded_grain: Option<Option<usize>>,
    /// Pen pressure, 0..1 (1 for mice).
    pressure: f32,
    line_anchor: Option<LineAnchor>,
}

const RESOLUTIONS: [u32; 4] = [512, 1024, 2048, 4096];
/// Where the source and releases live.
pub(crate) const REPO_URL: &str = "https://github.com/alla2001/material-painter";
/// `selected` when no layer is selected.
const NO_LAYER: usize = usize::MAX;

fn srgb_u8(c: [f32; 3]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

fn paint_checker(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, 0.0, Color32::from_gray(0x55));
    let n = 4;
    let s = rect.width() / n as f32;
    for y in 0..n {
        for x in 0..n {
            if (x + y) % 2 == 0 {
                let r = Rect::from_min_size(rect.min + vec2(x as f32 * s, y as f32 * s), vec2(s, s));
                painter.rect_filled(r, 0.0, Color32::from_gray(0x77));
            }
        }
    }
}

/// Image files a fill layer can load.
const IMAGE_EXT: [&str; 9] = ["png", "jpg", "jpeg", "tga", "bmp", "tif", "tiff", "exr", "hdr"];

/// Shelf thumbnail size range, in points.
const ICON_MIN: f32 = 48.0;
const ICON_MAX: f32 = 128.0;

/// A card-style section title: semibold white text over a hairline.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, title, egui::FontId::new(14.0, theme::heading_family()), Color32::WHITE);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, theme::OUTLINE));
    ui.add_space(6.0);
}

fn labeled<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_sized([92.0, 18.0], egui::Label::new(egui::RichText::new(label).color(theme::TEXT_DIM)));
        add(ui)
    })
    .inner
}

/// Normal, uv and uv units per world unit at a point of a triangle.
fn surface_at(mesh: &Mesh, tri: u32, bary: Vec3) -> (Vec3, Vec2, f32) {
    let v = |k: usize| &mesh.vertices[mesh.indices[tri as usize * 3 + k] as usize];
    let vs = [v(0), v(1), v(2)];
    let p = vs.map(|v| Vec3::from(v.pos));
    let t = vs.map(|v| Vec2::from(v.uv));
    let face = (p[1] - p[0]).cross(p[2] - p[0]);
    let smooth = Vec3::from(vs[0].normal) * bary.x + Vec3::from(vs[1].normal) * bary.y + Vec3::from(vs[2].normal) * bary.z;
    let n = smooth.try_normalize().unwrap_or_else(|| face.normalize_or(Vec3::Y));
    let uv = t[0] * bary.x + t[1] * bary.y + t[2] * bary.z;
    let world_area = face.length();
    let uv_area = (t[1] - t[0]).perp_dot(t[2] - t[0]).abs();
    let scale = if world_area > 1e-12 { (uv_area / world_area).sqrt() } else { 0.0 };
    (n, uv, scale)
}

/// A triangle's flat normal (decals project along the face, not along
/// smoothed vertex normals that lean toward the neighbors).
fn face_normal(mesh: &Mesh, tri: u32) -> Vec3 {
    let p = |k: usize| Vec3::from(mesh.vertices[mesh.indices[tri as usize * 3 + k] as usize].pos);
    (p(1) - p(0)).cross(p(2) - p(0)).normalize_or(Vec3::Y)
}

/// Where a ray meets the mesh, with the face normal turned toward the ray's
/// origin: where a decal placed there sits and looks.
fn decal_hit(mesh: &Mesh, bvh: &crate::bvh::Bvh, o: Vec3, d: Vec3) -> Option<(Vec3, Vec3)> {
    let hit = bvh.raycast(o, d)?;
    let n = face_normal(mesh, hit.tri);
    Some((hit.pos, if n.dot(d) > 0.0 { -n } else { n }))
}

/// Turns screen stamps (with their flow) into what the stamp shader
/// reads. The surface and UV projections find the surface under each
/// stamp, sized so it covers what the brush circle covers on screen, plus
/// its mirror image when `symmetry` is on (the screen projection mirrors
/// in the shader instead).
pub(crate) fn resolve_stamps(
    mesh: &Mesh,
    bvh: &Bvh,
    cam: &OrbitCam,
    size: Vec2,
    projection: BrushProjection,
    symmetry: u8,
    radius: f32,
    raw: &[([f32; 4], f32)],
) -> Vec<Stamp> {
    let mut out = Vec::with_capacity(raw.len() * if symmetry > 0 { 2 } else { 1 });
    let eye = cam.eye();
    let fwd = (cam.target - eye).normalize();
    let (right, _) = cam.basis();
    // World size of one pixel at unit distance in front of the camera.
    let px_world = 2.0 * (cam.fov_y * 0.5).tan() / size.y.max(1.0);
    let tangent = |n: Vec3, r: Vec3| (r - n * r.dot(n)).try_normalize().unwrap_or_else(|| n.any_orthonormal_vector());
    for &(s, flow) in raw {
        let mut st: Stamp = [s, [0.0; 4], [0.0; 4], [0.0; 4], [0.0, 0.0, flow, 0.0]];
        if projection == BrushProjection::Screen {
            out.push(st);
            continue;
        }
        let (o, d) = cam.ray(Vec2::new(s[0], s[1]), size);
        let Some(hit) = bvh.raycast(o, d) else { continue };
        let r = radius * s[2] * (hit.pos - eye).dot(fwd).max(1e-4) * px_world;
        let (n, uv, uv_scale) = surface_at(mesh, hit.tri, hit.bary);
        let x = tangent(n, right);
        st[1] = hit.pos.extend(r).to_array();
        st[2] = n.extend(r * uv_scale).to_array();
        st[3] = x.extend(0.0).to_array();
        st[4] = [uv.x, uv.y, flow, 0.0];
        out.push(st);
        if symmetry > 0 {
            let mut flip = Vec3::ONE;
            flip[symmetry as usize - 1] = -1.0;
            let (c, n) = (hit.pos * flip, n * flip);
            let mut m = st;
            m[1] = c.extend(r).to_array();
            m[2] = n.extend(r * uv_scale).to_array();
            m[3] = (x * flip).extend(0.0).to_array();
            if projection == BrushProjection::Uv {
                // The mirror image's UV: the surface at the mirrored point.
                match bvh.raycast(c + n * r, -n) {
                    Some(h) if h.pos.distance(c) < 2.0 * r => {
                        let (_, uv, scale) = surface_at(mesh, h.tri, h.bary);
                        m[2][3] = r * scale;
                        m[4][0] = uv.x;
                        m[4][1] = uv.y;
                    }
                    _ => continue,
                }
            }
            out.push(m);
        }
    }
    out
}

fn grow(acc: Option<[f32; 4]>, lo: Vec2, hi: Vec2) -> [f32; 4] {
    match acc {
        Some(a) => [a[0].min(lo.x), a[1].min(lo.y), a[2].max(hi.x), a[3].max(hi.y)],
        None => [lo.x, lo.y, hi.x, hi.y],
    }
}

/// UV bounds of what a frame's stamps can paint in the 3D view: the UV
/// boxes of every triangle they can reach. Screen stamps reach whatever
/// projects into their circles at any depth (and, with symmetry, whatever
/// mirrors into them), surface stamps whatever lies near their discs, UV
/// stamps their own squares. `None` when nothing is reachable.
pub(crate) fn stroke_region(
    mesh: &Mesh,
    bvh: &Bvh,
    cam: &OrbitCam,
    size: Vec2,
    projection: BrushProjection,
    symmetry: u8,
    radius: f32,
    stamps: &[Stamp],
) -> Option<[f32; 4]> {
    if projection == BrushProjection::Uv {
        return stamps.iter().filter(|s| s[1][3] > 0.0).fold(None, |acc, s| {
            let (uv, r) = (Vec2::new(s[4][0], s[4][1]), s[2][3]);
            Some(grow(acc, uv - Vec2::splat(r), uv + Vec2::splat(r)))
        });
    }
    let mut flip = Vec3::ONE;
    if symmetry > 0 {
        flip[symmetry as usize - 1] = -1.0;
    }
    let overlaps: Box<dyn Fn(Vec3, Vec3) -> bool> = match projection {
        BrushProjection::Surface => {
            // Spheres around the discs (planar radius r, slab r).
            let spheres: Vec<(Vec3, f32)> =
                stamps.iter().filter(|s| s[1][3] > 0.0).map(|s| (Vec3::new(s[1][0], s[1][1], s[1][2]), s[1][3] * 1.5)).collect();
            Box::new(move |lo: Vec3, hi: Vec3| spheres.iter().any(|&(c, r)| c.clamp(lo, hi).distance_squared(c) <= r * r))
        }
        _ => {
            // Screen box of all the circles; boxes that reach behind the
            // camera count as overlapping.
            let (mut a, mut b) = (Vec2::MAX, Vec2::MIN);
            for s in stamps {
                let r = radius * s[0][2];
                a = a.min(Vec2::new(s[0][0] - r, s[0][1] - r));
                b = b.max(Vec2::new(s[0][0] + r, s[0][1] + r));
            }
            let vp = cam.view_proj(size.x / size.y.max(1.0));
            let on_screen = move |lo: Vec3, hi: Vec3| {
                let (mut smin, mut smax) = (Vec2::MAX, Vec2::MIN);
                for k in 0..8 {
                    let p = Vec3::new(
                        if k & 1 == 0 { lo.x } else { hi.x },
                        if k & 2 == 0 { lo.y } else { hi.y },
                        if k & 4 == 0 { lo.z } else { hi.z },
                    );
                    let c = vp * p.extend(1.0);
                    if c.w <= 1e-4 {
                        return true;
                    }
                    let q = (Vec2::new(c.x, -c.y) / c.w * 0.5 + 0.5) * size;
                    smin = smin.min(q);
                    smax = smax.max(q);
                }
                smin.x <= b.x && smax.x >= a.x && smin.y <= b.y && smax.y >= a.y
            };
            Box::new(move |lo: Vec3, hi: Vec3| {
                on_screen(lo, hi) || (symmetry > 0 && { let (m, n) = (lo * flip, hi * flip); on_screen(m.min(n), m.max(n)) })
            })
        }
    };
    let mut acc = None;
    bvh.query(overlaps, |t| {
        for k in 0..3 {
            let uv = Vec2::from(mesh.vertices[mesh.indices[t as usize * 3 + k] as usize].uv);
            acc = Some(grow(acc, uv, uv));
        }
    });
    acc
}

/// The third-party license notices shipped next to the executable (or in
/// the source tree when run from it).
fn notices_path() -> Option<std::path::PathBuf> {
    let name = "THIRD_PARTY_NOTICES.html";
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
    // A macOS app bundle keeps it in Contents/Resources, beside Contents/MacOS.
    let resources = exe_dir.as_ref().and_then(|d| d.parent()).map(|c| c.join("Resources"));
    exe_dir.into_iter().chain(resources).chain(std::env::current_dir().ok()).map(|d| d.join(name)).find(|p| p.is_file())
}

/// HDRI preview pictures: width, height.
const HDRI_THUMB: [usize; 2] = [256, 128];

/// Makes a picture of every built-in HDRI on a worker thread, reading them
/// from the on-disk cache when it has them (else decoding the HDRI and
/// caching the picture), and sends them as they're ready.
fn start_hdri_thumbs(ctx: &egui::Context, builtins: Vec<usize>) -> mpsc::Receiver<(String, Vec<u8>)> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let dir = project_io::data_dir().map(|d| d.join("hdri_thumbs"));
        let [w, h] = HDRI_THUMB;
        for i in builtins {
            let name = environment::BUILTIN[i].0.to_string();
            let cached = dir.as_ref().map(|d| d.join(format!("{}.png", name.replace(' ', "_"))));
            let px = cached
                .as_ref()
                .and_then(|p| image::open(p).ok())
                .map(|img| img.into_rgba8().into_raw())
                .filter(|px| px.len() == w * h * 4)
                .or_else(|| {
                    let img = environment::decode(&EnvSource::Builtin(i)).ok()?;
                    let px = img.thumbnail(w, h);
                    if let Some(path) = &cached {
                        if let Some(d) = path.parent() {
                            let _ = std::fs::create_dir_all(d);
                        }
                        let _ = image::save_buffer(path, &px, w as u32, h as u32, image::ExtendedColorType::Rgba8);
                    }
                    Some(px)
                });
            if let Some(px) = px {
                if tx.send((name, px)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        }
    });
    rx
}

/// Runs `f`, turning a GPU out-of-memory error it causes into a message
/// instead of wgpu's default (a panic). Native error scopes resolve at once.
fn out_of_memory_guard<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> Result<T, String> {
    use std::future::Future;
    let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let value = f();
    let mut pop = std::pin::pin!(scope.pop());
    match pop.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(Some(_)) => {
            Err("Not enough GPU memory to bake. Lower the texture set resolution or close other GPU-heavy apps.".into())
        }
        _ => Ok(value),
    }
}

fn projection_combo(ui: &mut egui::Ui, id: &str, p: &mut BrushProjection) {
    egui::ComboBox::from_id_salt(id)
        .width(72.0)
        .selected_text(p.name())
        .show_ui(ui, |ui| {
            for m in BrushProjection::ALL {
                ui.selectable_value(p, m, m.name()).on_hover_text(m.tip());
            }
        })
        .response
        .on_hover_text(p.tip());
}

/// Smart mask inputs by what they read, for the Add input menu (custom
/// bake maps follow, by name).
const INPUT_GROUPS: [(&str, &[usize]); 4] = {
    use crate::engine::gin::*;
    [
        ("Shape", &[EDGE, CONVEX, CONCAVE, CREASE, THIN]),
        ("Light and exposure", &[OCCLUSION, CAVITY, SKY, UP, OVERHANG]),
        ("Position and parts", &[PART_GRADIENT, WORLD_HEIGHT, PART_RANDOM, ID]),
        ("Patterns", &[STREAKS, NOISE, GRAIN, CELLS, SCRATCHES]),
    ]
};

/// A new smart mask of a preset; a Custom Bake Map starts on the project's
/// first custom map.
fn new_generator(kind: GenKind, custom: &[bake::CustomMap]) -> crate::engine::Generator {
    let mut g = kind.default_generator();
    if let (GenKind::CustomMap, Some(c)) = (kind, custom.first()) {
        g.custom[0] = crate::engine::CustomInput { id: c.id, weight: 1.0 };
    }
    g
}

/// A custom bake map's name as masks list it ("missing" once removed).
fn custom_input_name(id: u32, custom: &[bake::CustomMap]) -> String {
    match custom.iter().find(|c| c.id == id) {
        Some(c) => format!("{}  {}", bake_live::kind_icon(c.kind), c.name),
        None => "(removed map)".to_string(),
    }
}

/// Which custom bake map a Custom Bake Map mask reads: the project's maps,
/// by name.
fn custom_map_picker(ui: &mut egui::Ui, g: &mut crate::engine::Generator, salt: usize, custom: &[bake::CustomMap]) -> bool {
    let mut dirty = false;
    // The input the mask reads most.
    let current = g.custom.iter().enumerate().filter(|(_, c)| c.id != 0).max_by(|a, b| a.1.weight.abs().total_cmp(&b.1.weight.abs())).map(|(k, c)| (k, c.id));
    if custom.is_empty() {
        labeled(ui, "Map", |ui| {
            ui.label(egui::RichText::new("No custom maps yet: add one in the bake window (Bake Mesh Maps → Custom maps).").small().color(theme::TEXT_DIM))
        });
        return false;
    }
    labeled(ui, "Map", |ui| {
        let text = current.map_or("Choose…".to_string(), |(_, id)| custom_input_name(id, custom));
        egui::ComboBox::from_id_salt(("gen custom map", salt)).selected_text(text).width(220.0).height(400.0).show_ui(ui, |ui| {
            for c in custom {
                let on = current.is_some_and(|(_, id)| id == c.id);
                let r = ui.add(egui::Button::selectable(on, custom_input_name(c.id, custom))).on_hover_text(c.kind.name());
                if r.clicked() && !on {
                    // Replace the map the mask reads most, keeping its weight.
                    let (k, weight) = current.map_or((g.custom.iter().position(|c| c.id == 0).unwrap_or(0), 1.0), |(k, _)| (k, g.custom[k].weight));
                    g.custom[k] = crate::engine::CustomInput { id: c.id, weight };
                    dirty = true;
                }
            }
        });
    });
    if current.is_some_and(|(_, id)| custom.iter().all(|c| c.id != id)) {
        ui.label(egui::RichText::new("This map was removed in the bake window; pick another.").small().color(Color32::from_rgb(240, 180, 80)));
    }
    dirty
}

/// One row of a mask's Mixes: remove button, name, weight slider.
fn input_row(ui: &mut egui::Ui, name: String, tip: &str, weight: &mut f32) -> (bool, bool) {
    let mut remove = false;
    let mut changed = false;
    ui.horizontal(|ui| {
        // Remove button first, so it never ends up past the panel's edge.
        if ui.add(egui::Button::new(icons::X).frame(false)).on_hover_text("Remove this input").clicked() {
            remove = true;
        }
        let label = egui::Label::new(egui::RichText::new(name).color(theme::TEXT)).truncate();
        ui.allocate_ui_with_layout(vec2(140.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(140.0);
            ui.add(label).on_hover_text(tip);
        });
        // The slider takes what's left, next to its value box.
        ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(40.0);
        let before = *weight;
        changed = ui.add(egui::Slider::new(weight, -2.0..=2.0).max_decimals(2)).changed();
        // A weight of 0 means "not used" and hides the row: one dragged or
        // typed to 0 keeps a sliver, so the row stays (and under the pointer)
        // until its remove button takes it out.
        if changed && *weight == 0.0 {
            *weight = if before < 0.0 { -1e-4 } else { 1e-4 };
        }
    });
    (remove, changed)
}

/// The inputs a smart mask mixes: only the ones it uses, each with its
/// weight and a remove button, and a menu to add more by kind.
fn generator_inputs_ui(ui: &mut egui::Ui, g: &mut crate::engine::Generator, custom: &[bake::CustomMap]) -> bool {
    let mut dirty = false;
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Mixes").small().color(theme::TEXT_DIM))
        .on_hover_text("What this mask is made of: each input adds (or with a negative weight, takes away) its share.");
    for i in 0..GEN_INPUTS.len() {
        if g.weights[i] == 0.0 {
            continue;
        }
        let (remove, changed) = input_row(ui, GEN_INPUTS[i].0.to_string(), GEN_INPUTS[i].1, &mut g.weights[i]);
        dirty |= changed;
        if remove {
            g.weights[i] = 0.0;
            dirty = true;
        }
    }
    for k in 0..g.custom.len() {
        if g.custom[k].id == 0 {
            continue;
        }
        let name = custom_input_name(g.custom[k].id, custom);
        let (remove, changed) = input_row(ui, name, "A custom bake map (bake window → Custom maps)", &mut g.custom[k].weight);
        dirty |= changed;
        if remove {
            g.custom[k] = crate::engine::CustomInput::default();
            dirty = true;
        }
    }
    let free = g.custom.iter().position(|c| c.id == 0);
    ui.menu_button(theme::with_icon(icons::PLUS, "Add input"), |ui| {
        ui.set_min_width(240.0);
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for (group, inputs) in INPUT_GROUPS {
                ui.label(egui::RichText::new(group).small().strong().color(theme::TEXT_DIM));
                for &i in inputs {
                    let used = g.weights[i] != 0.0;
                    let r = ui.add_enabled(!used, egui::Button::new(GEN_INPUTS[i].0).frame(false)).on_hover_text(GEN_INPUTS[i].1);
                    if r.clicked() {
                        g.weights[i] = 1.0;
                        dirty = true;
                        ui.close();
                    }
                }
                ui.add_space(4.0);
            }
            ui.label(egui::RichText::new("Custom maps").small().strong().color(theme::TEXT_DIM));
            if custom.is_empty() {
                ui.label(egui::RichText::new("None yet: add them in the bake window.").small().color(theme::TEXT_DIM));
            }
            for c in custom {
                let used = g.custom.iter().any(|x| x.id == c.id);
                let tip = if free.is_none() { "A mask mixes up to four custom maps" } else { c.kind.name() };
                let r = ui.add_enabled(!used && free.is_some(), egui::Button::new(custom_input_name(c.id, custom)).frame(false)).on_hover_text(tip);
                if let (true, Some(k)) = (r.clicked(), free) {
                    g.custom[k] = crate::engine::CustomInput { id: c.id, weight: 1.0 };
                    dirty = true;
                    ui.close();
                }
            }
        });
    })
    .response
    .on_hover_text("Mix in another baked map or pattern");
    dirty
}

fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    labeled(ui, label, |ui| ui.add(egui::Slider::new(v, range).max_decimals(2)).changed())
}

fn color_edit(ui: &mut egui::Ui, c: &mut [f32; 3]) -> bool {
    let mut rgb = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    let changed = ui.color_edit_button_srgb(&mut rgb).changed();
    if changed {
        *c = rgb.map(|v| v as f32 / 255.0);
    }
    changed
}

/// Channel toggles in Substance order: color, height, rough, metal.
fn channel_toggles(ui: &mut egui::Ui, enabled: &mut [bool; 4]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for (c, name) in [(0, "color"), (3, "height"), (1, "rough"), (2, "metal")] {
            let on = enabled[c];
            let text = egui::RichText::new(name).color(if on { Color32::WHITE } else { theme::TEXT_DIM });
            let b = egui::Button::new(text).fill(if on { theme::ACCENT_DIM } else { theme::WIDGET }).min_size(vec2(56.0, 20.0));
            if ui.add(b).clicked() {
                enabled[c] = !on;
                changed = true;
            }
        }
    });
    changed
}

fn material_controls(ui: &mut egui::Ui, m: &mut MatValues, show_values: bool) -> bool {
    let mut changed = channel_toggles(ui, &mut m.enabled);
    if !show_values {
        return changed;
    }
    ui.add_space(4.0);
    if m.enabled[0] {
        changed |= labeled(ui, "Base color", |ui| color_edit(ui, &mut m.color));
    }
    if m.enabled[3] {
        changed |= slider(ui, "Height", &mut m.height, -1.0..=1.0);
    }
    if m.enabled[1] {
        changed |= slider(ui, "Roughness", &mut m.rough, 0.0..=1.0);
    }
    if m.enabled[2] {
        changed |= slider(ui, "Metallic", &mut m.metal, 0.0..=1.0);
    }
    changed
}

/// A fill layer's channels: the brush's four, then normal and emission.
fn fill_controls(ui: &mut egui::Ui, f: &mut crate::engine::FillParams) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (c, name) in [(0, "color"), (3, "height"), (1, "rough"), (2, "metal"), (4, "normal"), (5, "emiss")] {
            let on = match c {
                4 => f.normal_on,
                5 => f.emission_on,
                _ => f.mat.enabled[c],
            };
            let text = egui::RichText::new(name).color(if on { Color32::WHITE } else { theme::TEXT_DIM });
            let b = egui::Button::new(text).fill(if on { theme::ACCENT_DIM } else { theme::WIDGET }).min_size(vec2(56.0, 20.0));
            if ui.add(b).clicked() {
                match c {
                    4 => f.normal_on = !on,
                    5 => f.emission_on = !on,
                    _ => f.mat.enabled[c] = !on,
                }
                changed = true;
            }
        }
    });
    ui.add_space(4.0);
    let m = &mut f.mat;
    if m.enabled[0] {
        changed |= labeled(ui, "Base color", |ui| color_edit(ui, &mut m.color));
    }
    if m.enabled[3] {
        changed |= slider(ui, "Height", &mut m.height, -1.0..=1.0);
    }
    if m.enabled[1] {
        changed |= slider(ui, "Roughness", &mut m.rough, 0.0..=1.0);
    }
    if m.enabled[2] {
        changed |= slider(ui, "Metallic", &mut m.metal, 0.0..=1.0);
    }
    if f.normal_on {
        changed |= slider(ui, "Normal strength", &mut f.normal_strength, 0.0..=4.0);
        changed |= labeled(ui, "", |ui| {
            ui.checkbox(&mut f.normal_dx, "DirectX normal (green down)")
                .on_hover_text("Unreal-style normal maps; leave off for OpenGL ones (Blender, Unity, Substance default)")
                .changed()
        });
    }
    if f.emission_on {
        changed |= labeled(ui, "Emission", |ui| color_edit(ui, &mut f.emission));
        changed |= labeled(ui, "Emission strength", |ui| {
            ui.add(egui::Slider::new(&mut f.emission_strength, 0.0..=50.0).logarithmic(true).max_decimals(2)).changed()
        });
    }
    changed
}

/// What the Decal section asks the app to do (it can't while the layer is borrowed).
#[derive(Default)]
struct DecalEvents {
    place: bool,
    snap: bool,
    runoff: bool,
    text: Option<crate::decal::TextSpec>,
    remove_text: bool,
}

fn log_slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    // Edits clamp, not every frame: a value typed past the range stays.
    labeled(ui, label, |ui| ui.add(egui::Slider::new(v, range).logarithmic(true).max_decimals(3).clamping(egui::SliderClamping::Edits)).changed())
}

/// The Decal section of a decal fill layer: placing, size, reach and its text.
fn decal_controls(
    ui: &mut egui::Ui,
    f: &mut FillParams,
    text: Option<&(Option<crate::decal::TextSpec>, f32)>,
    placing: bool,
    align: &mut bool,
    ev: &mut DecalEvents,
) -> bool {
    let mut changed = false;
    ui.label(
        egui::RichText::new("In the 3D view: drag the dot to slide it over the surface, the arrows to move it, the ring to turn it, the squares to scale it.")
            .small()
            .color(theme::TEXT_DIM),
    );
    ui.horizontal(|ui| {
        ui.checkbox(align, "Align to normal")
            .on_hover_text("Moving the decal turns it to face the surface under it (hold Shift to flip this while dragging)");
        if ui.button("Snap to surface").on_hover_text("Lay the decal flat on the surface under its center").clicked() {
            ev.snap = true;
        }
    });
    let label = if placing { "Placing… (Esc to stop)" } else { "Place on surface" };
    if ui
        .add(egui::Button::new(label).selected(placing))
        .on_hover_text("Drag on the mesh: the decal moves there and turns to face the surface (Shift keeps its angle). Scroll scales it, Ctrl+scroll turns it. Esc or right-click stops.")
        .clicked()
    {
        ev.place = true;
    }
    let spec = text.and_then(|(t, _)| t.clone());
    match text.filter(|_| spec.is_some()) {
        // Text keeps its proportions.
        Some((_, aspect)) => {
            if log_slider(ui, "Height", &mut f.decal.size[1], 0.003..=3.0) {
                f.decal.size[0] = f.decal.size[1] * aspect;
                changed = true;
            }
        }
        None => {
            changed |= log_slider(ui, "Width", &mut f.decal.size[0], 0.003..=10.0);
            changed |= log_slider(ui, "Height", &mut f.decal.size[1], 0.003..=10.0);
        }
    }
    changed |= slider(ui, "Rotation", &mut f.rotation, -180.0..=180.0);
    changed |= labeled(ui, "Depth", |ui| {
        ui.add(egui::Slider::new(&mut f.decal.depth, 0.002..=10.0).logarithmic(true).max_decimals(3).clamping(egui::SliderClamping::Edits))
            .on_hover_text(
                "How far the box reaches in and out along its normal, from its center (each way). \
                 Surfaces outside that slab don't take the decal; inside it, Facing decides. \
                 It matters where the same facing appears at another height, e.g. a lower step under the top, \
                 or with Facing 180, where only depth stops it reaching the far side.",
            )
            .changed()
    });
    changed |= labeled(ui, "Facing", |ui| {
        ui.add(egui::Slider::new(&mut f.decal.facing, 0.0..=180.0).suffix("°").max_decimals(0))
            .on_hover_text("Faces turned further than this from the decal don't take it; 180 wraps it around everything in the box")
            .changed()
    });
    changed |= slider(ui, "Edge softness", &mut f.decal.soft, 0.0..=0.5);
    changed |= labeled(ui, "Mirror", |ui| {
        ui.checkbox(&mut f.decal.mirror, "Both sides")
            .on_hover_text("Also on the other side of the mesh, mirrored across x = 0 (text stays readable)")
            .changed()
    });
    section(ui, "Text");
    match spec {
        Some(t) => {
            let mut t2 = t.clone();
            let mut edit = labeled(ui, "Text", |ui| ui.add(egui::TextEdit::multiline(&mut t2.text).desired_rows(1)).changed());
            edit |= labeled(ui, "Font", |ui| {
                let mut c = false;
                egui::ComboBox::from_id_salt("decal font").selected_text(t2.font.clone()).show_ui(ui, |ui| {
                    for name in crate::decal::FONT_CHOICES {
                        if crate::decal::font_path(name).is_some() {
                            c |= ui.selectable_value(&mut t2.font, name.to_string(), name).changed();
                        }
                    }
                });
                c
            });
            edit |= labeled(ui, "Weight", |ui| ui.add(egui::Slider::new(&mut t2.weight, 100.0..=900.0).step_by(50.0)).changed());
            edit |= slider(ui, "Letter spacing", &mut t2.spacing, -0.1..=0.6);
            if edit && t2 != t {
                ev.text = Some(t2);
            }
            ui.horizontal(|ui| {
                if ui.button("Add run-off").on_hover_text("Paint running down from the letters, on a layer below that follows this one").clicked() {
                    ev.runoff = true;
                }
                if ui.small_button("Remove text").clicked() {
                    ev.remove_text = true;
                }
            });
        }
        None => {
            if ui.button("Add text").on_hover_text("Cut the decal out by text (white, tinted by the base color)").clicked() {
                ev.text = Some(crate::decal::TextSpec::default());
            }
        }
    }
    changed
}

/// Adds a smart material as a folder holding its layers, at `at` inside
/// `parent`, as one undo step; returns the folder's index.
pub(crate) fn add_smart_material(
    engine: &mut Engine,
    undo: &mut UndoStack,
    smart: &presets::SmartMaterial,
    at: usize,
    parent: Option<u64>,
) -> usize {
    undo.begin_group(format!("Add {}", smart.name));
    let mut folder = engine.add_folder(at, parent, smart.name.to_string(), undo);
    let folder_id = engine.layers[folder].id;
    for l in smart.layers.iter().chain(&presets::MICRO_DETAIL) {
        // Each layer goes on top of the folder's contents, right below the folder.
        let idx = engine.add_fill_layer(folder, Some(folder_id), l.name.to_string(), l.fill(), undo);
        folder = idx + 1;
        if let Some(scan) = l.scan {
            // Maps the set lacks (or a missing assets folder) leave the
            // material's own values in place.
            if let Some(dir) = crate::environment::asset_dir("pbr").map(|d| d.join(scan.id)) {
                for (ch, file) in presets::Scan::FILES {
                    let path = dir.join(file);
                    if path.is_file()
                        && let Ok(img) = engine.shared_fill_image(&path)
                    {
                        engine.set_fill_image(idx, ch, Some(img), undo);
                    }
                }
            }
        }
        let layer = &mut engine.layers[idx];
        layer.opacity = l.opacity;
        layer.blend = l.blend;
        if let Some(g) = l.generator() {
            engine.add_mask(idx, true, Some(g), undo);
        }
    }
    engine.commit_props(undo);
    undo.end_group();
    folder
}

/// Something dragged off the shelf.
#[derive(Clone, Copy)]
enum ShelfItem {
    Smart(usize),
    Material(usize),
    Generator(GenKind),
    /// A brush tip (`brushes::BRUSH_ALPHAS`).
    Brush(usize),
    /// A tileable texture (`textures::TEXTURES`), added as a fill layer's height.
    Texture(usize),
    /// A saved material graph (`GraphStore::shelf`).
    Graph(usize),
    /// A downloaded material (`LibraryUi::local`).
    Library(usize),
    /// A smart material the user saved or downloaded (`user_smarts`).
    UserSmart(usize),
}

/// A layer being dragged in the layer stack.
#[derive(Clone, Copy)]
struct LayerDrag(usize);

/// Command line: `material_painter [--mcp] [project.mpp | project.json | mesh.fbx/.obj]`.
#[derive(Default)]
struct StartupArgs {
    /// Accept AI agent (MCP) connections this run, whatever the preference.
    mcp: bool,
    open: Option<std::path::PathBuf>,
}

impl StartupArgs {
    fn parse(args: impl Iterator<Item = String>) -> StartupArgs {
        let mut out = StartupArgs::default();
        for a in args {
            match a.as_str() {
                "--mcp" => out.mcp = true,
                _ if a.starts_with("--") => {}
                _ => out.open = Some(std::path::PathBuf::from(a)),
            }
        }
        out
    }
}

impl PainterApp {
    /// Switches workspace (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn set_workspace(&mut self, ws: Workspace) {
        self.workspace = ws;
    }

    /// Waits for every built-in HDRI picture, then shows the Preview tab
    /// (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_show_hdris(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.hdri_thumb_rx.take() {
            for (name, px) in rx.iter() {
                let tex = ctx.load_texture(format!("hdri {name}"), egui::ColorImage::from_rgba_unmultiplied(HDRI_THUMB, &px), Default::default());
                self.hdri_thumbs.insert(name, tex);
            }
        }
        self.docks.show_pane(Workspace::Paint, dock::Pane::Preview);
        self.export.open = false;
    }

    /// Turns on the UV seams overlay in 3D (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_show_seams(&mut self) {
        self.view.show_seams_3d = true;
    }

    /// Shows the plain materials on the shelf, every type (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_shelf_materials(&mut self) {
        self.shelf.cat = ShelfCat::Materials;
        self.shelf.kind = None;
    }

    /// Closes the welcome window (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_close_welcome(&mut self) {
        self.show_welcome = false;
    }

    /// Opens a project file (performance tests on real documents).
    #[cfg(test)]
    pub(crate) fn test_open_project(&mut self, path: &std::path::Path, ctx: &egui::Context) -> Result<(), String> {
        self.open_project(path, ctx)
    }

    #[cfg(test)]
    pub(crate) fn test_engine(&mut self) -> &mut Engine {
        &mut self.engine
    }

    #[cfg(test)]
    pub(crate) fn test_render_state(&self) -> egui_wgpu::RenderState {
        self.rs.clone()
    }

    /// Opens the asset library and shows the Downloaded shelf (offscreen UI
    /// tests); true once the open tab's catalog has arrived.
    #[cfg(test)]
    pub(crate) fn test_open_library(&mut self) -> bool {
        self.export.open = false;
        self.library.open = true;
        self.shelf.cat = ShelfCat::Downloaded;
        self.library.test_ready()
    }

    /// Saves a built-in scanned smart material as a user smart material and
    /// applies it again (offscreen UI tests). Returns a summary of the
    /// original and the applied block: (layers, masks, generators, filters,
    /// images, folders).
    #[cfg(test)]
    pub(crate) fn test_smart_roundtrip(&mut self, ctx: &egui::Context) -> [[usize; 6]; 2] {
        let k = presets::SMART_MATERIALS.iter().position(|s| s.is_scanned()).expect("a scanned smart material");
        let at = self.engine.layers.len();
        self.selected = add_smart_material(&mut self.engine, &mut self.undo, &presets::SMART_MATERIALS[k], at, None);
        // A mask filter, which smart materials must keep.
        let first = self.engine.block(self.selected).start;
        if let Some(m) = self.engine.layers[first..self.selected].iter_mut().find_map(|l| l.mask.as_mut()) {
            m.filters.push(crate::engine::MaskFilter::new(crate::engine::FilterKind::ALL[0]));
        }
        let summary = |e: &Engine, top: usize| {
            let b = e.block(top);
            let ls = &e.layers[b];
            [
                ls.len(),
                ls.iter().filter(|l| l.mask.is_some()).count(),
                ls.iter().filter(|l| l.mask.as_ref().is_some_and(|m| m.generator.is_some())).count(),
                ls.iter().map(|l| l.mask.as_ref().map_or(0, |m| m.filters.len())).sum(),
                ls.iter().map(|l| l.images.iter().flatten().count()).sum(),
                ls.iter().filter(|l| l.is_folder()).count(),
            ]
        };
        let before = summary(&self.engine, self.selected);
        self.save_smart_material(ctx);
        let path = self.user_smarts.iter().find(|s| s.name.starts_with(presets::SMART_MATERIALS[k].name)).expect("saved").path.clone();
        let at = self.engine.layers.len();
        self.apply_smart_material(&path, at, None);
        [before, summary(&self.engine, self.selected)]
    }

    /// Bakes again without ambient occlusion and edge distance (offscreen UI
    /// tests); true once that bake is in.
    #[cfg(test)]
    pub(crate) fn test_bake_without(&mut self, ctx: &egui::Context, maps: u32) -> bool {
        if self.bake_settings.maps != crate::bake::ALL_MAPS & !maps {
            self.bake_settings.maps = crate::bake::ALL_MAPS & !maps;
            self.start_bake(ctx);
        }
        self.bake_job.is_none() && self.engine.missing_maps() == maps
    }

    /// Sets up two custom bake maps, renames the first and shows the bake
    /// window with it open (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_show_custom(&mut self) {
        self.show_bake = true;
        self.add_custom_map(bake::CustomKind::Flow);
        self.add_custom_map(bake::CustomKind::Deposit);
        self.bake_settings.custom[0].name = "Rain from above".into();
        self.select_custom(self.bake_settings.custom[0].id);
    }

    /// A fill layer masked by the Custom Bake Map preset reading the second
    /// custom map, mixed with occlusion, selected with its mask (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_custom_mask(&mut self) {
        use crate::engine::gin;
        self.show_bake = false;
        let fill = FillParams { mat: MatValues { color: [0.8, 0.6, 0.3], ..Default::default() }, ..Default::default() };
        let idx = self.engine.add_fill_layer(self.engine.layers.len(), None, "Sand".into(), fill, &mut self.undo);
        let mut g = GenKind::CustomMap.default_generator();
        g.custom[0] = crate::engine::CustomInput { id: self.bake_settings.custom[1].id, weight: 1.0 };
        g.weights[gin::OCCLUSION] = 0.4;
        self.engine.add_mask(idx, true, Some(g), &mut self.undo);
        self.selected = idx;
        self.target = StrokeTarget::Mask;
        self.docks.show_pane(Workspace::Paint, dock::Pane::Layers);
    }

    /// Opens or closes the bake window (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_set_bake_window(&mut self, open: bool) {
        self.show_bake = open;
    }

    /// Shows smart materials on the shelf, the bake window, and selects the
    /// top layer of a smart material that uses the edge distance (offscreen
    /// UI tests).
    #[cfg(test)]
    pub(crate) fn test_show_missing(&mut self) {
        self.shelf.cat = ShelfCat::SmartMaterials;
        self.show_bake = true;
        let k = presets::SMART_MATERIALS.iter().position(|s| s.needs_maps() & (1 << 5) != 0).expect("a smart material with edge wear");
        let at = self.engine.layers.len();
        let folder = add_smart_material(&mut self.engine, &mut self.undo, &presets::SMART_MATERIALS[k], at, None);
        // The layer in it whose mask reads the edges.
        let block = self.engine.block(folder);
        self.selected = block
            .clone()
            .find(|&i| self.engine.layers[i].mask.as_ref().and_then(|m| m.generator).is_some_and(|g| g.needs_maps() & (1 << 5) != 0))
            .unwrap_or(folder);
        self.target = StrokeTarget::Mask;
    }

    /// Opens a graph in the Material Designer (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_open_graph(&mut self, id: u64) {
        self.open_graph(id);
    }

    /// Path tool on a new paint layer with the Stitch tip: a path across the
    /// ball in the 3D view or a curve on the UV map (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_path_setup(&mut self, kind_3d: bool) {
        let at = self.engine.layers.len();
        self.selected = self.engine.add_paint_layer(at, None, Some("Stitches".into()), &mut self.undo);
        self.target = StrokeTarget::Content;
        self.tool = Tool::Path;
        self.brush.tip = crate::brushes::BRUSH_ALPHAS.iter().position(|b| b.name == "Stitch");
        self.brush.size = 34.0;
        self.brush.spacing = 1.25;
        self.brush.angle = 0.0;
        self.brush.mat.enabled = [true, true, false, true];
        self.brush.mat.color = [0.95, 0.85, 0.3];
        self.brush.mat.height = 1.0;
        self.path.clear();
        let (kind, size) = if kind_3d { (ViewKind::V3d, self.views.view3d.size) } else { (ViewKind::V2d, self.views.view2d.size) };
        let (w, h) = (size[0] as f32, size[1] as f32);
        let marks: &[(f32, f32)] = if kind_3d { &[(0.25, 0.42), (0.4, 0.34), (0.6, 0.34), (0.75, 0.42)] } else { &[(0.2, 0.7), (0.35, 0.55), (0.55, 0.62), (0.75, 0.5)] };
        self.path.view = Some(kind);
        for &(x, y) in marks {
            if let Some(p) = self.path_from_screen(kind, Vec2::new(x * w, y * h)) {
                self.path.points.push(p);
            }
        }
    }

    /// Applies the path (offscreen UI tests); returns the status line.
    #[cfg(test)]
    pub(crate) fn test_path_apply(&mut self) -> String {
        self.apply_path();
        self.status.clone()
    }

    /// Closes the library and adds the first downloaded material as a layer
    /// (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_add_downloaded(&mut self) {
        self.library.open = false;
        self.pending.push(Action::AddOnTop(ShelfItem::Library(0)));
    }

    /// Opens the export window (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_open_export(&mut self) {
        self.export.open = true;
    }

    /// Queues an action by name (offscreen UI tests).
    #[cfg(test)]
    pub(crate) fn test_add_preset(&mut self, i: usize) {
        self.pending.push(Action::AddGraphPreset(i));
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let rs = cc.wgpu_render_state.clone().ok_or("Material Painter requires the wgpu renderer")?;
        let gpu = rs.adapter.get_info();
        crate::crash::set_context(format!("{} ({:?}, driver {} {})", gpu.name, gpu.backend, gpu.driver, gpu.driver_info));
        theme::apply(&cc.egui_ctx);

        let prefs = Preferences::load();
        let mesh = Primitive::MaterialBall.build();
        let gpu_mesh = GpuMesh::new(&rs.device, &mesh);
        let bvh = Bvh::from_mesh(&mesh);
        let mut engine = Engine::new(&rs, &gpu_mesh, (mesh.bounds_min, mesh.bounds_max), prefs.default_resolution);
        engine.add_paint_layer(0, None, None, &mut UndoStack::default());
        let views = ViewRenderer::new(&rs);
        let env_baker = EnvBaker::new(&rs.device);
        // A real studio HDRI (plus the key light) when installed, else the
        // procedural studio.
        let default_env = environment::BUILTIN
            .iter()
            .position(|b| b.0 == prefs.default_hdri)
            .or_else(|| environment::BUILTIN.iter().position(|b| b.0 == "Small Studio"))
            .filter(|i| environment::available_builtins().contains(i))
            .unwrap_or(0);
        let env = env_baker.upload(&rs, &environment::decode(&EnvSource::Builtin(default_env))?);
        let mut raytracer = RayTracer::new(&rs);
        raytracer.set_mesh(&rs.device, &mesh, &bvh);
        let baker = GpuBaker::new(&rs.device);

        let ctx = &cc.egui_ctx;
        let load = |name: &str, img: egui::ColorImage| ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
        let materials = presets::MATERIALS
            .iter()
            .map(|m| load(m.name, presets::render_preview(&[PreviewLayer { fill: m.fill(), opacity: 1.0, generator: None }])))
            .collect();
        let smart = presets::SMART_MATERIALS
            .iter()
            .map(|s| {
                let layers: Vec<PreviewLayer> = s
                    .layers
                    .iter()
                    .map(|l| PreviewLayer { fill: l.fill(), opacity: l.opacity, generator: l.generator() })
                    .collect();
                load(s.name, presets::render_preview(&layers))
            })
            .collect();
        let generators = GenKind::ALL.iter().map(|g| load(g.name(), presets::render_generator_preview(*g))).collect();
        // Brush tips show as white shapes; textures as their grayscale tile.
        const THUMB: u32 = 96;
        let brushes = crate::brushes::BRUSH_ALPHAS
            .iter()
            .map(|b| {
                let px: Vec<Color32> = b.render(THUMB).into_iter().map(|a| Color32::from_white_alpha(a)).collect();
                load(b.name, egui::ColorImage::new([THUMB as usize; 2], px))
            })
            .collect();
        let textures = crate::textures::TEXTURES
            .iter()
            .map(|t| {
                let px: Vec<Color32> = t.render(THUMB).into_iter().map(Color32::from_gray).collect();
                load(t.name, egui::ColorImage::new([THUMB as usize; 2], px))
            })
            .collect();

        let mut app = PainterApp {
            rs,
            mesh: Arc::new(mesh),
            bvh: Arc::new(bvh),
            gpu_mesh,
            engine,
            views,
            raytracer,
            rt: RtSettings::default(),
            undo: UndoStack::default(),
            selected: 0,
            target: StrokeTarget::Content,
            tool: Tool::Paint,
            brush: Brush::default(),
            cam: OrbitCam::default(),
            cam2d: Cam2d::default(),
            layout: Layout::View3d,
            view: ViewSettings::default(),
            env,
            env_baker,
            env_load: None,
            env_builtins: environment::available_builtins(),
            hdri_thumbs: Default::default(),
            hdri_thumb_rx: Some(start_hdri_thumbs(&cc.egui_ctx, environment::available_builtins())),
            hdri_new_thumb: None,
            stroke: None,
            bake_settings: BakeSettings::default(),
            bake_job: None,
            baker,
            show_bake: false,
            shelf: Shelf { cat: ShelfCat::SmartMaterials, search: String::new(), kind: None, icon_size: 72.0, smart, materials, generators, brushes, textures },
            shelf_previews: None,
            export: ExportDialog::new("MaterialBall".into()),
            prefs_applied: prefs.clone(),
            prefs,
            prefs_ui: PrefsUi { open: false, page: prefs::Page::General },
            graphs: GraphStore::new(&cc.egui_ctx),
            workspace: Workspace::Paint,
            docks: dock::Docks::load(),
            drew_3d: false,
            pending: Vec::new(),
            status: "Ready. Paint with LMB, orbit with Alt+LMB or RMB, pan with MMB.".into(),
            show_help: false,
            show_about: false,
            show_welcome: false,
            welcome_tex: None,
            library: Default::default(),
            user_smarts: smart_io::list(&cc.egui_ctx),
            path: Default::default(),
            bake_preview: Default::default(),
            custom_ui: Default::default(),
            spp: Default::default(),
            preview: Default::default(),
            crash_report: crate::crash::take_report(),
            frame_ms: 0.0,
            decal_place: None,
            decal_drag: None,
            decal_hot: None,
            decal_align: true,
            highlight: None,
            multi: Vec::new(),
            sel_anchor: None,
            layer_clip: Vec::new(),
            file_drop: None,
            gizmo_off: None,
            prof: Vec::new(),
            last_frame: None,
            profile: std::env::var_os("MP_PROFILE").is_some(),
            ppp: cc.egui_ctx.pixels_per_point(),
            mcp: None,
            live: live_reload::LiveProject::new(&cc.egui_ctx),
            mcp_bake_waiters: Vec::new(),
            mcp_env_waiters: Vec::new(),
            project_path: None,
            saved_serial: 0,
            confirm: None,
            allow_close: false,
            recent: project_io::load_recent(),
            autosaved_serial: 0,
            recovery: project_io::pending_recovery(),
            window_title: String::new(),
            mcp_views: None,
            overlays: Overlays::default(),
            cage_preview: CagePreview { show: true, probe: None, job: None, failed: None, drawn: None, stats: None },
            poly: PolyFillState {
                mode: FillMode::Polygon,
                box_drag: None,
                erase: false,
                topo: None,
                frame_hover: None,
                shown: None,
                filled: Default::default(),
                stroke_erase: false,
            },
            uploaded_tip: None,
            uploaded_grain: None,
            sets: Default::default(),
            pressure: 1.0,
            line_anchor: None,
        };
        // Shelf thumbnails from an earlier run, then render whatever's missing.
        let missing = shelf_previews::load_cached(&mut app.shelf);
        if !missing.is_empty() {
            app.shelf_previews = Some(shelf_previews::ShelfPreviews::start(&app.rs, ctx, &app.env_baker, missing));
        }
        app.apply_startup_preferences(ctx);
        let startup = StartupArgs::parse(std::env::args().skip(1));
        match crate::mcp::McpServer::start(&cc.egui_ctx, app.prefs.mcp_port, app.prefs.mcp_on_start || startup.mcp) {
            Ok(server) => app.mcp = Some(server),
            Err(e) => app.status = e,
        }
        match startup.open {
            Some(path) if project_io::is_text_project(&path) || path.extension().is_some_and(|x| x == project_io::EXTENSION) => {
                app.pending.push(Action::OpenProject(Some(path)));
            }
            Some(path) => match Mesh::load_file(&path) {
                Ok(mesh) => app.load_mesh(mesh, ctx),
                Err(e) => app.status = e,
            },
            None if app.prefs.bake_on_load => app.start_bake(ctx),
            None => {}
        }
        Ok(app)
    }

    // ------------------------------------------------------------------ jobs

    /// Bakes the texture set being edited.
    fn start_bake(&mut self, ctx: &egui::Context) {
        self.sets.bake_queue.clear();
        self.start_bake_set(self.sets.active, ctx);
    }

    /// Bakes every texture set, the edited one first.
    fn start_bake_all(&mut self, ctx: &egui::Context) {
        let active = self.sets.active;
        self.sets.bake_queue = (0..self.mesh.sets.len()).filter(|&k| k != active).collect();
        self.start_bake_set(active, ctx);
    }

    fn start_bake_set(&mut self, set: usize, ctx: &egui::Context) {
        if set == self.sets.active {
            self.preview_full_bake_started();
        }
        let Some(res) = self.set_engine_mut(set).map(|e| e.res.min(bake::MAX_BAKE_RES)) else { return };
        self.sets.baking = set;
        let mesh = self.mesh.clone();
        let settings = self.bake_settings.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let key = bake_cache::key(&mesh, &settings, res, set);
            let prepared = match bake_cache::load(&key) {
                Some(entry) => Ok(Prepared::Cached(entry)),
                None => bake::prepare(&mesh, &settings, set).map(|scene| Prepared::Scene(scene, key)),
            };
            let _ = tx.send(prepared);
            ctx.request_repaint();
        });
        // A newer job replaces any running one; its results are dropped.
        self.bake_job = Some(BakeJob::Preparing(rx));
        self.status = if self.mesh.sets.len() > 1 {
            format!("Preparing bake of {}…", self.mesh.sets[set].name)
        } else {
            "Preparing bake…".into()
        };
    }

    /// A bake finished (or failed): the next queued set starts.
    fn bake_next(&mut self, ctx: &egui::Context) {
        self.bake_job = None;
        if let Some(set) = self.sets.bake_queue.pop_front() {
            self.start_bake_set(set, ctx);
        }
    }

    fn poll_bake(&mut self, ctx: &egui::Context) {
        match &mut self.bake_job {
            None => {}
            Some(BakeJob::Preparing(rx)) => match rx.try_recv() {
                Ok(Ok(Prepared::Cached(entry))) => {
                    let rs = &self.rs;
                    match out_of_memory_guard(&rs.device, || entry.upload(&rs.device, &rs.queue)) {
                        Ok((maps, report)) => {
                            self.status = format!("Loaded {} cached mesh maps ({}², from {}).", report.baked.count_ones(), report.resolution, report.source);
                            if let Some(e) = self.set_engine_mut(self.sets.baking) {
                                e.set_maps(maps, Arc::new(report));
                            }
                        }
                        Err(e) => {
                            self.fail_mcp_bake(&e);
                            self.status = e;
                        }
                    }
                    self.bake_next(ctx);
                }
                Ok(Ok(Prepared::Scene(scene, key))) => {
                    let res = self.set_engines().get(self.sets.baking).map_or(self.engine.res, |e| e.res);
                    let (baker, rs, mesh, settings) = (&self.baker, &self.rs, &self.gpu_mesh, &self.bake_settings);
                    match out_of_memory_guard(&rs.device, || baker.start(&rs.device, &rs.queue, mesh, &scene, settings, res)) {
                        Ok(run) => {
                            self.bake_job = Some(BakeJob::Running(run, key));
                            ctx.request_repaint();
                        }
                        Err(e) => {
                            self.fail_mcp_bake(&e);
                            self.status = e;
                            self.bake_job = None;
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.fail_mcp_bake(&e);
                    self.status = e;
                    self.bake_job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.fail_mcp_bake("The bake preparation stopped unexpectedly.");
                    self.bake_job = None;
                }
            },
            Some(BakeJob::Running(run, key)) => {
                let (baker, rs) = (&self.baker, &self.rs);
                let pumped = match out_of_memory_guard(&rs.device, || baker.pump(run, &rs.device, &rs.queue, ctx)) {
                    Ok(maps) => maps,
                    Err(e) => {
                        self.fail_mcp_bake(&e);
                        self.status = e;
                        self.bake_job = None;
                        return;
                    }
                };
                if let Some(maps) = pumped {
                    let report = run.report.clone();
                    let matched = match report.matched_names {
                        0 => String::new(),
                        n => format!(", {n} parts matched by name"),
                    };
                    let set_res = texture_sets::engines(&self.engine, &self.sets, self.mesh.sets.len()).get(self.sets.baking).map_or(self.engine.res, |e| e.res);
                    let upscaled = if report.resolution < set_res { format!(", upscaled to {set_res}²") } else { String::new() };
                    let set_name = match self.mesh.sets.get(self.sets.baking) {
                        Some(s) if self.mesh.sets.len() > 1 => format!(" for {}", s.name),
                        _ => String::new(),
                    };
                    self.status = format!(
                        "Baked {} mesh maps{set_name} from {} ({} tris, {} parts{matched}{}) at {}²{upscaled}.",
                        report.baked.count_ones(),
                        report.source,
                        report.triangles,
                        report.parts,
                        if report.used_cage { ", cage" } else { "" },
                        report.resolution
                    );
                    bake_cache::store(key.clone(), &maps, report.clone(), &self.rs.device, &self.rs.queue);
                    if let Some(e) = self.set_engine_mut(self.sets.baking) {
                        e.set_maps(maps, report);
                    }
                    self.bake_next(ctx);
                }
            }
        }
    }

    fn bake_progress(&self) -> Option<(f32, &'static str)> {
        match &self.bake_job {
            None => None,
            Some(BakeJob::Preparing(_)) => Some((0.0, "Preparing")),
            Some(BakeJob::Running(run, _)) => Some((run.progress(), "Baking")),
        }
    }

    fn load_mesh(&mut self, mesh: Mesh, ctx: &egui::Context) {
        if self.stroke.is_some() {
            return;
        }
        // A bake of the old mesh can't land on the new one.
        self.bake_job = None;
        self.end_preview(false);
        self.gpu_mesh = GpuMesh::new(&self.rs.device, &mesh);
        self.bvh = Arc::new(Bvh::from_mesh(&mesh));
        self.raytracer.set_mesh(&self.rs.device, &mesh, &self.bvh);
        self.export.mesh_name = mesh.name.replace(' ', "");
        self.status = match mesh.sets.len() {
            1 => format!("Loaded '{}' ({} triangles).", mesh.name, mesh.triangle_count()),
            n => format!("Loaded '{}' ({} triangles, {n} texture sets).", mesh.name, mesh.triangle_count()),
        };
        let old_names: Vec<String> = self.mesh.sets.iter().map(|s| s.name.clone()).collect();
        self.mesh = Arc::new(mesh);
        self.rebuild_sets(&old_names);
        // Mesh-dependent helpers rebuild for the new mesh on demand.
        self.poly.topo = None;
        self.poly.shown = None;
        self.overlays.clear(OverlaySlot::Selection);
        self.cam = OrbitCam { fov_y: self.cam.fov_y, ..OrbitCam::default() };
        if self.prefs.bake_on_load {
            self.start_bake_all(ctx);
        }
    }

    // ------------------------------------------------------------------ actions

    fn process_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.pending) {
            if self.stroke.is_some() {
                continue;
            }
            let (at, parent) = self.insert_place();
            match action {
                a @ (Action::NewProject(_) | Action::OpenProject(_) | Action::OpenSpp(_) | Action::Exit) if self.is_modified() => {
                    self.confirm = Some(a);
                }
                Action::Confirmed(a) => self.run_unchecked(*a, ctx),
                a @ (Action::NewProject(_) | Action::OpenProject(_) | Action::OpenSpp(_) | Action::Exit) => self.run_unchecked(a, ctx),
                Action::SaveProject => {
                    self.save(false);
                }
                Action::SaveProjectAs => {
                    self.save(true);
                }
                Action::SaveThen(a) => {
                    if self.save(false) {
                        self.pending.push(Action::Confirmed(a));
                    }
                }
                Action::ChangeMesh(p) => self.load_mesh(p.build(), ctx),
                Action::ImportMesh => {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Mesh (FBX, OBJ)", &crate::import::EXTENSIONS).pick_file() {
                        match Mesh::load_file(&path) {
                            Ok(mesh) => self.load_mesh(mesh, ctx),
                            Err(e) => self.status = e,
                        }
                    }
                }
                Action::Undo => {
                    self.switch_for_history(false);
                    self.engine.undo(&mut self.undo);
                    self.after_history_step();
                }
                Action::Redo => {
                    self.switch_for_history(true);
                    self.engine.redo(&mut self.undo);
                    self.after_history_step();
                }
                Action::AddPaint => {
                    self.selected = self.engine.add_paint_layer(at, parent, None, &mut self.undo);
                    self.target = StrokeTarget::Content;
                }
                Action::AddFill(fill, name) => {
                    self.selected = self.engine.add_fill_layer(at, parent, name, fill, &mut self.undo);
                    self.target = StrokeTarget::Content;
                }
                Action::AddDecal { text } => {
                    // Where the view looks, on the surface facing the camera.
                    let (o, d) = (self.cam.eye(), (self.cam.target - self.cam.eye()).normalize_or(Vec3::NEG_Z));
                    let (pos, n) = decal_hit(&self.mesh, &self.bvh, o, d).unwrap_or((self.cam.target, -d));
                    let mut fill = FillParams { projection: Projection::Decal, ..FillParams::default() };
                    fill.mat.enabled = [true, true, true, false];
                    fill.mat.metal = 0.0;
                    fill.mat.rough = 0.5;
                    fill.decal.center = pos.to_array();
                    fill.decal.normal = n.to_array();
                    fill.decal.depth = 0.05;
                    if text {
                        fill.mat.color = [0.85, 0.85, 0.82];
                        fill.decal.size = [0.3, 0.06];
                    } else {
                        fill.mat.color = [0.62, 0.63, 0.62];
                        fill.decal.size = [0.2, 0.08];
                    }
                    let name = if text { "Text" } else { "Decal" };
                    self.selected = self.engine.add_fill_layer(at, parent, name.into(), fill, &mut self.undo);
                    self.target = StrokeTarget::Content;
                    if text {
                        self.pending.push(Action::SetText { idx: self.selected, spec: Some(crate::decal::TextSpec::default()) });
                    }
                    self.decal_place = Some(self.engine.layers[self.selected].id);
                    self.status = "Drag on the mesh to place the decal; scroll scales it, Ctrl+scroll turns it; Esc stops.".into();
                }
                Action::AddRunoff { idx } => {
                    if let Some(src) = self.engine.layers.get(idx) {
                        let (id, parent, name) = (src.id, src.parent, src.name.clone());
                        if let Content::Fill(sf) = &src.content {
                            let mut fill = *sf;
                            fill.mat.color = sf.mat.color.map(|c| c * 0.8);
                            fill.runoff = Some(crate::decal::Runoff { source: id, length: 0.8, seed: (id % 97) as f32 });
                            let at = self.engine.add_fill_layer(idx, parent, format!("Run-off: {name}"), fill, &mut self.undo);
                            self.engine.layers[at].opacity = 0.8;
                            self.selected = at;
                            self.target = StrokeTarget::Content;
                        }
                    }
                }
                Action::SetText { idx, spec } => match spec {
                    None => self.engine.set_fill_image(idx, 0, None, &mut self.undo),
                    Some(spec) => match self.engine.text_fill_image(&spec) {
                        Ok((img, aspect)) => {
                            self.engine.set_fill_image(idx, 0, Some(img), &mut self.undo);
                            if let Some(Content::Fill(f)) = self.engine.layers.get_mut(idx).map(|l| &mut l.content) {
                                // The height stays; the width follows the text.
                                f.decal.size[0] = f.decal.size[1] * aspect;
                                f.projection = Projection::Decal;
                            }
                            self.engine.mark_layer_dirty(idx);
                        }
                        Err(e) => self.status = e,
                    },
                },
                Action::AddFolder => {
                    let n = self.engine.layers.iter().filter(|l| l.is_folder()).count() + 1;
                    self.selected = self.engine.add_folder(at, parent, format!("Folder {n}"), &mut self.undo);
                    self.target = StrokeTarget::Content;
                }
                Action::GroupIntoFolder => {
                    let sel = self.selection();
                    if let Some(&top) = sel.last() {
                        let ids: Vec<u64> = sel.iter().map(|&i| self.engine.layers[i].id).collect();
                        let parent = self.engine.layers[top].parent;
                        self.undo.begin_group(format!("Group {} layers", ids.len()));
                        let n = self.engine.layers.iter().filter(|l| l.is_folder()).count() + 1;
                        let folder = self.engine.add_folder(top + 1, parent, format!("Folder {n}"), &mut self.undo);
                        let folder_id = self.engine.layers[folder].id;
                        // Bottom first: each goes on top inside the folder.
                        for id in ids {
                            if let Some(i) = self.engine.index_of(id) {
                                self.engine.move_to(i, Drop::Into(folder_id), &mut self.undo);
                            }
                        }
                        self.undo.end_group();
                        self.selected = self.engine.index_of(folder_id).unwrap_or(folder);
                        self.multi.clear();
                        self.target = StrokeTarget::Content;
                    }
                }
                Action::AddMask { white } => {
                    if self.selected < self.engine.layers.len() {
                        self.engine.add_mask(self.selected, white, None, &mut self.undo);
                        self.target = StrokeTarget::Mask;
                    }
                }
                Action::AddGenerator(kind) => {
                    let g = new_generator(kind, &self.bake_settings.custom);
                    if let Some(layer) = self.engine.layers.get_mut(self.selected) {
                        match &mut layer.mask {
                            Some(m) => {
                                m.generator = Some(g);
                                self.engine.mark_dirty();
                            }
                            None => self.engine.add_mask(self.selected, true, Some(g), &mut self.undo),
                        }
                        self.target = StrokeTarget::Mask;
                    } else {
                        self.status = "Select a layer to add a generator mask to.".into();
                    }
                }
                Action::RemoveMask => {
                    self.engine.remove_mask(self.selected, &mut self.undo);
                    self.target = StrokeTarget::Content;
                }
                Action::SaveSmart => self.save_smart_material(ctx),
                Action::Duplicate => {
                    let sel = self.selection();
                    self.undo.begin_group(format!("Duplicate {} layers", sel.len()));
                    // Top first: a copy goes right above its layer, moving only those above.
                    let mut copies = Vec::new();
                    for &i in sel.iter().rev() {
                        if let Some(c) = self.engine.duplicate_layer(i, &mut self.undo) {
                            copies.push(self.engine.layers[c].id);
                        }
                    }
                    self.undo.end_group();
                    if let Some(i) = copies.first().and_then(|&id| self.engine.index_of(id)) {
                        self.selected = i;
                    }
                    self.multi = copies;
                }
                Action::Delete => {
                    let sel = self.selection();
                    self.undo.begin_group(format!("Delete {} layers", sel.len()));
                    // Top first: removing a layer moves only those above it.
                    for &i in sel.iter().rev() {
                        self.engine.delete_layer(i, &mut self.undo);
                    }
                    self.undo.end_group();
                    let lowest = sel.first().copied().unwrap_or(self.selected);
                    self.selected = lowest.min(self.engine.layers.len().saturating_sub(1));
                    self.multi.clear();
                    self.target = StrokeTarget::Content;
                }
                Action::CopyLayers => {
                    let sel = self.selection();
                    let clip: Vec<_> = sel.iter().flat_map(|&i| self.engine.block(i)).map(|j| self.engine.copy_of(&self.engine.layers[j])).collect();
                    if !clip.is_empty() {
                        self.status = format!("Copied {} layer{}", sel.len(), if sel.len() == 1 { "" } else { "s" });
                        self.layer_clip = clip;
                    }
                }
                Action::PasteLayers => {
                    if !self.layer_clip.is_empty() {
                        let layers: Vec<_> = self.layer_clip.iter().map(|l| self.engine.copy_of(l)).collect();
                        let (at, parent) = match self.engine.layers.get(self.selected) {
                            Some(l) => (self.selected + 1, l.parent),
                            None => (self.engine.layers.len(), None),
                        };
                        let ids = self.engine.insert_layers(at, parent, layers, "Paste layers".into(), &mut self.undo);
                        let top: Vec<u64> = ids.iter().copied().filter(|&id| self.engine.index_of(id).is_some_and(|i| self.engine.layers[i].parent == parent)).collect();
                        if let Some(i) = top.last().and_then(|&id| self.engine.index_of(id)) {
                            self.selected = i;
                        }
                        self.status = format!("Pasted {} layer{}", top.len(), if top.len() == 1 { "" } else { "s" });
                        self.multi = top;
                        self.target = StrokeTarget::Content;
                        self.engine.mark_dirty();
                    }
                }
                Action::MoveSelection { drop } => {
                    let ids: Vec<u64> = self.selection().iter().map(|&i| self.engine.layers[i].id).collect();
                    let anchor = match drop {
                        Drop::Above(a) | Drop::Below(a) | Drop::Into(a) => a,
                    };
                    if !ids.contains(&anchor) {
                        self.undo.begin_group(format!("Move {} layers", ids.len()));
                        // The order that keeps them as they were: above an anchor,
                        // the top one first; below it or into a folder, the bottom one.
                        let order: Vec<u64> = match drop {
                            Drop::Above(_) => ids.iter().rev().copied().collect(),
                            _ => ids.clone(),
                        };
                        for id in order {
                            if let Some(i) = self.engine.index_of(id) {
                                self.engine.move_to(i, drop, &mut self.undo);
                            }
                        }
                        self.undo.end_group();
                        if let Some(&primary) = ids.last() {
                            self.selected = self.engine.index_of(primary).unwrap_or(self.selected);
                        }
                        self.multi = ids;
                    }
                }
                Action::Clear => self.engine.clear_layer(self.selected, &mut self.undo),
                // Shelf content added by clicking or dropping on a view goes
                // on top of the stack, outside any folder.
                Action::AddOnTop(item) => {
                    self.pending.push(Action::ShelfDrop { item, at: self.engine.layers.len(), parent: None });
                }
                Action::ShelfDrop { item, at, parent } => match item {
                    ShelfItem::Smart(i) => {
                        let smart = &presets::SMART_MATERIALS[i];
                        self.selected = add_smart_material(&mut self.engine, &mut self.undo, smart, at, parent);
                        self.target = StrokeTarget::Content;
                        self.status = format!("Added smart material '{}'.", smart.name);
                    }
                    ShelfItem::Material(i) => {
                        let m = &presets::MATERIALS[i];
                        self.selected = self.engine.add_fill_layer(at, parent, m.name.to_string(), m.fill(), &mut self.undo);
                        self.target = StrokeTarget::Content;
                    }
                    ShelfItem::Generator(g) => self.pending.push(Action::AddGenerator(g)),
                    ShelfItem::Brush(i) => self.pending.push(Action::SetBrushTip(Some(i))),
                    ShelfItem::Texture(i) => self.add_texture_layer(i, at, parent),
                    ShelfItem::Graph(i) => {
                        if let Some(path) = self.graphs.shelf.get(i).map(|g| g.path.clone()) {
                            self.add_shelf_graph(&path, at, parent);
                        }
                    }
                    ShelfItem::Library(i) => self.add_library_material(i, at, parent),
                    ShelfItem::UserSmart(i) => {
                        if let Some(path) = self.user_smarts.get(i).map(|s| s.path.clone()) {
                            self.apply_smart_material(&path, at, parent);
                        }
                    }
                },
                Action::AddGraphLayer => self.add_graph_layer(None, at, parent),
                Action::AddGraphPreset(i) => {
                    if let Some(p) = crate::graph::presets::PRESETS.get(i) {
                        self.add_graph_layer(Some((p.build)()), at, parent);
                    }
                }
                Action::AddGraphMask => self.add_graph_mask(),
                Action::EditGraph => self.edit_selected_graph(),
                Action::GraphUndo => self.graph_undo(false),
                Action::GraphRedo => self.graph_undo(true),
                Action::GraphEditCode(id) => self.open_code(&id),
                Action::GraphCopyNode(id) => self.new_custom_node(Some(&id), true),
                Action::GraphRefresh(id) => self.refresh_graph(id),
                Action::GraphAddNode(def) => self.add_node_to_open_graph(&def),
                Action::SetBrushTip(tip) => {
                    self.brush.tip = tip;
                    if !matches!(self.tool, Tool::Paint | Tool::Eraser) {
                        self.tool = Tool::Paint;
                    }
                    let name = tip.map_or("Round", |i| crate::brushes::BRUSH_ALPHAS[i].name);
                    self.status = format!("Brush tip: {name}.");
                }
                Action::TextureToChannel { texture, ch } => {
                    let is_fill = self.engine.layers.get(self.selected).is_some_and(|l| l.is_fill());
                    if !is_fill {
                        self.status = "Select a fill layer to put the texture on.".into();
                    } else {
                        match self.texture_image(texture) {
                            Ok(img) => {
                                let idx = self.selected;
                                if let Content::Fill(f) = &mut self.engine.layers[idx].content {
                                    f.mat.enabled[ch] = true;
                                }
                                self.engine.set_fill_image(idx, ch, Some(img), &mut self.undo);
                            }
                            Err(e) => self.status = e,
                        }
                    }
                }
                Action::SetFillImage { idx, ch, path } => match path {
                    None => self.engine.set_fill_image(idx, ch, None, &mut self.undo),
                    Some(path) => {
                        let name = path.file_name().map_or("image".into(), |n| n.to_string_lossy().into_owned());
                        let loaded = std::fs::read(&path)
                            .map_err(|e| format!("can't read {}: {e}", path.display()))
                            .and_then(|bytes| self.engine.decode_fill_image(name, Arc::new(bytes)));
                        match loaded {
                            Ok(img) => self.engine.set_fill_image(idx, ch, Some(img), &mut self.undo),
                            Err(e) => self.status = e,
                        }
                    }
                },
                Action::MoveTo { from, drop } => {
                    if let Some(i) = self.engine.move_to(from, drop, &mut self.undo) {
                        self.selected = i;
                    }
                }
                Action::SetResolution(r) => {
                    self.engine.set_resolution(r, &self.gpu_mesh, &mut self.undo);
                    self.repush_graphs();
                    if self.engine.has_bake {
                        // Mesh maps are baked at the texture set size; refresh them.
                        self.start_bake(ctx);
                    }
                    self.status = format!("Texture set resolution: {r}x{r}.");
                }
                Action::Bake => self.start_bake(ctx),
                Action::BakeAll => self.start_bake_all(ctx),
            }
        }
    }

    /// Saves to the project file, asking for one if there is none (or `as_new`).
    /// Returns whether it saved.
    fn save(&mut self, as_new: bool) -> bool {
        let path = match (&self.project_path, as_new) {
            (Some(p), false) => Some(p.clone()),
            _ => self.save_dialog(),
        };
        let Some(path) = path else { return false };
        match self.save_project(&path) {
            Ok(()) => true,
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    /// New, open and exit, once unsaved changes were saved or discarded.
    fn run_unchecked(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::NewProject(p) => {
                // The first set is the one cleared below and kept for the new mesh.
                self.switch_set(0);
                while !self.engine.layers.is_empty() {
                    self.engine.delete_layer(0, &mut self.undo);
                }
                self.graphs.clear(&self.rs);
                if self.engine.res != self.prefs.default_resolution {
                    self.engine.set_resolution(self.prefs.default_resolution, &self.gpu_mesh, &mut self.undo);
                }
                self.selected = self.engine.add_paint_layer(0, None, None, &mut self.undo);
                self.undo.clear();
                self.saved_serial = self.undo.state_serial();
                self.project_path = None;
                self.target = StrokeTarget::Content;
                self.apply_project_defaults();
                self.load_mesh(p.build(), ctx);
            }
            Action::OpenSpp(path) => self.start_spp_open(path, ctx),
            Action::OpenProject(path) => {
                if let Some(path) = path.or_else(Self::open_dialog) {
                    if let Err(e) = self.open_project(&path, ctx) {
                        self.status = e;
                    }
                    self.target = StrokeTarget::Content;
                }
            }
            Action::Exit => {
                self.allow_close = true;
                project_io::remove_autosave();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            other => self.pending.push(other),
        }
    }

    /// Offers to restore work left in the autosave by a crash.
    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = &self.recovery else { return };
        let when = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        let ago = when.map_or(String::new(), |d| match d.as_secs() {
            s if s < 120 => " a moment ago".into(),
            s if s < 7200 => format!(" {} minutes ago", s / 60),
            s => format!(" {} hours ago", s / 3600),
        });
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("recover autosave")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Recover unsaved work?");
            ui.label(format!("Material Painter didn't close normally. An autosave was made{ago}."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Recover").clicked() {
                    choice = Some(true);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(false);
                }
            });
        });
        if let Some(recover) = choice {
            let path = self.recovery.take().expect("checked above");
            if recover {
                if let Err(e) = self.open_project(&path, ctx) {
                    self.status = e;
                }
            } else {
                project_io::remove_recovery(&path);
            }
        }
    }

    /// "Save changes?" before an action that would drop them.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = &self.confirm else { return };
        let what = match action {
            Action::Exit => "closing",
            Action::OpenProject(_) => "opening another project",
            Action::OpenSpp(_) => "opening a Substance 3D Painter project",
            _ => "starting a new project",
        };
        let name = self.project_path.as_ref().and_then(|p| p.file_name()).map_or("this project".to_string(), |n| n.to_string_lossy().into_owned());
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("unsaved changes")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Save changes?");
            ui.label(format!("{name} has unsaved changes. Save them before {what}?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    choice = Some(2);
                }
            });
        });
        if let Some(c) = choice {
            let action = self.confirm.take().expect("checked above");
            match c {
                0 => self.pending.push(Action::SaveThen(Box::new(action))),
                1 => self.pending.push(Action::Confirmed(Box::new(action))),
                _ => {}
            }
        }
    }

    /// Where a new layer goes: above the selected layer, next to it, or at
    /// the top of the selected folder.
    /// Where a new layer goes: into a folder only when that folder is
    /// selected; otherwise at the top level, above the selected layer (or
    /// above the folder holding it), or on top when nothing is selected.
    fn insert_place(&self) -> (usize, Option<u64>) {
        match self.engine.layers.get(self.selected) {
            None => (self.engine.layers.len(), None),
            Some(l) if l.is_folder() => (self.selected, Some(l.id)),
            Some(_) => {
                let top = self.engine.ancestors(self.selected).max().unwrap_or(self.selected);
                (top + 1, None)
            }
        }
    }

    /// Keeps the selection valid after undo/redo changed the layer stack.
    fn after_history_step(&mut self) {
        if self.selected != NO_LAYER {
            self.selected = self.selected.min(self.engine.layers.len().saturating_sub(1));
        }
        let has_mask = self.engine.layers.get(self.selected).is_some_and(|l| l.mask.is_some());
        if self.target == StrokeTarget::Mask && !has_mask {
            self.target = StrokeTarget::Content;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        ctx.input_mut(|i| {
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::E)) {
                self.export.open = true;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma)) {
                self.prefs_ui.open = !self.prefs_ui.open;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::S)) {
                self.pending.push(Action::SaveProjectAs);
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)) {
                self.pending.push(Action::SaveProject);
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::O)) {
                self.pending.push(Action::OpenProject(None));
            }
            // Over the graph editor, undo and redo act on the graph.
            let graph = self.graphs.editor.hovered;
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
            {
                self.pending.push(if graph { Action::GraphRedo } else { Action::Redo });
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)) {
                self.pending.push(if graph { Action::GraphUndo } else { Action::Undo });
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::G)) {
                if self.workspace == Workspace::Designer {
                    self.workspace = Workspace::Paint;
                } else {
                    self.pending.push(Action::EditGraph);
                }
            }
            // Ctrl+PageUp / PageDown step through the workspaces.
            let step = if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::PageDown)) {
                1
            } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::PageUp)) {
                Workspace::ALL.len() - 1
            } else {
                0
            };
            if step > 0 {
                let at = Workspace::ALL.iter().position(|w| *w == self.workspace).unwrap_or(0);
                self.workspace = Workspace::ALL[(at + step) % Workspace::ALL.len()];
            }
        });
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        // Layers: copy, cut, paste, duplicate and delete (the graph editor
        // has its own while the pointer is over it).
        if !self.graphs.editor.hovered {
            let (copy, cut, paste, dup, del) = ctx.input(|i| {
                let cmd = |k| i.modifiers.command && i.key_pressed(k);
                (
                    cmd(Key::C) || i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                    cmd(Key::X) || i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
                    cmd(Key::V) || i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                    cmd(Key::D),
                    i.key_pressed(Key::Delete) && !i.modifiers.any(),
                )
            });
            if copy || cut {
                self.pending.push(Action::CopyLayers);
                // Something on the system clipboard, so Ctrl+V comes back as a paste.
                ctx.copy_text("Material Painter layers".into());
            }
            if cut || del {
                self.pending.push(Action::Delete);
            }
            if paste {
                self.pending.push(Action::PasteLayers);
            }
            if dup {
                self.pending.push(Action::Duplicate);
            }
        }
        ctx.input(|i| {
            if i.modifiers.any() {
                return;
            }
            if i.key_pressed(Key::Num1) {
                self.tool = Tool::Paint;
            }
            if i.key_pressed(Key::Num2) {
                self.tool = Tool::Eraser;
            }
            if i.key_pressed(Key::OpenBracket) {
                self.brush.size = (self.brush.size / 1.15).max(2.0);
            }
            if i.key_pressed(Key::CloseBracket) {
                self.brush.size = (self.brush.size * 1.15).min(800.0);
            }
            if i.key_pressed(Key::I) {
                self.tool = Tool::Picker;
            }
            if i.key_pressed(Key::V) {
                self.tool = Tool::Select;
            }
            if i.key_pressed(Key::Escape) && self.tool == Tool::Select && self.decal_place.is_none() {
                self.deselect();
            }
            if i.key_pressed(Key::Num3) || i.key_pressed(Key::P) {
                self.tool = Tool::PolyFill;
            }
            if i.key_pressed(Key::Num4) || i.key_pressed(Key::L) {
                self.tool = Tool::Path;
            }
            if i.key_pressed(Key::W) {
                self.view.show_wire_3d = !self.view.show_wire_3d;
            }
            if i.key_pressed(Key::X) {
                self.brush.symmetry = if self.brush.symmetry == 1 { 0 } else { 1 };
            }
            if i.key_pressed(Key::F) {
                self.cam = OrbitCam { yaw: self.cam.yaw, pitch: self.cam.pitch, fov_y: self.cam.fov_y, ..OrbitCam::default() };
                self.cam2d.zoom = 0.0;
            }
            if i.key_pressed(Key::M) {
                self.view.mode = ViewMode::Material;
            }
            if i.key_pressed(Key::C) {
                let all = ViewMode::all();
                let pos = all.iter().position(|m| *m == self.view.mode).unwrap_or(0);
                self.view.mode = all[(pos + 1) % all.len()];
            }
            if i.key_pressed(Key::F1) {
                self.layout = Layout::Split;
            }
            if i.key_pressed(Key::F2) {
                self.layout = Layout::View3d;
            }
            if i.key_pressed(Key::F10) {
                self.rt.enabled = !self.rt.enabled;
            }
            if i.key_pressed(Key::F3) {
                self.layout = Layout::View2d;
            }
        });
    }

    // ------------------------------------------------------------------ panels

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new(theme::with_icon(icons::FOLDER_OPEN, "Open project…")).shortcut_text("Ctrl+O")).clicked() {
                    self.pending.push(Action::OpenProject(None));
                    ui.close();
                }
                let recent = self.recent.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for p in recent {
                            let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
                            if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                                self.pending.push(Action::OpenProject(Some(p)));
                                ui.close();
                            }
                        }
                    });
                });
                if ui.add(egui::Button::new(theme::with_icon(icons::FLOPPY_DISK, "Save project")).shortcut_text("Ctrl+S")).clicked() {
                    self.pending.push(Action::SaveProject);
                    ui.close();
                }
                if ui.add(egui::Button::new("Save project as…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    self.pending.push(Action::SaveProjectAs);
                    ui.close();
                }
                ui.separator();
                ui.menu_button("New project", |ui| {
                    for p in Primitive::ALL {
                        if ui.button(p.name()).clicked() {
                            self.pending.push(Action::NewProject(p));
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Swap preview mesh (keep layers)", |ui| {
                    for p in Primitive::ALL {
                        if ui.button(p.name()).clicked() {
                            self.pending.push(Action::ChangeMesh(p));
                            ui.close();
                        }
                    }
                });
                if ui.button(theme::with_icon(icons::CUBE, "Import mesh (FBX, OBJ)…")).clicked() {
                    self.pending.push(Action::ImportMesh);
                    ui.close();
                }
                let spp = ui
                    .add_enabled(!self.spp_busy(), egui::Button::new(theme::with_icon(icons::FOLDER_OPEN, "Open Substance 3D Painter project (.spp)…")))
                    .on_hover_text("Brings in the model, the final look of its color, roughness, metallic and height, and as much of the layer stack as maps onto Material Painter's. A report lists what came across and what didn't.");
                if spp.clicked() {
                    self.pending.push(Action::OpenSpp(None));
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new(theme::with_icon(icons::EXPORT, "Export textures…")).shortcut_text("Ctrl+Shift+E")).clicked() {
                    self.export.open = true;
                    ui.close();
                }
                ui.separator();
                if let Some(server) = &self.mcp {
                    let mut on = server.enabled.load(std::sync::atomic::Ordering::Relaxed);
                    if ui.checkbox(&mut on, "MCP server (AI agents)").on_hover_text(server.url()).changed() {
                        server.enabled.store(on, std::sync::atomic::Ordering::Relaxed);
                    }
                    ui.separator();
                }
                if ui.button("Exit").clicked() {
                    self.pending.push(Action::Exit);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                let undo_label = self.undo.undo.last().map_or("Undo".to_string(), |e| format!("Undo {}", e.label));
                let redo_label = self.undo.redo.last().map_or("Redo".to_string(), |e| format!("Redo {}", e.label));
                let can_undo = !self.undo.undo.is_empty();
                let can_redo = !self.undo.redo.is_empty();
                if ui.add_enabled(can_undo, egui::Button::new(theme::with_icon(icons::UNDO, &undo_label)).shortcut_text("Ctrl+Z")).clicked() {
                    self.pending.push(Action::Undo);
                    ui.close();
                }
                if ui.add_enabled(can_redo, egui::Button::new(theme::with_icon(icons::REDO, &redo_label)).shortcut_text("Ctrl+Y")).clicked() {
                    self.pending.push(Action::Redo);
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new(theme::with_icon(icons::GEAR, "Preferences…")).shortcut_text("Ctrl+,")).clicked() {
                    self.prefs_ui.open = true;
                    ui.close();
                }
            });
            ui.menu_button("Layer", |ui| {
                self.layer_menu_items(ui);
            });
            ui.menu_button("View", |ui| {
                if ui.button("Reset panel layout").on_hover_text("Puts this workspace's panels back where they started").clicked() {
                    self.docks.reset(self.workspace);
                    ui.close();
                }
                ui.separator();
                for (l, name, key) in [(Layout::Split, "3D / 2D", "F1"), (Layout::View3d, "3D only", "F2"), (Layout::View2d, "2D only", "F3")] {
                    if ui.add(theme::selectable(self.layout == l, name).shortcut_text(key)).clicked() {
                        self.layout = l;
                        ui.close();
                    }
                }
                ui.separator();
                for m in ViewMode::all() {
                    if ui.add(theme::selectable(self.view.mode == m, m.name())).clicked() {
                        self.view.mode = m;
                        ui.close();
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.view.show_wire_3d, "Wireframe (3D)  W");
                ui.checkbox(&mut self.view.show_wire, "UV wireframe (2D)");
                ui.checkbox(&mut self.view.show_seams_2d, "UV seams (2D)");
                ui.checkbox(&mut self.view.show_seams_3d, "UV seams (3D)");
                ui.horizontal(|ui| {
                    ui.label("UV wire opacity");
                    ui.add(egui::Slider::new(&mut self.view.uv_wire_opacity, 0.05..=1.0).max_decimals(2));
                });
                ui.add(egui::Checkbox::new(&mut self.rt.enabled, "Ray traced preview (F10)"));
                if ui.add(egui::Button::new("Frame mesh").shortcut_text("F")).clicked() {
                    self.cam = OrbitCam { fov_y: self.cam.fov_y, ..OrbitCam::default() };
                    self.cam2d.zoom = 0.0;
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button(theme::with_icon(icons::INFO, "Controls")).clicked() {
                    self.show_help = true;
                    ui.close();
                }
                if ui.button("Source code and releases").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(REPO_URL));
                    ui.close();
                }
                if ui.button(egui::RichText::new(theme::with_icon(icons::HEART, "Support development on Ko-fi")).color(theme::DONATE)).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(welcome::DONATE_URL));
                    ui.close();
                }
                if ui.button("Open crash reports folder").clicked() {
                    if let Some(dir) = project_io::data_dir() {
                        open_in_file_browser(&dir);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Welcome screen").clicked() {
                    self.show_welcome = true;
                    ui.close();
                }
                if ui.button("About Material Painter").clicked() {
                    self.show_about = true;
                    ui.close();
                }
            });
            // Workspace tabs.
            ui.add_space(24.0);
            ui.separator();
            for ws in Workspace::ALL {
                let resp = theme::pill(ui, &theme::with_icon(ws.icon(), ws.name()), self.workspace == ws);
                let keys = match ws {
                    Workspace::Designer => "Ctrl+G opens the selected layer's graph; Ctrl+PageUp / PageDown switch workspaces",
                    _ => "Ctrl+PageUp / PageDown switch workspaces",
                };
                if resp.on_hover_text(keys).clicked() {
                    self.workspace = ws;
                }
            }
        });
    }

    fn layer_menu_items(&mut self, ui: &mut egui::Ui) {
        let has = self.selected < self.engine.layers.len();
        let is_paint = has && self.engine.layers[self.selected].is_paint();
        let has_mask = has && self.engine.layers[self.selected].mask.is_some();
        let mut item = |ui: &mut egui::Ui, enabled: bool, text: &str, action: fn() -> Action| {
            if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                self.pending.push(action());
                ui.close();
            }
        };
        item(ui, true, "Add paint layer", || Action::AddPaint);
        item(ui, true, "Add fill layer", || Action::AddFill(FillParams::default(), "Fill layer".into()));
        item(ui, true, "Add graph fill layer", || Action::AddGraphLayer);
        item(ui, true, "Add decal (band, stripe)", || Action::AddDecal { text: false });
        item(ui, true, "Add text decal", || Action::AddDecal { text: true });
        item(ui, true, "Add folder", || Action::AddFolder);
        item(ui, has, "Group into folder", || Action::GroupIntoFolder);
        ui.separator();
        item(ui, has && !has_mask, "Add black mask", || Action::AddMask { white: false });
        item(ui, has && !has_mask, "Add white mask", || Action::AddMask { white: true });
        item(ui, has && !has_mask, "Add graph mask", || Action::AddGraphMask);
        item(ui, true, "Open graph in Material Designer (Ctrl+G)", || Action::EditGraph);
        item(ui, has_mask, "Remove mask", || Action::RemoveMask);
        ui.separator();
        item(ui, has, "Save as smart material", || Action::SaveSmart);
        item(ui, has, "Duplicate layer", || Action::Duplicate);
        item(ui, is_paint, "Clear layer", || Action::Clear);
        item(ui, has, "Delete layer", || Action::Delete);
    }

    fn context_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            // The right-hand controls go first; the tool controls get what's
            // left and clip instead of running under them on narrow windows.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let rt_btn = ui
                    .add(theme::selectable(self.rt.enabled, theme::with_icon(icons::SUN, "Ray trace")))
                    .on_hover_text("Path-traced preview of the 3D view (F10). Right-click for settings.");
                if rt_btn.clicked() {
                    self.rt.enabled = !self.rt.enabled;
                }
                rt_btn.context_menu(|ui| {
                    ui.set_min_width(220.0);
                    labeled(ui, "Max samples", |ui| ui.add(egui::Slider::new(&mut self.rt.max_samples, 16..=4096).logarithmic(true)));
                    labeled(ui, "Bounces", |ui| ui.add(egui::Slider::new(&mut self.rt.bounces, 1..=12)));
                    ui.checkbox(&mut self.rt.ground, "Ground plane with shadows");
                });
                if ui
                    .add(theme::selectable(self.view.show_wire, theme::with_icon(icons::GRID_FOUR, "UV wire")))
                    .on_hover_text("Show the UV wireframe in the 2D view")
                    .clicked()
                {
                    self.view.show_wire = !self.view.show_wire;
                }
                if ui
                    .add(theme::selectable(self.view.show_wire_3d, theme::with_icon(icons::CUBE_TRANSPARENT, "Wireframe")))
                    .on_hover_text("Show the mesh's edges in the 3D view (W)")
                    .clicked()
                {
                    self.view.show_wire_3d = !self.view.show_wire_3d;
                }
                if ui
                    .add(theme::selectable(self.view.show_seams_3d, theme::with_icon(icons::POLYGON, "Seams")))
                    .on_hover_text("UV seams over the 3D view")
                    .clicked()
                {
                    self.view.show_seams_3d = !self.view.show_seams_3d;
                }
                ui.separator();
                for (l, icon, tip) in [
                    (Layout::View2d, icons::SQUARE, "2D view only (F3)"),
                    (Layout::View3d, icons::CUBE, "3D view only (F2)"),
                    (Layout::Split, icons::COLUMNS, "3D and 2D side by side (F1)"),
                ] {
                    if theme::icon_button(ui, icon, self.layout == l, 24.0).on_hover_text(tip).clicked() {
                        self.layout = l;
                    }
                }
                ui.separator();
                egui::ComboBox::from_id_salt("viewmode")
                    .selected_text(self.mode_name(self.view.mode))
                    .width(130.0)
                    .height(600.0)
                    .show_ui(ui, |ui| {
                        ui.label(egui::RichText::new("MATERIAL").small().color(theme::TEXT_DIM));
                        for m in ViewMode::CHANNELS {
                            ui.selectable_value(&mut self.view.mode, m, m.name());
                        }
                        ui.separator();
                        ui.label(egui::RichText::new("MESH MAPS").small().color(theme::TEXT_DIM));
                        for m in ViewMode::all().into_iter().skip(ViewMode::CHANNELS.len()) {
                            ui.selectable_value(&mut self.view.mode, m, m.name());
                        }
                        if !self.bake_settings.custom.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("CUSTOM MAPS").small().color(theme::TEXT_DIM));
                        }
                        for c in &self.bake_settings.custom {
                            ui.selectable_value(&mut self.view.mode, ViewMode::Custom(c.id), &c.name);
                        }
                    })
                    .response
                    .on_hover_text("What the views show (M: material, C: next channel)");
                ui.label(egui::RichText::new("Display").color(theme::TEXT_DIM));
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    ui.spacing_mut().slider_width = 90.0;
                    let fix_name;
                    let name = match (self.tool, self.target) {
                        (Tool::Paint, StrokeTarget::BakeFix(m)) => {
                            fix_name = format!("Fix bake: {}", MAPS[m].name);
                            fix_name.as_str()
                        }
                        (Tool::Eraser, StrokeTarget::BakeFix(_)) => "Restore bake",
                        (Tool::Paint, StrokeTarget::Mask) => "Paint (mask)",
                        (Tool::Paint, _) => "Paint",
                        (Tool::Eraser, _) => "Eraser",
                        (Tool::Picker, _) => "Pick material (click the surface)",
                        (Tool::PolyFill, StrokeTarget::Mask) => "Polygon fill (mask)",
                        (Tool::PolyFill, _) => "Polygon fill",
                        (Tool::Path, StrokeTarget::Mask) => "Path (mask)",
                        (Tool::Path, _) => "Path",
                        (Tool::Select, _) => "Select (click a decal to select its layer)",
                    };
                    let icon = match self.tool {
                        Tool::Paint => icons::PAINT_BRUSH,
                        Tool::Eraser => icons::ERASER,
                        Tool::Picker => icons::EYEDROPPER,
                        Tool::PolyFill => icons::POLYGON,
                        Tool::Path => icons::PATH,
                        Tool::Select => icons::CROSSHAIR,
                    };
                    ui.label(egui::RichText::new(theme::with_icon(icon, name)).strong().color(Color32::WHITE));
                    ui.separator();
                    if self.tool == Tool::PolyFill {
                        for (mode, icon, tip) in [
                            (FillMode::Triangle, icons::TRIANGLE, "Fill one triangle"),
                            (FillMode::Polygon, icons::SQUARE, "Fill a polygon (a quad: two coplanar triangles)"),
                            (FillMode::Part, icons::CUBE, "Fill a loose part (a connected piece of one object)"),
                            (FillMode::Object, icons::CUBE_TRANSPARENT, "Fill a whole imported object"),
                            (FillMode::UvIsland, icons::GRID_FOUR, "Fill a whole UV island"),
                        ] {
                            let on = self.poly.mode == mode;
                            let text = egui::RichText::new(theme::with_icon(icon, mode.name())).color(if on { Color32::WHITE } else { theme::TEXT });
                            if ui.add(theme::selectable(on, text)).on_hover_text(tip).clicked() {
                                self.poly.mode = mode;
                            }
                        }
                        ui.separator();
                        let text = theme::with_icon(icons::ERASER, "Erase");
                        if ui.add(theme::selectable(self.poly.erase, text)).on_hover_text("Remove paint instead (or hold Ctrl while clicking)").clicked() {
                            self.poly.erase = !self.poly.erase;
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Opacity").color(theme::TEXT_DIM));
                        let mut p = self.brush.opacity * 100.0;
                        if ui.add(egui::Slider::new(&mut p, 1.0..=100.0).max_decimals(0).suffix("%").clamping(egui::SliderClamping::Edits)).changed() {
                            self.brush.opacity = p / 100.0;
                        }
                        ui.label(egui::RichText::new("Click or drag across faces, Shift+drag a box. Uses the brush material.").small().color(theme::TEXT_DIM));
                    } else if self.tool == Tool::Path {
                        self.path_toolbar(ui);
                    } else {
                    let s = |ui: &mut egui::Ui, label: &str, v: &mut f32, r: std::ops::RangeInclusive<f32>, pct: bool| {
                        ui.label(egui::RichText::new(label).color(theme::TEXT_DIM));
                        let resp = if pct {
                            let mut p = *v * 100.0;
                            let lo = *r.start() * 100.0;
                            let hi = *r.end() * 100.0;
                            let resp = ui.add(egui::Slider::new(&mut p, lo..=hi).max_decimals(0).suffix("%").clamping(egui::SliderClamping::Edits));
                            if resp.changed() {
                                *v = p / 100.0;
                            }
                            resp
                        } else {
                            ui.add(egui::Slider::new(v, r).max_decimals(0))
                        };
                        ui.add_space(6.0);
                        resp
                    };
                    s(ui, "Size", &mut self.brush.size, 2.0..=800.0, false)
                        .on_hover_text("Brush size ([ and ] to shrink / grow, Ctrl+right-drag in the view)");
                    s(ui, "Flow", &mut self.brush.flow, 0.01..=1.0, true);
                    s(ui, "Stroke opacity", &mut self.brush.opacity, 0.01..=1.0, true);
                    s(ui, "Hardness", &mut self.brush.hardness, 0.0..=0.99, true);
                    s(ui, "Spacing", &mut self.brush.spacing, 0.02..=3.0, true);
                    ui.label(egui::RichText::new("Symmetry").color(theme::TEXT_DIM));
                    egui::ComboBox::from_id_salt("symmetry")
                        .width(52.0)
                        .selected_text(["Off", "X", "Y", "Z"][self.brush.symmetry as usize])
                        .show_ui(ui, |ui| {
                            for (i, name) in ["Off", "X", "Y", "Z"].iter().enumerate() {
                                ui.selectable_value(&mut self.brush.symmetry, i as u8, *name);
                            }
                        })
                        .response
                        .on_hover_text("Mirror strokes across the mesh's center (X toggles)");
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Projection").color(theme::TEXT_DIM));
                    projection_combo(ui, "projection_bar", &mut self.brush.projection);
                    }
                });
            });
        });
    }

    fn tool_strip(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        for (tool, icon, tip) in [
            (Tool::Paint, icons::PAINT_BRUSH, "Paint (1)"),
            (Tool::Eraser, icons::ERASER, "Eraser (2)"),
            (Tool::PolyFill, icons::POLYGON, "Polygon fill: triangles, quads, mesh parts or UV islands (3 / P)"),
            (Tool::Path, icons::PATH, "Path: place points, then paint along them; stitches, seams, panel lines (4 / L)"),
            (Tool::Picker, icons::EYEDROPPER, "Pick material from the surface (I)"),
            (Tool::Select, icons::CROSSHAIR, "Select: click a decal or text in the 3D view to select its layer (V)"),
        ] {
            if theme::icon_button(ui, icon, self.tool == tool, 34.0).on_hover_text(tip).clicked() {
                self.tool = tool;
            }
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if theme::icon_button(ui, icons::CUBE_TRANSPARENT, self.view.show_wire_3d, 34.0).on_hover_text("Wireframe in the 3D view (W)").clicked() {
            self.view.show_wire_3d = !self.view.show_wire_3d;
        }
        if theme::icon_button(ui, icons::FIRE, self.show_bake, 34.0).on_hover_text("Bake mesh maps…").clicked() {
            self.show_bake = !self.show_bake;
        }
        if theme::icon_button(ui, icons::EXPORT, self.export.open, 34.0).on_hover_text("Export textures… (Ctrl+Shift+E)").clicked() {
            self.export.open = !self.export.open;
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new(&self.status).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{:.1} ms  │  VRAM {:.0} MB  │  Undo {:.1} MB RAM ({} steps)  │  {}²  │  {} tris",
                        self.frame_ms,
                        mb(self.engine.vram_bytes()),
                        mb(self.undo.ram_bytes() as u64),
                        self.undo.undo.len(),
                        self.engine.res,
                        self.mesh.triangle_count(),
                    ))
                    .color(theme::TEXT_DIM)
                    .small(),
                );
                if let Some(server) = &self.mcp {
                    if server.enabled.load(std::sync::atomic::Ordering::Relaxed) {
                        ui.label(egui::RichText::new(format!("MCP :{}", server.port)).color(theme::ACCENT).small())
                            .on_hover_text(format!("AI agents can connect at {}", server.url()));
                    }
                }
                if let Some((p, what)) = self.bake_progress() {
                    ui.add(egui::ProgressBar::new(p).desired_width(140.0).text(format!("{what} {:.0}%", p * 100.0)));
                }
            });
        });
    }

    // ------------------------------------------------------------------ layers

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        // Which texture set the stack below belongs to.
        if self.mesh.sets.len() > 1 {
            let mut pick = None;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Texture set").color(theme::TEXT_DIM));
                let current = self.mesh.sets[self.sets.active].name.clone();
                egui::ComboBox::from_id_salt("texture set")
                    .selected_text(current)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (k, set) in self.mesh.sets.iter().enumerate() {
                            if ui.selectable_label(k == self.sets.active, &set.name).clicked() {
                                pick = Some(k);
                            }
                        }
                    })
                    .response
                    .on_hover_text("Each material of the mesh has its own layers, size and mesh maps");
            });
            if let Some(k) = pick {
                self.switch_set(k);
            }
            ui.add_space(2.0);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mut btn = |ui: &mut egui::Ui, icon: &str, tip: &str, action: Option<Action>| {
                let enabled = action.is_some();
                let resp = ui.add_enabled_ui(enabled, |ui| theme::icon_button(ui, icon, false, 28.0)).inner;
                if resp.on_hover_text(tip).clicked() {
                    if let Some(a) = action {
                        self.pending.push(a);
                    }
                }
            };
            let has = self.selected < self.engine.layers.len();
            let no_mask = has && self.engine.layers[self.selected].mask.is_none();
            btn(ui, icons::PAINT_BRUSH, "Add paint layer", Some(Action::AddPaint));
            btn(ui, icons::PAINT_BUCKET, "Add fill layer", Some(Action::AddFill(FillParams::default(), "Fill layer".into())));
            btn(ui, icons::SPARKLE, "Add graph fill layer (a material graph)", Some(Action::AddGraphLayer));
            btn(ui, icons::FOLDER_PLUS, "Add folder (inside the selected folder)", Some(Action::AddFolder));
            btn(ui, icons::MASK, "Add black mask", no_mask.then_some(Action::AddMask { white: false }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                btn(ui, icons::TRASH, "Delete layer", has.then_some(Action::Delete));
                btn(ui, icons::COPY, "Duplicate layer", has.then_some(Action::Duplicate));
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for idx in (0..self.engine.layers.len()).rev() {
                let collapsed = self
                    .engine
                    .ancestors(idx)
                    .any(|a| matches!(self.engine.layers[a].content, Content::Folder(Folder { open: false })));
                if !collapsed {
                    self.layer_row(ui, idx);
                }
            }
            // Clicking the empty space below the layers selects none.
            let rest = ui.available_rect_before_wrap();
            if rest.height() > 2.0 && ui.allocate_rect(rest, Sense::click()).clicked() {
                self.selected = NO_LAYER;
                self.target = StrokeTarget::Content;
            }
            if self.engine.layers.is_empty() {
                ui.label(egui::RichText::new("No layers. Add a paint or fill layer, or drop a smart material from the shelf.").color(theme::TEXT_DIM));
            }
        });
    }

    fn layer_row(&mut self, ui: &mut egui::Ui, idx: usize) {
        let h = 46.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
        let depth = self.engine.depth(idx);
        let indent = depth as f32 * 20.0;
        let layer = &self.engine.layers[idx];
        let (id, visible, name) = (layer.id, layer.visible, layer.name.clone());
        let folder_open = match layer.content {
            Content::Folder(f) => Some(f.open),
            _ => None,
        };
        let in_multi = self.multi.len() > 1 && self.multi.contains(&id) && self.multi_active();
        let selected = idx == self.selected || in_multi;
        let base_id = ui.scope_id().with(("layer", id));

        let painter = ui.painter_at(rect);
        let bg = if selected {
            theme::SELECT_BG
        } else if resp.hovered() {
            theme::WIDGET
        } else {
            theme::PANEL_DARK
        };
        painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), theme::RADIUS, bg);
        if selected {
            painter.rect_stroke(rect.shrink2(vec2(0.0, 1.0)), theme::RADIUS, Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
        }
        if selected {
            painter.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, h - 12.0)), 1.5, theme::ACCENT);
        }

        // Mute (visibility) and solo, in a column of their own.
        let soloed = self.engine.solo == Some(id);
        let muted_by_solo = self.engine.solo.is_some() && !soloed;
        let eye_rect = Rect::from_center_size(pos2(rect.min.x + 16.0, rect.center().y), vec2(22.0, 22.0));
        let eye = ui.interact(eye_rect, base_id.with("eye"), Sense::click()).on_hover_text("Show / hide (Ctrl+click: solo)");
        let (eye_icon, eye_col) = if visible { (icons::EYE, theme::TEXT) } else { (icons::EYE_SLASH, theme::TEXT_DIM.gamma_multiply(0.6)) };
        let eye_col = if eye.hovered() {
            Color32::WHITE
        } else if muted_by_solo {
            eye_col.gamma_multiply(0.4)
        } else {
            eye_col
        };
        painter.text(eye_rect.center(), egui::Align2::CENTER_CENTER, eye_icon, egui::FontId::proportional(15.0), eye_col);
        let solo_rect = Rect::from_center_size(pos2(rect.min.x + 38.0, rect.center().y), vec2(18.0, 18.0));
        let solo = ui.interact(solo_rect, base_id.with("solo"), Sense::click()).on_hover_text("Solo: show only this layer (click again to end)");
        if soloed {
            painter.rect_filled(solo_rect, 3.0, Color32::from_rgb(0xe8, 0xb9, 0x4a));
        } else if solo.hovered() {
            painter.rect_stroke(solo_rect, 3.0, Stroke::new(1.0, theme::TEXT), egui::StrokeKind::Inside);
        }
        let solo_col = if soloed { Color32::BLACK } else if solo.hovered() { Color32::WHITE } else { theme::TEXT_DIM };
        painter.text(solo_rect.center(), egui::Align2::CENTER_CENTER, "S", egui::FontId::proportional(12.0), solo_col);
        // Tree lines: one per enclosing folder, an elbow into this row.
        let tree_x = |d: usize| rect.min.x + 52.0 + d as f32 * 20.0 + 8.0;
        for d in 0..depth {
            let x = tree_x(d);
            let line = Stroke::new(1.0, theme::TEXT_DIM.gamma_multiply(0.45));
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], line);
            if d + 1 == depth {
                painter.line_segment([pos2(x, rect.center().y), pos2(x + 10.0, rect.center().y)], line);
            }
        }

        // Content thumbnail, indented by folder depth.
        let thumb = Rect::from_min_size(pos2(rect.min.x + 52.0 + indent, rect.min.y + 5.0), vec2(36.0, 36.0));
        let thumb_resp = ui.interact(thumb, base_id.with("thumb"), Sense::click());
        match &layer.content {
            Content::Paint(_) => {
                paint_checker(&painter, thumb);
                if let Some(tid) = layer.thumb {
                    painter.image(tid, thumb, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
            Content::Fill(f) => match self.engine.content_thumb(id) {
                // The fill as it comes out on the texture set (images, graph, variation).
                Some(tid) => {
                    paint_checker(&painter, thumb);
                    painter.image(tid, thumb, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.rect_filled(thumb, 0.0, srgb_u8(f.mat.color));
                }
            },
            Content::Folder(f) => {
                // Folder icon, open or closed; clicking it toggles.
                let col = if visible { Color32::from_rgb(0xe8, 0xb9, 0x4a) } else { theme::TEXT_DIM };
                let icon = if f.open { icons::FOLDER_OPEN } else { icons::FOLDER };
                painter.text(thumb.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(24.0), col);
            }
        }
        // What kind of layer this is, as a badge on the thumbnail.
        let badge = match &layer.content {
            Content::Paint(_) => Some((icons::PAINT_BRUSH, "Paint layer")),
            Content::Fill(f) if f.projection == Projection::Decal && layer.images[0].as_ref().is_some_and(|im| im.text.is_some()) => {
                Some(("T", "Text decal"))
            }
            Content::Fill(f) if f.projection == Projection::Decal => Some((icons::SQUARE, "Decal")),
            Content::Fill(f) if f.emission_on => Some((icons::LIGHTNING, "Emissive fill")),
            Content::Fill(_) if layer.images.iter().any(Option::is_some) => Some((icons::IMAGES, "Image fill")),
            Content::Fill(f) if f.graph.is_some() => Some((icons::SPARKLE, "Graph fill")),
            Content::Fill(_) => Some((icons::PAINT_BUCKET, "Fill")),
            Content::Folder(_) => None,
        };
        if let Some((icon, what)) = badge {
            let b = Rect::from_min_size(thumb.right_bottom() - vec2(15.0, 15.0), vec2(15.0, 15.0));
            painter.rect_filled(b, 3.0, Color32::from_black_alpha(200));
            painter.text(b.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(11.0), Color32::WHITE);
            if thumb_resp.hovered() {
                thumb_resp.clone().on_hover_text(what);
            }
        }
        if selected && self.target == StrokeTarget::Content {
            painter.rect_stroke(thumb.expand(1.0), 0.0, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Outside);
        }

        // Mask thumbnail.
        let mut x = thumb.max.x + 6.0;
        let mut mask_resp = None;
        if let Some(mask) = &layer.mask {
            let mrect = Rect::from_min_size(pos2(x, rect.min.y + 5.0), vec2(36.0, 36.0));
            mask_resp = Some(ui.interact(mrect, base_id.with("mask"), Sense::click()));
            painter.rect_filled(mrect, 0.0, Color32::BLACK);
            match self.engine.mask_thumb(id) {
                // The mask as it comes out (painting, smart mask, layers, filters).
                Some(tid) => {
                    painter.image(tid, mrect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None if mask.paint.is_some() => {
                    painter.circle_filled(mrect.center(), 10.0, Color32::WHITE);
                }
                None => {}
            }
            // A smart mask says so in the corner.
            if mask.generator.is_some() || !mask.layers.is_empty() {
                let at = mrect.right_bottom() - vec2(7.0, 7.0);
                painter.circle_filled(at, 6.5, Color32::from_black_alpha(190));
                painter.text(at, egui::Align2::CENTER_CENTER, icons::SPARKLE, egui::FontId::proportional(9.5), theme::ACCENT);
            }
            if selected && self.target == StrokeTarget::Mask {
                painter.rect_stroke(mrect.expand(1.0), 0.0, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Outside);
            }
            x = mrect.max.x + 6.0;
        }

        // Name and blend info.
        let text_col = if visible { Color32::WHITE } else { theme::TEXT_DIM };
        let title = theme::elided(&painter, &name, egui::FontId::proportional(12.5), text_col, rect.max.x - x - 8.0);
        let name_cut = title.elided;
        painter.galley(pos2(x, rect.min.y + 13.0 - title.size().y * 0.5), title, text_col);
        let mb = self.engine.layer_vram_bytes(idx) as f64 / (1024.0 * 1024.0);
        let info = match &layer.content {
            Content::Folder(_) => {
                let inside = self.engine.block(idx).len() - 1;
                format!("Folder  ·  {inside} layers  ·  {:.0}%", layer.opacity * 100.0)
            }
            Content::Fill(_) => format!("Fill  {} {:.0}%  ·  {:.0} MB", layer.blend.short(), layer.opacity * 100.0, mb),
            Content::Paint(_) => format!("Paint  {} {:.0}%  ·  {:.0} MB", layer.blend.short(), layer.opacity * 100.0, mb),
        };
        painter.text(pos2(x, rect.min.y + 31.0), egui::Align2::LEFT_CENTER, info, egui::FontId::proportional(10.5), theme::TEXT_DIM);
        painter.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, theme::BORDER));
        let resp = if name_cut { resp.on_hover_text(&name) } else { resp };

        // Interaction.
        if eye.clicked() && ui.input(|i| i.modifiers.command) || solo.clicked() {
            self.engine.set_solo(if soloed { None } else { Some(id) });
        } else if eye.clicked() {
            // On a selected row, the eye shows or hides the whole selection.
            let targets = if in_multi { self.selection() } else { vec![idx] };
            for i in targets {
                self.engine.layers[i].visible = !visible;
            }
            self.engine.mark_dirty();
        } else if mask_resp.as_ref().is_some_and(|r| r.clicked()) {
            self.selected = idx;
            self.target = StrokeTarget::Mask;
        } else if thumb_resp.clicked() {
            if let Content::Folder(f) = &mut self.engine.layers[idx].content {
                f.open = !f.open;
            }
            self.selected = idx;
            self.target = StrokeTarget::Content;
        } else if resp.double_clicked() && folder_open.is_some() {
            if let Content::Folder(f) = &mut self.engine.layers[idx].content {
                f.open = !f.open;
            }
        } else if resp.clicked() {
            if self.selected != idx || matches!(self.target, StrokeTarget::BakeFix(_)) {
                self.target = StrokeTarget::Content;
            }
            let mods = ui.input(|i| i.modifiers);
            self.gizmo_off = None;
            if mods.command {
                // Ctrl: add or remove this layer.
                if !self.multi_active() {
                    self.multi = self.engine.layers.get(self.selected).map(|l| vec![l.id]).unwrap_or_default();
                }
                if let Some(k) = self.multi.iter().position(|&m| m == id) {
                    self.multi.remove(k);
                    if self.selected == idx {
                        if let Some(i) = self.multi.last().and_then(|&m| self.engine.index_of(m)) {
                            self.selected = i;
                        }
                    }
                } else {
                    self.multi.push(id);
                    self.selected = idx;
                }
                self.sel_anchor = Some(idx);
            } else if mods.shift {
                // Shift: everything between the last clicked layer and this one.
                let from = self.sel_anchor.unwrap_or(self.selected).min(self.engine.layers.len().saturating_sub(1));
                let (lo, hi) = (from.min(idx), from.max(idx));
                self.multi = (lo..=hi).map(|i| self.engine.layers[i].id).collect();
                self.selected = idx;
            } else {
                self.multi = vec![id];
                self.sel_anchor = Some(idx);
                self.selected = idx;
            }
        }

        // Drag to reorder: the top or bottom part of a row drops above or below
        // it; the middle of a folder (or anywhere below an open one) drops inside.
        resp.dnd_set_drag_payload(LayerDrag(idx));
        let drop_at = |y: f32| {
            let t = (y - rect.top()) / rect.height();
            match folder_open {
                Some(open) if t >= 0.25 && (open || t <= 0.75) => Drop::Into(id),
                _ if t < 0.5 => Drop::Above(id),
                _ => Drop::Below(id),
            }
        };
        if let (Some(from), Some(p)) = (resp.dnd_hover_payload::<LayerDrag>(), ui.ctx().pointer_interact_pos()) {
            if from.0 != idx {
                let accent = Stroke::new(2.0, theme::ACCENT);
                match drop_at(p.y) {
                    Drop::Above(_) => {
                        ui.painter().line_segment([rect.left_top(), rect.right_top()], accent);
                    }
                    Drop::Below(_) => {
                        ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], accent);
                    }
                    Drop::Into(_) => {
                        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, accent, egui::StrokeKind::Inside);
                    }
                }
            }
        }
        if let (Some(from), Some(p)) = (resp.dnd_release_payload::<LayerDrag>(), ui.ctx().pointer_interact_pos()) {
            if from.0 != idx {
                // Dragging a selected layer moves the whole selection.
                let dragged = self.engine.layers.get(from.0).map(|l| l.id);
                if self.multi_active() && self.multi.len() > 1 && dragged.is_some_and(|d| self.multi.contains(&d)) {
                    self.pending.push(Action::MoveSelection { drop: drop_at(p.y) });
                } else {
                    self.pending.push(Action::MoveTo { from: from.0, drop: drop_at(p.y) });
                }
            }
        }
        // Shelf items land where they're dropped; a generator masks the row's layer.
        if let (Some(item), Some(p)) = (resp.dnd_hover_payload::<ShelfItem>(), ui.ctx().pointer_interact_pos()) {
            let accent = Stroke::new(2.0, theme::ACCENT);
            match (*item, drop_at(p.y)) {
                (ShelfItem::Generator(_), _) | (_, Drop::Into(_)) => {
                    ui.painter().rect_stroke(rect.shrink(1.0), 0.0, accent, egui::StrokeKind::Inside);
                }
                (_, Drop::Above(_)) => {
                    ui.painter().line_segment([rect.left_top(), rect.right_top()], accent);
                }
                (_, Drop::Below(_)) => {
                    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], accent);
                }
            }
        }
        if let (Some(item), Some(p)) = (resp.dnd_release_payload::<ShelfItem>(), ui.ctx().pointer_interact_pos()) {
            if let ShelfItem::Generator(g) = *item {
                self.selected = idx;
                self.pending.push(Action::AddGenerator(g));
            } else {
                let row_parent = self.engine.layers[idx].parent;
                let (at, parent) = match drop_at(p.y) {
                    Drop::Above(_) => (idx + 1, row_parent),
                    Drop::Below(_) => (self.engine.block(idx).start, row_parent),
                    Drop::Into(_) => (idx, Some(id)),
                };
                self.pending.push(Action::ShelfDrop { item: *item, at, parent });
            }
        }
        resp.context_menu(|ui| {
            self.selected = idx;
            self.layer_menu_items(ui);
        });
    }

    fn texture_set_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.mesh.sets.len() > 1 {
                section(ui, "Texture sets");
                self.texture_set_list(ui);
            }
            section(ui, "Texture set");
            labeled(ui, "Mesh", |ui| ui.label(&self.mesh.name));
            let set = &self.mesh.sets[self.sets.active.min(self.mesh.sets.len() - 1)];
            if self.mesh.sets.len() > 1 {
                labeled(ui, "Material", |ui| ui.label(&set.name));
            }
            labeled(ui, "Triangles", |ui| ui.label((set.tris.end - set.tris.start).to_string()));
            let res = self.engine.res;
            labeled(ui, "Size", |ui| {
                egui::ComboBox::from_id_salt("res").selected_text(format!("{res} x {res}")).show_ui(ui, |ui| {
                    for r in RESOLUTIONS {
                        if ui.selectable_label(r == res, format!("{r} x {r}")).clicked() && r != res {
                            self.pending.push(Action::SetResolution(r));
                        }
                    }
                });
            });
            ui.label(egui::RichText::new("Changing size resamples every painted layer; stroke history is cleared.").small().color(theme::TEXT_DIM));

            section(ui, "Normal");
            slider(ui, "Height scale", &mut self.view.height_strength, 0.0..=8.0);

            section(ui, "Mesh maps");
            if ui.button("Open baker…").clicked() {
                self.show_bake = true;
            }
        });
    }

    // ------------------------------------------------------------------ preview

    /// Starts decoding an HDRI; it replaces the current one when ready.
    pub(crate) fn load_env(&mut self, source: EnvSource, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        self.status = format!("Loading environment '{}'…", source.name());
        std::thread::spawn(move || {
            let _ = tx.send(environment::decode(&source));
            ctx.request_repaint();
        });
        // A newer load replaces any running one; its result is dropped.
        self.env_load = Some(rx);
    }

    fn poll_hdri_thumbs(&mut self, ctx: &egui::Context) {
        if let Some((name, px)) = self.hdri_new_thumb.take() {
            let tex = ctx.load_texture(format!("hdri {name}"), egui::ColorImage::from_rgba_unmultiplied(HDRI_THUMB, &px), Default::default());
            self.hdri_thumbs.insert(name, tex);
        }
        let Some(rx) = &self.hdri_thumb_rx else { return };
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok((name, px)) => {
                    let tex = ctx.load_texture(format!("hdri {name}"), egui::ColorImage::from_rgba_unmultiplied(HDRI_THUMB, &px), Default::default());
                    self.hdri_thumbs.entry(name).or_insert(tex);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.hdri_thumb_rx = None;
        }
    }

    fn poll_env(&mut self) {
        let Some(rx) = &self.env_load else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("The environment loader stopped unexpectedly.".into()),
        };
        self.env_load = None;
        match result {
            Ok(img) => {
                // Its picture (a loaded file has none yet); made into a texture by poll_hdri_thumbs.
                self.hdri_new_thumb = Some((img.source.name(), img.thumbnail(HDRI_THUMB[0], HDRI_THUMB[1])));
                self.env = self.env_baker.upload(&self.rs, &img);
                self.status = format!("Environment: {}.", self.env.source.name());
                for w in self.mcp_env_waiters.drain(..) {
                    let _ = w.send(Ok(vec![crate::mcp::Content::Text(format!("Environment '{}' in use.", self.env.source.name()))]));
                }
            }
            Err(e) => {
                for w in self.mcp_env_waiters.drain(..) {
                    let _ = w.send(Err(e.clone()));
                }
                self.status = e;
            }
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section(ui, "Camera");
            let mut fov = self.cam.fov_y.to_degrees();
            if slider(ui, "Field of view", &mut fov, 10.0..=90.0) {
                self.cam.fov_y = fov.to_radians();
            }

            section(ui, "Shader");
            self.preview_shader_section(ui);
            let v = &mut self.view;

            section(ui, "Environment");
            let current = self.env_load.as_ref().map_or_else(|| self.env.source.name(), |_| "Loading…".to_string());
            let mut pick = None;
            // The HDRI in use; clicking it opens a scrolling grid to choose from.
            let full = ui.available_width();
            let (r, resp) = ui.allocate_exact_size(vec2(full, full * 0.5), Sense::click());
            match self.hdri_thumbs.get(&self.env.source.name()) {
                Some(t) => {
                    ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().rect_filled(r, 0.0, theme::PANEL_DARK);
                }
            }
            let edge = if resp.hovered() { Stroke::new(1.5, theme::ACCENT) } else { Stroke::new(1.0, theme::OUTLINE) };
            ui.painter().rect_stroke(r, 0.0, edge, egui::StrokeKind::Inside);
            ui.painter().text(
                r.right_bottom() - vec2(8.0, 8.0),
                egui::Align2::RIGHT_BOTTOM,
                "Change ▾",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            let resp = resp.on_hover_text("Choose an HDRI");
            ui.label(egui::RichText::new(current).strong().color(Color32::WHITE));
            egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_width(360.0);
                ui.label(egui::RichText::new("Environment").strong().color(Color32::WHITE));
                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        let w = 170.0;
                        for &i in &self.env_builtins {
                            let src = EnvSource::Builtin(i);
                            let name = environment::BUILTIN[i].0;
                            let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.5 + 18.0), Sense::click());
                            let img = Rect::from_min_size(rect.min, vec2(w, w * 0.5));
                            let p = ui.painter();
                            match self.hdri_thumbs.get(name) {
                                Some(t) => {
                                    p.image(t.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                None => {
                                    p.rect_filled(img, 0.0, theme::PANEL_DARK);
                                    p.text(img.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(14.0), theme::TEXT_DIM);
                                }
                            }
                            let selected = self.env.source == src;
                            let stroke = if selected {
                                Stroke::new(2.0, theme::ACCENT)
                            } else if resp.hovered() {
                                Stroke::new(1.0, Color32::from_gray(120))
                            } else {
                                Stroke::new(1.0, theme::OUTLINE)
                            };
                            p.rect_stroke(img, 0.0, stroke, egui::StrokeKind::Inside);
                            p.text(
                                pos2(rect.min.x + 2.0, img.max.y + 2.0),
                                egui::Align2::LEFT_TOP,
                                name,
                                egui::FontId::proportional(11.5),
                                if selected { Color32::WHITE } else { theme::TEXT_DIM },
                            );
                            if resp.clicked() {
                                pick = Some(src);
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                if ui.button("Load HDRI file…").on_hover_text("An equirectangular .hdr or .exr").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("HDRI", &["hdr", "exr"]).pick_file() {
                        pick = Some(EnvSource::File(path));
                    }
                    ui.close();
                }
            });
            let mut deg = v.env_rotation.to_degrees();
            if slider(ui, "Rotation", &mut deg, -180.0..=180.0) {
                v.env_rotation = deg.to_radians();
            }
            slider(ui, "Intensity", &mut v.env_intensity, 0.0..=4.0);
            labeled(ui, "Ambient occlusion", |ui| {
                ui.add(egui::Slider::new(&mut v.ao_strength, 0.0..=1.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)))
            })
            .on_hover_text("How much the baked ambient occlusion darkens crevices in the 3D view. Only the view: bakes and exports keep the full map.");

            section(ui, "Key light");
            ui.checkbox(&mut v.key.on, theme::with_icon(icons::SUN, "Directional light with shadows"))
                .on_hover_text("A sun-like light on top of the environment: crisp highlights and contact shadows. It turns with the environment rotation (Shift+right-drag).");
            if v.key.on {
                slider(ui, "Intensity", &mut v.key.intensity, 0.0..=8.0);
                slider(ui, "Direction", &mut v.key.azimuth, -180.0..=180.0);
                slider(ui, "Height", &mut v.key.elevation, 5.0..=90.0);
                labeled(ui, "Color", |ui| color_edit(ui, &mut v.key.color));
            }

            section(ui, "Background");
            labeled(ui, "Show", |ui| {
                egui::ComboBox::from_id_salt("background").selected_text(v.background.name()).show_ui(ui, |ui| {
                    for b in Background::ALL {
                        ui.selectable_value(&mut v.background, b, b.name());
                    }
                });
            });
            match v.background {
                Background::Environment => {
                    slider(ui, "Blur", &mut v.bg_blur, 0.0..=1.0);
                    slider(ui, "Brightness", &mut v.bg_intensity, 0.0..=2.0);
                }
                Background::Color => {
                    labeled(ui, "Color", |ui| color_edit(ui, &mut v.bg_color));
                }
                Background::Transparent => {}
            }

            section(ui, "Post processing");
            labeled(ui, "Exposure", |ui| {
                ui.add(egui::Slider::new(&mut v.exposure, 0.05..=8.0).logarithmic(true).max_decimals(2));
            });
            let p = &mut v.post;
            labeled(ui, "Tone mapping", |ui| {
                egui::ComboBox::from_id_salt("tonemap").selected_text(p.tonemap.name()).show_ui(ui, |ui| {
                    for t in Tonemap::ALL {
                        ui.selectable_value(&mut p.tonemap, t, t.name()).on_hover_text(t.hint());
                    }
                });
            });
            slider(ui, "Contrast", &mut p.contrast, 0.5..=1.5);
            slider(ui, "Saturation", &mut p.saturation, 0.0..=2.0);
            slider(ui, "Temperature", &mut p.temperature, -1.0..=1.0);
            slider(ui, "Tint", &mut p.tint, -1.0..=1.0);
            slider(ui, "Bloom", &mut p.bloom, 0.0..=1.0);
            if p.bloom > 0.0 {
                slider(ui, "Bloom threshold", &mut p.bloom_threshold, 0.0..=4.0);
            }
            slider(ui, "Vignette", &mut p.vignette, 0.0..=1.0);
            slider(ui, "Chromatic ab.", &mut p.chromatic, 0.0..=1.0);
            slider(ui, "Sharpen", &mut p.sharpen, 0.0..=1.0);
            slider(ui, "Film grain", &mut p.grain, 0.0..=1.0);
            ui.add_space(6.0);
            if ui.button("Reset preview settings").clicked() {
                let keep = (v.mode, v.height_strength, v.show_wire, v.use_baked_normal);
                *v = ViewSettings { mode: keep.0, height_strength: keep.1, show_wire: keep.2, use_baked_normal: keep.3, ..ViewSettings::default() };
                self.cam.fov_y = self.prefs.fov_deg.to_radians();
            }
            if let Some(src) = pick {
                self.load_env(src, ui.ctx());
            }
        });
    }

    // ------------------------------------------------------------------ mesh maps

    fn mesh_maps_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(egui::RichText::new("Bake mesh maps…").strong()).min_size(vec2(130.0, 24.0))).clicked() {
                self.show_bake = true;
            }
            if let Some((p, what)) = self.bake_progress() {
                ui.add(egui::ProgressBar::new(p).text(format!("{what} {:.0}%", p * 100.0)));
            }
        });
        if let Some(r) = &self.engine.bake_report {
            let time = if r.from_cache {
                "loaded from the bake cache".to_string()
            } else {
                r.seconds().map_or("…".to_string(), |s| format!("{:.2}s GPU + {:.2}s prep", s, r.prep_seconds))
            };
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} tris · {} parts · {} feature edges · {}² · {} rays/texel · {}",
                    r.source, r.triangles, r.parts, r.hard_edges, r.resolution, r.rays_per_texel, time
                ))
                .small()
                .color(theme::TEXT_DIM),
            );
            if let Some(missed) = r.missed_percent() {
                let (col, text) = if missed > 0.05 {
                    (Color32::from_rgb(235, 120, 90), format!("⚠ {missed:.2}% of texels missed the bake source"))
                } else {
                    (Color32::from_rgb(120, 200, 120), "✔ No missed rays".to_string())
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(text).color(col));
                    if missed > 0.05 && ui.small_button("Show").clicked() {
                        self.view.mode = ViewMode::MissedRays;
                    }
                });
            }
        } else if self.bake_job.is_none() {
            ui.label(egui::RichText::new("No maps baked yet.").color(theme::TEXT_DIM));
        }
        ui.checkbox(&mut self.view.use_baked_normal, "Use baked normal in material");
        ui.separator();

        let fixing = match self.target {
            StrokeTarget::BakeFix(m) => Some(m),
            _ => None,
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, info) in MAPS.iter().enumerate() {
                let shown = self.view.mode == ViewMode::Map(i);
                let has_fix = self.engine.map_fix[i].is_some();
                let frame = egui::Frame::new()
                    .fill(if fixing == Some(i) { theme::ACCENT_DIM } else { theme::PANEL_DARK })
                    .inner_margin(egui::Margin::symmetric(6, 4));
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let eye = egui::RichText::new(if shown { "◉" } else { "○" }).color(if shown { theme::ACCENT } else { theme::TEXT_DIM });
                        if ui.add(egui::Button::new(eye).frame(false)).on_hover_text("Display this map").clicked() {
                            self.view.mode = if shown { ViewMode::Material } else { ViewMode::Map(i) };
                        }
                        ui.label(egui::RichText::new(info.name).strong()).on_hover_text(info.desc);
                        if has_fix {
                            ui.label(egui::RichText::new("fixed").small().color(theme::ACCENT));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if info.kind != MapKind::Info {
                                let label = if fixing == Some(i) { "Done" } else { "Fix" };
                                if ui
                                    .add_enabled(self.engine.has_bake, egui::Button::new(label))
                                    .on_hover_text("Paint corrections onto this map. The eraser restores the original bake.")
                                    .clicked()
                                {
                                    if fixing == Some(i) {
                                        self.target = StrokeTarget::Content;
                                    } else {
                                        self.target = StrokeTarget::BakeFix(i);
                                        self.view.mode = ViewMode::Map(i);
                                        self.tool = Tool::Paint;
                                    }
                                }
                                if has_fix && ui.small_button("✖").on_hover_text("Clear painted fixes").clicked() {
                                    self.engine.clear_map_fix(i, &mut self.undo);
                                }
                            }
                        });
                    });
                    if info.kind == MapKind::Scalar {
                        let lv = &mut self.engine.map_levels[i];
                        let mut changed = false;
                        egui::CollapsingHeader::new(egui::RichText::new("Levels").small())
                            .id_salt(("levels", i))
                            .default_open(*lv != Levels::default())
                            .show(ui, |ui| {
                                changed |= slider(ui, "Black", &mut lv.black, 0.0..=1.0);
                                changed |= slider(ui, "White", &mut lv.white, 0.0..=1.0);
                                changed |= slider(ui, "Gamma", &mut lv.gamma, 0.1..=4.0);
                                if ui.small_button("Reset").clicked() {
                                    *lv = Levels::default();
                                    changed = true;
                                }
                            });
                        if changed {
                            self.engine.map_levels_changed();
                        }
                    }
                });
                ui.add_space(2.0);
            }
            // The custom maps the bake made, by name.
            let baked: Vec<&bake::CustomMap> =
                self.bake_settings.custom.iter().filter(|c| self.engine.custom_index(c.id).is_some()).collect();
            if !baked.is_empty() {
                ui.label(egui::RichText::new("CUSTOM MAPS").small().color(theme::TEXT_DIM));
            }
            let mut show = None;
            for c in baked {
                let shown = self.view.mode == ViewMode::Custom(c.id);
                egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let eye = egui::RichText::new(if shown { "◉" } else { "○" }).color(if shown { theme::ACCENT } else { theme::TEXT_DIM });
                        if ui.add(egui::Button::new(eye).frame(false)).on_hover_text("Display this map").clicked() {
                            show = Some(if shown { ViewMode::Material } else { ViewMode::Custom(c.id) });
                        }
                        ui.label(egui::RichText::new(format!("{}  {}", bake_live::kind_icon(c.kind), c.name)).strong()).on_hover_text(c.kind.help());
                    });
                });
                ui.add_space(2.0);
            }
            if let Some(m) = show {
                self.view.mode = m;
            }
        });
    }

    fn bake_window(&mut self, ctx: &egui::Context) {
        const BAKE_WIDTH: f32 = 520.0;
        let mut open = self.show_bake;
        // Against the right edge, so the 3D view (and the gizmos in it) stays visible.
        let screen = ctx.content_rect();
        egui::Window::new("Bake Mesh Maps")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            // A fixed width (text wraps to it), so nothing inside can widen the window or
            // leave the bars above narrower than what's below them.
            .min_width(BAKE_WIDTH)
            .max_width(BAKE_WIDTH)
            .default_pos(egui::pos2((screen.max.x - BAKE_WIDTH - 30.0).max(screen.min.x), screen.min.y + 60.0))
            .show(ctx, |ui| {
            ui.set_width(BAKE_WIDTH);
            self.preview_bar(ui);
            // The settings scroll within the screen; the Bake button stays below them.
            let settings_height = (screen.height() - 260.0).max(200.0);
            egui::ScrollArea::vertical().max_height(settings_height).auto_shrink([false, true]).show(ui, |ui| {
            // Inside the scroll area too: text wraps here instead of running past the window.
            ui.set_max_width(BAKE_WIDTH - 14.0);
            let s = &mut self.bake_settings;
            section(ui, "Source");
            ui.horizontal(|ui| {
                if ui.radio(s.high_poly.is_none(), "Low poly only (geometry info)").clicked() {
                    s.high_poly = None;
                }
                if ui.radio(s.high_poly.is_some(), "High poly → low poly").clicked() && s.high_poly.is_none() {
                    s.high_poly = rfd::FileDialog::new().add_filter("Mesh (FBX, OBJ)", &crate::import::EXTENSIONS).pick_file();
                }
            });
            if let Some(path) = s.high_poly.clone() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(path.display().to_string()).small().color(theme::TEXT_DIM));
                    if ui.small_button("Change…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Mesh (FBX, OBJ)", &crate::import::EXTENSIONS).pick_file() {
                            s.high_poly = Some(p);
                        }
                    }
                });
                ui.label(
                    egui::RichText::new("The high poly must be in the same space as the low poly (no UVs needed).")
                        .small()
                        .color(theme::TEXT_DIM),
                );
                section(ui, "Projection");
                ui.checkbox(&mut s.match_by_name, "Match by mesh name")
                    .on_hover_text("Each low-poly part only receives detail from the high-poly part with the same name (_low / _high suffixes are ignored), so parts close together can't bleed into each other.");
                labeled(ui, "Cage mesh", |ui| {
                    match s.cage.clone() {
                        Some(p) => {
                            let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
                            ui.add(egui::Label::new(name).truncate()).on_hover_text(p.display().to_string());
                            if ui.small_button("✕").on_hover_text("Use the front/rear distances instead").clicked() {
                                s.cage = None;
                            }
                        }
                        None => {
                            if ui.button("Load cage…").on_hover_text("The low poly with its vertices pushed out to enclose the high poly (same topology)").clicked() {
                                s.cage = rfd::FileDialog::new().add_filter("Mesh (FBX, OBJ)", &crate::import::EXTENSIONS).pick_file();
                            }
                        }
                    }
                });
                if s.cage.is_none() {
                    slider(ui, "Front", &mut s.front, 0.001..=0.5);
                } else {
                    ui.label(egui::RichText::new("Rays start on the cage and travel to the low poly.").small().color(theme::TEXT_DIM));
                }
                slider(ui, "Rear", &mut s.rear, 0.001..=0.5);
                let c = &mut self.cage_preview;
                ui.checkbox(&mut c.show, theme::with_icon(icons::CUBE_TRANSPARENT, "Show cage in the 3D view"))
                    .on_hover_text("The shell the bake rays start from, colored by what they find from each low-poly vertex.");
                if c.show {
                    if c.job.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Probing the high poly…").small().color(theme::TEXT_DIM));
                        });
                    } else if let Some((_, e)) = &c.failed {
                        ui.label(egui::RichText::new(e).small().color(Color32::from_rgb(255, 110, 90)));
                    } else if let Some(st) = c.stats {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            for (status, label, share) in [
                                (crate::cage::Status::Ok, "covered", st.ok),
                                (crate::cage::Status::Tight, "tight", st.tight),
                                (crate::cage::Status::Missed, "missed", st.missed),
                                (crate::cage::Status::Overlap, "overlapping", st.overlap),
                            ] {
                                let [r, g, b] = status.color();
                                ui.label(egui::RichText::new(format!("■ {label} {:.1}%", share * 100.0)).small().color(Color32::from_rgb(r, g, b)));
                            }
                        });
                    }
                }
                ui.label(egui::RichText::new("Red: the cage misses the high poly, raise Front/Rear. Amber: close to the limit. Magenta: another part is reached first and bleeds in; lower Front, load a cage or match by name.").small().color(theme::TEXT_DIM));
            }
            labeled(ui, "ID from", |ui| {
                ui.radio_value(&mut s.id_source, IdSource::MeshParts, "Mesh parts");
                ui.radio_value(&mut s.id_source, IdSource::Materials, "Materials");
                ui.radio_value(&mut s.id_source, IdSource::VertexColors, "Vertex colors");
            });

            section(ui, "Maps");
            ui.horizontal_wrapped(|ui| {
                for &m in crate::bake::OPTIONAL_MAPS.iter() {
                    let bit = 1u32 << m;
                    let mut on = s.maps & bit != 0;
                    if ui.checkbox(&mut on, MAPS[m].name).on_hover_text(MAPS[m].desc).changed() {
                        s.maps ^= bit;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("All").clicked() {
                    s.maps = crate::bake::ALL_MAPS;
                }
                if ui.small_button("None").clicked() {
                    s.maps = crate::bake::ALL_MAPS & !crate::bake::OPTIONAL_MAPS.iter().fold(0, |a, m| a | (1 << m));
                }
                ui.label(
                    egui::RichText::new("Maps left out bake faster and stay neutral; masks that read them show a warning. Normal, height, ID and part maps are always baked.")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            });

            self.custom_maps_section(ui);
            let s = &mut self.bake_settings;

            section(ui, "Quality");
            labeled(ui, "Preset", |ui| {
                egui::ComboBox::from_id_salt("bake quality").selected_text(s.quality.name()).show_ui(ui, |ui| {
                    for q in Quality::ALL {
                        if ui.selectable_label(s.quality == q, q.name()).clicked() {
                            s.apply_quality(q);
                        }
                    }
                });
            });
            labeled(ui, "Anti-aliasing", |ui| {
                ui.add(egui::Slider::new(&mut s.aa, 1..=8).custom_formatter(|v, _| format!("{0}x{0}", v as u32)))
            });
            labeled(ui, "AO rays", |ui| ui.add(egui::Slider::new(&mut s.ao_rays, 4..=512)));
            labeled(ui, "Thickness rays", |ui| ui.add(egui::Slider::new(&mut s.thickness_rays, 0..=256)));
            labeled(ui, "Sky rays", |ui| ui.add(egui::Slider::new(&mut s.sky_rays, 0..=256)));
            ui.label(
                egui::RichText::new(format!(
                    "{} rays per texel · {:.0}M rays at {}²",
                    s.total_rays_per_texel(),
                    s.total_rays_per_texel() as f64 * (self.engine.res as f64).powi(2) / 1e6,
                    self.engine.res
                ))
                .small()
                .color(theme::TEXT_DIM),
            );

            egui::CollapsingHeader::new("Map settings").default_open(false).show(ui, |ui| {
                slider(ui, "AO distance", &mut s.ao_distance, 0.02..=2.0);
                slider(ui, "Cavity distance", &mut s.cavity_distance, 0.005..=0.3);
                slider(ui, "Thickness dist.", &mut s.thickness_distance, 0.02..=2.0);
                slider(ui, "Sky cone (°)", &mut s.sky_cone_deg, 1.0..=89.0);
                slider(ui, "Curv. radius (px)", &mut s.curvature_radius, 1.0..=16.0);
                slider(ui, "Curv. contrast", &mut s.curvature_scale, 0.02..=2.0);
                slider(ui, "Edge sharpness", &mut s.edge_threshold, 0.05..=0.95);
                labeled(ui, "Edge search dist.", |ui| {
                    ui.checkbox(&mut s.edge_auto, "Auto")
                        .on_hover_text("Scale with the mesh: a tenth of its thinnest dimension.");
                    if s.edge_auto {
                        let auto = s.edge_distance_for(&self.mesh);
                        ui.label(egui::RichText::new(format!("{auto:.3}")).color(theme::TEXT_DIM));
                    } else {
                        ui.add(egui::Slider::new(&mut s.edge_max_distance, 0.005..=1.0).max_decimals(3));
                    }
                });
                slider(ui, "Hard edge angle (°)", &mut s.hard_edge_angle, 5.0..=120.0);
                slider(ui, "Split parts at folds (°)", &mut s.part_crease_angle, 0.0..=90.0);
                ui.label(egui::RichText::new("Distances are in mesh units; the mesh is normalized to a 2-unit box.").small().color(theme::TEXT_DIM));
            });

            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let busy = self.bake_job.is_some();
                let sets = self.mesh.sets.len();
                let label = if sets > 1 { format!("Bake {}", self.mesh.sets[self.sets.active].name) } else { "Bake".into() };
                let bake = egui::Button::new(egui::RichText::new(theme::with_icon(icons::FIRE, &label)).strong()).fill(theme::ACCENT_DIM).min_size(vec2(120.0, 30.0));
                if ui.add_enabled(!busy, bake).on_hover_text("Bake the texture set being painted").clicked() {
                    self.pending.push(Action::Bake);
                }
                if sets > 1 {
                    let all = egui::Button::new(format!("Bake all {sets} sets")).min_size(vec2(0.0, 30.0));
                    if ui.add_enabled(!busy, all).on_hover_text("Bake every texture set, one after another").clicked() {
                        self.pending.push(Action::BakeAll);
                    }
                }
                if let Some((p, what)) = self.bake_progress() {
                    let set = match self.mesh.sets.get(self.sets.baking) {
                        Some(s) if sets > 1 => format!(" {}", s.name),
                        _ => String::new(),
                    };
                    ui.add(egui::ProgressBar::new(p).text(format!("{what}{set} {:.0}%", p * 100.0)));
                } else if self.engine.has_bake {
                    ui.label(egui::RichText::new("Painted fixes and levels are kept when you re-bake.").small().color(theme::TEXT_DIM));
                }
            });
        });
        self.show_bake = open;
    }

    fn fix_properties(&mut self, ui: &mut egui::Ui, map: usize) {
        let info = &MAPS[map];
        section(ui, "Bake fix");
        ui.label(egui::RichText::new(format!("Painting corrections on {}.", info.name)).color(Color32::WHITE));
        ui.label(
            egui::RichText::new("Paint (1) sets the value below, Eraser (2) restores the original bake. Fixes survive re-baking.")
                .small()
                .color(theme::TEXT_DIM),
        );
        if info.kind == MapKind::Normal {
            ui.label(egui::RichText::new("Painting flattens the normal: use it to remove projection artifacts.").color(theme::TEXT));
        } else {
            ui.horizontal(|ui| {
                ui.label("Value");
                ui.add(egui::Slider::new(&mut self.brush.fix_value, 0.0..=1.0));
            });
            ui.horizontal(|ui| {
                for (label, v) in [("Black", 0.0), ("50%", 0.5), ("White", 1.0)] {
                    if ui.button(label).clicked() {
                        self.brush.fix_value = v;
                    }
                }
            });
        }
        if ui.button("Done fixing").clicked() {
            self.target = StrokeTarget::Content;
        }
        self.brush_properties(ui, false);
    }

    /// A smart mask's settings; `salt` keeps several apart on one panel.
    /// `custom` names the custom bake maps the mask can read.
    fn generator_ui(ui: &mut egui::Ui, g: &mut crate::engine::Generator, salt: usize, custom: &[bake::CustomMap]) -> bool {
        use crate::engine::gin;
        let mut dirty = false;
        labeled(ui, "Preset", |ui| {
            egui::ComboBox::from_id_salt(("gen", salt)).selected_text(g.kind.name()).show_ui(ui, |ui| {
                for k in GenKind::ALL {
                    if ui.selectable_label(g.kind == k, k.name()).clicked() && g.kind != k {
                        *g = new_generator(k, custom);
                        dirty = true;
                    }
                }
            });
        });
        if g.kind == GenKind::CustomMap {
            dirty |= custom_map_picker(ui, g, salt, custom);
        }
        dirty |= slider(ui, "Level", &mut g.level, -1.0..=2.0);
        dirty |= slider(ui, "Contrast", &mut g.contrast, 0.001..=0.6);
        dirty |= slider(ui, "Noise breakup", &mut g.noise, 0.0..=2.0);
        dirty |= labeled(ui, "Scale", |ui| {
            ui.add(egui::Slider::new(&mut g.scale, 0.2..=400.0).logarithmic(true).max_decimals(1))
                .on_hover_text("Size of the noise, scratches, cells and grain: higher is finer")
                .changed()
        });
        dirty |= slider(ui, "Seed", &mut g.seed, 0.0..=100.0);
        dirty |= labeled(ui, "Invert", |ui| ui.checkbox(&mut g.invert, "").changed());
        let uses_width = g.weights[gin::EDGE] != 0.0 || g.weights[gin::CREASE] != 0.0;
        if uses_width {
            dirty |= slider(ui, "Edge width", &mut g.edge_width, 0.002..=0.5);
        }
        if g.weights[gin::ID] != 0.0 {
            dirty |= labeled(ui, "ID color", |ui| color_edit(ui, &mut g.id_color));
            dirty |= slider(ui, "ID tolerance", &mut g.id_tolerance, 0.01..=1.0);
        }
        dirty | generator_inputs_ui(ui, g, custom)
    }

    // ------------------------------------------------------------------ properties

    fn properties(&mut self, ui: &mut egui::Ui) {
        if let StrokeTarget::BakeFix(m) = self.target {
            ui.label(egui::RichText::new(format!("Properties – Fix · {}", MAPS[m].name)).strong().color(Color32::WHITE));
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.fix_properties(ui, m));
            return;
        }
        let title = match self.engine.layers.get(self.selected) {
            None => "Properties".to_string(),
            Some(l) if self.target == StrokeTarget::Mask => format!("Properties – Mask of {}", l.name),
            Some(l) if l.is_fill() => format!("Properties – Fill · {}", l.name),
            Some(l) => format!("Properties – Paint · {}", l.name),
        };
        ui.label(egui::RichText::new(title).strong().color(Color32::WHITE));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let idx = self.selected;
            if idx >= self.engine.layers.len() {
                ui.label(egui::RichText::new("Select a layer.").color(theme::TEXT_DIM));
                return;
            }
            let mut dirty = false;

            section(ui, "Layer");
            {
                let layer = &mut self.engine.layers[idx];
                labeled(ui, "Name", |ui| ui.text_edit_singleline(&mut layer.name));
                labeled(ui, "Blend mode", |ui| {
                    egui::ComboBox::from_id_salt("blend").selected_text(layer.blend.name()).show_ui(ui, |ui| {
                        for b in Blend::ALL {
                            dirty |= ui.selectable_value(&mut layer.blend, b, b.name()).changed();
                        }
                    });
                });
                // A percentage of the stored value: egui's default clamping would
                // round it to whole percent every frame (18.0000007 -> 18), see a
                // change each time and recomposite the stack on every frame.
                let mut op = layer.opacity * 100.0;
                if labeled(ui, "Opacity", |ui| ui.add(egui::Slider::new(&mut op, 0.0..=100.0).max_decimals(0).suffix("%").clamping(egui::SliderClamping::Edits)).changed()) {
                    layer.opacity = op / 100.0;
                    dirty = true;
                }
            }

            if self.target == StrokeTarget::Mask {
                dirty |= self.mask_properties(ui, idx);
                self.brush_properties(ui, false);
            } else {
                let is_fill = self.engine.layers[idx].is_fill();
                if is_fill {
                    let names: [Option<String>; crate::engine::IMAGE_SLOTS] =
                        std::array::from_fn(|i| self.engine.layers[idx].images[i].as_ref().map(|im| im.name.clone()));
                    let graph_name = match &self.engine.layers[idx].content {
                        Content::Fill(f) => f.graph.map(|g| self.graphs.doc(g).map_or("(missing)".to_string(), |d| d.graph.name.clone())),
                        _ => None,
                    };
                    if let Some(name) = &graph_name {
                        section(ui, "Material graph");
                        labeled(ui, "Graph", |ui| {
                            ui.label(egui::RichText::new(name).color(Color32::WHITE));
                            if ui.button("Edit graph").on_hover_text("Ctrl+G").clicked() {
                                self.pending.push(Action::EditGraph);
                            }
                        });
                        ui.label(egui::RichText::new("The graph's Output nodes fill the channels below; uncheck a channel to leave it out.").small().color(theme::TEXT_DIM));
                        if let Some(gid) = match &self.engine.layers[idx].content {
                            Content::Fill(f) => f.graph,
                            _ => None,
                        } {
                            self.exposed_controls(ui, gid);
                        }
                    }
                    let text_info = self.engine.layers[idx]
                        .images[0]
                        .as_ref()
                        .map(|im| (im.text.as_deref().cloned(), im.tex.width as f32 / im.tex.height.max(1) as f32));
                    let layer_id = self.engine.layers[idx].id;
                    let placing = self.decal_place == Some(layer_id);
                    let runoff_source = match &self.engine.layers[idx].content {
                        Content::Fill(f) => f.runoff.and_then(|r| self.engine.index_of(r.source)).map(|i| self.engine.layers[i].name.clone()),
                        _ => None,
                    };
                    let mut ev = DecalEvents::default();
                    if let Content::Fill(f) = &mut self.engine.layers[idx].content {
                        section(ui, "Fill material");
                        dirty |= fill_controls(ui, f);
                        if let Some(r) = &mut f.runoff {
                            section(ui, "Run-off");
                            ui.label(
                                egui::RichText::new(format!("Runs down from \"{}\" and follows it.", runoff_source.as_deref().unwrap_or("(missing layer)")))
                                    .small()
                                    .color(theme::TEXT_DIM),
                            );
                            dirty |= slider(ui, "Length", &mut r.length, 0.1..=4.0);
                            dirty |= slider(ui, "Seed", &mut r.seed, 0.0..=100.0);
                        } else if f.projection == Projection::Decal {
                            section(ui, "Decal");
                            dirty |= decal_controls(ui, f, text_info.as_ref(), placing, &mut self.decal_align, &mut ev);
                        } else if ui
                            .button("Use as decal")
                            .on_hover_text("Show this layer only inside a box placed on the surface: bands, stripes, text, insignia")
                            .clicked()
                        {
                            f.projection = Projection::Decal;
                            ev.place = true;
                            dirty = true;
                        }
                        if graph_name.is_none() {
                        section(ui, "Images");
                        let shelf_textures = &self.shelf.textures;
                        for (ch, label) in ["Color", "Roughness", "Metallic", "Height", "Normal", "Emission"].into_iter().enumerate() {
                            labeled(ui, label, |ui| {
                                match &names[ch] {
                                    Some(name) => {
                                        ui.add(egui::Label::new(name.as_str()).truncate()).on_hover_text(name.as_str());
                                        if ui.small_button("✕").on_hover_text("Remove image").clicked() {
                                            self.pending.push(Action::SetFillImage { idx, ch, path: None });
                                        }
                                    }
                                    None => {
                                        if ui.button("Load image…").clicked()
                                            && let Some(path) = rfd::FileDialog::new().add_filter("Image", &IMAGE_EXT).pick_file()
                                        {
                                            self.pending.push(Action::SetFillImage { idx, ch, path: Some(path) });
                                        }
                                    }
                                }
                                // Any shelf texture can feed the channel.
                                ui.menu_button("Texture ▾", |ui| {
                                    ui.label(egui::RichText::new(format!("Shelf texture as {}", label.to_lowercase())).small().color(theme::TEXT_DIM));
                                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                                        ui.set_width(4.0 * 70.0);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                                            for (i, t) in crate::textures::TEXTURES.iter().enumerate() {
                                                let (r, resp) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::click());
                                                if let Some(h) = shelf_textures.get(i) {
                                                    ui.painter().image(h.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                                }
                                                let stroke = if resp.hovered() { Stroke::new(1.5, theme::ACCENT) } else { Stroke::new(1.0, theme::OUTLINE) };
                                                ui.painter().rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
                                                if resp.on_hover_text(format!("{} · {}", t.name, t.category)).clicked() {
                                                    self.pending.push(Action::TextureToChannel { texture: i, ch });
                                                    ui.close();
                                                }
                                            }
                                        });
                                    });
                                });
                            });
                        }
                        if names.iter().any(Option::is_some) {
                            labeled(ui, "Projection", |ui| {
                                egui::ComboBox::from_id_salt("projection").selected_text(f.projection.name()).show_ui(ui, |ui| {
                                    for p in Projection::ALL {
                                        dirty |= ui.selectable_value(&mut f.projection, p, p.name()).on_hover_text(p.tip()).changed();
                                    }
                                });
                            });
                            dirty |= labeled(ui, "Tiling", |ui| {
                                ui.add(egui::Slider::new(&mut f.tiling, 0.05..=64.0).logarithmic(true).max_decimals(2)).changed()
                            });
                            dirty |= slider(ui, "Rotation", &mut f.rotation, -180.0..=180.0);
                            dirty |= slider(ui, "Offset U", &mut f.offset[0], -1.0..=1.0);
                            dirty |= slider(ui, "Offset V", &mut f.offset[1], -1.0..=1.0);
                            if f.projection == Projection::Triplanar {
                                dirty |= slider(ui, "Blend sharpness", &mut f.tri_sharpness, 1.0..=16.0);
                            }
                        }
                        }
                        section(ui, "Procedural variation");
                        dirty |= slider(ui, "Color var.", &mut f.color_var, 0.0..=1.0);
                        dirty |= slider(ui, "Rough var.", &mut f.rough_var, 0.0..=1.0);
                        dirty |= slider(ui, "Height var.", &mut f.height_var, 0.0..=1.0);
                        dirty |= slider(ui, "Scale", &mut f.var_scale, 0.5..=60.0);
                        dirty |= slider(ui, "Seed", &mut f.seed, 0.0..=100.0);
                        dirty |= slider(ui, "Per-part var.", &mut f.part_var, 0.0..=1.0);
                    }
                    if ev.place {
                        self.decal_place = if placing { None } else { Some(layer_id) };
                    }
                    if ev.snap {
                        self.align_decal_to_surface(idx);
                    }
                    if ev.runoff {
                        self.pending.push(Action::AddRunoff { idx });
                    }
                    if let Some(spec) = ev.text {
                        self.pending.push(Action::SetText { idx, spec: Some(spec) });
                    }
                    if ev.remove_text {
                        self.pending.push(Action::SetText { idx, spec: None });
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Fill layers cost no texture memory. Add a mask to paint where they show.").small().color(theme::TEXT_DIM));
                } else {
                    self.brush_properties(ui, true);
                }
            }
            if dirty {
                // Only the selected layer changed: composite from it up (as a
                // draft while a slider or color is being dragged).
                if ui.input(|i| i.pointer.any_down()) {
                    self.engine.draft = true;
                }
                self.engine.mark_layer_dirty(self.selected);
            }
        });
    }

    /// A warning badge on a shelf swatch that reads maps the bake left out.
    fn missing_badge(ui: &egui::Ui, resp: &egui::Response, missing: u32) {
        if missing == 0 {
            return;
        }
        let at = resp.rect.right_top() + vec2(-8.0, 6.0);
        let r = Rect::from_center_size(at + vec2(-5.0, 6.0), vec2(16.0, 16.0));
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(230, 160, 40));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(12.0), Color32::BLACK);
        resp.clone().on_hover_text(format!("Needs maps that weren't baked: {}. Bake them for this to look right.", crate::engine::map_names(missing)));
    }

    /// A note under a mask that reads maps the bake left out.
    /// What a view mode is called; custom maps by their names.
    fn mode_name(&self, mode: ViewMode) -> String {
        match mode {
            ViewMode::Custom(id) => self.bake_settings.custom.iter().find(|c| c.id == id).map_or("Custom map".to_string(), |c| c.name.clone()),
            m => m.name().to_string(),
        }
    }

    fn missing_maps_note(&mut self, ui: &mut egui::Ui, idx: usize) {
        let Some(mask) = self.engine.layers.get(idx).and_then(|l| l.mask.as_ref()) else { return };
        let mut needs = mask.generator.map_or(0, |g| g.needs_maps());
        for ml in &mask.layers {
            if let crate::engine::MaskSource::Generator(g) = &ml.source {
                needs |= g.needs_maps();
            }
        }
        let missing = needs & self.engine.missing_maps();
        // Custom maps it reads that the bake didn't make (or that were removed).
        let mut gens: Vec<&crate::engine::Generator> = mask.generator.iter().collect();
        gens.extend(mask.layers.iter().filter_map(|ml| match &ml.source {
            crate::engine::MaskSource::Generator(g) => Some(g),
            _ => None,
        }));
        let mut custom: Vec<String> = gens
            .iter()
            .flat_map(|g| self.engine.missing_custom(g))
            .map(|id| self.bake_settings.custom.iter().find(|c| c.id == id).map_or("a removed custom map".to_string(), |c| c.name.clone()))
            .collect();
        custom.dedup();
        if missing == 0 && custom.is_empty() {
            return;
        }
        let mut names: Vec<String> = (missing != 0).then(|| crate::engine::map_names(missing)).into_iter().collect();
        names.extend(custom);
        egui::Frame::new().fill(Color32::from_rgb(60, 45, 20)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("This mask reads maps that weren't baked: {}.", names.join(", ")))
                    .color(Color32::from_rgb(240, 190, 90)),
            );
            if ui.button("Open the baker").clicked() {
                self.show_bake = true;
            }
        });
    }

    fn mask_properties(&mut self, ui: &mut egui::Ui, idx: usize) -> bool {
        let mut dirty = false;
        let custom = self.bake_settings.custom.clone();
        section(ui, "Mask");
        self.missing_maps_note(ui, idx);
        let Some(mask) = self.engine.layers[idx].mask.as_mut() else {
            ui.label("This layer has no mask.");
            return false;
        };
        if let Some(gid) = mask.graph {
            let name = self.graphs.doc(gid).map_or("(missing)".to_string(), |d| d.graph.name.clone());
            labeled(ui, "Graph", |ui| {
                ui.label(egui::RichText::new(name).color(Color32::WHITE));
                if ui.button("Edit graph").on_hover_text("Ctrl+G").clicked() {
                    self.pending.push(Action::EditGraph);
                }
            });
            ui.label(egui::RichText::new("A material graph renders this mask (its Output set to Mask). Painting on it is replaced when the graph changes.").small().color(theme::TEXT_DIM));
            self.exposed_controls(ui, gid);
            let layers = self.mask_layers_ui(ui, idx);
            return self.mask_filters_ui(ui, idx) | layers;
        }
        let has_paint = mask.paint.is_some();
        if let Some(g) = &mut mask.generator {
            dirty |= Self::generator_ui(ui, g, 0, &custom);
        }
        ui.horizontal(|ui| {
            if !has_paint && ui.button("Add paint to mask").clicked() {
                self.engine.ensure_mask_paint(idx);
            }
            if ui.button("Remove mask").clicked() {
                self.pending.push(Action::RemoveMask);
            }
        });
        if self.engine.layers[idx].mask.as_ref().is_some_and(|m| m.generator.is_some()) && !self.engine.has_bake {
            ui.label(egui::RichText::new("Mesh maps are still baking; edge and dirt masks refine when done.").small().color(theme::TEXT_DIM));
        }
        dirty |= self.mask_layers_ui(ui, idx);
        dirty |= self.mask_filters_ui(ui, idx);
        dirty
    }

    /// The mask's stack: generators or flat values blended over the base
    /// mask (painting × smart mask), bottom first.
    fn mask_layers_ui(&mut self, ui: &mut egui::Ui, idx: usize) -> bool {
        use crate::engine::{MaskBlend, MaskLayer, MaskSource};
        if self.engine.layers[idx].is_folder() {
            return false;
        }
        let Some(mask) = self.engine.layers[idx].mask.as_mut() else { return false };
        section(ui, "Mask layers");
        let mut dirty = false;
        let custom = self.bake_settings.custom.clone();
        let (mut remove, mut swap) = (None, None);
        let count = mask.layers.len();
        // Top of the stack first, like the layer list.
        for i in (0..count).rev() {
            let ml = &mut mask.layers[i];
            egui::Frame::new().fill(theme::PANEL_DARK).stroke(Stroke::new(1.0, theme::OUTLINE)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    dirty |= ui.checkbox(&mut ml.enabled, "").on_hover_text("Switch the mask layer on or off").changed();
                    let name = match &ml.source {
                        MaskSource::Generator(g) => g.kind.name(),
                        MaskSource::Fill(_) => "Fill",
                    };
                    ui.label(egui::RichText::new(name).strong().color(if ml.enabled { Color32::WHITE } else { theme::TEXT_DIM }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(icons::TRASH).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                        if ui.add_enabled(i + 1 < count, egui::Button::new("▲").small()).on_hover_text("Move up").clicked() {
                            swap = Some(i);
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("▼").small()).on_hover_text("Move down").clicked() {
                            swap = Some(i - 1);
                        }
                    });
                });
                ui.add_enabled_ui(ml.enabled, |ui| {
                    labeled(ui, "Blend", |ui| {
                        egui::ComboBox::from_id_salt(("mask blend", i)).selected_text(ml.blend.name()).show_ui(ui, |ui| {
                            for b in MaskBlend::ALL {
                                dirty |= ui.selectable_value(&mut ml.blend, b, b.name()).changed();
                            }
                        });
                    });
                    dirty |= slider(ui, "Opacity", &mut ml.opacity, 0.0..=1.0);
                    match &mut ml.source {
                        MaskSource::Generator(g) => dirty |= Self::generator_ui(ui, g, 100 + i, &custom),
                        MaskSource::Fill(v) => dirty |= slider(ui, "Value", v, 0.0..=1.0),
                    }
                });
            });
            ui.add_space(3.0);
        }
        if let Some(i) = remove {
            mask.layers.remove(i);
            dirty = true;
        }
        if let Some(i) = swap {
            mask.layers.swap(i, i + 1);
            dirty = true;
        }
        ui.menu_button(theme::with_icon(icons::STACK, "Add mask layer ▾"), |ui| {
            for k in GenKind::ALL {
                if ui.button(k.name()).clicked() {
                    mask.layers.push(MaskLayer { source: MaskSource::Generator(new_generator(k, &custom)), blend: MaskBlend::Multiply, opacity: 1.0, enabled: true });
                    dirty = true;
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Fill (one value)").clicked() {
                mask.layers.push(MaskLayer { source: MaskSource::Fill(1.0), blend: MaskBlend::Normal, opacity: 1.0, enabled: true });
                dirty = true;
                ui.close();
            }
        });
        if count == 0 {
            ui.label(egui::RichText::new("Stack more smart masks on this one: each blends over the mask below it (Multiply keeps only where both are, Add joins them, Subtract cuts one out).").small().color(theme::TEXT_DIM));
        }
        if dirty {
            self.engine.mark_dirty();
        }
        dirty
    }

    /// The mask's filter stack: add, reorder, switch off, remove and tune
    /// filters that apply after painting, smart mask and graph.
    fn mask_filters_ui(&mut self, ui: &mut egui::Ui, idx: usize) -> bool {
        use crate::engine::{FilterKind, MaskFilter};
        if self.engine.layers[idx].is_folder() {
            return false;
        }
        let Some(mask) = self.engine.layers[idx].mask.as_mut() else { return false };
        section(ui, "Mask filters");
        let mut dirty = false;
        let (mut remove, mut up) = (None, None);
        let count = mask.filters.len();
        for (i, f) in mask.filters.iter_mut().enumerate() {
            egui::Frame::new().fill(theme::PANEL_DARK).stroke(Stroke::new(1.0, theme::OUTLINE)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    dirty |= ui.checkbox(&mut f.enabled, "").on_hover_text("Switch the filter on or off").changed();
                    ui.label(egui::RichText::new(f.kind.name()).strong().color(if f.enabled { Color32::WHITE } else { theme::TEXT_DIM }))
                        .on_hover_text(f.kind.tip());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(icons::TRASH).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text("Apply earlier").clicked() {
                            up = Some(i);
                        }
                        if ui.add_enabled(i + 1 < count, egui::Button::new("▼").small()).on_hover_text("Apply later").clicked() {
                            up = Some(i + 1);
                        }
                    });
                });
                ui.add_enabled_ui(f.enabled, |ui| {
                    for (k, (name, lo, hi)) in f.kind.params().iter().enumerate() {
                        let v = match k {
                            0 => &mut f.a,
                            1 => &mut f.b,
                            _ => &mut f.c,
                        };
                        dirty |= slider(ui, name, v, *lo..=*hi);
                    }
                });
            });
            ui.add_space(3.0);
        }
        if let Some(i) = remove {
            mask.filters.remove(i);
            dirty = true;
        }
        if let Some(i) = up {
            mask.filters.swap(i - 1, i);
            dirty = true;
        }
        ui.menu_button(theme::with_icon(icons::MAGIC_WAND, "Add filter ▾"), |ui| {
            for k in FilterKind::ALL {
                if ui.button(k.name()).on_hover_text(k.tip()).clicked() {
                    mask.filters.push(MaskFilter::new(k));
                    dirty = true;
                    ui.close();
                }
            }
        });
        if mask.filters.is_empty() {
            ui.label(egui::RichText::new("Filters run in order on the finished mask: levels, blur, warp, noise breakup and more.").small().color(theme::TEXT_DIM));
        }
        if dirty {
            self.engine.mark_dirty();
        }
        dirty
    }

    /// The current tip with a grid of all tips to choose from.
    fn tip_picker(&mut self, ui: &mut egui::Ui) {
        let name = self.brush.tip.map_or("Round", |i| crate::brushes::BRUSH_ALPHAS[i].name);
        labeled(ui, "Tip", |ui| {
            let tex = self.brush.tip.map(|i| self.shelf.brushes[i].id());
            let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
            ui.painter().rect_filled(rect, theme::RADIUS, theme::PANEL_DARK);
            match tex {
                Some(t) => {
                    ui.painter().image(t, rect.shrink(3.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().circle_filled(rect.center(), 13.0, Color32::WHITE);
                }
            }
            let popup = egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
            ui.label(name);
            popup.show(|ui| {
                ui.set_max_width(300.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    let cell = |ui: &mut egui::Ui, tex: Option<egui::TextureId>, selected: bool, tip: &str| {
                        let (rect, resp) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::click());
                        let bg = if selected { theme::ACCENT_DIM } else if resp.hovered() { theme::WIDGET_HOVER } else { theme::PANEL_DARK };
                        ui.painter().rect_filled(rect, theme::RADIUS, bg);
                        match tex {
                            Some(t) => {
                                ui.painter().image(t, rect.shrink(4.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => {
                                ui.painter().circle_filled(rect.center(), 15.0, Color32::WHITE);
                            }
                        }
                        resp.on_hover_text(tip).clicked()
                    };
                    if cell(ui, None, self.brush.tip.is_none(), "Round") {
                        self.pending.push(Action::SetBrushTip(None));
                    }
                    for (i, b) in crate::brushes::BRUSH_ALPHAS.iter().enumerate() {
                        if cell(ui, Some(self.shelf.brushes[i].id()), self.brush.tip == Some(i), b.name) {
                            self.pending.push(Action::SetBrushTip(Some(i)));
                        }
                    }
                });
            });
        });
    }

    fn brush_properties(&mut self, ui: &mut egui::Ui, content: bool) {
        section(ui, if self.tool == Tool::Eraser { "Eraser" } else { "Brush" });
        slider(ui, "Size", &mut self.brush.size, 2.0..=800.0);
        slider(ui, "Hardness", &mut self.brush.hardness, 0.0..=0.99);
        slider(ui, "Flow", &mut self.brush.flow, 0.01..=1.0);
        slider(ui, "Stroke opacity", &mut self.brush.opacity, 0.01..=1.0);
        slider(ui, "Spacing", &mut self.brush.spacing, 0.02..=1.0);
        section(ui, "Brush tip");
        self.tip_picker(ui);
        if self.brush.tip.is_some() {
            slider(ui, "Angle", &mut self.brush.angle, -180.0..=180.0);
            labeled(ui, "Follow stroke", |ui| ui.checkbox(&mut self.brush.follow, "").on_hover_text("Turn the tip along the stroke direction"));
            slider(ui, "Angle jitter", &mut self.brush.angle_jitter, 0.0..=1.0);
        }
        slider(ui, "Size jitter", &mut self.brush.size_jitter, 0.0..=1.0);
        slider(ui, "Scatter", &mut self.brush.scatter, 0.0..=2.0);

        section(ui, "Projection");
        labeled(ui, "Mode", |ui| projection_combo(ui, "projection_props", &mut self.brush.projection));
        if self.brush.projection == BrushProjection::Screen {
            labeled(ui, "Paint through", |ui| {
                ui.checkbox(&mut self.brush.paint_through, "").on_hover_text("Also paint surfaces hidden behind others")
            });
        }
        labeled(ui, "Angle limit", |ui| {
            ui.add(egui::Slider::new(&mut self.brush.angle_limit, 10.0..=90.0).max_decimals(0).suffix("°"))
                .on_hover_text("Paint fades out on surfaces turned further than this from the brush, so it doesn't smear on slopes")
        });

        section(ui, "Stroke");
        labeled(ui, "Stabilizer", |ui| {
            ui.add(egui::Slider::new(&mut self.brush.stabilize, 0.0..=200.0).max_decimals(0))
                .on_hover_text("Lazy mouse: the brush trails the cursor on a string for smooth lines")
        });
        labeled(ui, "Pen pressure", |ui| {
            ui.checkbox(&mut self.brush.pressure_size, "Size");
            ui.checkbox(&mut self.brush.pressure_flow, "Flow");
        });
        ui.label(egui::RichText::new("Shift+click draws a straight line from the last stroke.").small().color(theme::TEXT_DIM));

        section(ui, "Grain");
        labeled(ui, "Texture", |ui| {
            let name = self.brush.grain.map_or("None", |i| crate::textures::TEXTURES[i].name);
            egui::ComboBox::from_id_salt("brush_grain").selected_text(name).height(400.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.brush.grain, None, "None");
                for (i, t) in crate::textures::TEXTURES.iter().enumerate() {
                    ui.selectable_value(&mut self.brush.grain, Some(i), t.name);
                }
            })
        });
        if self.brush.grain.is_some() {
            slider(ui, "Strength", &mut self.brush.grain_strength, 0.0..=1.0);
            labeled(ui, "Scale", |ui| {
                ui.add(egui::Slider::new(&mut self.brush.grain_scale, 0.25..=64.0).logarithmic(true).max_decimals(2))
                    .on_hover_text("Repeats across the mesh (or the UV square with the UV projection)")
            });
            labeled(ui, "Invert", |ui| ui.checkbox(&mut self.brush.grain_invert, ""));
        } else {
            ui.label(egui::RichText::new("A texture fixed to the surface that breaks up the paint: scratches, dust, chipping.").small().color(theme::TEXT_DIM));
        }

        if content {
            section(ui, if self.tool == Tool::Eraser { "Erase channels" } else { "Material" });
            material_controls(ui, &mut self.brush.mat, self.tool != Tool::Eraser);
            if self.tool != Tool::Eraser {
                ui.label(egui::RichText::new("Tip: right-click a material in the shelf → Use as brush material.").small().color(theme::TEXT_DIM));
            }
        } else if self.tool != Tool::Eraser && self.target == StrokeTarget::Mask {
            section(ui, "Mask value");
            ui.horizontal(|ui| {
                if ui.button("Black").clicked() {
                    self.brush.mask_value = 0.0;
                }
                if ui.button("White").clicked() {
                    self.brush.mask_value = 1.0;
                }
                ui.add(egui::Slider::new(&mut self.brush.mask_value, 0.0..=1.0));
            });
        }
    }

    // ------------------------------------------------------------------ shelf

    fn shelf_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("shelf_cats")
            .exact_size(176.0)
            .frame(egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::symmetric(8, 8)))
            .show(ui, |ui| {
                ui.label(egui::RichText::new("SHELF").small().color(theme::TEXT_DIM));
                ui.spacing_mut().item_spacing.y = 2.0;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (cat, icon, name, count) in [
                    (ShelfCat::SmartMaterials, icons::SPARKLE, "Smart materials", presets::SMART_MATERIALS.len() + self.user_smarts.len()),
                    (ShelfCat::Materials, icons::SPHERE, "Materials", presets::MATERIALS.len()),
                    (ShelfCat::Generators, icons::MAGIC_WAND, "Generators", GenKind::ALL.len()),
                    (ShelfCat::Brushes, icons::PAINT_BRUSH, "Brushes", crate::brushes::BRUSH_ALPHAS.len()),
                    (ShelfCat::Textures, icons::IMAGES, "Textures", crate::textures::TEXTURES.len()),
                    (ShelfCat::Graphs, icons::SPARKLE, "Graphs", self.graphs.shelf.len()),
                    (ShelfCat::Downloaded, icons::DOWNLOAD_SIMPLE, "Downloaded", self.library.local.len()),
                ] {
                    let on = self.shelf.cat == cat;
                    let text = egui::RichText::new(format!("{}  {name}", icon)).color(if on { Color32::WHITE } else { theme::TEXT });
                    let resp = ui.add_sized([158.0, 21.0], theme::selectable(on, text));
                    ui.painter().text(
                        resp.rect.right_center() - vec2(8.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        count.to_string(),
                        egui::FontId::proportional(10.0),
                        theme::TEXT_DIM,
                    );
                    if resp.clicked() {
                        self.shelf.cat = cat;
                    }
                }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(6, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let browse = egui::Button::new(egui::RichText::new(theme::with_icon(icons::GLOBE, "Get more assets")).color(Color32::WHITE))
                        .fill(theme::ACCENT.gamma_multiply(0.55));
                    if ui.add(browse).on_hover_text("Thousands of free materials from Poly Haven and ambientCG, and graphs shared by the community").clicked() {
                        self.library.open = true;
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.shelf.search).hint_text("Search…").desired_width(200.0));
                    let hint = match self.shelf.cat {
                        ShelfCat::SmartMaterials => "Double-click or drag onto a view to add a smart material (a folder of layers). Save your own: right-click a folder in Layers → Save as smart material.",
                        ShelfCat::Materials => "Double-click or drag onto a view to add as a fill layer. Right-click for more.",
                        ShelfCat::Generators => "Double-click or drag onto a view to mask the selected layer.",
                        ShelfCat::Brushes => "Double-click or drag onto a view to paint with this brush tip.",
                        ShelfCat::Textures => "Double-click or drag to add as a height fill layer. Right-click to put it on the selected fill layer.",
                        ShelfCat::Graphs => "Material graphs you saved or downloaded. Double-click or drag onto a view to add as a graph fill layer. Right-click to share.",
                        ShelfCat::Downloaded => "Free materials from the asset library. Double-click or drag onto a view to add as a fill layer.",
                    };
                    if matches!(self.shelf.cat, ShelfCat::SmartMaterials | ShelfCat::Materials | ShelfCat::Downloaded) {
                        let text = self.shelf.kind.map_or("All types", |k| k.name());
                        egui::ComboBox::from_id_salt("shelf type").selected_text(text).width(130.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.shelf.kind, None, "All types").on_hover_text("Everything, grouped by type");
                            for k in presets::MaterialType::ALL {
                                ui.selectable_value(&mut self.shelf.kind, Some(k), k.name());
                            }
                        });
                    }
                    // The size slider at the right edge first; the hint fits in what's left,
                    // shortened with an ellipsis (whole on hover) instead of running under it.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Slider::new(&mut self.shelf.icon_size, ICON_MIN..=ICON_MAX).show_value(false))
                            .on_hover_text("Thumbnail size (Ctrl+scroll over the shelf)");
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(egui::RichText::new(hint).small().color(theme::TEXT_DIM)).truncate()).on_hover_text(hint);
                        });
                    });
                });
                ui.add_space(2.0);
                // Ctrl+scroll arrives as zoom; it resizes the thumbnails.
                if ui.rect_contains_pointer(ui.max_rect()) {
                    let zoom = ui.input(|i| i.zoom_delta());
                    if zoom != 1.0 {
                        self.shelf.icon_size = (self.shelf.icon_size * zoom).clamp(ICON_MIN, ICON_MAX);
                    }
                }
                let icon = self.shelf.icon_size;
                let search = self.shelf.search.to_lowercase();
                let kind = self.shelf.kind;
                let wanted = |k: presets::MaterialType| kind.is_none_or(|f| f == k);
                // The type's name as a heading of its own when the type changes
                // (all types shown). It takes the full width, so the wrapped
                // layout puts it on a fresh row and starts the next items below it.
                let group = |ui: &mut egui::Ui, last: &mut Option<presets::MaterialType>, k: presets::MaterialType| {
                    if kind.is_none() && *last != Some(k) {
                        let width = ui.max_rect().width();
                        let h = if last.is_some() { 30.0 } else { 20.0 };
                        let (r, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
                        let base = r.left_bottom() + vec2(2.0, -5.0);
                        let g = ui.painter().layout_no_wrap(k.name().to_uppercase(), egui::FontId::new(11.0, theme::heading_family()), theme::TEXT_DIM);
                        let line_y = base.y - g.size().y * 0.5;
                        let text_w = g.size().x;
                        ui.painter().galley(pos2(base.x, base.y - g.size().y), g, theme::TEXT_DIM);
                        ui.painter().line_segment(
                            [pos2(base.x + text_w + 8.0, line_y), pos2(r.right() - 4.0, line_y)],
                            Stroke::new(1.0, theme::OUTLINE),
                        );
                        *last = Some(k);
                    }
                };
                let mut share = None;
                let mut share_smart = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        match self.shelf.cat {
                            ShelfCat::Graphs => {
                                if self.graphs.shelf.is_empty() {
                                    ui.label(
                                        egui::RichText::new("No saved graphs yet. In the Material Graph window (Ctrl+G), use Save to shelf.")
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                let fallback = self.shelf.materials.first().map(|t| t.id());
                                for (i, g) in self.graphs.shelf.iter().enumerate() {
                                    if !g.name.to_lowercase().contains(&search) {
                                        continue;
                                    }
                                    let Some(tex) = g.thumb.as_ref().map(|t| t.id()).or(fallback) else { continue };
                                    let resp = Self::swatch(ui, tex, &g.name, icon);
                                    resp.dnd_set_drag_payload(ShelfItem::Graph(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::Graph(i)));
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button(theme::with_icon(icons::SHARE_NETWORK, "Share to community…")).clicked() {
                                            share = Some(i);
                                            ui.close();
                                        }
                                    });
                                }
                            }
                            ShelfCat::Downloaded => {
                                self.library.ensure_local(ui.ctx());
                                if self.library.local.is_empty() {
                                    ui.label(
                                        egui::RichText::new("Nothing downloaded yet. Click Get more assets above for thousands of free materials.")
                                            .color(theme::TEXT_DIM),
                                    );
                                }
                                let fallback = self.shelf.materials.first().map(|t| t.id());
                                let kinds: Vec<presets::MaterialType> = self.library.local.iter().map(|m| presets::MaterialType::of(&m.name, None)).collect();
                                let mut order: Vec<usize> = (0..self.library.local.len())
                                    .filter(|&i| self.library.local[i].name.to_lowercase().contains(&search) && wanted(kinds[i]))
                                    .collect();
                                order.sort_by_key(|&i| kinds[i]);
                                let mut last = None;
                                for i in order {
                                    group(ui, &mut last, kinds[i]);
                                    let m = &self.library.local[i];
                                    let thumb = self.library.local_thumbs.get(i).and_then(|t| t.as_ref()).map(|t| t.id());
                                    let Some(tex) = thumb.or(fallback) else { continue };
                                    let resp = Self::swatch(ui, tex, &m.name.clone(), icon).on_hover_text(format!("{}, from {}", m.name, m.source));
                                    resp.dnd_set_drag_payload(ShelfItem::Library(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::Library(i)));
                                    }
                                    resp.context_menu(|ui| self.library_material_menu(ui, i));
                                }
                            }
                            ShelfCat::SmartMaterials => {
                                // Your own first, marked with a badge.
                                let fallback = self.shelf.smart.first().map(|t| t.id());
                                for i in 0..self.user_smarts.len() {
                                    let s = &self.user_smarts[i];
                                    if !s.name.to_lowercase().contains(&search) || !wanted(presets::MaterialType::of(&s.name, None)) {
                                        continue;
                                    }
                                    let Some(tex) = s.thumb.as_ref().map(|t| t.id()).or(fallback) else { continue };
                                    let resp = Self::swatch(ui, tex, &s.name.clone(), icon);
                                    let at = resp.rect.left_top() + vec2(8.0, 6.0);
                                    let g = ui.painter().layout_no_wrap("MINE".into(), egui::FontId::proportional(8.5), Color32::WHITE);
                                    let r = Rect::from_min_size(at, g.size() + vec2(6.0, 2.0));
                                    ui.painter().rect_filled(r, 0.0, theme::DONATE.gamma_multiply(0.9));
                                    ui.painter().galley(r.min + vec2(3.0, 1.0), g, Color32::WHITE);
                                    resp.dnd_set_drag_payload(ShelfItem::UserSmart(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::UserSmart(i)));
                                    }
                                    resp.context_menu(|ui| self.user_smart_menu(ui, i, &mut share_smart));
                                }
                                let mut order: Vec<usize> = (0..presets::SMART_MATERIALS.len())
                                    .filter(|&i| presets::SMART_MATERIALS[i].name.to_lowercase().contains(&search) && wanted(presets::SMART_MATERIALS[i].kind()))
                                    .collect();
                                order.sort_by_key(|&i| presets::SMART_MATERIALS[i].kind());
                                // Your own stay first, apart from the groups.
                                let mut last = None;
                                for (i, s) in order.into_iter().map(|i| (i, &presets::SMART_MATERIALS[i])) {
                                    group(ui, &mut last, s.kind());
                                    let tex = self.shelf.smart[i].id();
                                    let resp = Self::swatch(ui, tex, s.name, icon);
                                    if s.is_scanned() {
                                        // Badge: built on a photo-scanned texture set.
                                        let at = resp.rect.left_top() + vec2(8.0, 6.0);
                                        let font = egui::FontId::proportional(8.5);
                                        let g = ui.painter().layout_no_wrap("SCAN".into(), font, Color32::WHITE);
                                        let r = Rect::from_min_size(at, g.size() + vec2(6.0, 2.0));
                                        ui.painter().rect_filled(r, 0.0, theme::ACCENT.gamma_multiply(0.85));
                                        ui.painter().galley(r.min + vec2(3.0, 1.0), g, Color32::WHITE);
                                    }
                                    Self::missing_badge(ui, &resp, s.needs_maps() & self.engine.missing_maps());
                                    resp.dnd_set_drag_payload(ShelfItem::Smart(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::Smart(i)));
                                    }
                                }
                            }
                            ShelfCat::Materials => {
                                let mut order: Vec<usize> =
                                    (0..presets::MATERIALS.len()).filter(|&i| presets::MATERIALS[i].name.to_lowercase().contains(&search) && wanted(presets::MATERIALS[i].kind())).collect();
                                order.sort_by_key(|&i| presets::MATERIALS[i].kind());
                                let mut last = None;
                                for (i, m) in order.into_iter().map(|i| (i, &presets::MATERIALS[i])) {
                                    group(ui, &mut last, m.kind());
                                    let tex = self.shelf.materials[i].id();
                                    let resp = Self::swatch(ui, tex, m.name, icon);
                                    resp.dnd_set_drag_payload(ShelfItem::Material(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::Material(i)));
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button("Add as fill layer").clicked() {
                                            self.pending.push(Action::AddOnTop(ShelfItem::Material(i)));
                                            ui.close();
                                        }
                                        if ui.button("Use as brush material").clicked() {
                                            let f = m.fill();
                                            let keep_height = self.brush.mat.enabled[3];
                                            self.brush.mat = MatValues { enabled: [true, true, true, keep_height], height: self.brush.mat.height, ..f.mat };
                                            self.tool = Tool::Paint;
                                            ui.close();
                                        }
                                    });
                                }
                            }
                            ShelfCat::Brushes => {
                                for (i, b) in crate::brushes::BRUSH_ALPHAS.iter().enumerate() {
                                    if !b.name.to_lowercase().contains(&search) {
                                        continue;
                                    }
                                    let resp = Self::swatch(ui, self.shelf.brushes[i].id(), b.name, icon);
                                    if self.brush.tip == Some(i) {
                                        ui.painter().rect_stroke(resp.rect.shrink(2.0), theme::RADIUS, Stroke::new(1.5, theme::ACCENT), egui::StrokeKind::Inside);
                                    }
                                    resp.dnd_set_drag_payload(ShelfItem::Brush(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::SetBrushTip(Some(i)));
                                    }
                                }
                            }
                            ShelfCat::Textures => {
                                for (i, t) in crate::textures::TEXTURES.iter().enumerate() {
                                    if !t.name.to_lowercase().contains(&search) && !t.category.to_lowercase().contains(&search) {
                                        continue;
                                    }
                                    let resp = Self::swatch(ui, self.shelf.textures[i].id(), t.name, icon);
                                    resp.dnd_set_drag_payload(ShelfItem::Texture(i));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddOnTop(ShelfItem::Texture(i)));
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button("Add as height fill layer").clicked() {
                                            self.pending.push(Action::AddOnTop(ShelfItem::Texture(i)));
                                            ui.close();
                                        }
                                        ui.separator();
                                        for (ch, label) in [(3, "height"), (0, "color"), (1, "roughness"), (2, "metallic")] {
                                            if ui.button(format!("Use as {label} on the selected fill layer")).clicked() {
                                                self.pending.push(Action::TextureToChannel { texture: i, ch });
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                            }
                            ShelfCat::Generators => {
                                for (i, g) in GenKind::ALL.iter().enumerate() {
                                    if !g.name().to_lowercase().contains(&search) {
                                        continue;
                                    }
                                    let tex = self.shelf.generators[i].id();
                                    let resp = Self::swatch(ui, tex, g.name(), icon);
                                    Self::missing_badge(ui, &resp, g.default_generator().needs_maps() & self.engine.missing_maps());
                                    resp.dnd_set_drag_payload(ShelfItem::Generator(*g));
                                    if resp.double_clicked() {
                                        self.pending.push(Action::AddGenerator(*g));
                                    }
                                }
                            }
                        }
                    });
                });
                if let Some(i) = share {
                    self.share_graph(ui.ctx(), i);
                }
                if let Some(path) = share_smart {
                    self.share_smart(ui.ctx(), &path);
                }
            });
    }

    /// A shelf thumbnail `size` points wide with its name below; hovering
    /// shows it at twice the size.
    fn swatch(ui: &mut egui::Ui, tex: egui::TextureId, name: &str, size: f32) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(size + 12.0, size + 22.0), Sense::click_and_drag());
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 0.0, theme::WIDGET_HOVER);
        }
        let img = Rect::from_min_size(rect.min + vec2(6.0, 4.0), vec2(size, size));
        p.image(tex, img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // One line, cut with "…"; the hover shows the full name.
        let galley = theme::elided(p, name, egui::FontId::proportional(10.5), theme::TEXT, size + 8.0);
        let pos = pos2(rect.center().x - galley.size().x * 0.5, img.max.y + 3.0);
        p.galley(pos, galley, theme::TEXT);
        if resp.dragged() {
            // The thumbnail follows the pointer while it's dragged to a view.
            if let Some(p) = resp.ctx.pointer_interact_pos() {
                let ghost = Rect::from_center_size(p, vec2(size, size));
                let painter = resp.ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, resp.id.with("drag ghost")));
                painter.image(tex, ghost, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(220));
            }
            return resp;
        }
        resp.on_hover_ui(|ui| {
            ui.add(egui::Image::new((tex, vec2(size * 2.0, size * 2.0))));
            ui.label(name);
        })
    }

    // ------------------------------------------------------------------ viewports

    fn stamp_uniform(&self, kind: ViewKind) -> StampUniform {
        let ppp_radius = self.brush.size * 0.5 * self.ppp;
        let brush = [ppp_radius.max(0.5), self.brush.hardness.min(0.99), self.brush.flow, self.brush.symmetry as f32];
        let clip = [NEAR, FAR, if self.brush.tip.is_some() { 1.0 } else { 0.0 }, 0.0];
        let proj = [
            match self.brush.projection {
                BrushProjection::Screen => 0.0,
                BrushProjection::Surface => 1.0,
                BrushProjection::Uv => 2.0,
            },
            if self.brush.paint_through { 1.0 } else { 0.0 },
            self.brush.angle_limit.to_radians().cos(),
            0.0,
        ];
        match kind {
            ViewKind::V3d => {
                let [w, h] = self.views.view3d.size;
                let vp = self.cam.view_proj(w as f32 / h as f32);
                StampUniform {
                    view_proj: vp.to_cols_array_2d(),
                    cam_pos: self.cam.eye().extend(1.0).to_array(),
                    view: [w as f32, h as f32, 0.0, 0.0],
                    view2d: [0.0; 4],
                    brush,
                    clip,
                    proj,
                }
            }
            ViewKind::V2d => {
                let [w, h] = self.views.view2d.size;
                StampUniform {
                    view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                    cam_pos: [0.0; 4],
                    view: [w as f32, h as f32, 1.0, 0.0],
                    view2d: [self.cam2d.zoom, self.cam2d.pan.x, self.cam2d.pan.y, 0.0],
                    brush,
                    clip,
                    proj,
                }
            }
        }
    }

    // ------------------------------------------------------------------ shelf textures

    /// A shelf texture rendered at working resolution as a fill image.
    fn texture_image(&self, i: usize) -> Result<crate::engine::FillImage, String> {
        use image::ImageEncoder;
        const SIZE: u32 = 1024;
        let t = &crate::textures::TEXTURES[i];
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&t.render(SIZE), SIZE, SIZE, image::ExtendedColorType::L8)
            .map_err(|e| format!("can't encode {}: {e}", t.name))?;
        self.engine.decode_fill_image(format!("{}.png", t.name), Arc::new(png))
    }

    /// A fill layer whose height comes from a shelf texture, projected
    /// tri-planar so it tiles seamlessly on any mesh.
    fn add_texture_layer(&mut self, i: usize, at: usize, parent: Option<u64>) {
        let t = &crate::textures::TEXTURES[i];
        let img = match self.texture_image(i) {
            Ok(img) => img,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let fill = FillParams {
            mat: MatValues { enabled: [false, false, false, true], height: 0.5, ..FillParams::default().mat },
            projection: Projection::Triplanar,
            tiling: 2.0,
            ..FillParams::default()
        };
        self.undo.begin_group(format!("Add {}", t.name));
        let idx = self.engine.add_fill_layer(at, parent, t.name.to_string(), fill, &mut self.undo);
        self.engine.set_fill_image(idx, 3, Some(img), &mut self.undo);
        self.undo.end_group();
        self.selected = idx;
        self.target = StrokeTarget::Content;
        self.status = format!("Added '{}' as height. Change the channel, projection and tiling in the layer properties.", t.name);
    }

    /// Keeps the engine's brush tip texture in step with the brush.
    fn sync_brush_tip(&mut self) {
        if self.uploaded_tip == Some(self.brush.tip) {
            return;
        }
        match self.brush.tip {
            Some(i) => {
                const SIZE: u32 = 256;
                let px = crate::brushes::BRUSH_ALPHAS[i].render(SIZE);
                self.engine.set_brush_alpha(Some((&px, SIZE)));
            }
            None => self.engine.set_brush_alpha(None),
        }
        self.uploaded_tip = Some(self.brush.tip);
    }

    /// Keeps the engine's brush grain texture in step with the brush.
    fn sync_brush_grain(&mut self) {
        if self.uploaded_grain == Some(self.brush.grain) {
            return;
        }
        match self.brush.grain {
            Some(i) => {
                const SIZE: u32 = 512;
                let px = crate::textures::TEXTURES[i].render(SIZE);
                self.engine.set_brush_grain(Some((&px, SIZE)));
            }
            None => self.engine.set_brush_grain(None),
        }
        self.uploaded_grain = Some(self.brush.grain);
    }

    /// The brush grain as `BrushPaint::grain`.
    fn grain_params(&self) -> [f32; 4] {
        let b = &self.brush;
        if b.grain.is_none() {
            return [0.0; 4];
        }
        let uv = b.projection == BrushProjection::Uv;
        let (lo, hi) = self.engine.bounds();
        let tiling = if uv { b.grain_scale } else { b.grain_scale / (hi - lo).max_element().max(1e-6) };
        [b.grain_strength, tiling, if uv { 1.0 } else { 0.0 }, if b.grain_invert { 1.0 } else { 0.0 }]
    }

    /// Identifies a view's camera, so a line anchor goes stale when it moves.
    fn view_key(&self, kind: ViewKind) -> [f32; 8] {
        match kind {
            ViewKind::V3d => {
                let c = &self.cam;
                let [w, h] = self.views.view3d.size;
                [c.yaw, c.pitch, c.dist, c.target.x, c.target.y, c.target.z, c.fov_y, w as f32 / h.max(1) as f32]
            }
            ViewKind::V2d => {
                let c = &self.cam2d;
                let [w, h] = self.views.view2d.size;
                [c.zoom, c.pan.x, c.pan.y, w as f32, h as f32, 0.0, 0.0, 0.0]
            }
        }
    }

    /// One stamp at screen position `p`: tip angle (optionally along the
    /// stroke direction `dir`) plus the brush's size, angle and position
    /// jitter, and pen pressure. Returns the stamp and its flow scale.
    fn make_stamp(&mut self, p: Vec2, dir: Option<Vec2>, pressure: f32) -> ([f32; 4], f32) {
        let radius = self.brush.size * 0.5 * self.ppp;
        let b = &mut self.brush;
        let mut rnd = || {
            b.rng ^= b.rng << 13;
            b.rng ^= b.rng >> 17;
            b.rng ^= b.rng << 5;
            b.rng as f32 / u32::MAX as f32
        };
        let (r1, r2, r3, r4) = (rnd(), rnd(), rnd(), rnd());
        let scale = (1.0 - b.size_jitter * r1) * if b.pressure_size { pressure } else { 1.0 };
        let offset = Vec2::new(r2 - 0.5, r3 - 0.5) * 2.0 * b.scatter * radius;
        let mut angle = b.angle.to_radians() + (r4 - 0.5) * std::f32::consts::TAU * b.angle_jitter;
        if b.follow {
            if let Some(d) = dir {
                angle += d.y.atan2(d.x);
            }
        }
        ([p.x + offset.x, p.y + offset.y, scale, angle], if b.pressure_flow { pressure } else { 1.0 })
    }

    // ------------------------------------------------------------------ cage preview

    /// Keeps the cage overlay in step with the bake settings: probes the high
    /// poly on a worker thread when the inputs change, recolors when the
    /// distances change, and hides it with the bake window.
    fn update_cage_preview(&mut self, ctx: &egui::Context) {
        let c = &mut self.cage_preview;
        if let Some((key, rx)) = &c.job {
            match rx.try_recv() {
                Ok(Ok(probe)) => {
                    c.probe = Some(Arc::new(probe));
                    c.job = None;
                    c.drawn = None;
                }
                Ok(Err(e)) => {
                    c.failed = Some((key.clone(), e));
                    c.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => c.job = None,
            }
        }
        // A cage only exists for high-poly bakes.
        if !(self.show_bake && c.show && self.bake_settings.high_poly.is_some()) {
            if self.overlays.is_set(OverlaySlot::Cage) {
                self.overlays.clear(OverlaySlot::Cage);
                ctx.request_repaint();
            }
            c.drawn = None;
            return;
        }
        let key = ProbeKey::new(&self.mesh, &self.bake_settings);
        let current = c.probe.as_ref().is_some_and(|p| p.key == key);
        if !current {
            if self.overlays.is_set(OverlaySlot::Cage) {
                self.overlays.clear(OverlaySlot::Cage);
                ctx.request_repaint();
            }
            c.drawn = None;
            c.stats = None;
            let running = c.job.as_ref().is_some_and(|(k, _)| *k == key);
            let failed = c.failed.as_ref().is_some_and(|(k, _)| *k == key);
            if !running && !failed {
                c.failed = None;
                let (tx, rx) = mpsc::channel();
                let (mesh, k, ctx) = (self.mesh.clone(), key.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(CageProbe::build(&mesh, k));
                    ctx.request_repaint();
                });
                c.job = Some((key, rx));
            }
            return;
        }
        let distances = (self.bake_settings.front, self.bake_settings.rear);
        if c.drawn != Some(distances) {
            let probe = c.probe.as_ref().expect("current");
            let (tris, lines, stats) = probe.overlay(distances.0, distances.1);
            self.overlays.set(&self.rs.device, OverlaySlot::Cage, &tris, &lines);
            c.drawn = Some(distances);
            c.stats = probe.has_high.then_some(stats);
            ctx.request_repaint();
        }
    }

    // ------------------------------------------------------------------ polygon fill

    fn poly_topology(&mut self) -> Arc<Topology> {
        self.poly.topo.get_or_insert_with(|| Arc::new(Topology::build(&self.mesh))).clone()
    }

    /// Blinks where the selected layer (or a folder's layers) shows in the 3D
    /// view right after it's selected.
    fn update_highlight(&mut self, ctx: &egui::Context) {
        let id = self.engine.layers.get(self.selected).map(|l| l.id);
        if id != self.highlight.map(|(h, _)| h) {
            self.highlight = id.map(|i| (i, Instant::now()));
            if id.is_some() {
                self.engine.render_highlight(self.selected);
            }
        }
        let t = self.highlight.map_or(9.0, |(_, at)| at.elapsed().as_secs_f32());
        // A blink: full for a moment, gone a moment later.
        let k = (1.0 - (t - 0.12).max(0.0) / 0.18).clamp(0.0, 1.0);
        if (k - self.engine.highlight).abs() > 1e-3 {
            self.engine.highlight = k;
            ctx.request_repaint();
        }
    }

    /// Image files dragged from outside onto the window become decals in the
    /// 3D view: while they hover, a hint; once dropped, each lands on the
    /// surface under the pointer (or the middle of the view when the pointer
    /// doesn't show up in time).
    fn file_drop_in_view(&mut self, ctx: &egui::Context, rect: Rect, painter: &egui::Painter) {
        let is_image = |p: &std::path::Path| {
            p.extension().is_some_and(|e| ["png", "jpg", "jpeg", "tga", "bmp", "webp", "tif", "tiff"].contains(&e.to_string_lossy().to_lowercase().as_str()))
        };
        let (hovering, dropped) = ctx.input(|i| {
            (
                i.raw.hovered_files.iter().any(|f| f.path.as_deref().is_some_and(is_image)),
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| is_image(p)).collect::<Vec<_>>(),
            )
        });
        if hovering {
            painter.rect_stroke(rect.shrink(2.0), 0.0, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
            painter.text(rect.center_bottom() - vec2(0.0, 24.0), egui::Align2::CENTER_CENTER, "Drop the image to add it as a decal", egui::FontId::proportional(13.0), Color32::WHITE);
        }
        if !dropped.is_empty() {
            self.file_drop = Some((dropped, Instant::now()));
        }
        let Some((files, since)) = &self.file_drop else { return };
        let pointer = ctx.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p));
        let waited = since.elapsed().as_secs_f32();
        if pointer.is_none() && waited < 0.5 {
            ctx.request_repaint();
            return;
        }
        let files = files.clone();
        self.file_drop = None;
        let at = pointer.map(|p| Vec2::new((p.x - rect.min.x) * self.ppp, (p.y - rect.min.y) * self.ppp));
        for path in files {
            self.add_image_decal(&path, at);
        }
    }

    /// A decal layer showing an image file, placed on the surface under a 3D
    /// view pixel (or where the view looks), sized to the image's proportions.
    fn add_image_decal(&mut self, path: &std::path::Path, at: Option<Vec2>) {
        let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
        let (o, d) = match at {
            Some(a) => self.cam.ray(a, size),
            None => (self.cam.eye(), (self.cam.target - self.cam.eye()).normalize_or(Vec3::NEG_Z)),
        };
        let Some((pos, n)) = decal_hit(&self.mesh, &self.bvh, o, d) else {
            self.status = "Drop the image on the mesh to place it".into();
            return;
        };
        let img = match self.engine.shared_fill_image(path) {
            Ok(img) => img,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let aspect = img.tex.height as f32 / img.tex.width.max(1) as f32;
        let mut fill = FillParams { projection: Projection::Decal, ..FillParams::default() };
        fill.mat.enabled = [true, true, true, false];
        // White: the image's own colors show (the fill color tints them).
        fill.mat.color = [1.0, 1.0, 1.0];
        fill.mat.metal = 0.0;
        fill.mat.rough = 0.5;
        // A fifth of the distance to the camera across: a good size at any zoom.
        let width = ((self.cam.eye() - pos).length() * 0.2).clamp(0.01, 3.0);
        fill.decal.center = pos.to_array();
        fill.decal.normal = n.to_array();
        fill.decal.size = [width, width * aspect];
        fill.decal.depth = (width * 0.25).max(0.02);
        let name = path.file_stem().map_or("Decal".into(), |s| s.to_string_lossy().into_owned());
        let (at_idx, parent) = self.insert_place();
        self.undo.begin_group(format!("Add decal {name}"));
        let idx = self.engine.add_fill_layer(at_idx, parent, name.clone(), fill, &mut self.undo);
        self.engine.set_fill_image(idx, 0, Some(img), &mut self.undo);
        self.undo.end_group();
        self.selected = idx;
        self.multi.clear();
        self.gizmo_off = None;
        self.target = StrokeTarget::Content;
        self.status = format!("Added decal {name}: drag its handles to move, turn or scale it");
    }

    /// Deselects in the view: the multi-selection ends and the selected
    /// decal's gizmo hides (the properties still show the layer).
    fn deselect(&mut self) {
        self.multi.clear();
        self.gizmo_off = self.engine.layers.get(self.selected).map(|l| l.id);
        self.decal_drag = None;
        self.status = "Deselected".into();
    }

    /// The multi-selection still holds the selected layer (anything else that
    /// changed `selected` ends it).
    fn multi_active(&self) -> bool {
        self.engine.layers.get(self.selected).is_some_and(|l| self.multi.contains(&l.id))
    }

    /// The selected layers, bottom to top (just the selected one without a
    /// multi-selection). Layers inside a selected folder are left out: the
    /// folder carries them.
    fn selection(&self) -> Vec<usize> {
        if !self.multi_active() || self.multi.len() < 2 {
            return if self.selected < self.engine.layers.len() { vec![self.selected] } else { Vec::new() };
        }
        let mut v: Vec<usize> = self.multi.iter().filter_map(|&id| self.engine.index_of(id)).collect();
        v.sort_unstable();
        let set: std::collections::HashSet<usize> = v.iter().copied().collect();
        v.retain(|&i| !self.engine.ancestors(i).any(|a| set.contains(&a)));
        v
    }

    /// Selects the topmost visible decal layer covering the surface under a
    /// 3D view pixel.
    fn select_at(&mut self, at: Vec2) {
        let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
        let (o, d) = self.cam.ray(at, size);
        let Some((p, n)) = decal_hit(&self.mesh, &self.bvh, o, d) else { return };
        let hit = (0..self.engine.layers.len()).rev().find(|&i| {
            let l = &self.engine.layers[i];
            let visible = l.visible && self.engine.ancestors(i).all(|a| self.engine.layers[a].visible);
            match &l.content {
                Content::Fill(f) if visible && f.projection == Projection::Decal && f.runoff.is_none() => f.decal.covers(f.rotation, p, n),
                _ => false,
            }
        });
        match hit {
            Some(i) => {
                // Open the folders around it so its row shows.
                for a in self.engine.ancestors(i).collect::<Vec<_>>() {
                    if let Content::Folder(f) = &mut self.engine.layers[a].content {
                        f.open = true;
                    }
                }
                self.selected = i;
                self.multi.clear();
                self.gizmo_off = None;
                self.target = StrokeTarget::Content;
                self.status = format!("Selected {}", self.engine.layers[i].name);
            }
            None => self.deselect(),
        }
    }

    /// The decal layer being placed, while it still is one.
    fn placing_decal(&self) -> Option<usize> {
        let i = self.engine.index_of(self.decal_place?)?;
        match &self.engine.layers[i].content {
            Content::Fill(f) if f.projection == Projection::Decal => Some(i),
            _ => None,
        }
    }

    /// The triangle under a view pixel.
    fn pick_triangle(&mut self, kind: ViewKind, at: Vec2) -> Option<u32> {
        match kind {
            ViewKind::V3d => {
                let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
                let (o, d) = self.cam.ray(at, size);
                self.bvh.raycast(o, d).map(|h| h.tri)
            }
            ViewKind::V2d => {
                let size = self.views.view2d.size;
                let center = Vec2::new(size[0] as f32, size[1] as f32) * 0.5;
                let uv = (at - center - self.cam2d.pan) / self.cam2d.zoom.max(1.0) + Vec2::splat(0.5);
                self.poly_topology().pick_uv(uv)
            }
        }
    }

    /// Fills the faces the fill mode selects around each seed triangle,
    /// skipping any already filled during this drag.
    fn poly_fill(&mut self, seeds: &[u32]) {
        let topo = self.poly_topology();
        let mut tris = Vec::new();
        for &s in seeds {
            // A filled seed's whole selection is already filled.
            if !self.poly.filled.contains(&s) {
                tris.extend(topo.select(self.poly.mode, s).into_iter().filter(|t| self.poly.filled.insert(*t)));
            }
        }
        // Only the edited texture set takes paint.
        if let Some(set) = self.mesh.sets.get(self.sets.active) {
            tris.retain(|t| set.tris.contains(t));
        }
        if tris.is_empty() {
            return;
        }
        // Padding past UV island borders hides seams.
        let geometry = topo.uv_geometry(&tris, self.prefs.poly_padding / self.engine.res as f32);
        let mut mat = self.brush.mat;
        if let StrokeTarget::BakeFix(_) = self.target {
            mat.rough = self.brush.fix_value;
        }
        let paint = BrushPaint {
            mat,
            mask_value: self.brush.mask_value,
            opacity: self.brush.opacity,
            erase: self.poly.stroke_erase,
            grain: self.grain_params(),
        };
        self.engine.stroke_polys(&geometry, &paint);
    }

    /// Triangles whose centers fall inside a box of view pixels. In 3D only
    /// the ones the camera sees, as with a click.
    fn box_triangles(&mut self, kind: ViewKind, a: Vec2, b: Vec2) -> Vec<u32> {
        use rayon::prelude::*;
        let topo = self.poly_topology();
        let (lo, hi) = (a.min(b), a.max(b));
        let n = topo.triangle_count() as u32;
        match kind {
            ViewKind::V3d => {
                let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
                let vp = self.cam.view_proj(size.x / size.y);
                let eye = self.cam.eye();
                let bvh = &self.bvh;
                (0..n)
                    .into_par_iter()
                    .filter(|&t| {
                        let c = topo.centroid(t);
                        let q = vp * c.extend(1.0);
                        if q.w <= 0.0 {
                            return false;
                        }
                        let px = Vec2::new((q.x / q.w * 0.5 + 0.5) * size.x, (0.5 - q.y / q.w * 0.5) * size.y);
                        if px.cmplt(lo).any() || px.cmpgt(hi).any() {
                            return false;
                        }
                        let to = c - eye;
                        let dist = to.length();
                        bvh.raycast(eye, to / dist).is_none_or(|h| h.tri == t || h.pos.distance(eye) >= dist * 0.999)
                    })
                    .collect()
            }
            ViewKind::V2d => {
                let size = self.views.view2d.size;
                let center = Vec2::new(size[0] as f32, size[1] as f32) * 0.5;
                let to_uv = |p: Vec2| (p - center - self.cam2d.pan) / self.cam2d.zoom.max(1.0) + Vec2::splat(0.5);
                let (ulo, uhi) = (to_uv(lo), to_uv(hi));
                (0..n)
                    .into_par_iter()
                    .filter(|&t| {
                        let c = topo.uv_centroid(t);
                        c.cmpge(ulo).all() && c.cmple(uhi).all()
                    })
                    .collect()
            }
        }
    }

    /// Shows the faces the fill tool would fill under the cursor.
    fn update_poly_highlight(&mut self) {
        let hover = self.poly.frame_hover.take().filter(|_| self.tool == Tool::PolyFill);
        let erase = if self.stroke.is_some() { self.poly.stroke_erase } else { self.poly.erase };
        let want = hover.map(|t| (t, self.poly.mode, erase));
        if want == self.poly.shown {
            return;
        }
        self.poly.shown = want;
        match want {
            Some((t, mode, erase)) => {
                let topo = self.poly_topology();
                let color = if erase { [255, 90, 80] } else { [80, 160, 255] };
                let (fill, outline) = topo.overlay(&topo.select(mode, t), color);
                self.overlays.set(&self.rs.device, OverlaySlot::Selection, &fill, &outline);
            }
            None => self.overlays.clear(OverlaySlot::Selection),
        }
    }

    fn try_begin_stroke(&mut self, kind: ViewKind, at: Vec2) {
        let erase = self.tool == Tool::Eraser;
        if self.begin_stroke_on_target(kind, at, erase) {
            let stamp = self.make_stamp(at, None, self.pressure);
            self.paint_stamps(kind, &[stamp]);
        }
    }

    /// Starts a stroke on the current target (layer content, mask or bake
    /// fix), or says in the status bar why it can't.
    fn begin_stroke_on_target(&mut self, kind: ViewKind, at: Vec2, erase: bool) -> bool {
        let idx = self.selected;
        if let StrokeTarget::BakeFix(_) = self.target {
            if self.engine.begin_stroke(0, self.target, [false; 4], erase) {
                self.stroke = Some(StrokeInput { view: kind, last: at, lazy: at, pressure: self.pressure });
                return true;
            } else if !self.engine.has_bake {
                self.status = "Bake mesh maps before fixing them.".into();
            }
            return false;
        }
        let Some(layer) = self.engine.layers.get(idx) else {
            self.status = if self.engine.layers.is_empty() { "Add a layer to paint on." } else { "Select a layer to paint on." }.into();
            return false;
        };
        if self.target == StrokeTarget::Content && !layer.is_paint() {
            self.status = "Fill layers are procedural. Add a mask to paint on it, or select a paint layer.".into();
            return false;
        }
        if self.engine.begin_stroke(idx, self.target, self.brush.mat.enabled, erase) {
            self.stroke = Some(StrokeInput { view: kind, last: at, lazy: at, pressure: self.pressure });
            true
        } else {
            self.status = if erase {
                "Nothing to erase on the enabled channels.".into()
            } else {
                "Enable at least one channel in the brush material.".into()
            };
            false
        }
    }

    /// Eyedropper: copies the final color, roughness and metallic under the
    /// cursor into the brush, then goes back to painting.
    fn pick_material(&mut self, kind: ViewKind, at: Vec2) {
        let mut set = self.sets.active;
        let uv = match kind {
            ViewKind::V3d => {
                let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
                let (o, d) = self.cam.ray(at, size);
                let Some(hit) = self.bvh.raycast(o, d) else {
                    self.status = "Pick on the mesh.".into();
                    return;
                };
                let corner = |k: usize| Vec2::from_array(self.mesh.vertices[self.mesh.indices[hit.tri as usize * 3 + k] as usize].uv);
                set = self.mesh.set_of(hit.tri);
                corner(0) * hit.bary.x + corner(1) * hit.bary.y + corner(2) * hit.bary.z
            }
            ViewKind::V2d => {
                let size = self.views.view2d.size;
                let center = Vec2::new(size[0] as f32, size[1] as f32) * 0.5;
                (at - center - self.cam2d.pan) / self.cam2d.zoom.max(1.0) + Vec2::splat(0.5)
            }
        };
        if !(0.0..=1.0).contains(&uv.x) || !(0.0..=1.0).contains(&uv.y) {
            self.status = "Pick inside the texture.".into();
            return;
        }
        let engines = self.set_engines();
        let (color, rough, metal) = engines.get(set).unwrap_or(&&self.engine).sample_final(uv);
        self.brush.mat.color = color;
        self.brush.mat.rough = rough;
        self.brush.mat.metal = metal;
        self.tool = Tool::Paint;
        let hex = color.map(|c| (c * 255.0).round() as u8);
        self.status = format!(
            "Picked #{:02X}{:02X}{:02X}, roughness {:.2}, metallic {:.2}.",
            hex[0], hex[1], hex[2], rough, metal
        );
    }

    fn paint_stamps(&mut self, kind: ViewKind, raw: &[([f32; 4], f32)]) {
        let su = self.stamp_uniform(kind);
        let stamps = match kind {
            ViewKind::V3d => {
                let [w, h] = self.views.view3d.size;
                let radius = self.brush.size * 0.5 * self.ppp;
                resolve_stamps(&self.mesh, &self.bvh, &self.cam, Vec2::new(w as f32, h as f32), self.brush.projection, self.brush.symmetry, radius, raw)
            }
            // The 2D view is the texture: stamps land where they are drawn.
            ViewKind::V2d => resolve_stamps(&self.mesh, &self.bvh, &self.cam, Vec2::ONE, BrushProjection::Screen, 0, 0.0, raw),
        };
        let mut mat = self.brush.mat;
        if let StrokeTarget::BakeFix(_) = self.target {
            mat.rough = self.brush.fix_value;
        }
        let paint = BrushPaint {
            mat,
            mask_value: self.brush.mask_value,
            opacity: self.brush.opacity,
            erase: self.tool == Tool::Eraser,
            grain: self.grain_params(),
        };
        let region = match kind {
            ViewKind::V3d => {
                let [w, h] = self.views.view3d.size;
                let radius = self.brush.size * 0.5 * self.ppp;
                let size = Vec2::new(w as f32, h as f32);
                stroke_region(&self.mesh, &self.bvh, &self.cam, size, self.brush.projection, self.brush.symmetry, radius, &stamps)
            }
            ViewKind::V2d => {
                // The 2D view shows the texture: the stamps' own UV squares.
                let [w, h] = self.views.view2d.size;
                let center = Vec2::new(w as f32, h as f32) * 0.5;
                let zoom = self.cam2d.zoom.max(1.0);
                let radius = self.brush.size * 0.5 * self.ppp;
                stamps.iter().fold(None, |acc, s| {
                    let uv = (Vec2::new(s[0][0], s[0][1]) - center - self.cam2d.pan) / zoom + Vec2::splat(0.5);
                    let r = radius * s[0][2] / zoom;
                    Some(grow(acc, uv - Vec2::splat(r), uv + Vec2::splat(r)))
                })
            }
        };
        self.engine.stroke_step(&stamps, su, self.views.view3d.depth_view(), &paint, region);
    }

    fn continue_stroke(&mut self, at: Vec2) {
        let Some(s) = &mut self.stroke else { return };
        // Stabilizer: the brush follows the cursor on a string, only moving
        // once the cursor pulls the string tight.
        let string = self.brush.stabilize * self.ppp;
        let pull = at - s.lazy;
        if pull.length() > string {
            s.lazy += pull * (1.0 - string / pull.length());
        }
        let at = s.lazy;
        let pressure = self.pressure;
        let radius = self.brush.size * 0.5 * self.ppp * if self.brush.pressure_size { pressure.max(0.1) } else { 1.0 };
        let mut step = (self.brush.spacing * radius * 2.0).max(1.0);
        let delta = at - s.last;
        let dist = delta.length();
        if dist < step {
            return;
        }
        // Keep each frame's stamp list bounded on very fast strokes.
        let max = crate::engine::MAX_STAMPS as f32;
        if dist / step > max {
            step = dist / max;
        }
        let dir = delta / dist;
        let mut points = Vec::new();
        let mut travelled = step;
        let p0 = s.pressure;
        while travelled <= dist {
            // Pressure eases from the last stamp's to the current one.
            points.push((s.last + dir * travelled, p0 + (pressure - p0) * travelled / dist));
            travelled += step;
        }
        s.last += dir * (travelled - step);
        s.pressure = points.last().map_or(p0, |p| p.1);
        let kind = s.view;
        let stamps: Vec<([f32; 4], f32)> = points.into_iter().map(|(p, pr)| self.make_stamp(p, Some(dir), pr)).collect();
        self.paint_stamps(kind, &stamps);
    }

    fn end_stroke(&mut self) {
        if let Some(s) = self.stroke.take() {
            if matches!(self.tool, Tool::Paint | Tool::Eraser) {
                self.line_anchor = Some(LineAnchor { view: s.view, at: s.last, key: self.view_key(s.view) });
            }
            if let Some(entry) = self.engine.end_stroke() {
                self.undo.push("Paint stroke", crate::undo::Edit::Stroke(entry));
            }
        }
    }

    /// Scroll zoom per point of scrolling, with the user's speed and direction.
    fn zoom_rate(&self) -> f32 {
        0.0015 * self.prefs.zoom_speed * if self.prefs.invert_zoom { -1.0 } else { 1.0 }
    }

    fn view_input(&mut self, ui: &egui::Ui, kind: ViewKind, rect: Rect, resp: &egui::Response) {
        let (hover, pressed, down, mods, delta, scroll) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.modifiers,
                i.pointer.delta(),
                i.smooth_scroll_delta.y,
            )
        });
        let ppp = self.ppp;
        let local = |p: egui::Pos2| Vec2::new((p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp);
        use egui::PointerButton as B;

        // Pen pressure arrives with touch events; mice never send them.
        let touch = ui.input(|i| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::Touch { phase, force, .. } => Some((*phase, *force)),
                _ => None,
            })
        });
        match touch {
            Some((egui::TouchPhase::End | egui::TouchPhase::Cancel, _)) => self.pressure = 1.0,
            Some((_, Some(force))) => self.pressure = force.clamp(0.02, 1.0),
            _ => {}
        }

        // Navigation.
        match kind {
            ViewKind::V3d => {
                let orbit = (resp.dragged_by(B::Primary) && mods.alt)
                    || (resp.dragged_by(B::Secondary) && !mods.alt && !mods.shift && !mods.ctrl);
                if orbit {
                    let k = 0.008 * self.prefs.orbit_speed;
                    self.cam.yaw -= delta.x * k;
                    self.cam.pitch = (self.cam.pitch + delta.y * k).clamp(-1.55, 1.55);
                }
                if resp.dragged_by(B::Middle) {
                    let (right, up) = self.cam.basis();
                    let k = self.cam.dist * 0.0016;
                    self.cam.target += (-right * delta.x + up * delta.y) * k;
                }
                if resp.dragged_by(B::Secondary) && mods.alt {
                    self.cam.dist = (self.cam.dist * (delta.y * 0.01).exp()).clamp(0.3, 40.0);
                }
                if resp.dragged_by(B::Secondary) && mods.shift {
                    self.view.env_rotation += delta.x * 0.01;
                }
                if resp.dragged_by(B::Secondary) && mods.ctrl {
                    self.brush.size = (self.brush.size + delta.x).clamp(2.0, 800.0);
                    self.brush.hardness = (self.brush.hardness - delta.y * 0.005).clamp(0.0, 0.99);
                }
                if resp.hovered() && scroll != 0.0 && self.placing_decal().is_none() {
                    self.cam.dist = (self.cam.dist * (-scroll * self.zoom_rate()).exp()).clamp(0.3, 40.0);
                }
                if resp.double_clicked() && mods.alt {
                    if let Some(p) = hover {
                        let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
                        let (o, d) = self.cam.ray(local(p), size);
                        if let Some(hit) = self.bvh.raycast(o, d) {
                            self.cam.target = hit.pos;
                        }
                    }
                }
            }
            ViewKind::V2d => {
                if resp.dragged_by(B::Middle) || resp.dragged_by(B::Secondary) || (resp.dragged_by(B::Primary) && mods.alt) {
                    self.cam2d.pan += Vec2::new(delta.x, delta.y) * ppp;
                }
                if resp.hovered() && scroll != 0.0 {
                    if let Some(p) = hover {
                        // Zoom about the cursor.
                        let size = self.views.view2d.size;
                        let center = Vec2::new(size[0] as f32, size[1] as f32) * 0.5;
                        let cursor = local(p);
                        let f = (scroll * self.zoom_rate()).exp();
                        let anchor = cursor - center - self.cam2d.pan;
                        self.cam2d.zoom *= f;
                        self.cam2d.pan -= anchor * (f - 1.0);
                    }
                }
            }
        }

        // The decal gizmo's handles take the clicks that land on them.
        if kind == ViewKind::V3d && !mods.alt && self.stroke.is_none() {
            let on_view = pressed && resp.hovered();
            if self.decal_gizmo_input(rect, hover, on_view, down, mods.shift) {
                return;
            }
        }

        // Placing a decal: drag moves it, scroll scales it, Ctrl+scroll turns it.
        if kind == ViewKind::V3d && !mods.alt {
            if let Some(i) = self.placing_decal() {
                let stop = ui.input(|i| i.key_pressed(egui::Key::Escape) || i.pointer.secondary_clicked());
                if stop {
                    self.decal_place = None;
                    return;
                }
                let mut changed = false;
                let dragging = resp.dragged_by(B::Primary) || (pressed && resp.hovered());
                if dragging {
                    if let Some(p) = hover.filter(|p| rect.contains(*p)) {
                        let size = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
                        let (o, d) = self.cam.ray(local(p), size);
                        if let Some((pos, n)) = decal_hit(&self.mesh, &self.bvh, o, d)
                            && let Content::Fill(f) = &mut self.engine.layers[i].content
                        {
                            f.decal.center = pos.to_array();
                            if self.decal_align != mods.shift {
                                f.decal.normal = n.to_array();
                            }
                            changed = true;
                        }
                    }
                }
                let zoom = ui.input(|i| i.zoom_delta());
                if resp.hovered() && let Content::Fill(f) = &mut self.engine.layers[i].content {
                    if scroll != 0.0 && !mods.ctrl {
                        let k = (scroll * 0.002).exp();
                        f.decal.size = f.decal.size.map(|v| (v * k).clamp(1e-3, 4.0));
                        changed = true;
                    }
                    if mods.ctrl && zoom != 1.0 {
                        f.rotation = (f.rotation + zoom.ln() * 90.0 + 180.0).rem_euclid(360.0) - 180.0;
                        changed = true;
                    }
                }
                if changed {
                    self.engine.draft |= dragging;
                    self.engine.mark_layer_dirty(i);
                }
                return;
            }
        }

        // The bake window's gizmo (direction arrow, point marker) comes first.
        if kind == ViewKind::V3d {
            self.custom_ui.view_rect = Some(rect);
        }
        if kind == ViewKind::V3d && !mods.alt {
            let cursor = hover.filter(|p| rect.contains(*p)).map(local);
            if self.gizmo_input(ui, cursor, pressed, down) {
                return;
            }
        }

        // The path tool places and drags points instead of painting.
        if self.tool == Tool::Path {
            let cursor = hover.filter(|p| rect.contains(*p) && !mods.alt).map(local);
            let secondary = ui.input(|i| i.pointer.secondary_clicked()) && !mods.any();
            self.path_input(kind, cursor, pressed && !mods.alt, down && !mods.alt, secondary, resp.hovered());
            return;
        }

        // Polygon fill picks faces instead of laying down stamps.
        if self.tool == Tool::PolyFill {
            if let Some(mut b) = self.poly.box_drag.filter(|b| b.view == kind) {
                if down {
                    if let Some(p) = hover {
                        b.end = rect.clamp(p);
                        self.poly.box_drag = Some(b);
                    }
                    return;
                }
                // Released: fill what the box covers. A Shift+click without
                // dragging fills the face under it, like a plain click.
                self.poly.box_drag = None;
                let (a, z) = (local(b.start), local(b.end));
                let seeds = if a.distance(z) < 3.0 * ppp {
                    self.pick_triangle(kind, a).into_iter().collect()
                } else {
                    self.box_triangles(kind, a, z)
                };
                if !seeds.is_empty() && self.begin_stroke_on_target(kind, a, b.erase) {
                    self.poly.filled.clear();
                    self.poly.stroke_erase = b.erase;
                    self.poly_fill(&seeds);
                    self.end_stroke();
                }
                return;
            }
            let at = hover.filter(|p| rect.contains(*p) && !mods.alt).map(local);
            let tri = at.and_then(|p| self.pick_triangle(kind, p));
            if at.is_some() {
                self.poly.frame_hover = tri;
            }
            if pressed && mods.shift && at.is_some() && resp.hovered() && self.stroke.is_none() {
                let p = hover.unwrap_or_default();
                self.poly.box_drag = Some(BoxDrag { view: kind, start: p, end: p, erase: self.poly.erase != mods.ctrl });
            } else if pressed && at.is_some() && resp.hovered() && self.stroke.is_none() {
                if let Some(t) = tri {
                    let erase = self.poly.erase != mods.ctrl;
                    if self.begin_stroke_on_target(kind, at.unwrap_or_default(), erase) {
                        self.poly.filled.clear();
                        self.poly.stroke_erase = erase;
                        self.poly_fill(&[t]);
                    }
                }
            } else if down && self.stroke.as_ref().is_some_and(|s| s.view == kind) {
                if let Some(t) = tri {
                    self.poly_fill(&[t]);
                }
            }
            return;
        }

        // Selecting: the decal under the click (topmost first).
        if self.tool == Tool::Select {
            if pressed && resp.hovered() && !mods.alt && kind == ViewKind::V3d {
                if let Some(p) = hover.filter(|p| rect.contains(*p)) {
                    self.select_at(local(p));
                }
            }
            return;
        }

        // Painting.
        if let Some(p) = hover {
            let in_view = rect.contains(p);
            if pressed && in_view && !mods.alt && self.stroke.is_none() && resp.hovered() && self.tool == Tool::Picker {
                self.pick_material(kind, local(p));
            } else if pressed && in_view && !mods.alt && self.stroke.is_none() && resp.hovered() {
                // Shift+click: a straight line from where the last stroke ended.
                let anchor = self.line_anchor.filter(|a| mods.shift && a.view == kind && a.key == self.view_key(kind));
                match anchor {
                    Some(a) => {
                        self.try_begin_stroke(kind, a.at);
                        if let Some(s) = &mut self.stroke {
                            s.lazy = local(p);
                        }
                        self.continue_stroke(local(p));
                    }
                    None => self.try_begin_stroke(kind, local(p)),
                }
            } else if down && self.stroke.as_ref().is_some_and(|s| s.view == kind) {
                self.continue_stroke(local(p));
            }
        }
    }

    /// One viewport (a dock tab): input, the GPU render at the tab's size,
    /// overlays and the brush cursor.
    fn view_pane(&mut self, ui: &mut egui::Ui, kind: ViewKind) {
        let rect = ui.available_rect_before_wrap();
        if rect.width() < 4.0 || rect.height() < 4.0 {
            return;
        }
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        self.view_input(ui, kind, rect, &resp);
        if let Some(item) = resp.dnd_release_payload::<ShelfItem>() {
            self.pending.push(Action::AddOnTop(*item));
        }
        self.update_poly_highlight();

        // GPU: composite, then render this view at its pixel size.
        let t0 = Instant::now();
        if self.engine.dirty {
            self.engine.composite();
            if self.profile {
                self.prof.push(("composite".into(), t0.elapsed().as_secs_f32() * 1000.0));
            }
        }
        let t0 = Instant::now();
        self.composite_parked();
        if self.profile {
            self.prof.push(("composite_parked".into(), t0.elapsed().as_secs_f32() * 1000.0));
        }
        let size = [(rect.width() * self.ppp).round() as u32, (rect.height() * self.ppp).round() as u32];
        let mut rt_view = None;
        match kind {
            ViewKind::V3d => {
                let engines = texture_sets::engines(&self.engine, &self.sets, self.mesh.sets.len());
                self.views.render_3d(&self.rs, size, &engines, &self.env, &self.gpu_mesh, &self.cam, &self.view, Some(&self.overlays));
                self.drew_3d = true;
                // Path tracing replaces the 3D view while it's idle. Painting and
                // the map display modes use the raster view, which reacts instantly.
                if self.rt.enabled && self.stroke.is_none() && self.view.mode == ViewMode::Material {
                    let engines = texture_sets::engines(&self.engine, &self.sets, self.mesh.sets.len());
                    if self.raytracer.render(&self.rs, size, &engines, &self.env, &self.cam, &self.view, &self.rt) {
                        ui.ctx().request_repaint();
                    }
                    rt_view = self.raytracer.egui_id;
                }
            }
            ViewKind::V2d => {
                if self.cam2d.zoom <= 0.0 {
                    self.cam2d.zoom = size[0].min(size[1]) as f32 * 0.9;
                    self.cam2d.pan = Vec2::ZERO;
                }
                self.views.render_2d(&self.rs, size, &self.engine, &self.env, &self.gpu_mesh, self.sets.active, &self.cam2d, &self.view);
            }
        }

        let painter = ui.painter_at(rect);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let id = match kind {
            ViewKind::V3d => rt_view.unwrap_or(self.views.view3d.egui_id),
            ViewKind::V2d => self.views.view2d.egui_id,
        };
        painter.image(id, rect, uv, Color32::WHITE);
        self.draw_path(&painter, kind, rect);
        self.draw_gizmo(&painter, kind, rect);
        if kind == ViewKind::V3d {
            self.draw_decal(&painter, rect);
        }
        if let Some(b) = self.poly.box_drag.filter(|b| b.view == kind) {
            let color = if b.erase { Color32::from_rgb(255, 90, 80) } else { Color32::from_rgb(80, 160, 255) };
            let r = Rect::from_two_pos(b.start, b.end);
            painter.rect_filled(r, 0.0, color.gamma_multiply(0.15));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        }
        let label = match kind {
            ViewKind::V3d if rt_view.is_some() => {
                let state = if self.raytracer.converged(&self.rt) { "done" } else { "refining" };
                format!("Ray traced · {}/{} spp ({state})", self.raytracer.samples, self.rt.max_samples)
            }
            _ => self.mode_name(self.view.mode),
        };
        painter.text(rect.min + vec2(10.0, 8.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(11.5), Color32::from_white_alpha(150));
        if kind == ViewKind::V3d {
            self.file_drop_in_view(ui.ctx(), rect, &painter);
        }
        if let Some(item) = resp.dnd_hover_payload::<ShelfItem>() {
            let what = match *item {
                ShelfItem::Generator(_) => "Drop to mask the selected layer",
                ShelfItem::Brush(_) => "Drop to paint with this brush tip",
                _ => "Drop to add",
            };
            painter.rect_stroke(rect.shrink(2.0), 0.0, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
            painter.text(rect.center_bottom() - vec2(0.0, 24.0), egui::Align2::CENTER_CENTER, what, egui::FontId::proportional(13.0), Color32::WHITE);
        }

        // Brush cursor.
        let alt = ui.input(|i| i.modifiers.alt);
        if let Some(p) = resp.hover_pos() {
            if self.tool == Tool::PolyFill && !alt {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            } else if !alt {
                let r = self.brush.size * 0.5;
                painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(140)));
                painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_white_alpha(220)));
                let inner = r * self.brush.hardness;
                if inner > 2.0 {
                    painter.circle_stroke(p, inner, Stroke::new(1.0, Color32::from_white_alpha(70)));
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if let Some(s) = self.stroke.as_ref().filter(|s| s.view == kind && self.brush.stabilize > 0.0) {
                    let lazy = rect.min + vec2(s.lazy.x, s.lazy.y) / self.ppp;
                    painter.line_segment([lazy, p], Stroke::new(1.0, Color32::from_white_alpha(120)));
                    painter.circle_filled(lazy, 2.5, Color32::WHITE);
                }
            }
        }
    }

    /// After every view drew: strokes end when the button is released, and
    /// the 3D depth the brush reads stays valid when no 3D view is showing.
    fn views_end_frame(&mut self, ctx: &egui::Context) {
        if self.stroke.is_some() && !ctx.input(|i| i.pointer.primary_down()) {
            self.end_stroke();
        }
        self.update_poly_highlight();
        if !self.drew_3d {
            let size = self.views.view3d.size;
            let engines = texture_sets::engines(&self.engine, &self.sets, self.mesh.sets.len());
            self.views.render_3d(&self.rs, size, &engines, &self.env, &self.gpu_mesh, &self.cam, &self.view, Some(&self.overlays));
        }
        self.drew_3d = false;
    }

    // ------------------------------------------------------------------ windows

    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(egui::RichText::new("Material Painter").heading().color(Color32::WHITE));
            ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(theme::TEXT_DIM));
            if let Some(gpu) = crate::crash::context() {
                // Selectable, so it can be copied into bug reports.
                ui.add(egui::Label::new(egui::RichText::new(format!("GPU: {gpu}")).color(theme::TEXT_DIM)).selectable(true));
            }
            ui.add_space(8.0);
            ui.label("A PBR texture painter: layers, smart materials and masks, mesh map baking, material graphs and custom node shaders.");
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Free software under the GNU General Public License, version 3 or later. It comes with no warranty.")
                    .small()
                    .color(theme::TEXT_DIM),
            );
            ui.label(
                egui::RichText::new("Bundled HDRIs and textures: Poly Haven (CC0). Icons: Phosphor (MIT).").small().color(theme::TEXT_DIM),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Source code").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(REPO_URL));
                }
                if ui.button("Third-party licenses").clicked() {
                    match notices_path() {
                        Some(p) => ui.ctx().open_url(egui::OpenUrl::new_tab(format!("file:///{}", p.display().to_string().replace('\\', "/")))),
                        None => ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{REPO_URL}/blob/master/THIRD_PARTY_NOTICES.html"))),
                    }
                }
            });
        });
        self.show_about = open;
    }

    /// Tells about a crash in the last run, once, with its report.
    fn crash_window(&mut self, ctx: &egui::Context) {
        let Some(path) = self.crash_report.clone() else { return };
        let mut close = false;
        egui::Window::new("Material Painter closed unexpectedly").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label("Last time, the app stopped because of an error. A report was saved; sending it helps fix the problem.");
            ui.add_space(4.0);
            ui.label(egui::RichText::new(path.display().to_string()).small().monospace().color(theme::TEXT_DIM));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Open the report").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("file:///{}", path.display().to_string().replace('\\', "/"))));
                }
                if ui.button("Report the problem").on_hover_text("Opens a new issue on the project page; attach the report").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{REPO_URL}/issues/new")));
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.crash_report = None;
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_help;
        egui::Window::new("Controls").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            egui::Grid::new("help").striped(true).show(ui, |ui| {
                for (k, v) in [
                    ("LMB", "Paint / erase"),
                    ("Alt + LMB  or  RMB", "Orbit (3D) · pan (2D)"),
                    ("MMB", "Pan"),
                    ("Alt + RMB / Wheel", "Zoom"),
                    ("Shift + RMB", "Rotate environment"),
                    ("Ctrl + RMB", "Brush size (→) / hardness (↑)"),
                    ("Alt + double-click", "Set orbit center on the mesh"),
                    ("[  ]", "Brush size"),
                    ("Shift + click", "Straight line from the last stroke"),
                    ("Ctrl+G", "Material Designer (the selected layer's graph)"),
                    ("Ctrl+PageUp / PageDown", "Switch workspace"),
                    ("1 / 2 / I", "Paint / Eraser / Pick material"),
                    ("X", "Toggle X symmetry"),
                    ("F", "Frame mesh"),
                    ("M / C", "Material view / cycle channels"),
                    ("F1 / F2 / F3", "3D+2D / 3D / 2D"),
                    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
                    ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
                    ("Ctrl+O", "Open project"),
                    ("Ctrl+Shift+E", "Export textures"),
                    ("Double-click (shelf)", "Add to the layer stack"),
                    ("Drag (shelf)", "Drop on a view or in the layer stack"),
                    ("Ctrl + wheel (shelf)", "Thumbnail size"),
                    ("Drag (layers)", "Reorder; drop on a folder's middle to put it inside"),
                ] {
                    ui.label(egui::RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        self.show_help = open;
    }
}

impl eframe::App for PainterApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let start = Instant::now();
        let ctx = ui.ctx().clone();
        self.ppp = ctx.pixels_per_point();
        { let t_ = Instant::now(); let _ = self.rs.device.poll(wgpu::PollType::Poll); if self.profile { self.prof.push(("rs.device.poll".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.undo.update(); if self.profile { self.prof.push(("undo.update".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.poll_bake(&ctx); if self.profile { self.prof.push(("poll_bake".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        self.poll_spp_open(&ctx);
        { let t_ = Instant::now(); self.update_preview(&ctx); if self.profile { self.prof.push(("update_preview".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        self.poll_env();
        self.poll_hdri_thumbs(&ctx);
        self.poll_library(&ctx);
        // Closing the window with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.is_modified() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Action::Exit);
            } else {
                project_io::remove_autosave();
            }
        }
        self.recovery_dialog(&ctx);
        self.confirm_dialog(&ctx);
        { let t_ = Instant::now(); self.autosave_if_due(); if self.profile { self.prof.push(("autosave_if_due".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        let title = format!(
            "{}{} — Material Painter",
            self.project_path.as_ref().and_then(|p| p.file_stem()).map_or("Untitled".to_string(), |n| n.to_string_lossy().into_owned()),
            if self.is_modified() { " *" } else { "" }
        );
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
        if let Some(p) = &mut self.shelf_previews
            && !p.pump(&self.rs, &ctx, &mut self.shelf)
        {
            self.shelf_previews = None;
        }
        self.path_keys(&ctx);
        self.shortcuts(&ctx);
        { let t_ = Instant::now(); self.process_actions(&ctx); if self.profile { self.prof.push(("process_actions".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.process_mcp(&ctx); if self.profile { self.prof.push(("process_mcp".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.process_live_reload(&ctx); if self.profile { self.prof.push(("process_live_reload".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }

        let t_ = Instant::now();
        egui::Panel::top("menu").frame(theme::bar().inner_margin(egui::Margin::symmetric(10, 5))).show(ui, |ui| self.menu_bar(ui));
        if self.profile {
            self.prof.push(("menu bar".into(), t_.elapsed().as_secs_f32() * 1000.0));
        }
        let paint = self.workspace == Workspace::Paint;
        if paint {
            egui::Panel::top("toolbar")
                .exact_size(46.0)
                .frame(theme::card(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 4)))
                .show(ui, |ui| self.context_toolbar(ui));
        }
        egui::Panel::bottom("status").exact_size(24.0).frame(theme::bar()).show(ui, |ui| self.status_bar(ui));
        if paint {
        egui::Panel::left("tools")
            .exact_size(56.0)
            .frame(theme::card(theme::PANEL).inner_margin(egui::Margin::symmetric(7, 10)))
            .show(ui, |ui| self.tool_strip(ui));
        }

        // Panel actions (add layer, etc.) apply before this frame's GPU work.
        { let t_ = Instant::now(); self.process_actions(&ctx); if self.profile { self.prof.push(("process_actions".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        // Edits made while a button is held (slider and color drags, gizmo
        // handles) show as quick drafts; the full composite follows on release.
        let held = ctx.input(|i| i.pointer.any_down());
        if self.engine.draft && !held {
            self.engine.draft = false;
            self.engine.mark_dirty();
        } else if held && self.engine.dirty && self.stroke.is_none() {
            self.engine.draft = true;
        }
        self.sync_brush_tip();
        self.sync_brush_grain();
        { let t_ = Instant::now(); self.update_graphs(); if self.profile { self.prof.push(("update_graphs".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.update_preview_shader(); if self.profile { self.prof.push(("update_preview_shader".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.update_cage_preview(&ctx); if self.profile { self.prof.push(("update_cage_preview".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        match self.workspace {
            Workspace::Paint => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(4))).show(ui, |ui| {
                    self.dock_ui(ui, Workspace::Paint);
                });
            }
            Workspace::Designer => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| self.designer(ui));
            }
            Workspace::ShaderMaker => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| self.shader_maker(ui));
            }
        }
        if self.workspace != Workspace::Designer {
            self.graphs.editor.hovered = false;
        }
        { let t_ = Instant::now(); self.views_end_frame(&ctx); if self.profile { self.prof.push(("views_end_frame".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        { let t_ = Instant::now(); self.docks.save_if_changed(&ctx); if self.profile { self.prof.push(("docks.save_if_changed".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }

        self.export_window(&ctx);
        self.preferences_window(&ctx);
        self.apply_preferences(&ctx);
        self.bake_window(&ctx);
        self.spp_report_window(&ctx);
        self.help_window(&ctx);
        self.about_window(&ctx);
        self.crash_window(&ctx);
        self.welcome_window(&ctx);
        self.library_window(&ctx);
        // Settings changed in the bake window show on the next frame.
        { let t_ = Instant::now(); self.update_cage_preview(&ctx); if self.profile { self.prof.push(("update_cage_preview".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }

        // Property edits become one undo step once the interaction ends: no
        // button held (slider drags) and no text field focused (typing).
        let idle = !ctx.input(|i| i.pointer.any_down()) && ctx.memory(|m| m.focused().is_none());
        if idle && self.stroke.is_none() {
            { let t_ = Instant::now(); self.engine.commit_props(&mut self.undo); if self.profile { self.prof.push(("engine.commit_props".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        }
        { let t_ = Instant::now(); self.update_highlight(&ctx); if self.profile { self.prof.push(("update_highlight".into(), t_.elapsed().as_secs_f32() * 1000.0)); } }
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        self.frame_ms = self.frame_ms * 0.9 + ms * 0.1;
        if self.profile {
            // The GPU work this frame queued, and how long since the last frame began.
            let t_gpu = Instant::now();
            let _ = self.rs.device.poll(wgpu::PollType::wait_indefinitely());
            let gpu = t_gpu.elapsed().as_secs_f32() * 1000.0;
            let gap = self.last_frame.map_or(0.0, |t| t.elapsed().as_secs_f32() * 1000.0);
            self.last_frame = Some(start);
            if ms > 40.0 || gpu > 25.0 || gap > 120.0 {
                let name = self.engine.layers.get(self.selected).map_or("-", |l| l.name.as_str());
                let parts: Vec<String> = self.prof.iter().filter(|(_, v)| *v > 0.5).map(|(k, v)| format!("{k} {v:.1}")).collect();
                let causes: Vec<String> = ctx.repaint_causes().iter().map(|c| c.to_string()).collect();
                let line = format!(
                    "{ms:.1} ms cpu, {gpu:.1} ms gpu, {gap:.0} ms since last frame  selected '{name}'  [{}]  repaint: {}\n",
                    parts.join(", "),
                    causes.join(" | ")
                );
                let path = std::env::temp_dir().join("mp_profile.log");
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    use std::io::Write;
                    let _ = f.write_all(line.as_bytes());
                }
            }
            self.prof.clear();
        }
    }
}
