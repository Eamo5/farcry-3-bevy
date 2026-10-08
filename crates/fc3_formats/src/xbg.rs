//! XBG geometry ("MESH") files.

use anyhow::{bail, Context, Result};

use crate::reader::Reader;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub kind: [u8; 4],
    pub unk04: u32,
    /// Absolute offset of the chunk payload within the file.
    pub data_offset: usize,
    pub data_len: usize,
    pub children: Vec<Chunk>,
}

impl Chunk {
    pub fn kind_str(&self) -> String {
        // Stored little endian, so reverse for readability.
        self.kind.iter().rev().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect()
    }

    pub fn find(&self, kind: &str) -> Option<&Chunk> {
        if self.kind_str() == kind {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(kind))
    }

    pub fn find_all<'a>(&'a self, kind: &str, out: &mut Vec<&'a Chunk>) {
        if self.kind_str() == kind {
            out.push(self);
        }
        for c in &self.children {
            c.find_all(kind, out);
        }
    }
}

pub fn parse_chunk(data: &[u8], offset: usize) -> Result<(Chunk, usize)> {
    let mut r = Reader::at(data, offset);
    let kind: [u8; 4] = r.bytes(4)?.try_into().unwrap();
    let unk04 = r.u32()?;
    let size = r.u32()? as usize;
    let data_size = r.u32()? as usize;
    let child_count = r.u32()?;
    if data_size > size || offset + size > data.len() {
        bail!("bad chunk sizes at {offset:#x}");
    }
    let child_start = offset + 20;
    let child_end = offset + size - data_size;
    let mut children = Vec::with_capacity(child_count as usize);
    let mut pos = child_start;
    for _ in 0..child_count {
        let (c, next) = parse_chunk(data, pos)?;
        children.push(c);
        pos = next;
    }
    if pos != child_end {
        bail!("child chunk size mismatch at {offset:#x}: {pos:#x} != {child_end:#x}");
    }
    Ok((Chunk { kind, unk04, data_offset: child_end, data_len: data_size, children }, offset + size))
}

pub struct XbgFile<'a> {
    pub data: &'a [u8],
    pub major: u16,
    pub minor: u16,
    pub root: Chunk,
}

impl<'a> XbgFile<'a> {
    pub fn parse(data: &'a [u8]) -> Result<XbgFile<'a>> {
        let mut r = Reader::new(data);
        if r.u32()? != 0x4D45_5348 {
            bail!("not an XBG (bad magic)");
        }
        let major = r.u16()?;
        let minor = r.u16()?;
        let _unk = r.u32()?;
        let (root, _) = parse_chunk(data, 12).context("parsing chunk tree")?;
        Ok(XbgFile { data, major, minor, root })
    }

    pub fn chunk_data(&self, c: &Chunk) -> &'a [u8] {
        &self.data[c.data_offset..c.data_offset + c.data_len]
    }
}

pub fn dump_tree(c: &Chunk, depth: usize, out: &mut String) {
    use std::fmt::Write;
    let _ = writeln!(
        out,
        "{:indent$}{} unk={} data@{:#x} len={} children={}",
        "",
        c.kind_str(),
        c.unk04,
        c.data_offset,
        c.data_len,
        c.children.len(),
        indent = depth * 2
    );
    for ch in &c.children {
        dump_tree(ch, depth + 1, out);
    }
}

#[derive(Debug, Clone)]
pub struct VertexBuffer {
    pub format: u32,
    pub stride: u32,
    pub count: u32,
    pub offset: u32,
}

#[derive(Debug, Clone)]
pub struct Primitive {
    pub buffer_index: i32,
    pub skeleton_index: i32,
    pub material_index: i32,
    pub index_start: i32,
    pub unk4: u32,
    pub unk5: u32,
    pub unk6: u32,
}

#[derive(Debug, Clone)]
pub struct Lod {
    pub distance: f32,
    pub buffers: Vec<VertexBuffer>,
    pub primitives: Vec<Primitive>,
    /// Range of the vertex blob in the file.
    pub vertex_data: std::ops::Range<usize>,
    pub indices: Vec<u16>,
}

/// Parses the LODS chunk (`data_offset` must be absolute; buffers are 16-byte aligned in file space).
pub fn parse_lods(file: &[u8], chunk: &Chunk) -> Result<Vec<Lod>> {
    let mut r = Reader::at(file, chunk.data_offset);
    let count = r.u32()?;
    let mut lods = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let distance = r.f32()?;
        let nb = r.u32()?;
        let mut buffers = Vec::with_capacity(nb as usize);
        for _ in 0..nb {
            buffers.push(VertexBuffer { format: r.u32()?, stride: r.u32()?, count: r.u32()?, offset: r.u32()? });
        }
        let np = r.u32()?;
        let mut primitives = Vec::with_capacity(np as usize);
        for _ in 0..np {
            primitives.push(Primitive {
                buffer_index: r.i32()?,
                skeleton_index: r.i32()?,
                material_index: r.i32()?,
                index_start: r.i32()?,
                unk4: r.u32()?,
                unk5: r.u32()?,
                unk6: r.u32()?,
            });
        }
        let vsize = r.u32()? as usize;
        r.align(16);
        let vstart = r.pos;
        r.skip(vsize)?;
        let icount = r.u32()? as usize;
        r.align(16);
        let ib = r.bytes(icount * 2)?;
        let indices = ib.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        lods.push(Lod { distance, buffers, primitives, vertex_data: vstart..vstart + vsize, indices });
    }
    if r.pos != chunk.data_offset + chunk.data_len {
        bail!("LODS size mismatch: ended at {:#x}, expected {:#x}", r.pos, chunk.data_offset + chunk.data_len);
    }
    Ok(lods)
}
