//! Binary object files ("FCbn" / `.fcb`): a tree of objects, each with a
//! CRC-32 name hash and a set of hashed fields holding raw bytes.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};

use crate::hash::name_hash;
use crate::reader::Reader;

#[derive(Debug, Clone, Default)]
pub struct Object {
    pub name_hash: u32,
    /// Fields in file order.
    pub fields: Vec<(u32, Vec<u8>)>,
    pub children: Vec<Arc<Object>>,
}

fn read_count(r: &mut Reader) -> Result<(u32, bool)> {
    let v = r.u8()?;
    if v < 0xFE {
        return Ok((v as u32, false));
    }
    let is_offset = v != 0xFF;
    Ok((r.u32()?, is_offset))
}

fn read_object(r: &mut Reader, pointers: &mut Vec<Arc<Object>>, depth: usize) -> Result<Arc<Object>> {
    if depth > 256 {
        bail!("FCB nesting too deep");
    }
    let (child_count, is_offset) = read_count(r)?;
    if is_offset {
        return pointers
            .get(child_count as usize)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bad FCB object back-reference {child_count}"));
    }
    let slot = pointers.len();
    pointers.push(Arc::new(Object::default()));
    let name_hash = r.u32()?;
    let (value_count, off) = read_count(r)?;
    if off {
        bail!("FCB value count stored as offset");
    }
    let mut fields = Vec::with_capacity(value_count as usize);
    for _ in 0..value_count {
        let hash = r.u32()?;
        let pos = r.pos;
        let (size, is_off) = read_count(r)?;
        let value = if is_off {
            // Value shared with an earlier field: offset is relative to this field.
            let mut back = Reader::at(r.data, pos - size as usize);
            let (sz, again) = read_count(&mut back)?;
            if again {
                bail!("nested FCB value offset");
            }
            back.bytes(sz as usize)?.to_vec()
        } else {
            r.bytes(size as usize)?.to_vec()
        };
        fields.push((hash, value));
    }
    let mut children = Vec::with_capacity(child_count as usize);
    for _ in 0..child_count {
        children.push(read_object(r, pointers, depth + 1)?);
    }
    let obj = Arc::new(Object { name_hash, fields, children });
    pointers[slot] = obj.clone();
    Ok(obj)
}

/// Parses an FCB blob that starts with the `nbCF` header.
pub fn parse(data: &[u8]) -> Result<Arc<Object>> {
    let mut r = Reader::new(data);
    if r.u32()? != 0x4643_626E {
        bail!("not an FCB (bad magic)");
    }
    let version = r.u16()?;
    if version != 2 && version != 3 {
        bail!("unsupported FCB version {version}");
    }
    let _flags = r.u16()?;
    let _objects = r.u32()?;
    let _values = r.u32()?;
    let mut pointers = Vec::new();
    read_object(&mut r, &mut pointers, 0)
}

impl Object {
    pub fn is(&self, name: &str) -> bool {
        self.name_hash == name_hash(name)
    }

    pub fn field_h(&self, hash: u32) -> Option<&[u8]> {
        self.fields.iter().find(|(h, _)| *h == hash).map(|(_, v)| v.as_slice())
    }

    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.field_h(name_hash(name))
    }

    pub fn str_h(&self, hash: u32) -> Option<String> {
        self.field_h(hash).map(bytes_to_string)
    }

    pub fn str(&self, name: &str) -> Option<String> {
        self.field(name).map(bytes_to_string)
    }

    pub fn f32(&self, name: &str) -> Option<f32> {
        self.field(name).filter(|b| b.len() >= 4).map(|b| f32::from_le_bytes(b[..4].try_into().unwrap()))
    }

    pub fn u32(&self, name: &str) -> Option<u32> {
        self.field(name).filter(|b| b.len() >= 4).map(|b| u32::from_le_bytes(b[..4].try_into().unwrap()))
    }

    pub fn u64(&self, name: &str) -> Option<u64> {
        self.field(name).filter(|b| b.len() >= 8).map(|b| u64::from_le_bytes(b[..8].try_into().unwrap()))
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        self.field(name).and_then(|b| b.first()).map(|&b| b != 0)
    }

    pub fn vec3(&self, name: &str) -> Option<[f32; 3]> {
        let b = self.field(name)?;
        if b.len() < 12 {
            return None;
        }
        let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        Some([f(0), f(1), f(2)])
    }

    pub fn child(&self, name: &str) -> Option<&Arc<Object>> {
        let h = name_hash(name);
        self.children.iter().find(|c| c.name_hash == h)
    }

    pub fn children_named<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Arc<Object>> + 'a {
        let h = name_hash(name);
        self.children.iter().filter(move |c| c.name_hash == h)
    }

    /// Depth-first visit of every object.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Object, usize)) {
        fn go<'a>(o: &'a Object, d: usize, f: &mut dyn FnMut(&'a Object, usize)) {
            f(o, d);
            for c in &o.children {
                go(c, d + 1, f);
            }
        }
        go(self, 0, f);
    }
}

pub fn bytes_to_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Reverse lookup table from CRC-32 to known names, for debugging dumps.
#[derive(Default)]
pub struct NameDb {
    pub names: HashMap<u32, String>,
}

impl NameDb {
    pub fn add(&mut self, name: &str) {
        self.names.entry(name_hash(name)).or_insert_with(|| name.to_string());
    }

    /// Adds every identifier-like ASCII run (3 to 64 characters) found in a
    /// binary, such as the game's own executable, for naming fields locally.
    pub fn add_identifiers_from(&mut self, bytes: &[u8]) {
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = None;
        for (i, &b) in bytes.iter().chain(std::iter::once(&0)).enumerate() {
            match (ident(b), start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    if (3..=64).contains(&(i - s)) && !bytes[s].is_ascii_digit() {
                        if let Ok(name) = std::str::from_utf8(&bytes[s..i]) {
                            self.add(name);
                        }
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }

    pub fn get(&self, h: u32) -> String {
        self.names.get(&h).cloned().unwrap_or_else(|| format!("{h:08X}"))
    }

    /// Loads names from Gibbed-style `*.binaryclass.xml` definitions (attributes `name="..."`).
    pub fn load_xml_dir(&mut self, dir: &std::path::Path) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.load_xml_dir(&p);
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                for part in text.split("name=\"").skip(1) {
                    if let Some(end) = part.find('"') {
                        self.add(&part[..end]);
                    }
                }
            }
        }
    }
}

/// Renders an object tree for debugging.
pub fn dump(o: &Object, names: &NameDb, max_depth: usize, out: &mut String) {
    use std::fmt::Write;
    o.walk(&mut |obj, d| {
        if d > max_depth {
            return;
        }
        let _ = writeln!(out, "{:indent$}<{}> ({} children)", "", names.get(obj.name_hash), obj.children.len(), indent = d * 2);
        for (h, v) in &obj.fields {
            let _ = writeln!(out, "{:indent$}  {} = {}", "", names.get(*h), describe_value(v), indent = d * 2);
        }
    });
}

pub fn describe_value(v: &[u8]) -> String {
    let printable = v.len() > 1
        && v[..v.len() - 1].iter().all(|&c| (0x20..0x7f).contains(&c))
        && v[v.len() - 1] == 0;
    if printable {
        return format!("\"{}\"", bytes_to_string(v));
    }
    match v.len() {
        1 => format!("{} (u8)", v[0]),
        4 => {
            let u = u32::from_le_bytes(v.try_into().unwrap());
            let f = f32::from_le_bytes(v.try_into().unwrap());
            if f.is_finite() && f.abs() > 1e-6 && f.abs() < 1e7 {
                format!("{f} (f32) / {u:#x}")
            } else {
                format!("{u:#x} ({u})")
            }
        }
        8 => format!("{:#018x} (u64)", u64::from_le_bytes(v.try_into().unwrap())),
        12 | 16 => {
            let fs: Vec<String> = v
                .chunks_exact(4)
                .map(|c| format!("{:.4}", f32::from_le_bytes(c.try_into().unwrap())))
                .collect();
            format!("[{}]", fs.join(", "))
        }
        _ => {
            let hex: Vec<String> = v.iter().take(24).map(|b| format!("{b:02x}")).collect();
            format!("{} bytes: {}{}", v.len(), hex.join(" "), if v.len() > 24 { " ..." } else { "" })
        }
    }
}

/// An object tree in our own compact form (for the cache): the name hash,
/// the fields (hash, length, bytes) and the children, recursively.
pub fn encode(o: &Object) -> Vec<u8> {
    fn put(o: &Object, out: &mut Vec<u8>) {
        out.extend_from_slice(&o.name_hash.to_le_bytes());
        out.extend_from_slice(&(o.fields.len() as u32).to_le_bytes());
        for (h, v) in &o.fields {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            out.extend_from_slice(v);
        }
        out.extend_from_slice(&(o.children.len() as u32).to_le_bytes());
        for c in &o.children {
            put(c, out);
        }
    }
    let mut out = Vec::new();
    put(o, &mut out);
    out
}

/// Reads back what [`encode`] wrote.
pub fn decode(data: &[u8]) -> Result<Arc<Object>> {
    fn get(r: &mut Reader, depth: usize) -> Result<Arc<Object>> {
        if depth > 256 {
            bail!("encoded object nesting too deep");
        }
        let name_hash = r.u32()?;
        let n = r.u32()? as usize;
        let mut fields = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            let h = r.u32()?;
            let len = r.u32()? as usize;
            fields.push((h, r.bytes(len)?.to_vec()));
        }
        let k = r.u32()? as usize;
        let mut children = Vec::with_capacity(k.min(1024));
        for _ in 0..k {
            children.push(get(r, depth + 1)?);
        }
        Ok(Arc::new(Object { name_hash, fields, children }))
    }
    get(&mut Reader::new(data), 0)
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_roundtrip() {
        let leaf = Arc::new(Object { name_hash: 3, fields: vec![(9, vec![1, 2, 3])], children: vec![] });
        let root = Object { name_hash: 1, fields: vec![(5, b"abc".to_vec()), (6, vec![])], children: vec![leaf.clone(), leaf] };
        let back = decode(&encode(&root)).unwrap();
        assert_eq!(back.name_hash, 1);
        assert_eq!(back.fields, root.fields);
        assert_eq!(back.children.len(), 2);
        assert_eq!(back.children[1].fields, vec![(9, vec![1, 2, 3])]);
    }
}
