//! Rooms (`CRoomComponent`, on caves and buildings): a convex volume that,
//! while the listener is inside, sets the sound: a mixing preset (by its
//! key in `soundmixing.lib`), an ambience sound region, a reverb, and the
//! sounds of going in and out.

use crate::fcb::Object;

/// A room as its entity holds it: the volume in the entity's own axes
/// (Dunia, Z up) and the sound settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoomVolume {
    /// `mixRoomSoundPreset`: the mixing's `hidKey` (0: none).
    pub mixing: u64,
    /// `sndregSoundRegion` while `bUseRoomSoundRegion`.
    pub region: Option<u32>,
    /// `sndReverb`, `sndInSound`, `sndOutSound`: events (0: none).
    pub reverb: u32,
    pub enter: u32,
    pub exit: u32,
    /// `vectorScale`, already in the vertices.
    pub scale: [f32; 3],
    pub vertices: Vec<[f32; 3]>,
    pub faces: Vec<Vec<u32>>,
}

fn event(o: &Object, name: &str) -> u32 {
    o.str(name).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).filter(|e| *e != u32::MAX).unwrap_or(0)
}

/// The room on an entity's components, if it has one with a volume.
pub fn parse_room(components: &Object) -> Option<RoomVolume> {
    let room = components.child("CRoomComponent")?;
    let volume = room.child("hidConvexVolume")?;
    let vertices: Vec<[f32; 3]> = volume.child("hidVertices")?.children.iter().filter_map(|v| v.vec3("Vertex")).collect();
    let faces: Vec<Vec<u32>> = volume
        .child("hidFacesDefs")?
        .children
        .iter()
        .filter_map(|f| f.child("hidVertexIndices").map(|ix| ix.children.iter().filter_map(|v| v.u32("Vertex")).collect::<Vec<u32>>()))
        .filter(|f| f.len() >= 3 && f.iter().all(|&i| (i as usize) < vertices.len()))
        .collect();
    if vertices.len() < 4 || faces.len() < 4 {
        return None;
    }
    Some(RoomVolume {
        mixing: room.u64("mixRoomSoundPreset").unwrap_or(0),
        region: room.bool("bUseRoomSoundRegion").unwrap_or(false).then(|| room.field("sndregSoundRegion").and_then(|b| b.first().copied()).unwrap_or(0) as u32),
        reverb: event(room, "sndReverb"),
        enter: event(room, "sndInSound"),
        exit: event(room, "sndOutSound"),
        scale: room.vec3("vectorScale").unwrap_or([1.0; 3]),
        vertices,
        faces,
    })
}

/// A room placed in the world (Bevy space): its planes (`n·p + d <= 0`
/// inside), bounds and sound settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Room {
    pub mixing: u64,
    pub region: Option<u32>,
    pub reverb: u32,
    pub enter: u32,
    pub exit: u32,
    pub planes: Vec<[f32; 4]>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Rotates a vector by a quaternion (x, y, z, w).
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [v[0] + w * t[0] + (y * t[2] - z * t[1]), v[1] + w * t[1] + (z * t[0] - x * t[2]), v[2] + w * t[2] + (x * t[1] - y * t[0])]
}

impl RoomVolume {
    /// The room where its entity stands (Dunia position, `hidAngles`),
    /// turned and moved as the entity's model is.
    pub fn place(&self, pos: [f32; 3], angles: [f32; 3]) -> Room {
        let q = crate::world::angles_to_bevy_quat(angles);
        let at = crate::world::pos_to_bevy(pos);
        let points: Vec<[f32; 3]> = self
            .vertices
            .iter()
            .map(|v| {
                // (The vertices have the scale in them already.)
                let local = crate::mesh::to_bevy(*v);
                let r = rotate(q, local);
                [r[0] + at[0], r[1] + at[1], r[2] + at[2]]
            })
            .collect();
        let n = points.len() as f32;
        let centre = points.iter().fold([0.0; 3], |c, p| [c[0] + p[0] / n, c[1] + p[1] / n, c[2] + p[2] / n]);
        let planes = self
            .faces
            .iter()
            .filter_map(|f| {
                let (a, b, c) = (points[f[0] as usize], points[f[1] as usize], points[f[2] as usize]);
                let mut normal = cross(sub(b, a), sub(c, a));
                let len = dot(normal, normal).sqrt();
                if len < 1e-6 {
                    return None;
                }
                normal = normal.map(|x| x / len);
                let mut d = -dot(normal, a);
                // Outward: the centre on the inside.
                if dot(normal, centre) + d > 0.0 {
                    normal = normal.map(|x| -x);
                    d = -d;
                }
                Some([normal[0], normal[1], normal[2], d])
            })
            .collect();
        let min = points.iter().fold([f32::MAX; 3], |m, p| [m[0].min(p[0]), m[1].min(p[1]), m[2].min(p[2])]);
        let max = points.iter().fold([f32::MIN; 3], |m, p| [m[0].max(p[0]), m[1].max(p[1]), m[2].max(p[2])]);
        Room { mixing: self.mixing, region: self.region, reverb: self.reverb, enter: self.enter, exit: self.exit, planes, min, max }
    }
}

impl Room {
    /// Whether a point (Bevy space) is inside.
    pub fn contains(&self, p: [f32; 3]) -> bool {
        (0..3).all(|i| p[i] >= self.min[i] && p[i] <= self.max[i]) && self.planes.iter().all(|pl| dot([pl[0], pl[1], pl[2]], p) + pl[3] <= 0.0)
    }

    /// How big it is (its bounds' volume), to prefer the smaller of two
    /// rooms one inside another.
    pub fn size(&self) -> f32 {
        (0..3).map(|i| (self.max[i] - self.min[i]).max(0.0)).product()
    }

    /// One line: `room <mixing> <region|-> <reverb> <enter> <exit>
    /// <min> <max> <planes...>` (vectors comma-separated).
    pub fn to_line(&self) -> String {
        let v = |a: &[f32]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        let planes: Vec<String> = self.planes.iter().map(|p| v(p)).collect();
        let region = self.region.map_or_else(|| "-".to_string(), |r| r.to_string());
        format!(
            "room {:016x} {region} {:08x} {:08x} {:08x} {} {} {}",
            self.mixing,
            self.reverb,
            self.enter,
            self.exit,
            v(&self.min),
            v(&self.max),
            planes.join(" ")
        )
    }

    pub fn from_line(line: &str) -> Option<Room> {
        let mut it = line.split_whitespace();
        (it.next()? == "room").then_some(())?;
        let hex = |s: &str| u32::from_str_radix(s, 16).ok();
        let mixing = u64::from_str_radix(it.next()?, 16).ok()?;
        let region = it.next()?.parse().ok();
        let (reverb, enter, exit) = (hex(it.next()?)?, hex(it.next()?)?, hex(it.next()?)?);
        let vec = |s: &str| -> Option<Vec<f32>> { s.split(',').map(|x| x.parse().ok()).collect() };
        let three = |s: &str| -> Option<[f32; 3]> { vec(s)?.try_into().ok() };
        let (min, max) = (three(it.next()?)?, three(it.next()?)?);
        let planes = it.map(|p| vec(p).and_then(|v| v.try_into().ok())).collect::<Option<Vec<[f32; 4]>>>()?;
        Some(Room { mixing, region, reverb, enter, exit, planes, min, max })
    }
}

/// The ambience sound regions (`soundregions_default.obj`), which rooms
/// name by index.
pub const REGIONS_PATH: &str = r"generated\databases\generic\soundregions_default.obj";

/// A sound region: its index (`disUniqueId`), name and looping bed (0:
/// none).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundRegion {
    pub index: u32,
    pub name: String,
    pub bed: u32,
}

pub fn parse_regions(bytes: &[u8]) -> Vec<SoundRegion> {
    let Ok(root) = crate::fcb::parse(bytes) else { return Vec::new() };
    let Some(list) = root.child("SoundRegions") else { return Vec::new() };
    list.children
        .iter()
        .filter_map(|r| {
            let index = r.field("disUniqueId").and_then(|b| b.first().copied())? as u32;
            let bed = r.child("Ambiance").map_or(0, |a| event(a, "sndMultitrackSound"));
            Some(SoundRegion { index, name: r.str("sName").unwrap_or_default(), bed })
        })
        .collect()
}

impl SoundRegion {
    /// `region <index> <bed> <name>`.
    pub fn to_line(&self) -> String {
        format!("region {} {:08x} {}", self.index, self.bed, self.name)
    }

    pub fn from_line(line: &str) -> Option<SoundRegion> {
        let mut it = line.splitn(4, ' ');
        (it.next()? == "region").then_some(())?;
        let index = it.next()?.parse().ok()?;
        let bed = u32::from_str_radix(it.next()?, 16).ok()?;
        Some(SoundRegion { index, bed, name: it.next().unwrap_or_default().to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placed_box_holds_what_is_inside_it() {
        // A 2 m cube (Dunia, Z up) whose faces wind either way.
        let vertices =
            vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0], [2.0, 0.0, 2.0], [2.0, 2.0, 2.0], [0.0, 2.0, 2.0]];
        let faces = vec![vec![0, 1, 2, 3], vec![4, 7, 6, 5], vec![0, 4, 5, 1], vec![3, 2, 6, 7], vec![0, 3, 7, 4], vec![1, 5, 6, 2]];
        let v = RoomVolume { mixing: 0xAC9A_D81A, region: Some(25), scale: [1.0; 3], vertices, faces, ..Default::default() };
        // At Dunia (100, 50, 10), turned a quarter about Z.
        let room = v.place([100.0, 50.0, 10.0], [0.0, 0.0, 90.0]);
        // Its middle: Dunia (100, 50, 10) + the turned (1, 1, 1) = (99, 51, 11),
        // Bevy (99, 11, -51).
        assert!(room.contains([99.0, 11.0, -51.0]));
        assert!(!room.contains([99.0, 13.5, -51.0]));
        assert!(!room.contains([101.5, 11.0, -51.0]));
        assert_eq!(Room::from_line(&room.to_line()), Some(room));
        let region = SoundRegion { index: 25, name: "MEM_INT_Cave".into(), bed: 0x618A2 };
        assert_eq!(SoundRegion::from_line(&region.to_line()), Some(region));
    }
}
