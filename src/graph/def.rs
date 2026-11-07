//! Node definitions: a WGSL file whose leading comment header declares the
//! node, followed by `fn node(uv: vec2<f32>) -> vec4<f32>`. Built-in nodes
//! and user nodes use the same format.
//!
//! ```text
//! // @name Blend
//! // @category Blend
//! // @description Mixes two inputs.
//! // @output color                     (or gray)
//! // @input a: color = 0, 0, 0          (value used while unconnected)
//! // @input mask: gray = 1
//! // @param mode: enum(Normal, Multiply, Screen) = Normal
//! // @param opacity: float(0, 1) = 1
//! // @param count: int(1, 16) = 4
//! // @param tint: color = 1, 1, 1
//! // @param flip: bool = false
//! // @param image: image                (a picked file, sampled with img_image(uv))
//! // @param map: meshmap                (a baked mesh map, sampled with mesh_map(uv))
//! ```
//!
//! In the body, inputs read as `in_a(uv)` (vec4 for color, f32 for gray),
//! parameters as `params.opacity`, and `res()` is the output size in texels.
//!
//! A `// @shading` line makes a *shading* node instead: a step of a preview
//! shader (see `graph::shading`), evaluated per pixel of the 3D view rather
//! than rendered to a texture. Its body is `fn node(c: Shade) -> vec4<f32>`;
//! `c` holds the painted surface, the baked maps, the normal, view and light
//! directions (see `Shade` in view.wgsl); inputs read as `in_a(c)` and
//! `has_a()` tells whether input `a` is connected.

/// Value kinds of inputs and outputs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Color,
    Gray,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Int { min: i32, max: i32 },
    Bool,
    Color,
    Enum(Vec<String>),
    /// An image file, bound as a texture.
    Image,
    /// One of the baked mesh maps (`engine::MAPS`), bound as a texture.
    MeshMap,
}

/// A parameter value. Enums hold the option index, images a file path.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Float(f32),
    Int(i32),
    Bool(bool),
    Color([f32; 3]),
    Enum(usize),
    Image(Option<std::path::PathBuf>),
    MeshMap(usize),
}

#[derive(Clone, Debug)]
pub struct Input {
    pub name: String,
    pub kind: Kind,
    pub default: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub default: Value,
}

#[derive(Clone, Debug)]
pub struct NodeDef {
    /// Unique key: the file stem, `user/` prefixed for user nodes.
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub output: Kind,
    pub inputs: Vec<Input>,
    pub params: Vec<Param>,
    /// The WGSL after the header.
    pub body: String,
    /// Full source, for change detection and the code editor.
    pub source: String,
    /// A step of a preview shader rather than a texture node (`@shading`).
    pub shading: bool,
    /// Reads its inputs' whole-image averages through `mean_<input>()`
    /// (`@mean`); the runtime hands it mip-mapped copies of its inputs.
    pub mean: bool,
    /// Reads its grey inputs' whole-image minimum, maximum and average
    /// through `min_<input>()`, `max_<input>()`, `mean_<input>()` (`@minmax`).
    pub minmax: bool,
    /// How many times it renders, each pass reading the last (`@passes`).
    pub passes: Passes,
}

/// A node's render passes (`@passes`): every pass sees `pass_index()` and
/// reads the previous pass with `prev_at(texel)`; the last pass is the output.
#[derive(Clone, Debug, PartialEq)]
pub enum Passes {
    One,
    /// `@passes log2`: log2 of the size plus two (jump flooding: an
    /// initial pass, one per halving step, a final pass).
    Log2,
    Fixed(u32),
    /// `@passes <param>`: the value of an int parameter.
    Param(String),
}

pub const MAX_INPUTS: usize = 8;
pub const MAX_PARAMS: usize = 16;
/// Texture slots for image and mesh map parameters.
pub const MAX_IMAGES: usize = 4;
/// Binding of a multi-pass node's previous pass (after inputs and images).
pub const PREV_BINDING: usize = 2 + MAX_INPUTS + MAX_IMAGES;

fn ident(s: &str) -> Result<String, String> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok { Ok(s.to_string()) } else { Err(format!("'{s}' is not a valid name (letters, digits, _)")) }
}

fn floats(s: &str) -> Result<Vec<f32>, String> {
    s.split(',').map(|v| v.trim().parse::<f32>().map_err(|_| format!("'{}' is not a number", v.trim()))).collect()
}

/// `name: rest = default` → (name, rest, default).
fn split_decl(s: &str) -> Result<(String, String, Option<String>), String> {
    let (name, rest) = s.split_once(':').ok_or(format!("expected 'name: type' in '{s}'"))?;
    let (ty, default) = match rest.split_once('=') {
        Some((t, d)) => (t.trim().to_string(), Some(d.trim().to_string())),
        None => (rest.trim().to_string(), None),
    };
    Ok((ident(name)?, ty, default))
}

fn kind(s: &str) -> Result<Kind, String> {
    match s.trim() {
        "color" => Ok(Kind::Color),
        "gray" => Ok(Kind::Gray),
        k => Err(format!("unknown kind '{k}' (color or gray)")),
    }
}

/// `float(0, 1)` → ("float", ["0", "1"]).
fn call(s: &str) -> (String, Vec<String>) {
    match s.split_once('(') {
        Some((head, args)) => {
            let args = args.trim_end().trim_end_matches(')');
            (head.trim().to_string(), args.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect())
        }
        None => (s.trim().to_string(), Vec::new()),
    }
}

fn param(decl: &str) -> Result<Param, String> {
    let (name, ty, default) = split_decl(decl)?;
    let (head, args) = call(&ty);
    let num = |i: usize, fallback: f32| args.get(i).map_or(Ok(fallback), |a| a.parse::<f32>().map_err(|_| format!("'{a}' is not a number")));
    let (kind, value) = match head.as_str() {
        "float" => {
            let (min, max) = (num(0, 0.0)?, num(1, 1.0)?);
            let v = default.as_deref().map_or(Ok(min), |d| d.parse::<f32>().map_err(|_| format!("'{d}' is not a number")))?;
            (ParamKind::Float { min, max }, Value::Float(v))
        }
        "int" => {
            let (min, max) = (num(0, 0.0)? as i32, num(1, 10.0)? as i32);
            let v = default.as_deref().map_or(Ok(min), |d| d.parse::<i32>().map_err(|_| format!("'{d}' is not a whole number")))?;
            (ParamKind::Int { min, max }, Value::Int(v))
        }
        "bool" => (ParamKind::Bool, Value::Bool(default.as_deref() == Some("true"))),
        "color" => {
            let c = match default.as_deref() {
                Some(d) => match floats(d)?.as_slice() {
                    [v] => [*v; 3],
                    [r, g, b] => [*r, *g, *b],
                    _ => return Err(format!("color default '{d}' needs 1 or 3 numbers")),
                },
                None => [0.5; 3],
            };
            (ParamKind::Color, Value::Color(c))
        }
        "enum" => {
            if args.is_empty() {
                return Err(format!("enum '{name}' has no options"));
            }
            let i = match default.as_deref() {
                Some(d) => args.iter().position(|a| a == d).ok_or(format!("'{d}' is not one of the options of '{name}'"))?,
                None => 0,
            };
            (ParamKind::Enum(args), Value::Enum(i))
        }
        "image" => (ParamKind::Image, Value::Image(None)),
        "meshmap" => (ParamKind::MeshMap, Value::MeshMap(0)),
        t => return Err(format!("unknown parameter type '{t}'")),
    };
    Ok(Param { name, kind, default: value })
}

fn input(decl: &str) -> Result<Input, String> {
    let (name, ty, default) = split_decl(decl)?;
    let kind = kind(&ty)?;
    let default = match default.as_deref() {
        Some(d) => match floats(d)?.as_slice() {
            [v] => [*v, *v, *v, 1.0],
            [r, g, b] => [*r, *g, *b, 1.0],
            [r, g, b, a] => [*r, *g, *b, *a],
            _ => return Err(format!("default '{d}' needs 1, 3 or 4 numbers")),
        },
        None => [0.0, 0.0, 0.0, 1.0],
    };
    Ok(Input { name, kind, default })
}

/// Parses a node file. Errors name the offending header line.
pub fn parse(id: &str, source: &str) -> Result<NodeDef, String> {
    let mut def = NodeDef {
        id: id.to_string(),
        name: id.rsplit('/').next().unwrap_or(id).to_string(),
        category: "Custom".into(),
        description: String::new(),
        output: Kind::Color,
        inputs: Vec::new(),
        params: Vec::new(),
        body: String::new(),
        source: source.to_string(),
        shading: false,
        mean: false,
        minmax: false,
        passes: Passes::One,
    };
    let mut body_start = 0;
    // Whole lines, endings included, so CRLF files find the body too.
    for (n, line) in source.split_inclusive('\n').enumerate() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("//") else {
            if t.is_empty() {
                body_start += line.len();
                continue;
            }
            break;
        };
        body_start += line.len();
        let Some(tag) = rest.trim().strip_prefix('@') else { continue };
        let (key, value) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let value = value.trim();
        let at = |e: String| format!("line {}: {e}", n + 1);
        match key {
            "name" => def.name = value.to_string(),
            "category" => def.category = value.to_string(),
            "description" => def.description = value.to_string(),
            "output" => def.output = kind(value).map_err(at)?,
            "input" => def.inputs.push(input(value).map_err(at)?),
            "param" => def.params.push(param(value).map_err(at)?),
            "shading" => def.shading = true,
            "mean" => def.mean = true,
            "minmax" => def.minmax = true,
            "passes" => {
                def.passes = match value {
                    "log2" => Passes::Log2,
                    v => match v.parse::<u32>() {
                        Ok(n) => Passes::Fixed(n.clamp(1, 1024)),
                        Err(_) => Passes::Param(v.to_string()),
                    },
                }
            }
            k => return Err(at(format!("unknown tag '@{k}'"))),
        }
    }
    def.body = source.get(body_start.min(source.len())..).unwrap_or("").to_string();
    if def.inputs.len() > MAX_INPUTS {
        return Err(format!("at most {MAX_INPUTS} inputs"));
    }
    if def.params.len() > MAX_PARAMS {
        return Err(format!("at most {MAX_PARAMS} parameters"));
    }
    let images = def.params.iter().filter(|p| matches!(p.kind, ParamKind::Image | ParamKind::MeshMap)).count();
    if images > MAX_IMAGES {
        return Err(format!("at most {MAX_IMAGES} image or mesh map parameters"));
    }
    let mut names: Vec<&str> = def.inputs.iter().map(|i| i.name.as_str()).chain(def.params.iter().map(|p| p.name.as_str())).collect();
    names.sort_unstable();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("'{}' is declared twice", w[0]));
    }
    if !def.body.contains("fn node(") {
        return Err(if def.shading {
            "the node needs a 'fn node(c: Shade) -> vec4<f32>'".into()
        } else {
            "the node needs a 'fn node(uv: vec2<f32>) -> vec4<f32>'".into()
        });
    }
    if def.shading && images > 0 {
        return Err("shading nodes read the surface from `c` and can't take image or mesh map parameters".into());
    }
    Ok(def)
}

impl NodeDef {
    /// The texture slot of an image or mesh map parameter.
    pub fn image_slot(&self, param: usize) -> Option<usize> {
        let is_img = |p: &Param| matches!(p.kind, ParamKind::Image | ParamKind::MeshMap);
        is_img(&self.params[param]).then(|| self.params[..param].iter().filter(|p| is_img(p)).count())
    }

    /// The complete fragment shader for this node (without `common.wgsl`,
    /// which `gpu::shader` prepends).
    pub fn shader(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "struct NodeU {\n    p: array<vec4<f32>, 16>,\n    defaults: array<vec4<f32>, 8>,\n    // res, connected input bits, mesh map component bits (2 per slot), unused\n    info: vec4<f32>,\n};\n",
        );
        s.push_str("@group(0) @binding(0) var<uniform> nu: NodeU;\n@group(0) @binding(1) var samp: sampler;\n");
        for (i, inp) in self.inputs.iter().enumerate() {
            s.push_str(&format!("@group(0) @binding({}) var t_{}: texture_2d<f32>;\n", 2 + i, inp.name));
        }
        for i in 0..MAX_IMAGES {
            s.push_str(&format!("@group(0) @binding({}) var t_img{i}: texture_2d<f32>;\n", 2 + MAX_INPUTS + i));
        }
        if self.passes != Passes::One {
            s.push_str(&format!("@group(0) @binding({PREV_BINDING}) var t_prev: texture_2d<f32>;\n"));
            s.push_str(
                "fn pass_index() -> i32 {\n    return i32(nu.info.w) % 4096;\n}\nfn pass_count() -> i32 {\n    return i32(nu.info.w) / 4096;\n}\n// The previous pass at a texel, wrapping (tileable).\nfn prev_at(t: vec2<i32>) -> vec4<f32> {\n    let n = vec2<i32>(textureDimensions(t_prev));\n    return textureLoad(t_prev, ((t % n) + n) % n, 0);\n}\n",
            );
        }
        s.push_str("\nfn res() -> f32 {\n    return nu.info.x;\n}\n\n");
        s.push_str(include_str!("lib.wgsl"));
        // Parameters.
        s.push_str("\nstruct Params {\n    _pad: f32,\n");
        for p in &self.params {
            let ty = match p.kind {
                ParamKind::Float { .. } => "f32",
                ParamKind::Int { .. } | ParamKind::Enum(_) => "i32",
                ParamKind::Bool => "bool",
                ParamKind::Color => "vec3<f32>",
                ParamKind::Image | ParamKind::MeshMap => continue,
            };
            s.push_str(&format!("    {}: {ty},\n", p.name));
        }
        s.push_str("};\nvar<private> params: Params;\n\n");
        for (i, inp) in self.inputs.iter().enumerate() {
            let (ty, pick) = match inp.kind {
                Kind::Color => ("vec4<f32>", ""),
                Kind::Gray => ("f32", ".r"),
            };
            s.push_str(&format!(
                "fn in_{n}(uv: vec2<f32>) -> {ty} {{\n    if ((u32(nu.info.y) & {bit}u) != 0u) {{\n        return textureSampleLevel(t_{n}, samp, uv, 0.0){pick};\n    }}\n    return nu.defaults[{i}]{pick};\n}}\n",
                n = inp.name,
                bit = 1u32 << i
            ));
            if self.mean || self.minmax {
                // The last mip level: the average, or (`@minmax`, grey
                // inputs) min, max and average in r, g, b.
                let stat = |f: &str, last: &str| {
                    format!(
                        "fn {f}_{n}() -> {ty} {{\n    if ((u32(nu.info.y) & {bit}u) != 0u) {{\n        return textureSampleLevel(t_{n}, samp, vec2<f32>(0.5), f32(textureNumLevels(t_{n}) - 1u)){last};\n    }}\n    return nu.defaults[{i}]{pick};\n}}\n",
                        n = inp.name,
                        bit = 1u32 << i,
                    )
                };
                if self.minmax {
                    s.push_str(&stat("min", ".r"));
                    s.push_str(&stat("max", ".g"));
                    s.push_str(&stat("mean", ".b"));
                } else {
                    s.push_str(&stat("mean", pick));
                }
            }
        }
        for (pi, p) in self.params.iter().enumerate() {
            let Some(slot) = self.image_slot(pi) else { continue };
            match p.kind {
                ParamKind::Image => s.push_str(&format!(
                    "fn img_{}(uv: vec2<f32>) -> vec4<f32> {{\n    return textureSampleLevel(t_img{slot}, samp, uv, 0.0);\n}}\n",
                    p.name
                )),
                _ => s.push_str(&format!(
                    "fn {}(uv: vec2<f32>) -> f32 {{\n    let c = (u32(nu.info.z) >> {}u) & 3u;\n    return textureSampleLevel(t_img{slot}, samp, uv, 0.0)[c];\n}}\n",
                    p.name,
                    slot * 2
                )),
            }
        }
        s.push('\n');
        s.push_str(&self.body);
        s.push_str("\n\n@fragment\nfn fs_node(in: FsOut) -> @location(0) vec4<f32> {\n    params._pad = 0.0;\n");
        for (i, p) in self.params.iter().enumerate() {
            let v = match p.kind {
                ParamKind::Float { .. } => format!("nu.p[{i}].x"),
                ParamKind::Int { .. } | ParamKind::Enum(_) => format!("i32(round(nu.p[{i}].x))"),
                ParamKind::Bool => format!("nu.p[{i}].x > 0.5"),
                ParamKind::Color => format!("nu.p[{i}].xyz"),
                ParamKind::Image | ParamKind::MeshMap => continue,
            };
            s.push_str(&format!("    params.{} = {v};\n", p.name));
        }
        s.push_str("    let v = node(in.pos.xy / nu.info.x);\n");
        // A multi-pass node's passes before the last carry its own data in
        // every channel.
        if self.passes != Passes::One {
            s.push_str("    if (pass_index() < pass_count() - 1) {\n        return v;\n    }\n");
        }
        match self.output {
            Kind::Color => s.push_str("    return v;\n}\n"),
            Kind::Gray => s.push_str("    return vec4<f32>(v.rrr, 1.0);\n}\n"),
        }
        s
    }
}

impl NodeDef {
    /// Rewrites the compiler's `wgsl:LINE:COL` locations (lines of the full
    /// generated module) as lines of this node's own file; lines in the
    /// generated wrapper read as "generated code".
    pub fn map_error(&self, msg: &str) -> String {
        // `gpu::shader` joins common.wgsl and the node shader with a newline.
        let before = include_str!("../shaders/common.wgsl").matches('\n').count() + 1;
        let shader = self.shader();
        let gen_body = shader.rfind(self.body.as_str()).map_or(0, |i| shader[..i].matches('\n').count());
        let file_body = self.source[..self.source.len() - self.body.len()].matches('\n').count();
        let mut out = String::new();
        let mut rest = msg;
        while let Some(i) = rest.find("wgsl:") {
            out.push_str(&rest[..i]);
            let tail = &rest[i + 5..];
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<i64>() {
                Ok(line) => {
                    let rel = line - 1 - before as i64 - gen_body as i64;
                    if rel >= 0 {
                        out.push_str(&format!("line {}", file_body as i64 + rel + 1));
                    } else {
                        out.push_str("generated code");
                    }
                    rest = &tail[digits.len()..];
                }
                Err(_) => {
                    out.push_str("wgsl:");
                    rest = tail;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// A parameter value packed into its uniform slot.
pub fn pack(v: &Value) -> [f32; 4] {
    match v {
        Value::Float(f) => [*f, 0.0, 0.0, 0.0],
        Value::Int(i) => [*i as f32, 0.0, 0.0, 0.0],
        Value::Bool(b) => [if *b { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        Value::Color(c) => [c[0], c[1], c[2], 1.0],
        Value::Enum(i) => [*i as f32, 0.0, 0.0, 0.0],
        Value::Image(_) | Value::MeshMap(_) => [0.0; 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLEND: &str = "// @name Blend
// @category Blend
// @output color
// @input a: color = 0
// @input mask: gray = 1
// @param mode: enum(Normal, Multiply) = Multiply
// @param opacity: float(0, 2) = 0.5
// @param count: int(1, 8) = 3
// @param tint: color = 1, 0.5, 0
// @param flip: bool = true
// @param map: meshmap

fn node(uv: vec2<f32>) -> vec4<f32> {
    return in_a(uv) * in_mask(uv) * params.opacity;
}
";

    #[test]
    fn parses_the_header() {
        let d = parse("blend", BLEND).unwrap();
        assert_eq!((d.name.as_str(), d.category.as_str(), d.output), ("Blend", "Blend", Kind::Color));
        assert_eq!(d.inputs.len(), 2);
        assert_eq!(d.inputs[1].kind, Kind::Gray);
        assert_eq!(d.inputs[1].default, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(d.params[0].default, Value::Enum(1));
        assert_eq!(d.params[1].kind, ParamKind::Float { min: 0.0, max: 2.0 });
        assert_eq!(d.params[2].default, Value::Int(3));
        assert_eq!(d.params[3].default, Value::Color([1.0, 0.5, 0.0]));
        assert_eq!(d.params[4].default, Value::Bool(true));
        assert_eq!(d.image_slot(5), Some(0));
        assert!(d.body.trim_start().starts_with("fn node("));
        let src = d.shader();
        assert!(src.contains("params.opacity = nu.p[1].x;"));
        assert!(src.contains("fn in_mask(uv: vec2<f32>) -> f32"));
        assert!(src.contains("fn map(uv: vec2<f32>) -> f32"));
    }

    #[test]
    fn reports_bad_headers() {
        let e = parse("x", "// @param a: float(0, 1) = hello\nfn node(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }").unwrap_err();
        assert!(e.contains("line 1") && e.contains("hello"), "{e}");
        let e = parse("x", "// @input 1a: color\nfn node(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }").unwrap_err();
        assert!(e.contains("valid name"), "{e}");
        assert!(parse("x", "// @name X\n").unwrap_err().contains("fn node"));
        let e = parse("x", "// @input a: color\n// @param a: bool\nfn node(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0); }").unwrap_err();
        assert!(e.contains("twice"), "{e}");
    }
}
