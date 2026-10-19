//! Command-line explorer for Far Cry 3 archives.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fc3_formats::{find_install, Vfs};

mod avm1_cmd;
mod facial_cmd;
mod playerglobal_cmd;

fn filelist_dir() -> PathBuf {
    let exe_rel = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists");
    if exe_rel.exists() {
        exe_rel
    } else {
        PathBuf::from("data/filelists")
    }
}

fn parse_target(vfs: &Vfs, s: &str) -> Result<u64> {
    if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(u64::from_str_radix(s, 16)?);
    }
    let h = fc3_formats::hash::path_hash(s);
    if !vfs.index.contains_key(&h) {
        bail!("{s} not found");
    }
    Ok(h)
}

/// Parses and validates WGSL the way wgpu does.
fn validate_wgsl(src: &str) -> Result<()> {
    let module = naga::front::wgsl::parse_str(src).map_err(|e| anyhow::anyhow!("{}", e.emit_to_string(src)))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| anyhow::anyhow!("{}", e.emit_to_string(src)))?;
    Ok(())
}

fn magic_of(data: &[u8]) -> String {
    let head: String = data.iter().take(4).map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect();
    head
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // (Needs no game install.)
    if args.first().is_some_and(|c| c == "playerglobal-abc") {
        return playerglobal_cmd::run(&args[1..]);
    }
    if args.first().is_some_and(|c| c == "swf-script") {
        return avm1_cmd::run(&args[1..]);
    }
    let data = find_install().context("Far Cry 3 install not found; set FC3_DATA")?;
    let vfs = Vfs::open_install(&data, Some(&filelist_dir()))?;
    let cmd = args.first().map(String::as_str).unwrap_or("stats");
    match cmd {
        "stats" => {
            for a in &vfs.archives {
                let n = a.fat.entries.len() + a.fat.subfat_entries.iter().map(Vec::len).sum::<usize>();
                println!("{:40} entries={n:7} subfats={}", a.name, a.fat.subfat_entries.len());
            }
            println!("total indexed: {} named: {}", vfs.index.len(), vfs.names.len());
        }
        "list" => {
            let filter = args.get(1).map(|s| s.to_ascii_lowercase());
            let mut rows: Vec<_> = vfs
                .index
                .iter()
                .filter_map(|(h, loc)| {
                    let name = vfs.name_of(*h).map(str::to_string).unwrap_or_else(|| format!("{h:016X}"));
                    if let Some(f) = &filter {
                        if !name.contains(f.as_str()) {
                            return None;
                        }
                    }
                    Some((name, loc.entry.size(), vfs.archives[loc.archive as usize].name.clone()))
                })
                .collect();
            rows.sort();
            for (n, s, a) in rows {
                println!("{s:10} {a:24} {n}");
            }
        }
        "extract" => {
            let target = args.get(1).context("usage: extract <path|hash> [out]")?;
            let h = parse_target(&vfs, target)?;
            let bytes = vfs.read_hash(h)?;
            let out = args.get(2).cloned().unwrap_or_else(|| {
                let n = vfs.name_of(h).map(str::to_string).unwrap_or(format!("{h:016X}"));
                n.rsplit('\\').next().unwrap().to_string()
            });
            std::fs::write(&out, &bytes)?;
            println!("wrote {} bytes to {out}", bytes.len());
        }
        "extract-all" => {
            let filter = args.get(1).context("usage: extract-all <substring> <outdir>")?.to_ascii_lowercase();
            let outdir = PathBuf::from(args.get(2).context("outdir")?);
            let mut n = 0;
            for (h, name) in &vfs.names {
                if name.contains(&filter) {
                    let p = outdir.join(name.replace('\\', "/"));
                    std::fs::create_dir_all(p.parent().unwrap())?;
                    match vfs.read_hash(*h) {
                        Ok(b) => {
                            std::fs::write(&p, b)?;
                            n += 1;
                        }
                        Err(e) => eprintln!("{name}: {e:#}"),
                    }
                }
            }
            println!("extracted {n} files");
        }
        "clip-events" => {
            // A clip's embedded event objects (nbCF): each with the time and
            // size before it, dumped as an FCB.
            let target = args.get(1).context("usage: clip-events <path|hash>")?;
            let d = vfs.read_hash(parse_target(&vfs, target)?)?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            // Field names from the installed game's own binary (local only).
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            let mut q = 0;
            while let Some(at) = d[q..].windows(4).position(|w| w == b"nbCF") {
                let o = q + at;
                q = o + 4;
                if o < 12 {
                    continue;
                }
                let time = f32::from_le_bytes(d[o - 12..o - 8].try_into()?);
                // (Some events have no size before them: those read on.)
                let size = u16::from_le_bytes([d[o - 2], d[o - 1]]) as usize;
                let end = if size < 16 { d.len() } else { (o + size).min(d.len()) };
                let obj = match fc3_formats::fcb::parse(&d[o..end]) {
                    Ok(obj) => obj,
                    Err(e) => {
                        println!("@{o:#x} t={time:.3} size {size}: {e:#}");
                        continue;
                    }
                };
                let mut s = String::new();
                fc3_formats::fcb::dump(&obj, &names, 16, &mut s);
                println!("@{o:#x} t={time:.3} size {size}\n{s}");
                q = o + size.max(4);
            }
        }
        "hex" => {
            let target = args.get(1).context("usage: hex <path|hash> [len] [offset]")?;
            let h = parse_target(&vfs, target)?;
            let bytes = vfs.read_hash(h)?;
            let len: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(256);
            let off: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(0);
            println!("size {}", bytes.len());
            for (i, chunk) in bytes[off.min(bytes.len())..(off + len).min(bytes.len())].chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                let asc: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
                println!("{:08x}: {:48} {asc}", off + i * 16, hex.join(" "));
            }
        }
        "verify" => {
            // Decompress every entry, report failures and magic histogram of unnamed files.
            let mut fails = 0;
            let mut magics: BTreeMap<String, usize> = BTreeMap::new();
            let filter = args.get(1).cloned();
            for (i, a) in vfs.archives.iter().enumerate() {
                if let Some(f) = &filter {
                    if !a.name.contains(f.as_str()) {
                        continue;
                    }
                }
                let entries: Vec<_> = a.fat.entries.iter().chain(a.fat.subfat_entries.iter().flatten()).collect();
                for e in entries {
                    match a.read(e) {
                        Ok(b) => {
                            if vfs.name_of(e.name_hash).is_none() {
                                *magics.entry(magic_of(&b)).or_default() += 1;
                            }
                        }
                        Err(err) => {
                            fails += 1;
                            if fails < 20 {
                                eprintln!("[{i}] {}: {err:#}", vfs.name_of(e.name_hash).unwrap_or("?"));
                            }
                        }
                    }
                }
                println!("{} done, fails so far {fails}", a.name);
            }
            let mut m: Vec<_> = magics.into_iter().collect();
            m.sort_by_key(|x| std::cmp::Reverse(x.1));
            println!("unnamed magics: {:?}", &m[..m.len().min(30)]);
        }
        "xbg" => {
            let target = args.get(1).context("usage: xbg <path>")?;
            let bytes = vfs.read_hash(parse_target(&vfs, target)?)?;
            let x = fc3_formats::xbg::XbgFile::parse(&bytes)?;
            let mut s = String::new();
            fc3_formats::xbg::dump_tree(&x.root, 0, &mut s);
            println!("version {}.{}\n{s}", x.major, x.minor);
        }
        "xbg-survey" => {
            let mut formats: BTreeMap<(u32, u32), usize> = BTreeMap::new();
            let mut examples: BTreeMap<u32, String> = BTreeMap::new();
            let mut errs = 0;
            let mut n = 0;
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            for (h, name) in &vfs.names {
                if !name.ends_with(".xbg") {
                    continue;
                }
                let bytes = vfs.read_hash(*h)?;
                n += 1;
                let x = match fc3_formats::xbg::XbgFile::parse(&bytes) {
                    Ok(x) => x,
                    Err(e) => {
                        errs += 1;
                        if errs < 10 {
                            eprintln!("{name}: {e:#}");
                        }
                        continue;
                    }
                };
                for c in &x.root.children {
                    *kinds.entry(c.kind_str()).or_default() += 1;
                }
                let Some(l) = x.root.find("LODS") else { continue };
                match fc3_formats::xbg::parse_lods(&bytes, l) {
                    Ok(lods) => {
                        for lod in lods {
                            for b in lod.buffers {
                                *formats.entry((b.format, b.stride)).or_default() += 1;
                                examples.entry(b.format).or_insert_with(|| name.clone());
                            }
                        }
                    }
                    Err(e) => {
                        errs += 1;
                        if errs < 10 {
                            eprintln!("{name}: LODS {e:#}");
                        }
                    }
                }
            }
            println!("{n} files, {errs} errors");
            println!("chunk kinds: {kinds:?}");
            for ((f, s), c) in formats {
                println!("format {f:#06x} ({f:016b}) stride {s:3} x{c}  e.g. {}", examples[&f]);
            }
        }
        "xbg-find" => {
            let want = u32::from_str_radix(args.get(1).context("format hex")?.trim_start_matches("0x"), 16)?;
            let mut hits = 0;
            for (h, name) in &vfs.names {
                if !name.ends_with(".xbg") {
                    continue;
                }
                let bytes = vfs.read_hash(*h)?;
                let x = fc3_formats::xbg::XbgFile::parse(&bytes)?;
                let lods = fc3_formats::xbg::parse_lods(&bytes, x.root.find("LODS").unwrap())?;
                if let Some(b) = lods[0].buffers.iter().find(|b| b.format == want) {
                    println!("{:6} {name}", b.count);
                    hits += 1;
                    if hits > 25 {
                        break;
                    }
                }
            }
        }
        "model" => {
            let target = args.get(1).context("usage: model <path>")?;
            let bytes = vfs.read_hash(parse_target(&vfs, target)?)?;
            let m = fc3_formats::mesh::decode(&bytes, 8)?;
            println!("bbox {:?} .. {:?}  bones {}", m.bbox_min, m.bbox_max, m.bones.len());
            for (i, b) in m.bones.iter().enumerate() {
                println!("bone {i} {} ({:08X}) parent {} at {:?}", b.name, b.name_hash, b.parent, b.translation);
            }
            for (i, (d, l)) in m.lods.iter().enumerate() {
                println!("lod {i} dist {d}: {} verts {} tris {} submeshes", l.positions.len(), l.indices.len() / 3, l.submeshes.len());
                // With `uvs`: each submesh's material, and the reach of its
                // positions, normals and UVs (the first LOD's).
                if i == 0 && args.get(2).is_some_and(|a| a == "uvs") {
                    for s in &l.submeshes {
                        let verts: Vec<usize> = l.indices[s.index_start..s.index_start + s.index_count].iter().map(|&v| v as usize).collect();
                        let reach = |of: &dyn Fn(usize) -> Vec<f32>| {
                            let n = of(verts[0]).len();
                            let lo: Vec<f32> = (0..n).map(|k| verts.iter().map(|&v| of(v)[k]).fold(f32::MAX, f32::min)).collect();
                            let hi: Vec<f32> = (0..n).map(|k| verts.iter().map(|&v| of(v)[k]).fold(f32::MIN, f32::max)).collect();
                            format!("{lo:.4?}..{hi:.4?}")
                        };
                        let name = m.materials.get(s.material).map_or("?", |r| r.name.as_str());
                        println!("  submesh {name}: {} tris", s.index_count / 3);
                        println!("    pos {}", reach(&|v| l.positions[v].to_vec()));
                        if !l.normals.is_empty() {
                            println!("    normal {}", reach(&|v| l.normals[v].to_vec()));
                        }
                        if !l.uvs.is_empty() {
                            println!("    uv {}", reach(&|v| l.uvs[v].to_vec()));
                        }
                        if !l.colors.is_empty() {
                            println!("    colour {}", reach(&|v| l.colors[v].to_vec()));
                        }
                    }
                }
            }
            for mr in &m.materials {
                println!("material {} -> {}", mr.name, mr.path);
                match vfs.read(&mr.path).and_then(|b| fc3_formats::material::parse(&b)) {
                    Ok(mat) => {
                        println!("   shader {} ({} params)", mat.shader, mat.params.len());
                        for (h, t) in &mat.textures {
                            println!("   tex {h:08X} {:?} {t} exists={}", fc3_formats::material::classify_texture(t), vfs.exists(t));
                        }
                    }
                    Err(e) => println!("   ERR {e:#}"),
                }
            }
        }
        "entity-prefixes" => {
            // Mesh-less world entities by name (trailing numbers dropped): entity-prefixes [max]
            let max: usize = args.get(1).and_then(|d| d.parse().ok()).unwrap_or(80);
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for (h, name) in &vfs.names {
                if !name.contains("worldsectors") || !name.ends_with(".data.fcb") {
                    continue;
                }
                let Ok(sec) = vfs.read_hash(*h).and_then(|b| fc3_formats::world::parse_sector(&b)) else { continue };
                for e in sec.entities.iter().filter(|e| e.meshes.is_empty() && e.effect.is_none() && e.sound_event.is_none() && e.light.is_none()) {
                    let base = e.name.rsplit('/').next().unwrap_or(&e.name);
                    let key = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_').to_string();
                    *counts.entry(key).or_default() += 1;
                }
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            for (k, n) in v.iter().take(max) {
                println!("{n:6} {k}");
            }
        }
        "fx-survey" => {
            // Placed particle effects by library name: fx-survey [depth]
            let depth: usize = args.get(1).and_then(|d| d.parse().ok()).unwrap_or(2);
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for (h, name) in &vfs.names {
                if !name.contains(r"worlds\fc3_main\") || !name.ends_with(".data.fcb") {
                    continue;
                }
                let Ok(bytes) = vfs.read_hash(*h) else { continue };
                let start = bytes.windows(4).position(|w| w == b"nbCF").unwrap_or(0);
                let Ok(root) = fc3_formats::fcb::parse(&bytes[start..]) else { continue };
                root.walk(&mut |o, _| {
                    if let Some(b) = o.field_h(0x1943_B130) {
                        let s = String::from_utf8_lossy(b).trim_end_matches('\0').to_ascii_lowercase();
                        let key: Vec<&str> = s.split('.').take(depth).collect();
                        *counts.entry(key.join(".")).or_default() += 1;
                    }
                });
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            for (k, n) in v.iter().take(60) {
                println!("{n:6} {k}");
            }
        }
        "animal-sounds" => {
            // Each animal archetype's vocal sound events (idle, hurt, death,
            // attack, flee, panic), from the entity library.
            let bytes = vfs.read(r"worlds\fc3_main\generated\entitylibrary_full.fcb")?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let roles = [
                ("idle", "sndIdleSound"),
                ("hurt", "sndHurtSound"),
                ("death", "sndDeathSound"),
                ("attack", "sndAttackSound"),
                ("flee", "sndFleeSound"),
                ("panic", "sndPanicSound"),
            ];
            root.walk(&mut |o, _| {
                if !o.is("Entity") {
                    return;
                }
                let Some(name) = o.str("hidName").filter(|n| n.contains("Animals")) else { return };
                let mut found: Vec<String> = Vec::new();
                o.walk(&mut |c, _| {
                    if !found.is_empty() {
                        return;
                    }
                    let vals: Vec<String> = roles
                        .iter()
                        .map(|(role, field)| {
                            let h = fc3_formats::hash::name_hash(field);
                            let v = c.field_h(h).map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_string()).unwrap_or_default();
                            format!("{role}={v}")
                        })
                        .collect();
                    if vals.iter().any(|v| v.contains("0x")) {
                        found = vals;
                    }
                });
                println!("{name}: {}", found.join(" "));
            });
        }
        "vehicle-sounds" => {
            // Each vehicle archetype's sound events by recovered field name
            // (first block found; the AI set follows as "ai.").
            let bytes = vfs.read(r"worlds\fc3_main\generated\entitylibrary_full.fcb")?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let fields = [
                "sndEngineLoop",
                "sndEngineIgnition",
                "sndEngineIgnitionStop",
                "sndTurnOffEngine",
                "sndRollingSound",
                "sndBrake",
                "sndHandbrake",
                "sndSuspensionSoundSmall",
                "sndSuspensionSoundMedium",
                "sndSuspensionSoundHeavy",
                "sndEngineBurning",
                "sndEngineSmoke",
                "sndEngineExplosion",
            ];
            let unnamed = [
                0x2142BD54u32,
                0xEB7BAA7B,
                0xA64C403E,
                0x5902AE1B,
                0xAE7B8515,
                0x86669DF1,
                0x1F6FCC4B,
                0x08FB0DF9,
                0x8841202C,
                0x4C79A8C1,
                0x241E05A8,
                0x40349650,
                0xCF4CDC78,
                0x2EB177C0,
            ];
            let event = |o: &fc3_formats::fcb::Object, h: u32| -> Option<String> {
                let v = String::from_utf8_lossy(o.field_h(h)?).trim_end_matches('\0').to_string();
                (v.starts_with("0x") && v != "0xFFFFFFFF").then_some(v)
            };
            root.walk(&mut |o, _| {
                if !o.is("Entity") {
                    return;
                }
                let Some(name) = o.str("hidName").filter(|n| n.starts_with("FC3_Vehicles:") && !n.contains("Attachments") && !n.contains('.')) else { return };
                let mut out: Vec<String> = Vec::new();
                o.walk(&mut |c, _| {
                    let tag = if out.iter().any(|s| s.starts_with("sndEngineLoop")) { "ai." } else { "" };
                    if c.field_h(fc3_formats::hash::name_hash("sndEngineLoop")).is_none() {
                        return;
                    }
                    for f in fields {
                        if let Some(v) = event(c, fc3_formats::hash::name_hash(f)) {
                            out.push(format!("{tag}{f}={v}"));
                        }
                    }
                    for h in unnamed {
                        if let Some(v) = event(c, h) {
                            out.push(format!("{tag}{h:08X}={v}"));
                        }
                    }
                });
                o.walk(&mut |c, _| {
                    if let Some(v) = event(c, fc3_formats::hash::name_hash("sndRollingSound")) {
                        out.push(format!("rolling={v}"));
                    }
                });
                println!("{name}: {}", out.join(" "));
            });
        }
        "treasure-maps" => {
            // The maps the shops sell, and what each shows where; with
            // `write`, the cache's table of them (the converter's own, a
            // few KB, for a cache converted before it had one).
            let items = vfs.read(fc3_formats::shop::ITEMS_LIB)?;
            let at = items.windows(4).position(|w| w == b"nbCF").context("no shopping FCB header")?;
            let maps = fc3_formats::shop::treasure_maps(&*fc3_formats::fcb::parse(&items[at..])?);
            for m in &maps {
                let cells: Vec<String> = m.reveals.iter().map(|(c, b)| format!("{c}:{b:#x}")).collect();
                println!("{:5} ${:<5} tower {:20} {} {} {}", m.id, m.cost, m.tower, m.unlock_mission, m.unlock_script, cells.join(" "));
            }
            if args.get(1).is_some_and(|a| a == "write") {
                let path = fc3_formats::cache::default_cache_root().join(fc3_formats::shop::TREASURE_MAPS_REL);
                std::fs::write(&path, fc3_formats::shop::treasure_maps_to_text(&maps))?;
                println!("wrote {} maps to {}", maps.len(), path.display());
            }
        }
        "sector-pickups" => {
            // The cached sectors' pickups (collectibles, plants, loot) by
            // name, counted: sector-pickups [name filter]
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "pickups/".into());
            let dir = fc3_formats::cache::default_cache_root().join("sectors");
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for entry in std::fs::read_dir(&dir)? {
                let Ok(bytes) = std::fs::read(entry?.path()) else { continue };
                let Ok(sector) = fc3_formats::cache::CachedSector::from_bytes(&bytes) else { continue };
                for e in sector.entities.iter().filter(|e| e.name.to_ascii_lowercase().contains(&filter)) {
                    *counts.entry(e.name.clone()).or_default() += 1;
                }
            }
            for (name, n) in counts {
                println!("{n:5} {name}");
            }
        }
        "gsf-dump" => {
            // The gameplay settings library's entries whose name (`hidName`)
            // contains a text, dumped: gsf-dump <text> [depth]
            let needle = args.get(1).context("usage: gsf-dump <text> [depth]")?.to_ascii_lowercase();
            let depth = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(16);
            let root = fc3_formats::fcb::parse(&vfs.read(fc3_formats::gsf::LIBRARY)?)?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            let mut found = Vec::new();
            root.walk(&mut |o, _| {
                if o.str("hidName").is_some_and(|n| n.to_ascii_lowercase().contains(&needle)) {
                    found.push(o.clone());
                }
            });
            for o in found {
                let mut out = String::new();
                fc3_formats::fcb::dump(&o, &names, depth, &mut out);
                println!("{out}");
            }
        }
        "library-dump" => {
            // Full FCB tree of the first entity-library prototype whose name contains a substring.
            let needle = args.get(1).context("usage: library-dump <substr | 0x library id>")?.to_ascii_lowercase();
            let bytes = vfs.read(r"worlds\fc3_main\generated\entitylibrary_full.fcb")?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            // (Or by the prototype's library id, in hex.)
            let id = u64::from_str_radix(needle.trim_start_matches("0x"), 16).ok().filter(|_| needle.starts_with("0x"));
            let mut found = None;
            root.walk(&mut |o, _| {
                if found.is_some() {
                    return;
                }
                if id.is_some() && o.is("EntityPrototype") && o.u64("disLibItemId") == id {
                    found = o.child("Entity").map(|e| (**e).clone());
                } else if o.is("Entity") && o.str("hidName").is_some_and(|s| s.to_ascii_lowercase().contains(&needle)) {
                    found = Some(o.clone());
                }
            });
            let e = found.context("not found")?;
            let mut out = String::new();
            fc3_formats::fcb::dump(&e, &names, 64, &mut out);
            println!("{out}");
        }
        "cache-rel" => {
            // Cache file for an original path: cache-rel <path>
            let p = args.get(1).context("usage: cache-rel <original path>")?;
            println!("model    {}", fc3_formats::cache::model_rel(p));
            println!("material {}", fc3_formats::cache::material_rel(p));
            println!("texture  {}", fc3_formats::cache::texture_rel(p));
            println!("anim     {}", fc3_formats::cache::anim_rel(p));
        }
        "weapon-stats" => {
            // The weapons' table as the converter writes it (from the cached
            // entity library and the shop's items), to a file: weapon-stats <out>
            let out = args.get(1).context("usage: weapon-stats <out>")?;
            let fcb = |bytes: Vec<u8>| -> Result<std::sync::Arc<fc3_formats::fcb::Object>> {
                let at = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
                fc3_formats::fcb::parse(&bytes[at..])
            };
            let library = fcb(std::fs::read(fc3_formats::cache::default_cache_root().join("story/entitylibrary.fcb"))?)?;
            let items = fcb(vfs.read(fc3_formats::shop::ITEMS_LIB)?)?;
            let stats = fc3_formats::weapons::weapon_stats(&library, Some(&items));
            std::fs::write(out, fc3_formats::weapons::to_text(&stats))?;
            println!("{} weapons, {} with a flame mesh", stats.len(), stats.iter().filter(|s| s.flame.as_ref().is_some_and(|f| f.rope.is_some())).count());
        }
        "handbook" => {
            // The handbook's table as the converter writes it, to a file: handbook <out>
            use fc3_formats::progression as p;
            let out = args.get(1).context("usage: handbook <out>")?;
            let read = |path| -> Result<std::sync::Arc<fc3_formats::fcb::Object>> {
                let bytes = vfs.read(path)?;
                let at = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
                fc3_formats::fcb::parse(&bytes[at..])
            };
            let strings = fc3_formats::oasis::Strings::from_bytes(&vfs.read(r"languages\english\oasisstrings_compressed.bin")?)?;
            let (items, categories, subcategories) = (read(p::WIKI_ITEMS)?, read(p::WIKI_CATEGORIES)?, read(p::WIKI_SUBCATEGORIES)?);
            let book = p::handbook(&items, &categories, &subcategories, &|id| strings.get(id).unwrap_or_default().to_string());
            std::fs::write(out, book.to_text())?;
            println!("{} entries, {} manual sections", book.entries.len(), book.manual.len());
        }
        "sdat-bits" => {
            // Sectors whose height samples set a given bit in a byte of the sample
            // (with `all`, every bit of the mask): sdat-bits <byte 0-3> <mask> [all]
            let byte: usize = args.get(1).and_then(|v| v.parse().ok()).context("usage: sdat-bits <byte> <mask> [all]")?;
            let mask = args.get(2).and_then(|v| u8::from_str_radix(v.trim_start_matches("0x"), 16).ok()).context("mask (hex)")?;
            let all = args.get(3).is_some_and(|a| a == "all");
            let mut hits = 0;
            for id in 0..(160 * 160) {
                let c = fc3_formats::terrain::SectorCoord::from_id(id);
                let Ok(d) = vfs.read(&c.sdat_path("fc3_main")) else { continue };
                let set = |b: &u8| if all { b & mask == mask } else { b & mask != 0 };
                let n = (0..65 * 65).filter(|i| d.get(0x258 + 4 * i + byte).is_some_and(set)).count();
                if n > 0 {
                    hits += 1;
                    let o = c.origin();
                    println!("sector {id} at ({}, {}): {n}", o[0], o[1]);
                }
            }
            println!("{hits} sectors");
        }
        "fcm" => {
            // A converted model's bounds, materials and LOD-0 parts: fcm <file.fcm>
            let p = args.get(1).context("usage: fcm <file.fcm>")?;
            let m = fc3_formats::cache::CachedModel::from_bytes(&std::fs::read(p)?)?;
            println!("aabb {:?} .. {:?}, {} lods, {} bones", m.aabb_min, m.aabb_max, m.lods.len(), m.bones.len());
            for (i, mat) in m.materials.iter().enumerate() {
                println!("  material {i} {mat} flags {:#x}", m.material_flags.get(i).copied().unwrap_or(0));
            }
            for part in m.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
                println!("  part material {} verts {}", part.material, part.positions.len());
                for (name, uvs) in [("uv0", &part.uvs), ("uv1", &part.uvs1)] {
                    if !uvs.is_empty() {
                        let min = uvs.iter().fold([f32::INFINITY; 2], |a, b| [a[0].min(b[0]), a[1].min(b[1])]);
                        let max = uvs.iter().fold([f32::NEG_INFINITY; 2], |a, b| [a[0].max(b[0]), a[1].max(b[1])]);
                        println!("    {name}: {} values, {min:?} .. {max:?}", uvs.len());
                    }
                }
                if part.positions.len() <= 16 {
                    for (i, p) in part.positions.iter().enumerate() {
                        println!("    {p:.2?} uv {:.3?} n {:.2?}", part.uvs.get(i), part.normals.get(i));
                    }
                    println!("    indices {:?}", part.indices);
                }
            }
        }
        "fcm-winding" => {
            // Signed volume per LOD-0 part of a converted model (> 0: CCW faces point out).
            let p = args.get(1).context("usage: fcm-winding <file.fcm>")?;
            let m = fc3_formats::cache::CachedModel::from_bytes(&std::fs::read(p)?)?;
            for (i, part) in m.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]).iter().enumerate() {
                let mut vol = 0.0f64;
                for t in part.indices.chunks_exact(3) {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| part.positions[i as usize].map(f64::from));
                    vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6.0;
                }
                // Agreement of each triangle's right-hand normal with its vertex normals.
                let (mut agree, mut total) = (0usize, 0usize);
                if part.normals.len() == part.positions.len() {
                    for t in part.indices.chunks_exact(3) {
                        let [a, b, c] = [t[0], t[1], t[2]].map(|i| part.positions[i as usize]);
                        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                        let vn: [f32; 3] = (0..3).map(|k| t.iter().map(|&i| part.normals[i as usize][k]).sum()).collect::<Vec<f32>>().try_into().unwrap();
                        total += 1;
                        if n[0] * vn[0] + n[1] * vn[1] + n[2] * vn[2] > 0.0 {
                            agree += 1;
                        }
                    }
                }
                // Normals pointing away from the part's centre (outward on convex shapes).
                let nv = part.positions.len().max(1) as f32;
                let centre: Vec<f32> = (0..3).map(|k| part.positions.iter().map(|p| p[k]).sum::<f32>() / nv).collect();
                let outward = part.positions.iter().zip(&part.normals).filter(|(p, n)| (0..3).map(|k| (p[k] - centre[k]) * n[k]).sum::<f32>() > 0.0).count();
                println!(
                    "part {i}: material {} {} tris, signed volume {vol:+.5}, CCW matches normals {agree}/{total}, normals outward {outward}/{}{}",
                    part.material,
                    part.indices.len() / 3,
                    part.normals.len(),
                    part.pivot.as_ref().map(|p| format!(", turns about {} at {:?}", p.name, p.at)).unwrap_or_default()
                );
            }
        }
        "fcmat" => {
            // A converted material: fcmat <cache file>
            let p = args.get(1).context("usage: fcmat <file.fcmat>")?;
            let m = fc3_formats::cache::CachedMaterial::from_bytes(&std::fs::read(p)?)?;
            println!("shader {} diffuse {} base {:?} flags {:#x}", m.shader, m.diffuse, m.base_color, m.flags);
            match &m.original {
                Some(o) => {
                    println!("original vs {:016x} ps {:016x} normal {}", o.vs, o.ps, o.use_normal);
                    for (r, v) in &o.ps_constants {
                        println!("  ps c{r} {v:?}");
                    }
                    for (s, t, srgb) in &o.samplers {
                        println!("  s{s} {t} srgb={srgb}");
                    }
                }
                None => println!("no original binding"),
            }
        }
        "unpaired" => {
            // Cached materials not drawn with an original pair (none, or
            // blended), by shader and kind: unpaired <cache materials dir> [list]
            let dir = args.get(1).context("usage: unpaired <materials dir>")?;
            let mut by: std::collections::BTreeMap<String, Vec<String>> = Default::default();
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let Ok(m) = fc3_formats::cache::CachedMaterial::from_bytes(&std::fs::read(e.path())?) else { continue };
                use fc3_formats::cache::mat_flags as f;
                // Blended surfaces stay on the standard material at run time
                // even with a pair.
                if m.original.is_some() && m.flags & f::ALPHA_BLEND == 0 {
                    continue;
                }
                let kind = if m.original.is_some() {
                    "paired-blended"
                } else if m.flags & f::NO_DRAW != 0 {
                    "no-draw"
                } else if m.flags & f::WATER != 0 {
                    "water"
                } else if m.flags & f::ALPHA_BLEND != 0 {
                    "blended"
                } else if m.flags & f::VEGETATION != 0 {
                    "vegetation"
                } else if m.diffuse.is_empty() {
                    "untextured"
                } else {
                    "opaque"
                };
                by.entry(format!("{kind:10} {}", m.shader)).or_default().push(e.file_name().to_string_lossy().into_owned());
            }
            let list = args.get(2).is_some_and(|a| a == "list");
            for (k, v) in &by {
                if list {
                    for f in v {
                        println!("{k} {f}");
                    }
                } else {
                    println!("{:5} {k}  e.g. {}", v.len(), v.first().map(String::as_str).unwrap_or(""));
                }
            }
        }
        "fcmat-diff" => {
            // How two converted material folders differ: fcmat-diff <dir a> <dir b>
            let (a, b) = (args.get(1).context("usage: fcmat-diff <a> <b>")?, args.get(2).context("usage: fcmat-diff <a> <b>")?);
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            let (mut tex_a, mut tex_b) = (std::collections::HashSet::new(), std::collections::HashSet::new());
            for e in std::fs::read_dir(b)? {
                let e = e?;
                let Ok(mb) = fc3_formats::cache::CachedMaterial::from_bytes(&std::fs::read(e.path())?) else { continue };
                let Ok(ma) = std::fs::read(std::path::Path::new(a).join(e.file_name()))
                    .map_err(anyhow::Error::from)
                    .and_then(|x| fc3_formats::cache::CachedMaterial::from_bytes(&x))
                else {
                    *counts.entry("only in b".into()).or_default() += 1;
                    continue;
                };
                for bit in 0..8 {
                    let (x, y) = (ma.flags >> bit & 1, mb.flags >> bit & 1);
                    if x != y {
                        *counts.entry(format!("flag bit {bit} {x}->{y}")).or_default() += 1;
                    }
                }
                for (n, x, y) in [
                    ("diffuse", &ma.diffuse, &mb.diffuse),
                    ("normal", &ma.normal, &mb.normal),
                    ("specular", &ma.specular, &mb.specular),
                    ("shader", &ma.shader, &mb.shader),
                ] {
                    if x != y {
                        *counts.entry(format!("{n} changed")).or_default() += 1;
                    }
                }
                if ma.base_color != mb.base_color {
                    *counts.entry("base colour changed".into()).or_default() += 1;
                }
                if ma.textures.len() != mb.textures.len() {
                    *counts.entry("texture count changed".into()).or_default() += 1;
                }
                match (&ma.original, &mb.original) {
                    (None, Some(_)) => *counts.entry("original gained".into()).or_default() += 1,
                    (Some(_), None) => *counts.entry("original lost".into()).or_default() += 1,
                    (Some(x), Some(y)) if x.ps != y.ps || x.vs != y.vs => *counts.entry("original pair changed".into()).or_default() += 1,
                    _ => {}
                }
                for (m, set) in [(&ma, &mut tex_a), (&mb, &mut tex_b)] {
                    set.extend(m.original.iter().flat_map(|o| o.samplers.iter().map(|s| s.1.clone())));
                    set.extend([m.diffuse.clone(), m.normal.clone(), m.specular.clone()]);
                }
            }
            for (k, v) in counts {
                println!("{v:6} {k}");
            }
            println!("distinct textures: a {} b {}", tex_a.len(), tex_b.len());
        }
        "materials-using" => {
            // Every material whose textures include a substring: materials-using <substr>
            let needle = args.get(1).context("usage: materials-using <substr>")?.to_ascii_lowercase();
            for (h, name) in &vfs.names {
                if !name.ends_with(".material.bin") {
                    continue;
                }
                let Ok(bytes) = vfs.read_hash(*h) else { continue };
                let Ok(m) = fc3_formats::material::parse(&bytes) else { continue };
                if m.textures.iter().any(|(_, t)| t.to_ascii_lowercase().contains(&needle)) {
                    let tex: Vec<String> = m.textures.iter().map(|(_, t)| t.rsplit('\\').next().unwrap_or(t).to_string()).collect();
                    println!("{name} [{}] {:?}", m.shader, tex);
                }
            }
        }
        "library" => {
            // Entity-library prototypes whose name or mesh contains a substring,
            // with their meshes and material overrides: library <substr> [max]
            let needle = args.get(1).context("usage: library <substr> [max]")?.to_ascii_lowercase();
            let max: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(30);
            let bytes = vfs.read(r"worlds\fc3_main\generated\entitylibrary_full.fcb")?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let ents = fc3_formats::world::parse_all_entities(&bytes[start..])?;
            println!("{} prototypes", ents.len());
            let mut n = 0;
            for e in ents {
                let hay = format!("{} {}", e.name, e.meshes.join(" ")).to_ascii_lowercase();
                if hay.contains(&needle) {
                    println!("{} | meshes {:?} | overrides {:?} | skeleton {:?}", e.name, e.meshes, e.material_overrides, e.skeleton);
                    n += 1;
                    if n >= max {
                        break;
                    }
                }
            }
        }
        "shadow-check" => {
            // A sector's baked shadow map (G16R16) against its heights: shadow-check <sector id>
            let id: u32 = args.get(1).context("usage: shadow-check <sector id>")?.parse()?;
            let c = fc3_formats::terrain::SectorCoord::from_id(id);
            let t = fc3_formats::terrain::parse_sdat(&vfs.read(&c.sdat_path("fc3_main"))?)?;
            let xbt = vfs.read(&c.shadow_path("fc3_main"))?;
            let dds = &xbt[xbt.windows(4).position(|w| w == b"DDS ").context("no DDS")?..];
            let px = &dds[128..];
            let texel = |x: usize, y: usize, k: usize| {
                let o = (y * 64 + x) * 4 + k * 2;
                u16::from_le_bytes([px[o], px[o + 1]]) as f32 / 65535.0 * 552.0
            };
            println!("heights {:.1}..{:.1}", t.min_height, t.max_height);
            for (label, transpose) in [("as stored", false), ("transposed", true)] {
                let (mut dr, mut dg, mut n) = (0.0f64, 0.0f64, 0.0f64);
                let (mut above_r, mut above_g) = (0, 0);
                for y in 0..64 {
                    for x in 0..64 {
                        let (sx, sy) = if transpose { (y, x) } else { (x, y) };
                        let h = t.height(sx, sy) as f64;
                        let (r, g) = (texel(x, y, 0) as f64, texel(x, y, 1) as f64);
                        dr += (r - h).abs();
                        dg += (g - h).abs();
                        above_r += usize::from(r >= h - 0.5);
                        above_g += usize::from(g >= h - 0.5);
                        n += 1.0;
                    }
                }
                println!(
                    "{label}: mean |R-h| {:.2} |G-h| {:.2}; R>=h {:.0}% G>=h {:.0}%",
                    dr / n,
                    dg / n,
                    above_r as f64 / n * 100.0,
                    above_g as f64 / n * 100.0
                );
            }
            for y in (0..64).step_by(16) {
                let row: Vec<String> =
                    (0..64).step_by(16).map(|x| format!("h{:5.1} r{:5.1} g{:5.1}", t.height(x, y), texel(x, y, 0), texel(x, y, 1))).collect();
                println!("  {}", row.join(" | "));
            }
            // Horizon angles along the four axes, traced through the
            // neighbouring sectors (up to 1 km), against the channels read as
            // 0.5 + angle / pi.
            let mut sectors: HashMap<(i32, i32), fc3_formats::terrain::TerrainSector> = HashMap::new();
            let mut height_at = |gx: i32, gy: i32| -> Option<f32> {
                let key = (c.x + gx.div_euclid(64), c.y + gy.div_euclid(64));
                if !sectors.contains_key(&key) {
                    let sc = fc3_formats::terrain::SectorCoord { x: key.0, y: key.1 };
                    let d = vfs.read(&sc.sdat_path("fc3_main")).ok().and_then(|b| fc3_formats::terrain::parse_sdat(&b).ok())?;
                    sectors.insert(key, d);
                }
                Some(sectors[&key].height(gx.rem_euclid(64) as usize, gy.rem_euclid(64) as usize))
            };
            // 16 azimuths, 0 = +x, counter-clockwise towards +y.
            let dirs: Vec<(String, f32, f32)> = (0..72)
                .map(|k| {
                    let a = k as f32 * std::f32::consts::TAU / 72.0;
                    (format!("{:5.1}", k as f32 * 5.0), a.cos(), a.sin())
                })
                .collect();
            let mut samples: Vec<(Vec<f32>, f32, f32)> = Vec::new();
            for y in (0..64).step_by(8) {
                for x in (0..64).step_by(8) {
                    let h0 = t.height(x, y);
                    let mut angles = vec![0.0f32; dirs.len()];
                    for (k, (_, dx, dy)) in dirs.iter().enumerate() {
                        let mut best = -90.0f32;
                        for step in 1..700 {
                            let d = step as f32 * 1.5;
                            let Some(h) = height_at(x as i32 + (dx * d).round() as i32, y as i32 + (dy * d).round() as i32) else { break };
                            best = best.max((h - h0).atan2(d).to_degrees());
                        }
                        angles[k] = best;
                    }
                    let to_angle = |v: f32| (v / 552.0 - 0.5) * 180.0;
                    // FC3_SHADOW_TRANSPOSE=1 reads the map transposed against the heights.
                    let (tx, ty) = if std::env::var("FC3_SHADOW_TRANSPOSE").is_ok() { (y, x) } else { (x, y) };
                    samples.push((angles, to_angle(texel(tx, ty, 0)), to_angle(texel(tx, ty, 1))));
                }
            }
            let corr = |a: &[f32], b: &[f32]| {
                let n = a.len() as f32;
                let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
                let cov: f32 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
                let va: f32 = a.iter().map(|x| (x - ma).powi(2)).sum();
                let vb: f32 = b.iter().map(|y| (y - mb).powi(2)).sum();
                cov / (va * vb).sqrt().max(1e-6)
            };
            let r: Vec<f32> = samples.iter().map(|s| s.1).collect();
            let g: Vec<f32> = samples.iter().map(|s| s.2).collect();
            let mut best_r = (f32::MIN, String::new());
            let mut best_g = (f32::MIN, String::new());
            for (k, (name, _, _)) in dirs.iter().enumerate() {
                let a: Vec<f32> = samples.iter().map(|s| s.0[k]).collect();
                let mean = a.iter().sum::<f32>() / a.len() as f32;
                let (cr, cg) = (corr(&a, &r), corr(&a, &g));
                if cr > best_r.0 {
                    best_r = (cr, format!("{name} (corr {cr:.3}, mean {mean:.1})"));
                }
                if cg > best_g.0 {
                    best_g = (cg, format!("{name} (corr {cg:.3}, mean {mean:.1})"));
                }
            }
            println!("best horizon for R: {}; for G: {}", best_r.1, best_g.1);
            println!("R as angle: mean {:5.1} deg, G: {:5.1} deg", r.iter().sum::<f32>() / r.len() as f32, g.iter().sum::<f32>() / g.len() as f32);
        }
        "sdat-info" => {
            // Layout of a terrain sector's data around a Dunia (x, y) point: sizes,
            // the bytes after the height samples, and odd samples (hole candidates).
            let x: f32 = args.get(1).context("usage: sdat-info x y")?.parse()?;
            let y: f32 = args.get(2).context("usage: sdat-info x y")?.parse()?;
            let c = fc3_formats::terrain::SectorCoord::containing(x, y);
            let path = c.sdat_path("fc3_main");
            let data = vfs.read(&path)?;
            let t = fc3_formats::terrain::parse_sdat(&data)?;
            println!("{path}: {} bytes, heights {:.1}..{:.1}, material {}", data.len(), t.min_height, t.max_height, t.material);
            let samples_end = 0x258 + 65 * 65 * 4;
            println!("bytes after samples (from {samples_end:#x}): {}", data.len().saturating_sub(samples_end));
            let mut hist = std::collections::BTreeMap::new();
            for nb in &t.normal_bytes {
                *hist.entry(nb[1] & 0xC0).or_insert(0) += 1;
            }
            println!("normal byte 1 top bits: {hist:?}");
            let lo = t.heights.iter().cloned().fold(f32::MAX, f32::min);
            let zero = t.heights.iter().filter(|h| **h == 0.0).count();
            println!("min sample {lo:.2}, zero samples {zero}");
            let [ox, oy] = c.origin();
            let (cx, cy) = (((x - ox) / 1.0).round() as usize, ((y - oy) / 1.0).round() as usize);
            println!("around ({cx},{cy}):");
            for row in cy.saturating_sub(3)..(cy + 4).min(65) {
                let line: Vec<String> = (cx.saturating_sub(3)..(cx + 4).min(65))
                    .map(|col| format!("{:6.1}/{:02x}{:02x}", t.height(col, row), t.normal_bytes[row * 65 + col][0], t.normal_bytes[row * 65 + col][1]))
                    .collect();
                println!("  {}", line.join(" "));
            }
            for flag in [0x40u8, 0xC0] {
                let pts: Vec<String> = (0..65 * 65)
                    .filter(|i| t.normal_bytes[*i][1] & 0xC0 == flag)
                    .map(|i| format!("({},{} {:.0})", ox + (i % 65) as f32, oy + (i / 65) as f32, t.heights[i]))
                    .collect();
                println!("flag {flag:#x}: {} samples, first {}", pts.len(), pts.iter().take(24).cloned().collect::<Vec<_>>().join(" "));
            }
            if data.len() > samples_end {
                let tail = &data[samples_end..(samples_end + 64).min(data.len())];
                println!("tail: {}", tail.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
            }
        }
        "skin-parts" => {
            // Per primitive of LOD 0: rigid node, its bind pose, and the vertex bounds.
            let target = args.get(1).context("usage: skin-parts <path>")?;
            let bytes = vfs.read_hash(parse_target(&vfs, target)?)?;
            let info = fc3_formats::anim::skin_info_lods(&bytes, 1)?;
            let m = fc3_formats::mesh::decode(&bytes, 1)?;
            for (i, n) in info.nodes.iter().enumerate() {
                let w = &info.bind_world[i];
                let ib = &info.inverse_bind[i];
                let prod = fc3_formats::anim::mat_mul(w, ib);
                let ident = prod.iter().enumerate().all(|(k, v)| (v - if k % 5 == 0 { 1.0 } else { 0.0 }).abs() < 1e-3);
                println!(
                    "node {i:2} {:24} parent {:2} bind {:?} world t=({:.3},{:.3},{:.3}) world*inv=I {ident}",
                    n.name, n.parent, n.bind_index, w[12], w[13], w[14]
                );
            }
            if let Some(lod) = info.lods.first() {
                let (_, mesh) = &m.lods[0];
                for p in &lod.primitives {
                    let vs = p.vertex_start as usize..(p.vertex_start + p.vertex_count) as usize;
                    let mut lo = [f32::MAX; 3];
                    let mut hi = [f32::MIN; 3];
                    for v in vs.clone() {
                        if let Some(q) = mesh.positions.get(v) {
                            for k in 0..3 {
                                lo[k] = lo[k].min(q[k]);
                                hi[k] = hi[k].max(q[k]);
                            }
                        }
                    }
                    println!(
                        "prim part {} cluster {} mat {} verts {:?} rigid {:?} palette {:?} lo ({:.3},{:.3},{:.3}) hi ({:.3},{:.3},{:.3})",
                        p.part,
                        p.cluster,
                        p.material,
                        vs,
                        p.rigid_node,
                        &p.palette[..p.palette.len().min(6)],
                        lo[0],
                        lo[1],
                        lo[2],
                        hi[0],
                        hi[1],
                        hi[2]
                    );
                }
            }
        }
        "hkx-bodies" => {
            // A model's rigid bodies and their entity bytes: hkx-bodies <hkx> [from] [to]
            let path = args.get(1).context("usage: hkx-bodies <hkx> [from] [to]")?;
            let hex = |i: usize, d: u32| args.get(i).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(d);
            let (from, to) = (hex(2, 0xC0), hex(3, 0xF0));
            for (name, bytes) in fc3_formats::havok::body_bytes(&vfs.read(path)?, from, to)? {
                let words: Vec<String> = bytes.chunks(4).map(|c| format!("{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])).collect();
                println!("{name:40} {}", words.join(" "));
            }
        }
        "veg-near" => {
            // Vegetation meshes within r metres of Dunia (x, y), with their
            // stored angles (degrees): veg-near <x> <y> <r>
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(r)) = (num(1), num(2), num(3)) else { bail!("usage: veg-near <x> <y> <r>") };
            let c = fc3_formats::terrain::SectorCoord::containing(x, y);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let s = fc3_formats::terrain::SectorCoord::new(c.x + dx, c.y + dy);
                    let Ok(plants) = fc3_formats::vegetation::sector_vegetation(&vfs, s.id()) else { continue };
                    for p in plants.iter().filter(|p| ((p.pos[0] - x).powi(2) + (p.pos[1] - y).powi(2)).sqrt() <= r) {
                        println!("{:?} {} at {:.2?} angles {:.1?} scale {}", p.kind, p.model, p.pos, p.angles, p.scale);
                    }
                }
            }
        }
        "state-survey" => {
            // Models with destruction states (nodes named `STATE02...`):
            // their state nodes, and how many models have them.
            let mut n = 0;
            for name in vfs.names.values().filter(|n| n.ends_with(".xbg")) {
                let Ok(bytes) = vfs.read(name) else { continue };
                let Ok(skin) = fc3_formats::anim::skin_info(&bytes) else { continue };
                let states: Vec<&str> = skin.nodes.iter().map(|n| n.name.as_str()).filter(|n| n.to_ascii_uppercase().starts_with("STATE0")).collect();
                if states.iter().any(|s| !s.to_ascii_uppercase().starts_with("STATE01")) {
                    n += 1;
                    println!("{name}\t{}", states.join(" "));
                }
            }
            eprintln!("{n} models with destruction states");
        }
        "model-survey" => {
            let mut errs = 0;
            let mut n = 0;
            let mut shaders: BTreeMap<String, usize> = BTreeMap::new();
            let mut texparams: BTreeMap<u32, (usize, String)> = BTreeMap::new();
            let mut mats_seen = std::collections::HashSet::new();
            for (h, name) in &vfs.names {
                if !name.ends_with(".xbg") {
                    continue;
                }
                n += 1;
                let bytes = vfs.read_hash(*h)?;
                match fc3_formats::mesh::decode(&bytes, 1) {
                    Ok(m) => {
                        for mr in &m.materials {
                            if !mats_seen.insert(mr.path.clone()) {
                                continue;
                            }
                            if let Ok(mat) = vfs.read(&mr.path).and_then(|b| fc3_formats::material::parse(&b)) {
                                *shaders.entry(mat.shader.clone()).or_default() += 1;
                                for (ph, t) in &mat.textures {
                                    let e = texparams.entry(*ph).or_insert((0, t.clone()));
                                    e.0 += 1;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        errs += 1;
                        if errs < 15 {
                            eprintln!("{name}: {e:#}");
                        }
                    }
                }
            }
            println!("{n} models, {errs} errors, {} unique materials", mats_seen.len());
            println!("shaders: {shaders:?}");
            for (h, (c, ex)) in texparams {
                println!("tex param {h:08X} x{c} e.g. {ex}");
            }
        }
        "scene-bindings" => {
            // A sequence's groups and what its scene entities bind them to:
            // scene-bindings <sequence path part>
            use fc3_formats::{cinema, story};
            let part = args.get(1).context("usage: scene-bindings <sequence path part>")?.to_ascii_lowercase();
            let root = fc3_formats::cache::default_cache_root();
            let entities = story::from_bytes(&std::fs::read(root.join(story::ENTITIES_REL))?)?;
            let by_id: HashMap<u64, &story::StoryEntity> = entities.iter().map(|e| (e.id, e)).collect();
            let names = component_names();
            for e in &entities {
                let Some(c) = e.components.children.iter().find(|c| c.is("CCinematicSceneComponent")) else { continue };
                let mut entries = Vec::new();
                c.walk(&mut |o, _| {
                    if let Some(p) = o.str_h(0xB1C2_6EBB).filter(|p| p.to_ascii_lowercase().contains(&part)) {
                        entries.push((p.to_ascii_lowercase(), o.clone()));
                    }
                });
                for (path, entry) in entries {
                    println!("scene {} ({}) plays {path}", e.name, e.id);
                    let mut binds: BTreeMap<u64, u64> = BTreeMap::new();
                    entry.walk(&mut |b, _| {
                        if let (Some(entity), Some(track)) = (b.u64("EntityID"), b.u64("TrackID")) {
                            binds.insert(track, entity);
                        }
                    });
                    let seq = std::fs::read_to_string(root.join(cinema::sequence_rel(&path))).ok().and_then(|t| cinema::Sequence::from_text(&t));
                    for g in seq.iter().flat_map(|s| &s.groups) {
                        let kinds: BTreeSet<String> =
                            g.clips.iter().map(|c| format!("{:?}", c.kind).split([' ', '{', '(']).next().unwrap_or_default().to_string()).collect();
                        let bound = binds.get(&g.uid).map(|id| match by_id.get(id) {
                            Some(b) => {
                                let comps: Vec<String> = b
                                    .components
                                    .children
                                    .iter()
                                    .map(|c| names.get(c.name_hash))
                                    .filter(|n| n.contains("Graphic") || n.contains("Pawn") || n.contains("Vehicle"))
                                    .collect();
                                format!("{} [{}] {}", b.name, b.class, comps.join(" "))
                            }
                            None => format!("{id} (not a story entity)"),
                        });
                        println!(
                            "  group {} {:?} '{}' archetype {:x}: {kinds:?} -> {}",
                            g.uid,
                            g.kind,
                            g.name,
                            g.archetype,
                            bound.unwrap_or_else(|| "unbound".into())
                        );
                    }
                }
            }
        }
        "skills" => {
            // The skill tree as the converter reads it: skills [--bonuses]
            use fc3_formats::{fcb, oasis, progression as p};
            let bytes = vfs.read(p::TEMPLATES)?;
            let at = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let templates = fcb::parse(&bytes[at..])?;
            let strings = oasis::Strings::from_bytes(&vfs.read(r"languages\english\oasisstrings_compressed.bin")?)?;
            let tree = p::skill_tree(&templates, &|id| strings.get(id).unwrap_or_default().to_string())?;
            let bonuses = args.iter().any(|a| a == "--bonuses");
            println!("{} branches, {} abilities, {} levels", tree.branches.len(), tree.abilities.len(), tree.xp_levels.len());
            for b in &tree.branches {
                println!("branch {} {} '{}'", b.number, b.key, b.name);
            }
            for a in &tree.abilities {
                println!(
                    "{:40} b{} cost {} r{} c{} buy {} avail {} mission '{}' script {} stat {}/{} req {:?} value {} '{}'",
                    a.id, a.branch, a.cost, a.row, a.column, a.purchasable, a.available, a.unlock_mission, a.unlock_script, a.stat, a.stat_target, a.requires, a.value, a.name
                );
                if a.stat != 0 {
                    println!("    stat {} x{}: '{}'", a.stat, a.stat_target, a.stat_text);
                }
                if bonuses {
                    for x in &a.bonuses {
                        println!("    bonus attr {} {} {}", x.attribute, if x.additive { "+" } else { "x" }, x.value);
                    }
                }
            }
        }
        "facial-voice" => {
            // How many spoken lines have a lip-sync clip under the engine's
            // naming, with a few found and missing: facial-voice
            let lines = fc3_formats::oasis::speech_lengths(&vfs.read(r"languages\english\speechlength.bin")?)?;
            let mut ids: Vec<u32> = lines.keys().copied().collect();
            ids.sort();
            let found: Vec<u32> = ids.iter().copied().filter(|&id| vfs.exists(&fc3_formats::facial::voice_clip_path(id))).collect();
            println!("{} lines, {} with a clip; found e.g. {:x?}", ids.len(), found.len(), &found[..found.len().min(5)]);
            let unnamed = vfs.index.keys().filter(|h| vfs.name_of(**h).is_none()).count();
            println!("{unnamed} unnamed archive entries");
        }
        "hkx-classes" => {
            // How often each Havok class appears in the models' .hkx files
            // (files, objects): hkx-classes [substr]
            let filter = args.get(1).map(|s| s.to_ascii_lowercase());
            let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            let mut files = 0;
            for n in vfs.names.values().filter(|n| n.ends_with(".xbg")) {
                if filter.as_ref().is_some_and(|f| !n.contains(f.as_str())) {
                    continue;
                }
                let Ok(bytes) = vfs.read(&format!("{}.hkx", &n[..n.len() - 4])) else { continue };
                let Ok(classes) = fc3_formats::havok::object_classes(&bytes) else { continue };
                files += 1;
                let mut seen = BTreeSet::new();
                for c in classes {
                    let e = by.entry(c.clone()).or_default();
                    e.1 += 1;
                    if seen.insert(c) {
                        e.0 += 1;
                    }
                }
            }
            println!("{files} files");
            let mut rows: Vec<_> = by.into_iter().collect();
            rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
            for (c, (f, o)) in rows {
                println!("{f:6} files {o:7} objects  {c}");
            }
        }
        "hkx-file" => {
            // One packfile's classes and collision (its own space), with the
            // triangles reaching into a box, or a class's objects' words:
            // hkx-file <path> [x y z half | hkClass]
            let path = args.get(1).context("usage: hkx-file <path> [x y z half]")?;
            let bytes = vfs.read(path)?;
            let mut by: BTreeMap<String, usize> = BTreeMap::new();
            for c in fc3_formats::havok::object_classes(&bytes)? {
                *by.entry(c).or_default() += 1;
            }
            println!("{by:?}");
            let g = fc3_formats::havok::collision_geometry(&bytes)?;
            println!("{} bodies, {} triangles, shapes {:?}, unhandled {:?}", g.bodies, g.triangles.len(), g.shapes, g.unhandled);
            let (lo, hi) = g.positions.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| {
                (std::array::from_fn(|i| lo[i].min(p[i])), std::array::from_fn(|i| hi[i].max(p[i])))
            });
            println!("bounds {lo:?} .. {hi:?}");
            if let Some(class) = args.get(2).filter(|a| a.starts_with("hk")) {
                for (o, words, links) in fc3_formats::havok::object_words(&bytes, class, 0, 0x80)? {
                    let floats: Vec<String> = words.iter().map(|w| format!("{:.2}", f32::from_bits(*w))).collect();
                    println!("{o:x}: {} {links:?}", floats.join(" "));
                }
                return Ok(());
            }
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            if let (Some(x), Some(y), Some(z), Some(h)) = (num(2), num(3), num(4), num(5)) {
                let c = [x, y, z];
                for t in &g.triangles {
                    let v = t.map(|i| g.positions[i as usize]);
                    if v.iter().any(|p| (0..3).all(|i| (p[i] - c[i]).abs() <= h)) {
                        println!("{v:?}");
                    }
                }
            }
        }
        "hkx-surfaces" => {
            // Collision surfaces of models with an .hkx: hkx-surfaces [substr] [--each]
            let filter = args.get(1).filter(|a| !a.starts_with("--")).map(|s| s.to_ascii_lowercase());
            let each = args.iter().any(|a| a == "--each");
            let (mut models, mut mixed, mut mixed_up_tris, mut errors) = (0, 0, 0usize, 0);
            let mut names: Vec<&String> = vfs.names.values().filter(|n| n.ends_with(".xbg")).collect();
            names.sort();
            for n in names {
                if filter.as_ref().is_some_and(|f| !n.contains(f.as_str())) {
                    continue;
                }
                let hkx = format!("{}.hkx", &n[..n.len() - 4]);
                let Ok(bytes) = vfs.read(&hkx) else { continue };
                let tris = match fc3_formats::havok::surface_triangles(&bytes) {
                    Ok(t) => t,
                    Err(e) => {
                        errors += 1;
                        if each {
                            println!("{hkx}: {e:#}");
                        }
                        continue;
                    }
                };
                models += 1;
                let coarse: Vec<_> = tris.iter().filter(|t| !t.detail).copied().collect();
                let walk = if coarse.is_empty() { &tris } else { &coarse };
                let up = fc3_formats::havok::surface_areas(walk, true);
                let all = fc3_formats::havok::surface_areas(&tris, false);
                if up.len() > 1 {
                    mixed += 1;
                    mixed_up_tris += walk
                        .iter()
                        .filter(|t| t.logic != up[0].0)
                        .filter(|t| {
                            let [a, b, c] = t.corners;
                            let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - a[0], c[1] - a[1]]);
                            (u[0] * v[1] - u[1] * v[0]).abs() > 1e-6
                        })
                        .count();
                }
                if each {
                    let fmt = |v: &[(u8, f32)]| v.iter().map(|(l, a)| format!("{l}:{a:.1}")).collect::<Vec<_>>().join(" ");
                    println!("{n}: {} tris ({} coarse); up [{}] all [{}]", tris.len(), coarse.len(), fmt(&up), fmt(&all));
                }
            }
            println!("{models} models, {mixed} with mixed walkable surfaces ({mixed_up_tris} non-vertical minority tris), {errors} errors");
        }
        "hkx-collision" => {
            // Full Havok collision of models with an .hkx, against their render
            // bounds (both Bevy space): hkx-collision [substr] [--survey] [--worst N]
            use rayon::prelude::*;
            let filter = args.get(1).filter(|a| !a.starts_with("--")).map(|s| s.to_ascii_lowercase());
            let survey = args.iter().any(|a| a == "--survey");
            let worst: usize = args.iter().position(|a| a == "--worst").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(25);
            let mut names: Vec<&String> =
                vfs.names.values().filter(|n| n.ends_with(".xbg") && filter.as_ref().is_none_or(|f| n.contains(f.as_str()))).collect();
            names.sort();
            names.dedup();
            let to_bevy = |(mn, mx): ([f32; 3], [f32; 3])| ([mn[0], mn[2], -mx[1]], [mx[0], mx[2], -mn[1]]);
            struct Row {
                name: String,
                geo: Option<fc3_formats::havok::CollisionGeometry>,
                error: Option<String>,
                render: Option<([f32; 3], [f32; 3])>,
            }
            let rows: Vec<Row> = names
                .par_iter()
                .filter_map(|n| {
                    let hkx = vfs.read(&format!("{}.hkx", &n[..n.len() - 4])).ok()?;
                    let render = vfs.read(n).ok().and_then(|b| fc3_formats::mesh::decode(&b, 1).ok()).map(|m| (m.bbox_min, m.bbox_max));
                    Some(match fc3_formats::havok::collision_geometry(&hkx) {
                        Ok(g) => Row { name: n.to_string(), geo: Some(g), error: None, render },
                        Err(e) => Row { name: n.to_string(), geo: None, error: Some(format!("{e:#}")), render },
                    })
                })
                .collect();
            // How far the collision box reaches past the render box, and how much
            // of the render box it leaves out (largest per side, metres).
            let compare = |h: ([f32; 3], [f32; 3]), r: ([f32; 3], [f32; 3])| {
                let over = (0..3).map(|k| (r.0[k] - h.0[k]).max(h.1[k] - r.1[k])).fold(0.0f32, f32::max);
                let short = (0..3).map(|k| (h.0[k] - r.0[k]).max(r.1[k] - h.1[k])).fold(0.0f32, f32::max);
                let diag = (0..3).map(|k| (r.1[k] - r.0[k]).powi(2)).sum::<f32>().sqrt();
                (over, short, diag)
            };
            let fmt3 = |v: [f32; 3]| format!("({:.2}, {:.2}, {:.2})", v[0], v[1], v[2]);
            let (mut empty, mut errors, mut detail_only, mut agree, mut bad_over, mut bad_short) = (0, 0, 0, 0, 0, 0);
            let mut tris = 0usize;
            let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
            let mut unhandled: BTreeMap<String, usize> = BTreeMap::new();
            let mut over_buckets = [0usize; 4];
            let mut flagged: Vec<(f32, String)> = Vec::new();
            for r in &rows {
                let Some(g) = &r.geo else {
                    errors += 1;
                    if !survey {
                        println!("{}: error {}", r.name, r.error.as_deref().unwrap_or(""));
                    }
                    continue;
                };
                tris += g.triangles.len();
                for (c, n) in &g.shapes {
                    *shapes.entry(c.clone()).or_default() += n;
                }
                for (c, n) in &g.unhandled {
                    *unhandled.entry(c.clone()).or_default() += n;
                }
                detail_only += (g.detail_bodies > 0) as usize;
                let Some(h) = g.bounds().map(to_bevy) else {
                    empty += 1;
                    if !survey {
                        println!("{}: no collision triangles; bodies {:?}", r.name, g.body_names);
                    }
                    continue;
                };
                let verdict = r.render.map(|rb| {
                    let (over, short, diag) = compare(h, rb);
                    let bucket = [0.1, 0.5, 2.0].iter().position(|&t| over < t).unwrap_or(3);
                    over_buckets[bucket] += 1;
                    // Out past the render box: misplaced. Far short of it on a side:
                    // possibly missing pieces (or a model whose bounds include
                    // destruction debris or far-off helpers).
                    let is_over = over > 0.5 + 0.1 * diag;
                    let is_short = short > 1.0 + 0.5 * diag;
                    bad_over += is_over as usize;
                    bad_short += is_short as usize;
                    if is_over || is_short {
                        flagged.push((over.max(short), format!("{} over {over:.2} short {short:.2} (render diag {diag:.1})", r.name)));
                    } else {
                        agree += 1;
                    }
                    (over, short)
                });
                if !survey {
                    println!(
                        "{}\n  bodies {} of {} ({} detail) {:?}\n  shapes {:?}{}\n  {} tris, {} verts\n  collision {} .. {}\n  render    {}{}",
                        r.name,
                        g.bodies,
                        g.body_names.len(),
                        g.detail_bodies,
                        g.body_names.iter().map(|(n, t)| format!("{}{n}", if *t { "+" } else { "-" })).collect::<Vec<_>>(),
                        g.shapes,
                        if g.unhandled.is_empty() { String::new() } else { format!("\n  UNHANDLED {:?}", g.unhandled) },
                        g.triangles.len(),
                        g.positions.len(),
                        fmt3(h.0),
                        fmt3(h.1),
                        r.render.map_or("?".to_string(), |rb| format!("{} .. {}", fmt3(rb.0), fmt3(rb.1))),
                        verdict.map_or(String::new(), |(o, s)| format!("\n  over {o:.2} short {s:.2}")),
                    );
                }
            }
            println!(
                "{} files: {} with collision ({} using detailed bodies), {} without triangles, {} errors; {tris} triangles",
                rows.len(),
                rows.len() - empty - errors,
                detail_only,
                empty,
                errors
            );
            println!(
                "bounds vs render: {agree} agree, {bad_over} reach past the render box, {bad_short} fall well short; overhang <0.1 m {}, <0.5 m {}, <2 m {}, more {}",
                over_buckets[0], over_buckets[1], over_buckets[2], over_buckets[3]
            );
            println!("shapes {shapes:?}");
            println!("unhandled {unhandled:?}");
            // What the game leaves non-solid whatever the collision: models drawn
            // as foliage, ones too small to block (from the converted cache),
            // and ones made only of the no-impact material where the render mesh
            // has nothing solid.
            let root = fc3_formats::cache::default_cache_root();
            let names = vfs.read(fc3_formats::havok::LOGIC_MATERIALS).ok().and_then(|b| fc3_formats::havok::logic_material_names(&b).ok()).unwrap_or_default();
            let no_impact = names.iter().position(|n| n.to_ascii_lowercase().ends_with("noimpact")).map(|i| i as u32);
            let (mut foliage, mut small, mut cached, mut soft) = (Vec::new(), 0, 0, Vec::new());
            for r in rows.iter().filter(|r| r.geo.as_ref().is_some_and(|g| !g.triangles.is_empty())) {
                let Some(m) =
                    std::fs::read(root.join(fc3_formats::cache::model_rel(&r.name))).ok().and_then(|b| fc3_formats::cache::CachedModel::from_bytes(&b).ok())
                else {
                    continue;
                };
                cached += 1;
                let palette = r.geo.as_ref().map(|g| g.palette.as_slice()).unwrap_or(&[]);
                if !palette.is_empty() && palette.iter().all(|&l| Some(l) == no_impact) && m.collision_indices.is_empty() {
                    soft.push(r.name.rsplit('\\').next().unwrap_or(""));
                }
                let flag = |p: &fc3_formats::cache::CachedPart| m.material_flags.get(p.material as usize).copied().unwrap_or(0);
                let parts = m.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]);
                if !parts.is_empty() && parts.iter().all(|p| flag(p) & fc3_formats::cache::mat_flags::VEGETATION != 0) {
                    foliage.push(r.name.as_str());
                }
                let half: Vec<f32> = (0..3).map(|k| (m.aabb_max[k] - m.aabb_min[k]) * 0.5).collect();
                small += ((half.iter().map(|h| h * h).sum::<f32>()).sqrt() < 0.35) as usize;
            }
            println!(
                "of {cached} converted: {} drawn as foliage {:?}, {small} under the collider size floor, {} no-impact only with no render collision {:?}",
                foliage.len(),
                &foliage[..foliage.len().min(12)],
                soft.len(),
                &soft[..soft.len().min(12)]
            );
            if survey {
                flagged.sort_by(|a, b| b.0.total_cmp(&a.0));
                for (_, line) in flagged.iter().take(worst) {
                    println!("  {line}");
                }
            }
        }
        "col-ray" => {
            // What a ray from Dunia (x, y, z) along (dx, dy, dz) hits among the placed
            // models' colliders, from the converted cache: the render-derived mesh
            // and the game's own (.col): col-ray <x> <y> <z> <dx> <dy> <dz> [length]
            use fc3_formats::cache;
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(z), Some(dx), Some(dy), Some(dz)) = (num(1), num(2), num(3), num(4), num(5), num(6)) else {
                bail!("usage: col-ray <x> <y> <z> <dx> <dy> <dz> [length]")
            };
            let length = num(7).unwrap_or(10.0);
            let root = cache::default_cache_root();
            let centre = [x, z, -y];
            let dir = {
                let d = [dx, dz, -dy];
                let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
                d.map(|e| e / n)
            };
            let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            // Möller–Trumbore, both sides.
            let hit = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| -> Option<f32> {
                let (e1, e2) = (sub(b, a), sub(c, a));
                let p = cross(dir, e2);
                let det = dot(e1, p);
                if det.abs() < 1e-9 {
                    return None;
                }
                let s = sub(centre, a);
                let u = dot(s, p) / det;
                let q = cross(s, e1);
                let v = dot(dir, q) / det;
                let t = dot(e2, q) / det;
                (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t >= 0.0 && t <= length).then_some(t)
            };
            let rotate = |q: [f32; 4], v: [f32; 3]| {
                let (u, w) = ([q[0], q[1], q[2]], q[3]);
                let t = cross(u, v).map(|e| e * 2.0);
                let ut = cross(u, t);
                [0, 1, 2].map(|k| v[k] + w * t[k] + ut[k])
            };
            let here = fc3_formats::terrain::SectorCoord::containing(x, y);
            for sy in -1..=1 {
                for sx in -1..=1 {
                    let s = fc3_formats::terrain::SectorCoord::new(here.x + sx, here.y + sy);
                    let Some(sector) = std::fs::read(root.join(cache::sector_rel(s.id()))).ok().and_then(|b| cache::CachedSector::from_bytes(&b).ok()) else {
                        continue;
                    };
                    for p in &sector.placements {
                        let d = ((p.translation[0] - centre[0]).powi(2) + (p.translation[2] - centre[2]).powi(2)).sqrt();
                        if d > 60.0 + length {
                            continue;
                        }
                        let rel = &sector.models[p.model as usize];
                        let stem = rel.trim_start_matches("models/").trim_end_matches(".fcm");
                        let place = |v: [f32; 3]| {
                            let r = rotate(p.rotation, v.map(|e| e * p.scale));
                            [0, 1, 2].map(|k| r[k] + p.translation[k])
                        };
                        let first = |positions: &[[f32; 3]], indices: &[u32]| {
                            indices
                                .chunks_exact(3)
                                .filter_map(|t| hit(place(positions[t[0] as usize]), place(positions[t[1] as usize]), place(positions[t[2] as usize])))
                                .reduce(f32::min)
                        };
                        let model = std::fs::read(root.join(rel)).ok().and_then(|b| cache::CachedModel::from_bytes(&b).ok());
                        let render = model.as_ref().and_then(|m| first(&m.collision_positions, &m.collision_indices));
                        let own = u64::from_str_radix(stem, 16)
                            .ok()
                            .and_then(|h| std::fs::read(root.join(cache::collision_rel_of_hash(h))).ok())
                            .and_then(|b| cache::CachedCollision::from_bytes(&b).ok());
                        let havok = own.as_ref().map(|c| first(&c.positions, &c.indices));
                        if render.is_some() || havok.flatten().is_some() {
                            let name = sector.entities.get(p.entity as usize).map_or("?", |e| e.name.as_str());
                            let radius = model.as_ref().map_or(0.0, |m| (0..3).map(|k| ((m.aabb_max[k] - m.aabb_min[k]) * 0.5).powi(2)).sum::<f32>().sqrt());
                            let fmt = |t: Option<f32>| t.map_or("-".to_string(), |t| format!("{t:.2} m"));
                            println!("{name}  {rel}  radius {radius:.2}: render mesh {}, own {}", fmt(render), havok.map_or("(none)".to_string(), fmt));
                        }
                    }
                }
            }
        }
        "col-near" => {
            // Placed models whose collision reaches into a box round Dunia (x, y, z),
            // from the converted cache: render-derived and the game's own (.col)
            // triangles touching it: col-near <x> <y> <z> [half size, m]
            use fc3_formats::cache;
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(z)) = (num(1), num(2), num(3)) else { bail!("usage: col-near <x> <y> <z> [half]") };
            let half = num(4).unwrap_or(1.0);
            let root = cache::default_cache_root();
            let centre = [x, z, -y];
            let rotate = |q: [f32; 4], v: [f32; 3]| {
                let (u, w) = ([q[0], q[1], q[2]], q[3]);
                let c = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let t = c(u, v).map(|e| e * 2.0);
                let ut = c(u, t);
                [0, 1, 2].map(|k| v[k] + w * t[k] + ut[k])
            };
            let here = fc3_formats::terrain::SectorCoord::containing(x, y);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let s = fc3_formats::terrain::SectorCoord::new(here.x + dx, here.y + dy);
                    let Some(sector) = std::fs::read(root.join(cache::sector_rel(s.id()))).ok().and_then(|b| cache::CachedSector::from_bytes(&b).ok()) else {
                        continue;
                    };
                    for p in &sector.placements {
                        let d = ((p.translation[0] - centre[0]).powi(2) + (p.translation[2] - centre[2]).powi(2)).sqrt();
                        if d > 60.0 {
                            continue;
                        }
                        let rel = &sector.models[p.model as usize];
                        let stem = rel.trim_start_matches("models/").trim_end_matches(".fcm");
                        let place = |v: [f32; 3]| {
                            let r = rotate(p.rotation, v.map(|e| e * p.scale));
                            [0, 1, 2].map(|k| r[k] + p.translation[k])
                        };
                        let touching = |positions: &[[f32; 3]], indices: &[u32]| {
                            indices
                                .chunks_exact(3)
                                .filter(|t| {
                                    let c: Vec<[f32; 3]> = t.iter().map(|&i| place(positions[i as usize])).collect();
                                    (0..3).all(|k| {
                                        let lo = c.iter().map(|v| v[k]).fold(f32::MAX, f32::min);
                                        let hi = c.iter().map(|v| v[k]).fold(f32::MIN, f32::max);
                                        lo <= centre[k] + half && hi >= centre[k] - half
                                    })
                                })
                                .count()
                        };
                        let model = std::fs::read(root.join(rel)).ok().and_then(|b| cache::CachedModel::from_bytes(&b).ok());
                        let render = model.as_ref().map_or(0, |m| touching(&m.collision_positions, &m.collision_indices));
                        let own = u64::from_str_radix(stem, 16)
                            .ok()
                            .and_then(|h| std::fs::read(root.join(cache::collision_rel_of_hash(h))).ok())
                            .and_then(|b| cache::CachedCollision::from_bytes(&b).ok());
                        let havok = own.as_ref().map(|c| touching(&c.positions, &c.indices));
                        if render > 0 || havok.is_some_and(|n| n > 0) {
                            let name = sector.entities.get(p.entity as usize).map_or("?", |e| e.name.as_str());
                            let radius = model.as_ref().map_or(0.0, |m| (0..3).map(|k| ((m.aabb_max[k] - m.aabb_min[k]) * 0.5).powi(2)).sum::<f32>().sqrt());
                            println!(
                                "{d:5.1} m  {name}  {rel}  radius {radius:.2}  render tris {render}  own {}",
                                havok.map_or("none".to_string(), |n| format!("tris {n}"))
                            );
                        }
                    }
                }
            }
        }
        "water-headers" => {
            // Every sector's water fields: water-headers [level] -> id x y f34 f38 level min max
            use fc3_formats::terrain::{SectorCoord, WORLD_SECTORS};
            let want: Option<f32> = args.get(1).and_then(|v| v.parse().ok());
            let f = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let u = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            for id in 0..(WORLD_SECTORS * WORLD_SECTORS) as u32 {
                let c = SectorCoord::from_id(id);
                let Ok(b) = vfs.read(&c.sdat_path("fc3_main")) else { continue };
                if b.len() < 0x40 {
                    continue;
                }
                let level = f(&b, 0x3C);
                if want.is_some_and(|w| (w - level).abs() > 0.5) {
                    continue;
                }
                println!("{id} {} {} {} {} {level} {:.1} {:.1}", c.x, c.y, u(&b, 0x34), u(&b, 0x38), f(&b, 0x1C), f(&b, 0x20));
            }
        }
        "water-planes" => {
            // Sectors with an inland water plane and its level: water-planes
            use fc3_formats::terrain::{water_plane, SectorCoord, WORLD_SECTORS};
            let mut levels: BTreeMap<i32, usize> = BTreeMap::new();
            let mut n = 0;
            for id in 0..(WORLD_SECTORS * WORLD_SECTORS) as u32 {
                let c = SectorCoord::from_id(id);
                let Ok(b) = vfs.read(&c.sdat_path("fc3_main")) else { continue };
                if let Some(level) = water_plane(&b) {
                    n += 1;
                    *levels.entry(level.round() as i32).or_default() += 1;
                }
            }
            println!("{n} sectors with inland water; by level: {levels:?}");
        }
        "rml" => {
            // Binary or text XML as an indented tree: rml <path|hash> [section]
            let target = args.get(1).context("usage: rml <path|hash> [section]")?;
            let root = fc3_formats::rml::parse(&vfs.read_hash(parse_target(&vfs, target)?)?)?;
            let node = match args.get(2) {
                Some(s) => root.find(s).context("no such section")?,
                None => &root,
            };
            fn show(n: &fc3_formats::rml::Node, depth: usize) {
                let attrs: Vec<String> = n.attrs.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                println!("{}<{}> {} {}", "  ".repeat(depth), n.name, n.value, attrs.join(" "));
                for c in &n.children {
                    show(c, depth + 1);
                }
            }
            show(node, 0);
        }
        "fcb" => {
            let target = args.get(1).context("usage: fcb <path|hash> [depth]")?;
            let bytes = vfs.read_hash(parse_target(&vfs, target)?)?;
            // (Encounter prefabs are packed: their size, then LZO.)
            let bytes = fc3_formats::encounters::unpack(&bytes).unwrap_or(bytes);
            let depth: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(64);
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            // Field names from the installed game's own binary (local only).
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            let mut s = String::new();
            fc3_formats::fcb::dump(&root, &names, depth, &mut s);
            println!("{s}");
        }
        "sector-survey" => {
            // Survey entity classes and model references across world sector files.
            let filter = args.get(1).cloned().unwrap_or_else(|| "levels\\c5\\".into());
            let class_h = fc3_formats::hash::name_hash("hidEntityClass");
            let _ = class_h;
            let type_h = 0xD2B3429Eu32;
            let mut classes: BTreeMap<String, usize> = BTreeMap::new();
            let mut models: BTreeMap<String, usize> = BTreeMap::new();
            let mut exts: BTreeMap<String, usize> = BTreeMap::new();
            let (mut ents, mut with_model, mut files) = (0, 0, 0);
            for (h, name) in &vfs.names {
                if !name.contains(&filter) || !name.ends_with(".data.fcb") {
                    continue;
                }
                files += 1;
                let bytes = vfs.read_hash(*h)?;
                let Ok(root) = fc3_formats::fcb::parse(&bytes) else { continue };
                root.walk(&mut |o, _| {
                    if !o.is("Entity") {
                        return;
                    }
                    ents += 1;
                    *classes.entry(o.str_h(type_h).unwrap_or_default()).or_default() += 1;
                    let mut found = false;
                    o.walk(&mut |c, _| {
                        for (_, v) in &c.fields {
                            if v.len() > 5 && v.last() == Some(&0) && v[..v.len() - 1].iter().all(|&b| (0x20..0x7f).contains(&b)) {
                                let s = fc3_formats::fcb::bytes_to_string(v).to_ascii_lowercase();
                                if let Some(dot) = s.rfind('.') {
                                    if s.contains('\\') {
                                        *exts.entry(s[dot..].to_string()).or_default() += 1;
                                    }
                                }
                                if s.ends_with(".xbg") {
                                    found = true;
                                    *models.entry(s).or_default() += 1;
                                }
                            }
                        }
                    });
                    if found {
                        with_model += 1;
                    }
                });
            }
            // Which (component, object, field) paths hold xbg strings?
            let mut slots: BTreeMap<String, usize> = BTreeMap::new();
            for (h, name) in &vfs.names {
                if !name.contains(&filter) || !name.ends_with(".data.fcb") {
                    continue;
                }
                let bytes = vfs.read_hash(*h)?;
                let Ok(root) = fc3_formats::fcb::parse(&bytes) else { continue };
                fn go(o: &fc3_formats::fcb::Object, path: &mut Vec<u32>, slots: &mut BTreeMap<String, usize>) {
                    for (fh, v) in &o.fields {
                        let s = fc3_formats::fcb::bytes_to_string(v).to_ascii_lowercase();
                        if s.ends_with(".xbg") {
                            let p: Vec<String> = path.iter().map(|h| format!("{h:08X}")).collect();
                            *slots.entry(format!("{} / {fh:08X}", p.join("/"))).or_default() += 1;
                        }
                    }
                    for c in &o.children {
                        path.push(c.name_hash);
                        go(c, path, slots);
                        path.pop();
                    }
                }
                go(&root, &mut vec![], &mut slots);
            }
            for (k, v) in &slots {
                println!("slot {v:6} {k}");
            }
            println!("{files} files, {ents} entities, {with_model} with xbg, {} unique models", models.len());
            println!("classes: {classes:?}");
            println!("exts: {exts:?}");
            let mut m: Vec<_> = models.into_iter().collect();
            m.sort_by_key(|x| std::cmp::Reverse(x.1));
            for (n, c) in m.iter().take(40) {
                println!("{c:6} {n}");
            }
        }
        "entity-export" => {
            // Every placed entity of fc3_main, one per line (tab separated):
            // id, class, name, Dunia pos, angles, solid, first mesh and its
            // path hash: entity-export <out.tsv>
            let out = args.get(1).context("usage: entity-export <out.tsv>")?;
            let world = fc3_formats::terrain::WORLD_SECTORS;
            let mut text = String::new();
            for y in 0..world {
                for x in 0..world {
                    let s = fc3_formats::terrain::SectorCoord::new(x, y);
                    let Ok(bytes) = vfs.read(&s.entities_path("fc3_main")) else { continue };
                    let Ok(ents) = fc3_formats::world::parse_all_entities(&bytes) else { continue };
                    for e in ents {
                        let mesh = e.meshes.first().cloned().unwrap_or_default();
                        let hash = if mesh.is_empty() { String::new() } else { fc3_formats::cache::hash_name(&mesh) };
                        text.push_str(&format!(
                            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            e.id,
                            e.class,
                            e.name,
                            e.pos[0],
                            e.pos[1],
                            e.pos[2],
                            e.angles[0],
                            e.angles[1],
                            e.angles[2],
                            e.solid as u8,
                            mesh,
                            hash,
                            s.id()
                        ));
                    }
                }
            }
            std::fs::write(out, text)?;
        }
        "entities-near" => {
            // Entities within r metres of Dunia (x, y), from the sectors round
            // it and (--meta) every metasector: entities-near <x> <y> <r> [--meta]
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(r)) = (num(1), num(2), num(3)) else { bail!("usage: entities-near <x> <y> <r> [--meta]") };
            let c = fc3_formats::terrain::SectorCoord::containing(x, y);
            let reach = (r / 64.0).ceil() as i32 + 1;
            let mut files: Vec<String> = Vec::new();
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    files.push(fc3_formats::terrain::SectorCoord::new(c.x + dx, c.y + dy).entities_path("fc3_main"));
                }
            }
            if args.iter().any(|a| a == "--meta") {
                let meta = |n: &&String| n.contains(r"worlds\fc3_main\") && n.contains(r"\metasectors\") && n.ends_with(".data.fcb");
                files.extend(vfs.names.values().filter(meta).cloned());
            }
            {
                for path in &files {
                    let Ok(bytes) = vfs.read(path) else { continue };
                    let Ok(ents) = fc3_formats::world::parse_all_entities(&bytes) else { continue };
                    for e in ents {
                        let d = ((e.pos[0] - x).powi(2) + (e.pos[1] - y).powi(2)).sqrt();
                        if d <= r {
                            let mesh = e.meshes.first().map_or(String::new(), |m| format!("  {m}"));
                            println!(
                                "{d:6.1}  {:28} {} at ({:.1}, {:.1}, {:.1}){mesh}  angles [{:.1}, {:.1}, {:.1}]",
                                e.class, e.name, e.pos[0], e.pos[1], e.pos[2], e.angles[0], e.angles[1], e.angles[2]
                            );
                        }
                    }
                }
            }
        }
        "spline-survey" => {
            // Spline primitives (rivers, roads...) by material: count, points, height range.
            // spline-survey [material substring] -> with a filter, one line per spline.
            let only = args.get(1).map(|s| s.to_ascii_lowercase());
            let mut by_mat: BTreeMap<String, (usize, usize, f32, f32)> = BTreeMap::new();
            for name in vfs.names.values().filter(|n| n.contains(r"worlds\fc3_main\") && n.ends_with(".data.fcb")) {
                let Ok(bytes) = vfs.read(name) else { continue };
                let Ok(root) = fc3_formats::fcb::parse(&bytes) else { continue };
                fn walk(o: &fc3_formats::fcb::Object, out: &mut Vec<fc3_formats::fcb::Object>) {
                    for c in &o.children {
                        if c.is("Entity") {
                            if c.str("hidName").is_some_and(|n| n.starts_with("SplinePrimitiveEntity")) {
                                out.push((**c).clone());
                            }
                        } else {
                            walk(c, out);
                        }
                    }
                }
                let mut found = Vec::new();
                walk(&root, &mut found);
                for e in &found {
                    let Some(comps) = e.child("Components") else { continue };
                    let Some(prim) = comps.children.iter().find(|c| c.name_hash == 0xE2FD401E) else { continue };
                    let mat = prim.str_h(0xEF034902).unwrap_or_default().to_ascii_lowercase();
                    let pts: Vec<[f32; 3]> = prim
                        .children
                        .iter()
                        .flat_map(|c| c.children.iter())
                        .find_map(|c| c.field_h(0x546E67DF))
                        .map(|b| {
                            b.get(4..).unwrap_or(&[]).chunks_exact(12).map(|p| [0, 4, 8].map(|o| f32::from_le_bytes(p[o..o + 4].try_into().unwrap()))).collect()
                        })
                        .unwrap_or_default();
                    let (lo, hi) = pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[2]), hi.max(p[2])));
                    if let Some(f) = &only {
                        if mat.contains(f.as_str()) {
                            let first = pts.first().copied().unwrap_or_default();
                            println!(
                                "{} pts {} z {lo:.1}..{hi:.1} from {:.0},{:.0} {mat}",
                                e.str("hidName").unwrap_or_default(),
                                pts.len(),
                                first[0],
                                first[1]
                            );
                        }
                        continue;
                    }
                    let m = by_mat.entry(mat).or_insert((0, 0, f32::MAX, f32::MIN));
                    m.0 += 1;
                    m.1 += pts.len();
                    m.2 = m.2.min(lo);
                    m.3 = m.3.max(hi);
                }
            }
            for (m, (n, p, lo, hi)) in &by_mat {
                println!("{n:5} splines {p:6} points z {lo:7.1}..{hi:7.1}  {m}");
            }
        }
        "spline-dump" => {
            // The control points of spline primitives near Dunia (x, y): spline-dump <x> <y> <r>
            use fc3_formats::terrain::SectorCoord;
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(r)) = (num(1), num(2), num(3)) else { bail!("usage: spline-dump <x> <y> <r>") };
            let c = SectorCoord::containing(x, y);
            let reach = (r / 64.0).ceil() as i32 + 1;
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let s = SectorCoord::new(c.x + dx, c.y + dy);
                    let Ok(bytes) = vfs.read(&s.entities_path("fc3_main")) else { continue };
                    let Ok(ents) = fc3_formats::world::parse_all_entities(&bytes) else { continue };
                    for e in ents {
                        let Some(sp) = e.spline else { continue };
                        let near = sp.sections.iter().any(|s| ((s.points[0][0] - x).powi(2) + (s.points[0][1] - y).powi(2)).sqrt() < r);
                        if !near {
                            continue;
                        }
                        println!("{} half-width {} draw {} sections {} {}", e.name, sp.half_width, sp.draw_distance, sp.sections.len(), sp.material);
                        for s in sp.sections.iter().take(4) {
                            println!("  {:.2?} d {:.2} w {:.3}", s.points, s.distance, s.weight);
                        }
                    }
                }
            }
        }
        "terrain-height" => {
            // The converted terrain's height under Dunia points (and its
            // sector's range): terrain-height <x> <y> [<x> <y>...]
            let root = fc3_formats::cache::default_cache_root();
            for pair in args[1..].chunks(2) {
                let (Some(x), Some(y)) = (pair.first().and_then(|v| v.parse::<f32>().ok()), pair.get(1).and_then(|v| v.parse::<f32>().ok())) else {
                    bail!("usage: terrain-height <x> <y> [<x> <y>...]")
                };
                let c = fc3_formats::terrain::SectorCoord::containing(x, y);
                let bytes =
                    std::fs::read(root.join(fc3_formats::cache::terrain_rel(c.id()))).with_context(|| format!("sector {} isn't in the cache", c.id()))?;
                let sector = fc3_formats::cache::CachedTerrain::from_bytes(&bytes)?.to_sector();
                let o = c.origin();
                println!(
                    "({x}, {y}): terrain {:.2} (sector {}: {:.1}..{:.1})",
                    sector.sample(x - o[0], y - o[1]),
                    c.id(),
                    sector.min_height,
                    sector.max_height
                );
            }
        }
        "ground-check" => {
            // Entity height minus the decoded terrain under it, for entities whose name
            // contains a substring, within r metres of Dunia (x, y):
            // ground-check <substr> <x> <y> <r>
            use fc3_formats::terrain::{parse_sdat, SectorCoord};
            let needle = args.get(1).context("usage: ground-check <substr> <x> <y> <r>")?.to_ascii_lowercase();
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(r)) = (num(2), num(3), num(4)) else { bail!("usage: ground-check <substr> <x> <y> <r>") };
            let c = SectorCoord::containing(x, y);
            let reach = (r / 64.0).ceil() as i32 + 1;
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let s = SectorCoord::new(c.x + dx, c.y + dy);
                    let Ok(bytes) = vfs.read(&s.entities_path("fc3_main")) else { continue };
                    let Ok(ents) = fc3_formats::world::parse_all_entities(&bytes) else { continue };
                    for e in ents.iter().filter(|e| e.name.to_ascii_lowercase().contains(&needle)) {
                        if ((e.pos[0] - x).powi(2) + (e.pos[1] - y).powi(2)).sqrt() > r {
                            continue;
                        }
                        let sc = SectorCoord::containing(e.pos[0], e.pos[1]);
                        let Ok(t) = vfs.read(&sc.sdat_path("fc3_main")).and_then(|b| parse_sdat(&b)) else { continue };
                        let o = sc.origin();
                        let g = t.sample(e.pos[0] - o[0], e.pos[1] - o[1]);
                        println!("{:8.1} {:8.1} {:7.2}  ground {g:7.2}  diff {:6.2}  {}", e.pos[0], e.pos[1], e.pos[2], e.pos[2] - g, e.name);
                    }
                }
            }
        }
        "find-entities" => {
            // Prints entities whose name, class or mesh contains a substring: find-entities <substr> [path filter]
            let needle = args.get(1).context("usage: find-entities <substr> [path filter]")?.to_ascii_lowercase();
            let filter = args.get(2).cloned().unwrap_or_else(|| "worlds\\fc3_main\\".into());
            let mut n = 0;
            for (h, name) in &vfs.names {
                if !name.contains(&filter) || !name.ends_with(".data.fcb") {
                    continue;
                }
                let Ok(bytes) = vfs.read_hash(*h) else { continue };
                let Ok(ents) = fc3_formats::world::parse_all_entities(&bytes) else { continue };
                for e in ents {
                    let hay = format!("{} {} {} id:{}", e.name, e.class, e.meshes.join(" "), e.id).to_ascii_lowercase();
                    if hay.contains(&needle) {
                        n += 1;
                        if n <= 400 {
                            println!("{:9.2} {:9.2} {:8.2}  {:28} {} {:?}", e.pos[0], e.pos[1], e.pos[2], e.class, e.name, e.meshes.first());
                        }
                    }
                }
            }
            println!("{n} matches");
        }
        "entity-dump" => {
            // Full FCB tree of entities whose name contains a substring, or
            // with that decimal id, in world sectors (mission layers too) and
            // generated files: entity-dump <substr|id> [max] [path filter]
            let needle = args.get(1).context("usage: entity-dump <substr|id> [max] [path filter]")?.to_ascii_lowercase();
            let max: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1);
            let filter = args.get(3).cloned().unwrap_or_else(|| "worlds\\fc3_main\\".into());
            let id: Option<u64> = needle.parse().ok();
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            // The engine's own identifiers name most field hashes.
            if let Ok(dll) = std::fs::read(data.parent().unwrap_or(&data).join("bin").join("FC3.dll")) {
                names.add_identifiers_from(&dll);
            }
            let mut n = 0;
            'files: for (h, name) in &vfs.names {
                if !name.contains(&filter) || !name.ends_with(".fcb") {
                    continue;
                }
                let Ok(bytes) = vfs.read_hash(*h) else { continue };
                let start = bytes.windows(4).position(|w| w == b"nbCF").unwrap_or(0);
                let Ok(root) = fc3_formats::fcb::parse(&bytes[start..]) else { continue };
                let mut found = Vec::new();
                for layer in fc3_formats::world::layer_roots(&root) {
                    layer.walk(&mut |o, _| {
                        if o.is("Entity")
                            && (o.str("hidName").is_some_and(|s| s.to_ascii_lowercase().contains(&needle)) || id.is_some() && o.u64("disEntityId") == id)
                        {
                            found.push(o.clone());
                        }
                    });
                }
                for e in found {
                    let mut out = String::new();
                    fc3_formats::fcb::dump(&e, &names, 64, &mut out);
                    println!("== {name}\n{out}");
                    n += 1;
                    if n >= max {
                        break 'files;
                    }
                }
            }
        }
        "override-survey" => {
            // How often do entity material overrides differ from the mesh's own materials?
            let filter = args.get(1).cloned().unwrap_or_else(|| "levels\\c5\\generated\\worldsectors".into());
            let stem = |p: &str| {
                let l = p.to_ascii_lowercase();
                let f = l.rsplit('\\').next().unwrap_or(&l).to_string();
                f.split('.').next().unwrap_or(&f).to_string()
            };
            let mut mesh_mats: std::collections::HashMap<String, Vec<String>> = Default::default();
            let (mut placed, mut with_ovr, mut differs, mut count_mismatch, mut placeholder_eff) = (0, 0, 0, 0, 0);
            let mut examples = Vec::new();
            let mut grey_meshes: BTreeMap<String, usize> = BTreeMap::new();
            for (h, name) in &vfs.names {
                if !name.contains(&filter) || !name.ends_with(".data.fcb") {
                    continue;
                }
                let Ok(sec) = vfs.read_hash(*h).and_then(|b| fc3_formats::world::parse_sector(&b)) else { continue };
                for e in sec.entities {
                    let Some(mesh) = e.meshes.first() else { continue };
                    placed += 1;
                    let own = mesh_mats
                        .entry(mesh.to_ascii_lowercase())
                        .or_insert_with(|| {
                            vfs.read(mesh)
                                .ok()
                                .and_then(|b| fc3_formats::mesh::decode(&b, 1).ok())
                                .map(|m| m.materials.iter().map(|r| stem(&r.path)).collect())
                                .unwrap_or_default()
                        })
                        .clone();
                    let ovr: Vec<String> = e.material_overrides.iter().map(|n| n.to_ascii_lowercase()).collect();
                    if !ovr.is_empty() {
                        with_ovr += 1;
                        if ovr.len() != own.len() {
                            count_mismatch += 1;
                        }
                        if ovr != own {
                            differs += 1;
                            if examples.len() < 8 {
                                examples.push(format!("{} own={:?} ovr={:?}", e.name, own, ovr));
                            }
                        }
                    }
                    let eff = if ovr.is_empty() { &own } else { &ovr };
                    // After the entity's replacement files.
                    let eff: Vec<String> = eff
                        .iter()
                        .map(|m| e.material_replacements.iter().find(|(from, _)| from == m).map(|(_, to)| to.clone()).unwrap_or_else(|| m.clone()))
                        .collect();
                    if eff.iter().any(|m| m == "lpcantin-m-20110323153738" || m == "lpcantin-m-2010032950932047") {
                        placeholder_eff += 1;
                        *grey_meshes.entry(mesh.to_ascii_lowercase()).or_default() += 1;
                    }
                }
            }
            println!(
                "placed {placed}, with overrides {with_ovr}, differing {differs}, count mismatch {count_mismatch}, effective placeholder {placeholder_eff}"
            );
            let mut grey: Vec<_> = grey_meshes.into_iter().collect();
            grey.sort_by(|a, b| b.1.cmp(&a.1));
            for (m, n) in grey.iter().take(25) {
                println!("  grey {n:5} {m}");
            }
            for e in examples {
                println!("  {e}");
            }
        }
        "world-overview" => {
            // Writes a 160x160 grid of per-sector min/max heights as CSV, plus a 2560x2560 PGM heightmap (16 px/sector).
            use fc3_formats::terrain::{parse_sdat, SectorCoord};
            let out = args.get(1).cloned().unwrap_or_else(|| "overview".into());
            let mut csv = String::from("sx,sy,min,max\n");
            let n = 160 * 16;
            let mut img = vec![0u16; n * n];
            for sy in 0..160 {
                for sx in 0..160 {
                    let c = SectorCoord::new(sx, sy);
                    let Ok(b) = vfs.read(&c.sdat_path("fc3_main")) else { continue };
                    let Ok(s) = parse_sdat(&b) else { continue };
                    csv += &format!("{sx},{sy},{},{}\n", s.min_height, s.max_height);
                    for py in 0..16 {
                        for px in 0..16 {
                            let h = s.height(px * 4, py * 4);
                            let gy = n - 1 - (sy as usize * 16 + py);
                            img[gy * n + sx as usize * 16 + px] = (h * 64.0).clamp(0.0, 65535.0) as u16;
                        }
                    }
                }
            }
            std::fs::write(format!("{out}.csv"), csv)?;
            let mut pgm = format!("P5\n{n} {n}\n65535\n").into_bytes();
            for v in img {
                pgm.extend_from_slice(&v.to_be_bytes());
            }
            std::fs::write(format!("{out}.pgm"), pgm)?;
            println!("wrote {out}.csv / {out}.pgm");
        }
        "tex-survey" => {
            let mut fmts: BTreeMap<(String, String), usize> = BTreeMap::new();
            let mut n = 0;
            for (h, name) in &vfs.names {
                if !name.ends_with(".xbt") || name.ends_with("_mip0.xbt") {
                    continue;
                }
                n += 1;
                if n % 7 != 0 {
                    continue;
                } // sample
                let b = vfs.read_hash(*h)?;
                let Ok(x) = fc3_formats::texture::parse(&b) else {
                    *fmts.entry(("bad".into(), "".into())).or_default() += 1;
                    continue;
                };
                let info = fc3_formats::texture::dds_info(x.dds).unwrap();
                let role = format!("{:?}", fc3_formats::material::classify_texture(name));
                let fc = String::from_utf8_lossy(&info.fourcc).to_string();
                let fc = if info.fourcc == [0; 4] { format!("rgb{}bpp", u32::from_le_bytes(x.dds[88..92].try_into().unwrap())) } else { fc };
                *fmts.entry((role, fc)).or_default() += 1;
            }
            for ((r, f), c) in fmts {
                println!("{r:10} {f:10} {c}");
            }
        }
        "archive-magics" => {
            let name = args.get(1).context("usage: archive-magics <archive>")?;
            let mut magics: BTreeMap<String, (usize, u64, String)> = BTreeMap::new();
            for a in vfs.archives.iter().filter(|a| &a.name == name) {
                for e in &a.fat.entries {
                    let Ok(d) = a.read(e) else { continue };
                    let m = magic_of(&d);
                    let entry = magics.entry(m).or_insert((0, 0, String::new()));
                    entry.0 += 1;
                    entry.1 += d.len() as u64;
                    if entry.2.is_empty() {
                        entry.2 = vfs.name_of(e.name_hash).map(str::to_string).unwrap_or(format!("{:016X}", e.name_hash));
                    }
                }
            }
            for (m, (n, bytes, example)) in magics {
                println!("{m:24} {n:7} {:9.1} MB  e.g. {example}", bytes as f64 / 1e6);
            }
        }
        "shader-wgsl" => {
            let target = args.get(1).context("usage: shader-wgsl <path|hash>")?;
            let data = vfs.read_hash(parse_target(&vfs, target)?)?;
            let obj = fc3_formats::d3d9::parse_object(&data)?;
            let t = fc3_formats::d3d9_wgsl::translate(&obj.program, &Default::default())?;
            print!("{}", t.wgsl);
            if let Err(e) = validate_wgsl(&t.wgsl) {
                eprintln!("validation failed: {e}");
            }
        }
        "shader-validate" => {
            use rayon::prelude::*;
            let mut blobs = Vec::new();
            for a in &vfs.archives {
                if a.name != "shadersobj" && a.name != "patch" {
                    continue;
                }
                for e in &a.fat.entries {
                    let Ok(d) = a.read(e) else { continue };
                    if d.len() >= 4 && u32::from_le_bytes(d[..4].try_into().unwrap()) == fc3_formats::d3d9::CONTAINER_MAGIC {
                        blobs.push((e.name_hash, d));
                    }
                }
            }
            let results: Vec<(u64, Result<u32, String>)> = blobs
                .par_iter()
                .map(|(h, d)| {
                    let r = (|| -> Result<u32> {
                        let obj = fc3_formats::d3d9::parse_object(d)?;
                        let t = fc3_formats::d3d9_wgsl::translate(&obj.program, &Default::default())?;
                        validate_wgsl(&t.wgsl)?;
                        Ok(t.nonuniform_samples)
                    })();
                    (*h, r.map_err(|e| format!("{e:#}")))
                })
                .collect();
            let total_bytes: usize = blobs
                .par_iter()
                .filter_map(|(_, d)| {
                    let obj = fc3_formats::d3d9::parse_object(d).ok()?;
                    let t = fc3_formats::d3d9_wgsl::translate(&obj.program, &Default::default()).ok()?;
                    Some(t.wgsl.len())
                })
                .sum();
            println!("total WGSL {:.1} MB, bytecode {:.1} MB", total_bytes as f64 / 1e6, blobs.iter().map(|b| b.1.len()).sum::<usize>() as f64 / 1e6);
            let ok = results.iter().filter(|r| r.1.is_ok()).count();
            let fallback = results.iter().filter(|r| matches!(r.1, Ok(n) if n > 0)).count();
            println!("translated and validated {ok}/{} ({fallback} with non-uniform sampling fallbacks)", results.len());
            let mut errors: BTreeMap<String, (usize, u64)> = BTreeMap::new();
            for (h, r) in &results {
                if let Err(e) = r {
                    let key: String = e.lines().next().unwrap_or("").chars().take(160).collect();
                    let entry = errors.entry(key).or_insert((0, *h));
                    entry.0 += 1;
                }
            }
            let mut errors: Vec<_> = errors.into_iter().collect();
            errors.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
            for (e, (n, h)) in errors.iter().take(25) {
                println!("  {n:6} e.g. {h:016X}: {e}");
            }
        }
        "shader-meta" => {
            // One line per shader: hash stage inputs outputs samplers bindings(used).
            let out = args.get(1).context("usage: shader-meta <out.txt>")?;
            let mut text = String::new();
            for a in &vfs.archives {
                if a.name != "shadersobj" && a.name != "patch" {
                    continue;
                }
                for e in &a.fat.entries {
                    let Ok(d) = a.read(e) else { continue };
                    let Ok(obj) = fc3_formats::d3d9::parse_object(&d) else { continue };
                    let Ok(t) = fc3_formats::d3d9_wgsl::translate(&obj.program, &Default::default()) else { continue };
                    let io = |v: &[fc3_formats::d3d9_wgsl::Io]| v.iter().map(|i| format!("{:?}{}", i.usage, i.usage_index)).collect::<Vec<_>>().join(",");
                    let samplers: Vec<String> = t.samplers.iter().map(|(n, ty)| format!("s{n}:{ty:?}")).collect();
                    let binds: Vec<String> = obj
                        .bindings
                        .iter()
                        .filter(|b| b.rows > 0)
                        .map(|b| format!("{:08x}@{}", b.name_hash, (b.register as u32) * 4 + b.component as u32))
                        .collect();
                    text += &format!(
                        "{:016x} {} {} {} {} {} {}\n",
                        e.name_hash,
                        if obj.program.stage == fc3_formats::d3d9::Stage::Vertex { "vs" } else { "ps" },
                        if t.inputs.is_empty() { "-".into() } else { io(&t.inputs) },
                        if t.outputs.is_empty() { "-".into() } else { io(&t.outputs) },
                        if samplers.is_empty() { "-".into() } else { samplers.join(",") },
                        if binds.is_empty() { "-".into() } else { binds.join(",") },
                        obj.program.instrs.len(),
                    );
                }
            }
            std::fs::write(out, text)?;
        }
        "slot-names" => {
            // Model material slots whose name contains a substring: slot-names <substr>
            let needle = args.get(1).context("usage: slot-names <substr>")?.to_ascii_lowercase();
            let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
            for name in vfs.names.values().filter(|n| n.ends_with(".xbg")) {
                let Ok(refs) = vfs.read(name).and_then(|b| fc3_formats::mesh::read_material_refs(&b)) else { continue };
                for r in refs.iter().filter(|r| r.name.to_ascii_lowercase().contains(&needle)) {
                    *counts.entry((r.name.clone(), r.path.to_ascii_lowercase())).or_default() += 1;
                }
            }
            for ((slot, path), n) in counts {
                println!("{n:5} {slot:32} {path}");
            }
        }
        "tex-params" => {
            // Every texture parameter name hash materials use, with counts.
            let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
            for name in vfs.names.values().filter(|n| n.ends_with(".material.bin")) {
                let Ok(m) = vfs.read(name).and_then(|b| fc3_formats::material::parse(&b)) else { continue };
                for (h, _) in &m.textures {
                    *counts.entry(*h).or_default() += 1;
                }
                // tex-params all: non-texture parameters too.
                if args.get(1).is_some_and(|a| a == "all") {
                    for (h, _) in &m.params {
                        *counts.entry(*h).or_default() += 1;
                    }
                }
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            for (h, n) in v {
                println!("{h:08x} {n}");
            }
        }
        "param-survey" => {
            // How materials set one float4 parameter, by shader: param-survey <hex hash> [shader]
            // (with a shader: a histogram of the mean of its first three values too).
            let hash = u32::from_str_radix(args.get(1).context("usage: param-survey <hex hash> [shader]")?.trim_start_matches("0x"), 16)?;
            let only = args.get(2).cloned();
            let mut histogram = [0usize; 21];
            let mut by_shader: std::collections::BTreeMap<String, (usize, usize, usize)> = Default::default();
            let mut samples: Vec<(String, String, [f32; 4])> = Vec::new();
            for name in vfs.names.values().filter(|n| n.ends_with(".material.bin")) {
                let Ok(m) = vfs.read(name).and_then(|b| fc3_formats::material::parse(&b)) else { continue };
                if only.as_ref().is_some_and(|o| *o != m.shader) {
                    continue;
                }
                let e = by_shader.entry(m.shader.clone()).or_default();
                e.0 += 1;
                let Some((_, raw)) = m.params.iter().find(|(h, _)| *h == hash) else { continue };
                e.1 += 1;
                let f: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                if f.len() >= 3 {
                    histogram[(((f[0] + f[1] + f[2]) / 3.0).clamp(0.0, 2.0) * 10.0) as usize] += 1;
                }
                if f.len() >= 3 && f[..3].iter().any(|v| (v - 1.0).abs() > 0.02) {
                    e.2 += 1;
                    if samples.len() < 30 {
                        samples.push((m.shader.clone(), name.clone(), [f[0], f[1], f[2], f.get(3).copied().unwrap_or(1.0)]));
                    }
                }
            }
            println!("{:24} {:>6} {:>6} {:>6}", "shader", "total", "has", "tinted");
            for (s, (n, has, tinted)) in &by_shader {
                if *has > 0 {
                    println!("{s:24} {n:6} {has:6} {tinted:6}");
                }
            }
            for (s, n, v) in samples {
                println!("  {s:16} {v:.2?} {n}");
            }
            if only.is_some() {
                for (i, n) in histogram.iter().enumerate().filter(|(_, n)| **n > 0) {
                    println!("  mean {:.1}-{:.1}: {n}", i as f32 / 10.0, (i + 1) as f32 / 10.0);
                }
            }
        }
        "material-source" => {
            // The game material behind cached material files: material-source <stem>...
            let stems: Vec<String> = args[1..].iter().map(|s| s.trim_end_matches(".fcmat").to_ascii_lowercase()).collect();
            for name in vfs.names.values().filter(|n| n.ends_with(".material.bin")) {
                let rel = fc3_formats::cache::material_rel(name);
                if stems.iter().any(|s| rel.contains(s.as_str())) {
                    println!("{rel} <- {name}");
                    if let Ok(m) = vfs.read(name).and_then(|b| fc3_formats::material::parse(&b)) {
                        for (h, t) in &m.textures {
                            println!("  tex {h:08x} {t} (in archive: {})", vfs.read(t).is_ok());
                        }
                    }
                }
            }
        }
        "domino-run" => {
            // Runs a Domino graph from the archives with no engine behind it,
            // printing the natives it reaches and the resources it loads:
            // domino-run <script path> [input (In)]
            struct Archives(std::sync::Arc<Vfs>);
            impl fc3_domino::ScriptSource for Archives {
                fn script(&self, path: &str) -> Option<String> {
                    let p: String = path.chars().map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() }).collect();
                    self.0.read(&p).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
                }
            }
            let path = args.get(1).context("usage: domino-run <script path> [input]")?;
            let input = args.get(2).map(String::as_str).unwrap_or("In");
            let source = Archives(vfs.clone());
            let d = fc3_domino::Domino::new(source).map_err(|e| anyhow::anyhow!("{e}"))?;
            let graph = d.create(path).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("created; resources:");
            for (p, k) in d.take_resources() {
                println!("  {k} {p}");
            }
            for c in d.take_unknown_calls() {
                println!("  init: {c}");
            }
            match d.call(&graph, input) {
                Ok(()) => println!("{input} returned"),
                Err(e) => println!("{input} failed: {e}"),
            }
            for c in d.take_unknown_calls() {
                println!("  {c}");
            }
        }
        "story-bytes" => {
            // Finds story entities whose components hold a u32 or u64 value
            // (hex): story-bytes <hex>
            use fc3_formats::story;
            let hex = args.get(1).context("usage: story-bytes <hex>")?;
            let v = u64::from_str_radix(hex.trim_start_matches("0x").trim_start_matches('#'), 16)?;
            let needle: Vec<u8> = if v <= u32::MAX as u64 { (v as u32).to_le_bytes().to_vec() } else { v.to_le_bytes().to_vec() };
            let root = fc3_formats::cache::default_cache_root();
            let entities = story::from_bytes(&std::fs::read(root.join(story::ENTITIES_REL))?)?;
            let names = component_names();
            for e in &entities {
                let mut hits = Vec::new();
                e.components.walk(&mut |o, _| {
                    for (h, b) in &o.fields {
                        if b.windows(needle.len()).any(|w| w == needle.as_slice()) {
                            hits.push(format!("{}.{}", names.get(o.name_hash), names.get(*h)));
                        }
                    }
                });
                if !hits.is_empty() {
                    println!("{} {} [{}] layer {:#x}: {}", e.id, e.name, e.class, e.layer, hits.join(" "));
                }
            }
        }
        "story-run" => {
            // Boots the story headless from the cache and plays it with a
            // stand-in world: cinematics, sounds and message boxes finish on
            // timers, teleports move the player, and when nothing happens for
            // a while the player steps into a trigger a script waits on.
            // story-run [seconds] [console command [parameter]]
            use fc3_story::{Arg, Command, Story, View};
            let root = fc3_formats::cache::default_cache_root();
            let seconds: f32 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10.0);
            let mut story = Story::open(&root)?;
            let data = story.data();
            println!(
                "{} entities, {} blocks, {} layers, {} strings; master on {:?}",
                data.entities.len(),
                data.blocks.len(),
                data.layers.len(),
                data.strings.len(),
                data.root
            );
            if let Ok(p) = std::env::var("FC3_STORY_TRACE") {
                story.domino().trace_calls(&p).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            // FC3_STORY_LOAD=<file>: from a kept checkpoint.
            match std::env::var("FC3_STORY_LOAD").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| fc3_story::save::StorySave::from_text(&t)) {
                Some(save) => story.boot_saved(&save),
                None => story.boot(),
            }
            if let Some(cmd) = args.get(2) {
                let ok = story.console(cmd, args.get(3).map(String::as_str).unwrap_or(""));
                println!("console {cmd}: {}", if ok { "ran" } else { "not registered" });
            }
            #[derive(Debug)]
            enum Due {
                Sequence(String, String, &'static str),
                Sound(u64),
                Answer(u64),
                Group(u64, &'static str, u64),
            }
            let mut due: Vec<(f32, Due)> = Vec::new();
            let mut view = View { time_of_day: 12.0, ..View::default() };
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
            let (mut t, mut quiet, mut shown) = (0.0f32, 0.0f32, 0usize);
            let mut used: std::collections::HashSet<(u64, String)> = Default::default();
            let mut pending = story.take_commands();
            loop {
                for l in story.take_log() {
                    if !l.starts_with("console:") {
                        println!("  [{t:6.1}s] log: {l}");
                    }
                }
                for u in story.take_unknown_calls() {
                    *unknown.entry(u.split('(').next().unwrap_or_default().to_string()).or_default() += 1;
                }
                for c in pending.drain(..) {
                    let text = format!("{c:?}");
                    let kind = match &c {
                        Command::Native { name, .. } => name.clone(),
                        other => format!("{other:?}").split(['(', ' ', '{']).next().unwrap_or_default().to_string(),
                    };
                    *counts.entry(kind).or_default() += 1;
                    if !matches!(c, Command::Layer { .. } | Command::Print(_) | Command::Dialog { .. }) {
                        quiet = 0.0;
                        if shown < 400 {
                            println!("  [{t:6.1}s] {text}");
                            shown += 1;
                        }
                    }
                    // A character gets to its contextual action's spot.
                    if let Command::Teleport { entity, pose } = &c {
                        if *entity == fc3_story::PLAYER {
                            view.player = pose.position;
                            view.player_yaw = pose.angles[2];
                        } else {
                            view.spawned.entry(*entity).or_insert(fc3_story::Live { alive: true, health: 1.0, ..Default::default() }).pos = pose.position;
                        }
                    }
                    if let Command::Action { pawn, action: Some(ca) } = &c {
                        due.push((t + 3.0, Due::Group(*pawn, "CA_Start", *ca)));
                    }
                    if let Command::Native { name, args } = &c {
                        let a = |i: usize| args.get(i).cloned().unwrap_or_default();
                        match name.as_str() {
                            "Sequence" => match a(0).as_str() {
                                "stop" => due.push((t, Due::Sequence(a(1), a(2), "Stopped"))),
                                "skip" => due.push((t, Due::Sequence(a(1), a(2), "Skipped"))),
                                "pause" => {}
                                _ => {
                                    due.push((t, Due::Sequence(a(1), a(2), "Started")));
                                    due.push((t + 3.0, Due::Sequence(a(1), a(2), "Finished")));
                                }
                            },
                            "PlaySound" => due.push((t + 1.0, Due::Sound(a(0).parse().unwrap_or(0)))),
                            "CAPI_Entity.TeleportEntity" => {
                                if let Some(p) = a(1).parse().ok().and_then(|d| data.position(d)) {
                                    view.player = p;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if t >= seconds {
                    break;
                }
                // Due host events.
                due.sort_by(|a, b| a.0.total_cmp(&b.0));
                while due.first().is_some_and(|d| d.0 <= t) {
                    match due.remove(0).1 {
                        Due::Sequence(scene, file, event) => story.sequence_event(&scene, &file, event, None),
                        Due::Sound(id) => story.sound_finished(id),
                        Due::Answer(id) => story.box_answered(id, true),
                        Due::Group(pawn, _event, ca) => {
                            if let Some(p) = data.position(ca) {
                                view.spawned.insert(pawn, fc3_story::Live { pos: p, alive: true, health: 1.0, ai_state: 0 });
                            }
                            story.action_started(pawn);
                        }
                    }
                }
                // Nothing for a while: walk into a trigger a script waits on.
                if quiet > 4.0 && due.is_empty() {
                    let watched = story.watched_triggers();
                    if let Some((id, event, point, _)) = watched.iter().find(|w| w.3 && !used.contains(&(w.0, w.1.clone()))) {
                        println!("  [{t:6.1}s] autopilot: {event} {} ({id})", data.name_of(*id).unwrap_or("?"));
                        used.insert((*id, event.clone()));
                        view.player = *point;
                        if event == "Use" {
                            story.entity_event(*id, "Use", vec![Arg::id(fc3_story::PLAYER)]);
                        }
                        quiet = 0.0;
                    } else if quiet > 30.0 {
                        println!("  [{t:6.1}s] stalled; watched triggers: {watched:?}");
                        if std::env::var("FC3_STORY_WAITING").is_ok() {
                            for w in story.waiting_on() {
                                println!("    waiting on {w}");
                            }
                        }
                        break;
                    }
                }
                // FC3_STORY_RELOAD_AT=<s>: save there and carry on from the
                // save in a fresh story (the reload path).
                if let Some(at) = std::env::var("FC3_STORY_RELOAD_AT").ok().and_then(|v| v.parse::<f32>().ok()) {
                    if t < at && t + 0.1 >= at {
                        let saved = story.save();
                        let text = saved.to_text();
                        println!(
                            "  [{t:6.1}s] reload: {} lines, {} missions, {} layers, {} globals",
                            text.lines().count(),
                            saved.missions.len(),
                            saved.layers.len(),
                            saved.persistent.len()
                        );
                        let back = fc3_story::save::StorySave::from_text(&text).context("save text")?;
                        story = Story::open(&root)?;
                        story.boot_saved(&back);
                        for (name, state) in story.missions().into_iter().filter(|m| m.1 != "Locked") {
                            println!("  [{t:6.1}s] after reload: mission {name}: {state}");
                        }
                    }
                }
                pending = story.update(0.1, view.clone());
                t += 0.1;
                quiet += 0.1;
            }
            println!("commands: {counts:#?}");
            for (name, state) in story.missions().into_iter().filter(|m| m.1 != "Locked") {
                println!("mission {name}: {state}");
            }
            for (name, state) in story.blocks() {
                println!("block {name}: {state}");
            }
            println!("{} layers present, {} script errors", story.present_layers().len(), story.errors());
            let mut u: Vec<(usize, String)> = unknown.into_iter().map(|(k, v)| (v, k)).collect();
            u.sort_by(|a, b| b.cmp(a));
            println!("natives not provided ({}):", u.len());
            for (n, k) in u {
                println!("  {n:6} {k}");
            }
        }
        // Facial clips, face rigs and the definition table: facial <path> [--rig <.dpdx>] [--at <s>]
        "facial" => facial_cmd::run(&vfs, &args)?,
        // Parses every named .dpax and .dpdx: facial-survey [path substring]
        "facial-survey" => facial_cmd::survey(&vfs, &args)?,
        "story-entry" => {
            // Includes generated/global entries omitted by the sector table.
            let needle = args.get(1).context("usage: story-entry <id|name part>")?.to_ascii_lowercase();
            let data = fc3_story::data::StoryData::load(&fc3_formats::cache::default_cache_root())?;
            let names = component_names();
            let mut entries: Vec<_> = data.entities.values().filter(|e|
                e.id.to_string() == needle || e.name.to_ascii_lowercase().contains(&needle)).collect();
            entries.sort_by_key(|e| e.id);
            for e in entries {
                println!("{} {:?} layer {:#x} {} [{}] at {:?} angles {:?}", e.id, e.origin, e.layer, e.name, e.class, e.pos, e.angles);
                let mut text = String::new();
                fc3_formats::fcb::dump(&e.fields, &names, 16, &mut text);
                fc3_formats::fcb::dump(&e.components, &names, 16, &mut text);
                println!("{text}");
                // A shape's points in full (the dump shortens them).
                if let Some(raw) = e.fields.field("hidShapePoints").filter(|r| r.len() >= 4) {
                    let f = |c: &[u8], i: usize| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
                    for (i, c) in raw[4..].chunks_exact(12).enumerate() {
                        println!("  point {i}: ({:.1}, {:.1}, {:.1})", f(c, 0), f(c, 1), f(c, 2));
                    }
                }
            }
        }
        "proto" => {
            // The cached entity library's prototype with a library item id
            // (hex, as shop items' arkArchetype name it): proto <id>
            let id = u64::from_str_radix(args.get(1).context("usage: proto <hex id>")?.trim_start_matches("0x"), 16)?;
            let bytes = std::fs::read(fc3_formats::cache::default_cache_root().join("story/entitylibrary.fcb"))?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            let mut found = None;
            root.walk(&mut |o, _| {
                if found.is_none() && o.is("EntityPrototype") && o.u64("disLibItemId") == Some(id) {
                    found = Some(o.clone());
                }
            });
            let mut out = String::new();
            fc3_formats::fcb::dump(&found.context("no prototype with that id")?, &names, 64, &mut out);
            println!("{out}");
        }
        "material-params" => {
            // A game material's shader, textures and parameters (as floats):
            // material-params <path>
            let path = args.get(1).context("usage: material-params <path>")?;
            let m = fc3_formats::material::parse(&vfs.read(path)?)?;
            println!("{} ({})", m.name, m.shader);
            for (h, t) in &m.textures {
                println!("  tex {h:08x} {t}");
            }
            for (h, raw) in &m.params {
                println!("  {h:08x} {:?}", fc3_formats::original::param_floats(raw));
            }
        }
        "material-named" => {
            // The materials whose own name contains a text: material-named <text>
            let needle = args.get(1).context("usage: material-named <text>")?.to_ascii_lowercase();
            for (h, name) in &vfs.names {
                if !name.ends_with(".material.bin") {
                    continue;
                }
                let Ok(m) = vfs.read_hash(*h).and_then(|b| fc3_formats::material::parse(&b)) else { continue };
                if m.name.to_ascii_lowercase().contains(&needle) {
                    println!("{} {} ({})", m.name, name, m.shader);
                }
            }
        }
        "mesh-colors" => {
            // Each submesh's material and the range of its vertex colours
            // (first LOD): mesh-colors <path>
            let target = args.get(1).context("usage: mesh-colors <path>")?;
            let bytes = vfs.read_hash(parse_target(&vfs, target)?)?;
            let m = fc3_formats::mesh::decode(&bytes, 8)?;
            let Some((_, l)) = m.lods.first() else { bail!("no LODs") };
            // (Skinned models' primitives name their materials in the skin data.)
            let skin = fc3_formats::anim::skin_info_lods(&bytes, 8).ok();
            for s in &l.submeshes {
                let material =
                    skin.as_ref().and_then(|k| k.lods.first()).and_then(|k| k.primitives.get(s.primitive)).map_or(s.material, |p| p.material as usize);
                let name = m.materials.get(material).map(|r| r.name.as_str()).unwrap_or("?");
                let mut lo = [f32::MAX; 4];
                let mut hi = [f32::MIN; 4];
                for &i in &l.indices[s.index_start..s.index_start + s.index_count] {
                    if let Some(c) = l.colors.get(i as usize) {
                        for k in 0..4 {
                            lo[k] = lo[k].min(c[k]);
                            hi[k] = hi[k].max(c[k]);
                        }
                    }
                }
                println!("{name}: rgba min {lo:.2?} max {hi:.2?}");
            }
        }
        "particle-effect" => {
            // An authored particle effect's definition, by name: particle-effect <name>
            let name = args.get(1).context("usage: particle-effect <name>")?;
            let particles = vfs.read(fc3_formats::lights::PARTICLE_LIBRARY)?;
            println!("{}", fc3_formats::particle_effects::effect_xml(std::str::from_utf8(&particles)?, name)?);
        }
        "library-grep" => {
            // The cached entity library's objects with a field containing a
            // text, under their prototype's name: library-grep <text> [depth]
            let needle = args.get(1).context("usage: library-grep <text> [depth]")?.to_ascii_lowercase();
            let depth = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(4);
            let bytes = std::fs::read(fc3_formats::cache::default_cache_root().join("story/entitylibrary.fcb"))?;
            let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let root = fc3_formats::fcb::parse(&bytes[start..])?;
            let mut names = fc3_formats::fcb::NameDb::default();
            for n in KNOWN_NAMES {
                names.add(n);
            }
            if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
                if let Ok(b) = std::fs::read(dll) {
                    names.add_identifiers_from(&b);
                }
            }
            // (`@name` finds a field by its name instead.)
            let field = args[1].strip_prefix('@').map(fc3_formats::hash::name_hash);
            let has = |o: &fc3_formats::fcb::Object| match field {
                Some(h) => o.fields.iter().any(|(k, _)| *k == h),
                None => o.fields.iter().any(|(_, v)| String::from_utf8_lossy(v).to_ascii_lowercase().contains(&needle)),
            };
            let mut protos = Vec::new();
            root.walk(&mut |o, _| {
                if o.is("EntityPrototype") {
                    protos.push(o);
                }
            });
            for proto in protos {
                let name = proto.child("Entity").and_then(|e| e.str("hidName")).unwrap_or_default();
                proto.walk(&mut |o, _| {
                    if has(o) {
                        let mut out = String::new();
                        fc3_formats::fcb::dump(o, &names, depth, &mut out);
                        println!("== {name}\n{out}");
                    }
                });
            }
        }
        "story-markups" => {
            // A contextual action's markups from its staged sequence:
            // story-markups [action id] (none: how many actions have some)
            let data = fc3_story::data::StoryData::load(&fc3_formats::cache::default_cache_root())?;
            match args.get(1).and_then(|a| a.parse::<u64>().ok()) {
                Some(id) => println!("{:?}", data.action_markups.get(&id)),
                None => println!("{} actions with markups", data.action_markups.len()),
            }
        }
        "story-map-markers" => {
            // The map markers by type (`mapobjectivetype`, a hash; the
            // markers' names tell what it is) with their sub-types: how
            // many, how many enabled, a few (all when filtered by a name or
            // a type's text): story-map-markers [filter]
            let data = fc3_story::data::StoryData::load(&fc3_formats::cache::default_cache_root())?;
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let mut by_kind: std::collections::BTreeMap<u64, Vec<&fc3_story::data::Marker>> = Default::default();
            for m in data.markers.values() {
                by_kind.entry(m.kind).or_default().push(m);
            }
            for (kind, list) in by_kind {
                if !format!("{kind:#010x}").contains(&filter) && !list.iter().any(|m| m.name.to_ascii_lowercase().contains(&filter)) {
                    continue;
                }
                let enabled = list.iter().filter(|m| m.enabled).count();
                let subs: std::collections::BTreeSet<u64> = list.iter().map(|m| m.sub_kind).collect();
                println!("{kind:#010x}: {} ({enabled} enabled), sub kinds {subs:?}", list.len());
                for m in list.iter().take(if filter.is_empty() { 4 } else { usize::MAX }) {
                    println!("    {} {} sub {} layer {:#x} at {:.0?} tooltip {} enabled {}", m.id, m.name, m.sub_kind, m.layer, m.pos, m.tooltip, m.enabled);
                }
            }
        }
        "story-layer" => {
            // A mission layer: its block, whether it comes with the block,
            // and the block's other layers: story-layer <id>
            let id: u64 = args.get(1).context("usage: story-layer <id>")?.parse()?;
            let data = fc3_story::data::StoryData::load(&fc3_formats::cache::default_cache_root())?;
            let Some(layer) = data.layers.get(&id) else { anyhow::bail!("no layer {id}") };
            println!("{layer:?}");
            if let Some(block) = data.blocks.get(&layer.block) {
                println!("block {} {} (zone {:?})", block.id, block.name, block.zone);
                for l in &block.layers {
                    println!("  {:?}", data.layers.get(l));
                }
                // The blocks that bring it with them (a mission's shared
                // characters and props).
                for user in data.blocks.values().filter(|b| b.refs.contains(&block.id)) {
                    println!("used by block {} {}", user.id, user.name);
                }
            }
        }
        "story-blocks" => {
            // Mission blocks by name, with their layers and how many of the
            // story's entities each holds: story-blocks <name filter>
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let data = fc3_story::data::StoryData::load(&fc3_formats::cache::default_cache_root())?;
            // `standing`: the base blocks' own layers the story brings in
            // with them (`fc3_story::standing_layers`).
            if filter == "standing" {
                let root = fc3_formats::cache::default_cache_root();
                let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(root.join(fc3_formats::scripts::DOMINO_PACK_REL))?)?;
                let gameplays = std::fs::read(root.join(fc3_formats::gsf::CACHE_REL)).unwrap_or_default();
                let standing = fc3_story::standing_layers(&pack, &data, &gameplays);
                let mut rows: Vec<String> = Vec::new();
                for (b, ls) in &standing {
                    let block = data.blocks.get(b).map_or("?", |b| b.name.as_str());
                    let init = data.active_on_init.contains(b);
                    for l in ls {
                        rows.push(format!("{block} {} (on from the start: {init})", data.layers.get(l).map_or("?", |l| l.name.as_str())));
                    }
                }
                rows.sort();
                for r in rows {
                    println!("{r}");
                }
                return Ok(());
            }
            // `init`: what the manager has active from the start, as blocks
            // or layers.
            if filter == "init" {
                for id in &data.active_on_init {
                    let what = match (data.blocks.get(id), data.layers.get(id)) {
                        (Some(b), _) => format!("block {}", b.name),
                        (_, Some(l)) => format!("layer {} of {}", l.name, data.blocks.get(&l.block).map_or("?", |b| b.name.as_str())),
                        _ => "?".to_string(),
                    };
                    println!("{id:#018x} {what}");
                }
                return Ok(());
            }
            let mut blocks: Vec<_> = data.blocks.values().filter(|b| b.name.to_ascii_lowercase().contains(&filter)).collect();
            blocks.sort_by(|a, b| a.name.cmp(&b.name));
            for block in blocks {
                println!("block {} {} (zone {:?}, quest {})", block.id, block.name, block.zone, block.is_quest);
                for l in &block.layers {
                    let held = data.entities.values().filter(|e| e.layer == *l).count();
                    let name = data.layers.get(l).map_or("?", |l| l.name.as_str());
                    println!("  layer {l} {name}: {held} entities");
                }
            }
        }
        "story-entities" => {
            // Searches the cache's story entity table by name, class, id,
            // layer or component: story-entities [text] (no text: a summary)
            use fc3_formats::story;
            let root = fc3_formats::cache::default_cache_root();
            let entities = story::from_bytes(&std::fs::read(root.join(story::ENTITIES_REL))?)?;
            let names = component_names();
            match args.get(1) {
                None => {
                    let mut by: BTreeMap<String, usize> = BTreeMap::new();
                    for e in &entities {
                        for c in &e.components.children {
                            *by.entry(names.get(c.name_hash)).or_default() += 1;
                        }
                    }
                    let layered = entities.iter().filter(|e| e.layer != fc3_formats::world::DEFAULT_MISSION_LAYER).count();
                    println!("{} entities, {layered} on mission layers; components: {by:#?}", entities.len());
                }
                Some(text) => {
                    let t = text.to_ascii_lowercase();
                    for e in &entities {
                        let comps: Vec<String> = e.components.children.iter().map(|c| names.get(c.name_hash)).collect();
                        let hit = e.name.to_ascii_lowercase().contains(&t)
                            || e.class.to_ascii_lowercase().contains(&t)
                            || e.id.to_string() == t
                            || format!("{:x}", e.layer) == t
                            || comps.iter().any(|c| c.to_ascii_lowercase() == t);
                        if hit {
                            println!("{} {:?} layer {:#x} {} [{}] at {:?}: {}", e.id, e.source, e.layer, e.name, e.class, e.pos, comps.join(" "));
                            if args.get(2).map(String::as_str) == Some("dump") {
                                let mut s = String::new();
                                fc3_formats::fcb::dump(&e.fields, &names, 16, &mut s);
                                fc3_formats::fcb::dump(&e.components, &names, 16, &mut s);
                                println!("{s}");
                                if let Some(raw) = e.fields.field("hidShapePoints") {
                                    let f = |c: &[u8], i: usize| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
                                    for c in raw.get(4..).unwrap_or_default().chunks_exact(12) {
                                        println!("  point {:9.2} {:9.2} {:7.2}", f(c, 0), f(c, 1), f(c, 2));
                                    }
                                }
                            }
                        }
                    }
                    // A spawn point (a respawn, a teleport's destination) by its id.
                    if let Ok(id) = t.parse::<u64>() {
                        let data = fc3_story::data::StoryData::load(&root)?;
                        if let Some(sp) = data.spawn_points.get(&id) {
                            println!("{id} spawn point {} layer {:#x} at {:?} angles {:?}", sp.name, sp.layer, sp.pos, sp.angles);
                        }
                    }
                }
            }
        }
        "ca-moves" => {
            // How the story's contextual actions' moves find clips:
            // ca-moves [show (the unmatched) | write (the cache's table)]
            use fc3_formats::{moves, story};
            let root = fc3_formats::cache::default_cache_root();
            let entities = story::from_bytes(&std::fs::read(root.join(story::ENTITIES_REL))?)?;
            let mut names: BTreeSet<String> = BTreeSet::new();
            let mut actions = 0;
            for e in &entities {
                let Some(ca) = e.components.children.iter().find(|c| c.is("CAIContextualActionComponent")) else { continue };
                actions += 1;
                if let Some(p) = ca.children.iter().find(|c| c.is("Parameters")) {
                    names.extend(moves::move_names(p));
                }
            }
            let clips: Vec<&str> = vfs.names.values().filter(|n| n.ends_with(".mab")).map(String::as_str).collect();
            let db = moves::parse_move_db(&vfs.read(moves::MOVE_DB)?);
            let table = moves::ca_clips(names.iter().map(String::as_str), &db, &clips);
            println!("{} of the moves in the move database", names.iter().filter(|n| db.contains_key(*n)).count());
            println!("{actions} actions, {} moves, {} with clips ({} clips listed)", names.len(), table.len(), clips.len());
            if args.get(1).map(String::as_str) == Some("write") {
                std::fs::write(root.join(moves::CA_MOVES_REL), moves::to_text(&table))?;
                println!("written to {}", root.join(moves::CA_MOVES_REL).display());
            }
            if args.get(1).map(String::as_str) == Some("show") {
                for n in names.iter().filter(|n| !table.contains_key(*n)) {
                    println!("  no clip: {n}");
                }
            }
        }
        "qte-moves" => {
            // The QTE moves (state trees) and their clips' QTE events:
            // qte-moves [write | <move>...]; write puts the table in the cache.
            use fc3_formats::qte;
            let t = qte::QteTable::build(&vfs.read(fc3_formats::moves::MOVE_DB)?, |c| vfs.read(c).ok());
            println!("{} QTE moves, {} clips", t.moves.len(), t.clips.len());
            if args.get(1).map(String::as_str) == Some("write") {
                let root = fc3_formats::cache::default_cache_root();
                std::fs::write(root.join(qte::QTE_REL), t.to_text())?;
                println!("written to {}", root.join(qte::QTE_REL).display());
                return Ok(());
            }
            for name in &args[1..] {
                let Some(states) = t.moves.get(&name.to_ascii_lowercase()) else {
                    println!("{name}: not a QTE move");
                    continue;
                };
                println!("{name}");
                for s in states {
                    let conds: Vec<String> = s.conds.iter().map(|(i, v)| format!("QTE{i}={v}")).collect();
                    let clip = s.clip.as_deref().map(|c| c.rsplit('\\').next().unwrap_or(c)).unwrap_or("-");
                    println!("{}{} [{}] {clip}{}", "  ".repeat(s.depth as usize), s.name, conds.join(" "), if s.anchored { " (anchored)" } else { "" });
                    for e in s.clip.as_ref().and_then(|c| t.clips.get(c)).into_iter().flatten() {
                        println!(
                            "{}    {:.3} {:?} QTE{} {} prompt {:?} {:?} {}",
                            "  ".repeat(s.depth as usize),
                            e.time,
                            e.kind,
                            e.id,
                            qte::input_name(e.input),
                            e.prompt,
                            e.values,
                            e.text
                        );
                    }
                }
            }
        }
        "vine-settings" => {
            let bytes = vfs.read(fc3_formats::gsf::LIBRARY)?;
            println!("{:#?}", fc3_formats::gsf::VineSettings::parse(&bytes)?);
        }
        "feu-swf" => {
            // An interface movie as a standard SWF, its bitmaps embedded from
            // the game's textures and its fonts traced from the game's font
            // files: feu-swf <archive .feu> <out.swf>
            use fc3_formats::{feu, ffd, texture};
            let path = args.get(1).context("usage: feu-swf <archive .feu> <out.swf>")?;
            let out = args.get(2).context("usage: feu-swf <archive .feu> <out.swf>")?;
            let feu = vfs.read(path)?;
            let pixels = |p: &str| vfs.read(p).ok().and_then(|x| texture::parse(&x).ok().and_then(|t| texture::decode_rgba(t.dds)));
            let (mut found, mut missing) = (0, Vec::new());
            let swf = feu::to_swf(
                &feu,
                |img| {
                    let px = pixels(&img.texture());
                    if px.is_some() { found += 1 } else { missing.push(img.texture()) }
                    px
                },
                |f| {
                    let font = ffd::parse(&vfs.read(&f.file.to_ascii_lowercase()).ok()?).ok()?;
                    let pages: Vec<_> =
                        (0..font.pages.len()).map(|i| font.page_texture(i).and_then(|t| pixels(&t)).map(|(w, h, px)| ffd::Page::from_rgba(w, h, &px))).collect();
                    match ffd::TracedFont::trace(&font, &pages) {
                        Ok(t) => {
                            let body = t.define_font3(f.id, &f.face);
                            println!("font {} {} ({}): {} glyphs, {} bytes", f.id, f.face, f.file, t.glyphs.len(), body.len());
                            Some(body)
                        }
                        Err(e) => {
                            println!("font {} ({}): {e:#}", f.id, f.file);
                            None
                        }
                    }
                },
                |url| {
                    let path = url.split(['\\', '/']).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\\");
                    vfs.read(&path).ok().and_then(|b| feu::exports(&b).ok()).unwrap_or_default()
                },
            )?;
            std::fs::write(out, &swf)?;
            println!("{} bytes, {found} images embedded, {} missing {:?}", swf.len(), missing.len(), missing.iter().take(5).collect::<Vec<_>>());
        }
        "clip-hands" => {
            // Where a first-person clip puts the hands, seen from its camera
            // bone (x right, y up, -z ahead): clip-hands <archive clip> [t]
            use fc3_formats::anim;
            let path = args.get(1).context("usage: clip-hands <archive clip> [t]")?;
            let clip = anim::parse_clip(&vfs.read(path)?)?;
            let skel = anim::parse_skeleton(&vfs.read(r"graphics\actors_\_skeleton_master\hips_ref.skeleton")?)?;
            let t: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let nodes: Vec<anim::Node> = skel
                .bones
                .iter()
                .enumerate()
                .map(|(i, b)| anim::Node {
                    name: b.name.clone(),
                    name_hash: b.name_hash,
                    parent: b.parent,
                    first_child: -1,
                    next_sibling: -1,
                    translation: b.translation,
                    rotation: b.rotation,
                    scale: [1.0; 3],
                    bind_index: i as i32,
                })
                .collect();
            let mut locals = anim::pose_locals(&nodes, &skel, &clip, t.clamp(0.0, clip.duration));
            // Over a base pose (an additive clip): base rotation times the
            // clip's, at the base's place.
            if let Some(base) = args.get(3) {
                let base = anim::parse_clip(&vfs.read(base)?)?;
                let under = anim::pose_locals(&nodes, &skel, &base, 0.0);
                for (l, b) in locals.iter_mut().zip(&under) {
                    let mut m = anim::mat_mul(b, l);
                    m[12..15].copy_from_slice(&b[12..15]);
                    *l = m;
                }
            }
            let ident = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
            let worlds = anim::compose_worlds(&nodes, &locals, &ident);
            let at = |name: &str| skel.find(name).map(|i| worlds[i]);
            let cam = at("Camera").context("no Camera bone")?;
            // Into the camera's frame (rigid: rotation transposed).
            let local = |m: [f32; 16]| {
                let d = [m[12] - cam[12], m[13] - cam[13], m[14] - cam[14]];
                [0, 1, 2].map(|k| d[0] * cam[k * 4] + d[1] * cam[k * 4 + 1] + d[2] * cam[k * 4 + 2])
            };
            println!("camera at {:.2?}, its axes x {:.2?} y {:.2?} z {:.2?}", &cam[12..15], &cam[0..3], &cam[4..7], &cam[8..11]);
            for b in ["RightHand", "LeftHand", "Head"] {
                if let Some(m) = at(b) {
                    println!("{b:10} in camera space {:.2?}", local(m));
                }
            }
            for b in ["Reference", "Hips", "Spine", "Spine1", "Spine2", "Neck", "Head", "Camera"] {
                if let Some(m) = at(b) {
                    println!("{b:10} world {:.2?} x {:.2?} y {:.2?}", &m[12..15], &m[0..3], &m[4..7]);
                }
            }
            let parent = skel.find("Camera").and_then(|i| usize::try_from(skel.bones[i].parent).ok()).map(|p| skel.bones[p].name.clone());
            let keyed = skel.find("Camera").is_some_and(|i| clip.tracks.iter().any(|t| t.bone as usize == i));
            println!("camera's parent {parent:?}, keyed {keyed}");
            let mut keyed_names: Vec<String> = clip.tracks.iter().filter_map(|t| skel.bones.get(t.bone as usize).map(|b| b.name.clone())).collect();
            keyed_names.sort();
            println!("keyed: {}", keyed_names.join(" "));
        }
        "clip-info" => {
            let path = args.get(1).context("usage: clip-info <archive animation path>")?;
            let clip = fc3_formats::anim::parse_clip(&vfs.read(path)?)?;
            println!("{path}: {:.3}s, {:.1} fps, skeleton {:#x}, {} tracks, {} subclips", clip.duration, clip.frame_rate, clip.skeleton_id, clip.tracks.len(), clip.sub_clips.len());
            if let Some(root) = &clip.root_motion {
                println!("root: start {:?} rotation {:?}, {} keys, end {:?} rotation {:?}", root.start_translation, root.start_rotation,
                    root.times.len(), root.translations.last(), root.rotations.last());
            }
            for event in &clip.sounds { println!("sound {:.3}s {:#x}", event.0, event.1); }
            for attachment in &clip.attachments { println!("attachment {attachment:?}"); }
            for sub in &clip.sub_clips { println!("sub-clip: skeleton {:#x}, {} tracks", sub.skeleton_id, sub.tracks.len()); }
        }
        "moves" => {
            // The engine's named moves, their states and clips: moves [name
            // part]
            let data = vfs.read(fc3_formats::moves::MOVE_DB)?;
            let moves = fc3_formats::moves::parse_move_db(&data);
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            println!("{} moves, {} with clips", moves.len(), moves.values().filter(|v| !v.is_empty()).count());
            for (m, states) in moves.iter().filter(|(m, _)| m.contains(&filter)).take(40) {
                println!("{m}");
                for (s, c) in states {
                    println!("    {s}: {c}");
                }
            }
        }
        "oasis" => {
            // Strings by id: oasis <id>...; or by text: oasis find <text>...;
            // or those containing a text: oasis grep <text>...; or by symbolic
            // name: oasis named <section> <name>...
            let root = fc3_formats::cache::default_cache_root();
            let strings = fc3_formats::oasis::Strings::from_bytes(&std::fs::read(root.join("story/strings.bin"))?)?;
            if args.get(1).map(String::as_str) == Some("find") {
                for text in &args[2..] {
                    println!("{text:?}: {:?}", strings.find(text));
                }
                return Ok(());
            }
            if args.get(1).map(String::as_str) == Some("named") {
                // By section and symbolic name: oasis named <section> <name>...
                let section = args.get(2).context("usage: oasis named <section> <name>...")?;
                for name in &args[3..] {
                    println!("{section}/{name}: {:?} {:?}", strings.id_of(section, name), strings.named(section, name));
                }
                return Ok(());
            }
            if args.get(1).map(String::as_str) == Some("grep") {
                for text in &args[2..] {
                    for id in strings.containing(text) {
                        println!("{id}: {:?}", strings.get(id).unwrap_or(""));
                    }
                }
                return Ok(());
            }
            for id in &args[1..] {
                println!("{id}: {:?}", id.parse().ok().and_then(|i| strings.get(i)));
            }
        }
        "script-grep" => {
            // The script pack's lines containing a text, with their graph:
            // script-grep <text> [path filter]
            let needle = args.get(1).context("usage: script-grep <text> [path filter]")?;
            let filter = args.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let root = fc3_formats::cache::default_cache_root();
            let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(root.join(fc3_formats::scripts::DOMINO_PACK_REL))?)?;
            let mut paths: Vec<String> = pack.paths().filter(|p| p.contains(&filter)).map(str::to_string).collect();
            paths.sort();
            for p in paths {
                let Some(text) = pack.get(&p) else { continue };
                for (i, line) in text.lines().enumerate() {
                    if line.contains(needle.as_str()) {
                        println!("{p}:{}: {}", i + 1, line.trim());
                    }
                }
            }
        }
        "preload" => {
            // A sector's preload list, its resource ids named where the
            // archive knows them, per layer: preload <level> <sector id> [substr]
            let (Some(level), Some(id)) = (args.get(1), args.get(2)) else { bail!("usage: preload <level> <sector id> [substr]") };
            let filter = args.get(3).map(|s| s.to_ascii_lowercase());
            let path = format!(r"worlds\fc3_main\levels\{level}\generated\preload\sector{id}.preload.fcb");
            let root = fc3_formats::fcb::parse(&vfs.read(&path)?)?;
            fn walk(o: &fc3_formats::fcb::Object, layer: u64, out: &mut Vec<(u64, u64)>) {
                let layer = o.u64("LayerId").unwrap_or(layer);
                if let Some(b) = o.field("ResIds") {
                    if b.len() >= 4 {
                        let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
                        for i in 0..n {
                            let at = 4 + i * 8;
                            if let Some(w) = b.get(at..at + 8) {
                                out.push((layer, u64::from_le_bytes(w.try_into().unwrap())));
                            }
                        }
                    }
                }
                for c in &o.children {
                    walk(c, layer, out);
                }
            }
            let mut ids = Vec::new();
            walk(&root, 0, &mut ids);
            for (layer, h) in ids {
                let name = vfs.name_of(h).map(str::to_string).unwrap_or(format!("{h:016x}"));
                if filter.as_ref().is_none_or(|f| name.to_ascii_lowercase().contains(f.as_str())) {
                    println!("{layer:x} {name}");
                }
            }
        }
        "nav-near" => {
            // Navmesh polygons of a sector with a vertex within r metres of
            // Dunia (x, y), Dunia space: nav-near <level> <sector id> <x> <y> <r>
            let (Some(level), Some(id)) = (args.get(1), args.get(2)) else { bail!("usage: nav-near <level> <sector id> <x> <y> <r>") };
            let num = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
            let (Some(x), Some(y), Some(r)) = (num(3), num(4), num(5)) else { bail!("usage: nav-near <level> <sector id> <x> <y> <r>") };
            let path = format!(r"worlds\fc3_main\levels\{level}\generated\nv\sector{id}.rnv");
            for t in fc3_formats::nav::parse_rnv(&vfs.read(&path)?) {
                for p in &t.polys {
                    let vs: Vec<[f32; 3]> = p.verts.iter().map(|&i| t.verts[i as usize]).map(|v| [v[0], v[2], v[1]]).collect();
                    if vs.iter().any(|v| ((v[0] - x).powi(2) + (v[1] - y).powi(2)).sqrt() <= r) {
                        let text: Vec<String> = vs.iter().map(|v| format!("({:.1}, {:.1}, {:.1})", v[0], v[1], v[2])).collect();
                        println!("tile {} {} layer {}: {}", t.x, t.y, t.layer, text.join(" "));
                    }
                }
            }
        }
        "prefab-dump" => {
            // An encounter prefab's entities (prefab space): prefab-dump <id, decimal or 0x>
            let raw = args.get(1).context("usage: prefab-dump <id>")?;
            let id = match raw.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16)?,
                None => raw.parse()?,
            };
            let bytes = vfs.read(&fc3_formats::encounters::prefab_path(id))?;
            let unpacked = fc3_formats::encounters::unpack(&bytes).context("not a packed prefab")?;
            for e in fc3_formats::world::parse_all_entities(&unpacked)? {
                let mesh = e.meshes.first().map_or(String::new(), |m| format!("  {m}"));
                println!("{:28} {} at ({:.1}, {:.1}, {:.1}){mesh}", e.class, e.name, e.pos[0], e.pos[1], e.pos[2]);
            }
        }
        "scene-survey" => {
            // Every encounter prefab's synced scenes: its smart terrain
            // points naming sequences, its scene entities, and each loop
            // sequence's parts (track name, tag, clip): scene-survey [name filter]
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let mut ids: Vec<u64> = vfs.names.values().filter_map(|n| n.strip_prefix(r"generated\encounters\")?.strip_suffix(".fcb")?.parse().ok()).collect();
            ids.sort();
            ids.dedup();
            let mut seen = std::collections::BTreeSet::new();
            for id in ids {
                let Ok(bytes) = vfs.read(&fc3_formats::encounters::prefab_path(id)) else { continue };
                let Ok(root) = fc3_formats::encounters::parse(&bytes) else { continue };
                let mut lines = Vec::new();
                let mut loops = Vec::new();
                for e in root.children.iter().filter(|c| c.is("Entity")) {
                    let name = e.str("hidName").unwrap_or_default();
                    let at = e.vec3("hidPos").unwrap_or_default();
                    let yaw = e.vec3("hidAngles").map_or(0.0, |a| a[2]);
                    let mut seqs = Vec::new();
                    e.walk(&mut |o, _| {
                        for (_, v) in &o.fields {
                            if v.last() == Some(&0) && v.len() > 5 {
                                let s = fc3_formats::fcb::bytes_to_string(v);
                                if s.to_ascii_lowercase().ends_with(".cseq") && !seqs.contains(&s) {
                                    seqs.push(s);
                                }
                            }
                        }
                    });
                    let lower = name.to_ascii_lowercase();
                    if seqs.is_empty() && !lower.starts_with("corpses/") && !lower.contains("civilian") && !lower.contains("hostage") {
                        continue;
                    }
                    lines.push(format!("  {name} at [{:.2}, {:.2}, {:.2}] yaw {yaw:.1}", at[0], at[1], at[2]));
                    for s in &seqs {
                        lines.push(format!("      {s}"));
                        if s.to_ascii_lowercase().contains("loop") && !loops.contains(s) {
                            loops.push(s.clone());
                        }
                    }
                }
                if loops.is_empty() || (!filter.is_empty() && !lines.iter().any(|l| l.to_ascii_lowercase().contains(&filter))) {
                    continue;
                }
                println!("{id}");
                for l in lines {
                    println!("{l}");
                }
                for s in loops.iter().filter(|s| seen.insert(s.to_ascii_lowercase())) {
                    let Ok(b) = vfs.read(s) else {
                        println!("  ! {s} missing");
                        continue;
                    };
                    let text = String::from_utf8_lossy(&b);
                    println!("  = {s}");
                    let Some(x) = fc3_formats::xml::parse(&text) else { continue };
                    x.walk(&mut |g| {
                        if g.name == "CCharacterTrackGroup" {
                            let mut clip = None;
                            g.walk(&mut |c| {
                                if clip.is_none() && c.name == "CAnimationClip" {
                                    clip = c.attr("sAnimationSource").map(|p| p.rsplit('\\').next().unwrap_or(p).to_string());
                                }
                            });
                            println!(
                                "      {} tag {:?}: {}",
                                g.attr("sName").unwrap_or_default(),
                                g.attr("sTag").unwrap_or_default(),
                                clip.unwrap_or_default()
                            );
                        }
                    });
                }
            }
        }
        "civ-points" => {
            // Every encounter prefab's single-actor points (SimpleAnimation)
            // whose group filter names one of its civilians: the point, its
            // moves, and who.
            let mut ids: Vec<u64> = vfs.names.values().filter_map(|n| n.strip_prefix(r"generated\encounters\")?.strip_suffix(".fcb")?.parse().ok()).collect();
            ids.sort();
            ids.dedup();
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            for id in ids {
                let Ok(bytes) = vfs.read(&fc3_formats::encounters::prefab_path(id)) else { continue };
                let Ok(root) = fc3_formats::encounters::parse(&bytes) else { continue };
                let entities: Vec<_> = root.children.iter().filter(|c| c.is("Entity")).collect();
                let groups = |e: &fc3_formats::fcb::Object| -> Vec<u32> {
                    e.child("Components")
                        .and_then(|c| c.child("CGroupComponent"))
                        .and_then(|c| c.child("Groups"))
                        .map(|g| g.children_named("group").filter_map(|o| o.u32("group")).collect())
                        .unwrap_or_default()
                };
                for e in &entities {
                    let Some(name) = e.str("hidName") else { continue };
                    if !name.to_ascii_lowercase().contains("simpleanimation") {
                        continue;
                    }
                    let Some(c) = e.child("Components").and_then(|c| c.child("CAIContextualActionComponent")) else { continue };
                    let Some(filter) = c.u32("GroupFilter").filter(|h| *h != 0 && *h != u32::MAX) else { continue };
                    let who: Vec<String> = entities
                        .iter()
                        .filter(|p| p.str("hidName").is_some_and(|n| n.starts_with("Outsiders/")) && groups(p).contains(&filter))
                        .filter_map(|p| p.str("hidName"))
                        .collect();
                    if who.is_empty() {
                        continue;
                    }
                    let moves = c.child("Parameters").map(|p| fc3_formats::moves::move_names(p)).unwrap_or_default();
                    println!("{id} {name} -> {who:?}: {moves:?}");
                    *kinds
                        .entry(name.split('.').last().unwrap_or_default().trim_end_matches(|c: char| c.is_ascii_digit() || c == '_').to_string())
                        .or_default() += 1;
                }
            }
            println!("{kinds:#?}");
        }
        "domino-survey" => {
            // Creates every single-player graph from the cache's script pack
            // (resources, boxes, init), reporting failures and the natives
            // reached: domino-survey [path filter]
            struct Pack(fc3_formats::scripts::ScriptPack);
            impl fc3_domino::ScriptSource for Pack {
                fn script(&self, path: &str) -> Option<String> {
                    self.0.get(path)
                }
            }
            let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "domino/user/fc3/".into());
            let root = fc3_formats::cache::default_cache_root();
            let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(root.join(fc3_formats::scripts::DOMINO_PACK_REL))?)?;
            let mut graphs: Vec<String> =
                pack.paths().filter(|p| p.starts_with(&filter) && !p.ends_with(".debug.lua") && !p.ends_with(".globals.lua")).map(str::to_string).collect();
            graphs.sort();
            let d = fc3_domino::Domino::new(Pack(pack)).map_err(|e| anyhow::anyhow!("{e}"))?;
            let (mut ok, mut failed) = (0, BTreeMap::<String, usize>::new());
            let mut natives: BTreeMap<String, usize> = BTreeMap::new();
            for g in &graphs {
                match d.create(g) {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        let first = e.to_string().lines().next().unwrap_or_default().to_string();
                        if failed.len() < 400 {
                            println!("{g}: {first}");
                        }
                        *failed.entry(first.split(':').skip(2).collect::<Vec<_>>().join(":")).or_default() += 1;
                    }
                }
                for c in d.take_unknown_calls() {
                    *natives.entry(c.split('(').next().unwrap_or_default().to_string()).or_default() += 1;
                }
                d.take_resources();
            }
            println!("{ok} of {} graphs created", graphs.len());
            println!("failures by message: {failed:#?}");
            println!("natives reached while creating: {natives:#?}");
        }
        "drawn-with" => {
            // Placements across the cached world whose LOD-0 parts draw with a
            // material once per-placement material sets apply, by model:
            // drawn-with <material stem>
            use fc3_formats::cache::{CachedModel, CachedSector, NO_MATERIAL_SET};
            let stem = args.get(1).context("usage: drawn-with <material stem> [all]")?.to_ascii_lowercase();
            let all = args.get(2).is_some_and(|a| a == "all");
            let root = fc3_formats::cache::default_cache_root();
            let mut models: HashMap<String, Option<CachedModel>> = HashMap::new();
            let mut by_model: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            let (mut overridden, mut sectors) = (0usize, 0usize);
            for entry in std::fs::read_dir(root.join("sectors"))? {
                let path = entry?.path();
                let Ok(sector) = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Ok(CachedSector::from_bytes(&b)?)) else { continue };
                let mut hit = false;
                for p in &sector.placements {
                    let Some(rel) = sector.models.get(p.model as usize) else { continue };
                    let model = models.entry(rel.clone()).or_insert_with(|| std::fs::read(root.join(rel)).ok().and_then(|b| CachedModel::from_bytes(&b).ok()));
                    let Some(model) = model.as_ref() else { continue };
                    let Some(lod) = model.lods.first() else { continue };
                    let set = (p.material_set != NO_MATERIAL_SET).then(|| sector.material_sets.get(p.material_set as usize)).flatten();
                    let mut verts = 0;
                    for part in &lod.parts {
                        let own = model.materials.get(part.material as usize).map(String::as_str).unwrap_or("");
                        let effective = set.and_then(|s| s.get(part.material as usize)).map(String::as_str).unwrap_or(own);
                        if effective.contains(stem.as_str()) {
                            verts += part.positions.len();
                        } else if own.contains(stem.as_str()) {
                            overridden += 1;
                        }
                    }
                    if verts > 0 {
                        let e = by_model.entry(rel.clone()).or_default();
                        if e.0 == 0 || all {
                            // Game coordinates (Z up), for --pos.
                            let [x, y, z] = p.translation;
                            let test = if fc3_formats::worldmap::in_test_area(x, -z) { " (developers' test area)" } else { "" };
                            println!("{rel} at {x:.1},{:.1},{y:.1}{test}", -z);
                        }
                        e.0 += 1;
                        e.1 = verts;
                        hit = true;
                    }
                }
                sectors += usize::from(hit);
            }
            for (rel, (n, verts)) in &by_model {
                println!("{n:5} placements, {verts:6} verts on it: {rel}");
            }
            println!("{} models in {sectors} sectors; {overridden} parts of it replaced by a placement's own material", by_model.len());
        }
        "flicker" => {
            // The engine's flicker wave as the converter reads it out of FC3.dll.
            let path = fc3_formats::engine::dll_path(&data);
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let pe = fc3_formats::engine::Pe::parse(&bytes).context("not a 32-bit PE")?;
            match fc3_formats::engine::flicker_wave(&pe) {
                Some(w) => print!("{}", w.to_text()),
                None => println!("flicker routine not found"),
            }
        }
        "effect-lights" => {
            // The particle library's looping effect lights: effect-lights [name substring]
            let needle = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let lights = fc3_formats::lights::effect_lights(&String::from_utf8_lossy(&vfs.read(fc3_formats::lights::PARTICLE_LIBRARY)?));
            let mut names: Vec<_> = lights.keys().filter(|k| k.contains(&needle)).collect();
            names.sort();
            for n in &names {
                for l in &lights[*n] {
                    println!("{n}: {l:?}");
                }
            }
            println!("{} effects with lights ({} shown)", lights.len(), names.len());
        }
        "placeholder-survey" => {
            // Placed entities whose mesh keeps a grey/checker placeholder
            // material in some slot, by whether the entity overrides any
            // material (if not, the game draws its debug checker there).
            let mut models: HashMap<String, bool> = HashMap::new();
            let mut stats: BTreeMap<String, usize> = BTreeMap::new();
            for name in vfs.names.values().filter(|n| n.contains(r"worlds\fc3_main\") && n.ends_with(".data.fcb")) {
                let Ok(bytes) = vfs.read(name) else { continue };
                let Ok(sector) = fc3_formats::world::parse_sector(&bytes) else { continue };
                for e in &sector.entities {
                    let Some(mesh) = e.meshes.first() else { continue };
                    let mesh = mesh.to_ascii_lowercase();
                    let has = *models.entry(mesh.clone()).or_insert_with(|| {
                        vfs.read(&mesh).ok().and_then(|b| fc3_formats::mesh::read_material_refs(&b).ok()).is_some_and(|refs| {
                            refs.iter().any(|r| {
                                let p = r.path.to_ascii_lowercase();
                                p.contains("lpcantin-m-20110323153738") || p.contains("lpcantin-m-2010032950932047")
                            })
                        })
                    });
                    if !has {
                        continue;
                    }
                    let overridden = !e.material_overrides.is_empty() || !e.material_replacements.is_empty();
                    let test = fc3_formats::worldmap::in_test_area(e.pos[0], e.pos[1]);
                    let key = format!("{} {}", if overridden { "overridden" } else { "NOT overridden" }, if test { "(test area)" } else { "" });
                    *stats.entry(key.clone()).or_default() += 1;
                    if !overridden && !test && stats[&key] <= 12 {
                        println!("{} at {:.0},{:.0},{:.0}: {mesh}", e.name, e.pos[0], e.pos[1], e.pos[2]);
                    }
                }
            }
            println!("{stats:#?}");
        }
        "model-source" => {
            // The game models behind cached model files: model-source <stem>...
            let stems: Vec<String> = args[1..].iter().map(|s| s.trim_end_matches(".fcm").to_ascii_lowercase()).collect();
            for name in vfs.names.values() {
                let rel = fc3_formats::cache::model_rel(name);
                if stems.iter().any(|s| rel.contains(s.as_str())) {
                    println!("{rel} <- {name}");
                }
            }
        }
        "material-params" => {
            let target = args.get(1).context("usage: material-params <path>")?;
            let m = fc3_formats::material::parse(&vfs.read_hash(parse_target(&vfs, target)?)?)?;
            println!("shader {}", m.shader);
            for (h, t) in &m.textures {
                println!("tex {h:08x} {t}");
            }
            for (h, v) in &m.params {
                let hex: String = v.iter().map(|b| format!("{b:02x}")).collect();
                println!("par {h:08x} {} {hex}", v.len());
            }
        }
        "gloss-survey" => {
            // What each paired material writes to targets 1 (alpha) and 2, by
            // shader family: the deferred lighting reads target 1's alpha as
            // specular intensity and target 2's alpha (x256) as its exponent.
            use fc3_formats::cache::{CachedMaterial, ShaderEntry};
            use fc3_formats::original::{self, MaterialSample, ShaderSet};
            let root = fc3_formats::cache::default_cache_root();
            let index = std::fs::read_to_string(root.join(fc3_formats::cache::SHADER_INDEX_REL))?;
            let mut set = ShaderSet::new(index.lines().filter(|l| !l.starts_with('#')).filter_map(ShaderEntry::from_line).collect());
            for name in vfs.names.values().filter(|n| n.starts_with(original::DESCRIPTOR_DIR) && n.ends_with(".xml")) {
                if let Ok(bytes) = vfs.read(name) {
                    set.add_descriptor(&String::from_utf8_lossy(&bytes));
                }
            }
            let mut objects: HashMap<u64, Option<fc3_formats::d3d9::ShaderObject>> = HashMap::new();
            // family -> (count, sum spec, sum power, [spec bins], [power bins], unit-vector count in target 2)
            let mut fams: BTreeMap<String, (usize, f32, f32, [usize; 5], [usize; 6], usize)> = BTreeMap::new();
            let show = std::env::var("FC3_SURVEY_FAMILY").ok();
            for e in std::fs::read_dir(root.join("materials"))? {
                let path = e?.path();
                let Ok(m) = CachedMaterial::from_bytes(&std::fs::read(&path)?) else { continue };
                let Some(o) = &m.original else { continue };
                let (Some(vs), Some(ps)) = (set.entry(o.vs), set.entry(o.ps)) else { continue };
                for h in [o.vs, o.ps] {
                    objects.entry(h).or_insert_with(|| vfs.read_hash(h).ok().and_then(|b| fc3_formats::d3d9::parse_object(&b).ok()));
                }
                let (Some(Some(vs_obj)), Some(Some(ps_obj))) = (objects.get(&o.vs), objects.get(&o.ps)) else { continue };
                let means: HashMap<u32, [f32; 4]> =
                    m.textures.iter().filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?))).collect();
                let sample = MaterialSample { material: &m, texture_means: &means };
                // The material's own gloss and reflectance (FC3_SURVEY_PARAMS).
                if std::env::var("FC3_SURVEY_PARAMS").is_ok() {
                    let p = |n: &str| m.params.iter().find(|(h, _)| *h == original::h(n)).map(|(_, raw)| original::param_floats(raw));
                    println!(
                        "params {} {:?} gloss {:?} reflectance {:?} spec-map {}",
                        m.shader,
                        path.file_name().unwrap(),
                        p("Glossiness"),
                        p("SpecularReflectance"),
                        m.textures.iter().any(|(h, _)| *h == original::h("SpecularTexture") || *h == original::h("SpecularMask"))
                    );
                }
                let Ok(t) = original::targets(&set, vs, vs_obj, ps, ps_obj, &sample) else { continue };
                let (spec, power) = (t[1][3], t[2][3] * 256.0);
                let v = [t[2][0] * 2.0 - 1.0, t[2][1] * 2.0 - 1.0, t[2][2] * 2.0 - 1.0];
                let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                if show.as_ref().is_some_and(|f| *f == m.shader) {
                    println!("{} spec {spec:.3} power {power:.1} t2 {:.3?}", path.file_name().unwrap().to_string_lossy(), t[2]);
                }
                let f = fams.entry(m.shader.clone()).or_default();
                f.0 += 1;
                f.1 += spec;
                f.2 += power;
                f.3[[0.05, 0.2, 0.5, 0.75].iter().position(|&b| spec < b).unwrap_or(4)] += 1;
                f.4[[2.0, 8.0, 32.0, 96.0, 200.0].iter().position(|&b| power < b).unwrap_or(5)] += 1;
                f.5 += usize::from((0.8..1.2).contains(&len));
            }
            println!("family: count, mean spec, mean power, spec <.05/<.2/<.5/<.75/more, power <2/<8/<32/<96/<200/more, target 2 rgb a unit vector");
            for (fam, (n, ss, sp, sb, pb, unit)) in fams {
                println!("{fam:28} {n:5} {:.3} {:7.1} {sb:?} {pb:?} {unit}", ss / n as f32, sp / n as f32);
            }
        }
        "pair-survey" => {
            use fc3_formats::cache::{CachedMaterial, ShaderEntry};
            use fc3_formats::original::{self, MaterialSample, ShaderSet};
            use std::sync::{Arc, Mutex};
            let root = fc3_formats::cache::default_cache_root();
            let index = std::fs::read_to_string(root.join(fc3_formats::cache::SHADER_INDEX_REL))?;
            let mut set = ShaderSet::new(index.lines().filter(|l| !l.starts_with('#')).filter_map(ShaderEntry::from_line).collect());
            for name in vfs.names.values().filter(|n| n.starts_with(original::DESCRIPTOR_DIR) && n.ends_with(".xml")) {
                if let Ok(bytes) = vfs.read(name) {
                    set.add_descriptor(&String::from_utf8_lossy(&bytes));
                }
            }
            let set = set;
            let objects: Mutex<HashMap<u64, Option<Arc<fc3_formats::d3d9::ShaderObject>>>> = Mutex::new(HashMap::new());
            let object = |h: u64| -> Option<Arc<fc3_formats::d3d9::ShaderObject>> {
                if let Some(o) = objects.lock().unwrap().get(&h) {
                    return o.clone();
                }
                let o = vfs.read_hash(h).ok().and_then(|b| fc3_formats::d3d9::parse_object(&b).ok()).map(Arc::new);
                objects.lock().unwrap().insert(h, o.clone());
                o
            };
            let mut stats: BTreeMap<String, usize> = BTreeMap::new();
            let mut first: BTreeMap<String, usize> = BTreeMap::new();
            let mut dye_debug = 0;
            let mut shaders: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
            // FC3_SURVEY_FAMILY=<shader> prints each unpaired material of one family.
            let only = std::env::var("FC3_SURVEY_FAMILY").ok();
            for e in std::fs::read_dir(root.join("materials"))? {
                let path = e?.path();
                let Ok(m) = CachedMaterial::from_bytes(&std::fs::read(&path)?) else { continue };
                if only.as_ref().is_some_and(|f| *f != m.shader) {
                    continue;
                }
                if only.is_some() && m.original.is_none() {
                    println!("{} diffuse {}", path.display(), m.diffuse);
                    for (h, t) in &m.textures {
                        println!("  tex {h:08x} {t}");
                    }
                    for (h, v) in &m.params {
                        println!("  par {h:08x} {}", v.len());
                    }
                    let ranked = set.ranked_pixel_shaders(&m);
                    println!("  {} pixel shader candidates", ranked.len());
                    // FC3_SURVEY_DEEP: every candidate's static vertex shaders and their checks.
                    if std::env::var("FC3_SURVEY_DEEP").is_ok() {
                        let means: HashMap<u32, [f32; 4]> = m
                            .textures
                            .iter()
                            .filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?)))
                            .collect();
                        let sample = MaterialSample { material: &m, texture_means: &means };
                        for (rank, ps) in ranked.iter().enumerate() {
                            let Some(ps_obj) = object(ps.hash) else { continue };
                            let vss: Vec<_> = set.vertex_shaders_for(ps, false).collect();
                            let mut results: BTreeMap<String, usize> = BTreeMap::new();
                            for vs in &vss {
                                let Some(vs_obj) = object(vs.hash) else { continue };
                                *results.entry(format!("{:?}", original::check(&set, vs, &vs_obj, ps, &ps_obj, &sample))).or_default() += 1;
                            }
                            let samplers: Vec<String> = ps.bindings.iter().filter(|b| b.sampler).map(|b| format!("{:08x}", b.name_hash)).collect();
                            println!("   #{rank} ps {:016x} [{}] {} static vs: {results:?}", ps.hash, samplers.join(" "), vss.len());
                        }
                    }
                }
                let fam = shaders.entry(m.shader.clone()).or_default();
                fam.0 += 1;
                if let Some(o) = &m.original {
                    fam.1 += 1;
                    fam.2 += usize::from(o.skinned_vs != 0);
                    *stats.entry("paired".into()).or_default() += 1;
                    // FC3_SURVEY_REPAIR: pair again as the converter now would,
                    // and count what would change.
                    if std::env::var("FC3_SURVEY_REPAIR").is_ok() {
                        let means: HashMap<u32, [f32; 4]> = m
                            .textures
                            .iter()
                            .filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?)))
                            .collect();
                        let sample = MaterialSample { material: &m, texture_means: &means };
                        let verdict = match original::pair_material(&set, &sample, &object, 96) {
                            None => "would lose its pair",
                            Some(n) if (n.vs, n.ps, n.skinned_vs) == (o.vs, o.ps, o.skinned_vs) => "same pair",
                            Some(n) if n.ps == o.ps => "same pixel shader, other vertex shader",
                            Some(_) => "other pixel shader",
                        };
                        *stats.entry(format!("re-paired: {verdict}")).or_default() += 1;
                        if verdict != "same pair" {
                            println!("re-paired {}: {verdict} [{}]", path.display(), m.shader);
                        }
                    }
                    // Dyed, but the pixel shader never reads the dye.
                    let dye = m.params.iter().find(|(h, _)| *h == fc3_formats::material::param::DIFFUSE_COLOR1).map(|(_, raw)| original::param_floats(raw));
                    if dye.as_ref().is_some_and(|f| f.len() >= 3 && f[..3].iter().any(|v| (v - 1.0).abs() > 0.02)) {
                        let reads = set.entry(o.ps).is_some_and(|e| e.bindings.iter().any(|b| b.name_hash == fc3_formats::material::param::DIFFUSE_COLOR1));
                        let vs_reads = set
                            .entry(o.vs)
                            .or(set.entry(o.skinned_vs))
                            .is_some_and(|e| e.bindings.iter().any(|b| b.name_hash == fc3_formats::material::param::DIFFUSE_COLOR1));
                        *stats
                            .entry(format!(
                                "paired dyed: {} {}{}",
                                m.shader,
                                if reads { "reads it" } else { "IGNORES it" },
                                if vs_reads { " (vs reads it)" } else { "" }
                            ))
                            .or_default() += 1;
                        // FC3_REPAIR: what pairing would pick now.
                        if !reads && std::env::var("FC3_REPAIR").is_ok() {
                            let means: HashMap<u32, [f32; 4]> = m
                                .textures
                                .iter()
                                .filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?)))
                                .collect();
                            let sample = MaterialSample { material: &m, texture_means: &means };
                            let now = original::pair_material(&set, &sample, &object, 96);
                            let outcome = match now {
                                Some(b)
                                    if set
                                        .entry(b.ps)
                                        .is_some_and(|e| e.bindings.iter().any(|x| x.name_hash == fc3_formats::material::param::DIFFUSE_COLOR1)) =>
                                {
                                    "now dyed"
                                }
                                Some(_) => "still undyed",
                                None => "now unpaired",
                            };
                            *stats.entry(format!("  repair {}: {outcome}", m.shader)).or_default() += 1;
                            if outcome == "still undyed" && dye_debug < 6 {
                                dye_debug += 1;
                                println!("{} ({}) dye {:?}", path.display(), m.shader, dye);
                                for ps in set
                                    .ranked_pixel_shaders(&m)
                                    .into_iter()
                                    .filter(|e| e.bindings.iter().any(|b| b.name_hash == fc3_formats::material::param::DIFFUSE_COLOR1))
                                    .take(4)
                                {
                                    let Some(ps_obj) = object(ps.hash) else { continue };
                                    for vs in set.vertex_shaders_for(ps, false).take(2) {
                                        let Some(vs_obj) = object(vs.hash) else { continue };
                                        let r = original::check_with(&set, vs, &vs_obj, ps, &ps_obj, &sample, false);
                                        let rn = original::check_with(&set, vs, &vs_obj, ps, &ps_obj, &sample, true);
                                        println!("   ps {:016x} vs {:016x}: {:?} / neutral {:?}", ps.hash, vs.hash, r, rn);
                                    }
                                }
                            }
                        }
                        // Which shaders anywhere read it?
                        if !reads && std::env::var("FC3_DYE_DEBUG").is_ok() {
                            let readers = set
                                .ranked_pixel_shaders(&m)
                                .iter()
                                .filter(|e| e.bindings.iter().any(|b| b.name_hash == fc3_formats::material::param::DIFFUSE_COLOR1))
                                .count();
                            *stats.entry(format!("  {}: candidates reading the dye {}", m.shader, if readers > 0 { "exist" } else { "none" })).or_default() +=
                                1;
                        }
                    }
                    if o.vs != 0 {
                        *stats.entry("paired: static".into()).or_default() += 1;
                    } else if std::env::var("FC3_SURVEY_STATIC").is_ok() {
                        // Skinned only: why each static vertex shader failed.
                        let means: HashMap<u32, [f32; 4]> = m
                            .textures
                            .iter()
                            .filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?)))
                            .collect();
                        let sample = MaterialSample { material: &m, texture_means: &means };
                        if let (Some(ps), Some(ps_obj)) = (set.entry(o.ps), object(o.ps)) {
                            let all: Vec<_> = set.vertex_shaders_for(ps, false).collect();
                            *stats.entry(format!("static-less: {} static candidates", all.len().min(9))).or_default() += 1;
                            for vs in all.iter().take(4) {
                                let Some(vs_obj) = object(vs.hash) else { continue };
                                let r = original::check(&set, vs, &vs_obj, ps, &ps_obj, &sample);
                                *stats.entry(format!("static-less: {:?}", r.err())).or_default() += 1;
                            }
                        }
                    }
                    if o.skinned_vs != 0 {
                        *stats.entry("paired: skinned".into()).or_default() += 1;
                    }
                    // Cut-out materials: does the pixel shader cut by `AlphaValues` itself?
                    let flags = m.flags;
                    if flags & fc3_formats::cache::mat_flags::ALPHA_TEST != 0 {
                        let veg = flags & fc3_formats::cache::mat_flags::VEGETATION != 0;
                        let cuts = set.entry(o.ps).is_some_and(|e| e.bindings.iter().any(|b| b.name_hash == 0xE53F_44C4));
                        let has = m.params.iter().any(|(h, _)| *h == 0xE53F_44C4);
                        *stats
                            .entry(format!("alpha-test{}: ps reads AlphaValues {cuts}, material sets it {has}", if veg { " (vegetation)" } else { "" }))
                            .or_default() += 1;
                        // Does the colour output's alpha come from the diffuse map?
                        let diffuse_reg = o.samplers.iter().find(|(_, rel, _)| *rel == m.diffuse).map(|(r, _, _)| *r as u32);
                        let from = object(o.ps).map(|obj| fc3_formats::d3d9_wgsl::colour_alpha_samplers(&obj.program)).unwrap_or(0);
                        let verdict = match diffuse_reg {
                            Some(r) if from == 1 << r => "diffuse alpha only",
                            Some(r) if from & (1 << r) != 0 => "diffuse alpha and more",
                            _ if from != 0 => "other textures",
                            _ => "no texture",
                        };
                        *stats.entry(format!("alpha-test{} output alpha: {verdict}", if veg { " (vegetation)" } else { "" })).or_default() += 1;
                    }
                    continue;
                }
                // As the converter: these never take a pair.
                use fc3_formats::cache::mat_flags;
                if let Some(kind) = [(mat_flags::PLACEHOLDER, "grey placeholder"), (mat_flags::WATER, "water"), (mat_flags::NO_DRAW, "not drawn")]
                    .iter()
                    .find(|(f, _)| m.flags & f != 0)
                    .map(|(_, k)| *k)
                {
                    *stats.entry(kind.into()).or_default() += 1;
                    continue;
                }
                let means: HashMap<u32, [f32; 4]> =
                    m.textures.iter().filter_map(|(h, rel)| Some((*h, fc3_formats::texture::mean_color(&std::fs::read(root.join(rel)).ok()?)?))).collect();
                let sample = MaterialSample { material: &m, texture_means: &means };
                // Pairing as the converter now would (the cache may be older).
                let mean = |p: &str| vfs.read(p).ok().and_then(|b| fc3_formats::texture::parse(&b).ok().and_then(|x| fc3_formats::texture::mean_color(x.dds)));
                if let Some((_, added)) = original::pair_material_or_replaced(&set, &sample, &object, 96, &mean) {
                    let with = if added.is_empty() { "" } else { " with the engine's stand-in textures" };
                    *stats.entry(format!("unpaired in the cache, pairs now{with}")).or_default() += 1;
                    println!("pairs now{with}: {} [{}]", path.display(), m.shader);
                    continue;
                }
                let ranked = set.ranked_pixel_shaders(&m);
                if ranked.is_empty() {
                    *stats.entry("no pixel shader candidate".into()).or_default() += 1;
                    continue;
                }
                if !ranked.iter().take(96).any(|ps| set.vertex_shaders_for(ps, false).next().is_some() || set.vertex_shaders_for(ps, true).next().is_some()) {
                    *stats.entry("no compatible vertex shader".into()).or_default() += 1;
                    continue;
                }
                let reasons = original::diagnose(&set, &sample, &object, 96);
                *stats.entry("rejected".into()).or_default() += 1;
                if let Some(r) = reasons.first() {
                    *first.entry(format!("{r:?}")).or_default() += 1;
                }
            }
            println!("{stats:#?}");
            println!("top candidate rejection: {first:#?}");
            match set.terrain_pair() {
                Some((vs, ps)) => println!("terrain pair: vs {:016x} ps {:016x}", vs.hash, ps.hash),
                None => println!("terrain pair: none"),
            }
            let mut fams: Vec<_> = shaders.into_iter().collect();
            fams.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
            for (f, (n, ok, sk)) in fams.iter().take(25) {
                println!("  {f:32} {ok:5}/{n:5} (skinned {sk})");
            }
        }
        "ctab-regs" => {
            // Which parameters the shaders' constant tables put at a register:
            // ctab-regs <register> [ps|vs]
            let reg: u8 = args.get(1).context("usage: ctab-regs <register> [ps|vs]")?.parse()?;
            let stage = args.get(2).cloned().unwrap_or_else(|| "ps".into());
            let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
            for a in &vfs.archives {
                if a.name != "shadersobj" && a.name != "patch" {
                    continue;
                }
                for e in &a.fat.entries {
                    let Ok(d) = a.read(e) else { continue };
                    let Ok(obj) = fc3_formats::d3d9::parse_object(&d) else { continue };
                    let is_ps = format!("{}", obj.program).trim_start().starts_with("ps");
                    if is_ps != (stage == "ps") {
                        continue;
                    }
                    for b in obj.bindings.iter().filter(|b| b.register == reg && b.kind != 12) {
                        *counts.entry(b.name_hash).or_default() += 1;
                    }
                }
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            for (hash, n) in v.into_iter().take(15) {
                println!("{hash:08x} {n}");
            }
        }
        "grade" => {
            // The tone curve as the converter derives it (FC3_GRADE_DEBUG for each candidate).
            let cube = fc3_formats::hash::crc32(b"ColorRemap0VolumeSampler");
            let mut passes = Vec::new();
            for a in vfs.archives.iter().filter(|a| a.name == "shadersobj" || a.name == "patch") {
                for e in &a.fat.entries {
                    let Ok(d) = a.read(e) else { continue };
                    let Ok(obj) = fc3_formats::d3d9::parse_object(&d) else { continue };
                    if obj.bindings.iter().any(|b| b.is_sampler() && b.name_hash == cube) {
                        passes.push(obj);
                    }
                }
            }
            println!("{} passes with a colour cube", passes.len());
            let (x_max, curve) = fc3_formats::grade::curve_from_shaders(&passes)?;
            println!("x_max {x_max}");
            for i in (0..curve.len()).step_by(16) {
                let x = x_max * (i as f32 / (curve.len() - 1) as f32).powi(2);
                println!("  x {x:8.4} -> {:.4}", curve[i]);
            }
        }
        "shader-dis" => {
            let target = args.get(1).context("usage: shader-dis <path|hash>")?;
            let data = vfs.read_hash(parse_target(&vfs, target)?)?;
            let obj = fc3_formats::d3d9::parse_object(&data)?;
            for b in &obj.bindings {
                println!("; {:08x} kind {:2} reg {:3}.{} rows {} cols {}", b.name_hash, b.kind, b.register, b.component, b.rows, b.columns);
            }
            print!("{}", obj.program);
        }
        "shader-survey" => {
            use fc3_formats::d3d9::{self, Op};
            let mut ok = 0usize;
            let mut failed: BTreeMap<String, usize> = BTreeMap::new();
            let mut versions: BTreeMap<String, usize> = BTreeMap::new();
            let mut ops: BTreeMap<String, usize> = BTreeMap::new();
            let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
            let mut hashes: BTreeMap<(u32, u8, u8, u8), usize> = BTreeMap::new();
            let mut total = 0usize;
            for a in &vfs.archives {
                if a.name != "shadersobj" && a.name != "patch" {
                    continue;
                }
                for e in &a.fat.entries {
                    let Ok(data) = a.read(e) else { continue };
                    if data.len() < 4 || u32::from_le_bytes(data[..4].try_into().unwrap()) != d3d9::CONTAINER_MAGIC {
                        continue;
                    }
                    total += 1;
                    match d3d9::parse_object(&data) {
                        Ok(o) => {
                            ok += 1;
                            let stage = if o.program.stage == d3d9::Stage::Vertex { "vs" } else { "ps" };
                            *versions.entry(format!("{stage}_{}_{}", o.program.major, o.program.minor)).or_default() += 1;
                            for i in &o.program.instrs {
                                let name = match &i.op {
                                    Op::Other(n) => format!("other{n}"),
                                    Op::Dcl(_) => "dcl".into(),
                                    Op::Def(_) => "def".into(),
                                    Op::DefI(_) => "defi".into(),
                                    Op::DefB(_) => "defb".into(),
                                    op => format!("{op:?}").to_ascii_lowercase(),
                                };
                                *ops.entry(name).or_default() += 1;
                            }
                            for b in &o.bindings {
                                *kinds.entry(b.kind).or_default() += 1;
                                let st = if o.program.stage == d3d9::Stage::Vertex { 0 } else { 1 };
                                *hashes.entry((b.name_hash, st, b.kind, b.rows)).or_default() += 1;
                            }
                        }
                        Err(err) => *failed.entry(format!("{err:#}")).or_default() += 1,
                    }
                }
            }
            println!("shader objects: {total}, parsed: {ok}");
            for (e, n) in &failed {
                println!("  fail {n:6} {e}");
            }
            println!("versions: {versions:?}");
            let mut ops: Vec<_> = ops.into_iter().collect();
            ops.sort_by(|a, b| b.1.cmp(&a.1));
            println!("ops: {ops:?}");
            println!("binding kinds: {kinds:?}");
            if let Some(out) = args.get(1) {
                let mut text = String::from("hash stage kind rows count\n");
                for ((h, st, kind, rows), n) in &hashes {
                    text += &format!("{h:08x} {} {kind} {rows} {n}\n", if *st == 0 { "vs" } else { "ps" });
                }
                std::fs::write(out, text)?;
            }
            let mut hashes: Vec<_> = hashes.into_iter().collect();
            hashes.sort_by(|a, b| b.1.cmp(&a.1));
            for ((h, st, kind, rows), n) in hashes.iter().take(60) {
                println!("  {h:08x} {} kind {kind:2} rows {rows} used {n}", if *st == 0 { "vs" } else { "ps" });
            }
        }
        _ => bail!("unknown command {cmd}"),
    }
    Ok(())
}

/// Field/class names recovered by guessing; used only to make dumps readable.
/// Field and object names: the known ones and the identifiers in the
/// installed game's own binary (local only).
fn component_names() -> fc3_formats::fcb::NameDb {
    let mut names = fc3_formats::fcb::NameDb::default();
    for n in KNOWN_NAMES {
        names.add(n);
    }
    if let Some(dll) = fc3_formats::find_install().and_then(|d| d.parent().map(|p| p.join("bin").join("FC3.dll"))) {
        if let Ok(b) = std::fs::read(dll) {
            names.add_identifiers_from(&b);
        }
    }
    names
}

const KNOWN_NAMES: &[&str] = &[
    "Entity",
    "EntityLibrary",
    "EntityLibraries",
    "EntityPrototype",
    "Components",
    "Component",
    "hidPos",
    "hidPos_precise",
    "hidAngles",
    "hidName",
    "hidEntityClass",
    "hidResourceFile",
    "disEntityId",
    "ArchetypeGuid",
    "ArchetypeName",
    "ArchetypeResDepList",
    "hidConstEntity",
    "CGraphicComponent",
    "CPhysComponent",
    "CEntity",
    "CStaticEntity",
    "fileDescriptor",
    "hidMesh",
    "Mesh",
    "MeshFile",
    "sModelName",
    "text_hidResourceFile",
    "hidBBox",
    "hidScale",
    "WorldSector",
    "Sector",
    "Entities",
    "Objects",
    "Layers",
    "Layer",
    "LayerName",
    "Name",
    "CPhysicsComponent",
    "CCollisionComponent",
    "CStaticGraphicComponent",
    "hidModel",
    "ResourceFile",
    "Resource",
    "Resources",
    "sResourceName",
    "Archetype",
    "Archetypes",
    "Prototype",
    "Prototypes",
    "hidLibraryEntityId",
    "hidCreatedByLibraryEntity",
    "LibEntity",
    "hidPrecise",
    "hidOrientation",
    "hidQuat",
    "hidRotation",
    "hidTransform",
    "Transform",
    "CTransformComponent",
    "Position",
    "Orientation",
    "Angles",
    "Scale",
    "Model",
    "Models",
    "CEditorComponent",
    "CDebugComponent",
    "hidNotLibraryEntity",
    "hidSkipCollision",
    "CVegetationComponent",
    "Vegetation",
    "CAIComponent",
    "CSoundComponent",
    "CLightComponent",
    "CParticleComponent",
    "CTriggerComponent",
    "CDestructibleComponent",
    "CPickupComponent",
    "CEditorPropertiesComponent",
    "CWaterComponent",
    "CRoadComponent",
    "Road",
    "Roads",
    "id",
    "Id",
    "ID",
    "guid",
    "Guid",
    "GUID",
    "type",
    "Type",
    "class",
    "Class",
    "value",
    "Value",
    "file",
    "File",
    "path",
    "Path",
    "mesh",
    "model",
    "material",
    "Material",
    "Materials",
];
