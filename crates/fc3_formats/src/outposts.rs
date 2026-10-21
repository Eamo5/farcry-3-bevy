//! Enemy outposts, recovered from world entities.
//!
//! Each outpost the game counts has its flag pole: the encounter prefab its
//! flag controller runs on (`FlagPole_Pirate_*`, `FlagPole_Privateer_*`, 34
//! of them, the map's 34 outposts). Its alarms say how far the outpost
//! reaches: alarm boxes (`Interaction/AlarmLight_*`), the spots guards run to
//! (`CombatHelpers/AlarmPoint`) and horns (`*Outpost*_AlarmHorn_*`), clustered,
//! give each outpost's centre and extent. Alarms with no flag pole by them
//! (story places: Badtown, Vaas's and Hoyt's compounds) are no outpost. The
//! rebel posts used once an outpost is taken (`CampEncounter/FreeOutpost_
//! Rebel_*`) are attached to the nearest outpost. Positions are in Bevy space
//! (Y up).

/// The text form's first line.
pub const HEADER: &str = "# fc3 outposts v2";

/// Kinds of entity that mark an outpost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerKind {
    /// An alarm box guards can trigger and the player can sabotage.
    AlarmBox,
    /// Where guards stand to raise the alarm.
    AlarmPoint,
    AlarmHorn,
    /// A friendly post manned after the outpost is liberated.
    RebelPost,
    /// The outpost's flag pole (its entity's id).
    FlagPole(u64),
}

/// Classifies a world entity by name (a flag pole by [`is_flag_pole`]).
pub fn marker_kind(name: &str) -> Option<MarkerKind> {
    let n = name.to_ascii_lowercase();
    if n.starts_with("interaction/alarmlight") {
        Some(MarkerKind::AlarmBox)
    } else if n.starts_with("combathelpers/alarmpoint") {
        Some(MarkerKind::AlarmPoint)
    } else if n.contains("outpost") && n.contains("alarmhorn") {
        Some(MarkerKind::AlarmHorn)
    } else if n.contains("freeoutpost_rebel") {
        Some(MarkerKind::RebelPost)
    } else {
        None
    }
}

/// Whether a world entity is an outpost's flag pole.
pub fn is_flag_pole(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("flagpole_pirate") || n.starts_with("flagpole_privateer")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outpost {
    pub centre: [f32; 3],
    /// Ground radius that counts as inside the outpost (metres).
    pub radius: f32,
    pub alarms: Vec<[f32; 3]>,
    pub rebels: Vec<[f32; 3]>,
    /// Its flag pole's entity id (saves name outposts by it).
    pub flag: Option<u64>,
    /// Its index in the alarm-only list of v1 tables, which older saves
    /// name outposts by.
    pub legacy: Option<u32>,
}

fn flat_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// How far (m) an outpost's flag pole may stand from its nearest alarm.
const FLAG_LINK: f32 = 100.0;

/// Groups alarm markers that lie within `link` metres of each other (single
/// linkage); each flag pole takes the group nearest it as its outpost's
/// extent, then rebel posts are attached. Without flag poles, every group of
/// two or more is an outpost.
pub fn cluster(markers: &[(MarkerKind, [f32; 3])]) -> Vec<Outpost> {
    const LINK: f32 = 70.0;
    let alarm: Vec<(MarkerKind, [f32; 3])> =
        markers.iter().copied().filter(|(k, _)| matches!(k, MarkerKind::AlarmBox | MarkerKind::AlarmPoint | MarkerKind::AlarmHorn)).collect();
    let flags: Vec<(u64, [f32; 3])> = markers
        .iter()
        .filter_map(|(k, p)| match k {
            MarkerKind::FlagPole(id) => Some((*id, *p)),
            _ => None,
        })
        .collect();
    let mut parent: Vec<usize> = (0..alarm.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..alarm.len() {
        for j in i + 1..alarm.len() {
            if flat_dist(alarm[i].1, alarm[j].1) < LINK {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..alarm.len() {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    let groups: Vec<Vec<usize>> = by_root.into_values().collect();
    let of_group = |g: &[usize]| {
        let n = g.len() as f32;
        let centre: [f32; 3] = std::array::from_fn(|k| g.iter().map(|&i| alarm[i].1[k]).sum::<f32>() / n);
        let extent = g.iter().map(|&i| flat_dist(alarm[i].1, centre)).fold(0.0, f32::max);
        Outpost {
            centre,
            radius: (extent + 40.0).clamp(55.0, 110.0),
            alarms: g.iter().filter(|&&i| alarm[i].0 == MarkerKind::AlarmBox).map(|&i| alarm[i].1).collect(),
            rebels: Vec::new(),
            flag: None,
            legacy: None,
        }
    };
    // West to east, then north to south.
    let order = |a: &Outpost, b: &Outpost| a.centre[0].total_cmp(&b.centre[0]).then(a.centre[2].total_cmp(&b.centre[2]));
    // The alarm-only list as v1 tables had it (groups of two or more).
    let mut v1: Vec<(usize, Outpost)> = groups.iter().enumerate().filter(|(_, g)| g.len() >= 2).map(|(i, g)| (i, of_group(g))).collect();
    v1.sort_by(|a, b| order(&a.1, &b.1));
    let legacy = |group: usize| v1.iter().position(|(i, _)| *i == group).map(|n| n as u32);
    let mut out: Vec<Outpost> = if flags.is_empty() {
        v1.iter().enumerate().map(|(n, (_, o))| Outpost { legacy: Some(n as u32), ..o.clone() }).collect()
    } else {
        flags
            .iter()
            .map(|&(id, at)| {
                let near = groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| (i, g.iter().map(|&m| flat_dist(alarm[m].1, at)).fold(f32::MAX, f32::min)))
                    .filter(|(_, d)| *d < FLAG_LINK)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                match near {
                    Some((i, _)) => {
                        let mut o = of_group(&groups[i]);
                        // (Its flag inside it.)
                        o.radius = o.radius.max(flat_dist(o.centre, at) + 15.0);
                        Outpost { flag: Some(id), legacy: legacy(i), ..o }
                    }
                    None => Outpost { centre: at, radius: 55.0, alarms: Vec::new(), rebels: Vec::new(), flag: Some(id), legacy: None },
                }
            })
            .collect()
    };
    for (_, p) in markers.iter().filter(|(k, _)| *k == MarkerKind::RebelPost) {
        if let Some(o) = out.iter_mut().filter(|o| flat_dist(o.centre, *p) < o.radius + 60.0).min_by(|a, b| flat_dist(a.centre, *p).total_cmp(&flat_dist(b.centre, *p))) {
            o.rebels.push(*p);
        }
    }
    out.sort_by(order);
    out
}

/// Text form: [`HEADER`], then `outpost x y z radius`, followed by its
/// `flag <id>`, `legacy <index>`, `alarm x y z` and `rebel x y z` lines.
pub fn to_text(outposts: &[Outpost]) -> String {
    let mut s = format!("{HEADER}\n");
    let v = |p: &[f32; 3]| format!("{:.2} {:.2} {:.2}", p[0], p[1], p[2]);
    for o in outposts {
        s += &format!("outpost {} {:.1}\n", v(&o.centre), o.radius);
        if let Some(id) = o.flag {
            s += &format!("flag {id}\n");
        }
        if let Some(n) = o.legacy {
            s += &format!("legacy {n}\n");
        }
        for a in &o.alarms {
            s += &format!("alarm {}\n", v(a));
        }
        for r in &o.rebels {
            s += &format!("rebel {}\n", v(r));
        }
    }
    s
}

/// Reads the text form (v1 tables too: their outposts' legacy index is
/// their place in the list).
pub fn from_text(text: &str) -> Vec<Outpost> {
    let v1 = text.lines().next() != Some(HEADER);
    let mut out: Vec<Outpost> = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        let rest: Vec<&str> = it.collect();
        if let (Some(o), Some(n)) = (out.last_mut(), rest.first()) {
            match key {
                "flag" => o.flag = n.parse().ok(),
                "legacy" => o.legacy = n.parse().ok(),
                _ => {}
            }
        }
        let nums: Vec<f32> = rest.iter().filter_map(|v| v.parse().ok()).collect();
        if nums.len() < 3 {
            continue;
        }
        let p = [nums[0], nums[1], nums[2]];
        match key {
            "outpost" => {
                let legacy = v1.then_some(out.len() as u32);
                out.push(Outpost { centre: p, radius: nums.get(3).copied().unwrap_or(70.0), alarms: Vec::new(), rebels: Vec::new(), flag: None, legacy })
            }
            "alarm" => {
                if let Some(o) = out.last_mut() {
                    o.alarms.push(p);
                }
            }
            "rebel" => {
                if let Some(o) = out.last_mut() {
                    o.rebels.push(p);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_and_clusters() {
        assert_eq!(marker_kind("Interaction/AlarmLight_01_3"), Some(MarkerKind::AlarmBox));
        assert_eq!(marker_kind("Outpost01_AlarmHorn_01_2"), Some(MarkerKind::AlarmHorn));
        assert_eq!(marker_kind("CampEncounter/FreeOutpost_Rebel_1"), Some(MarkerKind::RebelPost));
        assert_eq!(marker_kind("relax/pirate_guard"), None);
        assert!(is_flag_pole("FlagPole_Pirate_D3_28") && is_flag_pole("FlagPole_Privateer_G4_33"));
        assert!(!is_flag_pole("FlagCinematicSceneEntity") && !is_flag_pole("Flag"));
        let m = [
            (MarkerKind::AlarmBox, [0.0, 10.0, 0.0]),
            (MarkerKind::AlarmPoint, [20.0, 10.0, 5.0]),
            (MarkerKind::AlarmHorn, [-10.0, 12.0, 30.0]),
            (MarkerKind::AlarmBox, [1000.0, 10.0, 0.0]),
            (MarkerKind::AlarmPoint, [1030.0, 10.0, 0.0]),
            // A lone marker is not an outpost.
            (MarkerKind::AlarmPoint, [5000.0, 10.0, 0.0]),
            (MarkerKind::RebelPost, [15.0, 10.0, 10.0]),
        ];
        let o = cluster(&m);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].alarms.len(), 1);
        assert_eq!(o[0].rebels.len(), 1);
        assert_eq!((o[0].legacy, o[1].legacy), (Some(0), Some(1)));
        assert!(o[0].radius >= 55.0);
        let back = from_text(&to_text(&o));
        assert_eq!(back.len(), o.len());
        for (a, b) in back.iter().zip(&o) {
            assert!(flat_dist(a.centre, b.centre) < 0.01 && (a.radius - b.radius).abs() < 0.06);
            assert_eq!((a.alarms.len(), a.rebels.len(), a.flag, a.legacy), (b.alarms.len(), b.rebels.len(), b.flag, b.legacy));
        }
    }

    #[test]
    fn flag_poles_make_the_outposts() {
        let m = [
            // An outpost with its alarms.
            (MarkerKind::AlarmBox, [0.0, 10.0, 0.0]),
            (MarkerKind::AlarmPoint, [20.0, 10.0, 5.0]),
            (MarkerKind::FlagPole(7), [10.0, 10.0, 30.0]),
            // Alarms with no flag by them: a story place.
            (MarkerKind::AlarmBox, [1000.0, 10.0, 0.0]),
            (MarkerKind::AlarmPoint, [1030.0, 10.0, 0.0]),
            // A flag whose outpost has a single alarm box.
            (MarkerKind::AlarmBox, [-2000.0, 10.0, 40.0]),
            (MarkerKind::FlagPole(9), [-2000.0, 10.0, 0.0]),
            // And one with none.
            (MarkerKind::FlagPole(11), [3000.0, 20.0, 0.0]),
        ];
        let o = cluster(&m);
        assert_eq!(o.iter().map(|o| o.flag).collect::<Vec<_>>(), [Some(9), Some(7), Some(11)]);
        // (Old tables listed the flagged group first and the story place second.)
        assert_eq!(o.iter().map(|o| o.legacy).collect::<Vec<_>>(), [None, Some(0), None]);
        assert_eq!(o[0].alarms.len(), 1);
        assert_eq!(o[2].centre, [3000.0, 20.0, 0.0]);
        assert!(o.iter().all(|o| o.flag.is_some()));
        let back = from_text(&to_text(&o));
        assert_eq!(back.iter().map(|o| (o.flag, o.legacy)).collect::<Vec<_>>(), o.iter().map(|o| (o.flag, o.legacy)).collect::<Vec<_>>());
        // A v1 table's outposts are their legacy index.
        let old = "# fc3 outposts v1\noutpost 1 2 3 55.0\nalarm 1 2 3\noutpost 4 5 6 60.0\n";
        assert_eq!(from_text(old).iter().map(|o| o.legacy).collect::<Vec<_>>(), [Some(0), Some(1)]);
    }
}
