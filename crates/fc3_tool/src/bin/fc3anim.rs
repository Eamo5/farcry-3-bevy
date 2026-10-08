//! Skeleton / skinning / animation explorer.
//!
//! ```text
//! fc3anim skeleton <path.skeleton>
//! fc3anim nodes <model.xbg>
//! fc3anim skin <model.xbg> [lod]
//! fc3anim clip <path.mab> [skeleton] [samples]
//! fc3anim survey [path-substring]            all .mab files: variants + errors
//! fc3anim skin-survey [path-substring]       all .xbg files: skin binding checks
//! fc3anim find <substring>...                list .mab files matching all substrings
//! fc3anim bindpose-obj <xbg[,xbg..]> <out.obj> [--lod N]
//! fc3anim pose-obj <xbg[,xbg..]> <clip.mab> <time|frac%> <out.obj>
//!         [--skeleton P] [--lod N] [--root-motion]
//!         [--weapon XBG --weapon-skeleton P [--attach BONE]] [--additive CLIP[@time]]
//! fc3anim skeleton-obj <skeleton> <out.obj> [clip time]
//! ```
//! Paths are archive paths (forward slashes accepted).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fc3_formats::anim::{self, Clip, Mat4, Skeleton, SkinInfo};
use fc3_formats::{find_install, mesh, Vfs};

const HUMAN_SKELETON: &str = r"graphics\actors_\_skeleton_master\hips_ref.skeleton";

fn filelist_dir() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists");
    if p.exists() {
        p
    } else {
        PathBuf::from("data/filelists")
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            if args[i] != "--root-motion" {
                i += 1;
            }
        } else {
            out.push(args[i].as_str());
        }
        i += 1;
    }
    out
}

fn fmt_q(q: [f32; 4]) -> String {
    format!("({:+.3},{:+.3},{:+.3},{:+.3})", q[0], q[1], q[2], q[3])
}
fn fmt_v(v: [f32; 3]) -> String {
    format!("({:+.3},{:+.3},{:+.3})", v[0], v[1], v[2])
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = find_install().context("Far Cry 3 install not found; set FC3_DATA")?;
    let vfs = Vfs::open_install(&data, Some(&filelist_dir()))?;
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = &args[1.min(args.len())..];
    match cmd {
        "skeleton" => cmd_skeleton(&vfs, rest),
        "nodes" => cmd_nodes(&vfs, rest),
        "skin" => cmd_skin(&vfs, rest),
        "clip" => cmd_clip(&vfs, rest),
        "survey" => cmd_survey(&vfs, rest),
        "skin-survey" => cmd_skin_survey(&vfs, rest),
        "find" => {
            let mut v: Vec<&String> = vfs
                .names
                .values()
                .filter(|n| n.ends_with(".mab") && rest.iter().all(|s| n.contains(&s.to_ascii_lowercase())))
                .collect();
            v.sort();
            for n in v {
                println!("{n}");
            }
            Ok(())
        }
        "bindpose-obj" => cmd_pose_obj(&vfs, rest, true),
        "pose-obj" => cmd_pose_obj(&vfs, rest, false),
        "bones" => cmd_bones(&vfs, rest),
        "skeleton-obj" => cmd_skeleton_obj(&vfs, rest),
        _ => {
            println!("{}", include_str!("fc3anim.rs").lines().skip(2).take(15).map(|l| l.trim_start_matches("//! ")).collect::<Vec<_>>().join("\n"));
            Ok(())
        }
    }
}

fn load_skeleton(vfs: &Vfs, path: &str) -> Result<Skeleton> {
    anim::parse_skeleton(&vfs.read(path)?).with_context(|| format!("parsing {path}"))
}

fn cmd_skeleton(vfs: &Vfs, a: &[String]) -> Result<()> {
    let path = a.first().context("usage: skeleton <path>")?;
    let s = load_skeleton(vfs, path)?;
    println!("{} bones, {} sockets", s.bones.len(), s.sockets.len());
    for (i, b) in s.bones.iter().enumerate() {
        let proc_ = match &b.procedural {
            anim::Procedural::None => String::new(),
            anim::Procedural::Twist { driver, weight } => format!("  twist <- {driver} x{weight}"),
            anim::Procedural::Blend(d) => format!("  blend <- {d:?}"),
        };
        println!(
            "{i:3} {:28} parent {:3} hash {:08X} t {} r {}{proc_}",
            b.name,
            b.parent,
            b.name_hash,
            fmt_v(b.translation),
            fmt_q(b.rotation)
        );
    }
    for s in &s.sockets {
        println!("socket {:28} on {:24} t {} r {}", s.name, s.bone, fmt_v(s.translation), fmt_q(s.rotation));
    }
    Ok(())
}

fn cmd_nodes(vfs: &Vfs, a: &[String]) -> Result<()> {
    let path = a.first().context("usage: nodes <xbg>")?;
    let nodes = anim::parse_nodes(&vfs.read(path)?)?;
    let w = anim::bind_worlds(&nodes);
    for (i, n) in nodes.iter().enumerate() {
        println!(
            "{i:3} {:28} parent {:3} bind#{:3} local t {} r {} s {}  world {}",
            n.name,
            n.parent,
            n.bind_index,
            fmt_v(n.translation),
            fmt_q(n.rotation),
            fmt_v(n.scale),
            fmt_v([w[i][12], w[i][13], w[i][14]])
        );
    }
    Ok(())
}

fn max_abs_diff(a: &Mat4, b: &Mat4) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn cmd_skin(vfs: &Vfs, a: &[String]) -> Result<()> {
    let path = a.first().context("usage: skin <xbg> [lod]")?;
    let lod: usize = a.get(1).map(|s| s.parse()).transpose()?.unwrap_or(0);
    let bytes = vfs.read(path)?;
    let s = anim::skin_info(&bytes)?;
    let mats = mesh::read_material_refs(&bytes).unwrap_or_default();
    println!("{} nodes, {} LODs", s.nodes.len(), s.lods.len());
    let mut worst: f32 = 0.0;
    for (i, n) in s.nodes.iter().enumerate() {
        if n.bind_index >= 0 {
            let p = anim::mat_mul(&s.bind_world[i], &s.inverse_bind[i]);
            worst = worst.max(max_abs_diff(&p, &anim::MAT4_IDENTITY));
        }
    }
    println!("max |bind_world * O2BM - I| = {worst:.5}");
    let l = s.lods.get(lod).context("no such LOD")?;
    for p in &l.primitives {
        let pal: Vec<String> = p.palette.iter().map(|&n| s.nodes.get(n as usize).map(|x| x.name.clone()).unwrap_or(format!("#{n}"))).collect();
        println!(
            "prim buf {} part {} cluster {} material {} ({}) idx {}+{} verts {}+{} {}",
            p.buffer,
            p.part,
            p.cluster,
            p.material,
            mats.get(p.material as usize).map(|m| m.name.as_str()).unwrap_or("?"),
            p.index_start,
            p.index_count,
            p.vertex_start,
            p.vertex_count,
            match p.rigid_node {
                Some(n) => format!("rigid -> {}", s.nodes[n as usize].name),
                None => format!("palette {pal:?}"),
            }
        );
    }
    Ok(())
}

fn cmd_clip(vfs: &Vfs, a: &[String]) -> Result<()> {
    let path = a.first().context("usage: clip <mab> [skeleton] [samples]")?;
    let mut c = anim::parse_clip(&vfs.read(path)?)?;
    let skel_path = a.get(1).map(String::as_str).filter(|s| *s != "-").unwrap_or(HUMAN_SKELETON);
    let skel = if c.skeleton_id == anim::SKELETON_HUMAN || a.get(1).is_some() { load_skeleton(vfs, skel_path).ok() } else { None };
    if let Some(s) = &skel {
        c.resolve(s);
    }
    let samples: usize = a.get(2).map(|s| s.parse()).transpose()?.unwrap_or(3);
    print_clip(&c, skel.as_ref(), samples, 0);
    Ok(())
}

fn print_clip(c: &Clip, skel: Option<&Skeleton>, samples: usize, depth: usize) {
    let ind = "  ".repeat(depth);
    let nrot = c.tracks.iter().filter(|t| t.rotations.len() > 1).count();
    let ntr = c.tracks.iter().filter(|t| t.translations.len() > 1).count();
    println!(
        "{ind}skeleton {:08X} flags {} additive {} duration {:.3}s rate {:.2}fps tracks {} (animated rot {nrot}, animated trans {ntr}) ref r {} t {}",
        c.skeleton_id,
        c.flags,
        c.is_additive(),
        c.duration,
        c.frame_rate,
        c.tracks.len(),
        fmt_q(c.reference_rotation),
        fmt_v(c.reference_translation)
    );
    if let Some(r) = &c.root_motion {
        let (t, q) = r.sample(c.duration);
        println!("{ind}root motion: {} keys, end displacement {} rotation {}", r.times.len(), fmt_v(t), fmt_q(q));
        println!("{ind}  starts at {} facing {}", fmt_v(r.start_translation), fmt_q(r.start_rotation));
    }
    for at in &c.attachments {
        println!("{ind}attachment kind {} object '{}' bone '{}' target {:?}", at.kind, at.object, at.bone, at.target);
    }
    for (t, e) in &c.sounds {
        println!("{ind}sound {e:08x} at {t:.3}s");
    }
    for t in &c.tracks {
        let name = skel.and_then(|s| s.bones.get(t.bone as usize)).map(|b| b.name.as_str()).unwrap_or("?");
        let mut line = format!("{ind}  bone {:3} {:24} hash {:08X} rotkeys {:3} transkeys {:3}", t.bone, name, t.bone_hash, t.rotations.len(), t.translations.len());
        for k in 0..samples {
            let time = if samples > 1 { c.duration * k as f32 / (samples - 1) as f32 } else { 0.0 };
            if let Some(q) = t.sample_rotation(time) {
                let _ = write!(line, " r@{time:.2}={}", fmt_q(q));
            }
            if let Some(v) = t.sample_translation(time) {
                let _ = write!(line, " t@{time:.2}={}", fmt_v(v));
            }
        }
        println!("{line}");
    }
    for (i, s) in c.sub_clips.iter().enumerate() {
        println!("{ind}sub-clip {i}:");
        print_clip(s, None, samples.min(2), depth + 1);
    }
}

fn cmd_survey(vfs: &Vfs, a: &[String]) -> Result<()> {
    let filter = a.first().map(|s| s.to_ascii_lowercase());
    let mut names: Vec<(&u64, &String)> = vfs
        .names
        .iter()
        .filter(|(_, n)| n.ends_with(".mab") && filter.as_ref().is_none_or(|f| n.contains(f.as_str())))
        .collect();
    names.sort_by(|a, b| a.1.cmp(b.1));
    let (mut ok, mut errs, mut missing) = (0usize, 0usize, 0usize);
    let mut flags: BTreeMap<u8, usize> = BTreeMap::new();
    let mut skels: BTreeMap<u32, (usize, String)> = BTreeMap::new();
    let mut sub_skels: BTreeMap<u32, usize> = BTreeMap::new();
    let mut err_kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut keys, mut root, mut subs) = (0usize, 0usize, 0usize);
    let mut max_bone = 0u16;
    let mut additive: BTreeMap<(bool, bool), usize> = BTreeMap::new();
    let mut additive_examples: Vec<String> = Vec::new();
    for (h, name) in names {
        let bytes = match vfs.read_hash(*h) {
            Ok(b) => b,
            Err(_) => {
                missing += 1;
                continue;
            }
        };
        match anim::parse_clip(&bytes) {
            Ok(c) => {
                ok += 1;
                *flags.entry(c.flags).or_default() += 1;
                skels.entry(c.skeleton_id).or_insert((0, name.clone())).0 += 1;
                for s in &c.sub_clips {
                    *sub_skels.entry(s.skeleton_id).or_default() += 1;
                }
                subs += c.sub_clips.len();
                keys += c.tracks.iter().map(|t| t.rotations.len() + t.translations.len()).sum::<usize>();
                root += c.root_motion.is_some() as usize;
                if c.skeleton_id == anim::SKELETON_HUMAN {
                    let named = name.rsplit(['/', char::from(92)]).next().unwrap_or("").contains("add");
                    *additive.entry((named, c.is_additive())).or_default() += 1;
                    if named != c.is_additive() && additive_examples.len() < 6 {
                        additive_examples.push(name.clone());
                    }
                }
                if c.skeleton_id == anim::SKELETON_HUMAN {
                    max_bone = max_bone.max(c.tracks.iter().map(|t| t.bone).max().unwrap_or(0));
                }
            }
            Err(e) => {
                errs += 1;
                let k = format!("{e:#}").split(':').next().unwrap_or("").to_string();
                err_kinds.entry(k).or_insert((0, name.clone())).0 += 1;
            }
        }
    }
    println!("{ok} clips parsed, {errs} errors, {missing} listed but absent");
    println!("flags (AnD byte): {flags:?}");
    println!("clips with root motion: {root}; nested sub-clips: {subs}; total keys: {keys}");
    println!("max human bone index animated: {max_bone}");
    println!("human clips (name contains 'add', is_additive()): {additive:?}");
    for e in &additive_examples {
        println!("  disagreement: {e}");
    }
    println!("main skeleton ids (top 12):");
    let mut v: Vec<_> = skels.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1 .0));
    for (id, (n, ex)) in v.iter().take(12) {
        println!("  {id:08X} x{n:5}  e.g. {ex}");
    }
    let mut v: Vec<_> = sub_skels.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("sub-clip skeleton ids (top 6): {:?}", v.iter().take(6).map(|(k, n)| format!("{k:08X} x{n}")).collect::<Vec<_>>());
    for (k, (n, ex)) in err_kinds {
        println!("  error x{n}: {k}  e.g. {ex}");
    }
    Ok(())
}

fn cmd_skin_survey(vfs: &Vfs, a: &[String]) -> Result<()> {
    let filter = a.first().map(|s| s.to_ascii_lowercase());
    let mut names: Vec<(&u64, &String)> = vfs
        .names
        .iter()
        .filter(|(_, n)| n.ends_with(".xbg") && filter.as_ref().is_none_or(|f| n.contains(f.as_str())))
        .collect();
    names.sort_by(|a, b| a.1.cmp(b.1));
    let (mut ok, mut errs, mut prims, mut rigid, mut fallback, mut bad_bind, mut mat_differs) = (0, 0, 0, 0, 0, 0, 0);
    let mut first_err = BTreeMap::new();
    for (h, name) in names {
        let Ok(bytes) = vfs.read_hash(*h) else { continue };
        match anim::skin_info_lods(&bytes, 1) {
            Ok(s) => {
                ok += 1;
                for (i, n) in s.nodes.iter().enumerate() {
                    if n.bind_index >= 0 {
                        let p = anim::mat_mul(&s.bind_world[i], &s.inverse_bind[i]);
                        if max_abs_diff(&p, &anim::MAT4_IDENTITY) > 0.02 {
                            bad_bind += 1;
                            first_err.entry("bind mismatch".to_string()).or_insert(name.clone());
                        }
                    }
                }
                for p in &s.lods[0].primitives {
                    prims += 1;
                    rigid += p.rigid_node.is_some() as usize;
                    mat_differs += (p.material != p.cluster) as usize;
                    if p.rigid_node == Some(0) && s.nodes.len() > 1 {
                        fallback += 1;
                    }
                }
            }
            Err(e) => {
                errs += 1;
                first_err.entry(format!("{e:#}")).or_insert(name.clone());
            }
        }
    }
    println!("{ok} models ok, {errs} errors; LOD0 primitives {prims}, rigid {rigid} (bound to root node: {fallback}), material != cluster index: {mat_differs}, O2BM/NODE mismatches: {bad_bind}");
    for (e, n) in first_err.iter().take(20) {
        println!("  {e}  e.g. {n}");
    }
    Ok(())
}

struct PosedPart {
    name: String,
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn load_part(vfs: &Vfs, path: &str, lod: usize) -> Result<(mesh::Model, SkinInfo)> {
    let bytes = vfs.read(path)?;
    let m = mesh::decode(&bytes, lod + 1).with_context(|| format!("decoding {path}"))?;
    let s = anim::skin_info_lods(&bytes, lod + 1)?;
    Ok((m, s))
}

fn pose_part(m: &mesh::Model, s: &SkinInfo, lod: usize, worlds: &[Mat4], name: &str) -> Result<PosedPart> {
    let (_, md) = m.lods.get(lod).context("LOD missing")?;
    let sl = s.lods.get(lod).context("LOD missing")?;
    if sl.joints.len() != md.positions.len() {
        bail!("{name}: vertex count mismatch {} vs {}", sl.joints.len(), md.positions.len());
    }
    let sm = anim::skinning_matrices(worlds, &s.inverse_bind);
    Ok(PosedPart { name: name.to_string(), positions: anim::skin_positions(&md.positions, sl, &sm), indices: md.indices.clone() })
}

fn write_obj(path: &str, parts: &[PosedPart]) -> Result<()> {
    let mut out = String::new();
    let mut base = 1usize;
    for p in parts {
        let _ = writeln!(out, "o {}", p.name);
        for v in &p.positions {
            let _ = writeln!(out, "v {:.5} {:.5} {:.5}", v[0], v[1], v[2]);
        }
        for t in p.indices.chunks_exact(3) {
            let _ = writeln!(out, "f {} {} {}", t[0] as usize + base, t[1] as usize + base, t[2] as usize + base);
        }
        base += p.positions.len();
    }
    std::fs::write(path, out)?;
    let nv: usize = parts.iter().map(|p| p.positions.len()).sum();
    let nt: usize = parts.iter().map(|p| p.indices.len() / 3).sum();
    println!("wrote {path}: {} parts, {nv} vertices, {nt} triangles", parts.len());
    Ok(())
}

fn cmd_pose_obj(vfs: &Vfs, a: &[String], bind: bool) -> Result<()> {
    let pos = positional(a);
    let lod: usize = opt(a, "--lod").map(|s| s.parse()).transpose()?.unwrap_or(0);
    let (models, clip_path, time_s, out) = if bind {
        (pos.first().context("usage: bindpose-obj <xbg[,xbg]> <out.obj>")?, None, None, pos.get(1).context("out.obj")?)
    } else {
        (
            pos.first().context("usage: pose-obj <xbg[,xbg]> <clip> <time> <out.obj>")?,
            Some(*pos.get(1).context("clip")?),
            Some(*pos.get(2).context("time")?),
            pos.get(3).context("out.obj")?,
        )
    };
    let clip = match clip_path {
        Some(p) => Some(anim::parse_clip(&vfs.read(p)?)?),
        None => None,
    };
    let skel = load_skeleton(vfs, opt(a, "--skeleton").unwrap_or(HUMAN_SKELETON))?;
    let t = match (time_s, &clip) {
        (Some(s), Some(c)) if s.ends_with('%') => c.duration * s.trim_end_matches('%').parse::<f32>()? / 100.0,
        (Some(s), _) => s.parse::<f32>()?,
        _ => 0.0,
    };
    let root = match (&clip, a.iter().any(|x| x == "--root-motion")) {
        (Some(c), true) => match &c.root_motion {
            Some(r) => {
                let (tr, q) = r.sample(t);
                anim::mat_from_trs(tr, q, [1.0; 3])
            }
            None => anim::MAT4_IDENTITY,
        },
        _ => anim::MAT4_IDENTITY,
    };
    // --additive <clip>[@time]: layer an additive clip on top of the base clip.
    let additive = match opt(a, "--additive") {
        Some(spec) => {
            let (p, at) = spec.split_once('@').unwrap_or((spec, "0"));
            let c = anim::parse_clip(&vfs.read(p)?)?;
            println!("additive layer {p}: is_additive={}", c.is_additive());
            Some((c, at.parse::<f32>()?))
        }
        None => None,
    };
    let mut parts = Vec::new();
    let mut attach_world: Option<Mat4> = None;
    let mut camera_printed = false;
    let attach_bone = opt(a, "--attach").unwrap_or("RightHand");
    for path in models.split(',') {
        let (m, s) = load_part(vfs, path, lod)?;
        let mut locals: Vec<Mat4> = match &clip {
            Some(c) => anim::pose_locals(&s.nodes, &skel, c, t),
            None => s.nodes.iter().map(anim::node_local).collect(),
        };
        if let Some((ac, at)) = &additive {
            anim::apply_additive(&s.nodes, &skel, &mut locals, ac, *at, 1.0);
        }
        let worlds = anim::compose_worlds(&s.nodes, &locals, &root);
        if let Some(i) = s.nodes.iter().position(|n| n.name == attach_bone) {
            attach_world.get_or_insert(worlds[i]);
        }
        if let Some(i) = s.nodes.iter().position(|n| n.name == "Camera") {
            if !camera_printed {
                println!("camera world (column-major): {:?}", worlds[i]);
                camera_printed = true;
            }
        }
        let short = path.rsplit(['/', '\\']).next().unwrap_or(path);
        parts.push(pose_part(&m, &s, lod, &worlds, short)?);
    }
    if let Some(wpath) = opt(a, "--weapon") {
        let (m, s) = load_part(vfs, wpath, lod)?;
        let wskel = match opt(a, "--weapon-skeleton") {
            Some(p) => load_skeleton(vfs, p)?,
            None => Skeleton { bones: vec![], sockets: vec![] },
        };
        let attach = attach_world.with_context(|| format!("no node named {attach_bone} to attach the weapon to"))?;
        let locals: Vec<Mat4> = match clip.as_ref().and_then(|c| c.main_prop_clip()) {
            Some(wc) => anim::pose_locals(&s.nodes, &wskel, wc, t),
            None => s.nodes.iter().map(anim::node_local).collect(),
        };
        let worlds = anim::compose_worlds(&s.nodes, &locals, &attach);
        let short = wpath.rsplit(['/', '\\']).next().unwrap_or(wpath);
        parts.push(pose_part(&m, &s, lod, &worlds, short)?);
    }
    if let Some(c) = &clip {
        println!("clip duration {:.3}s, posed at t={t:.3}s, {} tracks, {} sub-clips", c.duration, c.tracks.len(), c.sub_clips.len());
    }
    write_obj(out, &parts)
}

/// `bones <xbg> <clip> <bone,bone..> [times..] [--root-motion]`: model-space
/// positions of bones over a clip (to line up paired clips).
fn cmd_bones(vfs: &Vfs, a: &[String]) -> Result<()> {
    let pos = positional(a);
    let (model, clip_path, names) = (pos.first().context("usage: bones <xbg> <clip> <bones> [times]")?, *pos.get(1).context("clip")?, pos.get(2).context("bones")?);
    let clip = anim::parse_clip(&vfs.read(clip_path)?)?;
    let skel = load_skeleton(vfs, opt(a, "--skeleton").unwrap_or(HUMAN_SKELETON))?;
    let (_, s) = load_part(vfs, model, 0)?;
    let mut times: Vec<f32> = pos[3..].iter().filter_map(|t| t.parse().ok()).collect();
    if times.is_empty() {
        times = (0..=4).map(|k| clip.duration * k as f32 / 4.0).collect();
    }
    for t in times {
        let root = match (&clip.root_motion, a.iter().any(|x| x == "--root-motion")) {
            (Some(r), true) => {
                let (tr, q) = r.sample(t);
                anim::mat_from_trs(tr, q, [1.0; 3])
            }
            _ => anim::MAT4_IDENTITY,
        };
        let locals = anim::pose_locals(&s.nodes, &skel, &clip, t);
        let worlds = anim::compose_worlds(&s.nodes, &locals, &root);
        let mut line = format!("t={t:.2}");
        for name in names.split(',') {
            if let Some(i) = s.nodes.iter().position(|n| n.name == name) {
                let m = worlds[i];
                let _ = write!(line, "  {name} ({:+.2},{:+.2},{:+.2})", m[12], m[13], m[14]);
            }
        }
        println!("{line}");
    }
    Ok(())
}

fn cmd_skeleton_obj(vfs: &Vfs, a: &[String]) -> Result<()> {
    let path = a.first().context("usage: skeleton-obj <skeleton> <out.obj> [clip time]")?;
    let out = a.get(1).context("out.obj")?;
    let skel = load_skeleton(vfs, path)?;
    // Treat the skeleton as a node list.
    let nodes: Vec<anim::Node> = skel
        .bones
        .iter()
        .map(|b| anim::Node {
            name: b.name.clone(),
            name_hash: b.name_hash,
            parent: b.parent,
            first_child: -1,
            next_sibling: -1,
            translation: b.translation,
            rotation: b.rotation,
            scale: [1.0; 3],
            bind_index: -1,
        })
        .collect();
    let locals: Vec<Mat4> = match (a.get(2), a.get(3)) {
        (Some(c), Some(t)) => {
            let clip = anim::parse_clip(&vfs.read(c)?)?;
            anim::pose_locals(&nodes, &skel, &clip, t.parse()?)
        }
        _ => nodes.iter().map(anim::node_local).collect(),
    };
    let w = anim::compose_worlds(&nodes, &locals, &anim::MAT4_IDENTITY);
    let mut s = String::new();
    for m in &w {
        let _ = writeln!(s, "v {:.5} {:.5} {:.5}", m[12], m[13], m[14]);
    }
    for (i, n) in nodes.iter().enumerate() {
        if n.parent >= 0 {
            let _ = writeln!(s, "l {} {}", n.parent + 1, i + 1);
        }
    }
    std::fs::write(out, s)?;
    println!("wrote {out}: {} joints", w.len());
    Ok(())
}
