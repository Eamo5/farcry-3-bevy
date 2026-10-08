//! Far Cry 3 audio explorer: survey / inspect / export the game's sound banks.
//!
//! ```text
//! fc3audio survey [--decode]             format/codec histogram over every sound file (+ decode QC)
//! fc3audio check                         converter path (list_sounds + decode_ref) over every sound + QC
//! fc3audio info <path|id> [--raw]        list BAOs/headers/events in a .spk/.sbao
//! fc3audio tree <id>                     event -> header graph across packages
//! fc3audio resolve <id..> [--switch g=v] voices / weighted variants an event plays
//! fc3audio export <path|id> <outdir>     decode to .wav/.ogg (+ RMS/peak/clip/ZCR)
//! fc3audio manifest <out.txt>            SoundGraph::manifest()
//! fc3audio find <id..>                   BAOs with / referencing an id
//! fc3audio refs <out.tsv> [substring]    scan other game files for sound ids
//! fc3audio summary <ids.txt> <out.tsv>   one-line description per id
//! fc3audio examples <class> <kind> [n]   raw payload dumps
//! fc3audio names                         resolve unnamed sound.fat entries
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fc3_formats::audio::{self, Codec, Header, SoundData, SoundFiles, Spk};
use fc3_formats::{find_install, hash::path_hash, Vfs};

fn filelist_dir() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists");
    if p.exists() {
        p
    } else {
        PathBuf::from("data/filelists")
    }
}

struct VfsFiles<'a>(&'a Vfs);
impl SoundFiles for VfsFiles<'_> {
    fn sbao(&mut self, id: u32) -> Option<Vec<u8>> {
        self.0.read(&audio::sbao_path(id)).ok()
    }
    fn spk(&mut self, id: u32) -> Option<Vec<u8>> {
        self.0.read(&audio::spk_path(id)).ok()
    }
}

/// Resolves a CLI argument to a file hash: archive path, hex id (spk, then sbao) or raw 16-hex hash.
fn resolve(vfs: &Vfs, s: &str) -> Result<(u64, String)> {
    let s = s.trim();
    if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        let h = u64::from_str_radix(s, 16)?;
        return Ok((h, vfs.name_of(h).unwrap_or(s).to_string()));
    }
    if vfs.exists(s) {
        return Ok((path_hash(s), s.replace('/', "\\")));
    }
    if let Ok(id) = u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("sndres0x"), 16) {
        for p in [audio::spk_path(id), audio::sbao_path(id)] {
            if vfs.exists(&p) {
                return Ok((path_hash(&p), p));
            }
        }
    }
    bail!("{s}: not found")
}

/// Every sound file in the VFS: (hash, display name). Unnamed sound.fat entries included.
fn sound_files(vfs: &Vfs) -> Vec<(u64, String)> {
    let sound_arch = vfs.archives.iter().position(|a| a.name == "sound");
    let mut v: Vec<(u64, String)> = vfs
        .index
        .iter()
        .filter_map(|(h, loc)| match vfs.name_of(*h) {
            Some(n) if n.starts_with("soundbinary\\") => Some((*h, n.to_string())),
            None if Some(loc.archive as usize) == sound_arch => Some((*h, format!("{h:016X}"))),
            _ => None,
        })
        .collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

#[derive(Default)]
struct Stats {
    secs: f64,
    rms: f64,
    peak: i32,
    clip_frac: f64,
    zcr: f64,
    dc: f64,
}

fn pcm_stats(samples: &[i16], channels: usize, rate: u32) -> Stats {
    let n = samples.len();
    if n == 0 {
        return Stats::default();
    }
    let mut sum2 = 0f64;
    let mut sum = 0f64;
    let mut peak = 0i32;
    let mut clip = 0usize;
    let mut zc = 0usize;
    for (i, &s) in samples.iter().enumerate() {
        let v = s as f64;
        sum += v;
        sum2 += v * v;
        peak = peak.max((s as i32).abs());
        if s == i16::MAX || s == i16::MIN {
            clip += 1;
        }
        if i >= channels && ((samples[i - channels] < 0) != (s < 0)) {
            zc += 1;
        }
    }
    let frames = n / channels.max(1);
    Stats {
        secs: frames as f64 / rate.max(1) as f64,
        rms: (sum2 / n as f64).sqrt() / 32768.0,
        peak,
        clip_frac: clip as f64 / n as f64,
        zcr: zc as f64 / n as f64,
        dc: sum / n as f64 / 32768.0,
    }
}

fn describe_header(h: &Header) -> String {
    match h {
        Header::Audio(a) => format!(
            "audio {:08x}: {} {}ch {}Hz {} samples ({:.2}s){} src {:08x} {}{} size {}",
            a.id,
            a.codec.name(),
            a.channels,
            a.sample_rate,
            a.num_samples,
            a.num_samples as f32 / a.sample_rate.max(1) as f32,
            if a.looping { format!(" loop@{}", a.loop_start) } else { String::new() },
            a.source.resource_id,
            if a.source.is_stream { "stream" } else { "memory" },
            if a.source.prefetch_size > 0 { format!("+prefetch {}", a.source.prefetch_size) } else { String::new() },
            a.source.size
        ),
        Header::Layer(l) => {
            let ls: Vec<String> = l
                .layers
                .iter()
                .map(|x| format!("{} {}ch {}Hz {}smp", x.codec.name(), x.channels, x.sample_rate, x.num_samples))
                .collect();
            format!(
                "layer {:08x}: {} layers [{}] src {:08x} {}{}{} size {}",
                l.id,
                l.layers.len(),
                ls.join(", "),
                l.source.resource_id,
                if l.source.inline { "inline" } else if l.source.is_stream { "stream" } else { "memory" },
                if l.source.prefetch_size > 0 { format!("+prefetch {}", l.source.prefetch_size) } else { String::new() },
                if l.looping { " loop" } else { "" },
                l.source.size
            )
        }
        Header::Random { id, children } => {
            let c: Vec<String> = children.iter().map(|(i, w)| format!("{i:08x}:{w:.2}")).collect();
            format!("random {id:08x}: [{}]", c.join(" "))
        }
        Header::Sequence { id, children, looping } => {
            let c: Vec<String> = children.iter().map(|i| format!("{i:08x}")).collect();
            format!("sequence {id:08x}{}: [{}]", if *looping { " loop" } else { "" }, c.join(" "))
        }
        Header::Silence { id, duration } => format!("silence {id:08x}: {duration}s"),
        Header::Switch { id, group, cases } => {
            let c: Vec<String> = cases.iter().map(|(i, v)| format!("{v:08x}=>{i:08x}")).collect();
            format!("switch {id:08x} on {group:08x}: [{}]", c.join(" "))
        }
        Header::Other { id, kind, refs } => {
            let c: Vec<String> = refs.iter().map(|i| format!("{i:08x}")).collect();
            format!("type{kind} {id:08x}: [{}]", c.join(" "))
        }
    }
}

fn hexdump_u32(b: &[u8], max: usize) -> String {
    b.chunks_exact(4)
        .take(max / 4)
        .map(|c| format!("{:08x}", u32::from_le_bytes(c.try_into().unwrap())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = find_install().context("Far Cry 3 install not found; set FC3_DATA")?;
    let vfs = Vfs::open_install(&data, Some(&filelist_dir()))?;
    let cmd = args.first().map(String::as_str).unwrap_or("survey");
    match cmd {
        "survey" => survey(&vfs, args.iter().any(|a| a == "--decode")),
        "info" => {
            let (h, name) = resolve(&vfs, args.get(1).context("usage: info <path|id> [--raw]")?)?;
            let raw = args.iter().any(|a| a == "--raw");
            let bytes = vfs.read_hash(h)?;
            println!("{name}: {} bytes", bytes.len());
            let baos = audio::parse_container(&bytes, audio::id_from_path(&name).unwrap_or(0))?;
            for b in &baos {
                print!("  {:08x} class {} size {:#x}", b.id, b.class, b.data.len());
                match b.class {
                    audio::CLASS_HEADER => match audio::parse_header(b) {
                        Ok(hd) => println!("  {}", describe_header(&hd)),
                        Err(e) => println!("  header ERR {e:#}"),
                    },
                    audio::CLASS_EVENT => match audio::parse_event(b) {
                        Ok(e) => println!(
                            "  event kind {} -> {:?} vol {}",
                            e.kind,
                            e.targets.iter().map(|t| format!("{t:08x}")).collect::<Vec<_>>(),
                            e.volume
                        ),
                        Err(e) => println!("  event ERR {e:#}"),
                    },
                    _ => println!("  payload starts {}", hexdump_u32(&b.payload()[..b.payload().len().min(32)], 32)),
                }
                if raw && matches!(b.class, audio::CLASS_HEADER | audio::CLASS_EVENT) {
                    println!("      {}", hexdump_u32(b.payload(), 0x400));
                }
            }
            Ok(())
        }
        "export" => {
            let (h, name) = resolve(&vfs, args.get(1).context("usage: export <path|id> <outdir>")?)?;
            let outdir = PathBuf::from(args.get(2).context("usage: export <path|id> <outdir>")?);
            std::fs::create_dir_all(&outdir)?;
            let bytes = vfs.read_hash(h)?;
            let sounds = audio::decode_sound_with(&bytes, &mut VfsFiles(&vfs))?;
            let stem = name.rsplit('\\').next().unwrap().split('.').next().unwrap().to_string();
            for s in &sounds {
                let (file, ext) = s.data.to_file_bytes();
                let fname = match s.layer {
                    Some((i, n)) => format!("{stem}_{:08x}_L{i}of{n}.{ext}", s.header_id),
                    None => format!("{stem}_{:08x}.{ext}", s.header_id),
                };
                std::fs::write(outdir.join(&fname), &file)?;
                print!(
                    "{fname}: {} {}ch {}Hz {:.2}s{}",
                    s.codec.name(),
                    s.channels,
                    s.sample_rate,
                    s.duration_secs(),
                    if s.looping { " loop" } else { "" }
                );
                if let SoundData::Pcm { samples, channels, sample_rate } = &s.data {
                    let st = pcm_stats(samples, *channels as usize, *sample_rate);
                    print!(
                        "  pcm {:.2}s rms {:.4} peak {} clip {:.5} zcr {:.4} dc {:+.4}",
                        st.secs, st.rms, st.peak, st.clip_frac, st.zcr, st.dc
                    );
                }
                println!();
            }
            if sounds.is_empty() {
                println!("no playable sounds in {name} (event/random-only package?)");
            }
            Ok(())
        }
        "tree" => {
            let (h, name) = resolve(&vfs, args.get(1).context("usage: tree <path|id>")?)?;
            let index = build_index(&vfs);
            let root = audio::id_from_path(&name).unwrap_or(0);
            let _ = h;
            print_tree(&vfs, &index, root, 0, &mut HashSet::new())?;
            Ok(())
        }
        "names" => {
            // Unnamed sound.fat entries are soundbinary\%08x.sbao for stream ids referenced by headers.
            let mut known = HashMap::new();
            for (h, name) in sound_files(&vfs) {
                if !name.ends_with(".spk") {
                    continue;
                }
                let Ok(bytes) = vfs.read_hash(h) else { continue };
                let Ok(spk) = Spk::parse(&bytes) else { continue };
                for b in spk.baos.iter().filter(|b| b.class == audio::CLASS_HEADER) {
                    if let Ok(hd) = audio::parse_header(b) {
                        let src = match &hd {
                            Header::Audio(a) => Some(a.source),
                            Header::Layer(l) => Some(l.source),
                            _ => None,
                        };
                        if let Some(s) = src.filter(|s| s.is_stream) {
                            known.insert(path_hash(&audio::sbao_path(s.resource_id)), s.resource_id);
                        }
                    }
                }
            }
            let mut unnamed = 0;
            let mut resolved = 0;
            for (h, name) in sound_files(&vfs) {
                if vfs.name_of(h).is_none() {
                    unnamed += 1;
                    if let Some(id) = known.get(&h) {
                        resolved += 1;
                        println!("{name} = {}", audio::sbao_path(*id));
                    } else {
                        println!("{name} = ?");
                    }
                }
            }
            println!("resolved {resolved}/{unnamed} unnamed; {} streams referenced", known.len());
            Ok(())
        }
        "check" => {
            // End-to-end converter path: list_sounds + decode_ref for every sound, with QC.
            let t0 = std::time::Instant::now();
            let graph = audio::SoundGraph::build(&vfs);
            let sounds = graph.sounds();
            println!(
                "graph: {} events, {} headers, {} packages, {} playable sounds ({:.1}s)",
                graph.events.len(),
                graph.headers.len(),
                graph.packages.len(),
                sounds.len(),
                t0.elapsed().as_secs_f32()
            );
            let mut qc = Qc::default();
            let mut errs = 0;
            let mut by_codec: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
            for (i, r) in sounds.iter().enumerate() {
                if i % 5000 == 0 {
                    eprintln!("{i}/{}", sounds.len());
                }
                let e = by_codec.entry(r.codec.name()).or_default();
                e.0 += 1;
                e.1 += r.duration_secs() as f64;
                match audio::decode_ref(&vfs, r) {
                    Ok(d) => qc.check(&r.package, &d),
                    Err(err) => {
                        errs += 1;
                        if errs < 20 {
                            println!("ERR {} {}: {err:#}", r.package, r.file_stem());
                        }
                    }
                }
            }
            println!("decoded {} / {} sounds, {} errors, {:.1}s", sounds.len() - errs, sounds.len(), errs, t0.elapsed().as_secs_f32());
            println!("by codec (count, hours): {:?}", by_codec.iter().map(|(k, v)| (*k, v.0, (v.1 / 3600.0 * 10.0).round() / 10.0)).collect::<Vec<_>>());
            let ev_resolved = graph.events.keys().filter(|id| !graph.resolve(**id).is_empty()).count();
            println!("events resolving to >=1 playable sound: {} / {}", ev_resolved, graph.events.len());
            let pk_resolved = graph.packages.iter().filter(|id| !graph.resolve(**id).is_empty()).count();
            println!("packages (sndres ids) resolving to >=1 playable sound: {} / {}", pk_resolved, graph.packages.len());
            qc.report();
            Ok(())
        }
        "resolve" => {
            // resolve <id...> [--switch group=value ...]: voices and weighted alternatives
            let graph = audio::SoundGraph::build(&vfs);
            let mut switches = HashMap::new();
            let mut ids = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--switch" {
                    let kv = it.next().context("--switch group=value")?;
                    let (g, v) = kv.split_once('=').context("--switch group=value")?;
                    switches.insert(
                        u32::from_str_radix(g.trim_start_matches("0x"), 16)?,
                        u32::from_str_radix(v.trim_start_matches("0x"), 16)?,
                    );
                } else {
                    ids.push(u32::from_str_radix(a.trim_start_matches("0x").trim_start_matches("sndres0x"), 16)?);
                }
            }
            for id in ids {
                println!("{id:08x}:");
                for (n, voice) in graph.voices(id, &switches).iter().enumerate() {
                    println!("  voice {n} ({} alternatives)", voice.len());
                    for v in voice {
                        let h = graph.headers.get(&v.header_id).map(describe_header).unwrap_or_default();
                        println!("    {:.3}  {}", v.weight, h);
                    }
                }
            }
            Ok(())
        }
        "manifest" => {
            let out = args.get(1).context("usage: manifest <out.txt>")?;
            let graph = audio::SoundGraph::build(&vfs);
            std::fs::write(out, graph.manifest())?;
            println!("wrote {out}");
            Ok(())
        }
        "refs" => {
            // refs <out.tsv> [ext-filter]: find references to sound package/event ids in other game files.
            let out = args.get(1).context("usage: refs <out.tsv> [substring]")?;
            let filter = args.get(2).map(|s| s.to_ascii_lowercase());
            let graph = audio::SoundGraph::build(&vfs);
            let pk: HashSet<u32> = graph.packages.iter().copied().collect();
            let ev: HashSet<u32> = graph.events.keys().copied().chain(graph.headers.keys().copied()).collect();
            let skip_ext = [".xbt", ".xbg", ".mab", ".pso", ".vso", ".bik", ".spk", ".sbao", ".bao", ".hkx", ".dds", ".cso", ".hso", ".dso"];
            let mut lines = Vec::new();
            let mut per_file: Vec<(usize, String)> = Vec::new();
            let mut names: Vec<(&u64, &String)> = vfs.names.iter().collect();
            names.sort_by(|a, b| a.1.cmp(b.1));
            for (h, name) in names {
                if (filter.is_none() && skip_ext.iter().any(|e| name.ends_with(e))) || name.starts_with("soundbinary\\") {
                    continue;
                }
                if let Some(f) = &filter {
                    if !name.contains(f.as_str()) {
                        continue;
                    }
                }
                let Ok(bytes) = vfs.read_hash(*h) else { continue };
                let text = bytes.iter().take(4096).filter(|&&b| b == 0 || b > 0x7e).count() * 50 < bytes.len().min(4096);
                let mut found = 0usize;
                if text {
                    let t = String::from_utf8_lossy(&bytes);
                    for (ln, line) in t.lines().enumerate() {
                        let lower = line.to_ascii_lowercase();
                        let mut i = 0;
                        while let Some(pos) = lower[i..].find("0x") {
                            let st = i + pos + 2;
                            i = st;
                            let hex: String = lower[st..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                            if hex.len() != 8 {
                                continue;
                            }
                            let v = u32::from_str_radix(&hex, 16).unwrap();
                            if pk.contains(&v) || ev.contains(&v) {
                                found += 1;
                                let ctx: String = line.trim().chars().take(240).collect();
                                lines.push(format!("{v:08x}\t{}\t{name}:{}\t{ctx}", if pk.contains(&v) { "pkg" } else { "bao" }, ln + 1));
                            }
                        }
                    }
                } else {
                    for o in 0..bytes.len().saturating_sub(3) {
                        let v = u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
                        if v >= 0x1_0000 && v < 0x0100_0000 && pk.contains(&v) {
                            found += 1;
                            // nearest preceding printable string as context
                            let lo = o.saturating_sub(96);
                            let ctx: String = bytes[lo..o]
                                .split(|&b| b < 0x20 || b > 0x7e)
                                .filter(|w| w.len() >= 4)
                                .last()
                                .map(|w| String::from_utf8_lossy(w).to_string())
                                .unwrap_or_default();
                            lines.push(format!("{v:08x}\tpkg\t{name}@{o:#x}\t{ctx}"));
                        }
                    }
                }
                if found > 0 {
                    per_file.push((found, name.clone()));
                }
            }
            std::fs::write(out, lines.join("\n"))?;
            per_file.sort_by(|a, b| b.0.cmp(&a.0));
            for (n, f) in per_file.iter().take(80) {
                println!("{n:7} {f}");
            }
            println!("{} files reference sound ids; {} refs written to {out}", per_file.len(), lines.len());
            Ok(())
        }
        "summary" => {
            // summary <ids.txt> <out.tsv>: one line per id describing what it plays.
            let src = args.get(1).context("usage: summary <ids.txt> <out.tsv>")?;
            let out = args.get(2).context("usage: summary <ids.txt> <out.tsv>")?;
            let graph = audio::SoundGraph::build(&vfs);
            let mut lines = Vec::new();
            for tok in std::fs::read_to_string(src)?.split_whitespace() {
                let Ok(id) = u32::from_str_radix(tok.trim_start_matches("0x"), 16) else { continue };
                lines.push(format!("{id:08x}\t{}", summarize(&graph, id)));
            }
            std::fs::write(out, lines.join("\n"))?;
            println!("wrote {} lines", lines.len());
            Ok(())
        }
        "find" => {
            // find <id...>: BAOs with that id, and BAOs whose payload mentions it.
            let ids: Vec<u32> = args[1..]
                .iter()
                .filter_map(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())
                .collect();
            for (h, name) in sound_files(&vfs) {
                let Ok(bytes) = vfs.read_hash(h) else { continue };
                let Ok(baos) = audio::parse_container(&bytes, 0) else { continue };
                for b in &baos {
                    for &id in &ids {
                        if b.id == id {
                            println!("{id:08x}: is BAO class {} in {name} (size {:#x})", b.class, b.data.len());
                        } else if b.class != audio::CLASS_MEMORY
                            && b.class != audio::CLASS_STREAM
                            && b.payload().chunks_exact(4).any(|c| u32::from_le_bytes(c.try_into().unwrap()) == id)
                        {
                            println!("{id:08x}: referenced by {:08x} (class {}) in {name}", b.id, b.class);
                        }
                    }
                }
            }
            Ok(())
        }
        "examples" => {
            // examples <class> <kind> [n]: raw payload dumps of BAOs with a given class/kind
            let class: u8 = args.get(1).context("usage: examples <class> <kind> [n]")?.parse()?;
            let kind: u32 = args.get(2).context("kind")?.parse()?;
            let n: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(3);
            let mut found = 0;
            for (h, name) in sound_files(&vfs) {
                let Ok(bytes) = vfs.read_hash(h) else { continue };
                let Ok(baos) = audio::parse_container(&bytes, 0) else { continue };
                for b in &baos {
                    if b.class == class && b.payload().len() >= 8 && u32::from_le_bytes(b.payload()[4..8].try_into().unwrap()) == kind {
                        println!("{name} {:08x} size {:#x}\n    {}", b.id, b.data.len(), hexdump_u32(b.payload(), 0x300));
                        found += 1;
                        if found >= n {
                            return Ok(());
                        }
                    }
                }
            }
            Ok(())
        }
        _ => bail!("unknown command {cmd}"),
    }
}

/// BAO id -> package hash for every .spk.
fn build_index(vfs: &Vfs) -> HashMap<u32, u64> {
    let mut idx = HashMap::new();
    for (h, name) in sound_files(vfs) {
        if !name.ends_with(".spk") {
            continue;
        }
        let Ok(bytes) = vfs.read_hash(h) else { continue };
        let Ok(spk) = Spk::parse(&bytes) else { continue };
        for b in &spk.baos {
            // prefer the package named after the id
            let own = audio::id_from_path(&name) == Some(b.id);
            if own || !idx.contains_key(&b.id) {
                idx.insert(b.id, h);
            }
        }
    }
    idx
}

fn print_tree(vfs: &Vfs, index: &HashMap<u32, u64>, id: u32, depth: usize, seen: &mut HashSet<u32>) -> Result<()> {
    let pad = "  ".repeat(depth);
    if !seen.insert(id) || depth > 12 {
        println!("{pad}{id:08x} (seen)");
        return Ok(());
    }
    let Some(h) = index.get(&id) else {
        println!("{pad}{id:08x} (not found)");
        return Ok(());
    };
    let bytes = vfs.read_hash(*h)?;
    let spk = Spk::parse(&bytes)?;
    let b = spk.get(id).context("id vanished")?;
    let pkg = vfs.name_of(*h).unwrap_or("?");
    match b.class {
        audio::CLASS_EVENT => {
            let e = audio::parse_event(b)?;
            println!("{pad}event {id:08x} kind {} vol {} [{pkg}]", e.kind, e.volume);
            for t in e.targets {
                print_tree(vfs, index, t, depth + 1, seen)?;
            }
        }
        audio::CLASS_HEADER => {
            let hd = audio::parse_header(b)?;
            println!("{pad}{} [{pkg}]", describe_header(&hd));
            for t in hd.children() {
                print_tree(vfs, index, t, depth + 1, seen)?;
            }
        }
        c => println!("{pad}{id:08x} class {c} [{pkg}]"),
    }
    Ok(())
}

fn survey(vfs: &Vfs, decode: bool) -> Result<()> {
    let files = sound_files(vfs);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes: BTreeMap<u8, usize> = BTreeMap::new();
    let mut htypes: BTreeMap<u32, usize> = BTreeMap::new();
    let mut ekinds: BTreeMap<u32, usize> = BTreeMap::new();
    let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
    let mut sources: BTreeMap<String, usize> = BTreeMap::new();
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    let mut ima_versions: BTreeMap<String, usize> = BTreeMap::new();
    let mut dec_ok: BTreeMap<String, usize> = BTreeMap::new();
    let mut dec_err: BTreeMap<String, usize> = BTreeMap::new();
    let mut err_examples: Vec<String> = Vec::new();
    let mut parse_err = 0usize;
    let mut total_secs: BTreeMap<String, f64> = BTreeMap::new();
    let mut wild: Vec<String> = Vec::new();
    let mut all_ids: HashSet<u32> = HashSet::new();
    let mut child_refs: Vec<(u32, String)> = Vec::new();
    let mut qc = Qc::default();

    for (n, (h, name)) in files.iter().enumerate() {
        if n % 2000 == 0 {
            eprintln!("{n}/{}", files.len());
        }
        let bytes = match vfs.read_hash(*h) {
            Ok(b) => b,
            Err(e) => {
                parse_err += 1;
                err_examples.push(format!("{name}: read {e:#}"));
                continue;
            }
        };
        let kind = if Spk::is_spk(&bytes) {
            format!("spk v{}", bytes[0])
        } else if bytes.len() > 0x28 {
            format!("bao class {}", bytes[0x23] >> 4)
        } else {
            "other".into()
        };
        *kinds.entry(kind).or_default() += 1;
        let baos = match audio::parse_container(&bytes, audio::id_from_path(name).unwrap_or(0)) {
            Ok(b) => b,
            Err(e) => {
                parse_err += 1;
                if err_examples.len() < 30 {
                    err_examples.push(format!("{name}: {e:#}"));
                }
                continue;
            }
        };
        for b in &baos {
            all_ids.insert(b.id);
            *classes.entry(b.class).or_default() += 1;
            *versions.entry(format!("{:02x}.{:06x}", b.format, b.version)).or_default() += 1;
            match b.class {
                audio::CLASS_EVENT => {
                    if let Ok(e) = audio::parse_event(b) {
                        *ekinds.entry(e.kind).or_default() += 1;
                        for t in e.targets {
                            child_refs.push((t, name.clone()));
                        }
                    }
                }
                audio::CLASS_HEADER => {
                    let ht = u32::from_le_bytes(b.payload()[4..8].try_into().unwrap());
                    *htypes.entry(ht).or_default() += 1;
                    let hd = match audio::parse_header(b) {
                        Ok(x) => x,
                        Err(e) => {
                            if err_examples.len() < 30 {
                                err_examples.push(format!("{name} {:08x}: {e:#}", b.id));
                            }
                            continue;
                        }
                    };
                    for t in hd.children() {
                        child_refs.push((t, name.clone()));
                    }
                    let (desc, src, secs) = match &hd {
                        Header::Audio(a) => (
                            format!("t1 {} {}ch {}Hz", a.codec.name(), a.channels, a.sample_rate),
                            Some(a.source),
                            a.num_samples as f64 / a.sample_rate.max(1) as f64,
                        ),
                        Header::Layer(l) => {
                            let c: Vec<String> = l.layers.iter().map(|x| format!("{}/{}ch/{}", x.codec.name(), x.channels, x.sample_rate)).collect();
                            let secs = l.layers.first().map(|x| x.num_samples as f64 / x.sample_rate.max(1) as f64).unwrap_or(0.0);
                            (format!("t6 x{} {}", l.layers.len(), dedup(&c)), Some(l.source), secs)
                        }
                        _ => continue,
                    };
                    let codec_key = desc.split(' ').take(2).collect::<Vec<_>>().join(" ");
                    *codecs.entry(desc.clone()).or_default() += 1;
                    *total_secs.entry(codec_key.clone()).or_default() += secs;
                    if let Some(s) = src {
                        let k = format!(
                            "t{} {}{}",
                            hd.kind(),
                            if s.inline { "inline" } else if s.is_stream { "stream" } else { "memory" },
                            if s.prefetch_size > 0 { "+prefetch" } else { "" }
                        );
                        *sources.entry(k).or_default() += 1;
                        if wild.len() < 10 && matches!(&hd, Header::Audio(a) if a.channels > 2 || a.sample_rate > 48000 || a.sample_rate < 8000) {
                            wild.push(format!("{name}: {}", describe_header(&hd)));
                        }
                        // codec-level header peek for memory IMA
                        if let Header::Audio(a) = &hd {
                            if matches!(a.codec, Codec::UbiIma | Codec::UbiImaSeek) && !s.is_stream {
                                if let Some(m) = baos.iter().find(|x| x.id == s.resource_id && x.class == audio::CLASS_MEMORY) {
                                    let p = m.payload();
                                    let key = format!(
                                        "{} v{} lay{:08x} ch{}",
                                        a.codec.name(),
                                        p.first().copied().unwrap_or(0),
                                        u32::from_le_bytes(p[0..4].try_into().unwrap_or([0; 4])),
                                        a.channels
                                    );
                                    *ima_versions.entry(key).or_default() += 1;
                                    // size consistency: header + PCM preamble + nibbles must equal the stored size
                                    if let Ok(ih) = audio::parse_ubi_ima_header(p) {
                                        let ch = a.channels as usize;
                                        let n = a.num_samples as usize - ih.pcm_samples.min(a.num_samples as usize);
                                        let expect = if !ih.seek.is_empty() {
                                            ih.seek.last().unwrap().1 as usize
                                        } else {
                                            ih.data_offset + ih.pcm_samples * 2 * ch + if ch == 1 { n.div_ceil(2) } else { n }
                                        };
                                        let diff = a.source.size as i64 - expect as i64;
                                        *qc.ima_size_diff.entry(diff.clamp(-9, 9)).or_default() += 1;
                                    }
                                }
                            }
                        }
                    }
                    if decode {
                        match audio::decode_header(b, &baos, &mut VfsFiles(vfs)) {
                            Ok(v) => {
                                *dec_ok.entry(codec_key).or_default() += 1;
                                for snd in &v {
                                    qc.check(name, snd);
                                }
                            }
                            Err(e) => {
                                *dec_err.entry(codec_key).or_default() += 1;
                                if err_examples.len() < 60 {
                                    err_examples.push(format!("{name} {:08x} decode: {e:#}", b.id));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let missing: Vec<&(u32, String)> = child_refs.iter().filter(|(t, _)| !all_ids.contains(t)).collect();
    println!("files: {} ({} unparsable)", files.len(), parse_err);
    println!("file kinds: {kinds:?}");
    println!("BAO versions: {versions:?}");
    println!("BAO classes: {classes:?}");
    println!("event kinds: {ekinds:?}");
    println!("header types: {htypes:?}");
    println!("data sources: {sources:?}");
    println!("codec/layout histogram:");
    for (k, v) in &codecs {
        println!("  {v:6}  {k}");
    }
    println!("total header duration (s): {:?}", total_secs.iter().map(|(k, v)| (k.clone(), v.round())).collect::<Vec<_>>());
    println!("IMA memory header peek: {ima_versions:?}");
    println!("refs: {} total, {} to ids not in any package (e.g. {:?})", child_refs.len(), missing.len(), &missing[..missing.len().min(5)]);
    println!("IMA memory size (stored - expected) histogram: {:?}", qc.ima_size_diff);
    if decode {
        println!("decode ok: {dec_ok:?}");
        println!("decode err: {dec_err:?}");
        qc.report();
    }
    for w in wild {
        println!("unusual: {w}");
    }
    for e in err_examples {
        println!("ERR {e}");
    }
    Ok(())
}

fn dedup(v: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        if !out.contains(s) {
            out.push(s.clone());
        }
    }
    out.join("+")
}

/// Quality checks over decoded sounds.
#[derive(Default)]
struct Qc {
    ima_size_diff: BTreeMap<i64, usize>,
    pcm: usize,
    pcm_len_mismatch: usize,
    pcm_secs: f64,
    silent: usize,
    clipped: usize,
    noisy: usize,
    rms_hist: BTreeMap<String, usize>,
    zcr_hist: BTreeMap<String, usize>,
    ogg: usize,
    ogg_secs: f64,
    ogg_bad: usize,
    ogg_crc: usize,
    ogg_trailing: usize,
    ogg_fmt_mismatch: usize,
    ogg_len_mismatch: usize,
    examples: Vec<String>,
}

impl Qc {
    fn note(&mut self, s: String) {
        if self.examples.len() < 40 {
            self.examples.push(s);
        }
    }

    fn check(&mut self, name: &str, s: &audio::DecodedSound) {
        let tag = format!("{name} {:08x}{}", s.header_id, s.layer.map(|l| format!(" L{}", l.0)).unwrap_or_default());
        match &s.data {
            SoundData::Pcm { sample_rate, channels, samples } => {
                self.pcm += 1;
                let st = pcm_stats(samples, *channels as usize, *sample_rate);
                self.pcm_secs += st.secs;
                if samples.len() != s.num_samples as usize * *channels as usize {
                    self.pcm_len_mismatch += 1;
                    self.note(format!("pcm length {} != {}x{} {tag}", samples.len(), s.num_samples, channels));
                }
                let rb = match st.rms {
                    r if r == 0.0 => "0 silent",
                    r if r < 0.001 => "<0.001",
                    r if r < 0.01 => "<0.01",
                    r if r < 0.05 => "<0.05",
                    r if r < 0.15 => "<0.15",
                    r if r < 0.3 => "<0.30",
                    _ => ">=0.30",
                };
                *self.rms_hist.entry(rb.into()).or_default() += 1;
                let zb = format!("{:.1}", (st.zcr * 10.0).floor() / 10.0);
                *self.zcr_hist.entry(zb).or_default() += 1;
                if st.rms == 0.0 {
                    self.silent += 1;
                }
                if st.clip_frac > 0.001 {
                    self.clipped += 1;
                    self.note(format!("clipping {:.4} rms {:.3} {tag}", st.clip_frac, st.rms));
                }
                if st.zcr > 0.35 && st.rms > 0.05 {
                    self.noisy += 1;
                    self.note(format!("noise-like zcr {:.3} rms {:.3} {tag}", st.zcr, st.rms));
                }
            }
            SoundData::Ogg(b) => {
                self.ogg += 1;
                match audio::ogg_info(b) {
                    Ok(i) => {
                        self.ogg_secs += i.last_granule as f64 / i.sample_rate.max(1) as f64;
                        if i.bad_crc > 0 {
                            self.ogg_crc += 1;
                            self.note(format!("ogg {} bad CRC pages {tag}", i.bad_crc));
                        }
                        if i.trailing > 0 {
                            self.ogg_trailing += 1;
                        }
                        if i.channels as u32 != s.channels || i.sample_rate != s.sample_rate {
                            self.ogg_fmt_mismatch += 1;
                            self.note(format!("ogg fmt {}ch {}Hz vs header {}ch {}Hz {tag}", i.channels, i.sample_rate, s.channels, s.sample_rate));
                        }
                        let d = (i.last_granule as i64 - s.num_samples as i64).abs();
                        if d > 4096 {
                            self.ogg_len_mismatch += 1;
                            self.note(format!("ogg granule {} vs header {} {tag}", i.last_granule, s.num_samples));
                        }
                    }
                    Err(e) => {
                        self.ogg_bad += 1;
                        self.note(format!("ogg invalid: {e:#} {tag}"));
                    }
                }
            }
        }
    }

    fn report(&self) {
        println!(
            "PCM sounds: {} ({:.0} s); length mismatches {}; all-zero {}; clipping>0.1% {}; noise-like {}",
            self.pcm, self.pcm_secs, self.pcm_len_mismatch, self.silent, self.clipped, self.noisy
        );
        println!("  RMS histogram: {:?}", self.rms_hist);
        println!("  zero-crossing-rate histogram: {:?}", self.zcr_hist);
        println!(
            "Ogg sounds: {} ({:.0} s); invalid {}; bad CRC {}; trailing bytes {}; fmt mismatch {}; length mismatch {}",
            self.ogg, self.ogg_secs, self.ogg_bad, self.ogg_crc, self.ogg_trailing, self.ogg_fmt_mismatch, self.ogg_len_mismatch
        );
        for e in &self.examples {
            println!("  QC {e}");
        }
    }
}

/// Compact description of an event/header id: root kind, voices, variants, codecs, durations.
fn summarize(g: &audio::SoundGraph, id: u32) -> String {
    let root = if let Some(e) = g.events.get(&id) {
        format!("event{}", e.kind)
    } else if let Some(h) = g.headers.get(&id) {
        format!("header{}", h.kind())
    } else {
        return "missing".into();
    };
    // walk for container kinds and switch groups
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut groups: Vec<String> = Vec::new();
    let mut stack = vec![(id, 0)];
    let mut seen = HashSet::new();
    while let Some((cur, d)) = stack.pop() {
        if d > 16 || !seen.insert(cur) {
            continue;
        }
        if let Some(e) = g.events.get(&cur) {
            *kinds.entry(format!("ev{}", e.kind)).or_default() += 1;
            if e.kind == 2 {
                continue;
            }
            if let Some((grp, _)) = &e.switch {
                groups.push(format!("{grp:08x}"));
            }
            stack.extend(e.targets.iter().map(|t| (*t, d + 1)));
        } else if let Some(h) = g.headers.get(&cur) {
            let k = match h {
                Header::Audio(_) => "audio",
                Header::Layer(_) => "layer",
                Header::Random { .. } => "random",
                Header::Sequence { .. } => "seq",
                Header::Silence { .. } => "silence",
                Header::Switch { .. } => "switch",
                Header::Other { kind: 2, .. } => "chain",
                Header::Other { kind: 7, .. } => "blend",
                Header::Other { .. } => "other",
            };
            *kinds.entry(k.into()).or_default() += 1;
            if let Header::Switch { group, .. } = h {
                groups.push(format!("{group:08x}"));
            }
            stack.extend(h.children().into_iter().map(|t| (t, d + 1)));
        }
    }
    groups.sort();
    groups.dedup();
    let vars = g.resolve(id);
    let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
    let (mut dmin, mut dmax, mut looping) = (f32::MAX, 0f32, false);
    for v in &vars {
        match g.headers.get(&v.header_id) {
            Some(Header::Audio(a)) => {
                *codecs.entry(format!("{}/{}ch/{}", a.codec.name(), a.channels, a.sample_rate)).or_default() += 1;
                let d = a.num_samples as f32 / a.sample_rate.max(1) as f32;
                dmin = dmin.min(d);
                dmax = dmax.max(d);
                looping |= a.looping;
            }
            Some(Header::Layer(l)) => {
                *codecs.entry(format!("layers{}", l.layers.len())).or_default() += 1;
                let d = l.layers.first().map(|x| x.num_samples as f32 / x.sample_rate.max(1) as f32).unwrap_or(0.0);
                dmin = dmin.min(d);
                dmax = dmax.max(d);
                looping |= l.looping;
            }
            _ => {}
        }
    }
    if vars.is_empty() {
        dmin = 0.0;
    }
    format!(
        "{root}\tvariants={}\tdur={:.2}-{:.2}s\t{}\tkinds={}\tswitch={}\tcodecs={}",
        vars.len(),
        dmin,
        dmax,
        if looping { "loop" } else { "oneshot" },
        kinds.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(","),
        groups.join(","),
        codecs.iter().map(|(k, v)| format!("{k}x{v}")).collect::<Vec<_>>().join(",")
    )
}
