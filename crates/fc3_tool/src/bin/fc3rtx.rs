//! Far Cry 3 RealTree (`.rtx`) explorer.
//!
//! ```text
//! fc3rtx list [filter]                  one summary line per tree (all trees if no filter)
//! fc3rtx info <filter>                  details for the matching trees: skeleton, leaf types,
//!                                       materials and their textures, built parts, bounds
//! fc3rtx check                          parse every .rtx, report failures and bounds mismatches
//! fc3rtx obj <filter> <out.obj> [--part branches|leaves|bigleaves] [--lod N] [--yup]
//!                                       export the rebuilt geometry (Dunia Z-up unless --yup)
//! ```
//!
//! `filter` is a case-insensitive substring of the archive path, e.g. `koa_a`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fc3_formats::realtree::{self, MeshOptions, Part, PartKind, RealTree};
use fc3_formats::{find_install, material, Vfs};

fn filelist_dir() -> PathBuf {
    let rel = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists");
    if rel.exists() {
        rel
    } else {
        PathBuf::from("data/filelists")
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data = find_install().context("Far Cry 3 install not found; set FC3_DATA")?;
    let dir = filelist_dir();
    let vfs = Vfs::open_install(&data, dir.exists().then_some(dir.as_path()))?;
    let mut paths: Vec<String> = vfs.names.values().filter(|n| n.ends_with(".rtx")).cloned().collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no .rtx names known; is data/filelists present?");
    }
    // A filter equal to a file stem (e.g. `rt_koa_a`) selects just that file;
    // otherwise it is a substring of the path.
    let matching = |f: Option<&str>| -> Vec<String> {
        let f = f.map(str::to_ascii_lowercase);
        let exact: Vec<String> =
            paths.iter().filter(|p| f.as_deref().is_some_and(|f| stem(p) == f)).cloned().collect();
        if !exact.is_empty() {
            return exact;
        }
        paths.iter().filter(|p| f.as_ref().is_none_or(|f| p.contains(f.as_str()))).cloned().collect()
    };
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "list" => list(&vfs, &matching(args.get(1).map(String::as_str))),
        "info" => {
            let sel = matching(Some(args.get(1).context("usage: info <filter>")?));
            for p in &sel {
                info(&vfs, p)?;
            }
            if sel.is_empty() {
                bail!("no .rtx matches");
            }
            Ok(())
        }
        "check" => check(&vfs, &paths),
        "obj" => {
            let filter = args.get(1).context("usage: obj <filter> <out.obj> [--part P] [--lod N] [--yup]")?;
            let out = args.get(2).context("missing output path")?;
            let mut part = None;
            let mut lod = 0usize;
            let mut yup = false;
            let mut it = args.iter().skip(3);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--part" => part = Some(it.next().context("--part needs a value")?.to_ascii_lowercase()),
                    "--lod" => lod = it.next().context("--lod needs a value")?.parse()?,
                    "--yup" => yup = true,
                    other => bail!("unknown option {other}"),
                }
            }
            let sel = matching(Some(filter));
            let path = match sel.as_slice() {
                [] => bail!("no .rtx matches {filter}"),
                [one] => one.clone(),
                many => bail!("{} files match {filter}; be more specific", many.len()),
            };
            export_obj(&vfs, &path, Path::new(out), part.as_deref(), lod, yup)
        }
        _ => {
            println!(
                "usage: fc3rtx list [filter] | info <filter> | check | obj <filter> <out.obj> [--part branches|leaves|bigleaves] [--lod N] [--yup]"
            );
            Ok(())
        }
    }
}

fn short(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

fn stem(path: &str) -> &str {
    short(path).trim_end_matches(".rtx")
}

fn load(vfs: &Vfs, path: &str) -> Result<RealTree> {
    let bytes = vfs.read(path)?;
    realtree::parse(&bytes).with_context(|| format!("parsing {path}"))
}

fn fmt3(v: [f32; 3]) -> String {
    format!("({:.2}, {:.2}, {:.2})", v[0], v[1], v[2])
}

fn kind_name(k: PartKind) -> &'static str {
    match k {
        PartKind::Branches => "branches",
        PartKind::Leaves => "leaves",
        PartKind::BigLeaves => "bigleaves",
    }
}

fn list(vfs: &Vfs, paths: &[String]) -> Result<()> {
    println!(
        "{:28} {:>4} {:>2} {:>5} {:>4} {:>5} {:>4} {:>7} {:>7} {:>6}  size (x y z)",
        "file", "ver", "k", "nodes", "brch", "cards", "big", "verts", "tris", "height"
    );
    for p in paths {
        match load(vfs, p) {
            Ok(t) => {
                let parts = t.build_parts(&MeshOptions::default());
                let verts: usize = parts.iter().map(|p| p.positions.len()).sum();
                let tris: usize = parts.iter().map(Part::triangle_count).sum();
                let size = [0, 1, 2].map(|k| t.bbox_max[k] - t.bbox_min[k]);
                println!(
                    "{:28} {:>4x} {:>2} {:>5} {:>4} {:>5} {:>4} {:>7} {:>7} {:>6.1}  {:.1} x {:.1} x {:.1}",
                    short(p),
                    t.version,
                    t.kind,
                    t.nodes.len(),
                    t.branches.len(),
                    t.leaves.len(),
                    t.big_leaves.len(),
                    verts,
                    tris,
                    t.bbox_max[2],
                    size[0],
                    size[1],
                    size[2]
                );
            }
            Err(e) => println!("{:28} ERROR {e:#}", short(p)),
        }
    }
    Ok(())
}

fn info(vfs: &Vfs, path: &str) -> Result<()> {
    let t = load(vfs, path)?;
    let mut s = String::new();
    let _ = writeln!(s, "{path}");
    let _ = writeln!(s, "  name {:?}  version {:#x}  kind {}", t.name, t.version, t.kind);
    let _ = writeln!(s, "  stored bbox {} .. {}", fmt3(t.bbox_min), fmt3(t.bbox_max));
    if let Some((mn, mx)) = t.geometry_bounds(&MeshOptions::default()) {
        let _ = writeln!(s, "  rebuilt bbox {} .. {}", fmt3(mn), fmt3(mx));
    }
    let _ = writeln!(s, "  {} nodes, {} branches, {} leaf cards, {} big leaves", t.nodes.len(), t.branches.len(), t.leaves.len(), t.big_leaves.len());
    let max_level = t.branches.iter().map(|b| b.level).max().unwrap_or(0);
    for level in 0..=max_level {
        let bs: Vec<_> = t.branches.iter().filter(|b| b.level == level).collect();
        let len: f32 = bs.iter().map(|b| b.length).sum();
        let r0 = bs.iter().map(|b| t.nodes[b.first_node as usize].radius).fold(0.0f32, f32::max);
        let _ = writeln!(s, "    level {level}: {:3} branches, total length {len:7.1} m, max base radius {r0:.3} m", bs.len());
    }
    if let Some(trunk) = t.branches.first() {
        let _ = writeln!(s, "  trunk (branch 0):");
        for i in trunk.first_node..=trunk.first_node + trunk.segments {
            let n = &t.nodes[i as usize];
            let _ = writeln!(
                s,
                "    node {i:3} pos {} r {:.3} seg {:.2} v {:.2} u-rep {} children {:?} leaves {}",
                fmt3(n.position),
                n.radius,
                n.segment_length,
                n.tex_v,
                n.tex_u_repeat,
                n.children,
                n.leaf_count
            );
        }
    }
    for (i, lt) in t.leaf_types.iter().enumerate() {
        let _ = writeln!(
            s,
            "  leaf type {i}: kind {} uv ({:.3},{:.3})..({:.3},{:.3}) cards {} code {}",
            lt.kind, lt.uv_min[0], lt.uv_min[1], lt.uv_max[0], lt.uv_max[1], lt.count, lt.code
        );
    }
    if !t.leaves.is_empty() {
        let sizes: Vec<f32> = t.leaves.iter().map(|l| l.size).collect();
        let min = sizes.iter().cloned().fold(f32::MAX, f32::min);
        let max = sizes.iter().cloned().fold(0.0, f32::max);
        let _ = writeln!(s, "  leaf card half-diagonal {min:.2} .. {max:.2} m");
    }
    for (i, bl) in t.big_leaves.iter().take(3).enumerate() {
        let lods: Vec<String> =
            bl.lods.iter().map(|l| format!("{}v/{}t", l.positions.len(), l.indices.len() / 3)).collect();
        let _ = writeln!(s, "  big leaf {i}: lods {}", lods.join(", "));
    }
    for m in &t.materials {
        let _ = writeln!(s, "  material slot {} {}", m.slot, m.path);
        match vfs.read(&m.path).and_then(|b| material::parse(&b)) {
            Ok(mat) => {
                let r = realtree::resolve_material(&mat);
                let _ = writeln!(s, "      shader {} ({}), two-sided {}, tiling {:?}", r.shader, mat.name, r.two_sided, r.tiling);
                for (role, tex) in [("diffuse", &r.diffuse), ("normal", &r.normal), ("mask", &r.mask)] {
                    if let Some(tex) = tex {
                        let _ = writeln!(s, "      {role:7} {tex} (exists: {})", vfs.exists(tex));
                    }
                }
            }
            Err(e) => {
                let _ = writeln!(s, "      ERR {e:#}");
            }
        }
    }
    for part in t.build_parts(&MeshOptions::default()) {
        let (mn, mx) = part.bounds().unwrap_or_default();
        let _ = writeln!(
            s,
            "  part {:9} slot {} {:6} verts {:6} tris  bounds {} .. {}{}",
            kind_name(part.kind),
            part.slot,
            part.positions.len(),
            part.triangle_count(),
            fmt3(mn),
            fmt3(mx),
            if part.material.is_none() { "  (no material)" } else { "" }
        );
    }
    print!("{s}");
    Ok(())
}

fn check(vfs: &Vfs, paths: &[String]) -> Result<()> {
    let mut ok = 0;
    let mut failed = Vec::new();
    let mut worst: Vec<(f32, String)> = Vec::new();
    let (mut nodes, mut cards, mut big, mut verts, mut tris) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for p in paths {
        match load(vfs, p) {
            Ok(t) => {
                ok += 1;
                nodes += t.nodes.len();
                cards += t.leaves.len();
                big += t.big_leaves.len();
                let parts = t.build_parts(&MeshOptions::default());
                verts += parts.iter().map(|p| p.positions.len()).sum::<usize>();
                tris += parts.iter().map(Part::triangle_count).sum::<usize>();
                // Every node must be covered by exactly one branch; every card owned.
                let covered: usize = t.branches.iter().map(|b| b.segments as usize + 1).sum();
                let orphan = t.leaves.iter().filter(|l| l.node == u32::MAX).count();
                let missing_mat = parts.iter().filter(|p| p.material.is_none()).count();
                if covered != t.nodes.len() || orphan > 0 || missing_mat > 0 {
                    failed.push(format!(
                        "{}: {} nodes but branches cover {covered}; {orphan} orphan cards; {missing_mat} parts without material",
                        short(p),
                        t.nodes.len()
                    ));
                }
                // Compare the rebuilt bounds (cards + big leaves + node centres) with the stored box.
                let mut pts: Vec<[f32; 3]> = t.nodes.iter().map(|n| n.position).collect();
                pts.extend(t.leaves.iter().flat_map(|l| l.corners()));
                pts.extend(t.big_leaves.iter().filter_map(|b| b.lods.first()).flat_map(|l| l.positions.iter().copied()));
                let mn = pts.iter().fold([f32::MAX; 3], |a, p| [0, 1, 2].map(|k| a[k].min(p[k])));
                let mx = pts.iter().fold([f32::MIN; 3], |a, p| [0, 1, 2].map(|k| a[k].max(p[k])));
                let err: f32 = (0..3).map(|k| (mn[k] - t.bbox_min[k]).abs() + (mx[k] - t.bbox_max[k]).abs()).sum();
                worst.push((err, short(p).to_string()));
            }
            Err(e) => failed.push(format!("{}: {e:#}", short(p))),
        }
    }
    println!("{} .rtx files, {ok} parsed", paths.len());
    println!("totals: {nodes} nodes, {cards} leaf cards, {big} big leaves, {verts} vertices, {tris} triangles (LOD 0)");
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    let exact = worst.iter().filter(|w| w.0 < 0.01).count();
    println!("stored bbox reproduced within 1 cm: {exact}/{}", worst.len());
    for (e, n) in worst.iter().take(8) {
        println!("  bbox error {e:6.3} m  {n}");
    }
    if failed.is_empty() {
        println!("no failures");
    } else {
        println!("{} problems:", failed.len());
        for f in &failed {
            println!("  {f}");
        }
    }
    Ok(())
}

fn export_obj(vfs: &Vfs, path: &str, out: &Path, part: Option<&str>, lod: usize, yup: bool) -> Result<()> {
    let t = load(vfs, path)?;
    let opts = MeshOptions { lod, ..Default::default() };
    let parts: Vec<Part> = t
        .build_parts(&opts)
        .into_iter()
        .filter(|p| part.is_none_or(|want| kind_name(p.kind) == want))
        .collect();
    if parts.is_empty() {
        bail!("nothing to export");
    }
    let conv = |p: [f32; 3]| if yup { [p[0], p[2], -p[1]] } else { p };
    let mtl_path = out.with_extension("mtl");
    let mut obj = String::new();
    let mut mtl = String::new();
    let _ = writeln!(obj, "# {path} ({}), {} space", t.name, if yup { "Y-up" } else { "Dunia Z-up" });
    let _ = writeln!(obj, "mtllib {}", mtl_path.file_name().unwrap().to_string_lossy());
    let mut base = 1usize;
    for p in &parts {
        let name = kind_name(p.kind);
        let _ = writeln!(obj, "o {name}\nusemtl {name}");
        for v in &p.positions {
            let v = conv(*v);
            let _ = writeln!(obj, "v {:.5} {:.5} {:.5}", v[0], v[1], v[2]);
        }
        for uv in &p.uvs {
            // OBJ texture coordinates are v-up.
            let _ = writeln!(obj, "vt {:.5} {:.5}", uv[0], 1.0 - uv[1]);
        }
        for n in &p.normals {
            let n = conv(*n);
            let _ = writeln!(obj, "vn {:.4} {:.4} {:.4}", n[0], n[1], n[2]);
        }
        for tri in p.indices.chunks_exact(3) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| i as usize + base);
            let _ = writeln!(obj, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}");
        }
        base += p.positions.len();
        let _ = writeln!(mtl, "newmtl {name}");
        if let Some(diffuse) = p
            .material
            .as_ref()
            .and_then(|m| vfs.read(m).ok())
            .and_then(|b| material::parse(&b).ok())
            .and_then(|m| realtree::resolve_material(&m).diffuse)
        {
            let file = short(&diffuse).replace(".xbt", ".png");
            let _ = writeln!(mtl, "# {diffuse}\nmap_Kd {file}");
        }
        let _ = writeln!(mtl, "Kd 1 1 1");
    }
    std::fs::write(out, obj)?;
    std::fs::write(&mtl_path, mtl)?;
    let verts: usize = parts.iter().map(|p| p.positions.len()).sum();
    let tris: usize = parts.iter().map(Part::triangle_count).sum();
    println!("wrote {} ({} parts, {verts} vertices, {tris} triangles)", out.display(), parts.len());
    Ok(())
}
