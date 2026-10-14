//! `playerglobal-abc <ruffle binary> <ruffle checkout> <out.abc>`: Ruffle's
//! AVM2 playerglobal ABC without compiling it (which takes Java and
//! `asc.jar`), for `vendor/build_playerglobal`.
//!
//! Every Ruffle build embeds its playerglobal as a SWF holding one ABC: the
//! compiler's output exactly, less the `[Ruffle(...)]` metadata, which the
//! build reads to make its native tables and then strips. The metadata is
//! put back from the AS3 sources it came from (each annotation sits just
//! before the class or member it marks), so the binary must be built from
//! the same revision as the checkout. Everything else in the ABC is left as
//! it is.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use swf::avm2::types::{AbcFile, Index, Metadata, MetadataItem, Multiname, Namespace, Trait, TraitKind};

pub fn run(args: &[String]) -> Result<()> {
    let [binary, checkout, out] = args else { bail!("usage: playerglobal-abc <ruffle binary> <ruffle checkout> <out.abc>") };
    let bytes = std::fs::read(binary).with_context(|| format!("reading {binary}"))?;
    let abc_bytes = embedded_abc(&bytes).context("no AVM2 playerglobal SWF in the binary")?;
    let mut abc = swf::avm2::read::Reader::new(&abc_bytes).read().context("reading the playerglobal ABC")?;
    ensure!(abc.metadata.is_empty(), "the ABC still has its metadata; use it as it is");
    let notes = annotations(&Path::new(checkout).join("core/src/avm2/globals"))?;
    let mut by_key: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for n in &notes {
        by_key.entry(n.key.as_str()).or_default().0 += 1;
    }
    let mut missing = Vec::new();
    for n in &notes {
        match attach(&mut abc, n) {
            Ok(()) => by_key.entry(n.key.as_str()).or_default().1 += 1,
            Err(e) => missing.push(format!("{}: {e:#}", n.describe())),
        }
    }
    for (key, (found, attached)) in &by_key {
        println!("{key:20} {attached}/{found}");
    }
    if !missing.is_empty() {
        for m in &missing {
            eprintln!("unmatched: {m}");
        }
        bail!("{} annotations found no place in the ABC", missing.len());
    }
    let mut out_bytes = Vec::new();
    swf::avm2::write::Writer::new(&mut out_bytes).write(abc)?;
    std::fs::write(out, &out_bytes)?;
    println!("{out}: {} bytes", out_bytes.len());
    Ok(())
}

/// The ABC of the AVM2 playerglobal SWF a binary embeds (the SWF version 19
/// one holding a `DoAbc2` tag).
fn embedded_abc(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut at = 0;
    while let Some(i) = find(&bytes[at..], b"WS").map(|i| at + i) {
        at = i + 1;
        let Some(start) = i.checked_sub(1) else { continue };
        if !matches!(bytes[start], b'F' | b'C' | b'Z') || bytes.get(i + 2) != Some(&19) {
            continue;
        }
        let len = u32::from_le_bytes(bytes.get(i + 3..i + 7)?.try_into().ok()?) as usize;
        let Some(data) = bytes.get(start..(start + len).min(bytes.len())) else { continue };
        let Ok(buf) = swf::decompress_swf(data) else { continue };
        let Ok(parsed) = swf::parse_swf(&buf) else { continue };
        for tag in &parsed.tags {
            if let swf::Tag::DoAbc2(abc) = tag {
                return Some(abc.data.to_vec());
            }
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What an annotation marks.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Class,
    Slot,
    Method,
    Getter,
    Setter,
}

/// One `[Ruffle(Key)]` and the declaration it marks: in a package, on a
/// class's member (static or not) or at the package's level.
#[derive(Debug, Clone)]
struct Note {
    key: String,
    file: String,
    package: String,
    class: Option<String>,
    name: String,
    target: Target,
    is_static: bool,
}

impl Note {
    fn describe(&self) -> String {
        format!(
            "{} [{}] {:?} {}{}{}",
            self.file,
            self.key,
            self.target,
            self.package,
            self.class.as_ref().map_or(String::new(), |c| format!(".{c}")),
            format_args!(".{}", self.name)
        )
    }
}

/// The annotations in every `.as` file under `dir`.
fn annotations(dir: &Path) -> Result<Vec<Note>> {
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f)?;
        let text = text.trim_start_matches('\u{feff}');
        out.extend(notes_in(&strip_comments(text), &f.strip_prefix(dir).unwrap_or(&f).display().to_string()));
    }
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "as") {
            out.push(p);
        }
    }
    Ok(())
}

/// The source without comments, and with string literals emptied (so their
/// braces and brackets don't count), lines kept.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' {
            out.push(c);
            i += 1;
            while i < b.len() && b[i] != c && b[i] != '\n' {
                if b[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push(c);
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Words of a declaration line, up to its first `(`, `:`, `=`, `;` or `{`.
fn decl_words(line: &str) -> Vec<&str> {
    let end = line.find(['(', ':', '=', ';', '{']).unwrap_or(line.len());
    line[..end].split_whitespace().collect()
}

fn notes_in(src: &str, file: &str) -> Vec<Note> {
    let mut out = Vec::new();
    let mut package = String::new();
    let mut depth = 0i32;
    // The class whose body is open, and the depth its body opened at.
    let mut class: Option<(String, i32)> = None;
    let mut pending: Vec<String> = Vec::new();
    let mut opening_class: Option<String> = None;
    for raw in src.lines() {
        let mut line = raw.trim();
        // Annotations at the line's start (any metadata: `[API(...)]` too).
        while line.starts_with('[') {
            let Some(end) = line.find(']') else { break };
            let inner = &line[1..end];
            if let Some(key) = inner.strip_prefix("Ruffle(").and_then(|r| r.strip_suffix(')')) {
                pending.push(key.trim().to_string());
            }
            line = line[end + 1..].trim();
        }
        let words = decl_words(line);
        if let Some(p) = words.iter().position(|w| *w == "package") {
            package = words.get(p + 1).map_or(String::new(), |s| s.to_string());
        }
        let in_class = class.as_ref().map(|(name, _)| name.clone());
        let is_static = words.contains(&"static");
        let decl = if let Some(p) = words.iter().position(|w| *w == "class" || *w == "interface") {
            words.get(p + 1).map(|n| {
                opening_class = Some(n.to_string());
                (Target::Class, n.to_string())
            })
        } else if let Some(p) = words.iter().position(|w| *w == "function") {
            match words.get(p + 1) {
                Some(&"get") if words.len() > p + 2 => Some((Target::Getter, words[p + 2].to_string())),
                Some(&"set") if words.len() > p + 2 => Some((Target::Setter, words[p + 2].to_string())),
                Some(n) => Some((Target::Method, n.to_string())),
                None => None,
            }
        } else if let Some(p) = words.iter().position(|w| *w == "var" || *w == "const") {
            words.get(p + 1).map(|n| (Target::Slot, n.to_string()))
        } else {
            None
        };
        if let Some((target, name)) = decl {
            for key in pending.drain(..) {
                let class = if target == Target::Class { None } else { in_class.clone() };
                out.push(Note { key, file: file.to_string(), package: package.clone(), class, name: name.clone(), target: target.clone(), is_static });
            }
        }
        for ch in line.chars() {
            match ch {
                '{' => {
                    if let Some(name) = opening_class.take() {
                        class = Some((name, depth));
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if class.as_ref().is_some_and(|(_, d)| *d == depth) {
                        class = None;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn string<'a>(abc: &'a AbcFile, i: Index<String>) -> &'a [u8] {
    if i.0 == 0 {
        &[]
    } else {
        abc.constant_pool.strings.get(i.0 as usize - 1).map_or(&[], Vec::as_slice)
    }
}

/// A multiname's namespace name and local name.
/// (Trait names come as qualified names or with a namespace set; a set's
/// package is its first package namespace.)
fn qname(abc: &AbcFile, m: Index<Multiname>) -> Option<(String, String)> {
    let package = |ns: Index<Namespace>| match abc.constant_pool.namespaces.get((ns.0 as usize).checked_sub(1)?)? {
        Namespace::Package(n) | Namespace::PackageInternal(n) => Some(String::from_utf8_lossy(string(abc, *n)).into_owned()),
        _ => None,
    };
    let (ns, name) = match abc.constant_pool.multinames.get((m.0 as usize).checked_sub(1)?)? {
        Multiname::QName { namespace, name } => (package(*namespace).unwrap_or_default(), name),
        Multiname::Multiname { namespace_set, name } => {
            let set = abc.constant_pool.namespace_sets.get((namespace_set.0 as usize).checked_sub(1)?)?;
            (set.iter().find_map(|ns| package(*ns)).unwrap_or_default(), name)
        }
        _ => return None,
    };
    Some((ns, String::from_utf8_lossy(string(abc, *name)).into_owned()))
}

/// The packages a name is in (a namespace set's: any of them), without the
/// API version mark `asc.jar` puts at a namespace's end (a private-use
/// character).
fn packages(abc: &AbcFile, m: Index<Multiname>) -> Vec<String> {
    let package = |ns: &Index<Namespace>| match abc.constant_pool.namespaces.get((ns.0 as usize).checked_sub(1)?)? {
        Namespace::Package(n) | Namespace::PackageInternal(n) => {
            Some(String::from_utf8_lossy(string(abc, *n)).trim_end_matches(|c: char| ('\u{e000}'..='\u{f8ff}').contains(&c)).to_string())
        }
        _ => None,
    };
    match (m.0 as usize).checked_sub(1).and_then(|i| abc.constant_pool.multinames.get(i)) {
        Some(Multiname::QName { namespace, .. }) => package(namespace).into_iter().collect(),
        Some(Multiname::Multiname { namespace_set, .. }) => (namespace_set.0 as usize)
            .checked_sub(1)
            .and_then(|i| abc.constant_pool.namespace_sets.get(i))
            .map_or(Vec::new(), |set| set.iter().filter_map(package).collect()),
        _ => Vec::new(),
    }
}

fn intern(abc: &mut AbcFile, s: &str) -> Index<String> {
    if let Some(i) = abc.constant_pool.strings.iter().position(|x| x.as_slice() == s.as_bytes()) {
        return Index::new(i as u32 + 1);
    }
    abc.constant_pool.strings.push(s.as_bytes().to_vec());
    Index::new(abc.constant_pool.strings.len() as u32)
}

fn fits(t: &Trait, target: &Target) -> bool {
    matches!(
        (&t.kind, target),
        (TraitKind::Slot { .. } | TraitKind::Const { .. }, Target::Slot)
            | (TraitKind::Method { .. } | TraitKind::Function { .. }, Target::Method)
            | (TraitKind::Getter { .. }, Target::Getter)
            | (TraitKind::Setter { .. }, Target::Setter)
            | (TraitKind::Class { .. }, Target::Class)
    )
}

/// Puts a note's metadata on its trait: a class's on the script trait that
/// defines it, a member's among the class's (static) or its instances'
/// traits, a package-level one's among the scripts'.
fn attach(abc: &mut AbcFile, n: &Note) -> Result<()> {
    let class_index = |abc: &AbcFile, name: &str| -> Option<usize> {
        abc.instances.iter().position(|inst| qname(abc, inst.name).is_some_and(|(ns, local)| local == name && ns == n.package))
    };
    // (script, trait) or (class, static, trait) positions that match.
    let mut places: Vec<(usize, usize, usize)> = Vec::new();
    match (&n.target, &n.class) {
        (Target::Class, _) => {
            let c = class_index(abc, &n.name).context("no such class")?;
            for (s, script) in abc.scripts.iter().enumerate() {
                for (t, tr) in script.traits.iter().enumerate() {
                    if matches!(tr.kind, TraitKind::Class { class, .. } if class.0 as usize == c) {
                        places.push((0, s, t));
                    }
                }
            }
        }
        (_, Some(class)) => {
            let c = class_index(abc, class).with_context(|| format!("no class {class}"))?;
            let traits = if n.is_static { &abc.classes[c].traits } else { &abc.instances[c].traits };
            for (t, tr) in traits.iter().enumerate() {
                if fits(tr, &n.target) && qname(abc, tr.name).is_some_and(|(_, local)| local == n.name) {
                    places.push((if n.is_static { 1 } else { 2 }, c, t));
                }
            }
        }
        (_, None) => {
            for (s, script) in abc.scripts.iter().enumerate() {
                for (t, tr) in script.traits.iter().enumerate() {
                    if fits(tr, &n.target) && qname(abc, tr.name).is_some_and(|(_, local)| local == n.name) && packages(abc, tr.name).contains(&n.package) {
                        places.push((0, s, t));
                    }
                }
            }
        }
    }
    if places.is_empty() && n.class.is_none() {
        let near: Vec<String> = abc
            .scripts
            .iter()
            .flat_map(|s| &s.traits)
            .filter(|t| qname(abc, t.name).is_some_and(|(_, local)| local == n.name))
            .map(|t| format!("{:?} in {:?}", t.kind, packages(abc, t.name)))
            .collect();
        bail!("no matching script trait; by name: {near:?}");
    }
    if let (true, Some(class)) = (places.is_empty(), &n.class) {
        // (What the class has, to see why.)
        let c = class_index(abc, class).unwrap_or(0);
        let traits = if n.is_static { &abc.classes[c].traits } else { &abc.instances[c].traits };
        let names: Vec<String> = traits
            .iter()
            .take(3)
            .map(|t| format!("{:?} {:?}", abc.constant_pool.multinames.get(t.name.0 as usize - 1), std::mem::discriminant(&t.kind)))
            .collect();
        bail!("no matching trait among {}: {names:?}", traits.len());
    }
    ensure!(places.len() == 1, "{} matching traits", places.len());
    let name = intern(abc, "Ruffle");
    let value = intern(abc, &n.key);
    abc.metadata.push(Metadata { name, items: vec![MetadataItem { key: Index::new(0), value }] });
    let m = Index::new(abc.metadata.len() as u32 - 1);
    let (kind, a, t) = places[0];
    let tr = match kind {
        0 => &mut abc.scripts[a].traits[t],
        1 => &mut abc.classes[a].traits[t],
        _ => &mut abc.instances[a].traits[t],
    };
    tr.metadata.push(m);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_mark_the_next_declaration_in_its_class() {
        let src = r#"package a.b {
    // `[Ruffle(CustomConstructor)]` in a comment is no annotation
    [Ruffle(InstanceAllocator)]
    [API("1")]
    [Ruffle(CallHandler)]
    public class Thing extends Base {
        [Ruffle(NativeAccessible)]
        private var _x:int = "{";
        prototype.f = function():void { };
        [Ruffle(FastCall)]
        public native function get x():int;
        [Ruffle(NativeCallable)]
        public static native function make():Thing;
    }
    [Ruffle(FastCall)]
    public native function helper():void;
}"#;
        let notes = notes_in(&strip_comments(src), "t.as");
        let got: Vec<_> = notes.iter().map(|n| (n.key.as_str(), n.class.as_deref(), n.name.as_str(), n.target.clone(), n.is_static)).collect();
        assert_eq!(
            got,
            vec![
                ("InstanceAllocator", None, "Thing", Target::Class, false),
                ("CallHandler", None, "Thing", Target::Class, false),
                ("NativeAccessible", Some("Thing"), "_x", Target::Slot, false),
                ("FastCall", Some("Thing"), "x", Target::Getter, false),
                ("NativeCallable", Some("Thing"), "make", Target::Method, true),
                ("FastCall", None, "helper", Target::Method, false),
            ]
        );
        assert!(notes.iter().all(|n| n.package == "a.b"));
    }
}
