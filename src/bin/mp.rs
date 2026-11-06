//! `mp`: Material Painter from the command line.
//!
//! A thin client for the app's MCP server (`src/mcp.rs`): every tool the
//! app offers AI agents is a subcommand, so scripts and agents with a shell
//! can drive the live document without an MCP client. Images that tools
//! return (renders, screenshots) are written to files.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_PORT: u16 = 39831;

const USAGE: &str = "\
mp - drive Material Painter from the command line (talks to the running app)

USAGE
  mp launch [FILE] [--no-wait]   Start the app with agent connections on; FILE is a
                                 project (.mpp/.json) or a mesh (.fbx/.obj)
  mp tools                       List the app's tools and their arguments
  mp state                       Document overview (same as: mp get_state)
  mp <tool> [key=value ...]      Call a tool. Values are JSON when they parse
                                 (2, true, [1,0,0], {\"Curvature\":1}), text otherwise
  mp <tool> --json '{...}'       Arguments as one JSON object
  mp render [key=value ...] [--views LIST]
                                 Render the 3D view. LIST: comma-separated names from
                                 front, back, left, right, top, bottom, three_quarter,
                                 rear_three_quarter, or turntable:N
  mp shot                        Screenshot of the whole app window

OPTIONS
  --out PATH    Where images go: a .png file, or a folder (default: current folder)
  --port N      App port (default: find it on 39831-39840, or $MP_PORT)

EXAMPLES
  mp launch ship.json
  mp add_layer kind=fill preset=\"Steel Painted\" name=Hull
  mp set_mask index=1 generator=\"Edge Wear\" level=0.6
  mp render --views front,three_quarter --out shots/
  mp save_project path=C:/work/ship.json
";

struct Opts {
    port: Option<u16>,
    out: Option<PathBuf>,
    views: Option<String>,
    json: Option<String>,
    no_wait: bool,
    positional: Vec<String>,
}

fn parse_opts(args: Vec<String>) -> Result<Opts, String> {
    let mut o = Opts { port: None, out: None, views: None, json: None, no_wait: false, positional: Vec::new() };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--port" => o.port = Some(value("--port")?.parse().map_err(|_| "--port needs a number")?),
            "--out" | "-o" => o.out = Some(PathBuf::from(value("--out")?)),
            "--views" => o.views = Some(value("--views")?),
            "--json" => o.json = Some(value("--json")?),
            "--no-wait" => o.no_wait = true,
            "-h" | "--help" => o.positional.insert(0, "help".into()),
            _ => o.positional.push(a),
        }
    }
    if o.port.is_none() {
        o.port = std::env::var("MP_PORT").ok().and_then(|p| p.parse().ok());
    }
    Ok(o)
}

/// An absolute path without Windows' `\?\` prefix, which the app's file
/// handling and people reading the output don't need.
fn absolute(f: &str) -> Result<PathBuf, String> {
    let p = std::fs::canonicalize(f).map_err(|e| format!("{f}: {e}"))?;
    let s = p.to_string_lossy();
    Ok(s.strip_prefix(r"\?\").map_or(p.clone(), PathBuf::from))
}

/// `key=value` pairs as a JSON object; values that parse as JSON stay typed.
fn parse_args(pairs: &[String], json_arg: Option<&str>) -> Result<Value, String> {
    let mut obj = match json_arg {
        Some(j) => serde_json::from_str::<Value>(j).map_err(|e| format!("--json: {e}"))?,
        None => json!({}),
    };
    let map = obj.as_object_mut().ok_or("--json must be an object")?;
    for p in pairs {
        let (k, v) = p.split_once('=').ok_or(format!("'{p}' isn't key=value"))?;
        let v = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.to_string()));
        map.insert(k.to_string(), v);
    }
    Ok(obj)
}

// ------------------------------------------------------------------ HTTP

fn post(port: u16, body: &Value) -> Result<(u16, Value), String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let text = body.to_string();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )
    .map_err(|e| e.to_string())?;
    s.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let value = if body.trim().is_empty() { Value::Null } else { serde_json::from_str(body).map_err(|e| format!("bad reply: {e}"))? };
    Ok((status, value))
}

fn rpc(port: u16, method: &str, params: Value) -> Result<Value, String> {
    let (status, v) = post(port, &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))?;
    match status {
        200 => {}
        503 => return Err("Material Painter is running but agent connections are off. Turn on Preferences > Agents, or start it with `mp launch`.".into()),
        s => return Err(format!("the app answered HTTP {s}")),
    }
    if let Some(e) = v.get("error") {
        return Err(e["message"].as_str().unwrap_or("error").to_string());
    }
    Ok(v["result"].clone())
}

/// The port of a running Material Painter.
fn find_app(port: Option<u16>) -> Result<u16, String> {
    let ports: Vec<u16> = match port {
        Some(p) => vec![p],
        None => (0..10).map(|i| DEFAULT_PORT + i).collect(),
    };
    let mut off = None;
    for p in ports {
        let init = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mp", "version": env!("CARGO_PKG_VERSION")}});
        match rpc(p, "initialize", init) {
            Ok(r) if r["serverInfo"]["name"] == "material-painter" => return Ok(p),
            Err(e) if e.contains("agent connections are off") => off = Some(e),
            _ => {}
        }
    }
    Err(off.unwrap_or_else(|| "Material Painter isn't running (or not reachable). Start it with `mp launch [file]`.".into()))
}

// ------------------------------------------------------------------ output

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c).ok_or("bad base64")? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Where the `k`-th image of a call goes.
fn image_path(out: Option<&Path>, stem: &str, k: usize, count: usize) -> PathBuf {
    let numbered = |s: &str| if count > 1 { format!("{s}-{k}.png") } else { format!("{s}.png") };
    match out {
        Some(p) if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) => {
            if count > 1 { p.with_file_name(numbered(&p.file_stem().unwrap_or_default().to_string_lossy())) } else { p.to_path_buf() }
        }
        Some(dir) => dir.join(numbered(stem)),
        None => PathBuf::from(numbered(stem)),
    }
}

/// Prints a tool result's text and saves its images; false if it failed.
fn show(result: &Value, out: Option<&Path>, stem: &str) -> Result<bool, String> {
    let content = result["content"].as_array().cloned().unwrap_or_default();
    let images = content.iter().filter(|c| c["type"] == "image").count();
    let mut k = 0;
    for c in &content {
        match c["type"].as_str() {
            Some("text") => println!("{}", c["text"].as_str().unwrap_or_default()),
            Some("image") => {
                k += 1;
                let path = image_path(out, stem, k, images);
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
                }
                std::fs::write(&path, base64_decode(c["data"].as_str().unwrap_or_default())?).map_err(|e| format!("can't write {}: {e}", path.display()))?;
                println!("{}", path.display());
            }
            _ => {}
        }
    }
    Ok(result["isError"] != true)
}

fn call(port: u16, tool: &str, args: Value) -> Result<Value, String> {
    rpc(port, "tools/call", json!({"name": tool, "arguments": args}))
}

// ------------------------------------------------------------------ commands

/// Named camera angles (radians) for `mp render --views`.
fn view_angles(name: &str) -> Result<Vec<(String, f64, f64)>, String> {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    if let Some(n) = name.strip_prefix("turntable:") {
        let n: usize = n.parse().map_err(|_| format!("'{name}': turntable needs a count, e.g. turntable:8"))?;
        return Ok((0..n.max(1)).map(|i| (format!("turn{i:02}"), 2.0 * PI * i as f64 / n.max(1) as f64, 0.35)).collect());
    }
    let (yaw, pitch) = match name {
        "front" => (0.0, 0.0),
        "back" => (PI, 0.0),
        "left" => (-FRAC_PI_2, 0.0),
        "right" => (FRAC_PI_2, 0.0),
        "top" => (0.0, FRAC_PI_2 - 0.01),
        "bottom" => (0.0, -FRAC_PI_2 + 0.01),
        "three_quarter" => (FRAC_PI_4, 0.4),
        "rear_three_quarter" => (PI - FRAC_PI_4, 0.4),
        _ => return Err(format!("unknown view '{name}'")),
    };
    Ok(vec![(name.to_string(), yaw, pitch)])
}

fn render(port: u16, o: &Opts, pairs: &[String]) -> Result<bool, String> {
    let mut args = parse_args(pairs, o.json.as_deref())?;
    let Some(list) = &o.views else {
        return show(&call(port, "render_view", args)?, o.out.as_deref(), "render");
    };
    // Named views look at the mesh's center from close enough to fill the
    // frame (the mesh fits a 2-unit box), unless told otherwise.
    for (k, v) in [("target", json!([0.0, 0.0, 0.0])), ("distance", json!(2.8))] {
        args.as_object_mut().expect("arguments are an object").entry(k).or_insert(v);
    }
    let mut ok = true;
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        for (stem, yaw, pitch) in view_angles(name)? {
            let mut a = args.clone();
            a["yaw"] = json!(yaw);
            a["pitch"] = json!(pitch);
            let out = match o.out.as_deref() {
                // One file name for several views: number them after it.
                Some(p) if p.extension().is_some_and(|x| x == "png") => {
                    p.with_file_name(format!("{}-{stem}.png", p.file_stem().unwrap_or_default().to_string_lossy()))
                }
                Some(dir) => dir.join(format!("{stem}.png")),
                None => PathBuf::from(format!("{stem}.png")),
            };
            ok &= show(&call(port, "render_view", a)?, Some(&out), &stem)?;
        }
    }
    Ok(ok)
}

fn launch(o: &Opts, file: Option<&String>) -> Result<bool, String> {
    if let Ok(port) = find_app(o.port) {
        println!("Material Painter is already running (port {port}).");
        if let Some(f) = file {
            let path = absolute(f)?;
            let ext = path.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let (tool, args) = if ext == "mpp" || ext == "json" {
                ("open_project", json!({"path": path}))
            } else {
                ("load_mesh", json!({"path": path}))
            };
            return show(&call(port, tool, args)?, None, tool);
        }
        return Ok(true);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.with_file_name(if cfg!(windows) { "material_painter.exe" } else { "material_painter" });
    if !app.is_file() {
        return Err(format!("can't find {} next to mp", app.display()));
    }
    let mut cmd = std::process::Command::new(&app);
    // Detached from this terminal: the app outlives the command.
    cmd.arg("--mcp").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(f) = file {
        cmd.arg(absolute(f)?);
    }
    cmd.spawn().map_err(|e| format!("can't start {}: {e}", app.display()))?;
    if o.no_wait {
        return Ok(true);
    }
    // Waiting on another process to come up: polling is the only signal.
    for _ in 0..240 {
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(port) = find_app(o.port) {
            println!("Material Painter is ready (port {port}).");
            return Ok(true);
        }
    }
    Err("the app started but didn't accept connections within a minute".into())
}

fn list_tools(port: u16) -> Result<bool, String> {
    let tools = rpc(port, "tools/list", json!({}))?;
    for t in tools["tools"].as_array().into_iter().flatten() {
        println!("{}\n    {}", t["name"].as_str().unwrap_or_default(), t["description"].as_str().unwrap_or_default());
        let req: Vec<&str> = t["inputSchema"]["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if let Some(props) = t["inputSchema"]["properties"].as_object() {
            for (k, p) in props {
                let ty = p["type"].as_str().unwrap_or("any");
                let en = p["enum"].as_array().map(|e| format!(" {}", Value::Array(e.clone()))).unwrap_or_default();
                let star = if req.contains(&k.as_str()) { "*" } else { " " };
                println!("      {star}{k} ({ty}{en}) {}", p["description"].as_str().unwrap_or_default());
            }
        }
    }
    Ok(true)
}

fn run(args: Vec<String>) -> Result<bool, String> {
    let o = parse_opts(args)?;
    let Some((cmd, rest)) = o.positional.split_first() else {
        print!("{USAGE}");
        return Ok(true);
    };
    match cmd.as_str() {
        "help" => {
            print!("{USAGE}");
            Ok(true)
        }
        "launch" => launch(&o, rest.first()),
        "tools" => list_tools(find_app(o.port)?),
        "state" => show(&call(find_app(o.port)?, "get_state", json!({}))?, None, "state"),
        "render" => render(find_app(o.port)?, &o, rest),
        "shot" => show(&call(find_app(o.port)?, "screenshot_app", json!({}))?, o.out.as_deref(), "screenshot"),
        tool => {
            let args = parse_args(rest, o.json.as_deref())?;
            show(&call(find_app(o.port)?, tool, args)?, o.out.as_deref(), tool)
        }
    }
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("mp: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_values_keep_json_types() {
        let v = parse_args(&["index=2".into(), "name=Hull plates".into(), "color=[1,0.5,0]".into(), "invert=true".into()], None).unwrap();
        assert_eq!(v, json!({"index": 2, "name": "Hull plates", "color": [1, 0.5, 0], "invert": true}));
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("AQID").unwrap(), [1, 2, 3]);
    }

    #[test]
    fn image_paths() {
        assert_eq!(image_path(Some(Path::new("a/b.png")), "x", 1, 1), PathBuf::from("a/b.png"));
        assert_eq!(image_path(Some(Path::new("a/b.png")), "x", 2, 3), PathBuf::from("a/b-2.png"));
        assert_eq!(image_path(Some(Path::new("shots")), "render", 1, 1), PathBuf::from("shots/render.png"));
    }
}
