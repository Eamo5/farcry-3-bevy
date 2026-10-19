//! Faces: a character's face rig posed from facial clips
//! (`fc3_formats::facial`) over its body pose.
//!
//! A face is a set of poses (jaw open, lips, brows, blinks...); clips are a
//! weight per pose over time, and the rig turns the weights into the face
//! bones' transforms. Scenes play their facial tracks on the characters they
//! hold (`cinema` sets the clip and time each frame), and a spoken line plays
//! its lip-sync clip on the speaker. The rigs and clips come from the cache's
//! pack, decoded the first time they're needed.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::transform::TransformSystems;
use fc3_formats::facial::{self, FaceChoice, FaceClip, FaceRig, PakIndex};
use fc3_formats::hash::path_hash;

use crate::characters::CharacterRig;
use crate::data::CacheInfo;

pub struct FacesPlugin;

impl Plugin for FacesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Voices>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_faces)
            .add_systems(Update, (start_voices, attach_kit_faces, mouth_where_said))
            .add_systems(PostUpdate, pose_faces.after(crate::characters::animate_characters).before(TransformSystems::Propagate));
    }
}

/// Faces farther than this from the camera (m) keep their body pose.
const FACE_RANGE: f32 = 30.0;

/// The library's blinking loop, under whatever else plays (not in scenes,
/// whose tracks blink of their own).
const IDLE_CLIP: &str = r"graphics\characters\_common\facial\library\lib_face_neutralblinks_long_01.dpax";

/// The cache's face rigs and clips, decoded as needed.
#[derive(Resource, Default)]
pub struct FaceLibrary {
    pak: Option<(PathBuf, PakIndex)>,
    /// Definition key -> its rig's path hash.
    definitions: HashMap<u32, u64>,
    /// Graphic kit id -> the definition key its characters wear.
    kit_faces: HashMap<u64, u32>,
    rigs: HashMap<u64, Option<Arc<FaceRig>>>,
    clips: HashMap<u64, Option<Arc<FaceClip>>>,
}

impl FaceLibrary {
    fn open(root: &std::path::Path) -> anyhow::Result<FaceLibrary> {
        let path = root.join(facial::PAK_REL);
        let mut f = std::fs::File::open(&path)?;
        let mut first = [0u8; 8];
        f.read_exact(&mut first)?;
        let mut header = vec![0u8; PakIndex::header_len(&first)?];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut header)?;
        let index = PakIndex::parse(&header)?;
        let definitions = facial::definitions_from_text(&std::fs::read_to_string(root.join(facial::DEFINITIONS_REL))?);
        let kit_faces = std::fs::read_to_string(root.join(facial::KIT_FACES_REL)).map(|t| facial::kit_faces_from_text(&t)).unwrap_or_default();
        Ok(FaceLibrary { pak: Some((path, index)), definitions, kit_faces, ..Default::default() })
    }

    fn read(&self, hash: u64) -> Option<Vec<u8>> {
        let (path, index) = self.pak.as_ref()?;
        let (at, len) = index.find(hash)?;
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::Start(at as u64)).ok()?;
        let mut bytes = vec![0u8; len as usize];
        f.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }

    /// The rig a definition key names.
    pub fn rig(&mut self, key: u32) -> Option<Arc<FaceRig>> {
        let hash = *self.definitions.get(&key)?;
        if let Some(r) = self.rigs.get(&hash) {
            return r.clone();
        }
        let rig = self.read(hash).and_then(|b| facial::parse_rig(&b).map_err(|e| warn!("face rig {hash:016x}: {e:#}")).ok()).map(Arc::new);
        self.rigs.insert(hash, rig.clone());
        rig
    }

    /// The rig for a character's `CFacialPoseComponent`.
    pub fn rig_for(&mut self, component: &fc3_formats::fcb::Object) -> Option<Arc<FaceRig>> {
        self.rig(FaceChoice::from_component(component).definition(&[])?)
    }

    /// The idle loop faces play.
    pub fn idle(&mut self) -> Option<Arc<FaceClip>> {
        self.clip(IDLE_CLIP)
    }

    /// A facial clip by archive path, if the game has it.
    pub fn clip(&mut self, path: &str) -> Option<Arc<FaceClip>> {
        let hash = path_hash(path);
        if let Some(c) = self.clips.get(&hash) {
            return c.clone();
        }
        let clip = self.read(hash).and_then(|b| facial::parse_clip(&b).map_err(|e| warn!("facial clip {path}: {e:#}")).ok()).map(Arc::new);
        self.clips.insert(hash, clip.clone());
        clip
    }
}

fn load_faces(info: Option<Res<CacheInfo>>, mut commands: Commands) {
    let library = info.and_then(|i| FaceLibrary::open(&i.root).map_err(|e| warn!("faces: {e:#}")).ok()).unwrap_or_default();
    info!("faces: {} rig definitions, {} files", library.definitions.len(), library.pak.as_ref().map_or(0, |p| p.1.len()));
    commands.insert_resource(library);
}

/// A character's face: its rig, and what plays on it.
#[derive(Component)]
pub struct Face {
    pub rig: Arc<FaceRig>,
    /// Rig bone -> the character's joint (found once the rig is built).
    joints: Option<Vec<(usize, Entity)>>,
    /// A scene's facial clip and the time into it (set each frame by the
    /// scene playing the character).
    pub scene: Option<(Arc<FaceClip>, f32)>,
    /// A spoken line's lip-sync clip and the time into it.
    pub voice: Option<(Arc<FaceClip>, f32)>,
    /// The idle loop (blinks) and the time into it.
    idle: Option<(Arc<FaceClip>, f32)>,
}

impl Face {
    /// A face on `rig`, blinking with `idle` from `phase` seconds in (so
    /// faces don't blink together).
    pub fn new(rig: Arc<FaceRig>, idle: Option<Arc<FaceClip>>, phase: f32) -> Face {
        Face { rig, joints: None, scene: None, voice: None, idle: idle.map(|c| (c, phase)) }
    }
}

/// Gives generic characters the face their kit wears, blinking. Kits the
/// game gives no face of their own wear the face of a kit of the same
/// faction (the name before the first `_`) that has one.
fn attach_kit_faces(
    library: Option<ResMut<FaceLibrary>>,
    chars: Option<Res<crate::characters::CharacterAssets>>,
    rigs: Query<(Entity, &crate::characters::KitId), (With<CharacterRig>, Without<Face>)>,
    mut fallback: Local<Option<HashMap<u64, u32>>>,
    mut commands: Commands,
) {
    let Some(mut library) = library else { return };
    if rigs.is_empty() || library.kit_faces.is_empty() {
        return;
    }
    let fallback = fallback.get_or_insert_with(|| {
        let kits = chars.as_ref().map(|c| c.kits.as_slice()).unwrap_or_default();
        let faction = |name: &str| name.split('_').next().unwrap_or(name).to_ascii_lowercase();
        kits.iter()
            .filter(|k| !library.kit_faces.contains_key(&k.id))
            .filter_map(|k| {
                let same = kits.iter().find(|o| faction(&o.name) == faction(&k.name) && library.kit_faces.contains_key(&o.id))?;
                Some((k.id, library.kit_faces[&same.id]))
            })
            .collect()
    });
    for (e, kit) in &rigs {
        let key = library.kit_faces.get(&kit.0).or_else(|| fallback.get(&kit.0)).copied();
        let rig = key.and_then(|k| library.rig(k));
        let phase = ((e.to_bits() & 0xffff_ffff) as f32 * 0.618).fract() * 4.0;
        match rig {
            Some(rig) => commands.entity(e).insert(Face::new(rig, library.idle(), phase)),
            // (No face: don't look again.)
            None => commands.entity(e).remove::<crate::characters::KitId>(),
        };
    }
}

/// A spoken line played where a face is (a villager calling the player
/// over, the poker players, the race keepers, the knife thrower): that face
/// mouths it, where the game has the line's lip-sync. (What isn't speech
/// has none.)
fn mouth_where_said(mut said: MessageReader<crate::sound::PlaySfx>, library: Option<ResMut<FaceLibrary>>, mut faces: Query<(&mut Face, &GlobalTransform)>) {
    let Some(mut library) = library else {
        said.clear();
        return;
    };
    for s in said.read() {
        let Some(at) = s.at else { continue };
        let Some(clip) = library.clip(&facial::voice_clip_path(s.event)) else { continue };
        // (The face whose feet are under where it was said, within a
        // stride.)
        let near = |t: &GlobalTransform| {
            let d = t.translation() - at;
            (d.y.abs() < 2.5).then(|| Vec2::new(d.x, d.z).length()).filter(|r| *r < 1.2)
        };
        let Some((mut face, _)) = faces.iter_mut().filter_map(|(f, t)| near(t).map(|r| (f, r))).min_by(|a, b| a.1.total_cmp(&b.1)) else { continue };
        // (Not again when its speaker already set it going.)
        if !face.voice.as_ref().is_some_and(|(c, _)| Arc::ptr_eq(c, &clip)) {
            face.voice = Some((clip, 0.0));
            debug!("faces: line {:#x} mouthed where it was said", s.event);
        }
    }
}

/// The face a takedown's victim pulls with their half of it (a body clip,
/// its path): the game keeps it beside the clip (`..._+000fw_facial.dpax`
/// for `..._+000fw_target.mab`, or the clip's own name with `_facial`) or
/// among the takedowns' faces (`facial\takedowns\3rdge_facial_...` for
/// `3rdge_fulb_...`).
pub fn takedown_face(library: &mut FaceLibrary, body_clip: &str) -> Option<Arc<FaceClip>> {
    let stem = body_clip.strip_suffix(".mab").unwrap_or(body_clip);
    let file = stem.rsplit_once('\\').map_or(stem, |(_, f)| f);
    let untargeted = stem.strip_suffix("_target").unwrap_or(stem);
    let shared = file.replacen("3rdge_fulb_", "3rdge_facial_", 1);
    let candidates = [
        format!("{untargeted}_facial.dpax"),
        format!("{stem}_facial.dpax"),
        format!(r"graphics\characters\_common\facial\takedowns\{shared}.dpax"),
        format!(r"graphics\characters\_common\facial\takedowns\{}.dpax", shared.strip_suffix("_target").unwrap_or(&shared)),
    ];
    candidates.iter().find_map(|c| library.clip(c))
}

/// Spoken lines whose speakers' faces should move: (story id, sound id).
#[derive(Resource, Default)]
pub struct Voices(pub Vec<(u64, u32)>);

fn start_voices(
    mut voices: ResMut<Voices>,
    library: Option<ResMut<FaceLibrary>>,
    actors: Query<&crate::story_actors::StoryActor>,
    mut faces: Query<&mut Face>,
) {
    let Some(mut library) = library else { return };
    for (speaker, sound) in voices.0.drain(..) {
        let Some(rig) = actors.iter().find(|a| a.id == speaker).and_then(|a| a.rig) else { continue };
        let Ok(mut face) = faces.get_mut(rig) else { continue };
        face.voice = library.clip(&facial::voice_clip_path(sound)).map(|c| (c, 0.0));
    }
}

/// Poses each nearby face from the clips playing on it, over the body pose.
fn pose_faces(
    time: Res<Time>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut faces: Query<(&mut Face, &CharacterRig, &GlobalTransform)>,
    mut joints: Query<&mut Transform>,
) {
    let cam = cameras.iter().next().map(|c| c.translation()).unwrap_or_default();
    let dt = time.delta_secs();
    for (mut face, rig, gt) in &mut faces {
        let face = &mut *face;
        if let Some((clip, t)) = &mut face.voice {
            *t += dt;
            if *t > clip.duration {
                face.voice = None;
            }
        }
        if let Some((clip, t)) = &mut face.idle {
            *t = (*t + dt) % clip.duration.max(0.1);
        }
        // A scene sets its clip again next frame if it still plays.
        let scene = face.scene.take();
        if gt.translation().distance(cam) > FACE_RANGE {
            continue;
        }
        let face_rig = face.rig.clone();
        let bones = face.joints.get_or_insert_with(|| {
            let found: Vec<(usize, Entity)> = face_rig.bones.iter().enumerate().filter_map(|(i, b)| rig.joint(b.name_hash).map(|(_, e)| (i, e))).collect();
            debug!("face: {} of {} rig bones on the character", found.len(), face_rig.bones.len());
            found
        });
        // (Tests: FC3_NO_FACES leaves faces on the body pose.)
        if std::env::var_os("FC3_NO_FACES").is_some() {
            continue;
        }
        let mut weights = face.rig.zero_weights();
        let idle = face.idle.as_ref().filter(|_| scene.is_none());
        for (clip, t) in scene.iter().chain(face.voice.iter()).chain(idle) {
            clip.add_weights(*t, 1.0, &face.rig.channel_map(clip), &mut weights);
        }
        for &(bone, joint) in bones.iter() {
            let Ok(mut tf) = joints.get_mut(joint) else { continue };
            let (t, q) = face.rig.bone_local(bone, &weights);
            tf.translation = Vec3::from_array(t);
            tf.rotation = Quat::from_array(q);
        }
    }
}
