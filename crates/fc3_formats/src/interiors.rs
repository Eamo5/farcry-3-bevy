//! Interior volumes (`CInteriorVolumeComponent`): closed volumes the artists
//! put inside buildings, tunnels and caves, each with an ambient of its own
//! (`intambientInteriorAmbient`, a key into `interiorambient.lib`). Inside
//! one the game lights with that ambient instead of the world's light
//! probes, which would bleed the sky's blue light indoors (the DRTV talk,
//! GDC 2012: the volumes are drawn to a mask, blurred, and the pixels within
//! take their volume's probe).
//!
//! The volume's shape is a model of its own (string field `0x3182766C`, an
//! `*_interiorvolume.xbg`) in the entity's axes; its bounds stand for it. An
//! ambient is a floor and a ceiling colour and an intensity, by day and by
//! night.

use crate::fcb::Object;

/// Where the game keeps the interiors' ambients.
pub const AMBIENTS_PATH: &str = r"generated\databases\generic\interiorambient.lib";

/// The component's volume model (a string field the name database lacks).
const VOLUME_MODEL: u32 = 0x3182_766C;

/// An entity's interior volume as it names it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InteriorRef {
    pub model: String,
    pub ambient: u32,
}

/// The interior volume on an entity's components, if it names a volume
/// model and an ambient.
pub fn parse_interior(components: &Object) -> Option<InteriorRef> {
    let c = components.child("CInteriorVolumeComponent")?;
    let model = c.str_h(VOLUME_MODEL).filter(|m| !m.trim().is_empty())?;
    let ambient = c.u64("intambientInteriorAmbient").map(|a| a as u32).filter(|a| *a != u32::MAX && *a != 0)?;
    Some(InteriorRef { model: model.to_ascii_lowercase(), ambient })
}

/// An interior's ambient: floor and ceiling colours (as picked, sRGB) and an
/// intensity, by day and by night.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ambient {
    pub day: ([f32; 3], [f32; 3], f32),
    pub night: ([f32; 3], [f32; 3], f32),
}

/// The ambients of `interiorambient.lib`, by key.
pub fn parse_ambients(bytes: &[u8]) -> Vec<(u32, String, Ambient)> {
    let Some(start) = bytes.windows(4).position(|w| w == b"nbCF") else { return Vec::new() };
    let Ok(root) = crate::fcb::parse(&bytes[start..]) else { return Vec::new() };
    root.children
        .iter()
        .filter_map(|o| {
            let key = o.u64("hidKey")? as u32;
            let colour = |n: &str| o.vec3(n).unwrap_or([0.0; 3]);
            let side = |when: &str| (colour(&format!("clr{when}FloorColor")), colour(&format!("clr{when}CeilingColor")), o.f32(&format!("f{when}Intensity")).unwrap_or(1.0));
            Some((key, o.str("hidName").unwrap_or_default(), Ambient { day: side("Daytime"), night: side("Nighttime") }))
        })
        .collect()
}

/// An interior volume placed in the world (Bevy space): a box turned with
/// its entity, the ambient within, and the entity (on its mission layer, it
/// is there only while the layer is).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Interior {
    pub entity: u64,
    pub layer: u64,
    pub centre: [f32; 3],
    /// (x, y, z, w)
    pub rotation: [f32; 4],
    pub half: [f32; 3],
    pub ambient: Ambient,
}

impl Interior {
    /// The volume of a model's bounds (Bevy axes, its own space) where its
    /// entity stands (Dunia position, `hidAngles`).
    pub fn place(entity: u64, layer: u64, bounds: ([f32; 3], [f32; 3]), pos: [f32; 3], angles: [f32; 3], ambient: Ambient) -> Interior {
        let q = crate::world::angles_to_bevy_quat(angles);
        let at = crate::world::pos_to_bevy(pos);
        let (min, max) = bounds;
        let local: [f32; 3] = std::array::from_fn(|i| (min[i] + max[i]) * 0.5);
        let half = std::array::from_fn(|i| (max[i] - min[i]).abs() * 0.5);
        let r = rotate(q, local);
        Interior { entity, layer, centre: std::array::from_fn(|i| at[i] + r[i]), rotation: q, half, ambient }
    }

    /// One line: `interior <entity> <layer> <centre> <rotation> <half>
    /// <day floor> <day ceiling> <day intensity> <night floor> <night
    /// ceiling> <night intensity>` (vectors comma-separated).
    pub fn to_line(&self) -> String {
        let v = |a: &[f32]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        let (d, n) = (self.ambient.day, self.ambient.night);
        format!(
            "interior {:016x} {:016x} {} {} {} {} {} {} {} {} {}",
            self.entity,
            self.layer,
            v(&self.centre),
            v(&self.rotation),
            v(&self.half),
            v(&d.0),
            v(&d.1),
            d.2,
            v(&n.0),
            v(&n.1),
            n.2
        )
    }

    pub fn from_line(line: &str) -> Option<Interior> {
        let mut it = line.split_whitespace();
        (it.next()? == "interior").then_some(())?;
        let entity = u64::from_str_radix(it.next()?, 16).ok()?;
        let layer = u64::from_str_radix(it.next()?, 16).ok()?;
        let vec = |s: &str| -> Option<Vec<f32>> { s.split(',').map(|x| x.parse().ok()).collect() };
        let three = |s: Option<&str>| -> Option<[f32; 3]> { vec(s?)?.try_into().ok() };
        let centre = three(it.next())?;
        let rotation: [f32; 4] = vec(it.next()?)?.try_into().ok()?;
        let half = three(it.next())?;
        let day = (three(it.next())?, three(it.next())?, it.next()?.parse().ok()?);
        let night = (three(it.next())?, three(it.next())?, it.next()?.parse().ok()?);
        Some(Interior { entity, layer, centre, rotation, half, ambient: Ambient { day, night } })
    }
}

/// Rotates a vector by a quaternion (x, y, z, w).
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [v[0] + w * t[0] + (y * t[2] - z * t[1]), v[1] + w * t[1] + (z * t[0] - x * t[2]), v[2] + w * t[2] + (x * t[1] - y * t[0])]
}

pub fn to_text(interiors: &[Interior]) -> String {
    interiors.iter().map(|i| i.to_line() + "\n").collect()
}

pub fn from_text(text: &str) -> Vec<Interior> {
    text.lines().filter_map(Interior::from_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A volume off its entity's origin, the entity turned a quarter about
    /// the vertical: the box turns and moves with it, and a line reads back.
    #[test]
    fn an_interior_is_placed_with_its_entity_and_round_trips() {
        let ambient = Ambient { day: ([0.3, 0.3, 0.3], [0.2, 0.2, 0.2], 0.32), night: ([0.25, 0.3, 0.35], [0.1, 0.15, 0.16], 0.32) };
        // Bevy-axis bounds: 4 m along x, 3 m up, 2 m along z, centred at x = 2.
        let i = Interior::place(7, 0x64, ([0.0, 0.0, -1.0], [4.0, 3.0, 1.0]), [100.0, 200.0, 50.0], [0.0, 0.0, 90.0], ambient);
        assert_eq!(i.half, [2.0, 1.5, 1.0]);
        // Dunia (100, 200, 50) is Bevy (100, 50, -200); the 2 m along x
        // turned a quarter, onto the z axis (either way, by the convention).
        assert!((i.centre[1] - 51.5).abs() < 1e-4, "{:?}", i.centre);
        assert!((i.centre[0] - 100.0).abs() < 1e-4 && ((i.centre[2] + 200.0).abs() - 2.0).abs() < 1e-4, "{:?}", i.centre);
        let back = Interior::from_line(&i.to_line()).unwrap();
        assert_eq!(back, i);
        assert_eq!(from_text(&to_text(&[i, i])).len(), 2);
    }
}
