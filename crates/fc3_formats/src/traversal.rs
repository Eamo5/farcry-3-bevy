//! Ways around the world: cables (rideable zip lines and the power lines
//! strung between poles), ladders, and radio tower panels.
//!
//! A cable runs between two anchor entities: each anchor carries a link
//! component naming the entity at the far end and where on itself the cable
//! attaches. The converter resolves those links into one list with the
//! ladders and tower panels found in the world.

use crate::fcb::Object;

pub const CACHE_HEADER: &str = "# traversal 5: ladder identities, layers, animation packages and kinds; climbing lines";

/// The installed CLadder package shared by the hand-over-hand vine ladders.
pub const VINE_ANIMATION_PACKAGE: u64 = 0x95aea5a8ded23d2c;

mod h {
    pub const LINK_COMPONENT: u32 = 0x1C222C20;
    pub const LINK_TARGET: u32 = 0x7C064F01;
    pub const ROPE: u32 = 0x112C7809;
    pub const ATTACH_OFFSET: u32 = 0xC30D4AFC;
}

/// One end of a cable as stored on its anchor entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CableAnchor {
    /// Entity id of the anchor at the far end.
    pub link: u64,
    /// Attachment point in the anchor's own frame (Dunia space, Z up).
    pub offset: [f32; 3],
}

/// Reads an anchor's cable link from its `Components`.
pub fn parse_anchor(components: &Object) -> Option<CableAnchor> {
    let c = components.children.iter().find(|c| c.name_hash == h::LINK_COMPONENT)?;
    let link = c.field_h(h::LINK_TARGET).filter(|b| b.len() >= 8).map(|b| u64::from_le_bytes(b[..8].try_into().unwrap()))?;
    if link == 0 || link == u64::MAX {
        return None;
    }
    let offset = c
        .children
        .iter()
        .find(|o| o.name_hash == h::ROPE)
        .and_then(|r| r.field_h(h::ATTACH_OFFSET))
        .filter(|b| b.len() >= 12)
        .map(|b| {
            let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
            [f(0), f(1), f(2)]
        })
        .unwrap_or([0.0; 3]);
    Some(CableAnchor { link, offset })
}

/// Whether an anchor entity's cable is a zip line the player can ride.
pub fn is_zipline_anchor(name: &str) -> bool {
    name.to_ascii_lowercase().contains("ziplineanchor")
}

/// Whether an entity is a climbable ladder.
pub fn is_ladder(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.starts_with("ladders/") && n.contains("ladder")) || n.starts_with("radiotower_ladder")
}

/// Whether an entity is a radio tower's control panel.
pub fn is_tower_panel(name: &str) -> bool {
    name.to_ascii_lowercase().contains("radiotowerpanel")
}

/// A cable between two points (Bevy space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cable {
    pub zipline: bool,
    pub a: [f32; 3],
    pub b: [f32; 3],
}

/// A ladder: its foot, the height of its top, and the horizontal direction
/// its rungs face (a climber stands on that side).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ladder {
    /// Authored entity identity. Zero only for legacy geometry-only caches.
    pub id: u64,
    pub mission_layer: u64,
    pub animation_package: u64,
    pub base: [f32; 3],
    pub top: f32,
    pub facing: [f32; 3],
    /// Climbed hand over hand (vines, ropes) rather than hands on the side.
    pub hand_over_hand: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Traversal {
    pub cables: Vec<Cable>,
    pub ladders: Vec<Ladder>,
    /// Radio tower control panels.
    pub towers: Vec<[f32; 3]>,
    /// Ledges authored to be climbed onto.
    pub climbs: Vec<Climb>,
}

/// A ledge authored to be climbed onto (Bevy space): from `a` to `b`, the
/// climber ending at `anchor`, while its mission layer is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Climb {
    pub mission_layer: u64,
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub anchor: [f32; 3],
}

impl Traversal {
    pub fn to_text(&self) -> String {
        let mut s =
            format!("{CACHE_HEADER}\n# zip|wire ax ay az bx by bz / ladder-animation id layer package x y z top fx fy fz hand-over-hand / tower x y z\n");
        let v = |p: [f32; 3]| format!("{:.2} {:.2} {:.2}", p[0], p[1], p[2]);
        for c in &self.cables {
            s += &format!("{} {} {}\n", if c.zipline { "zip" } else { "wire" }, v(c.a), v(c.b));
        }
        for l in &self.ladders {
            s += &format!(
                "ladder-animation {} {} {} {} {:.2} {:.3} {:.3} {:.3} {}\n",
                l.id,
                l.mission_layer,
                l.animation_package,
                v(l.base),
                l.top,
                l.facing[0],
                l.facing[1],
                l.facing[2],
                u8::from(l.hand_over_hand)
            );
        }
        for t in &self.towers {
            s += &format!("tower {}\n", v(*t));
        }
        for c in &self.climbs {
            s += &format!("climb {} {} {} {}\n", c.mission_layer, v(c.a), v(c.b), v(c.anchor));
        }
        s
    }

    pub fn from_text(text: &str) -> Traversal {
        let mut t = Traversal::default();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut it = line.split_whitespace();
            let Some(kind) = it.next() else { continue };
            // Entity ids exceed f64's integer precision, let alone f32's.
            // Parse identity separately from the geometry coordinates.
            let id = if matches!(kind, "ladder-entity" | "ladder-animation") {
                let Some(id) = it.next().and_then(|s| s.parse::<u64>().ok()) else { continue };
                id
            } else { 0 };
            let mission_layer = if matches!(kind, "ladder-entity" | "ladder-animation" | "climb") {
                let Some(layer) = it.next().and_then(|s| s.parse::<u64>().ok()) else { continue };
                layer
            } else { crate::world::DEFAULT_MISSION_LAYER };
            let animation_package = if kind == "ladder-animation" {
                let Some(package) = it.next().and_then(|s| s.parse::<u64>().ok()) else { continue };
                package
            } else { 0 };
            let v: Vec<f32> = it.filter_map(|s| s.parse().ok()).collect();
            match (kind, v.len()) {
                ("zip" | "wire", 6) => t.cables.push(Cable { zipline: kind == "zip", a: [v[0], v[1], v[2]], b: [v[3], v[4], v[5]] }),
                ("ladder" | "ladder-entity" | "ladder-animation", 7 | 8) => t.ladders.push(Ladder {
                    id,
                    mission_layer,
                    animation_package,
                    base: [v[0], v[1], v[2]],
                    top: v[3],
                    facing: [v[4], v[5], v[6]],
                    hand_over_hand: v.get(7).is_some_and(|h| *h != 0.0),
                }),
                ("tower", 3) => t.towers.push([v[0], v[1], v[2]]),
                ("climb", 9) => t.climbs.push(Climb { mission_layer, a: [v[0], v[1], v[2]], b: [v[3], v[4], v[5]], anchor: [v[6], v[7], v[8]] }),
                _ => {}
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip() {
        let t = Traversal {
            cables: vec![
                Cable { zipline: true, a: [1.0, 120.5, -3.0], b: [40.0, 101.25, 7.5] },
                Cable { zipline: false, a: [0.0, 1.0, 2.0], b: [3.0, 4.0, 5.0] },
            ],
            ladders: vec![Ladder {
                id: 2078490335079601680,
                mission_layer: 0xa07db8c44bc1f1,
                animation_package: VINE_ANIMATION_PACKAGE,
                base: [5.0, 100.0, 5.0],
                top: 108.5,
                facing: [0.0, 0.0, -1.0],
                hand_over_hand: true,
            }],
            towers: vec![[10.0, 150.0, -20.0]],
            climbs: vec![Climb { mission_layer: 0x64, a: [1.0, 2.5, 3.0], b: [2.0, 2.5, 3.5], anchor: [1.5, 2.5, 4.0] }],
        };
        assert_eq!(Traversal::from_text(&t.to_text()), t);
    }

    #[test]
    fn names() {
        assert!(is_zipline_anchor("Ziplines/ZiplineAnchorSmall_1"));
        assert!(!is_zipline_anchor("Ziplines/ElectricWireAnchor_20"));
        assert!(is_ladder("Ladders/Ladder14steps_3"));
        assert!(is_ladder("Radiotower_Ladder01_2"));
        assert!(!is_ladder("Visuals/Ladder_VinesTiledLeaf_4m_1"));
        assert!(is_tower_panel("Interaction/RadioTowerPanel_3"));
    }

    #[test]
    fn legacy_ladders_remain_readable_without_inventing_identity() {
        let old = Traversal::from_text("ladder 1 2 3 12 0 0 -1\n");
        assert_eq!(old.ladders.len(), 1);
        assert_eq!(old.ladders[0].id, 0);
        assert_eq!(old.ladders[0].top, 12.0);
        assert_eq!(old.ladders[0].mission_layer, crate::world::DEFAULT_MISSION_LAYER);
        let v2 = Traversal::from_text("ladder-entity 2078490335079601680 45174228794458609 1 2 3 12 0 0 -1\n");
        assert_eq!(v2.ladders[0].id, 2078490335079601680);
        assert_eq!(v2.ladders[0].animation_package, 0);
        assert!(Traversal::from_text("ladder-entity invalid 1 2 3 12 0 0 -1\n").ladders.is_empty());
    }
}
