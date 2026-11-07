//! Material graphs: a node graph that renders tileable textures, node by
//! node, for graph fill layers and graph masks. Nodes are WGSL files (see
//! [`def`]); the built-in library and user nodes share the format.

pub mod builtin;
pub mod def;
pub mod presets;
pub(crate) mod runtime;
pub mod script;
pub mod shading;

pub use def::{Kind, NodeDef, ParamKind, Value};
pub use runtime::Runtime;

use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Channels an Output node can feed (its `usage` parameter, in order).
pub const USAGES: [&str; 5] = ["Base Color", "Roughness", "Metallic", "Height", "Mask"];
pub const OUTPUT_DEF: &str = "output";

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: u64,
    pub def: String,
    /// Canvas position.
    pub pos: [f32; 2],
    /// Values that differ from the definition's defaults.
    pub params: HashMap<String, Value>,
    /// Input name → the node feeding it.
    pub links: HashMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub name: String,
    /// Texture size every node renders at.
    pub res: u32,
    pub nodes: Vec<Node>,
    pub next_id: u64,
    /// Parameters shown in the layer properties: (node, parameter name).
    pub exposed: Vec<(u64, String)>,
    /// The material script this graph was compiled from (see [`script`]),
    /// saved instead of the nodes while the nodes still match it.
    pub script: Option<String>,
    /// Where the script lives, relative to a text project's data folder.
    pub script_file: Option<String>,
    /// The script's named steps and the node each ended at.
    pub steps: Vec<(String, u64)>,
    /// The folder the script's relative image and include paths resolve in
    /// (the project's data folder), kept so saving can recompile it.
    pub script_base: Option<PathBuf>,
}

impl Graph {
    pub fn new(name: &str) -> Graph {
        Graph {
            name: name.to_string(),
            res: 1024,
            nodes: Vec::new(),
            next_id: 1,
            exposed: Vec::new(),
            script: None,
            script_file: None,
            steps: Vec::new(),
            script_base: None,
        }
    }

    /// A starter graph: noise through a gradient map into base color, and
    /// the same noise into roughness.
    pub fn starter(name: &str) -> Graph {
        let mut g = Graph::new(name);
        let noise = g.add("noise", [0.0, 0.0]);
        let map = g.add("gradient_map", [220.0, -60.0]);
        let color = g.add("output", [440.0, -60.0]);
        let levels = g.add("levels", [220.0, 100.0]);
        let rough = g.add("output", [440.0, 100.0]);
        g.connect(noise, map, "input");
        g.connect(map, color, "input");
        g.connect(noise, levels, "input");
        g.connect(levels, rough, "input");
        g.node_mut(levels).unwrap().params.insert("out_low".into(), Value::Float(0.35));
        g.node_mut(levels).unwrap().params.insert("out_high".into(), Value::Float(0.75));
        g.node_mut(rough).unwrap().params.insert("usage".into(), Value::Enum(1));
        g
    }

    pub fn add(&mut self, def: &str, pos: [f32; 2]) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(Node { id, def: def.to_string(), pos, params: HashMap::new(), links: HashMap::new() });
        id
    }

    pub fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn remove(&mut self, id: u64) {
        self.nodes.retain(|n| n.id != id);
        self.exposed.retain(|(n, _)| *n != id);
        for n in &mut self.nodes {
            n.links.retain(|_, from| *from != id);
        }
    }

    /// Whether `to` feeds `from`, directly or not (a link from `from` to
    /// `to` would close a loop).
    fn feeds(&self, to: u64, from: u64) -> bool {
        let mut stack = vec![from];
        let mut seen = Vec::new();
        while let Some(id) = stack.pop() {
            if id == to {
                return true;
            }
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(n) = self.node(id) {
                stack.extend(n.links.values().copied());
            }
        }
        false
    }

    /// Links `from`'s output to `to`'s input; refuses loops.
    pub fn connect(&mut self, from: u64, to: u64, input: &str) -> bool {
        if from == to || self.feeds(to, from) {
            return false;
        }
        match self.node_mut(to) {
            Some(n) => {
                n.links.insert(input.to_string(), from);
                true
            }
            None => false,
        }
    }

    /// Nodes ordered so every node comes after the nodes feeding it.
    pub fn order(&self) -> Vec<u64> {
        fn visit(g: &Graph, id: u64, done: &mut Vec<u64>, active: &mut Vec<u64>) {
            if done.contains(&id) || active.contains(&id) {
                return;
            }
            active.push(id);
            if let Some(n) = g.node(id) {
                let mut inputs: Vec<u64> = n.links.values().copied().collect();
                inputs.sort_unstable();
                for i in inputs {
                    visit(g, i, done, active);
                }
            }
            active.pop();
            done.push(id);
        }
        let mut done = Vec::new();
        for n in &self.nodes {
            visit(self, n.id, &mut done, &mut Vec::new());
        }
        done
    }

    /// The Output node feeding each usage (the first one, if several do).
    pub fn outputs(&self, lib: &Library) -> [Option<u64>; 5] {
        let mut out = [None; 5];
        for n in self.nodes.iter().filter(|n| n.def == OUTPUT_DEF) {
            let usage = match lib.value(n, "usage") {
                Some(Value::Enum(i)) => i,
                _ => 0,
            };
            if usage < out.len() && out[usage].is_none() {
                out[usage] = Some(n.id);
            }
        }
        out
    }

    /// Names the change from `self` to `after`, for the undo history.
    pub fn change_label(&self, after: &Graph) -> String {
        let ids = |g: &Graph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
        let (old, new) = (ids(self), ids(after));
        let added = new.iter().filter(|id| !old.contains(id)).count();
        let removed = old.iter().filter(|id| !new.contains(id)).count();
        let plural = |n: usize, one: &str, many: &str| if n == 1 { one.to_string() } else { format!("{many} ({n})") };
        if added > 0 && removed == 0 {
            return plural(added, "Add node", "Add nodes");
        }
        if removed > 0 && added == 0 {
            return plural(removed, "Delete node", "Delete nodes");
        }
        if added > 0 {
            return "Edit nodes".into();
        }
        let pairs = || self.nodes.iter().filter_map(|a| Some((a, after.node(a.id)?)));
        let links = |pick: fn(usize, usize) -> bool| pairs().any(|(a, b)| a.links != b.links && pick(a.links.len(), b.links.len()));
        if pairs().any(|(a, b)| a.links != b.links) {
            return if links(|a, b| b > a) {
                "Connect".into()
            } else if links(|a, b| b < a) {
                "Disconnect".into()
            } else {
                "Reconnect".into()
            };
        }
        let changed: Vec<&str> = pairs()
            .flat_map(|(a, b)| {
                let mut keys: Vec<&str> = a.params.keys().chain(b.params.keys()).map(String::as_str).collect();
                keys.retain(|k| a.params.get(*k) != b.params.get(*k));
                keys
            })
            .collect();
        if let [one, rest @ ..] = changed.as_slice() {
            return if rest.iter().all(|k| k == one) { format!("Change {}", one.replace('_', " ")) } else { "Change parameters".into() };
        }
        if pairs().any(|(a, b)| a.def != b.def) {
            return "Replace node".into();
        }
        if self.exposed != after.exposed {
            return if after.exposed.len() > self.exposed.len() { "Expose parameter".into() } else { "Hide parameter".into() };
        }
        if pairs().any(|(a, b)| a.pos != b.pos) {
            return "Move nodes".into();
        }
        if self.name != after.name {
            return "Rename graph".into();
        }
        if self.res != after.res {
            return "Change graph size".into();
        }
        "Edit graph".into()
    }

    pub fn to_json(&self, lib: &Library) -> Json {
        // A script graph saves its script while the nodes still match it
        // (positions aside); edited on the canvas, it saves its nodes.
        if let Some(src) = &self.script {
            if script::compile(src, lib, self.script_base.as_deref()).is_ok_and(|g| g.res == self.res && same_nodes(&g, self)) {
                return match &self.script_file {
                    Some(f) => json!({"name": self.name, "script_file": f}),
                    None => json!({"name": self.name, "script": src}),
                };
            }
        }
        let nodes: Vec<Json> = self
            .nodes
            .iter()
            .map(|n| {
                let def = lib.find(&n.def);
                let params: serde_json::Map<String, Json> = n
                    .params
                    .iter()
                    .map(|(k, v)| {
                        let kind = def.and_then(|d| d.params.iter().find(|p| &p.name == k)).map(|p| &p.kind);
                        (k.clone(), value_json(v, kind))
                    })
                    .collect();
                let links: serde_json::Map<String, Json> = n.links.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                json!({"id": n.id, "def": n.def, "pos": n.pos, "params": params, "links": links})
            })
            .collect();
        let exposed: Vec<Json> = self.exposed.iter().map(|(n, p)| json!([n, p])).collect();
        json!({"name": self.name, "res": self.res, "next_id": self.next_id, "nodes": nodes, "exposed": exposed})
    }

    /// Reads a graph; parameters are typed by the definitions in `lib`
    /// (values of unknown nodes or parameters are dropped).
    pub fn from_json(v: &Json, lib: &Library) -> Result<Graph, String> {
        Self::from_json_in(v, lib, None)
    }

    /// Reads a graph; a script graph (`"script"`) compiles, with relative
    /// image paths found in `base`.
    pub fn from_json_in(v: &Json, lib: &Library, base: Option<&Path>) -> Result<Graph, String> {
        if let Some(src) = v["script"].as_str() {
            let mut g = script::compile(src, lib, base)?;
            if let Some(name) = v["name"].as_str() {
                g.name = name.to_string();
            }
            g.script = Some(src.to_string());
            g.script_file = v["script_file"].as_str().map(str::to_string);
            g.script_base = base.map(Path::to_path_buf);
            return Ok(g);
        }
        let mut g = Graph::new(v["name"].as_str().unwrap_or("Graph"));
        g.res = v["res"].as_u64().map_or(1024, |r| (r as u32).clamp(64, 4096));
        for n in v["nodes"].as_array().ok_or("graph has no nodes list")? {
            let id = n["id"].as_u64().ok_or("node without id")?;
            let def = n["def"].as_str().ok_or("node without def")?.to_string();
            let pos = [n["pos"][0].as_f64().unwrap_or(0.0) as f32, n["pos"][1].as_f64().unwrap_or(0.0) as f32];
            let mut params = HashMap::new();
            if let (Some(d), Some(map)) = (lib.find(&def), n["params"].as_object()) {
                for (k, jv) in map {
                    if let Some(p) = d.params.iter().find(|p| &p.name == k) {
                        if let Some(val) = json_value(jv, &p.kind) {
                            params.insert(k.clone(), val);
                        }
                    }
                }
            }
            let links = n["links"].as_object().map_or_else(HashMap::new, |m| {
                m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect()
            });
            g.next_id = g.next_id.max(id + 1);
            g.nodes.push(Node { id, def, pos, params, links });
        }
        g.next_id = g.next_id.max(v["next_id"].as_u64().unwrap_or(1));
        for e in v["exposed"].as_array().into_iter().flatten() {
            if let (Some(n), Some(p)) = (e[0].as_u64(), e[1].as_str()) {
                g.exposed.push((n, p.to_string()));
            }
        }
        Ok(g)
    }
}

fn value_json(v: &Value, kind: Option<&ParamKind>) -> Json {
    match v {
        Value::Float(f) => json!(f),
        Value::Int(i) => json!(i),
        Value::Bool(b) => json!(b),
        Value::Color(c) => json!(c),
        Value::Enum(i) => match kind {
            Some(ParamKind::Enum(opts)) => json!(opts.get(*i).cloned().unwrap_or_default()),
            _ => json!(i),
        },
        Value::Image(p) => json!(p.as_ref().map(|p| p.display().to_string())),
        // Custom bake maps by id ("custom:3"), built-in ones by name.
        Value::MeshMap(i) if *i >= crate::engine::CUSTOM_MAP_VALUE => json!(format!("custom:{}", i - crate::engine::CUSTOM_MAP_VALUE)),
        Value::MeshMap(i) => json!(crate::engine::MAPS.get(*i).map_or("", |m| m.name)),
    }
}

fn json_value(v: &Json, kind: &ParamKind) -> Option<Value> {
    Some(match kind {
        ParamKind::Float { .. } => Value::Float(v.as_f64()? as f32),
        ParamKind::Int { .. } => Value::Int(v.as_i64()? as i32),
        ParamKind::Bool => Value::Bool(v.as_bool()?),
        ParamKind::Color => {
            let a = v.as_array()?;
            Value::Color([0, 1, 2].map(|i| a.get(i).and_then(Json::as_f64).unwrap_or(0.0) as f32))
        }
        ParamKind::Enum(opts) => match v {
            Json::String(s) => Value::Enum(opts.iter().position(|o| o == s)?),
            _ => Value::Enum((v.as_u64()? as usize).min(opts.len().saturating_sub(1))),
        },
        ParamKind::Image => Value::Image(v.as_str().map(PathBuf::from)),
        ParamKind::MeshMap => {
            let name = v.as_str()?;
            match name.strip_prefix("custom:") {
                Some(id) => Value::MeshMap(crate::engine::CUSTOM_MAP_VALUE + id.parse::<usize>().ok()?),
                None => Value::MeshMap(crate::engine::MAPS.iter().position(|m| m.name == name)?),
            }
        }
    })
}

/// Every node definition: the built-ins, then user nodes from a folder
/// (`user/<file stem>`), with the errors of user files that didn't parse.
pub struct Library {
    pub defs: Vec<NodeDef>,
    /// User node file → parse error.
    pub errors: Vec<(PathBuf, String)>,
    pub user_dir: Option<PathBuf>,
}

/// The same nodes, links and parameters (positions aside).
fn same_nodes(a: &Graph, b: &Graph) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.nodes.iter().zip(&b.nodes).all(|(x, y)| x.id == y.id && x.def == y.def && x.params == y.params && x.links == y.links)
}

impl Library {
    pub fn load(user_dir: Option<PathBuf>) -> Library {
        let mut lib = Library { defs: Vec::new(), errors: Vec::new(), user_dir };
        for (id, src) in builtin::BUILTIN {
            lib.defs.push(def::parse(id, src).unwrap_or_else(|e| panic!("built-in node {id}: {e}")));
        }
        lib.reload_user();
        lib
    }

    /// Rereads the user folder's `.wgsl` files.
    pub fn reload_user(&mut self) {
        self.defs.retain(|d| !d.id.starts_with("user/"));
        self.errors.clear();
        let Some(dir) = &self.user_dir else { return };
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut files: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "wgsl")).collect();
        files.sort();
        for path in files {
            let id = format!("user/{}", path.file_stem().unwrap_or_default().to_string_lossy());
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|src| def::parse(&id, &src)) {
                Ok(d) => self.defs.push(d),
                Err(e) => self.errors.push((path, e)),
            }
        }
    }

    pub fn find(&self, id: &str) -> Option<&NodeDef> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// The file a user node lives in.
    pub fn user_path(&self, id: &str) -> Option<PathBuf> {
        Some(self.user_dir.as_ref()?.join(format!("{}.wgsl", id.strip_prefix("user/")?)))
    }

    /// Writes a user node file and reloads the folder. The file is written
    /// even when it doesn't parse, so work isn't lost; the error comes back.
    pub fn save_user(&mut self, stem: &str, source: &str) -> Result<String, String> {
        let dir = self.user_dir.clone().ok_or("no user node folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let id = format!("user/{stem}");
        std::fs::write(dir.join(format!("{stem}.wgsl")), source).map_err(|e| e.to_string())?;
        self.reload_user();
        def::parse(&id, source).map(|_| id)
    }

    /// A node's value for a parameter: its own if set and of the right
    /// kind, else the definition's default.
    pub fn value(&self, node: &Node, name: &str) -> Option<Value> {
        let p = self.find(&node.def)?.params.iter().find(|p| p.name == name)?;
        let own = node.params.get(name).filter(|v| std::mem::discriminant(*v) == std::mem::discriminant(&p.default));
        Some(own.cloned().unwrap_or_else(|| p.default.clone()))
    }

    /// Categories in display order, each with its node ids.
    pub fn categories(&self) -> Vec<(String, Vec<String>)> {
        let order = ["Generators", "Adjust", "Blend", "Filters", "Inputs", "Outputs"];
        let mut cats: Vec<(String, Vec<String>)> = Vec::new();
        for d in &self.defs {
            match cats.iter_mut().find(|(c, _)| *c == d.category) {
                Some((_, ids)) => ids.push(d.id.clone()),
                None => cats.push((d.category.clone(), vec![d.id.clone()])),
            }
        }
        cats.sort_by_key(|(c, _)| order.iter().position(|o| o == c).unwrap_or(order.len()));
        cats
    }
}

/// A new user node's starting source.
pub fn template(name: &str) -> String {
    format!(
        "// @name {name}\n// @category Custom\n// @description What this node does.\n// @output color\n// @input input: color = 0.5\n// @param amount: float(0, 1) = 0.5\n\nfn node(uv: vec2<f32>) -> vec4<f32> {{\n    let c = in_input(uv);\n    return vec4<f32>(c.rgb * params.amount, 1.0);\n}}\n"
    )
}

/// Reads a graph file (`.mpgraph`, JSON).
pub fn load_file(path: &Path, lib: &Library) -> Result<Graph, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Graph::from_json(&v, lib)
}

pub fn save_file(path: &Path, graph: &Graph, lib: &Library) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&graph.to_json(lib)).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses_and_the_starter_round_trips() {
        let lib = Library::load(None);
        assert_eq!(lib.defs.len(), builtin::BUILTIN.len());
        let g = Graph::starter("Test");
        let outs = g.outputs(&lib);
        assert!(outs[0].is_some() && outs[1].is_some(), "{outs:?}");
        let back = Graph::from_json(&g.to_json(&lib), &lib).unwrap();
        assert_eq!(back.nodes.len(), g.nodes.len());
        assert_eq!(back.outputs(&lib), outs);
        let levels = back.nodes.iter().find(|n| n.def == "levels").unwrap();
        assert_eq!(lib.value(levels, "out_low"), Some(Value::Float(0.35)));
        // Topological order: every node after its inputs.
        let order = back.order();
        for n in &back.nodes {
            let at = order.iter().position(|&i| i == n.id).unwrap();
            for from in n.links.values() {
                assert!(order.iter().position(|i| i == from).unwrap() < at);
            }
        }
    }

    #[test]
    fn connect_refuses_loops() {
        let mut g = Graph::new("Loop");
        let a = g.add("invert", [0.0; 2]);
        let b = g.add("invert", [0.0; 2]);
        assert!(g.connect(a, b, "input"));
        assert!(!g.connect(b, a, "input"));
        assert!(!g.connect(a, a, "input"));
    }
}
