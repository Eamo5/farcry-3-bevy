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

#[derive(Debug, Clone, PartialEq)]
pub enum ClipKind {
    /// A skeletal animation played from frame `from` (30 fps) on.
    Animation {
        source: String,
        from: u32,
        to: u32,
        blend: f32,
    },
    /// A prop's animation.
    SimpleAnimation {
        source: String,
        from: u32,
        to: u32,
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
    /// First-person free look within the scene.
    PlayerCamera,
    /// Field of view over the clip (degrees, sampled every `FOV_STEP`).
    Fov {
        samples: Vec<f32>,
    },
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

/// A Bezier float track's keys (time, value), from a clip's
/// `AnimationTrackCollection`, by track name.
fn bezier_keys(clip: &Element, name: &str) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    clip.walk(&mut |e| {
        if e.name == "Track" && e.attr("Name") == Some(name) {
            e.walk(&mut |k| {
                if k.name == "Key" {
                    if let (Some(t), Some(v)) = (k.f32("Time"), k.f32("Value")) {
                        out.push((t, v));
                    }
                }
            });
        }
    });
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn sample_keys(keys: &[(f32, f32)], t: f32) -> Option<f32> {
    let first = keys.first()?;
    if t <= first.0 {
        return Some(first.1);
    }
    for w in keys.windows(2) {
        if t <= w[1].0 {
            let f = if w[1].0 > w[0].0 { (t - w[0].0) / (w[1].0 - w[0].0) } else { 1.0 };
            return Some(w[0].1 + (w[1].1 - w[0].1) * f);
        }
    }
    keys.last().map(|k| k.1)
}

fn clip_of(e: &Element) -> Option<Clip> {
    let start = e.f32("timeStartTime")?;
    let duration = e.f32("timeDuration").unwrap_or(0.0).max(0.0);
    let kind = match e.name.as_str() {
        "CAnimationClip" | "CAnimationLoopClip" => ClipKind::Animation {
            source: e.attr("sAnimationSource")?.to_ascii_lowercase(),
            from: e.u32("nAnimStartFrame").unwrap_or(0),
            to: e.u32("nAnimStopFrame").unwrap_or(0),
            blend: e.f32("fBlendTime").unwrap_or(0.0),
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
        "CPlayerCameraClip" => ClipKind::PlayerCamera,
        "CFOVClip" => {
            let keys = bezier_keys(e, "FOVOverTime");
            if keys.is_empty() {
                return None;
            }
            let n = ((duration / FOV_STEP).ceil() as usize).clamp(1, 20_000);
            ClipKind::Fov { samples: (0..=n).filter_map(|i| sample_keys(&keys, i as f32 * FOV_STEP)).collect() }
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
    let duration = groups.iter().flat_map(|g| &g.clips).map(|c| c.start + c.duration).fold(0.0f32, f32::max);
    Ok(Sequence {
        name: seq.attr("sName").unwrap_or_default().to_string(),
        duration,
        looping: seq.bool("bLoop").unwrap_or(false),
        pause_at_end: seq.bool("bPauseAtEnd").unwrap_or(false),
        cutscene: seq.bool("bIsCutscene").unwrap_or(false),
        groups,
        sync_points,
    })
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
                    ClipKind::Animation { source, from, to, blend } => format!("anim\t{from}\t{to}\t{blend}\t{source}"),
                    ClipKind::SimpleAnimation { source, from, to } => format!("simple\t{from}\t{to}\t{source}"),
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
                    ClipKind::PlayerCamera => "pcam".to_string(),
                    ClipKind::Fov { samples } => format!("fov\t{}", samples.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(";")),
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
                        "anim" => ClipKind::Animation { from: gu(1), to: gu(2), blend: gn(3), source: g(4).to_string() },
                        "simple" => ClipKind::SimpleAnimation { from: gu(1), to: gu(2), source: g(3).to_string() },
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
                        "pcam" => ClipKind::PlayerCamera,
                        "fov" => ClipKind::Fov { samples: g(1).split(';').filter_map(|x| x.parse().ok()).collect() },
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
}
