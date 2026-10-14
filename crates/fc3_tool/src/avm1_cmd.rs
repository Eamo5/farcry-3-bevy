//! `swf-script <movie.swf> [names|strings]`: what a converted interface
//! movie's ActionScript 2 is made of, to see what it expects of the engine.
//! `names` (the default) lists each script block (a frame's actions, a
//! sprite's init actions, a clip's event handlers) with the functions it
//! defines, nested; `strings` lists every string its constant pools and
//! pushes name, with a count, most used first; `dis <block>` prints the
//! actions of the blocks whose label contains a text (pool constants
//! resolved).

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
