//! World sector entities (`worldsector_N.data.fcb`).
//!
//! Layout: `WorldSector { Id, sx, sy, <container> { Entity* } }`. Each entity
//! carries `hidName`, a class-name string, `hidPos` (Dunia Z-up metres),
//! `hidAngles` (degrees) and a `Components` list. Static meshes live in the
//! graphic component, either as a direct `.xbg` path field or on child
//! "slot" objects.

use anyhow::Result;

use crate::fcb::{self, Object};

/// Field hashes whose names are not known yet.
pub mod h {
    pub const CLASS_NAME: u32 = 0xD2B3429E;
    pub const GRAPHIC_COMPONENT: u32 = 0x035982C6;
    pub const GRAPHIC_SLOT: u32 = 0xA8ADABEC;
    pub const MESH_PATH: u32 = 0x3182766C;
    pub const COMPONENT_SUBCLASS: u32 = 0x431EBA8E;
    pub const NPC_WEAPON_COMPONENT: u32 = 0xD5CB8253;
    pub const PHYSICS_COMPONENT: u32 = 0x037FB709;
    pub const PHYSICS_PATH: u32 = 0x527E7674;
    /// Components that give an entity a body: static, compound, dynamic,
    /// rigid, character, the vehicles' (wheeled, floating, helicopter,
    /// paraglider, plane and generic), breakable and plain physics
    /// (`C*PhysComponent`; phantoms, particles and network sync aren't).
    pub const BODY_COMPONENTS: [u32; 13] = [
        0x037FB709, 0xAA13F142, 0x7FFE4439, 0x40B958BA, 0x39B1CE18, 0x87458482, 0x51A9185C, 0x60C6A0F4, 0xAB72CDCE, 0xAB8C754C, 0xA5D64270, 0x5CB06534,
        0x7EA0E3DD,
    ];
    pub const SKELETON_COMPONENT: u32 = 0x09D5E1B2;
    pub const SKELETON_PATH: u32 = 0xF9F2D5F4;
    /// A `CRealtreeComponent`'s tree (`.rtx`): the same field as a skeleton's
    /// path.
    pub const REALTREE_PATH: u32 = 0xF9F2D5F4;
    pub const ARCHETYPE_COMPONENT: u32 = 0x049C7D70;
    pub const ARCHETYPE_PATH: u32 = 0x2A7BCA49;
    pub const SECTOR_X: u32 = 0xB7B2364B;
    pub const SECTOR_Y: u32 = 0xC0B506DD;
    /// Graphic component: per-slot material override list.
    pub const MATERIAL_OVERRIDES: u32 = 0x8576A3A6;
    pub const MATERIAL_LIST: u32 = 0xB3864C0F;
    pub const MATERIAL_NAME: u32 = 0x8BAC1025;
    /// The material file an override entry puts in that slot, when it names
    /// one (the name above is then the slot's own material).
    pub const MATERIAL_FILE: u32 = 0x9301DFBD;
    /// Graphic component: the instance's own material list (same layout).
    pub const INSTANCE_MATERIALS: u32 = 0x0FA60B61;
    /// `CSoundComponent`, and its sound event (a u64 holding the event id).
    pub const SOUND_COMPONENT: u32 = 0x35FD2B44;
    pub const SOUND_EVENT: u32 = 0xEEACC4D5;
    /// A placed particle effect's component, and its effect's library name.
    pub const EFFECT_COMPONENT: u32 = 0x53D53912;
    pub const EFFECT_NAME: u32 = 0x1943B130;
    /// `CParticleFXComponent`: effects a prop plays on itself (a camp fire's
    /// flames), its `ParticleSystems` list of `ParticleSystem` entries, each
    /// with the effect's library name, the bone it plays at and `fScale`.
    pub const PARTICLE_FX_COMPONENT: u32 = 0x21F8DC61;
    pub const PARTICLE_SYSTEMS: u32 = 0xB5150F3F;
    pub const PARTICLE_SYSTEM: u32 = 0xCAAA7EE8;
    pub const PARTICLE_SYSTEM_NAME: u32 = 0xE025591F;
    pub const PARTICLE_SYSTEM_BONE: u32 = 0x089AE5BB;
    pub const PARTICLE_SYSTEM_SCALE: u32 = 0x84CAB079;
}

/// An effect a prop plays on itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachedEffect {
    /// Library name, lower case (`fc3_environment.fire.camp_fire`).
    pub name: String,
    /// The bone of the prop's mesh it plays at (empty: the prop's origin).
    pub bone: String,
    pub scale: f32,
}

#[derive(Debug, Clone, Default)]
pub struct WorldEntity {
    pub id: u64,
    pub name: String,
    pub class: String,
    /// Dunia space (Z-up) metres.
    pub pos: [f32; 3],
    /// Degrees; `[2]` is yaw about +Z.
    pub angles: [f32; 3],
    /// Static meshes from the graphic component.
    pub meshes: Vec<String>,
    pub graphic_class: String,
    pub skeleton: Option<String>,
    pub physics: Option<String>,
    /// It has a physics component: the engine gives it a body. (An entity
    /// with only a graphic is passed through.)
    pub solid: bool,
    pub archetype: Option<String>,
    /// Material names (`graphics\_materials\<name>.material.bin`) overriding the mesh's slots, in order.
    pub material_overrides: Vec<String>,
    /// Override entries that swap a slot's own material (by name) for
    /// another (by name): placeholder waterfalls get their flowing water.
    pub material_replacements: Vec<(String, String)>,
    /// Light settings, for light entities.
    pub light: Option<crate::lights::LightParams>,
    /// Cable to another entity (zip lines, power lines).
    pub cable: Option<crate::traversal::CableAnchor>,
    /// CLadder's authored first-person animation package.
    pub ladder_animation_package: u64,
    /// `CLadder`'s `selLadderType`: 1 is climbed hand over hand (vines).
    pub ladder_type: u32,
    /// Ledges authored to be climbed onto (`CClimbableComponent`'s `Lines`),
    /// in the entity's own axes: the ledge from one end to the other, and
    /// where the climber ends up.
    pub climb_lines: Vec<ClimbLine>,
    /// The sound event a sound point plays (its sound component's event).
    pub sound_event: Option<u32>,
    /// The particle effect a placed effect plays, by its library name
    /// (`fc3_biome_waterfall.clear_waterfall.waterfall_hitsplash`).
    pub effect: Option<String>,
    /// Effects the entity plays on itself (a camp fire's flames).
    pub attached_effects: Vec<AttachedEffect>,
    /// The mission layer the entity belongs to ([`DEFAULT_MISSION_LAYER`]
    /// for the world as it always is; others come and go with missions).
    pub mission_layer: u64,
    /// A road or river laid along a spline.
    pub spline: Option<crate::splines::SplinePrimitive>,
    /// A smart terrain point's moves (its contextual action's parameters).
    pub stp_moves: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WorldSectorData {
    pub id: u32,
    pub entities: Vec<WorldEntity>,
}

fn str_field(o: &Object, hash: u32) -> Option<String> {
    o.str_h(hash).filter(|s| !s.is_empty())
}

fn vec3_field(o: &Object, name: &str) -> Option<[f32; 3]> {
    o.vec3(name)
}

pub fn parse_entity(e: &Object) -> WorldEntity {
    let mut out = WorldEntity {
        id: e.u64("disEntityId").unwrap_or(0),
        name: e.str("hidName").unwrap_or_default(),
        class: str_field(e, h::CLASS_NAME).unwrap_or_default(),
        pos: vec3_field(e, "hidPos_precise").or_else(|| vec3_field(e, "hidPos")).unwrap_or_default(),
        angles: vec3_field(e, "hidAngles").unwrap_or_default(),
        ..Default::default()
    };
    let Some(components) = e.child("Components") else { return out };
    out.light = crate::lights::parse_light(components);
    out.cable = crate::traversal::parse_anchor(components);
    out.ladder_animation_package = components.child("CLadder").and_then(|c| c.u64("sAnimationPackage")).unwrap_or(0);
    out.ladder_type = components.child("CLadder").and_then(|c| c.u32("selLadderType")).unwrap_or(0);
    if let Some(lines) = components.child("CClimbableComponent").filter(|c| c.bool("bCanClimbFromLand") != Some(false)).and_then(|c| c.child("Lines")) {
        for l in lines.children_named("Lines") {
            let (Some(start), Some(end), Some(anchor)) = (l.vec3("disVectorLineStart"), l.vec3("disVectorLineEnd"), l.vec3("disVectorAnchor")) else { continue };
            if start != end {
                out.climb_lines.push(ClimbLine { start, end, anchor });
            }
        }
    }
    out.spline = crate::splines::parse(components);
    if let Some(params) = components.child("CAIContextualActionComponent").and_then(|c| c.child("Parameters")) {
        out.stp_moves = crate::moves::move_names(params);
    }
    out.solid = components.children.iter().any(|c| h::BODY_COMPONENTS.contains(&c.name_hash));
    for c in &components.children {
        // A placed RealTree (the ones the story grows) is its own model, and
        // its trunk stands in the way as a planted tree's does.
        if c.is("CRealtreeComponent") {
            if let Some(p) = str_field(c, h::REALTREE_PATH).filter(|p| p.to_ascii_lowercase().ends_with(".rtx")) {
                out.meshes.insert(0, p);
                out.solid = true;
            }
        }
        match c.name_hash {
            h::EFFECT_COMPONENT => {
                out.effect = str_field(c, h::EFFECT_NAME).map(|s| s.to_ascii_lowercase());
            }
            h::PARTICLE_FX_COMPONENT => {
                for list in c.children.iter().filter(|o| o.name_hash == h::PARTICLE_SYSTEMS) {
                    for ps in list.children.iter().filter(|o| o.name_hash == h::PARTICLE_SYSTEM) {
                        let Some(name) = str_field(ps, h::PARTICLE_SYSTEM_NAME) else { continue };
                        let scale = ps.field_h(h::PARTICLE_SYSTEM_SCALE).filter(|b| b.len() >= 4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                        out.attached_effects.push(AttachedEffect {
                            name: name.to_ascii_lowercase(),
                            bone: str_field(ps, h::PARTICLE_SYSTEM_BONE).unwrap_or_default(),
                            scale: scale.filter(|s| *s > 0.0).unwrap_or(1.0),
                        });
                    }
                }
            }
            h::SOUND_COMPONENT => {
                out.sound_event = c
                    .field_h(h::SOUND_EVENT)
                    .filter(|b| b.len() >= 4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .filter(|&id| id != 0 && id != u32::MAX);
            }
            h::GRAPHIC_COMPONENT => {
                out.graphic_class = str_field(c, h::COMPONENT_SUBCLASS).unwrap_or_default();
                if let Some(p) = str_field(c, h::MESH_PATH) {
                    out.meshes.push(p);
                }
                // The default list, then the instance's own: entries in either
                // that name a file swap their slot's material for it.
                for (container, listed) in [(h::MATERIAL_OVERRIDES, true), (h::INSTANCE_MATERIALS, false)] {
                    let Some(list) = c.children.iter().find(|o| o.name_hash == container) else { continue };
                    for group in list.children.iter().filter(|o| o.name_hash == h::MATERIAL_LIST) {
                        for m in group.children_named("Material") {
                            let name = str_field(m, h::MATERIAL_NAME).unwrap_or_default();
                            let file = str_field(m, h::MATERIAL_FILE).map(|f| {
                                let l = f.to_ascii_lowercase();
                                let base = l.rsplit('\\').next().unwrap_or(&l).to_string();
                                base.trim_end_matches(".material.bin").to_string()
                            });
                            if let Some(f) = file.filter(|f| *f != name.to_ascii_lowercase()) {
                                let swap = (name.to_ascii_lowercase(), f);
                                if !out.material_replacements.contains(&swap) {
                                    out.material_replacements.push(swap);
                                }
                            }
                            if listed {
                                out.material_overrides.push(name);
                            }
                        }
                    }
                }
                for slot in c.children.iter().filter(|s| s.name_hash == h::GRAPHIC_SLOT) {
                    if let Some(p) = str_field(slot, h::MESH_PATH).filter(|p| p.to_ascii_lowercase().ends_with(".xbg")) {
                        if !out.meshes.contains(&p) {
                            out.meshes.push(p);
                        }
                    }
                }
            }
            h::PHYSICS_COMPONENT => out.physics = str_field(c, h::PHYSICS_PATH),
            h::SKELETON_COMPONENT => out.skeleton = str_field(c, h::SKELETON_PATH),
            h::ARCHETYPE_COMPONENT => out.archetype = str_field(c, h::ARCHETYPE_PATH),
            _ => {}
        }
    }
    out.meshes.retain(|m| {
        let m = m.to_ascii_lowercase();
        m.ends_with(".xbg") || m.ends_with(".rtx")
    });
    out
}

/// A ledge authored to be climbed onto (Dunia axes, metres).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClimbLine {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub anchor: [f32; 3],
}

/// Parses top-level entities of a world sector (prefab children are references, not copies).
/// The mission layer of the world as it always is.
pub const DEFAULT_MISSION_LAYER: u64 = 0x64;

pub fn parse_sector(data: &[u8]) -> Result<WorldSectorData> {
    let root = fcb::parse(data)?;
    let mut out = WorldSectorData { id: root.u32("Id").unwrap_or(0), entities: Vec::new() };
    for (layer, e) in layered_entities(&root) {
        let mut e = parse_entity(&e);
        e.mission_layer = layer;
        out.entities.push(e);
    }
    Ok(out)
}

/// Every entity object in a sector (or metasector) file with its mission
/// layer, the default one's and the others'.
pub fn layered_entities(root: &std::sync::Arc<Object>) -> Vec<(u64, std::sync::Arc<Object>)> {
    let mut out = Vec::new();
    for container in &root.children {
        let layer = container.u64("missionLayerId").unwrap_or(DEFAULT_MISSION_LAYER);
        out.extend(container.children_named("Entity").map(|e| (layer, e.clone())));
        // Layers other than the default keep their entities LZO-compressed
        // (`CNH_CompressedData`: a u32 size, then the stream of an FCB whose
        // root is the layer again).
        if let Some(inner) = compressed_layer(container) {
            out.extend(inner.children_named("Entity").map(|e| (layer, e.clone())));
        }
    }
    out
}

/// Every mission layer's object tree in a sector or generated file: the
/// file's root, and the roots of its compressed layers.
pub fn layer_roots(root: &std::sync::Arc<Object>) -> Vec<std::sync::Arc<Object>> {
    let mut out = vec![root.clone()];
    for layer in &root.children {
        out.extend(compressed_layer(layer));
    }
    out
}

fn compressed_layer(layer: &Object) -> Option<std::sync::Arc<Object>> {
    let blob = layer.field("CNH_CompressedData")?;
    let size = layer.u32("CNH_UncompressedSize")? as usize;
    let raw = crate::lzo::decompress(blob.get(4..)?, size).ok()?;
    let start = raw.windows(4).position(|w| w == b"nbCF").unwrap_or(0);
    fcb::parse(&raw[start..]).ok()
}

/// Parses every `Entity` object anywhere in an FCB (used for level layer files).
pub fn parse_all_entities(data: &[u8]) -> Result<Vec<WorldEntity>> {
    let root = fcb::parse(data)?;
    let mut out = Vec::new();
    fn go(o: &Object, out: &mut Vec<WorldEntity>) {
        for c in &o.children {
            if c.is("Entity") {
                out.push(parse_entity(c));
            } else {
                go(c, out);
            }
        }
    }
    go(&root, &mut out);
    Ok(out)
}

/// Dunia position (Z-up) to Bevy space (Y-up): (x, y, z) -> (x, z, -y).
pub fn pos_to_bevy(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx, aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz]
}

fn axis_quat(axis: [f32; 3], angle: f32) -> [f32; 4] {
    let (s, c) = (angle * 0.5).sin_cos();
    [axis[0] * s, axis[1] * s, axis[2] * s, c]
}

/// Entity orientation from `hidAngles` (degrees) as a Bevy-space quaternion (x, y, z, w).
/// Yaw is about Dunia +Z (Bevy +Y), pitch about Dunia +X, roll about Dunia +Y (Bevy -Z).
pub fn angles_to_bevy_quat(a: [f32; 3]) -> [f32; 4] {
    let [rx, ry, rz] = a.map(f32::to_radians);
    let yaw = axis_quat([0.0, 1.0, 0.0], rz);
    let pitch = axis_quat([1.0, 0.0, 0.0], rx);
    let roll = axis_quat([0.0, 0.0, 1.0], -ry);
    quat_mul(quat_mul(yaw, pitch), roll)
}

/// A vegetation mesh's rotation from its stored angles (degrees, the same
/// axes as an entity's), which the game applies yaw, then roll, then pitch
/// (Z·Y·X), not as entities above. It matters only with both pitch and roll
/// (rocks thrown in at all angles). Scattered with the other order, the
/// rocks in the doctor's cave put 14 of its 26 floor mushrooms inside rock
/// and left 14 resting on a surface; with this order, 6 and 22.
pub fn vegetation_angles_to_bevy_quat(a: [f32; 3]) -> [f32; 4] {
    let [rx, ry, rz] = a.map(f32::to_radians);
    let yaw = axis_quat([0.0, 1.0, 0.0], rz);
    let pitch = axis_quat([1.0, 0.0, 0.0], rx);
    let roll = axis_quat([0.0, 0.0, 1.0], -ry);
    quat_mul(quat_mul(yaw, roll), pitch)
}

#[cfg(test)]
mod vegetation_order_tests {
    use super::*;

    fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
        let p = quat_mul(quat_mul(q, [v[0], v[1], v[2], 0.0]), [-q[0], -q[1], -q[2], q[3]]);
        [p[0], p[1], p[2]]
    }

    /// Pitch then roll differ from roll then pitch; with either alone, or
    /// yaw alone, the two orders agree.
    #[test]
    fn vegetation_applies_roll_before_pitch() {
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5);
        for a in [[30.0, 0.0, 70.0], [0.0, 40.0, -20.0], [0.0, 0.0, 135.0]] {
            assert!(close(rotate(vegetation_angles_to_bevy_quat(a), [0.3, 0.5, 0.7]), rotate(angles_to_bevy_quat(a), [0.3, 0.5, 0.7])));
        }
        // Both at 90 degrees: a vegetation mesh's up (Bevy +Y) is pitched
        // onto +Z first, which the roll about Z then leaves; an entity's is
        // rolled onto -X first, which the pitch about X then leaves.
        let up = rotate(vegetation_angles_to_bevy_quat([90.0, -90.0, 0.0]), [0.0, 1.0, 0.0]);
        assert!(close(up, [0.0, 0.0, 1.0]), "{up:?}");
        let up = rotate(angles_to_bevy_quat([90.0, -90.0, 0.0]), [0.0, 1.0, 0.0]);
        assert!(close(up, [-1.0, 0.0, 0.0]), "{up:?}");
    }
}

/// Entities whose meshes belong to gameplay systems (weapon pickups) rather than static scenery.
/// Archive path of a material referenced by name from an entity override.
pub fn material_override_path(name: &str) -> String {
    format!(r"graphics\_materials\{}.material.bin", name.to_ascii_lowercase())
}

pub fn is_gameplay_mesh(e: &WorldEntity) -> bool {
    e.name.to_ascii_lowercase().starts_with("pickups/") && e.meshes.iter().any(|m| m.to_ascii_lowercase().contains("\\weapons\\"))
}
