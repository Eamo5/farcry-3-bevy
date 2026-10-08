//! Dunia 2 "big file" archives: a `.fat` index plus a `.dat` blob.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use memmap2::Mmap;

use crate::hash::path_hash;
use crate::lzo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Lzo1x,
    Zlib,
    Unknown3,
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub name_hash: u64,
    pub uncompressed_size: u32,
    pub compressed_size: u32,
    pub offset: u64,
    pub compression: Compression,
}

impl Entry {
    pub fn size(&self) -> u32 {
        if self.compression == Compression::None {
            self.compressed_size
        } else {
            self.uncompressed_size
        }
    }
}

pub struct Fat {
    pub version: u32,
    pub entries: Vec<Entry>,
    /// Entries listed in sub-FAT sections (streamed world sectors).
    pub subfat_entries: Vec<Vec<Entry>>,
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Fat {
    pub fn parse(data: &[u8]) -> Result<Fat> {
        if data.len() < 12 || rd32(data, 0) != 0x4641_5432 {
            bail!("bad FAT magic");
        }
        let version = rd32(data, 4);
        if version != 9 {
            bail!("unsupported FAT version {version}");
        }
        let flags = rd32(data, 8);
        if flags & 0xFF != 1 && flags & 0xFF != 0 {
            bail!("non-PC FAT platform {}", flags & 0xFF);
        }
        let _subfat_total = rd32(data, 12);
        let subfat_count = rd32(data, 16) as usize;
        let mut pos = 20;

        let read_entries = |pos: &mut usize| -> Result<Vec<Entry>> {
            let count = rd32(data, *pos) as usize;
            *pos += 4;
            if *pos + count * 20 > data.len() {
                bail!("FAT truncated");
            }
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                let a = rd32(data, *pos);
                let b = rd32(data, *pos + 4);
                let c = rd32(data, *pos + 8);
                let d = rd32(data, *pos + 12);
                let e = rd32(data, *pos + 16);
                *pos += 20;
                v.push(Entry {
                    name_hash: ((a as u64) << 32) | b as u64,
                    uncompressed_size: c >> 2,
                    compression: match c & 3 {
                        0 => Compression::None,
                        1 => Compression::Lzo1x,
                        2 => Compression::Zlib,
                        _ => Compression::Unknown3,
                    },
                    offset: ((d as u64) << 2) | (e >> 30) as u64,
                    compressed_size: e & 0x3FFF_FFFF,
                });
            }
            Ok(v)
        };

        let entries = read_entries(&mut pos)?;
        let unk1 = rd32(data, pos);
        pos += 4;
        if unk1 != 0 {
            bail!("unsupported FAT localization section");
        }
        let unk2 = rd32(data, pos) as usize;
        pos += 4 + unk2 * 16;
        let mut subfat_entries = Vec::with_capacity(subfat_count);
        for _ in 0..subfat_count {
            subfat_entries.push(read_entries(&mut pos)?);
        }
        Ok(Fat { version, entries, subfat_entries })
    }
}

/// One opened `.fat`/`.dat` pair.
pub struct Archive {
    pub name: String,
    pub path: PathBuf,
    pub fat: Fat,
    dat: Mmap,
}

impl Archive {
    pub fn open(fat_path: &Path) -> Result<Archive> {
        let mut fat_bytes = Vec::new();
        File::open(fat_path)
            .with_context(|| format!("opening {}", fat_path.display()))?
            .read_to_end(&mut fat_bytes)?;
        let fat = Fat::parse(&fat_bytes).with_context(|| format!("parsing {}", fat_path.display()))?;
        let dat_path = fat_path.with_extension("dat");
        let file = File::open(&dat_path).with_context(|| format!("opening {}", dat_path.display()))?;
        // SAFETY: the game data files are read-only while we run.
        let dat = unsafe { Mmap::map(&file)? };
        let name = fat_path.file_stem().unwrap().to_string_lossy().to_string();
        Ok(Archive { name, path: fat_path.to_path_buf(), fat, dat })
    }

    pub fn read(&self, e: &Entry) -> Result<Vec<u8>> {
        let start = e.offset as usize;
        let end = start + e.compressed_size as usize;
        if end > self.dat.len() {
            bail!("entry {:016X} out of bounds in {}", e.name_hash, self.name);
        }
        let raw = &self.dat[start..end];
        match e.compression {
            Compression::None => Ok(raw.to_vec()),
            Compression::Lzo1x => lzo::decompress(raw, e.uncompressed_size as usize)
                .with_context(|| format!("LZO entry {:016X}", e.name_hash)),
            Compression::Zlib => decompress_zlib_blocks(raw, e.uncompressed_size as usize),
            Compression::Unknown3 => bail!("unsupported compression scheme 3"),
        }
    }
}

fn decompress_zlib_blocks(raw: &[u8], uncompressed: usize) -> Result<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress};
    let mut pos = 0usize;
    let read_sizes = |pos: &mut usize| -> Result<[u16; 8]> {
        if *pos + 16 > raw.len() {
            bail!("zlib: header overrun");
        }
        let mut s = [0u16; 8];
        for (i, v) in s.iter_mut().enumerate() {
            *v = u16::from_le_bytes([raw[*pos + i * 2], raw[*pos + i * 2 + 1]]);
        }
        *pos += 16;
        Ok(s)
    };
    let mut sizes = read_sizes(&mut pos)?;
    let block_count = sizes[0] as usize;
    let max_block = 16 * (sizes[1] as usize + 1);
    let mut out = Vec::with_capacity(uncompressed);
    let mut left = uncompressed;
    let mut c = 2;
    for i in 0..block_count {
        if c == 8 {
            sizes = read_sizes(&mut pos)?;
            c = 0;
        }
        let csize = sizes[c] as usize;
        c += 1;
        let usize_ = if i + 1 < block_count { max_block.min(left) } else { left };
        let mut buf = vec![0u8; usize_];
        let mut d = Decompress::new(false);
        d.decompress(&raw[pos..pos + csize], &mut buf, FlushDecompress::Finish)?;
        out.extend_from_slice(&buf);
        left -= usize_;
        pos += csize;
        pos += (16 - csize % 16) % 16;
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub archive: u16,
    pub entry: Entry,
}

/// Virtual file system over every archive in a Far Cry 3 install.
/// Later-mounted archives override earlier ones (patch wins).
pub struct Vfs {
    pub root: PathBuf,
    pub archives: Vec<Archive>,
    pub index: HashMap<u64, Location>,
    pub names: HashMap<u64, String>,
}

impl Vfs {
    /// Mounts the standard single-player archive set from `data_win32`.
    pub fn open_install(data_win32: &Path, filelists: Option<&Path>) -> Result<Arc<Vfs>> {
        let order = [
            "common.fat",
            "shadersobj.fat",
            "sound.fat",
            "worlds/multicommon/multicommon.fat",
            "worlds/multicommon/multicommon_english.fat",
            "worlds/fc3_main/fc3_main.fat",
            "worlds/fc3_main/fc3_main_english.fat",
            "worlds/fc3_main/fc3_main_vistas.fat",
            "ige.fat",
            "patch.fat",
            "igepatch.fat",
        ];
        let mut vfs = Vfs {
            root: data_win32.to_path_buf(),
            archives: Vec::new(),
            index: HashMap::new(),
            names: HashMap::new(),
        };
        for rel in order {
            let p = data_win32.join(rel);
            if !p.exists() {
                continue;
            }
            vfs.mount(Archive::open(&p)?);
        }
        if let Some(dir) = filelists {
            vfs.load_filelists(dir)?;
        }
        Ok(Arc::new(vfs))
    }

    pub fn mount(&mut self, archive: Archive) {
        let id = self.archives.len() as u16;
        for e in archive.fat.entries.iter().chain(archive.fat.subfat_entries.iter().flatten()) {
            self.index.insert(e.name_hash, Location { archive: id, entry: *e });
        }
        self.archives.push(archive);
    }

    pub fn load_filelists(&mut self, dir: &Path) -> Result<()> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
            for e in std::fs::read_dir(dir)? {
                let p = e?.path();
                if p.is_dir() {
                    walk(&p, out)?;
                } else if p.extension().is_some_and(|x| x == "filelist") {
                    out.push(p);
                }
            }
            Ok(())
        }
        let mut lists = Vec::new();
        walk(dir, &mut lists)?;
        for l in lists {
            let text = std::fs::read_to_string(&l)?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with(';') {
                    continue;
                }
                self.add_name(line);
            }
        }
        Ok(())
    }

    pub fn add_name(&mut self, name: &str) -> bool {
        let h = path_hash(name);
        if self.index.contains_key(&h) {
            self.names.entry(h).or_insert_with(|| name.to_ascii_lowercase().replace('/', "\\"));
            true
        } else {
            false
        }
    }

    pub fn exists(&self, path: &str) -> bool {
        self.index.contains_key(&path_hash(path))
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.read_hash(path_hash(path)).with_context(|| format!("reading {path}"))
    }

    pub fn read_hash(&self, hash: u64) -> Result<Vec<u8>> {
        let loc = self.index.get(&hash).with_context(|| format!("file {hash:016X} not found"))?;
        self.archives[loc.archive as usize].read(&loc.entry)
    }

    pub fn name_of(&self, hash: u64) -> Option<&str> {
        self.names.get(&hash).map(|s| s.as_str())
    }
}

/// Locates the Far Cry 3 `data_win32` directory.
pub fn find_install() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FC3_DATA") {
        let p = PathBuf::from(p);
        if p.join("common.fat").exists() {
            return Some(p);
        }
        if p.join("data_win32/common.fat").exists() {
            return Some(p.join("data_win32"));
        }
    }
    let mut candidates: Vec<PathBuf> = [
        r"C:\Program Files (x86)\Steam\steamapps\common\Far Cry 3\data_win32",
        r"C:\Program Files\Steam\steamapps\common\Far Cry 3\data_win32",
        r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Far Cry 3\data_win32",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    // Steam on Linux (the game runs through Proton; its data files are the same).
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for steam in [".steam/steam", ".local/share/Steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"] {
            candidates.push(home.join(steam).join("steamapps/common/Far Cry 3/data_win32"));
        }
    }
    candidates.into_iter().find(|p| p.join("common.fat").exists())
}
