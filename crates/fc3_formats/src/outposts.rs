//! Enemy outposts, recovered from world entities.
//!
//! The world places no single "outpost" entity, but every outpost has an
//! alarm: alarm boxes (`Interaction/AlarmLight_*`), the spots guards run to
//! (`CombatHelpers/AlarmPoint`) and horns (`*Outpost*_AlarmHorn_*`). Clustering
//! those gives each outpost's centre and extent. The rebel posts used once an
//! outpost is taken (`CampEncounter/FreeOutpost_Rebel_*`) are attached to the
//! nearest outpost. Positions are in Bevy space (Y up).

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
}

/// Classifies a world entity by name.
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

#[derive(Debug, Clone, PartialEq)]
pub struct Outpost {
    pub centre: [f32; 3],
    /// Ground radius that counts as inside the outpost (metres).
    pub radius: f32,
    pub alarms: Vec<[f32; 3]>,
    pub rebels: Vec<[f32; 3]>,
}

fn flat_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Groups alarm markers that lie within `link` metres of each other into
/// outposts (single linkage), then attaches rebel posts.
pub fn cluster(markers: &[(MarkerKind, [f32; 3])]) -> Vec<Outpost> {
    const LINK: f32 = 70.0;
    let alarm: Vec<(MarkerKind, [f32; 3])> = markers.iter().copied().filter(|(k, _)| *k != MarkerKind::RebelPost).collect();
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
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..alarm.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Outpost> = groups
        .values()
        .filter(|g| g.len() >= 2)
        .map(|g| {
            let n = g.len() as f32;
            let centre: [f32; 3] = std::array::from_fn(|k| g.iter().map(|&i| alarm[i].1[k]).sum::<f32>() / n);
            let extent = g.iter().map(|&i| flat_dist(alarm[i].1, centre)).fold(0.0, f32::max);
            Outpost {
                centre,
                radius: (extent + 40.0).clamp(55.0, 110.0),
                alarms: g.iter().filter(|&&i| alarm[i].0 == MarkerKind::AlarmBox).map(|&i| alarm[i].1).collect(),
                rebels: Vec::new(),
            }
        })
        .collect();
    for (_, p) in markers.iter().filter(|(k, _)| *k == MarkerKind::RebelPost) {
        if let Some(o) = out.iter_mut().filter(|o| flat_dist(o.centre, *p) < o.radius + 60.0).min_by(|a, b| flat_dist(a.centre, *p).total_cmp(&flat_dist(b.centre, *p))) {
            o.rebels.push(*p);
        }
    }
    // Stable order (west to east, then north to south) so ids survive re-conversion.
    out.sort_by(|a, b| a.centre[0].total_cmp(&b.centre[0]).then(a.centre[2].total_cmp(&b.centre[2])));
    out
}

/// Text form: `outpost x y z radius`, followed by its `alarm x y z` and
/// `rebel x y z` lines.
pub fn to_text(outposts: &[Outpost]) -> String {
    let mut s = String::from("# fc3 outposts v1\n");
    let v = |p: &[f32; 3]| format!("{:.2} {:.2} {:.2}", p[0], p[1], p[2]);
    for o in outposts {
        s += &format!("outpost {} {:.1}\n", v(&o.centre), o.radius);
        for a in &o.alarms {
            s += &format!("alarm {}\n", v(a));
        }
        for r in &o.rebels {
            s += &format!("rebel {}\n", v(r));
        }
    }
    s
}

pub fn from_text(text: &str) -> Vec<Outpost> {
    let mut out: Vec<Outpost> = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        let nums: Vec<f32> = it.filter_map(|v| v.parse().ok()).collect();
        if nums.len() < 3 {
            continue;
        }
        let p = [nums[0], nums[1], nums[2]];
        match key {
            "outpost" => out.push(Outpost { centre: p, radius: nums.get(3).copied().unwrap_or(70.0), alarms: Vec::new(), rebels: Vec::new() }),
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
        assert!(o[0].radius >= 55.0);
        let back = from_text(&to_text(&o));
        assert_eq!(back.len(), o.len());
        for (a, b) in back.iter().zip(&o) {
            assert!(flat_dist(a.centre, b.centre) < 0.01 && (a.radius - b.radius).abs() < 0.06);
            assert_eq!((a.alarms.len(), a.rebels.len()), (b.alarms.len(), b.rebels.len()));
        }
    }
}
