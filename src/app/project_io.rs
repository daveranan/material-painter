//! Project files (.mpp): a JSON manifest plus LZ4-compressed binary blobs
//! (the mesh, painted channels, masks and bake fixes). Baked mesh maps aren't
//! stored, which keeps files small; opening a project bakes them with the
//! saved settings, or loads them from the bake cache (`bake_cache`).
//!
//! Layout: magic `MPPROJ\0` + format version byte, u64 manifest length,
//! manifest (UTF-8 JSON), u64 blob count, then per blob a u64 length and the
//! compressed bytes. Saving writes a temporary file and renames it over the
//! target, so a failed save never damages an existing project.
//!
//! Text projects (`name.json`) hold the same manifest, pretty-printed, so
//! people and agents can read, diff and edit them. Blob references become
//! paths into the `name.data/` folder next to it: 8-bit textures as PNG,
//! everything else LZ4-compressed. Unchanged files aren't rewritten, so a
//! save only touches what changed (see `live_reload` for edits on disk).

use super::PainterApp;
use crate::bake::{IdSource, Quality};
use crate::engine::{Blend, Content, Engine, FillParams, Folder, GenKind, Generator, Layer, Levels, Mask, MatValues, Projection};
use crate::environment::EnvSource;
use crate::gpu::Tex;
use crate::mesh::{Mesh, Vertex};
use crate::post::Tonemap;
use crate::viewport::{Background, OrbitCam};
use eframe::{egui, wgpu};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"MPPROJ\x00\x01";

/// Per-user app data: recent projects and the autosave.
pub(super) fn data_dir() -> Option<PathBuf> {
    crate::crash::data_dir()
}

/// This instance's recovery copy of unsaved work, written every few edits.
/// Each running instance has its own, guarded by a lock file it holds open
/// for its lifetime; an autosave whose lock is free belongs to a session that
/// ended without closing cleanly.
pub(super) fn autosave_path() -> Option<PathBuf> {
    static LOCK: std::sync::OnceLock<Option<std::fs::File>> = std::sync::OnceLock::new();
    let dir = data_dir()?;
    let pid = std::process::id();
    LOCK.get_or_init(|| {
        std::fs::create_dir_all(&dir).ok()?;
        let file = std::fs::File::create(dir.join(format!("autosave-{pid}.lock"))).ok()?;
        file.try_lock().ok()?;
        Some(file)
    })
    .as_ref()?;
    Some(dir.join(format!("autosave-{pid}.mpp")))
}

pub(super) fn is_autosave(path: &Path) -> bool {
    data_dir().is_some_and(|d| path.parent() == Some(d.as_path()))
        && path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("autosave-"))
}

/// The newest autosave left by a session that didn't close cleanly.
pub(super) fn pending_recovery() -> Option<PathBuf> {
    let own = autosave_path();
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(data_dir()?)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_autosave(p) && p.extension().is_some_and(|x| x == EXTENSION) && Some(p) != own.as_ref())
        .filter(|p| {
            // A lock we can take means its owner is gone.
            let lock = p.with_extension("lock");
            std::fs::File::open(&lock).map_or(true, |f| f.try_lock().is_ok())
        })
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// Deletes an autosave and its lock file.
pub(super) fn remove_recovery(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

/// Deletes this instance's autosave (the work was saved or discarded).
pub(super) fn remove_autosave() {
    if let Some(p) = autosave_path() {
        let _ = std::fs::remove_file(p);
    }
}

pub(super) fn load_recent() -> Vec<PathBuf> {
    let Some(text) = data_dir().and_then(|d| std::fs::read_to_string(d.join("recent.json")).ok()) else { return Vec::new() };
    let list: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
    list.into_iter().map(PathBuf::from).filter(|p| p.is_file()).collect()
}
pub const EXTENSION: &str = "mpp";

/// Texture formats a project can hold, by name.
const FORMATS: [wgpu::TextureFormat; 6] = [
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Rg8Unorm,
    wgpu::TextureFormat::Rg16Float,
    wgpu::TextureFormat::R8Unorm,
    wgpu::TextureFormat::R16Float,
    wgpu::TextureFormat::Rgba16Float,
];

/// Text projects: the data folder next to `name.json`.
pub(super) fn data_dir_of(manifest: &Path) -> PathBuf {
    manifest.with_extension("data")
}

pub(super) fn is_text_project(path: &Path) -> bool {
    path.extension().is_some_and(|x| x.eq_ignore_ascii_case("json"))
}

/// A data-folder path from a manifest, refused if it could leave the folder
/// (the manifest may have been edited by hand or by an agent).
pub(super) fn data_path(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("'{rel}' isn't a path inside the project's data folder"));
    }
    Ok(dir.join(p))
}

struct Writer {
    blobs: Vec<Vec<u8>>,
    /// Text projects: data-folder files by relative path, instead of blobs.
    files: Option<Vec<(String, Vec<u8>)>>,
}

impl Writer {
    /// `name` (no extension) is the file's path in a text project.
    fn blob(&mut self, raw: &[u8], name: &str) -> Value {
        let packed = lz4_flex::compress_prepend_size(raw);
        match &mut self.files {
            Some(files) => {
                let rel = format!("{name}.lz4");
                files.push((rel.clone(), packed));
                json!(rel)
            }
            None => {
                self.blobs.push(packed);
                json!(self.blobs.len() - 1)
            }
        }
    }

    /// Already-encoded bytes (an image file): stored as they are in a text
    /// project, as a compressed blob otherwise.
    fn encoded(&mut self, bytes: &[u8], name: &str) -> Value {
        match &mut self.files {
            Some(files) => {
                let ext = image::guess_format(bytes).ok().and_then(|f| f.extensions_str().first().copied()).unwrap_or("bin");
                let rel = format!("{name}.{ext}");
                files.push((rel.clone(), bytes.to_vec()));
                json!(rel)
            }
            None => self.blob(bytes, name),
        }
    }
}

/// A project's graph entry: node graphs as saved, script graphs compiled,
/// with a `script_file` read from the data folder (where relative image
/// paths in the script are found too).
fn graph_from(r: &Reader, gv: &Value, lib: &crate::graph::Library) -> Result<crate::graph::Graph, String> {
    let mut g = gv["graph"].clone();
    if let Some(rel) = g["script_file"].as_str() {
        let bytes = r.blob(&json!(rel))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("{rel} isn't text"))?;
        g["script"] = json!(text);
    }
    crate::graph::Graph::from_json_in(&g, lib, r.dir.as_deref())
}

struct Reader {
    blobs: Vec<Vec<u8>>,
    /// Text projects: where blob paths point.
    dir: Option<PathBuf>,
}

impl Reader {
    /// A blob's bytes; for text projects a `.lz4` file is decompressed and
    /// any other file (images) is returned as stored.
    fn blob(&self, v: &Value) -> Result<Vec<u8>, String> {
        if let (Some(rel), Some(dir)) = (v.as_str(), &self.dir) {
            let path = data_path(dir, rel)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
            return if rel.ends_with(".lz4") {
                lz4_flex::decompress_size_prepended(&bytes).map_err(|e| format!("{rel} is damaged: {e}"))
            } else {
                Ok(bytes)
            };
        }
        let i = v.as_u64().ok_or("missing blob reference")? as usize;
        let packed = self.blobs.get(i).ok_or(format!("blob {i} is missing"))?;
        lz4_flex::decompress_size_prepended(packed).map_err(|e| format!("blob {i} is damaged: {e}"))
    }
}

/// Writes through a temporary file, so readers (the live reload, an editor)
/// never see a half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("can't replace {}: {e}", path.display()))
}

/// Saves a text project: the data files that changed, then the manifest.
/// Returns the total size.
fn write_text_project(path: &Path, manifest: &Value, files: Vec<(String, Vec<u8>)>, live: &mut super::live_reload::LiveProject) -> Result<usize, String> {
    let dir = data_dir_of(path);
    let mut manifest = manifest.clone();
    name_generator_weights(&mut manifest);
    let manifest = &manifest;
    let text = pretty_json(manifest);
    // Before writing: the file watcher must recognise these files as ours.
    // What the file says, as read back (floats shortened), to compare edits against.
    live.watch(path, serde_json::from_str(&text).unwrap_or_else(|_| manifest.clone()));
    live.expect(super::live_reload::fingerprint(text.as_bytes(), files.iter().map(|(r, b)| (r.as_str(), b.as_slice()))));
    let mut total = text.len();
    let mut keep = std::collections::HashSet::new();
    for (rel, bytes) in &files {
        let full = data_path(&dir, rel)?;
        total += bytes.len();
        keep.insert(full.clone());
        if std::fs::read(&full).is_ok_and(|old| old == *bytes) {
            continue;
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("can't create {}: {e}", parent.display()))?;
        }
        write_atomic(&full, bytes)?;
    }
    remove_stale(&dir, &keep);
    write_atomic(path, text.as_bytes())?;
    Ok(total)
}

/// Generator weights as `{input name: weight}` (nonzero only) instead of a
/// list in input order, so a text project says what each weight is.
fn name_generator_weights(v: &mut Value) {
    match v {
        Value::Object(o) => {
            if o.get("kind").is_some() {
                if let Some(Value::Array(a)) = o.get("weights") {
                    let named: serde_json::Map<String, Value> = a
                        .iter()
                        .zip(crate::engine::GEN_INPUTS)
                        .filter(|(w, _)| w.as_f64().is_some_and(|w| w != 0.0))
                        .map(|(w, (name, _))| (name.to_string(), w.clone()))
                        .collect();
                    if a.len() == crate::engine::GEN_INPUT_COUNT {
                        o.insert("weights".into(), Value::Object(named));
                    }
                }
            }
            o.values_mut().for_each(name_generator_weights);
        }
        Value::Array(a) => a.iter_mut().for_each(name_generator_weights),
        _ => {}
    }
}

/// Deletes files a previous save wrote that nothing refers to any more
/// (deleted layers, removed masks). Only the folders saves write to are
/// cleaned (`mesh`, `set<N>`); anything else in the data folder is left.
fn remove_stale(dir: &Path, keep: &std::collections::HashSet<PathBuf>) {
    let ours = |name: &str| name == "mesh" || name.strip_prefix("set").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    for sub in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if !sub.path().is_dir() || !ours(&sub.file_name().to_string_lossy()) {
            continue;
        }
        for file in std::fs::read_dir(sub.path()).into_iter().flatten().flatten() {
            let p = file.path();
            if p.is_file() && !keep.contains(&p) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Reads a project of either kind: the manifest and its blobs.
fn read_project(path: &Path) -> Result<(Value, Reader), String> {
    let data = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let bad = || format!("{} isn't a Material Painter project (or is damaged)", path.display());
    if data.first() == Some(&b'{') || data.starts_with(b"\xEF\xBB\xBF{") {
        let manifest: Value = serde_json::from_slice(data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&data)).map_err(|e| format!("{}: {e}", path.display()))?;
        if manifest["app"] != "Material Painter" {
            return Err(bad());
        }
        return Ok((manifest, Reader { blobs: Vec::new(), dir: Some(data_dir_of(path)) }));
    }
    if data.len() < 24 || &data[..8] != MAGIC {
        return Err(bad());
    }
    let mut at = 8;
    let mut take = |n: usize| -> Result<&[u8], String> {
        let slice = data.get(at..at + n).ok_or_else(bad)?;
        at += n;
        Ok(slice)
    };
    let u64_at = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap()) as usize;
    let len = u64_at(take(8)?);
    let manifest: Value = serde_json::from_slice(take(len)?).map_err(|e| format!("{}: {e}", bad()))?;
    let count = u64_at(take(8)?);
    let mut blobs = Vec::with_capacity(count);
    for _ in 0..count {
        let n = u64_at(take(8)?);
        blobs.push(take(n)?.to_vec());
    }
    Ok((manifest, Reader { blobs, dir: None }))
}

/// Raw texels as a PNG, for the 8-bit formats PNG can hold losslessly.
fn texels_to_png(format: wgpu::TextureFormat, w: u32, h: u32, raw: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let color = match format {
        wgpu::TextureFormat::Rgba8Unorm => image::ExtendedColorType::Rgba8,
        wgpu::TextureFormat::Rg8Unorm => image::ExtendedColorType::La8,
        wgpu::TextureFormat::R8Unorm => image::ExtendedColorType::L8,
        _ => return None,
    };
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, Default::default())
        .write_image(raw, w, h, color)
        .ok()?;
    Some(out)
}

/// A PNG back to raw texels of `format`, scaled to `w`×`h` if it was
/// replaced by an image of another size (a mask made by a script, say).
fn png_to_texels(format: wgpu::TextureFormat, w: u32, h: u32, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let img = if (img.width(), img.height()) == (w, h) { img } else { img.resize_exact(w, h, image::imageops::FilterType::Triangle) };
    Ok(match format {
        wgpu::TextureFormat::Rgba8Unorm => img.into_rgba8().into_raw(),
        wgpu::TextureFormat::Rg8Unorm => img.into_luma_alpha8().into_raw(),
        wgpu::TextureFormat::R8Unorm => img.into_luma8().into_raw(),
        _ => return Err(format!("{format:?} textures aren't stored as images")),
    })
}

/// JSON with nested objects indented but short arrays of numbers, strings
/// and booleans on one line (colors, offsets), so diffs stay readable.
pub(super) fn pretty_json(v: &Value) -> String {
    fn scalar(v: &Value) -> bool {
        !matches!(v, Value::Array(_) | Value::Object(_))
    }
    fn write(v: &Value, depth: usize, out: &mut String) {
        let pad = |d: usize| "  ".repeat(d);
        match v {
            Value::Array(a) if a.is_empty() => out.push_str("[]"),
            Value::Array(a) if a.len() <= 16 && a.iter().all(scalar) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write(x, depth, out);
                }
                out.push(']');
            }
            Value::Array(a) => {
                out.push_str("[\n");
                for (i, x) in a.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write(x, depth + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Value::Object(o) if o.is_empty() => out.push_str("{}"),
            Value::Object(o) => {
                out.push_str("{\n");
                for (i, (k, x)) in o.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push_str(": ");
                    write(x, depth + 1, out);
                    out.push_str(if i + 1 < o.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            Value::Number(n) if n.is_f64() => out.push_str(&short_float(n.as_f64().unwrap_or(0.0))),
            x => out.push_str(&x.to_string()),
        }
    }
    /// Most values are f32 widened to f64; print the f32's shortest form.
    fn short_float(x: f64) -> String {
        let f = x as f32;
        let s = if f as f64 == x || (f as f64 - x).abs() <= x.abs() * 1e-7 { format!("{f:?}") } else { format!("{x:?}") };
        s
    }
    let mut out = String::new();
    write(v, 0, &mut out);
    out.push('\n');
    out
}

fn f(v: &Value, key: &str, default: f32) -> f32 {
    v.get(key).and_then(Value::as_f64).map_or(default, |x| x as f32)
}

fn arr<const N: usize>(v: &Value, key: &str, default: [f32; N]) -> [f32; N] {
    match v.get(key).and_then(Value::as_array) {
        Some(a) if a.len() == N => std::array::from_fn(|i| a[i].as_f64().unwrap_or(default[i] as f64) as f32),
        _ => default,
    }
}

fn b(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn custom_map_json(c: &crate::bake::CustomMap) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "kind": format!("{:?}", c.kind),
        "azimuth": c.azimuth, "elevation": c.elevation, "spread": c.spread,
        "distance": c.distance, "strength": c.strength, "wander": c.wander,
        "point": c.point,
    })
}

/// A custom bake map; `None` for an empty slot (older projects had four
/// fixed slots) or an unknown kind. Maps saved without an id (those slots)
/// get `fallback_id`, the slot's number, which older masks refer to.
fn custom_map_from(v: &Value, fallback_id: u32) -> Option<crate::bake::CustomMap> {
    let name = s(v, "kind")?;
    let kind = crate::bake::CustomKind::ALL.into_iter().find(|k| format!("{k:?}") == name)?;
    let d = kind.default_map();
    let mut point = d.point;
    for (dst, x) in point.iter_mut().zip(v["point"].as_array().into_iter().flatten()) {
        *dst = x.as_f64().unwrap_or(0.0) as f32;
    }
    Some(crate::bake::CustomMap {
        id: v["id"].as_u64().map_or(fallback_id, |id| id as u32),
        name: s(v, "name").unwrap_or(kind.name()).to_string(),
        kind,
        azimuth: f(v, "azimuth", d.azimuth),
        elevation: f(v, "elevation", d.elevation),
        spread: f(v, "spread", d.spread),
        distance: f(v, "distance", d.distance),
        strength: f(v, "strength", d.strength),
        wander: f(v, "wander", d.wander),
        point,
    })
}

pub(super) fn fill_json(p: &FillParams) -> Value {
    json!({
        // color, roughness, metallic, height, normal, emission
        "enabled": [p.mat.enabled[0], p.mat.enabled[1], p.mat.enabled[2], p.mat.enabled[3], p.normal_on, p.emission_on],
        "color": p.mat.color,
        "roughness": p.mat.rough,
        "metallic": p.mat.metal,
        "height": p.mat.height,
        "color_variation": p.color_var,
        "variation_scale": p.var_scale,
        "roughness_variation": p.rough_var,
        "height_variation": p.height_var,
        "seed": p.seed,
        "part_variation": p.part_var,
        "projection": format!("{:?}", p.projection),
        "tiling": p.tiling,
        "height_blend": p.height_blend,
        "rotation": p.rotation,
        "offset": p.offset,
        "triplanar_sharpness": p.tri_sharpness,
        "graph": p.graph,
        "normal_strength": p.normal_strength,
        "normal_directx": p.normal_dx,
        "emission": p.emission,
        "emission_strength": p.emission_strength,
        "decal": decal_json(&p.decal),
        "runoff": p.runoff.map(|r| json!({"source": r.source, "length": r.length, "seed": r.seed})),
    })
}

pub(super) fn decal_json(d: &crate::decal::Decal) -> Value {
    json!({"center": d.center, "normal": d.normal, "size": d.size, "depth": d.depth, "facing": d.facing, "soft": d.soft, "mirror": d.mirror})
}

pub(super) fn decal_from(v: &Value) -> crate::decal::Decal {
    let d = crate::decal::Decal::default();
    crate::decal::Decal {
        center: arr(v, "center", d.center),
        normal: arr(v, "normal", d.normal),
        size: arr(v, "size", d.size),
        depth: f(v, "depth", d.depth),
        facing: f(v, "facing", d.facing),
        soft: f(v, "soft", d.soft),
        mirror: v["mirror"].as_bool().unwrap_or(d.mirror),
    }
}

pub(super) fn text_json(t: &crate::decal::TextSpec) -> Value {
    json!({"text": t.text, "font": t.font, "weight": t.weight, "spacing": t.spacing})
}

pub(super) fn text_from(v: &Value) -> crate::decal::TextSpec {
    let d = crate::decal::TextSpec::default();
    crate::decal::TextSpec {
        text: s(v, "text").map_or(d.text, str::to_string),
        font: s(v, "font").map_or(d.font, str::to_string),
        weight: f(v, "weight", d.weight),
        spacing: f(v, "spacing", d.spacing),
    }
}

pub(super) fn fill_from(v: &Value) -> FillParams {
    let d = FillParams::default();
    // [color, roughness, metallic, height] and, since normal/emission, [.., normal, emission].
    let flags = v.get("enabled").and_then(Value::as_array).filter(|a| a.len() == 4 || a.len() == 6);
    let enabled = match flags {
        Some(a) => std::array::from_fn(|i| a[i].as_bool().unwrap_or(true)),
        None => d.mat.enabled,
    };
    let extra = |i: usize| flags.and_then(|a| a.get(i)).and_then(Value::as_bool).unwrap_or(false);
    FillParams {
        mat: MatValues {
            enabled,
            color: arr(v, "color", d.mat.color),
            rough: f(v, "roughness", d.mat.rough),
            metal: f(v, "metallic", d.mat.metal),
            height: f(v, "height", d.mat.height),
        },
        color_var: f(v, "color_variation", d.color_var),
        var_scale: f(v, "variation_scale", d.var_scale),
        rough_var: f(v, "roughness_variation", d.rough_var),
        height_var: f(v, "height_variation", d.height_var),
        seed: f(v, "seed", d.seed),
        part_var: f(v, "part_variation", d.part_var),
        projection: s(v, "projection").and_then(Projection::from_key).unwrap_or(Projection::Uv),
        tiling: f(v, "tiling", d.tiling),
        height_blend: f(v, "height_blend", d.height_blend).max(0.0),
        rotation: f(v, "rotation", d.rotation),
        offset: arr(v, "offset", d.offset),
        tri_sharpness: f(v, "triplanar_sharpness", d.tri_sharpness),
        graph: v["graph"].as_u64(),
        normal_on: extra(4),
        normal_strength: f(v, "normal_strength", d.normal_strength),
        normal_dx: v["normal_directx"].as_bool().unwrap_or(d.normal_dx),
        emission_on: extra(5),
        emission: arr(v, "emission", d.emission),
        emission_strength: f(v, "emission_strength", d.emission_strength),
        decal: v.get("decal").filter(|d| d.is_object()).map_or(d.decal, decal_from),
        runoff: v.get("runoff").filter(|r| r.is_object()).and_then(|r| {
            Some(crate::decal::Runoff { source: r["source"].as_u64()?, length: f(r, "length", 0.8), seed: f(r, "seed", 1.0) })
        }),
    }
}

pub(super) fn generator_json(g: &Generator) -> Value {
    json!({
        "kind": g.kind.name(),
        "weights": g.weights,
        "level": g.level,
        "contrast": g.contrast,
        "scale": g.scale,
        "noise": g.noise,
        "invert": g.invert,
        "seed": g.seed,
        "edge_width": g.edge_width,
        "id_color": g.id_color,
        "id_tolerance": g.id_tolerance,
        "custom": g.custom.iter().filter(|c| c.id != 0).map(|c| json!({"id": c.id, "weight": c.weight})).collect::<Vec<_>>(),
    })
}

/// A mask's filters, as saved.
pub(super) fn mask_filters_json(filters: &[crate::engine::MaskFilter]) -> Value {
    filters.iter().map(|f| json!({"kind": f.kind.key(), "enabled": f.enabled, "a": f.a, "b": f.b, "c": f.c})).collect()
}

pub(super) fn mask_filters_from(v: &Value) -> Vec<crate::engine::MaskFilter> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|fv| {
            let kind = crate::engine::FilterKind::ALL.into_iter().find(|k| Some(k.key()) == fv["kind"].as_str())?;
            let mut f = crate::engine::MaskFilter::new(kind);
            f.enabled = fv["enabled"].as_bool().unwrap_or(true);
            f.a = fv["a"].as_f64().map_or(f.a, |v| v as f32);
            f.b = fv["b"].as_f64().map_or(f.b, |v| v as f32);
            f.c = fv["c"].as_f64().map_or(f.c, |v| v as f32);
            Some(f)
        })
        .collect()
}

/// A mask's mask layers, as saved.
pub(super) fn mask_layers_json(layers: &[crate::engine::MaskLayer]) -> Value {
    layers
        .iter()
        .map(|ml| {
            let source = match &ml.source {
                crate::engine::MaskSource::Generator(g) => json!({"generator": generator_json(g)}),
                crate::engine::MaskSource::Fill(v) => json!({"fill": v}),
            };
            json!({"source": source, "blend": ml.blend.key(), "opacity": ml.opacity, "enabled": ml.enabled})
        })
        .collect()
}

/// A texture set's features (see `engine::Features`): names by slot, and size.
fn features_json(f: &crate::engine::Features) -> Value {
    json!({"main": f.names[0], "mask1": f.names[1], "mask2": f.names[2], "tile_m": f.tile_m, "relief_m": f.relief_m})
}

fn features_from(v: &Value) -> crate::engine::Features {
    let d = crate::engine::Features::default();
    let name = |k: &str| s(v, k).unwrap_or("").trim().to_string();
    crate::engine::Features {
        names: [name("main"), name("mask1"), name("mask2")],
        tile_m: f(v, "tile_m", d.tile_m).max(1e-3),
        relief_m: f(v, "relief_m", d.relief_m).max(0.0),
    }
}

pub(super) fn mask_layers_from(v: &Value) -> Vec<crate::engine::MaskLayer> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|lv| {
            let src = &lv["source"];
            let source = match (src.get("generator"), src["fill"].as_f64()) {
                (Some(g), _) => crate::engine::MaskSource::Generator(generator_from(g).ok()?),
                (None, Some(v)) => crate::engine::MaskSource::Fill(v as f32),
                _ => return None,
            };
            Some(crate::engine::MaskLayer {
                source,
                blend: crate::engine::MaskBlend::ALL.into_iter().find(|b| Some(b.key()) == lv["blend"].as_str()).unwrap_or(crate::engine::MaskBlend::Multiply),
                opacity: lv["opacity"].as_f64().unwrap_or(1.0) as f32,
                enabled: lv["enabled"].as_bool().unwrap_or(true),
            })
        })
        .collect()
}

pub(super) fn generator_from(v: &Value) -> Result<Generator, String> {
    let name = s(v, "kind").ok_or("generator without a kind")?;
    let kind = GenKind::ALL.into_iter().find(|k| k.name() == name).ok_or(format!("unknown generator '{name}'"))?;
    let d = kind.default_generator();
    // Without weights (a hand-written generator) the kind's defaults apply.
    let mut weights = if v.get("weights").is_some() { [0.0; crate::engine::GEN_INPUT_COUNT] } else { d.weights };
    let mut custom = [crate::engine::CustomInput::default(); crate::engine::MAX_CUSTOM_INPUTS];
    // Text projects name the inputs: {"Edges": 1.0, "Noise": 0.35}.
    if let Some(o) = v.get("weights").and_then(Value::as_object) {
        for (name, w) in o {
            let i = crate::engine::GEN_INPUTS.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)).ok_or(format!("unknown generator input '{name}'"))?;
            weights[i] = w.as_f64().unwrap_or(0.0) as f32;
        }
    }
    if let Some(a) = v.get("weights").and_then(Value::as_array) {
        // Older projects have fewer inputs; new ones default to 0.
        for (w, x) in weights.iter_mut().zip(a) {
            *w = x.as_f64().unwrap_or(0.0) as f32;
        }
        // Projects from the four fixed custom slots kept their weights after
        // the built-in inputs: slot k is the map with id k + 1.
        let old = a.iter().skip(crate::engine::GEN_INPUT_COUNT).map(|x| x.as_f64().unwrap_or(0.0) as f32);
        let used = old.enumerate().filter(|(_, w)| *w != 0.0).take(crate::engine::MAX_CUSTOM_INPUTS);
        for (slot, (k, w)) in custom.iter_mut().zip(used) {
            *slot = crate::engine::CustomInput { id: k as u32 + 1, weight: w };
        }
    }
    for (slot, c) in custom.iter_mut().zip(v["custom"].as_array().into_iter().flatten()) {
        *slot = crate::engine::CustomInput { id: c["id"].as_u64().unwrap_or(0) as u32, weight: c["weight"].as_f64().unwrap_or(0.0) as f32 };
    }
    Ok(Generator {
        kind,
        weights,
        level: f(v, "level", d.level),
        contrast: f(v, "contrast", d.contrast),
        scale: f(v, "scale", d.scale),
        noise: f(v, "noise", d.noise),
        invert: b(v, "invert", d.invert),
        seed: f(v, "seed", d.seed),
        edge_width: f(v, "edge_width", d.edge_width),
        id_color: arr(v, "id_color", d.id_color),
        id_tolerance: f(v, "id_tolerance", d.id_tolerance),
        custom,
    })
}

/// A mesh's part of a project; `base` prefixes its files in a text project.
fn mesh_json(w: &mut Writer, mesh: &Mesh, base: &str, auto_smooth: Option<f32>) -> Value {
    json!({
        "auto_smooth": auto_smooth,
        "name": mesh.name,
        "center": mesh.center.to_array(),
        "scale": mesh.scale,
        "vertices": w.blob(bytemuck::cast_slice(&mesh.vertices), &format!("{base}/vertices")),
        "indices": w.blob(bytemuck::cast_slice(&mesh.indices), &format!("{base}/indices")),
        "objects": mesh.objects,
        "tri_object": w.blob(bytemuck::cast_slice(&mesh.tri_object), &format!("{base}/tri_object")),
        "tri_color": mesh.tri_color.as_ref().map(|c| w.blob(bytemuck::cast_slice(c), &format!("{base}/tri_color"))),
    })
}

/// A saved mesh (see `mesh_json`) from its decoded vertices and indices.
fn mesh_from(r: &Reader, m: &Value, vertices: Vec<Vertex>, indices: Vec<u32>) -> Result<Mesh, String> {
    let center = glam::Vec3::from_array(arr(m, "center", [0.0; 3]));
    let tris = indices.len() / 3;
    let mut mesh = Mesh::from_saved(s(m, "name").unwrap_or("Mesh").to_string(), vertices, indices, center, f(m, "scale", 1.0));
    if let Some(names) = m["objects"].as_array() {
        let tri_object: Vec<u32> = bytemuck::pod_collect_to_vec(&r.blob(&m["tri_object"])?);
        if tri_object.len() == tris && tri_object.iter().all(|&o| (o as usize) < names.len()) {
            mesh.objects = names.iter().map(|n| n.as_str().unwrap_or("").to_string()).collect();
            mesh.tri_object = tri_object;
        }
    }
    if !m["tri_color"].is_null() {
        let colors: Vec<[u8; 3]> = r.blob(&m["tri_color"])?.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        if colors.len() == tris {
            mesh.tri_color = Some(colors);
        }
    }
    Ok(mesh)
}

impl PainterApp {
    /// `name` names the file in a text project, e.g. `set0/layer3_color`.
    fn tex_json(&self, w: &mut Writer, t: &Tex, name: &str) -> Value {
        let format = FORMATS.iter().find(|&&x| x == t.format).map(|x| format!("{x:?}"));
        let raw = self.engine.read_tex(t);
        let png = w.files.as_ref().and_then(|_| texels_to_png(t.format, t.width, t.height, &raw));
        json!({
            "blob": match png {
                Some(png) => w.encoded(&png, name),
                None => w.blob(&raw, name),
            },
            "format": format.expect("only project formats are saved"),
            "width": t.width,
            "height": t.height,
        })
    }

    fn tex_from(&self, r: &Reader, v: &Value, label: &str) -> Result<Tex, String> {
        let name = s(v, "format").ok_or("texture without a format")?;
        let format = FORMATS.into_iter().find(|x| format!("{x:?}") == name).ok_or(format!("unknown texture format '{name}'"))?;
        let (w, h) = (v["width"].as_u64().unwrap_or(0) as u32, v["height"].as_u64().unwrap_or(0) as u32);
        let mut bytes = r.blob(&v["blob"])?;
        if v["blob"].as_str().is_some_and(|p| p.ends_with(".png")) {
            let file = v["blob"].as_str().unwrap_or_default();
            bytes = png_to_texels(format, w, h, &bytes).map_err(|e| format!("{file}: {e}"))?;
        }
        let expected = (w * h * crate::gpu::bytes_per_texel(format)) as usize;
        if w == 0 || bytes.len() != expected {
            return Err(format!("{label} texture has the wrong size"));
        }
        Ok(self.engine.texture_from_bytes(label, format, w, h, &bytes))
    }

    /// The mesh set: each member's name, file and auto smooth; the others'
    /// meshes and paint (the shown one's are the project's own).
    fn mesh_set_json(&self, w: &mut Writer) -> Value {
        const CHANNELS: [&str; 4] = ["color", "roughness", "metallic", "height"];
        let ms = &self.mesh_set;
        if !ms.is_on() {
            return Value::Null;
        }
        let mut members = Vec::new();
        for (k, m) in ms.members.iter().enumerate() {
            let mut v = json!({"id": m.id, "name": m.name, "path": m.path, "auto_smooth": m.auto_smooth});
            if k != ms.active {
                let base = format!("meshset/m{}", m.id);
                v["mesh"] = mesh_json(w, &m.source, &format!("{base}/mesh"), m.auto_smooth);
                // Layers deleted since have nothing to keep.
                let mut ids: Vec<u64> = m.uv.layers.keys().copied().filter(|id| self.engine.layers.iter().any(|l| l.id == *id)).collect();
                ids.sort();
                let mut layers = Vec::new();
                for id in ids {
                    let l = &m.uv.layers[&id];
                    let lb = format!("{base}/layer{id}");
                    let channels = l.channels.as_ref().map(|ch| {
                        ch.iter()
                            .zip(CHANNELS)
                            .map(|(t, c)| t.as_ref().map_or(Value::Null, |t| self.tex_json(w, t, &format!("{lb}_{c}"))))
                            .collect::<Vec<_>>()
                    });
                    // Only layers that still have a paint mask need one per member.
                    let has_mask = self.engine.layers.iter().any(|x| x.id == id && x.mask.as_ref().is_some_and(|m| m.paint.is_some() && m.graph.is_none()));
                    let mask = l.mask.as_ref().filter(|_| has_mask).map(|t| self.tex_json(w, t, &format!("{lb}_mask")));
                    layers.push(json!({"id": id, "channels": channels, "mask": mask}));
                }
                v["layers"] = json!(layers);
                let mut fixes = Vec::new();
                for (i, t) in m.uv.map_fix.iter().flat_map(|f| f.iter().enumerate()) {
                    if let Some(t) = t {
                        fixes.push(json!({"map": i, "texture": self.tex_json(w, t, &format!("{base}/fix{i}"))}));
                    }
                }
                v["map_fixes"] = json!(fixes);
            }
            members.push(v);
        }
        json!({"shown": ms.active, "next_id": ms.next_id, "members": members})
    }

    /// Restores a saved mesh set around the mesh just loaded (the shown member).
    fn mesh_set_from(&mut self, r: &Reader, v: &Value) -> Result<(), String> {
        use super::mesh_set::{Member, MeshSet};
        use crate::engine::uv_data::{LayerUv, UvData};
        let Some(list) = v["members"].as_array() else { return Ok(()) };
        let shown = v["shown"].as_u64().unwrap_or(0) as usize;
        let mut set = MeshSet { active: shown.min(list.len().saturating_sub(1)), ..Default::default() };
        for (k, mv) in list.iter().enumerate() {
            let id = mv["id"].as_u64().unwrap_or(k as u64);
            let auto_smooth = mv["auto_smooth"].as_f64().map(|a| (a as f32).clamp(1.0, 180.0));
            let name = s(mv, "name").unwrap_or("Mesh").to_string();
            let path = s(mv, "path").map(PathBuf::from);
            let (source, uv) = if k == set.active {
                (self.mesh_source.clone(), UvData::default())
            } else {
                let m = &mv["mesh"];
                let vertices: Vec<Vertex> = bytemuck::pod_collect_to_vec(&r.blob(&m["vertices"])?);
                let indices: Vec<u32> = bytemuck::pod_collect_to_vec(&r.blob(&m["indices"])?);
                if indices.iter().any(|&i| i as usize >= vertices.len()) {
                    return Err(format!("mesh set member '{name}' is damaged"));
                }
                let mesh = mesh_from(r, m, vertices, indices)?;
                let mut uv = UvData::default();
                for lv in mv["layers"].as_array().into_iter().flatten() {
                    let Some(lid) = lv["id"].as_u64() else { continue };
                    let channels = match lv["channels"].as_array() {
                        Some(ch) => {
                            let mut out: [Option<crate::gpu::Tex>; 4] = Default::default();
                            for (i, c) in ch.iter().enumerate().take(4) {
                                if !c.is_null() {
                                    out[i] = Some(self.tex_from(r, c, "paint")?);
                                }
                            }
                            Some(out)
                        }
                        None => None,
                    };
                    let mask = if lv["mask"].is_null() { None } else { Some(self.tex_from(r, &lv["mask"], "mask")?) };
                    uv.layers.insert(lid, LayerUv { channels, mask });
                }
                let mut fix: [Option<crate::gpu::Tex>; crate::engine::MAP_COUNT] = Default::default();
                for fv in mv["map_fixes"].as_array().into_iter().flatten() {
                    let map = fv["map"].as_u64().unwrap_or(u64::MAX) as usize;
                    if map < fix.len() {
                        fix[map] = Some(self.tex_from(r, &fv["texture"], "map fix")?);
                    }
                }
                uv.map_fix = Some(fix);
                (std::sync::Arc::new(mesh), uv)
            };
            set.next_id = set.next_id.max(id + 1);
            set.members.push(Member { id, name, path, source, auto_smooth, uv });
        }
        set.next_id = set.next_id.max(v["next_id"].as_u64().unwrap_or(0));
        self.mesh_set = set;
        Ok(())
    }

    /// One texture set's part of a project; `set` is its index.
    fn set_json(&self, w: &mut Writer, engine: &Engine, set: usize) -> Value {
        const CHANNELS: [&str; 4] = ["color", "roughness", "metallic", "height"];
        let mut layers = Vec::new();
        for l in &engine.layers {
            let base = format!("set{set}/layer{}", l.id);
            let content = match &l.content {
                Content::Paint(ch) => {
                    let channels: Vec<Value> = ch
                        .iter()
                        .zip(CHANNELS)
                        .map(|(t, c)| t.as_ref().map_or(Value::Null, |t| self.tex_json(w, t, &format!("{base}_{c}"))))
                        .collect();
                    json!({"kind": "paint", "channels": channels})
                }
                Content::Fill(p) => json!({"kind": "fill", "fill": fill_json(p)}),
                Content::Folder(fo) => json!({"kind": "folder", "open": fo.open}),
            };
            let mask = l.mask.as_ref().map(|m| {
                json!({
                    "paint": m.paint.as_ref().map(|t| self.tex_json(w, t, &format!("{base}_mask"))),
                    "generator": m.generator.as_ref().map(generator_json),
                    "graph": m.graph,
                    "filters": mask_filters_json(&m.filters),
                    "layers": mask_layers_json(&m.layers),
                })
            });
            // A graph's outputs are rendered again on load, not stored; a
            // normal image beside them is the layer's own and is kept.
            let from_graph = matches!(&l.content, Content::Fill(p) if p.graph.is_some());
            let images: Vec<Value> = l
                .images
                .iter()
                .enumerate()
                .map(|(k, im)| (k, im.as_ref().filter(|_| !from_graph || k == crate::engine::NORMAL_CH)))
                .map(|(k, im)| {
                    im.map_or(Value::Null, |im| {
                        let mut v = json!({"name": im.name, "data": w.encoded(&im.encoded, &format!("{base}_image{k}"))});
                        if let Some(t) = &im.text {
                            v["text"] = text_json(t);
                        }
                        v
                    })
                })
                .collect();
            let mut lv = json!({
                "images": images,
                "id": l.id,
                "name": l.name,
                "visible": l.visible,
                "opacity": l.opacity,
                "blend": l.blend.name(),
                "parent": l.parent,
                "content": content,
                "mask": mask,
            });
            if let Some(slot) = l.feature {
                let name = &engine.features.names[slot as usize];
                lv["feature"] = if name.is_empty() { json!(slot) } else { json!(name) };
            }
            layers.push(lv);
        }
        let fixes: Vec<Value> = (0..engine.map_fix.len())
            .filter_map(|i| Some(json!({"map": i, "texture": self.tex_json(w, engine.map_fix[i].as_ref()?, &format!("set{set}/fix{i}"))})))
            .collect();
        let levels: Vec<Value> = engine.map_levels.iter().map(|l| json!([l.black, l.white, l.gamma])).collect();
        let mut v = json!({
            "resolution": engine.res,
            "layers_bottom_to_top": layers,
            "map_fixes": fixes,
            "map_levels": levels,
        });
        if engine.features.is_on() {
            v["features"] = features_json(&engine.features);
        }
        v
    }

    pub(super) fn save_project(&mut self, path: &Path) -> Result<(), String> {
        let bytes = self.write_project(path)?;
        self.project_path = Some(path.to_path_buf());
        self.saved_serial = self.undo.state_serial();
        self.saved_auto_smooth = self.auto_smooth;
        self.mesh_set.modified = false;
        self.status = format!("Saved {} ({:.1} MB).", path.display(), bytes as f64 / 1048576.0);
        self.remember_recent(path);
        remove_autosave();
        self.autosaved_serial = self.saved_serial;
        Ok(())
    }

    /// Writes the recovery copy if enough has changed since the last one.
    pub(super) fn autosave_if_due(&mut self) {
        let every = self.prefs.autosave_every.max(1) as u64;
        let serial = self.undo.state_serial();
        if !self.prefs.autosave || self.stroke.is_some() || !self.is_modified() || serial.abs_diff(self.autosaved_serial) < every {
            return;
        }
        let Some(path) = autosave_path() else { return };
        self.autosaved_serial = serial;
        if let Err(e) = self.write_project(&path) {
            self.status = format!("Autosave failed: {e}");
        }
    }

    /// Writes the project file; returns its size. Doesn't change which file
    /// the document is saved to.
    fn write_project(&mut self, path: &Path) -> Result<usize, String> {
        self.engine.commit_props(&mut self.undo);
        let text = is_text_project(path);
        let mut w = Writer { blobs: Vec::new(), files: text.then(Vec::new) };
        // The imported mesh; auto smooth is a setting applied to it on open.
        let mesh_json = mesh_json(&mut w, &self.mesh_source, "mesh", self.auto_smooth);
        let mesh_set = self.mesh_set_json(&mut w);
        // Every texture set: its layers, size and mesh-map corrections.
        let engines = super::texture_sets::engines(&self.engine, &self.sets, self.mesh.sets.len());
        let sets: Vec<Value> = self
            .mesh
            .sets
            .iter()
            .zip(&engines)
            .enumerate()
            .map(|(k, (set, engine))| {
                let selected = match self.sets.parked.get(k).and_then(Option::as_ref) {
                    Some(p) => p.selected,
                    None => self.selected,
                };
                let mut v = self.set_json(&mut w, engine, k);
                v["name"] = json!(set.name);
                v["tris"] = json!([set.tris.start, set.tris.end]);
                v["selected"] = json!(selected);
                v
            })
            .collect();
        let bs = &self.bake_settings;
        let bake = json!({
            "quality": format!("{:?}", bs.quality),
            "high_poly": bs.high_poly.as_ref().map(|p| p.display().to_string()),
            "id_source": format!("{:?}", bs.id_source),
            "front": bs.front, "rear": bs.rear, "aa": bs.aa,
            "ao_rays": bs.ao_rays, "ao_distance": bs.ao_distance, "cavity_distance": bs.cavity_distance,
            "thickness_rays": bs.thickness_rays, "thickness_distance": bs.thickness_distance,
            "sky_rays": bs.sky_rays, "sky_cone_deg": bs.sky_cone_deg,
            "curvature_radius": bs.curvature_radius, "curvature_scale": bs.curvature_scale,
            "edge_threshold": bs.edge_threshold, "edge_max_distance": bs.edge_max_distance,
            "edge_auto": bs.edge_auto, "hard_edge_angle": bs.hard_edge_angle,
            "part_crease_angle": bs.part_crease_angle,
            "match_by_name": bs.match_by_name,
            "cage": bs.cage.as_ref().map(|p| p.display().to_string()),
            "maps": bs.maps,
            "custom": bs.custom.iter().map(custom_map_json).collect::<Vec<_>>(),
        });
        let v = &self.view;
        let p = &v.post;
        let env = match &self.env.source {
            EnvSource::Builtin(i) => json!({"builtin": crate::environment::BUILTIN[*i].0}),
            EnvSource::File(path) => json!({"file": path.display().to_string()}),
        };
        let view = json!({
            "preview_shader": self.preview.graph,
            "environment": env,
            "exposure": v.exposure, "env_rotation": v.env_rotation, "env_intensity": v.env_intensity,
            "background": format!("{:?}", v.background), "bg_blur": v.bg_blur, "bg_intensity": v.bg_intensity,
            "bg_color": v.bg_color, "height_strength": v.height_strength, "displacement": v.displacement,
            "tessellation": v.tessellation.name(),
            "show_wire": v.show_wire, "use_baked_normal": v.use_baked_normal, "ao_strength": v.ao_strength,
            "key_light": {"on": v.key.on, "azimuth": v.key.azimuth, "elevation": v.key.elevation,
                "intensity": v.key.intensity, "color": v.key.color},
            "tonemap": format!("{:?}", p.tonemap), "contrast": p.contrast, "saturation": p.saturation,
            "temperature": p.temperature, "tint": p.tint, "vignette": p.vignette, "bloom": p.bloom,
            "bloom_threshold": p.bloom_threshold, "chromatic": p.chromatic, "grain": p.grain, "sharpen": p.sharpen,
            "camera": {"yaw": self.cam.yaw, "pitch": self.cam.pitch, "distance": self.cam.dist,
                       "target": self.cam.target.to_array(), "fov": self.cam.fov_y},
        });
        let manifest = json!({
            "app": "Material Painter",
            // 2: map levels gamma brightens above 1 (version 1 darkened).
            // 3: texture sets (layers, size and map fixes per set).
            "version": 3,
            "mesh": mesh_json,
            "mesh_set": mesh_set,
            "texture_sets": sets,
            "active_set": self.sets.active,
            "graphs": self.graphs.to_json(&self.used_graphs()),
            "bake": bake,
            "view": view,
        });
        if let Some(files) = w.files {
            return write_text_project(path, &manifest, files, &mut self.live);
        }
        let text = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(text.len() + w.blobs.iter().map(|b| b.len() + 8).sum::<usize>() + 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(text.len() as u64).to_le_bytes());
        out.extend_from_slice(&text);
        out.extend_from_slice(&(w.blobs.len() as u64).to_le_bytes());
        for blob in &w.blobs {
            out.extend_from_slice(&(blob.len() as u64).to_le_bytes());
            out.extend_from_slice(blob);
        }
        let tmp = path.with_extension("mpp.tmp");
        std::fs::write(&tmp, &out).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("can't replace {}: {e}", path.display()))?;
        Ok(out.len())
    }

    fn remember_recent(&mut self, path: &Path) {
        if is_autosave(path) {
            return;
        }
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(self.prefs.recent_count.max(1));
        if let Some(dir) = data_dir() {
            let list: Vec<String> = self.recent.iter().map(|p| p.display().to_string()).collect();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("recent.json"), json!(list).to_string());
        }
    }

    /// A texture set's saved layer stack, decoded (textures uploaded) but not
    /// yet in the document.
    fn layers_from(&self, r: &Reader, sv: &Value) -> Result<Vec<Layer>, String> {
        let bad = |what: &str| format!("layer stack: {what}");
        let features = features_from(&sv["features"]);
        let mut layers = Vec::new();
        for lv in sv["layers_bottom_to_top"].as_array().ok_or_else(|| bad("'layers_bottom_to_top' is missing"))? {
            let name = s(lv, "name").unwrap_or("Layer").to_string();
            let c = &lv["content"];
            let content = match s(c, "kind") {
                Some("paint") => {
                    let ch = c["channels"].as_array().ok_or_else(|| bad(&format!("'{name}' has no channels")))?;
                    let mut out: [Option<Tex>; 4] = [None, None, None, None];
                    for (k, t) in ch.iter().enumerate().take(4) {
                        if !t.is_null() {
                            out[k] = Some(self.tex_from(r, t, "layer")?);
                        }
                    }
                    Content::Paint(out)
                }
                Some("fill") => Content::Fill(fill_from(&c["fill"])),
                Some("folder") => Content::Folder(Folder { open: b(c, "open", true) }),
                other => return Err(bad(&format!("'{name}' has unknown kind {other:?} (paint, fill or folder)"))),
            };
            let mask = match lv.get("mask").filter(|m| !m.is_null()) {
                Some(mv) => Some(Mask {
                    paint: match mv.get("paint").filter(|p| !p.is_null()) {
                        Some(t) => Some(self.tex_from(r, t, "mask")?),
                        None => None,
                    },
                    generator: match mv.get("generator").filter(|g| !g.is_null()) {
                        Some(g) => Some(generator_from(g)?),
                        None => None,
                    },
                    graph: mv["graph"].as_u64(),
                    filters: mask_filters_from(&mv["filters"]),
                    layers: mask_layers_from(&mv["layers"]),
                }),
                None => None,
            };
            let mut images: [Option<crate::engine::FillImage>; crate::engine::IMAGE_SLOTS] = Default::default();
            for (k, iv) in lv["images"].as_array().into_iter().flatten().enumerate().take(crate::engine::IMAGE_SLOTS) {
                if !iv.is_null() {
                    // Text images: the text, rendered again when the stored image is missing.
                    let text = iv.get("text").filter(|t| t.is_object()).map(text_from);
                    let image = match (iv.get("data").filter(|d| !d.is_null()), &text) {
                        (Some(d), _) => {
                            let data = std::sync::Arc::new(r.blob(d)?);
                            let mut im = self.engine.decode_fill_image(s(iv, "name").unwrap_or("image").to_string(), data)?;
                            im.text = text.map(std::sync::Arc::new);
                            im
                        }
                        (None, Some(t)) => self.engine.text_fill_image(t).map_err(|e| bad(&e))?.0,
                        (None, None) => continue,
                    };
                    images[k] = Some(image);
                }
            }
            let blend_name = s(lv, "blend").unwrap_or("Normal");
            let feature = match &lv["feature"] {
                Value::Null => None,
                Value::String(f) => Some(features.slot(f).ok_or_else(|| {
                    bad(&format!("'{name}' has feature '{f}', which isn't one of the set's features {:?}", features.names))
                })?),
                Value::Number(n) => Some(n.as_u64().filter(|&n| n < 3).ok_or_else(|| bad(&format!("'{name}' has feature {n}; use 0, 1 or 2")))? as u8),
                other => return Err(bad(&format!("'{name}' has feature {other}; use a feature name or 0, 1, 2"))),
            };
            layers.push(Layer {
                id: lv["id"].as_u64().ok_or_else(|| bad(&format!("'{name}' has no numeric 'id'")))?,
                name,
                visible: b(lv, "visible", true),
                opacity: f(lv, "opacity", 1.0),
                blend: Blend::ALL.into_iter().find(|x| x.name() == blend_name).unwrap_or(Blend::Normal),
                content,
                mask,
                parent: lv["parent"].as_u64(),
                images,
                thumb: None,
                feature,
            });
        }
        let mut ids = std::collections::HashSet::new();
        if let Some(l) = layers.iter().find(|l| !ids.insert(l.id)) {
            return Err(bad(&format!("two layers have id {}", l.id)));
        }
        Ok(layers)
    }

    fn map_levels_from(&mut self, sv: &Value, version: u64) {
        for (dst, lv) in self.engine.map_levels.iter_mut().zip(sv["map_levels"].as_array().into_iter().flatten()) {
            if let Some(a) = lv.as_array().filter(|a| a.len() == 3) {
                let g = |i: usize| a[i].as_f64().unwrap_or(0.0) as f32;
                // Version 1 projects stored the inverse gamma; keep their look.
                let gamma = if version < 2 { 1.0 / g(2).max(1e-3) } else { g(2) };
                *dst = Levels { black: g(0), white: g(1), gamma };
            }
        }
        self.engine.map_levels_changed();
    }

    /// Applies a text project edited on disk to the open document when only
    /// its layers, graphs or view changed: one undo step, mesh maps kept.
    /// Returns false when the mesh, bake settings or texture sets changed,
    /// which need a full open instead.
    /// `changed`: data files rewritten since the document last matched the
    /// disk; texture sets that use one reload even if their JSON is the same.
    pub(super) fn reload_in_place(
        &mut self,
        path: &Path,
        before: &Value,
        changed: &std::collections::HashSet<String>,
        ctx: &egui::Context,
    ) -> Result<bool, String> {
        use super::live_reload::refers_to_any;
        let (manifest, r) = read_project(path)?;
        let same = |k: &str| manifest[k] == before[k] && !refers_to_any(&manifest[k], changed);
        let sets_now = manifest["texture_sets"].as_array().map_or(0, Vec::len);
        let sets_before = before["texture_sets"].as_array().map_or(0, Vec::len);
        let set_ranges_same = (0..sets_now).all(|k| manifest["texture_sets"][k]["tris"] == before["texture_sets"][k]["tris"]);
        if !same("mesh") || !same("bake") || sets_now != sets_before || sets_now != self.mesh.sets.len() || !set_ranges_same {
            return Ok(false);
        }
        // Decode every set first, so a mistake in the file changes nothing.
        let mut decoded = Vec::new();
        for (k, sv) in manifest["texture_sets"].as_array().into_iter().flatten().enumerate() {
            if *sv != before["texture_sets"][k] || refers_to_any(sv, changed) {
                decoded.push((k, self.layers_from(&r, sv)?));
            }
        }
        if !same("graphs") {
            let mut graphs = Vec::new();
            for gv in manifest["graphs"].as_array().into_iter().flatten() {
                let id = gv["id"].as_u64().ok_or("graphs: entry without an 'id'")?;
                graphs.push((id, graph_from(&r, gv, &self.graphs.lib).map_err(|e| format!("graph {id}: {e}"))?));
            }
            self.graphs.clear(&self.rs);
            for (id, g) in graphs {
                self.graphs.insert(&self.rs, g, Some(id));
            }
            self.repush_graphs();
        }
        let active = self.sets.active;
        let version = manifest["version"].as_u64().unwrap_or(1);
        for (k, layers) in decoded {
            self.switch_set(k);
            self.engine.replace_layers(layers, "Reload from disk".into(), &mut self.undo);
            let sv = &manifest["texture_sets"][k];
            self.engine.features = features_from(&sv["features"]);
            self.map_levels_from(sv, version);
            self.selected = self.selected.min(self.engine.layers.len().saturating_sub(1));
        }
        self.switch_set(active);
        if !same("view") {
            self.view_from(&manifest["view"], ctx);
        }
        self.live.watch(path, manifest);
        Ok(true)
    }

    pub(super) fn open_project(&mut self, path: &Path, ctx: &egui::Context) -> Result<(), String> {
        let bad = || format!("{} isn't a Material Painter project (or is damaged)", path.display());
        let (manifest, r) = read_project(path)?;

        // Everything is decoded before the open document is touched.
        let m = &manifest["mesh"];
        let vertices: Vec<Vertex> = bytemuck::pod_collect_to_vec(&r.blob(&m["vertices"])?);
        let indices: Vec<u32> = bytemuck::pod_collect_to_vec(&r.blob(&m["indices"])?);
        if indices.iter().any(|&i| i as usize >= vertices.len()) {
            return Err(bad());
        }
        let mut mesh = mesh_from(&r, m, vertices, indices)?;
        let bv = &manifest["bake"];
        let mut bs = crate::bake::BakeSettings::default();
        if let Some(q) = s(bv, "quality") {
            bs.quality = Quality::ALL.into_iter().find(|x| format!("{x:?}") == q).unwrap_or(bs.quality);
        }
        bs.high_poly = s(bv, "high_poly").map(PathBuf::from).filter(|p| p.is_file());
        bs.front = f(bv, "front", bs.front);
        bs.rear = f(bv, "rear", bs.rear);
        bs.aa = bv["aa"].as_u64().map_or(bs.aa, |x| x as u32);
        bs.ao_rays = bv["ao_rays"].as_u64().map_or(bs.ao_rays, |x| x as u32);
        bs.ao_distance = f(bv, "ao_distance", bs.ao_distance);
        bs.cavity_distance = f(bv, "cavity_distance", bs.cavity_distance);
        bs.thickness_rays = bv["thickness_rays"].as_u64().map_or(bs.thickness_rays, |x| x as u32);
        bs.thickness_distance = f(bv, "thickness_distance", bs.thickness_distance);
        bs.sky_rays = bv["sky_rays"].as_u64().map_or(bs.sky_rays, |x| x as u32);
        bs.sky_cone_deg = f(bv, "sky_cone_deg", bs.sky_cone_deg);
        bs.curvature_radius = f(bv, "curvature_radius", bs.curvature_radius);
        bs.curvature_scale = f(bv, "curvature_scale", bs.curvature_scale);
        bs.edge_threshold = f(bv, "edge_threshold", bs.edge_threshold);
        bs.edge_max_distance = f(bv, "edge_max_distance", bs.edge_max_distance);
        bs.edge_auto = b(bv, "edge_auto", bs.edge_auto);
        bs.hard_edge_angle = f(bv, "hard_edge_angle", bs.hard_edge_angle);
        bs.part_crease_angle = f(bv, "part_crease_angle", bs.part_crease_angle);
        bs.match_by_name = b(bv, "match_by_name", bs.match_by_name);
        bs.cage = s(bv, "cage").map(PathBuf::from).filter(|p| p.is_file());
        // Projects from before map selection baked everything.
        bs.maps = bv["maps"].as_u64().map_or(crate::bake::ALL_MAPS, |m| m as u32);
        bs.custom = bv["custom"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(k, cv)| custom_map_from(cv, k as u32 + 1))
            .collect();
        bs.id_source = match s(bv, "id_source") {
            Some("Materials") => IdSource::Materials,
            Some("VertexColors") => IdSource::VertexColors,
            _ => IdSource::MeshParts,
        };

        // Texture sets: projects before version 3 have one, at the top level.
        let set_values: Vec<&Value> = match manifest["texture_sets"].as_array() {
            Some(a) if !a.is_empty() => a.iter().collect(),
            _ => vec![&manifest],
        };
        let ranges: Vec<(String, std::ops::Range<u32>)> = set_values
            .iter()
            .filter_map(|sv| {
                let t = sv["tris"].as_array()?;
                let (a, b) = (t.first()?.as_u64()? as u32, t.get(1)?.as_u64()? as u32);
                Some((s(sv, "name").unwrap_or("Material").to_string(), a..b))
            })
            .collect();
        if ranges.len() == set_values.len() {
            mesh.set_ranges(ranges);
        }
        let set_count = mesh.sets.len();

        // Switch mesh first, then each set's size: layers are created at it.
        self.bake_settings = bs;
        self.auto_smooth = m["auto_smooth"].as_f64().map(|a| (a as f32).clamp(1.0, 180.0));
        self.saved_auto_smooth = self.auto_smooth;
        if let Some(a) = self.auto_smooth {
            self.auto_smooth_angle = a;
        }
        self.load_mesh(mesh, ctx);
        self.graphs.clear(&self.rs);
        for gv in manifest["graphs"].as_array().into_iter().flatten() {
            let Some(id) = gv["id"].as_u64() else { continue };
            match graph_from(&r, gv, &self.graphs.lib) {
                Ok(g) => {
                    self.graphs.insert(&self.rs, g, Some(id));
                }
                Err(e) => self.graphs.load_errors.push(format!("graph {id}: {e}")),
            }
        }
        for (k, sv) in set_values.iter().enumerate().take(set_count) {
            self.switch_set(k);
            while !self.engine.layers.is_empty() {
                self.engine.delete_layer(0, &mut self.undo);
            }
            let res = sv["resolution"].as_u64().unwrap_or(2048) as u32;
            if res != self.engine.res {
                self.engine.set_resolution(res, &self.gpu_mesh, &mut self.undo);
            }
            let layers = self.layers_from(&r, sv)?;
            self.engine.restore_layers(layers);
            self.engine.map_fix = Default::default();
            for fv in sv["map_fixes"].as_array().into_iter().flatten() {
                let map = fv["map"].as_u64().unwrap_or(u64::MAX) as usize;
                if map < self.engine.map_fix.len() {
                    self.engine.map_fix[map] = Some(self.tex_from(&r, &fv["texture"], "map fix")?);
                }
            }
            self.map_levels_from(sv, manifest["version"].as_u64().unwrap_or(1));
            self.engine.features = features_from(&sv["features"]);
            self.selected = sv["selected"].as_u64().map_or(0, |x| x as usize).min(self.engine.layers.len().saturating_sub(1));
        }
        self.switch_set(manifest["active_set"].as_u64().map_or(0, |x| x as usize).min(set_count - 1));
        if set_count == 1 {
            self.mesh_set_from(&r, &manifest["mesh_set"])?;
        }
        self.repush_graphs();
        self.view_from(&manifest["view"], ctx);

        self.undo.clear();
        self.saved_serial = self.undo.state_serial();
        self.autosaved_serial = self.saved_serial;
        if is_autosave(path) {
            // Recovered work isn't saved anywhere yet; its file goes once opened.
            remove_recovery(path);
            self.project_path = None;
            self.saved_serial = u64::MAX;
            self.status = "Recovered your unsaved work. Save it to keep it.".into();
        } else {
            self.project_path = Some(path.to_path_buf());
            self.remember_recent(path);
            self.status = format!("Opened {}.", path.display());
        }
        self.live.watch(path, manifest);
        super::live_reload::mark_loaded(&mut self.live, path);
        Ok(())
    }

    /// The saved preview settings: camera, lighting, environment, post.
    fn view_from(&mut self, v: &Value, ctx: &egui::Context) {
        let d = crate::viewport::ViewSettings::default();
        let vs = &mut self.view;
        vs.exposure = f(v, "exposure", d.exposure);
        vs.env_rotation = f(v, "env_rotation", d.env_rotation);
        vs.env_intensity = f(v, "env_intensity", d.env_intensity);
        vs.ao_strength = f(v, "ao_strength", d.ao_strength);
        vs.background = Background::ALL.into_iter().find(|x| Some(format!("{x:?}").as_str()) == s(v, "background")).unwrap_or(d.background);
        vs.bg_blur = f(v, "bg_blur", d.bg_blur);
        vs.bg_intensity = f(v, "bg_intensity", d.bg_intensity);
        vs.bg_color = arr(v, "bg_color", d.bg_color);
        vs.height_strength = f(v, "height_strength", d.height_strength);
        vs.displacement = f(v, "displacement", d.displacement).clamp(0.0, 1.0);
        vs.tessellation = s(v, "tessellation").and_then(crate::viewport::Tessellation::from_name).unwrap_or(d.tessellation);
        vs.show_wire = b(v, "show_wire", d.show_wire);
        vs.use_baked_normal = b(v, "use_baked_normal", d.use_baked_normal);
        self.preview.graph = v["preview_shader"].as_u64().filter(|id| self.graphs.doc(*id).is_some_and(|d| d.shading));
        let k = &v["key_light"];
        // Projects saved before the key light existed open as they looked.
        vs.key = if k.is_null() {
            crate::viewport::KeyLight { on: false, ..d.key }
        } else {
            crate::viewport::KeyLight {
                on: b(k, "on", d.key.on),
                azimuth: f(k, "azimuth", d.key.azimuth),
                elevation: f(k, "elevation", d.key.elevation),
                intensity: f(k, "intensity", d.key.intensity),
                color: arr(k, "color", d.key.color),
            }
        };
        let p = &mut vs.post;
        p.tonemap = Tonemap::ALL.into_iter().find(|x| Some(format!("{x:?}").as_str()) == s(v, "tonemap")).unwrap_or(d.post.tonemap);
        p.contrast = f(v, "contrast", d.post.contrast);
        p.saturation = f(v, "saturation", d.post.saturation);
        p.temperature = f(v, "temperature", d.post.temperature);
        p.tint = f(v, "tint", d.post.tint);
        p.vignette = f(v, "vignette", d.post.vignette);
        p.bloom = f(v, "bloom", d.post.bloom);
        p.bloom_threshold = f(v, "bloom_threshold", d.post.bloom_threshold);
        p.chromatic = f(v, "chromatic", d.post.chromatic);
        p.grain = f(v, "grain", d.post.grain);
        p.sharpen = f(v, "sharpen", d.post.sharpen);
        let cam = &v["camera"];
        let dc = OrbitCam::default();
        self.cam = OrbitCam {
            yaw: f(cam, "yaw", dc.yaw),
            pitch: f(cam, "pitch", dc.pitch),
            dist: f(cam, "distance", dc.dist),
            target: glam::Vec3::from_array(arr(cam, "target", dc.target.to_array())),
            fov_y: f(cam, "fov", dc.fov_y),
        };
        let env = &v["environment"];
        let source = match (s(env, "builtin"), s(env, "file")) {
            (Some(name), _) => Some(EnvSource::parse(name)),
            (_, Some(file)) if Path::new(file).is_file() => Some(EnvSource::File(PathBuf::from(file))),
            _ => None,
        };
        if let Some(src) = source.filter(|src| *src != self.env.source) {
            self.load_env(src, ctx);
        }
    }

    /// Unsaved edits since the last save or open.
    pub(super) fn is_modified(&self) -> bool {
        self.undo.state_serial() != self.saved_serial || self.engine.has_uncommitted_props() || self.auto_smooth != self.saved_auto_smooth || self.mesh_set.modified
    }

    pub(super) fn save_dialog(&self) -> Option<PathBuf> {
        let name = self.project_path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || format!("{}.{EXTENSION}", self.mesh.name.replace(' ', "")),
            |n| n.to_string_lossy().into_owned(),
        );
        rfd::FileDialog::new()
            .add_filter("Material Painter project", &[EXTENSION])
            .add_filter("Material Painter text project (editable, live reload)", &["json"])
            .set_file_name(name)
            .save_file()
    }

    pub(super) fn open_dialog() -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("Material Painter project", &[EXTENSION, "json"]).pick_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Data paths from a hand-edited manifest can't leave the data folder.
    #[test]
    fn data_paths_stay_inside() {
        let dir = Path::new("proj.data");
        assert_eq!(data_path(dir, "set0/layer3_mask.png").unwrap(), dir.join("set0/layer3_mask.png"));
        for bad in ["", "../x.png", "set0/../../x", "/etc/passwd", "C:/x.png"] {
            assert!(data_path(dir, bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn pretty_json_round_trips_and_keeps_short_arrays_inline() {
        let v = json!({"b": {"color": [0.5, 0.25, 1.0], "names": ["a", "b"]}, "a": [], "layers": [{"id": 1}, {"id": 2}]});
        let text = pretty_json(&v);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
        assert!(text.contains("\"color\": [0.5, 0.25, 1.0]"), "{text}");
        assert!(text.contains("\"layers\": [\n"), "{text}");
    }

    #[test]
    fn pngs_hold_8_bit_texels() {
        let raw: Vec<u8> = (0..4 * 4 * 2).map(|i| (i * 7) as u8).collect();
        let png = texels_to_png(wgpu::TextureFormat::Rg8Unorm, 4, 4, &raw).unwrap();
        assert_eq!(png_to_texels(wgpu::TextureFormat::Rg8Unorm, 4, 4, &png).unwrap(), raw);
        assert!(texels_to_png(wgpu::TextureFormat::R16Float, 4, 4, &[0; 32]).is_none());
        // A replacement image of another size is scaled to the texture's.
        let small = texels_to_png(wgpu::TextureFormat::R8Unorm, 2, 2, &[255; 4]).unwrap();
        assert_eq!(png_to_texels(wgpu::TextureFormat::R8Unorm, 4, 4, &small).unwrap(), vec![255; 16]);
    }

    #[test]
    fn named_weights_round_trip() {
        let g = generator_from(&json!({"kind": "Edge Wear"})).unwrap();
        let mut v = json!({"mask": {"generator": generator_json(&g)}});
        name_generator_weights(&mut v);
        let named = &v["mask"]["generator"]["weights"];
        assert!(named.is_object() && named["Edges"].as_f64().is_some(), "{named}");
        assert_eq!(generator_from(&v["mask"]["generator"]).unwrap(), g);
        assert!(generator_from(&json!({"kind": "Dirt", "weights": {"Nope": 1}})).is_err());
    }

    /// What a save records must equal what the watcher computes from disk,
    /// or every save would reload itself.
    #[test]
    fn save_fingerprint_matches_disk() {
        let dir = std::env::temp_dir().join(format!("mp-fp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.json");
        let mut w = Writer { blobs: Vec::new(), files: Some(Vec::new()) };
        let manifest = json!({
            "app": "Material Painter",
            "mesh": {"vertices": w.blob(&[1, 2, 3], "mesh/vertices"), "tri_color": null},
            "texture_sets": [{"layers_bottom_to_top": [{"images": [{"name": "x", "data": w.encoded(&texels_to_png(wgpu::TextureFormat::R8Unorm, 1, 1, &[9]).unwrap(), "set0/layer1_image0")}]}]}],
        });
        let mut live = crate::app::live_reload::LiveProject::default();
        write_text_project(&path, &manifest, w.files.take().unwrap(), &mut live).unwrap();
        assert!(dir.join("p.data/set0/layer1_image0.png").is_file());
        assert_eq!(live.seen_for_test(), crate::app::live_reload::disk_fingerprint_for_test(&path));
        let (back, r) = read_project(&path).unwrap();
        assert_eq!(r.blob(&back["mesh"]["vertices"]).unwrap(), [1, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Projects from the four fixed custom slots load as maps with ids 1-4
    /// (empty slots skipped), and their masks' extra weights as inputs on
    /// those ids.
    #[test]
    fn old_custom_slots_migrate() {
        let slots = json!([{"kind": "Flow", "elevation": 90.0}, null, {"kind": "Deposit"}, null]);
        let maps: Vec<_> = slots.as_array().unwrap().iter().enumerate().filter_map(|(k, v)| custom_map_from(v, k as u32 + 1)).collect();
        assert_eq!(maps.iter().map(|c| (c.id, c.name.as_str())).collect::<Vec<_>>(), vec![(1, "Fluid Flow"), (3, "Wind Deposits")]);

        let mut weights = vec![0.0; crate::engine::GEN_INPUT_COUNT];
        weights[crate::engine::gin::OCCLUSION] = 0.4;
        weights.extend([0.0, 0.0, 1.0, 0.0]);
        let g = generator_from(&json!({"kind": "Custom Bake Map", "weights": weights})).unwrap();
        assert_eq!(g.weights[crate::engine::gin::OCCLUSION], 0.4);
        assert_eq!((g.custom[0].id, g.custom[0].weight), (3, 1.0), "old slot 3 reads the map with id 3");
        assert!(g.custom[1..].iter().all(|c| c.id == 0));

        // And the new format round-trips.
        let back = generator_from(&generator_json(&g)).unwrap();
        assert_eq!(back, g);
    }
}
