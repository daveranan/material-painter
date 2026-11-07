//! Material scripts: a material graph written as text, one named step per
//! line, compiled onto the same nodes the canvas uses.
//!
//! ```text
//! size 1024
//! base   = noise(perlin, scale=3) + 0.3 * noise(perlin, scale=12, seed=4)
//! stones = tile_sampler(disc, columns=16, size_random=0.4) | blur(2)
//! height = max(base, stones)
//! output height = height
//! output base_color = gradient_map(height, dark=#4a3b2a, light=#c2ab86)
//! ```
//!
//! - A call is a node (`noise`, `blur`, any node id, `user/...` nodes) or a
//!   function defined with `fn`. Positional arguments that are images fill
//!   the node's inputs in order; plain values (numbers, colors, names) fill
//!   its parameters in order. `name=value` sets an input or parameter by
//!   name. Enum options are written as names (`perlin`, `lighten`).
//! - `a | f(...)` feeds `a` into `f`'s first input.
//! - `+ - * /` between images (or an image and a number) make math nodes,
//!   unclamped; `min`, `max` too.
//! - `output <channel> = expr`: base_color, roughness, metallic, height, mask.
//! - `fn name(x, amount=0.5) { ... return expr }` defines a reusable step;
//!   its body sees its parameters and the script's earlier steps.
//! - `size N` sets the texture size; `#` starts a comment.

use super::def::{Kind, NodeDef, ParamKind, Value};
use super::{Graph, Library, USAGES};
use std::collections::HashMap;
use std::path::Path;

/// Functions every script can call, written in the script language itself.
/// (A function here hides a node of the same name: keep them distinct.)
const PRELUDE: &str = "
fn mix(a, b, mask) { return blend(foreground=b, background=a, mask=mask) }
fn clamp01(x) { return min(max(x, 0), 1) }
";

/// Compiles a script into a graph. `base` resolves relative image paths
/// (the project's data folder).
pub fn compile(source: &str, lib: &Library, base: Option<&Path>) -> Result<Graph, String> {
    let mut c = Compiler { lib, base, graph: Graph::new("Script"), fns: HashMap::new(), globals: HashMap::new(), steps: Vec::new() };
    let prelude = parse(&tokenize(PRELUDE).map_err(|e| format!("prelude: {e}"))?).map_err(|e| format!("prelude: {e}"))?;
    c.run(&prelude, true)?;
    let stmts = parse(&tokenize(source)?)?;
    c.run(&stmts, false)?;
    layout(&mut c.graph);
    c.graph.steps = c.steps;
    Ok(c.graph)
}

// ------------------------------------------------------------------ tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Num(f32),
    Str(String),
    Color([f32; 3]),
    Sym(char),
    Newline,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
}

fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut depth) = (0, 1, 0i32);
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '#' if i + 1 < chars.len() && chars[i + 1].is_ascii_hexdigit() && is_color_at(&chars, i) => {
                let hex: String = chars[i + 1..i + 7].iter().collect();
                let v = u32::from_str_radix(&hex, 16).map_err(|_| format!("line {line}: bad color #{hex}"))?;
                let c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map(|x| x as f32 / 255.0);
                out.push(Token { tok: Tok::Color(c), line });
                i += 7;
            }
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\n' => {
                if depth == 0 {
                    out.push(Token { tok: Tok::Newline, line });
                }
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '"' => {
                let start = i + 1;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\n' {
                        return Err(format!("line {line}: unclosed string"));
                    }
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(format!("line {line}: unclosed string"));
                }
                out.push(Token { tok: Tok::Str(chars[start..i].iter().collect()), line });
                i += 1;
            }
            c if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                let n = s.parse::<f32>().map_err(|_| format!("line {line}: bad number {s}"))?;
                out.push(Token { tok: Tok::Num(n), line });
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '/') {
                    i += 1;
                }
                out.push(Token { tok: Tok::Ident(chars[start..i].iter().collect()), line });
            }
            '(' | '[' => {
                depth += 1;
                out.push(Token { tok: Tok::Sym(ch), line });
                i += 1;
            }
            ')' | ']' => {
                depth -= 1;
                out.push(Token { tok: Tok::Sym(ch), line });
                i += 1;
            }
            '=' | ',' | '|' | '+' | '-' | '*' | '/' | '{' | '}' => {
                out.push(Token { tok: Tok::Sym(ch), line });
                i += 1;
            }
            other => return Err(format!("line {line}: unexpected '{other}'")),
        }
    }
    out.push(Token { tok: Tok::Newline, line });
    Ok(out)
}

/// `#rrggbb` followed by a non-word character is a color; anything else
/// after `#` is a comment.
fn is_color_at(chars: &[char], i: usize) -> bool {
    i + 7 <= chars.len()
        && chars[i + 1..i + 7].iter().all(|c| c.is_ascii_hexdigit())
        && chars.get(i + 7).is_none_or(|c| !c.is_alphanumeric() && *c != '_')
}

// ------------------------------------------------------------------ syntax

#[derive(Clone, Debug)]
enum Expr {
    Num(f32),
    Str(String),
    Color([f32; 3]),
    Ident(String),
    Call { name: String, args: Vec<Arg>, line: usize },
    Bin { op: char, a: Box<Expr>, b: Box<Expr>, line: usize },
    Neg(Box<Expr>, usize),
}

#[derive(Clone, Debug)]
struct Arg {
    name: Option<String>,
    value: Expr,
}

#[derive(Clone, Debug)]
struct FnDef {
    params: Vec<(String, Option<Expr>)>,
    body: Vec<Stmt>,
    ret: Expr,
}

#[derive(Clone, Debug)]
enum Stmt {
    Size(u32),
    Name(String),
    Assign(String, Expr),
    Output(String, Expr, usize),
    Fn(String, FnDef),
}

struct Parser<'a> {
    t: &'a [Token],
    i: usize,
}

fn parse(tokens: &[Token]) -> Result<Vec<Stmt>, String> {
    let mut p = Parser { t: tokens, i: 0 };
    let mut out = Vec::new();
    while p.i < tokens.len() {
        if p.eat(&Tok::Newline) {
            continue;
        }
        out.push(p.stmt()?);
    }
    Ok(out)
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        self.t.get(self.i).map_or(&Tok::Newline, |t| &t.tok)
    }

    fn line(&self) -> usize {
        self.t.get(self.i).or(self.t.last()).map_or(1, |t| t.line)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(&Tok::Sym(c)) { Ok(()) } else { Err(format!("line {}: expected '{c}'", self.line())) }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                self.i += 1;
                Ok(s)
            }
            other => Err(format!("line {}: expected a name, found {}", self.line(), show(&other))),
        }
    }

    fn end_of_statement(&mut self) -> Result<(), String> {
        if self.eat(&Tok::Newline) || matches!(self.peek(), Tok::Sym('}')) {
            Ok(())
        } else {
            Err(format!("line {}: unexpected {} (one step per line)", self.line(), show(self.peek())))
        }
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let word = self.ident()?;
        let s = match word.as_str() {
            "size" => match self.peek().clone() {
                Tok::Num(n) => {
                    self.i += 1;
                    Stmt::Size(n as u32)
                }
                _ => return Err(format!("line {line}: size needs a number")),
            },
            "name" => match self.peek().clone() {
                Tok::Str(s) => {
                    self.i += 1;
                    Stmt::Name(s)
                }
                _ => return Err(format!("line {line}: name needs a \"string\"")),
            },
            "output" => {
                let ch = self.ident()?;
                self.expect('=')?;
                Stmt::Output(ch, self.expr()?, line)
            }
            "fn" => {
                let name = self.ident()?;
                self.expect('(')?;
                let mut params = Vec::new();
                while !self.eat(&Tok::Sym(')')) {
                    let p = self.ident()?;
                    let default = if self.eat(&Tok::Sym('=')) { Some(self.expr()?) } else { None };
                    params.push((p, default));
                    if !self.eat(&Tok::Sym(',')) {
                        self.expect(')')?;
                        break;
                    }
                }
                self.expect('{')?;
                let mut body = Vec::new();
                loop {
                    if self.eat(&Tok::Newline) {
                        continue;
                    }
                    if matches!(self.peek(), Tok::Ident(w) if w == "return") {
                        self.i += 1;
                        let ret = self.expr()?;
                        while self.eat(&Tok::Newline) {}
                        self.expect('}')?;
                        return Ok(Stmt::Fn(name, FnDef { params, body, ret }));
                    }
                    if self.i >= self.t.len() || matches!(self.peek(), Tok::Sym('}')) {
                        return Err(format!("line {line}: fn {name} needs a 'return'"));
                    }
                    body.push(self.stmt()?);
                }
            }
            _ => {
                self.expect('=').map_err(|_| format!("line {line}: expected '{word} = ...'"))?;
                Stmt::Assign(word, self.expr()?)
            }
        };
        self.end_of_statement()?;
        Ok(s)
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.sum()?;
        while self.eat(&Tok::Sym('|')) {
            let line = self.line();
            let name = self.ident()?;
            let mut args = if matches!(self.peek(), Tok::Sym('(')) { self.args()? } else { Vec::new() };
            args.insert(0, Arg { name: None, value: left });
            left = Expr::Call { name, args, line };
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Expr, String> {
        let mut a = self.term()?;
        loop {
            let line = self.line();
            let op = match self.peek() {
                Tok::Sym(c @ ('+' | '-')) => *c,
                _ => return Ok(a),
            };
            self.i += 1;
            let b = self.term()?;
            a = Expr::Bin { op, a: Box::new(a), b: Box::new(b), line };
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut a = self.unary()?;
        loop {
            let line = self.line();
            let op = match self.peek() {
                Tok::Sym(c @ ('*' | '/')) => *c,
                _ => return Ok(a),
            };
            self.i += 1;
            let b = self.unary()?;
            a = Expr::Bin { op, a: Box::new(a), b: Box::new(b), line };
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        if self.eat(&Tok::Sym('-')) {
            return Ok(Expr::Neg(Box::new(self.unary()?), line));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Num(n) => {
                self.i += 1;
                Ok(Expr::Num(n))
            }
            Tok::Str(s) => {
                self.i += 1;
                Ok(Expr::Str(s))
            }
            Tok::Color(c) => {
                self.i += 1;
                Ok(Expr::Color(c))
            }
            Tok::Ident(name) => {
                self.i += 1;
                if matches!(self.peek(), Tok::Sym('(')) {
                    let args = self.args()?;
                    Ok(Expr::Call { name, args, line })
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            Tok::Sym('(') => {
                self.i += 1;
                let e = self.expr()?;
                self.expect(')')?;
                Ok(e)
            }
            other => Err(format!("line {line}: unexpected {}", show(&other))),
        }
    }

    fn args(&mut self) -> Result<Vec<Arg>, String> {
        self.expect('(')?;
        let mut args = Vec::new();
        while !self.eat(&Tok::Sym(')')) {
            let named = matches!(self.peek(), Tok::Ident(_)) && self.t.get(self.i + 1).is_some_and(|t| t.tok == Tok::Sym('='));
            let name = if named {
                let n = self.ident()?;
                self.i += 1;
                Some(n)
            } else {
                None
            };
            args.push(Arg { name, value: self.expr()? });
            if !self.eat(&Tok::Sym(',')) {
                self.expect(')')?;
                break;
            }
        }
        Ok(args)
    }
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("'{s}'"),
        Tok::Num(n) => format!("{n}"),
        Tok::Str(s) => format!("\"{s}\""),
        Tok::Color(_) => "a color".into(),
        Tok::Sym(c) => format!("'{c}'"),
        Tok::Newline => "the end of the line".into(),
    }
}

// ------------------------------------------------------------------ compile

/// A value while compiling: an image (a node's output) or a plain value.
#[derive(Clone, Debug)]
enum Val {
    Node(u64),
    Num(f32),
    Str(String),
    Color([f32; 3]),
    /// A bare word: an enum option, a mesh map name, `true`/`false`.
    Word(String),
}

struct Compiler<'a> {
    lib: &'a Library,
    base: Option<&'a Path>,
    graph: Graph,
    fns: HashMap<String, FnDef>,
    globals: HashMap<String, Val>,
    steps: Vec<(String, u64)>,
}

/// Names compared loosely: case, spaces, underscores and `&` don't matter.
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

impl Compiler<'_> {
    fn run(&mut self, stmts: &[Stmt], prelude: bool) -> Result<(), String> {
        for s in stmts {
            match s {
                Stmt::Size(n) => self.graph.res = (*n).clamp(64, 4096),
                Stmt::Name(n) => self.graph.name = n.clone(),
                Stmt::Fn(name, f) => {
                    self.fns.insert(name.clone(), f.clone());
                }
                Stmt::Assign(name, e) => {
                    let v = self.eval(e, &HashMap::new())?;
                    if let (Val::Node(id), false) = (&v, prelude) {
                        self.steps.retain(|(n, _)| n != name);
                        self.steps.push((name.clone(), *id));
                    }
                    self.globals.insert(name.clone(), v);
                }
                Stmt::Output(ch, e, line) => {
                    let usage = match norm(ch).as_str() {
                        "basecolor" | "color" | "albedo" => 0,
                        "roughness" => 1,
                        "metallic" | "metal" => 2,
                        "height" => 3,
                        "mask" => 4,
                        _ => return Err(format!("line {line}: unknown output '{ch}' (base_color, roughness, metallic, height, mask)")),
                    };
                    let v = self.eval(e, &HashMap::new())?;
                    let src = self.as_node(v, *line)?;
                    let out = self.graph.add(super::OUTPUT_DEF, [0.0; 2]);
                    self.graph.node_mut(out).expect("just added").params.insert("usage".into(), Value::Enum(usage));
                    self.graph.connect(src, out, "input");
                    debug_assert!(usage < USAGES.len());
                }
            }
        }
        Ok(())
    }

    fn lookup(&self, name: &str, locals: &HashMap<String, Val>) -> Option<Val> {
        locals.get(name).or_else(|| self.globals.get(name)).cloned()
    }

    fn eval(&mut self, e: &Expr, locals: &HashMap<String, Val>) -> Result<Val, String> {
        Ok(match e {
            Expr::Num(n) => Val::Num(*n),
            Expr::Str(s) => Val::Str(s.clone()),
            Expr::Color(c) => Val::Color(*c),
            Expr::Ident(name) => self.lookup(name, locals).unwrap_or_else(|| Val::Word(name.clone())),
            Expr::Neg(x, line) => match self.eval(x, locals)? {
                Val::Num(n) => Val::Num(-n),
                v => {
                    let n = self.as_node(v, *line)?;
                    self.math("Multiply", n, Operand::Num(-1.0), *line)?
                }
            },
            Expr::Bin { op, a, b, line } => {
                let (a, b) = (self.eval(a, locals)?, self.eval(b, locals)?);
                self.binary(*op, a, b, *line)?
            }
            Expr::Call { name, args, line } => self.call(name, args, locals, *line)?,
        })
    }

    fn binary(&mut self, op: char, a: Val, b: Val, line: usize) -> Result<Val, String> {
        let name = match op {
            '+' => "Add",
            '-' => "Subtract",
            '*' => "Multiply",
            _ => "Divide",
        };
        Ok(match (a, b) {
            (Val::Num(x), Val::Num(y)) => Val::Num(match op {
                '+' => x + y,
                '-' => x - y,
                '*' => x * y,
                _ => x / y,
            }),
            (Val::Num(x), b) if op == '+' || op == '*' => {
                let n = self.as_node(b, line)?;
                self.math(name, n, Operand::Num(x), line)?
            }
            (a, Val::Num(y)) => {
                let n = self.as_node(a, line)?;
                self.math(name, n, Operand::Num(y), line)?
            }
            (a, b) => {
                let (na, nb) = (self.as_node(a, line)?, self.as_node(b, line)?);
                self.math(name, na, Operand::Node(nb), line)?
            }
        })
    }

    fn math(&mut self, op: &str, a: u64, b: Operand, line: usize) -> Result<Val, String> {
        let def = self.def("math", line)?;
        let ops = match &def.params.iter().find(|p| p.name == "op").map(|p| &p.kind) {
            Some(ParamKind::Enum(v)) => v.clone(),
            _ => return Err("the math node has no 'op'".into()),
        };
        let idx = ops.iter().position(|o| o == op).ok_or(format!("math has no {op}"))?;
        let n = self.graph.add("math", [0.0; 2]);
        let node = self.graph.node_mut(n).expect("just added");
        node.params.insert("op".into(), Value::Enum(idx));
        node.params.insert("clamp_result".into(), Value::Bool(false));
        match b {
            Operand::Num(v) => {
                node.params.insert("value".into(), Value::Float(v));
            }
            Operand::Node(id) => {
                self.graph.connect(id, n, "b");
            }
        }
        self.graph.connect(a, n, "a");
        Ok(Val::Node(n))
    }

    fn def(&self, name: &str, line: usize) -> Result<&NodeDef, String> {
        if let Some(d) = self.lib.find(name).filter(|d| !d.shading) {
            return Ok(d);
        }
        let known = self.lib.defs.iter().filter(|d| !d.shading).map(|d| d.id.as_str()).chain(self.fns.keys().map(String::as_str));
        let best = known.map(|k| (distance(name, k), k)).min();
        let hint = match best {
            Some((d, k)) if d <= 3 => format!("; did you mean '{k}'?"),
            _ => String::new(),
        };
        Err(format!("line {line}: unknown node or function '{name}'{hint}"))
    }

    /// An image for an input: a node as is, a number or color as a flat color node.
    fn as_node(&mut self, v: Val, line: usize) -> Result<u64, String> {
        let c = match v {
            Val::Node(n) => return Ok(n),
            Val::Num(x) => [x; 3],
            Val::Color(c) => c,
            Val::Str(s) | Val::Word(s) => return Err(format!("line {line}: '{s}' isn't an image or a step defined above")),
        };
        let n = self.graph.add("color", [0.0; 2]);
        self.graph.node_mut(n).expect("just added").params.insert("color".into(), Value::Color(c));
        Ok(n)
    }

    fn call(&mut self, name: &str, args: &[Arg], locals: &HashMap<String, Val>, line: usize) -> Result<Val, String> {
        if name == "min" || name == "max" {
            if args.len() != 2 {
                return Err(format!("line {line}: {name} takes two values"));
            }
            let (a, b) = (self.eval(&args[0].value, locals)?, self.eval(&args[1].value, locals)?);
            return match (a, b) {
                (Val::Num(x), Val::Num(y)) => Ok(Val::Num(if name == "min" { x.min(y) } else { x.max(y) })),
                (Val::Num(x), b) | (b, Val::Num(x)) => {
                    let n = self.as_node(b, line)?;
                    self.math(if name == "min" { "Min" } else { "Max" }, n, Operand::Num(x), line)
                }
                (a, b) => {
                    let (na, nb) = (self.as_node(a, line)?, self.as_node(b, line)?);
                    self.math(if name == "min" { "Min" } else { "Max" }, na, Operand::Node(nb), line)
                }
            };
        }
        if let Some(f) = self.fns.get(name).cloned() {
            return self.call_fn(name, &f, args, locals, line);
        }
        let def = self.def(name, line)?.clone();
        let mut values = Vec::with_capacity(args.len());
        for a in args {
            values.push((a.name.clone(), self.eval(&a.value, locals)?));
        }
        let n = self.graph.add(&def.id, [0.0; 2]);
        let mut next_input = 0;
        let mut next_param = 0;
        let settable: Vec<usize> = (0..def.params.len()).collect();
        let mut set_params: Vec<usize> = Vec::new();
        for (arg_name, v) in values {
            let target = match &arg_name {
                Some(an) => {
                    let k = norm(an);
                    if let Some(i) = def.inputs.iter().position(|inp| norm(&inp.name) == k) {
                        Target::Input(i)
                    } else if let Some(p) = def.params.iter().position(|p| norm(&p.name) == k) {
                        Target::Param(p)
                    } else {
                        let ins: Vec<&str> = def.inputs.iter().map(|i| i.name.as_str()).collect();
                        let ps: Vec<&str> = def.params.iter().map(|p| p.name.as_str()).collect();
                        return Err(format!("line {line}: {} has no '{an}' (inputs: {}; parameters: {})", def.id, ins.join(", "), ps.join(", ")));
                    }
                }
                None if matches!(v, Val::Node(_)) => {
                    if next_input >= def.inputs.len() {
                        return Err(format!("line {line}: {} takes {} image input(s)", def.id, def.inputs.len()));
                    }
                    next_input += 1;
                    Target::Input(next_input - 1)
                }
                None => {
                    while next_param < settable.len() && set_params.contains(&settable[next_param]) {
                        next_param += 1;
                    }
                    if next_param >= settable.len() {
                        return Err(format!("line {line}: too many values for {}", def.id));
                    }
                    next_param += 1;
                    Target::Param(settable[next_param - 1])
                }
            };
            match target {
                Target::Input(i) => {
                    let src = self.as_node(v, line)?;
                    let input = &def.inputs[i];
                    if input.kind == Kind::Gray || input.kind == Kind::Color {
                        self.graph.connect(src, n, &input.name);
                    }
                }
                Target::Param(p) => {
                    let param = &def.params[p];
                    let value = self.param_value(&def, &param.name, &param.kind, v, line)?;
                    self.graph.node_mut(n).expect("just added").params.insert(param.name.clone(), value);
                    set_params.push(p);
                }
            }
        }
        Ok(Val::Node(n))
    }

    fn call_fn(&mut self, name: &str, f: &FnDef, args: &[Arg], caller: &HashMap<String, Val>, line: usize) -> Result<Val, String> {
        let mut locals: HashMap<String, Val> = HashMap::new();
        for a in args {
            let v = self.eval(&a.value, caller)?;
            let pname = match &a.name {
                Some(n) => {
                    if !f.params.iter().any(|(p, _)| p == n) {
                        return Err(format!("line {line}: {name} has no parameter '{n}'"));
                    }
                    n.clone()
                }
                None => {
                    let Some((p, _)) = f.params.iter().find(|(p, _)| !locals.contains_key(p)) else {
                        return Err(format!("line {line}: too many values for {name}"));
                    };
                    p.clone()
                }
            };
            locals.insert(pname, v);
        }
        for (p, default) in &f.params {
            if !locals.contains_key(p) {
                let d = default.as_ref().ok_or(format!("line {line}: {name} needs '{p}'"))?;
                let v = self.eval(d, &locals)?;
                locals.insert(p.clone(), v);
            }
        }
        for s in &f.body {
            match s {
                Stmt::Assign(n, e) => {
                    let v = self.eval(e, &locals)?;
                    locals.insert(n.clone(), v);
                }
                _ => return Err(format!("line {line}: only 'name = ...' steps go inside fn {name}")),
            }
        }
        self.eval(&f.ret, &locals)
    }

    fn param_value(&self, def: &NodeDef, pname: &str, kind: &ParamKind, v: Val, line: usize) -> Result<Value, String> {
        let bad = |what: &str| format!("line {line}: {}.{pname} needs {what}", def.id);
        Ok(match kind {
            ParamKind::Float { .. } => match v {
                Val::Num(x) => Value::Float(x),
                _ => return Err(bad("a number")),
            },
            ParamKind::Int { .. } => match v {
                Val::Num(x) => Value::Int(x.round() as i32),
                _ => return Err(bad("a whole number")),
            },
            ParamKind::Bool => match v {
                Val::Word(w) if w == "true" => Value::Bool(true),
                Val::Word(w) if w == "false" => Value::Bool(false),
                Val::Num(x) => Value::Bool(x != 0.0),
                _ => return Err(bad("true or false")),
            },
            ParamKind::Color => match v {
                Val::Color(c) => Value::Color(c),
                Val::Num(x) => Value::Color([x; 3]),
                _ => return Err(bad("a #rrggbb color or a grey number")),
            },
            ParamKind::Enum(opts) => match v {
                Val::Word(w) | Val::Str(w) => {
                    let k = norm(&w);
                    Value::Enum(opts.iter().position(|o| norm(o) == k).ok_or_else(|| bad(&format!("one of: {}", opts.join(", "))))?)
                }
                Val::Num(x) if (x as usize) < opts.len() => Value::Enum(x as usize),
                _ => return Err(bad(&format!("one of: {}", opts.join(", ")))),
            },
            ParamKind::Image => match v {
                Val::Str(s) => {
                    let p = std::path::PathBuf::from(&s);
                    let p = match self.base {
                        Some(b) if p.is_relative() => b.join(p),
                        _ => p,
                    };
                    Value::Image(Some(p))
                }
                _ => return Err(bad("an image path in quotes")),
            },
            ParamKind::MeshMap => match v {
                Val::Word(w) | Val::Str(w) => {
                    let k = norm(&w);
                    let names: Vec<&str> = crate::engine::MAPS.iter().map(|m| m.name).collect();
                    Value::MeshMap(names.iter().position(|m| norm(m) == k).ok_or_else(|| bad(&format!("one of: {}", names.join(", "))))?)
                }
                _ => return Err(bad("a mesh map name")),
            },
        })
    }
}

enum Operand {
    Num(f32),
    Node(u64),
}

enum Target {
    Input(usize),
    Param(usize),
}

/// Lays the compiled nodes out left to right by depth, so the canvas reads
/// like the script.
fn layout(g: &mut Graph) {
    let mut depth: HashMap<u64, usize> = HashMap::new();
    // Nodes are added after their inputs, so one pass in order suffices.
    for n in &g.nodes {
        let d = n.links.values().filter_map(|src| depth.get(src)).max().map_or(0, |d| d + 1);
        depth.insert(n.id, d);
    }
    let mut rows: HashMap<usize, usize> = HashMap::new();
    for n in &mut g.nodes {
        let d = depth[&n.id];
        let row = rows.entry(d).or_insert(0);
        n.pos = [d as f32 * 220.0, *row as f32 * 140.0];
        *row += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        Library::load(None)
    }

    #[test]
    fn a_script_compiles_to_nodes_outputs_and_steps() {
        let src = "
size 512
# a comment, and a color that isn't one: #notacolor
base = noise(perlin, scale=3) + 0.3 * noise(perlin, scale=12, seed=4)
stones = tile_sampler(disc, columns=16, size_random=0.4) | blur(2)
height = max(base, stones)
output height = height
output base_color = gradient_map(height, dark=#4a3b2a, light=#c2ab86)
";
        let g = compile(src, &lib(), None).unwrap();
        assert_eq!(g.res, 512);
        let outs: Vec<_> = g.nodes.iter().filter(|n| n.def == "output").collect();
        assert_eq!(outs.len(), 2);
        assert_eq!(g.steps.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["base", "stones", "height"]);
        let noise = g.nodes.iter().find(|n| n.def == "noise" && n.params.get("seed") == Some(&Value::Int(4))).unwrap();
        assert_eq!(noise.params.get("scale"), Some(&Value::Int(12)));
        let blur = g.nodes.iter().find(|n| n.def == "blur").unwrap();
        assert_eq!(blur.params.get("radius"), Some(&Value::Float(2.0)));
        let ts = g.nodes.iter().find(|n| n.def == "tile_sampler").unwrap();
        assert_eq!(blur.links.get("input"), Some(&ts.id));
        let gm = g.nodes.iter().find(|n| n.def == "gradient_map").unwrap();
        assert_eq!(gm.params.get("dark"), Some(&Value::Color([0x4a as f32 / 255.0, 0x3b as f32 / 255.0, 0x2a as f32 / 255.0])));
    }

    #[test]
    fn functions_and_the_prelude_expand_inline() {
        let src = "
fn crevices(h, radius=12, level=0.45) {
  hp = highpass(h, radius)
  return hp | histogram_scan(position=level, invert=true)
}
h = noise(ridged, scale=8)
dirt = crevices(h, level=0.4)
output mask = dirt
";
        let g = compile(src, &lib(), None).unwrap();
        let scan = g.nodes.iter().find(|n| n.def == "histogram_scan").unwrap();
        assert_eq!(scan.params.get("position"), Some(&Value::Float(0.4)));
        assert_eq!(scan.params.get("invert"), Some(&Value::Bool(true)));
        assert!(g.nodes.iter().any(|n| n.def == "highpass" && n.params.get("radius") == Some(&Value::Float(12.0))));
        // Numbers and colors given to mix become flat color inputs.
        let g = compile("m = noise()
c = mix(0.85, #7d6a50, m)
", &lib(), None).unwrap();
        let blend = g.nodes.iter().find(|n| n.def == "blend").unwrap();
        assert_eq!(blend.links.len(), 3);
        assert!(blend.params.is_empty());
    }

    #[test]
    fn mistakes_name_the_line_and_suggest() {
        let e = compile("a = nosie(perlin)\n", &lib(), None).unwrap_err();
        assert!(e.contains("line 1") && e.contains("noise"), "{e}");
        let e = compile("a = noise(scale=3, flavour=2)\n", &lib(), None).unwrap_err();
        assert!(e.contains("no 'flavour'") && e.contains("octaves"), "{e}");
        let e = compile("a = noise(stripes)\n", &lib(), None).unwrap_err();
        assert!(e.contains("Perlin"), "{e}");
        let e = compile("output shiny = noise()\n", &lib(), None).unwrap_err();
        assert!(e.contains("unknown output"), "{e}");
    }
}
