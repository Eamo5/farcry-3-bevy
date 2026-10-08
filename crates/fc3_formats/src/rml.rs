//! Dunia binary XML (`.xml` files that start with a zero byte).
//!
//! Layout: a zero magic byte, then packed u32s (one byte, or `0xFF` followed by
//! a little-endian u32): unknown, string table size, node count, attribute
//! count. The node tree follows depth first; each node is `name, value,
//! attribute count, child count`, then its attributes (`flag byte, name,
//! value`), then its children. Names and values are byte offsets into a table
//! of NUL-terminated strings at the end of the file.

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub value: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Depth-first search for the first node called `name`.
    pub fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    strings: &'a [u8],
}

impl Parser<'_> {
    fn u8(&mut self) -> Result<u8> {
        let v = *self.data.get(self.pos).context("binary xml truncated")?;
        self.pos += 1;
        Ok(v)
    }

    fn packed(&mut self) -> Result<u32> {
        let v = self.u8()?;
        if v != 0xFF {
            return Ok(v as u32);
        }
        let b = self.data.get(self.pos..self.pos + 4).context("binary xml truncated")?;
        self.pos += 4;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String> {
        let off = self.packed()? as usize;
        let rest = self.strings.get(off..).context("string offset out of range")?;
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    fn node(&mut self, depth: usize) -> Result<Node> {
        if depth > 256 {
            bail!("binary xml nested too deeply");
        }
        let name = self.string()?;
        let value = self.string()?;
        let attr_count = self.packed()?;
        let child_count = self.packed()?;
        let mut attrs = Vec::with_capacity(attr_count.min(1024) as usize);
        for _ in 0..attr_count {
            let _flag = self.u8()?;
            attrs.push((self.string()?, self.string()?));
        }
        let mut children = Vec::with_capacity(child_count.min(1024) as usize);
        for _ in 0..child_count {
            children.push(self.node(depth + 1)?);
        }
        Ok(Node { name, value, attrs, children })
    }
}

pub fn is_binary_xml(data: &[u8]) -> bool {
    data.first() == Some(&0)
}

pub fn parse(data: &[u8]) -> Result<Node> {
    if !is_binary_xml(data) {
        bail!("not a binary xml file");
    }
    let mut p = Parser { data, pos: 1, strings: &[] };
    let _unknown = p.packed()?;
    let table = p.packed()? as usize;
    let _nodes = p.packed()?;
    let _attrs = p.packed()?;
    if table > data.len() {
        bail!("string table larger than file");
    }
    p.strings = &data[data.len() - table..];
    p.data = &data[..data.len() - table];
    p.node(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree() {
        // Strings: 0 "Root", 5 "", 6 "Child", 12 "a", 14 "1".
        let strings = b"Root\0\0Child\0a\01\0";
        let mut d = vec![0u8, 0, strings.len() as u8, 2, 1];
        d.extend_from_slice(&[0, 5, 0, 1]); // Root: no attributes, one child
        d.extend_from_slice(&[6, 5, 1, 0, 0, 12, 14]); // Child a="1"
        d.extend_from_slice(strings);
        let root = parse(&d).unwrap();
        assert_eq!(root.name, "Root");
        let c = root.find("Child").unwrap();
        assert_eq!(c.attr("a"), Some("1"));
    }
}
