//! Far Cry 3 vegetation explorer: baked collection clusters (trees, plants,
//! grass, clustered rocks) stored in every world sector.
//!
//! ```text
//! fc3veg survey [level]                 parse every sector (or one level, e.g. c5) and report totals
//! fc3veg sector <id>                    zones, per-model counts and sanity checks for one sector
//! fc3veg dump <id> [out.csv]            instance list as CSV
//! fc3veg plot <id> <out.png> [radius] [px_per_m]
//!                                       top-down scatter over the terrain atlas (radius in sectors)
//! fc3veg find <model substring> [level] sectors with the most instances of a model
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context, Result};
use fc3_formats::{find_install, Vfs};
// The vegetation module refers to sibling modules through `crate::`; these
// imports make the same source compile inside this binary until it is
// registered in fc3_formats' lib.rs.
#[allow(unused_imports)]
use fc3_formats::{archive, fcb, hash, reader, terrain};

#[path = "../../../fc3_formats/src/vegetation.rs"]
#[allow(dead_code)]
mod vegetation;

use terrain::{parse_sdat, SectorCoord, TerrainSector, SECTOR_SIZE};
use vegetation::{ClusterLibrary, PlantInstance, PlantKind};

const SEA_LEVEL: f32 = 100.0;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = find_install().context("Far Cry 3 install not found; set FC3_DATA")?;
    let vfs = Vfs::open_install(&data, None)?;
    let lib = ClusterLibrary::load(&vfs)?;
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let arg = |i: usize| args.get(i).map(String::as_str);
    let sector_arg = |i: usize| -> Result<u32> {
        let s = arg(i).context("missing sector id")?;
        let id: u32 = s.parse().with_context(|| format!("bad sector id {s}"))?;
        if id >= 160 * 160 {
            bail!("sector id {id} out of range");
        }
        Ok(id)
    };
    match cmd {
        "survey" => survey(&vfs, &lib, arg(1)),
        "sector" => sector(&vfs, &lib, sector_arg(1)?),
        "dump" => dump(&vfs, &lib, sector_arg(1)?, arg(2)),
        "plot" => {
            let id = sector_arg(1)?;
            let out = arg(2).context("usage: plot <id> <out.png> [radius] [px_per_m]")?;
            let radius: i32 = arg(3).map(str::parse).transpose()?.unwrap_or(0);
            let ppm: u32 = arg(4).map(str::parse).transpose()?.unwrap_or(if radius == 0 { 8 } else { 4 });
            plot(&vfs, &lib, id, Path::new(out), radius, ppm)
        }
        "find" => find(&vfs, &lib, arg(1).context("usage: find <model substring> [level]")?, arg(2)),
        _ => {
            println!("usage: fc3veg survey [level] | sector <id> | dump <id> [out.csv] | plot <id> <out.png> [radius] [px_per_m] | find <model> [level]");
            Ok(())
        }
    }
}

fn short(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

fn sectors_of(level: Option<&str>) -> Vec<u32> {
    (0..160 * 160).filter(|&id| level.is_none_or(|l| SectorCoord::from_id(id).level() == l)).collect()
}

fn load_terrain(vfs: &Vfs, id: u32) -> Option<TerrainSector> {
    let c = SectorCoord::from_id(id);
    vfs.read(&c.sdat_path(vegetation::WORLD)).ok().and_then(|b| parse_sdat(&b).ok())
}

#[derive(Default)]
struct Stats {
    sectors: usize,
    with_veg: usize,
    errors: usize,
    zones: BTreeMap<usize, usize>,
    versions: BTreeMap<u32, usize>,
    owners: BTreeMap<String, usize>,
    kinds: BTreeMap<PlantKind, usize>,
    models: HashMap<String, usize>,
    unknown_blocks: usize,
    reserved_nonzero: usize,
    outside: usize,
    max_outside: f32,
    per_sector: Vec<usize>,
    trees_per_sector: Vec<usize>,
    scale: BTreeMap<PlantKind, (f32, f32)>,
}

fn survey(vfs: &Vfs, lib: &ClusterLibrary, level: Option<&str>) -> Result<()> {
    let mut s = Stats::default();
    for id in sectors_of(level) {
        let path = vegetation::sector_file(id);
        if !vfs.exists(&path) {
            continue;
        }
        s.sectors += 1;
        let zones = match vfs.read(&path).and_then(|b| vegetation::parse_sector_zones(&b, lib)) {
            Ok(z) => z,
            Err(e) => {
                s.errors += 1;
                if s.errors <= 10 {
                    eprintln!("sector {id}: {e:#}");
                }
                continue;
            }
        };
        *s.zones.entry(zones.len()).or_default() += 1;
        let c = SectorCoord::from_id(id);
        let [ox, oy] = c.origin();
        let (mut n, mut trees) = (0, 0);
        for z in &zones {
            *s.versions.entry(z.data_version).or_default() += 1;
            let prefix = z.owner.split('_').next().unwrap_or("").to_string();
            *s.owners.entry(prefix).or_default() += 1;
            for b in &z.blocks {
                s.unknown_blocks += usize::from(!b.known);
                s.reserved_nonzero += usize::from(b.reserved != 0);
            }
            for i in z.instances() {
                n += 1;
                trees += usize::from(i.kind == PlantKind::Tree);
                *s.kinds.entry(i.kind).or_default() += 1;
                *s.models.entry(short(&i.model).to_string()).or_default() += 1;
                let e = s.scale.entry(i.kind).or_insert((f32::MAX, f32::MIN));
                e.0 = e.0.min(i.scale);
                e.1 = e.1.max(i.scale);
                let dx = (ox - i.pos[0]).max(i.pos[0] - ox - SECTOR_SIZE).max(0.0);
                let dy = (oy - i.pos[1]).max(i.pos[1] - oy - SECTOR_SIZE).max(0.0);
                if dx > 0.0 || dy > 0.0 {
                    s.outside += 1;
                    s.max_outside = s.max_outside.max(dx.max(dy));
                }
            }
        }
        if n > 0 {
            s.with_veg += 1;
            s.per_sector.push(n);
            s.trees_per_sector.push(trees);
        }
    }
    let total: usize = s.kinds.values().sum();
    println!("world sector files: {}  with vegetation: {}  parse errors: {}", s.sectors, s.with_veg, s.errors);
    println!("zones per sector: {:?}  dataVersion: {:?}  owners: {:?}", s.zones, s.versions, s.owners);
    println!("instances: {total}  by kind: {:?}", s.kinds.iter().map(|(k, v)| (k.name(), *v)).collect::<Vec<_>>());
    println!("blocks with resource missing from library: {}  reserved!=0: {}", s.unknown_blocks, s.reserved_nonzero);
    println!("instances outside their sector: {} (max {:.1} m beyond the edge)", s.outside, s.max_outside);
    for (k, (lo, hi)) in &s.scale {
        println!("scale range {:5}: {lo:.3} .. {hi:.3}", k.name());
    }
    let pct = |v: &mut Vec<usize>, p: f32| {
        v.sort_unstable();
        v.get(((v.len() as f32 - 1.0) * p) as usize).copied().unwrap_or(0)
    };
    for (name, v) in [("all", &mut s.per_sector), ("trees", &mut s.trees_per_sector)] {
        let mean = v.iter().sum::<usize>() as f32 / v.len().max(1) as f32;
        println!(
            "per vegetated sector, {name:5}: mean {mean:.0}  median {}  p90 {}  max {}",
            pct(v, 0.5),
            pct(v, 0.9),
            pct(v, 1.0)
        );
    }
    let mut models: Vec<_> = s.models.into_iter().collect();
    models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("{} distinct models; top 40:", models.len());
    for (m, n) in models.iter().take(40) {
        println!("{n:9} {m}");
    }
    Ok(())
}

fn sector(vfs: &Vfs, lib: &ClusterLibrary, id: u32) -> Result<()> {
    let c = SectorCoord::from_id(id);
    let [ox, oy] = c.origin();
    println!("sector {id} ({}, {}) level {}  origin ({ox}, {oy})  file {}", c.x, c.y, c.level(), vegetation::sector_file(id));
    let zones = vegetation::sector_zones(vfs, lib, id)?;
    let terrain = load_terrain(vfs, id);
    let mut models: BTreeMap<(PlantKind, String), (usize, f32, f32)> = BTreeMap::new();
    let mut kinds: BTreeMap<PlantKind, usize> = BTreeMap::new();
    let (mut dz_sum, mut dz_max, mut dz_n, mut under_sea) = (0.0f32, 0.0f32, 0usize, 0usize);
    for z in &zones {
        println!(
            "zone {:#018x} of {}  dataVersion {} bbox {:?} .. {:?}  blocks {}",
            z.zone_id,
            z.owner,
            z.data_version,
            z.bbox_min,
            z.bbox_max,
            z.blocks.len()
        );
        for b in &z.blocks {
            let n: usize = b.groups.iter().map(|g| g.instances.len()).sum();
            let name = lib.get(b.resource).map(|r| r.path.as_str()).unwrap_or("?");
            println!(
                "  block {:016x} {:5} {} groups {:3} instances {:5}  {}",
                b.resource,
                b.kind.name(),
                if b.known { " " } else { "?" },
                b.groups.len(),
                n,
                short(name)
            );
        }
        for i in z.instances() {
            *kinds.entry(i.kind).or_default() += 1;
            let e = models.entry((i.kind, short(&i.model).to_string())).or_insert((0, f32::MAX, f32::MIN));
            e.0 += 1;
            e.1 = e.1.min(i.scale);
            e.2 = e.2.max(i.scale);
            let (lx, ly) = (i.pos[0] - ox, i.pos[1] - oy);
            if let Some(t) = &terrain {
                if (0.0..=SECTOR_SIZE).contains(&lx) && (0.0..=SECTOR_SIZE).contains(&ly) {
                    let d = (i.pos[2] - t.sample(lx, ly)).abs();
                    dz_sum += d;
                    dz_max = dz_max.max(d);
                    dz_n += 1;
                    if t.sample(lx, ly) < SEA_LEVEL - 0.5 && i.kind != PlantKind::Mesh {
                        under_sea += 1;
                    }
                }
            }
        }
    }
    println!("totals: {:?}", kinds.iter().map(|(k, v)| (k.name(), *v)).collect::<Vec<_>>());
    println!("per model (count, scale range):");
    for ((k, m), (n, lo, hi)) in &models {
        println!("  {:5} {n:6}  {lo:.2}..{hi:.2}  {m}", k.name());
    }
    if dz_n > 0 {
        println!(
            "height check: |z - terrain| mean {:.3} m, max {:.2} m over {dz_n} in-sector instances; plants on ground below sea level ({SEA_LEVEL} m): {under_sea}",
            dz_sum / dz_n as f32,
            dz_max
        );
    }
    Ok(())
}

fn dump(vfs: &Vfs, lib: &ClusterLibrary, id: u32, out: Option<&str>) -> Result<()> {
    let mut s = String::from("kind,model,x,y,z,yaw_deg,scale,rx_deg,ry_deg,rz_deg,nx,ny,nz,tint_r,tint_g,tint_b\n");
    for i in vegetation::sector_vegetation_with(vfs, lib, id)? {
        let n = i.normal.unwrap_or([0.0; 3]);
        let t = i.tint.unwrap_or([0; 3]);
        s += &format!(
            "{},{},{:.3},{:.3},{:.3},{:.2},{:.3},{:.2},{:.2},{:.2},{:.3},{:.3},{:.3},{},{},{}\n",
            i.kind.name(),
            i.model,
            i.pos[0],
            i.pos[1],
            i.pos[2],
            i.yaw.to_degrees(),
            i.scale,
            i.angles[0],
            i.angles[1],
            i.angles[2],
            n[0],
            n[1],
            n[2],
            t[0],
            t[1],
            t[2]
        );
    }
    match out {
        Some(p) => std::fs::write(p, s)?,
        None => print!("{s}"),
    }
    Ok(())
}

fn find(vfs: &Vfs, lib: &ClusterLibrary, needle: &str, level: Option<&str>) -> Result<()> {
    let needle = needle.to_ascii_lowercase();
    let mut hits = Vec::new();
    for id in sectors_of(level) {
        let Ok(zones) = vegetation::sector_zones(vfs, lib, id) else { continue };
        let (mut n, mut total) = (0, 0);
        for i in zones.iter().flat_map(|z| z.instances()) {
            total += 1;
            n += usize::from(i.model.to_ascii_lowercase().contains(&needle));
        }
        if n > 0 {
            hits.push((n, total, id));
        }
    }
    hits.sort_by(|a, b| b.cmp(a));
    println!("{} sectors contain '{needle}'", hits.len());
    for (n, total, id) in hits.iter().take(30) {
        let c = SectorCoord::from_id(*id);
        println!("{n:6} of {total:6}  sector {id} ({}, {}) level {}", c.x, c.y, c.level());
    }
    Ok(())
}

// ---------------------------------------------------------------- plotting

struct Image {
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

impl Image {
    fn new(w: u32, h: u32) -> Image {
        Image { w, h, rgb: vec![0; (w * h * 3) as usize] }
    }

    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h {
            let o = ((y as u32 * self.w + x as u32) * 3) as usize;
            self.rgb[o..o + 3].copy_from_slice(&c);
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3]) {
        let ri = r.ceil() as i32;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                if (dx * dx + dy * dy) as f32 <= r * r {
                    self.put(cx as i32 + dx, cy as i32 + dy, c);
                }
            }
        }
    }

    fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: [u8; 3]) {
        let ri = (r + width).ceil() as i32;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                if (d - r).abs() <= width * 0.5 {
                    self.put(cx as i32 + dx, cy as i32 + dy, c);
                }
            }
        }
    }

    fn square(&mut self, cx: f32, cy: f32, r: i32, c: [u8; 3]) {
        for d in -r..=r {
            self.put(cx as i32 + d, cy as i32 - r, c);
            self.put(cx as i32 + d, cy as i32 + r, c);
            self.put(cx as i32 - r, cy as i32 + d, c);
            self.put(cx as i32 + r, cy as i32 + d, c);
        }
    }

    fn write_png(&self, path: &Path) -> Result<()> {
        let mut raw = Vec::with_capacity(((self.w * 3 + 1) * self.h) as usize);
        for row in self.rgb.chunks_exact((self.w * 3) as usize) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        // zlib stream with stored (uncompressed) deflate blocks.
        let mut z = vec![0x78, 0x01];
        let mut chunks = raw.chunks(65535).peekable();
        while let Some(c) = chunks.next() {
            z.push(u8::from(chunks.peek().is_none()));
            z.extend_from_slice(&(c.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
            z.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &v in &raw {
            a = (a + v as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |tag: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = tag.to_vec();
            body.extend_from_slice(data);
            png.extend_from_slice(&body);
            png.extend_from_slice(&hash::crc32(&body).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&self.w.to_be_bytes());
        ihdr.extend_from_slice(&self.h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        std::fs::write(path, png)?;
        Ok(())
    }
}

/// Decodes the top mip of a BC1/BC2/BC3 DDS into RGB.
fn decode_dds(dds: &[u8]) -> Result<(u32, u32, Vec<[u8; 3]>)> {
    let info = fc3_formats::texture::dds_info(dds).context("bad DDS")?;
    let header = if &info.fourcc == b"DX10" { 148 } else { 128 };
    let (bytes_per_block, color_offset) = match &info.fourcc {
        b"DXT1" => (8, 0),
        b"DXT3" | b"DXT5" => (16, 8),
        other => bail!("unsupported atlas format {:?}", String::from_utf8_lossy(other)),
    };
    let (w, h) = (info.width, info.height);
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    let mut out = vec![[0u8; 3]; (w * h) as usize];
    let expand = |c: u16| -> [i32; 3] {
        let r = ((c >> 11) & 31) as i32;
        let g = ((c >> 5) & 63) as i32;
        let b = (c & 31) as i32;
        [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
    };
    for by in 0..bh {
        for bx in 0..bw {
            let o = header + ((by * bw + bx) as usize) * bytes_per_block + color_offset;
            if o + 8 > dds.len() {
                bail!("DDS truncated");
            }
            let c0 = u16::from_le_bytes([dds[o], dds[o + 1]]);
            let c1 = u16::from_le_bytes([dds[o + 2], dds[o + 3]]);
            let idx = u32::from_le_bytes(dds[o + 4..o + 8].try_into().unwrap());
            let (a, b) = (expand(c0), expand(c1));
            let mix = |p: i32, q: i32, d: i32| -> [u8; 3] { [0, 1, 2].map(|i| ((a[i] * p + b[i] * q) / d) as u8) };
            let palette = if c0 > c1 || bytes_per_block == 16 {
                [mix(1, 0, 1), mix(0, 1, 1), mix(2, 1, 3), mix(1, 2, 3)]
            } else {
                [mix(1, 0, 1), mix(0, 1, 1), mix(1, 1, 2), [0, 0, 0]]
            };
            for py in 0..4 {
                for px in 0..4 {
                    let (x, y) = (bx * 4 + px, by * 4 + py);
                    if x < w && y < h {
                        out[(y * w + x) as usize] = palette[((idx >> (2 * (py * 4 + px))) & 3) as usize];
                    }
                }
            }
        }
    }
    Ok((w, h, out))
}

struct Atlas {
    size: u32,
    rgb: Vec<[u8; 3]>,
}

impl Atlas {
    fn load(vfs: &Vfs, c: SectorCoord) -> Option<Atlas> {
        let bytes = vfs.read(&c.atlas_path(vegetation::WORLD, "diffuse")).ok()?;
        let x = fc3_formats::texture::parse(&bytes).ok()?;
        let (w, h, rgb) = decode_dds(x.dds).ok()?;
        (w == h).then_some(Atlas { size: w, rgb })
    }

    /// Bilinear sample at sector-local metres, staying inside the sector's quadrant.
    fn sample(&self, c: SectorCoord, lx: f32, ly: f32) -> [u8; 3] {
        let a = c.atlas_coord();
        let half = (self.size / 2) as f32;
        let (qx, qy) = ((c.x - a.x) as f32 * half, (c.y - a.y) as f32 * half);
        // Quadrant is transposed: rows follow +X, columns follow +Y (see vegetation::atlas_pixel).
        let fr = (lx / SECTOR_SIZE * half - 0.5).clamp(0.0, half - 1.0);
        let fc = (ly / SECTOR_SIZE * half - 0.5).clamp(0.0, half - 1.0);
        let (r0, c0) = (fr.floor(), fc.floor());
        let (tr, tc) = (fr - r0, fc - c0);
        let at = |r: f32, col: f32| {
            let r = (qy + r.min(half - 1.0)) as usize;
            let col = (qx + col.min(half - 1.0)) as usize;
            self.rgb[r * self.size as usize + col]
        };
        let (p00, p01, p10, p11) = (at(r0, c0), at(r0, c0 + 1.0), at(r0 + 1.0, c0), at(r0 + 1.0, c0 + 1.0));
        [0, 1, 2].map(|i| {
            let top = p00[i] as f32 * (1.0 - tc) + p01[i] as f32 * tc;
            let bot = p10[i] as f32 * (1.0 - tc) + p11[i] as f32 * tc;
            (top * (1.0 - tr) + bot * tr) as u8
        })
    }
}

fn plot(vfs: &Vfs, lib: &ClusterLibrary, id: u32, out: &Path, radius: i32, ppm: u32) -> Result<()> {
    let center = SectorCoord::from_id(id);
    let n = (2 * radius + 1) as u32;
    let side = n * 64 * ppm;
    let mut img = Image::new(side, side);
    let (x0, y0) = (center.x - radius, center.y - radius); // SW sector of the plot
    let mut atlases: HashMap<u32, Option<Atlas>> = HashMap::new();
    let mut instances = Vec::new();
    let mut entities: Vec<[f32; 3]> = Vec::new();
    for sy in 0..n as i32 {
        for sx in 0..n as i32 {
            let c = SectorCoord::new(x0 + sx, y0 + sy);
            if !c.valid() {
                continue;
            }
            let atlas = atlases.entry(c.atlas_coord().id()).or_insert_with(|| Atlas::load(vfs, c));
            let terrain = load_terrain(vfs, c.id());
            for py in 0..64 * ppm {
                for px in 0..64 * ppm {
                    let lx = (px as f32 + 0.5) / ppm as f32;
                    let ly = SECTOR_SIZE - (py as f32 + 0.5) / ppm as f32;
                    let mut col = atlas.as_ref().map(|a| a.sample(c, lx, ly)).unwrap_or([60, 60, 60]);
                    col = col.map(|v| (v as f32 * 0.8) as u8);
                    if terrain.as_ref().is_some_and(|t| t.sample(lx, ly) < SEA_LEVEL) {
                        col = [col[0] / 2 + 15, col[1] / 2 + 45, col[2] / 2 + 100];
                    }
                    let ix = sx as u32 * 64 * ppm + px;
                    let iy = (n - 1 - sy as u32) * 64 * ppm + py;
                    img.put(ix as i32, iy as i32, col);
                }
            }
            match vegetation::sector_vegetation_with(vfs, lib, c.id()) {
                Ok(v) => instances.extend(v),
                Err(e) => eprintln!("sector {}: {e:#}", c.id()),
            }
            // Ordinary placed entities with a mesh (buildings, props), for reference.
            if let Ok(ws) = vfs.read(&c.entities_path(vegetation::WORLD)).and_then(|b| fc3_formats::world::parse_sector(&b)) {
                entities.extend(ws.entities.iter().filter(|e| !e.meshes.is_empty()).map(|e| e.pos));
            }
        }
    }
    if n > 1 {
        for k in 1..n {
            let p = (k * 64 * ppm) as i32;
            for t in 0..side as i32 {
                img.put(p, t, [200, 200, 200]);
                img.put(t, p, [200, 200, 200]);
            }
        }
    }
    let [wx0, wy0] = SectorCoord::new(x0, y0).origin();
    let to_px = |p: &PlantInstance| ((p.pos[0] - wx0) * ppm as f32, side as f32 - (p.pos[1] - wy0) * ppm as f32);
    let s = ppm as f32;
    // Grass first so trees stay visible on top.
    for i in instances.iter().filter(|i| i.kind == PlantKind::Grass) {
        let (x, y) = to_px(i);
        img.disc(x, y, (0.12 * s).max(1.0), [255, 230, 40]);
    }
    for i in instances.iter().filter(|i| i.kind == PlantKind::Mesh) {
        let (x, y) = to_px(i);
        img.square(x, y, (0.3 * s).max(2.0) as i32, [40, 220, 255]);
    }
    for i in instances.iter().filter(|i| i.kind == PlantKind::Tree) {
        let (x, y) = to_px(i);
        img.ring(x, y, (1.5 * i.scale * s).max(3.0), (0.25 * s).max(1.5), [255, 40, 40]);
        img.disc(x, y, (0.2 * s).max(1.0), [255, 40, 40]);
    }
    for p in &entities {
        let (x, y) = ((p[0] - wx0) * s, side as f32 - (p[1] - wy0) * s);
        let r = (0.4 * s).max(2.0) as i32;
        for d in -r..=r {
            img.put(x as i32 + d, y as i32 + d, [255, 255, 255]);
            img.put(x as i32 + d, y as i32 - d, [255, 255, 255]);
        }
    }
    img.write_png(out)?;
    let mut kinds: BTreeMap<PlantKind, usize> = BTreeMap::new();
    for i in &instances {
        *kinds.entry(i.kind).or_default() += 1;
    }
    println!(
        "wrote {} ({side}x{side}, {n}x{n} sectors around {id}): {:?}, {} entities  [red rings = trees, yellow = grass, cyan = other meshes, white x = placed entities, blue tint = terrain below {SEA_LEVEL} m]",
        out.display(),
        kinds.iter().map(|(k, v)| (k.name(), *v)).collect::<Vec<_>>(),
        entities.len()
    );
    Ok(())
}
