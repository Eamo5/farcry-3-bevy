//! `facial` and `facial-survey`: inspect facial clips (`.dpax`), face rigs
//! (`.dpdx`) and the facial pose definition table. Everything printed comes from
//! the local install; nothing is written.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use fc3_formats::anim::{self, quat_conj, quat_mul, Quat};
use fc3_formats::facial::{self, ChannelKind, FaceClip, FaceRig};
use fc3_formats::hash::path_hash;
use fc3_formats::Vfs;

/// The NPC skeleton names most face bones (unique characters add a few more).
const FACE_SKELETON: &str = r"graphics\actors_\_skeleton_master\npc_hips_ref.skeleton";

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn read(vfs: &Vfs, path: &str) -> Result<Vec<u8>> {
    if path.len() == 16 && path.chars().all(|c| c.is_ascii_hexdigit()) {
        return vfs.read_hash(u64::from_str_radix(path, 16)?);
    }
    vfs.read_hash(path_hash(path)).with_context(|| format!("{path} not found"))
}

fn bone_names(vfs: &Vfs) -> HashMap<u32, String> {
    vfs.read_hash(path_hash(FACE_SKELETON))
        .ok()
        .and_then(|b| anim::parse_skeleton(&b).ok())
        .map(|s| s.bones.into_iter().map(|b| (b.name_hash, b.name)).collect())
        .unwrap_or_default()
}

fn angle_deg(q: Quat) -> f32 {
    2.0 * q[3].abs().min(1.0).acos().to_degrees()
}

/// `facial <path> [--rig <dpdx>] [--at <seconds>]`
pub fn run(vfs: &Vfs, args: &[String]) -> Result<()> {
    let path = args.get(1).context("usage: facial <.dpax|.dpdx|.lib path or hash> [--rig <.dpdx>] [--at <seconds>]")?;
    let bytes = read(vfs, path)?;
    match bytes.get(2..4) {
        Some(b"Ap") => print_clip(vfs, &facial::parse_clip(&bytes)?, args),
        Some(b"Dp") => print_rig(vfs, &facial::parse_rig(&bytes)?),
        _ => print_definitions(vfs, &bytes),
    }
}

fn print_clip(vfs: &Vfs, clip: &FaceClip, args: &[String]) -> Result<()> {
    let block_frames = clip.blocks.first().map(|b| b.frame_count).unwrap_or(0);
    println!(
        "clip: {} frames at {} fps ({:.3} s), {} channels, {} blocks of up to {block_frames} frames",
        clip.frame_count,
        clip.frame_rate,
        clip.duration,
        clip.channels.len(),
        clip.blocks.len()
    );
    for (i, b) in clip.blocks.iter().enumerate() {
        let count = |k: ChannelKind| b.kinds.iter().filter(|&&x| x == k).count();
        println!(
            "  block {i:3} frames {:5}..{:5} {:6} bytes  raw {:2} zero {:2} constant {:2} sparse {:2}",
            b.first_frame,
            b.first_frame + b.frame_count - 1,
            b.bytes,
            count(ChannelKind::Raw),
            count(ChannelKind::Zero),
            count(ChannelKind::Constant),
            count(ChannelKind::Sparse)
        );
    }
    let rig = opt(args, "--rig").map(|p| read(vfs, p).and_then(|b| facial::parse_rig(&b))).transpose()?;
    let map = rig.as_ref().map(|r| r.channel_map(clip));
    let n = clip.frame_count;
    println!("channels (pose hash, peak weight, frames above 0.01{}):", if rig.is_some() { ", rig pose" } else { "" });
    for (c, h) in clip.channels.iter().enumerate() {
        let curve = &clip.weights[c * n..][..n];
        let peak = curve.iter().cloned().fold(0.0f32, f32::max);
        let active = curve.iter().filter(|&&w| w > 0.01).count();
        let pose = match map.as_ref().map(|m| m[c]) {
            Some(Some(p)) => format!("  pose {p}"),
            Some(None) => "  (not in rig)".into(),
            None => String::new(),
        };
        println!("  {c:3} {h:08x} peak {peak:.3} active {active:5}{pose}");
    }
    // A few sampled frames: one column per channel, weights in hundredths.
    println!("weights x100 at:");
    print!("  {:>8}", "t");
    for c in 0..clip.channels.len() {
        print!(" {c:3}");
    }
    println!();
    for frac in [0.0f32, 0.1, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9, 1.0] {
        let t = clip.duration * frac;
        print!("  {t:8.3}");
        for w in clip.sample(t) {
            print!(" {:3.0}", w * 100.0);
        }
        println!();
    }
    if let Some(rig) = &rig {
        let t = opt(args, "--at").map(str::parse::<f32>).transpose()?.unwrap_or(clip.duration * 0.5);
        let weights = rig.clip_weights(clip, t);
        let names = bone_names(vfs);
        println!("posed bones at t={t:.3} (offset from rest in mm, rotation from rest in degrees):");
        for (i, b) in rig.bones.iter().enumerate() {
            let (tr, q) = rig.bone_local(i, &weights);
            let d = [tr[0] - b.translation[0], tr[1] - b.translation[1], tr[2] - b.translation[2]];
            let moved = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 1000.0;
            let turned = angle_deg(quat_mul(quat_conj(anim::quat_normalize(b.rotation)), q));
            if moved > 0.05 || turned > 0.05 {
                let name = names.get(&b.name_hash).cloned().unwrap_or_else(|| format!("{:08x}", b.name_hash));
                println!("  {name:20} {moved:6.2} mm {turned:6.2} deg");
            }
        }
    }
    Ok(())
}

fn print_rig(vfs: &Vfs, rig: &FaceRig) -> Result<()> {
    let names = bone_names(vfs);
    let entries: usize = rig.bones.iter().map(|b| b.poses.len()).sum();
    println!("rig: {} bones, {} poses, {entries} bone-pose entries", rig.bones.len(), rig.poses.len());
    println!("bones (rest translation in Bevy space, poses that move it, largest offset in mm and turn in degrees):");
    for b in &rig.bones {
        let name = names.get(&b.name_hash).cloned().unwrap_or_else(|| format!("{:08x}", b.name_hash));
        let far = b.poses.iter().map(|p| (p.translation.iter().map(|v| v * v).sum::<f32>()).sqrt() * 1000.0).fold(0.0f32, f32::max);
        let turn = b.poses.iter().map(|p| angle_deg(p.rotation)).fold(0.0f32, f32::max);
        let t = b.translation;
        println!("  {name:20} rest ({:+.4},{:+.4},{:+.4}) poses {:2} max {far:6.2} mm {turn:6.2} deg", t[0], t[1], t[2], b.poses.len());
    }
    println!("poses (hash, bones moved):");
    for (i, h) in rig.poses.iter().enumerate() {
        let moved = rig.bones.iter().filter(|b| b.poses.iter().any(|p| p.pose as usize == i)).count();
        println!("  {i:3} {h:08x} {moved:3}");
    }
    Ok(())
}

fn print_definitions(vfs: &Vfs, bytes: &[u8]) -> Result<()> {
    let defs = facial::parse_definitions(bytes)?;
    println!("{} facial pose definitions (key, name, rig, rig present in the archives):", defs.len());
    for d in &defs {
        let present = !d.rig.is_empty() && vfs.index.contains_key(&path_hash(&d.rig));
        println!("  {:08x} {:32} {} {}", d.key, d.name, d.rig, if present { "ok" } else { "MISSING" });
    }
    Ok(())
}

/// `facial-survey [path substring]`: parses every named `.dpax` and `.dpdx` and
/// reports errors, sizes, value ranges, smoothness and pose coverage.
pub fn survey(vfs: &Vfs, args: &[String]) -> Result<()> {
    let filter = args.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let mut rigs: Vec<(String, FaceRig)> = Vec::new();
    let mut clips = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut rates: BTreeMap<String, usize> = BTreeMap::new();
    let mut block_frames: BTreeMap<usize, usize> = BTreeMap::new();
    let mut kinds = [0usize; 4];
    let (mut frames, mut samples, mut max_frames) = (0usize, 0usize, 0usize);
    let (mut out_of_range, mut length_mismatch) = (0usize, 0usize);
    let (mut inner_sum, mut inner_n, mut edge_sum, mut edge_n, mut curve_sum) = (0f64, 0usize, 0f64, 0usize, 0f64);
    let mut clip_hashes: HashMap<u32, usize> = HashMap::new();
    let mut clip_channels: Vec<(String, Vec<u32>)> = Vec::new();
    let mut names: Vec<&String> = vfs.names.values().filter(|n| n.contains(&filter)).collect();
    names.sort();
    for name in names {
        let is_clip = name.ends_with(".dpax");
        if !is_clip && !name.ends_with(".dpdx") {
            continue;
        }
        let Ok(bytes) = vfs.read_hash(path_hash(name)) else {
            errors.push(format!("{name}: unreadable"));
            continue;
        };
        if !is_clip {
            match facial::parse_rig(&bytes) {
                Ok(r) => rigs.push((name.clone(), r)),
                Err(e) => errors.push(format!("{name}: {e:#}")),
            }
            continue;
        }
        let clip = match facial::parse_clip(&bytes) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{name}: {e:#}"));
                continue;
            }
        };
        clips += 1;
        clip_channels.push((name.clone(), clip.channels.clone()));
        *rates.entry(format!("{}", clip.frame_rate)).or_default() += 1;
        *block_frames.entry(clip.blocks.iter().map(|b| b.frame_count).max().unwrap_or(0)).or_default() += 1;
        for b in &clip.blocks {
            for k in &b.kinds {
                kinds[*k as usize] += 1;
            }
        }
        let n = clip.frame_count;
        frames += n;
        max_frames = max_frames.max(n);
        if (clip.duration * clip.frame_rate).round() as usize + 1 != n {
            length_mismatch += 1;
        }
        let starts: HashSet<usize> = clip.blocks.iter().skip(1).map(|b| b.first_frame).collect();
        for (c, h) in clip.channels.iter().enumerate() {
            *clip_hashes.entry(*h).or_default() += 1;
            let curve = &clip.weights[c * n..][..n];
            samples += n;
            out_of_range += curve.iter().filter(|w| !(0.0..=1.0).contains(*w)).count();
            for f in 1..n {
                let d = (curve[f] - curve[f - 1]).abs() as f64 * 255.0;
                if starts.contains(&f) {
                    edge_sum += d;
                    edge_n += 1;
                } else {
                    inner_sum += d;
                    inner_n += 1;
                }
                if f + 1 < n {
                    curve_sum += (curve[f + 1] - 2.0 * curve[f] + curve[f - 1]).abs() as f64 * 255.0;
                }
            }
        }
    }
    println!("{clips} clips parsed, {} rigs parsed, {} errors", rigs.len(), errors.len());
    for e in errors.iter().take(20) {
        println!("  {e}");
    }
    println!("frame rates {rates:?}; longest block {block_frames:?}");
    println!("frames {frames} (longest clip {max_frames}); clips whose frame count isn't duration*rate+1: {length_mismatch}");
    println!("channel blocks: raw {} zero {} constant {} sparse {}", kinds[0], kinds[1], kinds[2], kinds[3]);
    println!("samples {samples}, outside 0..=1: {out_of_range}");
    println!(
        "mean |frame-to-frame change| (in 1/255): inside blocks {:.3}, across block boundaries {:.3}; mean |second difference| {:.3}",
        inner_sum / inner_n.max(1) as f64,
        edge_sum / edge_n.max(1) as f64,
        curve_sum / samples.max(1) as f64
    );
    let known: HashSet<u32> = rigs.iter().flat_map(|(_, r)| r.poses.iter().copied()).collect();
    let missing: usize = clip_hashes.iter().filter(|(h, _)| !known.contains(h)).map(|(_, n)| n).sum();
    println!("{} distinct channel hashes in clips, {} distinct poses in rigs; channel uses not found in any rig: {missing}", clip_hashes.len(), known.len());
    let odd: Vec<&String> = clip_channels.iter().filter(|(_, h)| h.iter().any(|h| !known.contains(h))).map(|(n, _)| n).collect();
    println!("clips with channels no rig has: {}", odd.len());
    for n in odd.iter().take(10) {
        println!("  {n}");
    }
    if !rigs.is_empty() {
        let (bones, poses): (Vec<usize>, Vec<usize>) = rigs.iter().map(|(_, r)| (r.bones.len(), r.poses.len())).unzip();
        println!(
            "rig bones {}..{}, rig poses {}..{}",
            bones.iter().min().unwrap(),
            bones.iter().max().unwrap(),
            poses.iter().min().unwrap(),
            poses.iter().max().unwrap()
        );
    }
    Ok(())
}
