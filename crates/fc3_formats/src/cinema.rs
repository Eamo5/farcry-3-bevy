//! In-engine cinematics: `.cseq` sequences (plain XML), as the converter
//! keeps them in the cache.
//!
//! A sequence is a timeline of track groups, each bound at play time to a
//! world entity (characters, props, cameras), the player, or nothing
//! (overlay: screen effects, field of view, soundtrack). Groups hold clips:
//! animation clips (a `.mab` played from a frame range), dialogue and sound
//! keys, events (holster, draw...), particles, camera cuts and camera
//! animation, the first-person camera's free look. The scene entity that
//! plays a sequence names which world entity each group's `hidUniqueId`
//! binds to; clip root motion is in the scene entity's frame.
//!
//! The cache form is one tab-separated line per sequence, sync point, group
//! and clip; curves are sampled.

use anyhow::{Context, Result};

use crate::xml::Element;

/// The cache folder for converted sequences: `cinema/<crc64 of path>.seq`.
pub const CINEMA_DIR: &str = "cinema";
pub const POSTFX_LIBRARY: &str = r"worlds\fc3_main\generated\cinema_postfx.xml";
/// Preserve the whole source library, including effect types not rendered yet.
pub const POSTFX_CACHE_REL: &str = "cinema/postfx.xml";

/// The cache path of a sequence by its archive path.
pub fn sequence_rel(archive_path: &str) -> String {
    format!("{CINEMA_DIR}/{:016x}.seq", crate::hash::path_hash(archive_path))
}

/// The archive path of a sequence as Domino names it
/// (`Sequences/fc3_main/x/y.seq` -> `sequences\fc3_main\x\y.cseq`).
pub fn archive_path(domino: &str) -> String {
    let p: String = domino.chars().map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() }).collect();
    match p.strip_suffix(".seq") {
        Some(stem) => format!("{stem}.cseq"),
        None if p.ends_with(".cseq") => p,
        None => format!("{p}.cseq"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// A world pawn.
    Character,
    /// The local player.
    Player,
    /// A world entity (prop, vehicle, light, particle emitter).
    Generic,
    /// An entity spawned from an archetype for the sequence.
    Spawn,
    /// A camera entity.
    Camera,
    /// Unbound: screen effects, field of view, soundtrack.
    Overlay,
}

impl GroupKind {
    fn code(self) -> &'static str {
        match self {
            GroupKind::Character => "character",
            GroupKind::Player => "player",
            GroupKind::Generic => "generic",
            GroupKind::Spawn => "spawn",
            GroupKind::Camera => "camera",
            GroupKind::Overlay => "overlay",
        }
    }

    fn from_code(s: &str) -> Option<GroupKind> {
        Some(match s {
            "character" => GroupKind::Character,
            "player" => GroupKind::Player,
            "generic" => GroupKind::Generic,
            "spawn" => GroupKind::Spawn,
            "camera" => GroupKind::Camera,
            "overlay" => GroupKind::Overlay,
            _ => return None,
        })
    }
}

/// A camera key: time (s), position (scene frame, Dunia), rotation
/// quaternion (x, y, z, w; camera +Y looks forward), field of view (deg).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraKey {
    pub time: f32,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub fov: f32,
}

/// A recorded vehicle pose in the scene's frame, Dunia space, plus the
/// controls used by its wheel/engine presentation. Angles are degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleKey {
    pub time: f32,
    pub pos: [f32; 3],
    pub angles: [f32; 3],
    pub gas: f32,
    pub steering: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementKey {
    pub time: f32,
    pub pos: [f32; 3],
    pub angles: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipKind {
    /// A skeletal animation played from frame `from` (30 fps) on.
    Animation {
        source: String,
        from: u32,
        to: u32,
        blend: f32,
        anchored: bool,
    },
    /// A prop's animation.
    SimpleAnimation {
        source: String,
        from: u32,
        to: u32,
    },
    /// A whole-track recording, not an animation clip. It owns the
    /// vehicle's world displacement while simple animation poses its parts.
    VehicleMovement {
        keys: Vec<VehicleKey>,
    },
    /// An authored prop/entity path: numeric channels 0..5, Dunia XYZ/angles.
    Movement {
        keys: Vec<MovementKey>,
    },
    /// A spoken line (voice from the speaker).
    Dialog {
        sound: u32,
    },
    /// A facial clip (`.dpax`) on the character, from frame `from` on.
    Facial {
        source: String,
        from: u32,
        to: u32,
        blend: f32,
    },
    /// A sound (`kind`: the game's sound type).
    Sound {
        sound: u32,
        kind: u32,
    },
    /// A named event (`HideWeaponEvent`, `ShowWeaponEvent`, ...).
    Event {
        name: String,
    },
    Particle {
        effect: String,
        bone: String,
    },
    /// The bound camera takes (`start`) or gives back the view.
    CameraCut {
        start: bool,
        hide_hud: bool,
    },
    /// The bound camera's path.
    CameraAnimation {
        keys: Vec<CameraKey>,
    },
    /// First-person free look within the scene: how far the head turns.
    PlayerCamera {
        head: HeadLook,
    },
    /// Field of view over the clip (degrees, sampled every `FOV_STEP`).
    Fov {
        samples: Vec<f32>,
    },
    /// Authored screen blur, including mask and intensity-control switches.
    Blur {
        texture: String,
        kind: u32,
        priority: u32,
        intensity: [bool; 3],
        /// BlurFactor, RGBA, Direction XY, TextureAlpha, MaskOffset.
        samples: Vec<[f32; 9]>,
    },
}

/// How a player-camera clip lets the head turn (`CPlayerCameraClip`): its
/// speed, the seconds before it turns back and how fast, and its least and
/// most angles (degrees: pitch, roll, yaw).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeadLook {
    pub speed: f32,
    pub return_delay: f32,
    pub return_speed: f32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl HeadLook {
    /// Whether it lets the head turn at all.
    pub fn turns(&self) -> bool {
        self.min.iter().zip(&self.max).any(|(a, b)| b > a)
    }
}

/// Curves are kept sampled at this step (seconds).
pub const FOV_STEP: f32 = 1.0 / 30.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub start: f32,
    pub duration: f32,
    pub kind: ClipKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub uid: u64,
    pub kind: GroupKind,
    pub name: String,
    /// Spawn groups' archetype.
    pub archetype: u64,
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncPoint {
    pub name: String,
    pub time: f32,
    /// 0 out only, 1 in only, 2 in and out.
    pub kind: u32,
    pub pause: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sequence {
    pub name: String,
    pub duration: f32,
    pub looping: bool,
    pub pause_at_end: bool,
    pub cutscene: bool,
    pub groups: Vec<Group>,
    pub sync_points: Vec<SyncPoint>,
}

/// A sound id as the sequences write them (`0x00161782`).
fn sound_id(s: Option<&str>) -> Option<u32> {
    let s = s?.trim();
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok()?,
        None => s.parse().ok()?,
    };
    (v != u32::MAX && v != 0).then_some(v)
}

/// A Bezier float track's keys and time/value tangents, from a clip's
/// `AnimationTrackCollection`, by track name.
fn bezier_keys(clip: &Element, name: &str) -> Vec<BezierKey> {
    let mut out = Vec::new();
    clip.walk(&mut |e| {
        if e.name == "Track" && e.attr("Name") == Some(name) {
            for k in e.child("Keys").into_iter().flat_map(|k| &k.children) {
                let (Some(time), Some(value)) = (k.f32("Time"), k.f32("Value")) else { continue };
                if !time.is_finite() || !value.is_finite() { continue; }
                let tangent = |name| k.child(name).and_then(|t| Some([t.f32("Time")?, t.f32("Value")?]))
                    .filter(|v| v.iter().all(|x| x.is_finite())).unwrap_or([time, value]);
                out.push(BezierKey { time, value, incoming: tangent("TangentIn"), outgoing: tangent("TangentOut") });
            }
        }
    });
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    out.dedup_by(|a, b| a.time == b.time);
    out
}

#[derive(Clone, Copy)]
struct BezierKey {
    time: f32,
    value: f32,
    incoming: [f32; 2],
    outgoing: [f32; 2],
}

fn vehicle_keys(e: &Element) -> Vec<VehicleKey> {
    let names = ["X", "Y", "Z", "Pitch", "Roll", "Yaw", "Gas", "Steering"];
    let tracks: Vec<Vec<BezierKey>> = names.iter().map(|name| bezier_keys(e, name)).collect();
    // An absent position channel isn't a usable recorded path.
    if tracks[..3].iter().any(Vec::is_empty) { return Vec::new(); }
    let end = tracks.iter().filter_map(|k| k.last().map(|k| k.time)).fold(0.0f32, f32::max);
    let steps = ((end / FOV_STEP).ceil() as usize).min(200_000);
    (0..=steps).map(|i| {
        let time = (i as f32 * FOV_STEP).min(end);
        let values: Vec<f32> = tracks.iter().map(|k| sample_bezier(k, time).unwrap_or(0.0)).collect();
        VehicleKey { time, pos: [values[0], values[1], values[2]], angles: [values[3], values[4], values[5]],
            gas: values[6], steering: values[7] }
    }).collect()
}

fn movement_keys(e: &Element) -> Vec<MovementKey> {
    let tracks: Vec<_> = (0..6).map(|i| bezier_keys(e, &i.to_string())).collect();
    if tracks[..3].iter().any(Vec::is_empty) { return Vec::new(); }
    let end = tracks.iter().filter_map(|k| k.last().map(|k| k.time)).fold(0.0f32, f32::max);
    let steps = ((end / FOV_STEP).ceil() as usize).min(200_000);
    (0..=steps).map(|i| {
        let time = (i as f32 * FOV_STEP).min(end);
        let values: Vec<_> = tracks.iter().map(|k| sample_bezier(k, time).unwrap_or(0.0)).collect();
        MovementKey { time, pos: [values[0], values[1], values[2]], angles: [values[3], values[4], values[5]] }
    }).collect()
}

/// Tangents contain both time and value. Solve the cubic time channel
/// before evaluating its value; linear interpolation loses authored easing.
fn sample_bezier(keys: &[BezierKey], t: f32) -> Option<f32> {
    let first = keys.first()?;
    if t <= first.time { return Some(first.value); }
    let i = keys.partition_point(|k| k.time < t);
    let Some(b) = keys.get(i) else { return keys.last().map(|k| k.value) };
    let a = keys[i - 1];
    let cubic = |p: [f32; 4], u: f32| {
        let v = 1.0 - u;
        v * v * v * p[0] + 3.0 * v * v * u * p[1] + 3.0 * v * u * u * p[2] + u * u * u * p[3]
    };
    let tx = [a.time, a.outgoing[0].clamp(a.time, b.time), b.incoming[0].clamp(a.time, b.time), b.time];
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..24 {
        let u = (lo + hi) * 0.5;
        if cubic(tx, u) < t { lo = u; } else { hi = u; }
    }
    Some(cubic([a.value, a.outgoing[1], b.incoming[1], b.value], (lo + hi) * 0.5))
}

fn clip_of(e: &Element) -> Option<Clip> {
    if e.name == "CMovementTrack" {
        let keys = movement_keys(e);
        return Some(Clip { start: 0.0, duration: keys.last()?.time, kind: ClipKind::Movement { keys } });
    }
    if e.name == "CVehicleMovementTrack" {
        let keys = vehicle_keys(e);
        let duration = keys.last()?.time;
        return Some(Clip { start: 0.0, duration, kind: ClipKind::VehicleMovement { keys } });
    }
    let start = e.f32("timeStartTime")?;
    let duration = e.f32("timeDuration").unwrap_or(0.0).max(0.0);
    let kind = match e.name.as_str() {
        "CAnimationClip" | "CAnimationLoopClip" => ClipKind::Animation {
            source: e.attr("sAnimationSource")?.to_ascii_lowercase(),
            from: e.u32("nAnimStartFrame").unwrap_or(0),
            to: e.u32("nAnimStopFrame").unwrap_or(0),
            blend: e.f32("fBlendTime").unwrap_or(0.0),
            anchored: e.bool("bEnableAnchor").unwrap_or(false),
        },
        "CSimpleAnimationClip" => ClipKind::SimpleAnimation {
            source: e.attr("sAnimationSource")?.to_ascii_lowercase(),
            from: e.u32("nAnimStartFrame").unwrap_or(0),
            to: e.u32("nAnimStopFrame").unwrap_or(0),
        },
        "CFacialClip" => ClipKind::Facial {
            source: e.attr("fileFacial")?.to_ascii_lowercase(),
            from: e.u32("nAnimStartFrame").unwrap_or(0),
            to: e.u32("nAnimStopFrame").unwrap_or(0),
            blend: e.f32("fBlendTime").unwrap_or(0.0),
        },
        "CDialogClip" | "CDialogKeyClip" => ClipKind::Dialog { sound: sound_id(e.attr("sndSoundId"))? },
        "CSoundKeyClip" | "CSoundClip" => ClipKind::Sound { sound: sound_id(e.attr("sndSoundId"))?, kind: e.u32("sndtpSoundType").unwrap_or(0) },
        "CEventClip" => {
            let ev = e.attr("sEvent")?;
            ClipKind::Event { name: ev.split_once(',').map(|(_, n)| n).unwrap_or(ev).to_string() }
        }
        "CParticleClip" | "CParticleKeyClip" => {
            ClipKind::Particle { effect: e.attr("psParticles").unwrap_or_default().to_string(), bone: e.attr("sBoneName").unwrap_or_default().to_string() }
        }
        "CCameraCutStart" => ClipKind::CameraCut { start: true, hide_hud: e.bool("bHideGameplayHud").unwrap_or(true) },
        "CCameraCutStop" => ClipKind::CameraCut { start: false, hide_hud: false },
        "CCameraAnimationClip" => ClipKind::CameraAnimation { keys: Vec::new() },
        "CPlayerCameraClip" => {
            let f = |n: &str| e.attr(n).and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
            let angles = |n: &str| {
                let v: Vec<f32> = e.attr(n).unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() == 3 {
                    [v[0], v[1], v[2]]
                } else {
                    [0.0; 3]
                }
            };
            ClipKind::PlayerCamera {
                head: HeadLook {
                    speed: f("fHeadSpeed"),
                    return_delay: f("fHeadReturnDelay"),
                    return_speed: f("fHeadReturnSpeed"),
                    min: angles("vectorHeadMinAngles"),
                    max: angles("vectorHeadMaxAngles"),
                },
            }
        }
        "CFOVClip" => {
            let keys = bezier_keys(e, "FOVOverTime");
            if keys.is_empty() {
                return None;
            }
            let n = ((duration / FOV_STEP).ceil() as usize).clamp(1, 20_000);
            ClipKind::Fov { samples: (0..=n).filter_map(|i| sample_bezier(&keys, i as f32 * FOV_STEP)).collect() }
        }
        "CBlurPostFxClip" => {
            let blur = e.children.iter().find(|e| e.name == "Blur")?;
            let intensity = blur.children.iter().find(|e| e.name == "Intensity");
            let channels = ["BlurFactor", "BlurColor_R", "BlurColor_G", "BlurColor_B", "BlurColor_A",
                "Direction_X", "Direction_Y", "TextureAlpha", "MaskOffset"];
            let keys = channels.map(|name| bezier_keys(e, name));
            if keys.iter().any(|k| k.is_empty()) { return None; }
            let n = ((duration / FOV_STEP).ceil() as usize).clamp(1, 20_000);
            ClipKind::Blur {
                texture: blur.attr("fileTexture").unwrap_or_default().to_string(),
                kind: blur.u32("selBlurType")?, priority: e.u32("nPriority").unwrap_or(0),
                intensity: ["bOnBlurFactor", "bOnTextureAlpha", "bOnMaskOffset"].map(|name|
                    intensity.and_then(|e| e.bool(name)).unwrap_or(false)),
                samples: (0..=n).map(|i| std::array::from_fn(|channel|
                    sample_bezier(&keys[channel], i as f32 * FOV_STEP).unwrap())).collect(),
            }
        }
        _ => return None,
    };
    Some(Clip { start, duration, kind })
}

/// The camera animation resources a sequence's camera clips use (the
/// `.animtrackcol.rml` paths), in clip order.
pub fn camera_resources(root: &Element) -> Vec<String> {
    let mut out = Vec::new();
    root.walk(&mut |e| {
        if e.name == "CCameraAnimationClip" && e.bool("bEnabled") != Some(false) {
            out.push(e.attr("sAnimationTrackCollectionResource").unwrap_or_default().to_ascii_lowercase());
        }
    });
    out
}

const GROUP_ELEMENTS: [(&str, GroupKind); 5] = [
    ("CCharacterTrackGroup", GroupKind::Character),
    ("CPlayerTrackGroup", GroupKind::Player),
    ("CGenericTrackGroup", GroupKind::Generic),
    ("CGenericSpawnTrackGroup", GroupKind::Spawn),
    ("CEntityTrackGroup", GroupKind::Camera),
];

/// Clips under an element (stopping at nested groups), enabled ones only.
fn collect_clips(e: &Element, out: &mut Vec<Clip>) {
    for c in &e.children {
        if c.bool("bEnabled") == Some(false) || GROUP_ELEMENTS.iter().any(|(n, _)| *n == c.name) {
            continue;
        }
        if let Some(clip) = clip_of(c) {
            out.push(clip);
        } else {
            collect_clips(c, out);
        }
    }
}

fn collect_groups(e: &Element, groups: &mut Vec<Group>, overlay: &mut Vec<Clip>) {
    for c in &e.children {
        if c.bool("bEnabled") == Some(false) {
            continue;
        }
        if let Some((_, kind)) = GROUP_ELEMENTS.iter().find(|(n, _)| *n == c.name) {
            let mut clips = Vec::new();
            collect_clips(c, &mut clips);
            groups.push(Group {
                uid: c.u64("hidUniqueId").unwrap_or(0),
                kind: *kind,
                name: c.attr("sName").unwrap_or_default().to_string(),
                archetype: c.u64("arkArchetype").unwrap_or(0),
                clips,
            });
        } else if c.name == "CSequenceTrackGroup" {
            collect_groups(c, groups, overlay);
        } else {
            // Overlay and other unbound tracks.
            collect_clips(&Element { children: vec![c.clone()], ..Default::default() }, overlay);
        }
    }
}

/// A sequence from its XML.
pub fn parse(xml: &str) -> Result<Sequence> {
    let root = crate::xml::parse(xml).context("no XML")?;
    let seq = if root.name == "CCinematicSequence" { &root } else { root.child("CCinematicSequence").context("no CCinematicSequence")? };
    Ok(parse_sequence(seq))
}

/// Read supported timeline tracks from the named gameplay-effect library.
/// The cached XML remains authoritative for unsupported blur/color effects.
pub fn parse_postfx_library(xml: &str) -> Result<Vec<Sequence>> {
    let root = crate::xml::parse(xml).context("no post-FX XML")?;
    anyhow::ensure!(root.name == "CinemaPostFX", "no CinemaPostFX root");
    let mut names = std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in root.children.iter().filter(|e| e.name == "CGameplaySequence") {
        let name = e.attr("sName").context("unnamed gameplay effect")?;
        anyhow::ensure!(!name.is_empty() && names.insert(name.to_ascii_lowercase()), "empty or duplicate gameplay effect {name}");
        out.push(parse_sequence(e));
    }
    anyhow::ensure!(!out.is_empty(), "empty post-FX library");
    Ok(out)
}

fn parse_sequence(seq: &Element) -> Sequence {
    let mut groups = Vec::new();
    let mut overlay = Vec::new();
    collect_groups(seq, &mut groups, &mut overlay);
    if !overlay.is_empty() {
        groups.push(Group { uid: 0, kind: GroupKind::Overlay, name: "overlay".into(), archetype: 0, clips: overlay });
    }
    let mut sync_points = Vec::new();
    if let Some(sp) = seq.child("SyncPoints") {
        for p in &sp.children {
            sync_points.push(SyncPoint {
                name: p.attr("sName").unwrap_or_default().to_string(),
                time: p.f32("timeTime").unwrap_or(0.0),
                kind: p.u32("selSyncPointType").unwrap_or(0),
                pause: p.bool("bPauseWhenReached").unwrap_or(false),
            });
        }
    }
    // Unsupported visual tracks still occupy authored time. A blur-only
    // gameplay sequence must not become a zero-duration loop.
    fn timeline_end(e: &Element) -> f32 {
        if e.bool("bEnabled") == Some(false) { return 0.0; }
        let end = e.f32("timeStartTime").zip(e.f32("timeDuration"))
            .map(|(start, duration)| start + duration.max(0.0)).filter(|v| v.is_finite()).unwrap_or(0.0);
        e.children.iter().map(timeline_end).fold(end, f32::max)
    }
    let duration = groups.iter().flat_map(|g| &g.clips).map(|c| c.start + c.duration).fold(timeline_end(seq), f32::max);
    Sequence {
        name: seq.attr("sName").unwrap_or_default().to_string(),
        duration,
        looping: seq.bool("bLoop").unwrap_or(false),
        pause_at_end: seq.bool("bPauseAtEnd").unwrap_or(false),
        cutscene: seq.bool("bIsCutscene").unwrap_or(false),
        groups,
        sync_points,
    }
}

/// A camera path from its `.animtrackcol.rml` (an FCB: tracks
/// `camera_position` (vec3), `camera_rotation` (quaternion), `camera_fov`),
/// merged on the position track's key times.
pub fn camera_keys(fcb_bytes: &[u8]) -> Result<Vec<CameraKey>> {
    let start = fcb_bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&fcb_bytes[start..])?;
    let name_of = |t: &crate::fcb::Object| t.str_h(0xF96F_AF0D).unwrap_or_default();
    let mut tracks: Vec<(String, Vec<(f32, Vec<u8>)>)> = Vec::new();
    root.walk(&mut |o, _| {
        if !o.is("Track") {
            return;
        }
        let name = name_of(o);
        if name.is_empty() {
            return;
        }
        let mut keys = Vec::new();
        o.walk(&mut |k, _| {
            if k.is("Key") {
                if let (Some(t), Some(v)) = (k.f32("Time"), k.field("Value")) {
                    keys.push((t, v.to_vec()));
                }
            }
        });
        tracks.push((name, keys));
    });
    let get = |n: &str| tracks.iter().find(|(name, _)| name.eq_ignore_ascii_case(n)).map(|(_, k)| k.as_slice()).unwrap_or(&[]);
    let f = |b: &[u8], i: usize| b.get(i * 4..i * 4 + 4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).unwrap_or(0.0);
    let pos = get("camera_position");
    let rot = get("camera_rotation");
    let fov = get("camera_fov");
    let lerp_track = |keys: &[(f32, Vec<u8>)], t: f32, n: usize| -> Vec<f32> {
        if keys.is_empty() {
            return Vec::new();
        }
        let i = keys.iter().position(|k| k.0 >= t).unwrap_or(keys.len() - 1);
        if i == 0 || keys[i].0 <= t {
            return (0..n).map(|j| f(&keys[i].1, j)).collect();
        }
        let (a, b) = (&keys[i - 1], &keys[i]);
        let w = (t - a.0) / (b.0 - a.0).max(1e-6);
        (0..n).map(|j| f(&a.1, j) + (f(&b.1, j) - f(&a.1, j)) * w).collect()
    };
    let times: Vec<f32> = if pos.len() >= rot.len() { pos.iter().map(|k| k.0).collect() } else { rot.iter().map(|k| k.0).collect() };
    let mut out = Vec::new();
    for t in times {
        let p = lerp_track(pos, t, 3);
        let mut q = lerp_track(rot, t, 4);
        let len = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        if len > 1e-6 {
            q.iter_mut().for_each(|x| *x /= len);
        }
        let fv = lerp_track(fov, t, 1);
        out.push(CameraKey {
            time: t,
            pos: [p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0), p.get(2).copied().unwrap_or(0.0)],
            rot: if q.len() == 4 { [q[0], q[1], q[2], q[3]] } else { [0.0, 0.0, 0.0, 1.0] },
            fov: fv.first().copied().unwrap_or(60.0),
        });
    }
    Ok(out)
}

impl Sequence {
    /// The cache form.
    pub fn to_text(&self) -> String {
        let b = |v: bool| if v { "1" } else { "0" };
        let mut out = format!("seq\t{}\t{}\t{}\t{}\t{}\n", self.duration, b(self.looping), b(self.pause_at_end), b(self.cutscene), self.name);
        for s in &self.sync_points {
            out += &format!("sync\t{}\t{}\t{}\t{}\n", s.time, s.kind, b(s.pause), s.name);
        }
        for g in &self.groups {
            out += &format!("group\t{}\t{}\t{}\t{}\n", g.uid, g.kind.code(), g.archetype, g.name);
            for c in &g.clips {
                let body = match &c.kind {
                    ClipKind::Animation { source, from, to, blend, anchored } => format!("anim\t{from}\t{to}\t{blend}\t{source}\t{}", b(*anchored)),
                    ClipKind::SimpleAnimation { source, from, to } => format!("simple\t{from}\t{to}\t{source}"),
                    ClipKind::VehicleMovement { keys } => format!("vehicle\t{}", keys.iter().map(|k|
                        format!("{},{},{},{},{},{},{},{},{}", k.time, k.pos[0], k.pos[1], k.pos[2], k.angles[0], k.angles[1], k.angles[2], k.gas, k.steering)
                    ).collect::<Vec<_>>().join(";")),
                    ClipKind::Movement { keys } => format!("movement\t{}", keys.iter().map(|k|
                        format!("{},{},{},{},{},{},{}", k.time, k.pos[0], k.pos[1], k.pos[2], k.angles[0], k.angles[1], k.angles[2])
                    ).collect::<Vec<_>>().join(";")),
                    ClipKind::Dialog { sound } => format!("dialog\t{sound}"),
                    ClipKind::Facial { source, from, to, blend } => format!("face\t{from}\t{to}\t{blend}\t{source}"),
                    ClipKind::Sound { sound, kind } => format!("sound\t{sound}\t{kind}"),
                    ClipKind::Event { name } => format!("event\t{name}"),
                    ClipKind::Particle { effect, bone } => format!("particle\t{effect}\t{bone}"),
                    ClipKind::CameraCut { start, hide_hud } => format!("cut\t{}\t{}", b(*start), b(*hide_hud)),
                    ClipKind::CameraAnimation { keys } => {
                        let k: Vec<String> = keys
                            .iter()
                            .map(|k| format!("{},{},{},{},{},{},{},{},{}", k.time, k.pos[0], k.pos[1], k.pos[2], k.rot[0], k.rot[1], k.rot[2], k.rot[3], k.fov))
                            .collect();
                        format!("cam\t{}", k.join(";"))
                    }
                    ClipKind::PlayerCamera { head: h } => format!(
                        "pcam\t{}",
                        [h.speed, h.return_delay, h.return_speed, h.min[0], h.min[1], h.min[2], h.max[0], h.max[1], h.max[2]].map(|v| v.to_string()).join(",")
                    ),
                    ClipKind::Fov { samples } => format!("fov\t{}", samples.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(";")),
                    ClipKind::Blur { texture, kind, priority, intensity, samples } => format!("blur\t{texture}\t{kind}\t{priority}\t{},{},{}\t{}",
                        b(intensity[0]), b(intensity[1]), b(intensity[2]), samples.iter().map(|sample|
                            sample.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join(";")),
                };
                out += &format!("clip\t{}\t{}\t{body}\n", c.start, c.duration);
            }
        }
        out
    }

    pub fn from_text(text: &str) -> Option<Sequence> {
        let mut seq = Sequence::default();
        let mut seen_header = false;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            let num = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            let uint = |i: usize| f.get(i).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let flag = |i: usize| f.get(i) == Some(&"1");
            let text = |i: usize| f.get(i).copied().unwrap_or_default().to_string();
            match f[0] {
                "seq" => {
                    seen_header = true;
                    seq.duration = num(1);
                    seq.looping = flag(2);
                    seq.pause_at_end = flag(3);
                    seq.cutscene = flag(4);
                    seq.name = text(5);
                }
                "sync" => seq.sync_points.push(SyncPoint { time: num(1), kind: uint(2), pause: flag(3), name: text(4) }),
                "group" => seq.groups.push(Group {
                    uid: f.get(1).and_then(|v| v.parse().ok()).unwrap_or(0),
                    kind: GroupKind::from_code(f.get(2).copied().unwrap_or_default())?,
                    archetype: f.get(3).and_then(|v| v.parse().ok()).unwrap_or(0),
                    name: text(4),
                    clips: Vec::new(),
                }),
                "clip" => {
                    let (start, duration) = (num(1), num(2));
                    let g = |i: usize| f.get(i + 3).copied().unwrap_or_default();
                    let gn = |i: usize| g(i).parse::<f32>().unwrap_or(0.0);
                    let gu = |i: usize| g(i).parse::<u32>().unwrap_or(0);
                    let kind = match f.get(3).copied().unwrap_or_default() {
                        "anim" => ClipKind::Animation { from: gu(1), to: gu(2), blend: gn(3), source: g(4).to_string(), anchored: g(5) == "1" },
                        "simple" => ClipKind::SimpleAnimation { from: gu(1), to: gu(2), source: g(3).to_string() },
                        "movement" => ClipKind::Movement { keys: g(1).split(';').filter_map(|k| {
                            let v: Vec<f32> = k.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 7 && v.iter().all(|x| x.is_finite())).then(|| MovementKey {
                                time: v[0], pos: [v[1], v[2], v[3]], angles: [v[4], v[5], v[6]],
                            })
                        }).collect() },
                        "vehicle" => ClipKind::VehicleMovement { keys: g(1).split(';').filter_map(|k| {
                            let v: Vec<f32> = k.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 9 && v.iter().all(|x| x.is_finite())).then(|| VehicleKey { time: v[0], pos: [v[1], v[2], v[3]],
                                angles: [v[4], v[5], v[6]], gas: v[7], steering: v[8] })
                        }).collect() },
                        "dialog" => ClipKind::Dialog { sound: gu(1) },
                        "face" => ClipKind::Facial { from: gu(1), to: gu(2), blend: gn(3), source: g(4).to_string() },
                        "sound" => ClipKind::Sound { sound: gu(1), kind: gu(2) },
                        "event" => ClipKind::Event { name: g(1).to_string() },
                        "particle" => ClipKind::Particle { effect: g(1).to_string(), bone: g(2).to_string() },
                        "cut" => ClipKind::CameraCut { start: g(1) == "1", hide_hud: g(2) == "1" },
                        "cam" => ClipKind::CameraAnimation {
                            keys: g(1)
                                .split(';')
                                .filter_map(|k| {
                                    let v: Vec<f32> = k.split(',').filter_map(|x| x.parse().ok()).collect();
                                    (v.len() == 9).then(|| CameraKey { time: v[0], pos: [v[1], v[2], v[3]], rot: [v[4], v[5], v[6], v[7]], fov: v[8] })
                                })
                                .collect(),
                        },
                        "pcam" => {
                            let v: Vec<f32> = g(1).split(',').filter_map(|x| x.parse().ok()).collect();
                            let head = if v.len() == 9 {
                                HeadLook { speed: v[0], return_delay: v[1], return_speed: v[2], min: [v[3], v[4], v[5]], max: [v[6], v[7], v[8]] }
                            } else {
                                HeadLook::default()
                            };
                            ClipKind::PlayerCamera { head }
                        }
                        "fov" => ClipKind::Fov { samples: g(1).split(';').filter_map(|x| x.parse().ok()).collect() },
                        "blur" => {
                            let switches: Vec<u8> = g(4).split(',').map(str::parse).collect::<Result<_, _>>().ok()?;
                            if switches.len() != 3 || switches.iter().any(|v| *v > 1) { return None; }
                            let samples = g(5).split(';').map(|row| {
                                let values: Vec<f32> = row.split(',').map(str::parse).collect::<Result<_, _>>().ok()?;
                                if values.iter().any(|v| !v.is_finite()) { return None; }
                                values.try_into().ok()
                            }).collect::<Option<Vec<[f32; 9]>>>()?;
                            ClipKind::Blur { texture: g(1).to_string(), kind: g(2).parse().ok()?, priority: g(3).parse().ok()?,
                                intensity: [switches[0] != 0, switches[1] != 0, switches[2] != 0], samples }
                        }
                        _ => continue,
                    };
                    seq.groups.last_mut()?.clips.push(Clip { start, duration, kind });
                }
                _ => {}
            }
        }
        seen_header.then_some(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gameplay_effects_preserve_easing_names_and_unsupported_track_duration() {
        let xml = r#"<CinemaPostFX>
          <CGameplaySequence sName="postfx:eased" bLoop="1">
            <CSequenceTrackGroup><COverlayTrack>
              <CFOVClip timeStartTime="0" timeDuration="1">
                <Track Name="FOVOverTime"><Keys>
                  <Key Time="0" Value="60"><TangentOut Time="0.33333333" Value="60"/></Key>
                  <Key Time="1" Value="100"><TangentIn Time="0.66666667" Value="60"/></Key>
                </Keys></Track>
              </CFOVClip>
              <CBlurPostFxClip timeStartTime="1" timeDuration="2"/>
            </COverlayTrack>
            <COverlayTrack bEnabled="0"><CBlurPostFxClip timeStartTime="0" timeDuration="99"/></COverlayTrack>
            </CSequenceTrackGroup>
          </CGameplaySequence>
        </CinemaPostFX>"#;
        let effects = parse_postfx_library(xml).unwrap();
        assert_eq!(effects[0].name, "postfx:eased");
        assert!(effects[0].looping);
        assert_eq!(effects[0].duration, 3.0);
        let ClipKind::Fov { samples } = &effects[0].groups[0].clips[0].kind else { panic!("FOV missing") };
        // Uniform time handles and value handles at 60 produce 60 + 40*t^3,
        // not the old straight line (80 at the midpoint).
        assert!((samples[(0.5 / FOV_STEP).round() as usize] - 65.0).abs() < 0.001);
        assert_eq!(samples[0], 60.0);
        assert!((samples.last().unwrap() - 100.0).abs() < 0.001);
        assert!(parse_postfx_library(&xml.replace("</CinemaPostFX>",
            "<CGameplaySequence sName=\"POSTFX:EASED\"/></CinemaPostFX>")).is_err());
        assert!(parse_postfx_library("<CinemaPostFX/>").is_err());
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_greenhouse_movement_keeps_its_path_and_cue_timing() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let bytes = vfs.read(r"sequences\fc3_main\doc01_b02\seq_doc01_b02_hauntedgreenhouse.cseq").unwrap();
        let seq = parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert!((seq.duration - 4.26667).abs() < 0.001);
        assert_eq!(seq.sync_points.len(), 3);
        assert!(seq.sync_points.iter().all(|s| s.time < seq.duration));
        let ClipKind::Movement { keys } = &seq.groups[0].clips[0].kind else { panic!("prop movement missing") };
        let first = keys.first().unwrap();
        assert!((first.pos[0] + 15.4969).abs() < 0.001);
        assert!((first.pos[1] + 58.0762).abs() < 0.001);
        assert!((first.angles[2] - 75.9999).abs() < 0.001);
        assert_eq!(keys.last().unwrap().time, seq.duration);
        let text = seq.to_text();
        assert_eq!(Sequence::from_text(&text).unwrap().to_text(), text);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_postfx_library_preserves_vine_curve_and_mushroom_loop_timing() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let bytes = vfs.read(POSTFX_LIBRARY).unwrap();
        let effects = parse_postfx_library(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let vine = effects.iter().find(|e| e.name == "postfx:Mushroom_Sequence.Vine_Climbing").unwrap();
        assert!((vine.duration - 5.001).abs() < 0.001);
        let samples = vine.groups.iter().flat_map(|g| &g.clips).find_map(|c| match &c.kind {
            ClipKind::Fov { samples } => Some(samples), _ => None,
        }).unwrap();
        assert_eq!(samples[0], 85.0);
        assert!(samples.iter().copied().fold(0.0f32, f32::max) > 149.0);
        assert!((samples.last().unwrap() - 85.0).abs() < 0.1);
        for (name, alpha) in [("transition", [1.0, 1.0]), ("transition_IN", [0.0, 1.0]), ("transition_OUT", [1.0, 0.0])] {
            let effect = effects.iter().find(|e| e.name == format!("postfx:mush_postfx.{name}")).unwrap();
            let ClipKind::Blur { texture, kind, priority, intensity, samples } = &effect.groups.iter()
                .flat_map(|g| &g.clips).find(|c| matches!(c.kind, ClipKind::Blur { .. })).unwrap().kind else { unreachable!() };
            assert_eq!(texture, r"graphics\postfx\tunnel.xbt");
            assert_eq!(*kind, 1);
            assert_eq!(*priority, 0);
            assert_eq!(*intensity, [true; 3]);
            assert!(samples.iter().all(|v| (v[0] - 5.0).abs() < 0.001
                && v[1..5].iter().all(|c| c.abs() < 0.001) && v[5..7].iter().all(|c| (*c - 1.0).abs() < 0.001)));
            for (sample, expected) in [(samples.first().unwrap(), alpha[0]), (samples.last().unwrap(), alpha[1])] {
                assert!((sample[7] - expected).abs() < 0.001);
                assert!((sample[8] - expected).abs() < 0.001);
            }
            assert_eq!(Sequence::from_text(&effect.to_text()).unwrap(), *effect);
        }
        let mush = effects.iter().find(|e| e.name == "postfx:mush_postfx.03_LOOP").unwrap();
        assert!(mush.looping);
        assert_eq!(mush.duration, 1.0);
        assert!(mush.groups.iter().flat_map(|g| &g.clips).any(|c|
            matches!(c.kind, ClipKind::Sound { sound: 0x000a95ae, .. })));
    }

    const XML: &str = r#"<Sequence>
	<CCinematicSequence sName="test" bLoop="0" bPauseAtEnd="0" bIsCutscene="1">
		<CSequenceTrackGroup sName="Track Group" hidUniqueId="1" bEnabled="1">
			<CCharacterTrackGroup sName="Actor_0" hidUniqueId="42" bEnabled="1">
				<CAnimationSequenceTrack sName="Animation Track" hidUniqueId="2" bEnabled="1">
					<CAnimationClip sName="a" hidUniqueId="3" bEnabled="1" timeStartTime="0" timeDuration="10" nAnimStartFrame="30" nAnimStopFrame="330" sAnimationSource="graphics\x\a.mab" fBlendTime="0.3" />
				</CAnimationSequenceTrack>
				<CDialogSequenceTrack bEnabled="1"><CDialogKeyClip timeStartTime="2.5" timeDuration="1e-006" sndSoundId="0x00190A6D" bEnabled="1"/></CDialogSequenceTrack>
			</CCharacterTrackGroup>
			<CPlayerTrackGroup sName="Player Group" hidUniqueId="43" bEnabled="0" />
			<COverlayTrack bEnabled="1"><CSoundKeyClip timeStartTime="1" timeDuration="1e-006" sndSoundId="0x00000010" sndtpSoundType="15" bEnabled="1"/></COverlayTrack>
		</CSequenceTrackGroup>
		<SyncPoints><CSequenceSyncPoint sName="go" timeTime="0.15" selSyncPointType="2" bPauseWhenReached="0" /></SyncPoints>
	</CCinematicSequence>
</Sequence>"#;

    #[test]
    fn parses_and_round_trips() {
        let s = parse(XML).unwrap();
        assert_eq!(s.duration, 10.0);
        assert_eq!(s.groups.len(), 2, "disabled groups are dropped, overlay kept");
        assert_eq!(s.groups[0].uid, 42);
        assert_eq!(s.groups[0].clips[1].kind, ClipKind::Dialog { sound: 0x0019_0A6D });
        assert_eq!(s.groups[1].kind, GroupKind::Overlay);
        assert_eq!(s.sync_points[0].name, "go");
        let back = Sequence::from_text(&s.to_text()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn domino_paths() {
        assert_eq!(archive_path("Sequences/fc3_main/JAS_020_BRIEF/JAS_020_BRIEF_Escape.seq"), r"sequences\fc3_main\jas_020_brief\jas_020_brief_escape.cseq");
    }

    #[test]
    fn vehicle_tracks_keep_curves_controls_anchors_and_the_last_pose() {
        let track = |name: &str, a: f32, b: f32| format!(r#"<Track><Track Name="{name}"><Keys>
            <Key Time="0" Value="{a}"><TangentOut Time="0.25" Value="{a}"/></Key>
            <Key Time="1" Value="{b}"><TangentIn Time="0.75" Value="{b}"/></Key>
            </Keys></Track></Track>"#);
        let curves = [track("X", 1.0, 11.0), track("Y", 2.0, 4.0), track("Z", 3.0, 8.0),
            track("Yaw", -90.0, -80.0), track("Gas", 0.0, 1.0), track("Steering", 0.0, -0.5)].join("");
        let xml = format!(r#"<CCinematicSequence><CSequenceTrackGroup><CGenericTrackGroup hidUniqueId="9">
            <CVehicleMovementTrack><Curves>{curves}</Curves></CVehicleMovementTrack>
            <CVehicleMovementTrack bEnabled="0"><Curves>{curves}</Curves></CVehicleMovementTrack>
            </CGenericTrackGroup><CCharacterTrackGroup hidUniqueId="10">
            <CAnimationClip timeStartTime="0" timeDuration="2" sAnimationSource="x.mab" bEnableAnchor="1"/>
            </CCharacterTrackGroup></CSequenceTrackGroup></CCinematicSequence>"#);
        let seq = parse(&xml).unwrap();
        assert_eq!(seq.duration, 2.0);
        assert_eq!(seq.groups[0].clips.len(), 1);
        let ClipKind::VehicleMovement { keys } = &seq.groups[0].clips[0].kind else { panic!("dropped vehicle movement") };
        assert_eq!(keys.first().unwrap().pos, [1.0, 2.0, 3.0]);
        assert_eq!(keys.last().unwrap().time, 1.0);
        assert_eq!(keys.last().unwrap().pos, [11.0, 4.0, 8.0]);
        let mid = keys.iter().find(|k| (k.time - 0.5).abs() < 1e-5).unwrap();
        assert!((mid.pos[0] - 6.0).abs() < 1e-4);
        assert!((mid.gas - 0.5).abs() < 1e-4);
        assert!((mid.steering + 0.25).abs() < 1e-4);
        assert!(matches!(seq.groups[1].clips[0].kind, ClipKind::Animation { anchored: true, .. }));
        assert_eq!(Sequence::from_text(&seq.to_text()).unwrap(), seq);
    }

    #[test]
    fn bezier_time_channel_is_solved_and_edges_are_held() {
        let keys = [BezierKey { time: 0.0, value: 0.0, incoming: [0.0, 0.0], outgoing: [0.0, 0.0] },
            BezierKey { time: 1.0, value: 1.0, incoming: [0.0, 1.0], outgoing: [1.0, 1.0] }];
        // x(u)=u^3; y(u)=3u^2-2u^3. x=.125 therefore u=.5, y=.5.
        assert!((sample_bezier(&keys, 0.125).unwrap() - 0.5).abs() < 1e-5);
        assert_eq!(sample_bezier(&keys, -1.0), Some(0.0));
        assert_eq!(sample_bezier(&keys, 2.0), Some(1.0));
        assert_eq!(sample_bezier(&[], 0.0), None);
    }

    #[test]
    fn old_animation_cache_defaults_to_no_anchor() {
        let seq = Sequence::from_text("seq\t1\t0\t0\t1\told\ngroup\t1\tcharacter\t0\tactor\nclip\t0\t1\tanim\t0\t30\t0.3\tx.mab\n").unwrap();
        assert!(matches!(seq.groups[0].clips[0].kind, ClipKind::Animation { anchored: false, .. }));
    }
}
