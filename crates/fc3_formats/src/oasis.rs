//! The game's localised strings ("oasis" tables).
//!
//! `languages\<lang>\oasisstrings_compressed.bin` holds every on-screen text:
//! mission names and objectives, menus, and the subtitles (section
//! `Subtitles`, keyed by the decimal sound id of the line). Scripts refer to
//! a string by its global numeric id; the engine also looks strings up by
//! section and symbolic name (`enum`), both hashed with CRC-32.
//!
//! Layout (little endian): u32 version, u32 hash, u32 section count; per
//! section: u32 section CRC, u32 n, n entries {u32 id, u32 section CRC, u32
//! enum CRC, u32 package CRC}, u32 block count, blocks {u32 last enum CRC,
//! u32 compressed size, u32 size, LZO1X data}. A block holds u32 m, m enum
//! CRCs (ascending), m offsets into the record area that follows, and
//! records {u32 id, UTF-16LE text, u16 0}.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::hash::crc32;
use crate::reader::Reader;

/// Every string of one language.
#[derive(Default)]
pub struct Strings {
    by_id: HashMap<u32, String>,
    /// (section CRC, enum CRC) -> id.
    by_name: HashMap<(u32, u32), u32>,
}

impl Strings {
    pub fn from_bytes(data: &[u8]) -> Result<Strings> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        if version != 1 {
            bail!("oasis table version {version}");
        }
        let _hash = r.u32()?;
        let sections = r.u32()?;
        let mut out = Strings::default();
        for _ in 0..sections {
            let section = r.u32()?;
            let n = r.u32()? as usize;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let id = r.u32()?;
                let _section = r.u32()?;
                let name = r.u32()?;
                let _package = r.u32()?;
                entries.push((id, name));
            }
            let mut texts: HashMap<u32, String> = HashMap::new();
            for _ in 0..r.u32()? {
                let _last = r.u32()?;
                let packed = r.u32()? as usize;
                let size = r.u32()? as usize;
                let block = crate::lzo::decompress(r.bytes(packed)?, size).context("oasis block")?;
                read_block(&block, &mut texts)?;
            }
            for (id, name) in entries {
                // Entries sharing a symbolic name share its one stored text.
                if let Some(text) = texts.get(&name) {
                    out.by_id.insert(id, text.clone());
                }
                out.by_name.insert((section, name), id);
            }
        }
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// A string by its global id.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    /// The ids of the strings that read `text` (in any case), lowest first.
    pub fn find(&self, text: &str) -> Vec<u32> {
        let mut ids: Vec<u32> = self.by_id.iter().filter(|(_, t)| t.eq_ignore_ascii_case(text)).map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids
    }

    /// The ids of the strings that contain `text` (in any case), lowest first.
    pub fn containing(&self, text: &str) -> Vec<u32> {
        let text = text.to_lowercase();
        let mut ids: Vec<u32> = self.by_id.iter().filter(|(_, t)| t.to_lowercase().contains(&text)).map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids
    }

    /// A string's id by section and symbolic name.
    pub fn id_of(&self, section: &str, name: &str) -> Option<u32> {
        self.by_name.get(&(crc32(section.as_bytes()), crc32(name.as_bytes()))).copied()
    }

    /// A string by section and symbolic name.
    pub fn named(&self, section: &str, name: &str) -> Option<&str> {
        self.id_of(section, name).and_then(|id| self.get(id))
    }

    /// The subtitle of a voiced line, by its sound id.
    pub fn subtitle(&self, sound_id: u32) -> Option<&str> {
        self.named("Subtitles", &sound_id.to_string())
    }
}

fn read_block(b: &[u8], texts: &mut HashMap<u32, String>) -> Result<()> {
    let mut r = Reader::new(b);
    let m = r.u32()? as usize;
    let mut names = Vec::with_capacity(m);
    for _ in 0..m {
        names.push(r.u32()?);
    }
    let mut offsets = Vec::with_capacity(m);
    for _ in 0..m {
        offsets.push(r.u32()? as usize);
    }
    let base = 4 + 8 * m;
    for (name, off) in names.into_iter().zip(offsets) {
        let mut r = Reader::at(b, base + off);
        let _id = r.u32()?;
        let mut units = Vec::new();
        loop {
            let u = r.u16()?;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        texts.insert(name, String::from_utf16_lossy(&units));
    }
    Ok(())
}

/// Spoken line lengths (`speechlength.bin`): sound id -> seconds. An FCB
/// whose `DurationMap` holds two counted arrays, `Keys` (u32 sound ids,
/// ascending) and `Values` (f32 seconds).
pub fn speech_lengths(data: &[u8]) -> Result<HashMap<u32, f32>> {
    let start = data.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&data[start..])?;
    let map = root.child("DurationMap").context("no DurationMap")?;
    let (keys, values) = (map.field("Keys").context("no Keys")?, map.field("Values").context("no Values")?);
    let words = |b: &[u8]| -> Vec<[u8; 4]> { b.chunks_exact(4).skip(1).map(|c| c.try_into().unwrap()).collect() };
    Ok(words(keys).into_iter().zip(words(values)).map(|(k, v)| (u32::from_le_bytes(k), f32::from_le_bytes(v))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(records: &[(u32, u32, &str)]) -> Vec<u8> {
        let mut recs = Vec::new();
        let mut offsets = Vec::new();
        for (_, id, text) in records {
            offsets.push(recs.len() as u32);
            recs.extend_from_slice(&id.to_le_bytes());
            for u in text.encode_utf16() {
                recs.extend_from_slice(&u.to_le_bytes());
            }
            recs.extend_from_slice(&[0, 0]);
        }
        let mut b = (records.len() as u32).to_le_bytes().to_vec();
        for (name, _, _) in records {
            b.extend_from_slice(&name.to_le_bytes());
        }
        for o in offsets {
            b.extend_from_slice(&o.to_le_bytes());
        }
        b.extend_from_slice(&recs);
        b
    }

    #[test]
    fn reads_blocks() {
        let (a, z) = (crc32(b"A"), crc32(b"1536010"));
        let mut names = vec![(a, 7, "Alpha"), (z, 9, "Line")];
        names.sort();
        let b = block(&names);
        let mut texts = HashMap::new();
        read_block(&b, &mut texts).unwrap();
        assert_eq!(texts[&a], "Alpha");
        assert_eq!(texts[&z], "Line");
    }
}
