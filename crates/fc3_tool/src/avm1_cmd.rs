//! `swf-script <movie.swf> [names|strings]`: what a converted interface
//! movie's ActionScript 2 is made of, to see what it expects of the engine.
//! `names` (the default) lists each script block (a frame's actions, a
//! sprite's init actions, a clip's event handlers) with the functions it
//! defines, nested; `strings` lists every string its constant pools and
//! pushes name, with a count, most used first; `dis <block>` prints the
//! actions of the blocks whose label contains a text (pool constants
//! resolved); `expr [block]` reads them as statements, the stack's pushes
//! folded into expressions (`this.m_olist.createList(...)`), with labels at
//! jump targets and a function's preloaded registers by name (`this`,
//! `_global`, its parameters).

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use swf::avm1::read::Reader;
use swf::avm1::types::{Action, Value};

pub fn run(args: &[String]) -> Result<()> {
    let path = args.first().context("usage: swf-script <movie.swf> [names|strings]")?;
    let mode = args.get(1).map(String::as_str).unwrap_or("names");
    if mode == "fsc2" {
        // Each FSCommand2 (opcode 0x2D, Flash Lite's engine call) with the
        // values pushed just before it: the argument count, the command and
        // its literal arguments.
        let data = std::fs::read(path)?;
        let buf = swf::decompress_swf(&data[..])?;
        let movie = swf::parse_swf(&buf)?;
        let mut blocks: Vec<(String, &[u8])> = Vec::new();
        collect(&movie.tags, "", &mut blocks);
        for (_, code) in &blocks {
            fsc2(code, movie.header.version(), &mut Vec::new(), &mut Vec::new());
        }
        return Ok(());
    }
    if mode == "labels" {
        // Each sprite's frame labels, with the frame each marks (a text in
        // the labels to find the sprite by).
        let filter = args.get(2).map(String::as_str).unwrap_or("");
        let data = std::fs::read(path)?;
        let buf = swf::decompress_swf(&data[..])?;
        let movie = swf::parse_swf(&buf)?;
        for tag in &movie.tags {
            let swf::Tag::DefineSprite(sprite) = tag else { continue };
            let mut frame = 1;
            let mut labels = Vec::new();
            for t in &sprite.tags {
                match t {
                    swf::Tag::FrameLabel(l) => labels.push(format!("{}={frame}", l.label.to_str_lossy(swf::UTF_8))),
                    swf::Tag::ShowFrame => frame += 1,
                    _ => {}
                }
            }
            if !labels.is_empty() && labels.iter().any(|l| l.contains(filter)) {
                println!("sprite {} ({} frames): {}", sprite.id, frame - 1, labels.join(" "));
            }
        }
        return Ok(());
    }
    if mode == "expr" {
        let filter = args.get(2).map(String::as_str).unwrap_or("");
        let data = std::fs::read(path)?;
        let buf = swf::decompress_swf(&data[..])?;
        let movie = swf::parse_swf(&buf)?;
        let mut blocks: Vec<(String, &[u8])> = Vec::new();
        collect(&movie.tags, "", &mut blocks);
        for (label, code) in blocks.iter().filter(|(l, _)| l.contains(filter)) {
            println!("== {label}");
            let mut out = Vec::new();
            Expr::new(movie.header.version(), Vec::new(), BTreeMap::new(), 1).block(code, &mut out);
            for line in out {
                println!("{line}");
            }
        }
        return Ok(());
    }
    if mode == "dis" {
        let filter = args.get(2).map(String::as_str).unwrap_or("");
        let data = std::fs::read(path)?;
        let buf = swf::decompress_swf(&data[..])?;
        let movie = swf::parse_swf(&buf)?;
        let mut blocks: Vec<(String, &[u8])> = Vec::new();
        collect(&movie.tags, "", &mut blocks);
        for (label, code) in blocks.iter().filter(|(l, _)| l.contains(filter)) {
            println!("== {label}");
            dis(code, movie.header.version(), 1, &mut Vec::new());
        }
        return Ok(());
    }
    let data = std::fs::read(path)?;
    let buf = swf::decompress_swf(&data[..])?;
    let movie = swf::parse_swf(&buf)?;
    let version = movie.header.version();
    let mut blocks: Vec<(String, &[u8])> = Vec::new();
    collect(&movie.tags, "", &mut blocks);
    let mut strings: BTreeMap<String, usize> = BTreeMap::new();
    for (label, code) in &blocks {
        let mut out = Vec::new();
        walk(code, version, 1, &mut out, &mut strings);
        if mode == "names" {
            println!("{label} ({} bytes)", code.len());
            for line in out {
                println!("{line}");
            }
        }
    }
    match mode {
        "names" => {}
        "strings" => {
            let mut v: Vec<_> = strings.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (s, n) in v {
                println!("{n:5} {s}");
            }
        }
        _ => bail!("unknown mode {mode}"),
    }
    Ok(())
}

/// The script blocks of a tag list, sprites' included.
fn collect<'a>(tags: &'a [swf::Tag<'a>], at: &str, out: &mut Vec<(String, &'a [u8])>) {
    let mut frame = 1;
    for tag in tags {
        match tag {
            swf::Tag::ShowFrame => frame += 1,
            swf::Tag::DoAction(code) => out.push((format!("{at}frame {frame}"), code)),
            swf::Tag::DoInitAction { id, action_data } => out.push((format!("{at}init of sprite {id}"), action_data)),
            swf::Tag::DefineSprite(s) => collect(&s.tags, &format!("{at}sprite {} ", s.id), out),
            swf::Tag::PlaceObject(p) => {
                for (i, r) in p.clip_actions.iter().flat_map(|c| &c.records).enumerate() {
                    out.push((
                        format!("{at}frame {frame} clip {:?} handler {i} ({:?})", p.name.map(|n| n.to_string_lossy(swf::UTF_8)), r.events),
                        r.action_data,
                    ));
                }
            }
            _ => {}
        }
    }
}

fn walk(code: &[u8], version: u8, depth: usize, out: &mut Vec<String>, strings: &mut BTreeMap<String, usize>) {
    let mut reader = Reader::new(code, version);
    let text = |s: &swf::SwfStr| s.to_string_lossy(swf::UTF_8);
    while !reader.get_mut().is_empty() {
        let Ok(action) = reader.read_action() else { break };
        match action {
            Action::ConstantPool(pool) => {
                for s in &pool.strings {
                    *strings.entry(text(s)).or_default() += 1;
                }
            }
            Action::Push(p) => {
                for v in &p.values {
                    if let Value::Str(s) = v {
                        *strings.entry(text(s)).or_default() += 1;
                    }
                }
            }
            Action::DefineFunction(f) => {
                let params: Vec<String> = f.params.iter().map(|p| text(p)).collect();
                out.push(format!("{:indent$}function {}({})", "", text(f.name), params.join(", "), indent = depth * 2));
                walk(f.actions, version, depth + 1, out, strings);
            }
            Action::DefineFunction2(f) => {
                let params: Vec<String> = f.params.iter().map(|p| text(p.name)).collect();
                out.push(format!("{:indent$}function {}({})", "", text(f.name), params.join(", "), indent = depth * 2));
                walk(f.actions, version, depth + 1, out, strings);
            }
            Action::With(w) => walk(w.actions, version, depth, out, strings),
            Action::Try(t) => {
                walk(t.try_body, version, depth, out, strings);
                if let Some((_, c)) = t.catch_body {
                    walk(c, version, depth, out, strings);
                }
                if let Some(f) = t.finally_body {
                    walk(f, version, depth, out, strings);
                }
            }
            _ => {}
        }
    }
}

/// Prints a block's actions, one a line, with the constant pool's strings
/// put in for their indices; function bodies indented.
fn dis(code: &[u8], version: u8, depth: usize, pool: &mut Vec<String>) {
    let mut reader = Reader::new(code, version);
    let text = |s: &swf::SwfStr| s.to_string_lossy(swf::UTF_8);
    while !reader.get_mut().is_empty() {
        let at = code.len() - reader.get_mut().len();
        let Ok(action) = reader.read_action() else { break };
        let pad = depth * 2;
        match action {
            Action::ConstantPool(p) => *pool = p.strings.iter().map(|s| text(s)).collect(),
            Action::Push(p) => {
                let vals: Vec<String> = p
                    .values
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => format!("{:?}", text(s)),
                        Value::ConstantPool(i) => format!("{:?}", pool.get(*i as usize).cloned().unwrap_or_default()),
                        Value::Register(r) => format!("r{r}"),
                        other => format!("{other:?}"),
                    })
                    .collect();
                println!("{:pad$}{at:5} push {}", "", vals.join(", "));
            }
            Action::DefineFunction(f) => {
                let params: Vec<String> = f.params.iter().map(|p| text(p)).collect();
                println!("{:pad$}{at:5} function {}({})", "", text(f.name), params.join(", "));
                dis(f.actions, version, depth + 1, &mut pool.clone());
            }
            Action::DefineFunction2(f) => {
                let params: Vec<String> =
                    f.params.iter().map(|p| format!("{}{}", text(p.name), p.register_index.map_or(String::new(), |r| format!("=r{r}")))).collect();
                println!("{:pad$}{at:5} function {}({}) {:?}", "", text(f.name), params.join(", "), f.flags);
                dis(f.actions, version, depth + 1, &mut pool.clone());
            }
            other => println!("{:pad$}{at:5} {other:?}", ""),
        }
    }
}

/// Prints each FSCommand2 with the literal values pushed before it (most
/// recent last), as `cmd(args) [count]`.
fn fsc2(code: &[u8], version: u8, pool: &mut Vec<String>, recent: &mut Vec<String>) {
    let mut reader = Reader::new(code, version);
    let text = |s: &swf::SwfStr| s.to_string_lossy(swf::UTF_8);
    while !reader.get_mut().is_empty() {
        let Ok(action) = reader.read_action() else { break };
        match action {
            Action::ConstantPool(p) => *pool = p.strings.iter().map(|s| text(s)).collect(),
            Action::Push(p) => {
                for v in &p.values {
                    recent.push(match v {
                        Value::Str(s) => format!("{:?}", text(s)),
                        Value::ConstantPool(i) => format!("{:?}", pool.get(*i as usize).cloned().unwrap_or_default()),
                        Value::Int(i) => i.to_string(),
                        Value::Double(d) => d.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => "?".into(),
                    });
                }
            }
            Action::Unknown(u) if u.opcode == 0x2D => {
                let count: usize = recent.last().and_then(|c| c.parse().ok()).unwrap_or(0);
                let n = recent.len();
                if count >= 1 && n > count {
                    let cmd = &recent[n - 2];
                    let args: Vec<&str> = recent[n - 1 - count..n - 2].iter().rev().map(String::as_str).collect();
                    println!("{cmd}({})", args.join(", "));
                } else {
                    println!("? {:?}", &recent[n.saturating_sub(4)..]);
                }
                recent.clear();
            }
            Action::DefineFunction(f) => fsc2(f.actions, version, &mut pool.clone(), &mut Vec::new()),
            Action::DefineFunction2(f) => fsc2(f.actions, version, &mut pool.clone(), &mut Vec::new()),
            Action::GetVariable | Action::GetMember | Action::CallMethod | Action::CallFunction | Action::Add2 | Action::NewObject => recent.push("?".into()),
            _ => {}
        }
    }
}

/// A block read as statements: values pushed are kept as expression text
/// until an action uses them.
struct Expr {
    version: u8,
    pool: Vec<String>,
    regs: BTreeMap<u8, String>,
    depth: usize,
    stack: Vec<String>,
}

impl Expr {
    fn new(version: u8, pool: Vec<String>, regs: BTreeMap<u8, String>, depth: usize) -> Self {
        Self { version, pool, regs, depth, stack: Vec::new() }
    }

    fn pop(&mut self) -> String {
        self.stack.pop().unwrap_or_else(|| "<?>".into())
    }

    fn emit(&mut self, out: &mut Vec<String>, line: String) {
        // (Values left on the stack come first, as pushes.)
        for v in std::mem::take(&mut self.stack) {
            out.push(format!("{:pad$}push {v}", "", pad = self.depth * 2));
        }
        out.push(format!("{:pad$}{line}", "", pad = self.depth * 2));
    }

    /// `n` arguments popped (the count first).
    fn args(&mut self) -> Vec<String> {
        let n = self.pop();
        match n.parse::<f64>() {
            Ok(n) => (0..(n as usize).min(64)).map(|_| self.pop()).collect(),
            Err(_) => vec![format!("{n}...")],
        }
    }

    fn name(v: &str) -> String {
        v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).map_or_else(|| format!("[{v}]"), str::to_string)
    }

    fn member(o: &str, m: &str) -> String {
        match m.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            Some(n) if n.chars().all(|c| c.is_alphanumeric() || c == '_') && !n.is_empty() => format!("{o}.{n}"),
            _ => format!("{o}[{m}]"),
        }
    }

    fn block(&mut self, code: &[u8], out: &mut Vec<String>) {
        let text = |s: &swf::SwfStr| s.to_string_lossy(swf::UTF_8);
        // Jump targets, for their labels.
        let mut targets = std::collections::BTreeSet::new();
        let mut reader = Reader::new(code, self.version);
        while !reader.get_mut().is_empty() {
            let Ok(action) = reader.read_action() else { break };
            let next = (code.len() - reader.get_mut().len()) as i64;
            match action {
                Action::If(i) => {
                    targets.insert(next + i.offset as i64);
                }
                Action::Jump(j) => {
                    targets.insert(next + j.offset as i64);
                }
                _ => {}
            }
        }
        let mut reader = Reader::new(code, self.version);
        while !reader.get_mut().is_empty() {
            let at = (code.len() - reader.get_mut().len()) as i64;
            if targets.contains(&at) {
                for v in std::mem::take(&mut self.stack) {
                    out.push(format!("{:pad$}push {v}", "", pad = self.depth * 2));
                }
                out.push(format!("{:pad$}L{at}:", "", pad = self.depth.saturating_sub(1) * 2));
            }
            let Ok(action) = reader.read_action() else { break };
            let next = (code.len() - reader.get_mut().len()) as i64;
            let binary = |op: &str, me: &mut Self| {
                let (r, l) = (me.pop(), me.pop());
                me.stack.push(format!("({l} {op} {r})"));
            };
            match action {
                Action::ConstantPool(p) => self.pool = p.strings.iter().map(|s| text(s)).collect(),
                Action::Push(p) => {
                    for v in &p.values {
                        let v = match v {
                            Value::Undefined => "undefined".into(),
                            Value::Null => "null".into(),
                            Value::Bool(b) => b.to_string(),
                            Value::Int(i) => i.to_string(),
                            Value::Float(f) => f.to_string(),
                            Value::Double(d) => d.to_string(),
                            Value::Str(s) => format!("{:?}", text(s)),
                            Value::Register(r) => self.regs.get(r).cloned().unwrap_or_else(|| format!("r{r}")),
                            Value::ConstantPool(i) => format!("{:?}", self.pool.get(*i as usize).cloned().unwrap_or_default()),
                        };
                        self.stack.push(v);
                    }
                }
                Action::StoreRegister(r) => {
                    let name = self.regs.get(&r.register).cloned().unwrap_or_else(|| format!("r{}", r.register));
                    let v = self.stack.last().cloned().unwrap_or_else(|| "<?>".into());
                    if v != name {
                        out.push(format!("{:pad$}{name} = {v}", "", pad = self.depth * 2));
                    }
                    if let Some(top) = self.stack.last_mut() {
                        *top = name;
                    }
                }
                Action::Add | Action::Add2 => binary("+", self),
                Action::Subtract => binary("-", self),
                Action::Multiply => binary("*", self),
                Action::Divide => binary("/", self),
                Action::Modulo => binary("%", self),
                Action::Equals | Action::Equals2 => binary("==", self),
                Action::StrictEquals => binary("===", self),
                Action::Less | Action::Less2 => binary("<", self),
                Action::Greater => binary(">", self),
                Action::And => binary("&&", self),
                Action::Or => binary("||", self),
                Action::StringAdd => binary("add", self),
                Action::StringEquals => binary("eq", self),
                Action::StringLess => binary("lt", self),
                Action::StringGreater => binary("gt", self),
                Action::BitAnd => binary("&", self),
                Action::BitOr => binary("|", self),
                Action::BitXor => binary("^", self),
                Action::BitLShift => binary("<<", self),
                Action::BitRShift => binary(">>", self),
                Action::BitURShift => binary(">>>", self),
                Action::InstanceOf => binary("instanceof", self),
                Action::Not => {
                    let v = self.pop();
                    self.stack.push(format!("!{v}"));
                }
                Action::Increment => {
                    let v = self.pop();
                    self.stack.push(format!("({v} + 1)"));
                }
                Action::Decrement => {
                    let v = self.pop();
                    self.stack.push(format!("({v} - 1)"));
                }
                Action::ToNumber | Action::ToString | Action::ToInteger | Action::TypeOf | Action::StringLength | Action::TargetPath => {
                    let f = match action {
                        Action::ToNumber => "Number",
                        Action::ToString => "String",
                        Action::ToInteger => "int",
                        Action::TypeOf => "typeof",
                        Action::StringLength => "length",
                        _ => "targetPath",
                    };
                    let v = self.pop();
                    self.stack.push(format!("{f}({v})"));
                }
                Action::GetVariable => {
                    let v = self.pop();
                    self.stack.push(v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).map_or_else(|| format!("eval({v})"), str::to_string));
                }
                Action::SetVariable => {
                    let (v, n) = (self.pop(), self.pop());
                    self.emit(out, format!("{} = {v}", Self::name(&n)));
                }
                Action::DefineLocal => {
                    let (v, n) = (self.pop(), self.pop());
                    self.emit(out, format!("var {} = {v}", Self::name(&n)));
                }
                Action::DefineLocal2 => {
                    let n = self.pop();
                    self.emit(out, format!("var {}", Self::name(&n)));
                }
                Action::GetMember => {
                    let (m, o) = (self.pop(), self.pop());
                    self.stack.push(Self::member(&o, &m));
                }
                Action::SetMember => {
                    let (v, m, o) = (self.pop(), self.pop(), self.pop());
                    let target = Self::member(&o, &m);
                    self.emit(out, format!("{target} = {v}"));
                }
                Action::CallFunction => {
                    let f = self.pop();
                    let a = self.args();
                    self.stack.push(format!("{}({})", Self::name(&f), a.join(", ")));
                }
                Action::CallMethod => {
                    let (m, o) = (self.pop(), self.pop());
                    let a = self.args();
                    let callee = if m == "undefined" || m == "\"\"" { o } else { Self::member(&o, &m) };
                    self.stack.push(format!("{callee}({})", a.join(", ")));
                }
                Action::NewObject => {
                    let f = self.pop();
                    let a = self.args();
                    self.stack.push(format!("new {}({})", Self::name(&f), a.join(", ")));
                }
                Action::NewMethod => {
                    let (m, o) = (self.pop(), self.pop());
                    let a = self.args();
                    self.stack.push(format!("new {}({})", Self::member(&o, &m), a.join(", ")));
                }
                Action::InitArray => {
                    let a = self.args();
                    self.stack.push(format!("[{}]", a.join(", ")));
                }
                Action::InitObject => {
                    let n = self.pop().parse::<f64>().unwrap_or(0.0) as usize;
                    let mut kv = Vec::new();
                    for _ in 0..n.min(64) {
                        let (v, k) = (self.pop(), self.pop());
                        kv.push(format!("{k}: {v}"));
                    }
                    self.stack.push(format!("{{{}}}", kv.join(", ")));
                }
                // (A value dropped is shown when making it did something.)
                Action::Pop => {
                    let v = self.stack.pop().unwrap_or_default();
                    if v.contains('(') && !v.starts_with('"') {
                        self.emit(out, v);
                    }
                }
                Action::End => {}
                Action::PushDuplicate => {
                    let v = self.stack.last().cloned().unwrap_or_else(|| "<?>".into());
                    self.stack.push(v);
                }
                Action::StackSwap => {
                    let n = self.stack.len();
                    if n >= 2 {
                        self.stack.swap(n - 1, n - 2);
                    }
                }
                Action::Return => {
                    let v = self.pop();
                    self.emit(out, format!("return {v}"));
                }
                Action::If(i) => {
                    let c = self.pop();
                    self.emit(out, format!("if ({c}) goto L{}", next + i.offset as i64));
                }
                Action::Jump(j) => self.emit(out, format!("goto L{}", next + j.offset as i64)),
                Action::DefineFunction(f) => {
                    let params: Vec<String> = f.params.iter().map(|p| text(p)).collect();
                    self.function(text(f.name), params, BTreeMap::new(), f.actions, out);
                }
                Action::DefineFunction2(f) => {
                    // Preloaded registers, from 1, in this order.
                    let mut regs = BTreeMap::new();
                    let mut r = 1u8;
                    for (flag, name) in [
                        (swf::avm1::types::FunctionFlags::PRELOAD_THIS, "this"),
                        (swf::avm1::types::FunctionFlags::PRELOAD_ARGUMENTS, "arguments"),
                        (swf::avm1::types::FunctionFlags::PRELOAD_SUPER, "super"),
                        (swf::avm1::types::FunctionFlags::PRELOAD_ROOT, "_root"),
                        (swf::avm1::types::FunctionFlags::PRELOAD_PARENT, "_parent"),
                        (swf::avm1::types::FunctionFlags::PRELOAD_GLOBAL, "_global"),
                    ] {
                        if f.flags.contains(flag) {
                            regs.insert(r, name.to_string());
                            r += 1;
                        }
                    }
                    let mut params = Vec::new();
                    for p in &f.params {
                        let name = text(p.name);
                        if let Some(reg) = p.register_index {
                            regs.insert(reg.get(), name.clone());
                        }
                        params.push(name);
                    }
                    self.function(text(f.name), params, regs, f.actions, out);
                }
                Action::GotoLabel(l) => self.emit(out, format!("gotoLabel({:?})", text(l.label))),
                Action::GotoFrame(g) => self.emit(out, format!("gotoFrame({})", g.frame)),
                Action::Play => self.emit(out, "play()".into()),
                Action::Stop => self.emit(out, "stop()".into()),
                Action::Trace => {
                    let v = self.pop();
                    self.emit(out, format!("trace({v})"));
                }
                Action::Throw => {
                    let v = self.pop();
                    self.emit(out, format!("throw {v}"));
                }
                Action::Delete => {
                    let (m, o) = (self.pop(), self.pop());
                    self.emit(out, format!("delete {}", Self::member(&o, &m)));
                }
                Action::Delete2 => {
                    let n = self.pop();
                    self.emit(out, format!("delete {n}"));
                }
                Action::Enumerate | Action::Enumerate2 => {
                    let o = self.pop();
                    self.emit(out, format!("for (k in {o})"));
                }
                Action::Extends => {
                    let (sup, sub) = (self.pop(), self.pop());
                    self.emit(out, format!("{sub} extends {sup}"));
                }
                Action::ImplementsOp => {
                    let o = self.pop();
                    let a = self.args();
                    self.emit(out, format!("{o} implements {}", a.join(", ")));
                }
                Action::CastOp => {
                    let (o, c) = (self.pop(), self.pop());
                    self.stack.push(format!("cast({c}, {o})"));
                }
                Action::GetTime => self.stack.push("getTimer()".into()),
                other => self.emit(out, format!("{other:?}")),
            }
        }
        for v in std::mem::take(&mut self.stack) {
            out.push(format!("{:pad$}push {v}", "", pad = self.depth * 2));
        }
    }

    fn function(&mut self, name: String, params: Vec<String>, regs: BTreeMap<u8, String>, body: &[u8], out: &mut Vec<String>) {
        let mut inner = Expr::new(self.version, self.pool.clone(), regs, self.depth + 1);
        let mut lines = Vec::new();
        inner.block(body, &mut lines);
        let head = format!("function {name}({})", params.join(", "));
        if name.is_empty() {
            // (An anonymous function is a value: it goes on the stack.)
            let at = format!("<fn{}>", out.len());
            out.push(format!("{:pad$}{at} = {head} {{", "", pad = self.depth * 2));
            out.extend(lines);
            out.push(format!("{:pad$}}}", "", pad = self.depth * 2));
            self.stack.push(at);
        } else {
            self.emit(out, format!("{head} {{"));
            out.extend(lines);
            out.push(format!("{:pad$}}}", "", pad = self.depth * 2));
        }
    }
}
