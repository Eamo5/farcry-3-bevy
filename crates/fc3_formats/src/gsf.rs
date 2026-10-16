//! Authored specific-gameplay settings. Domino supplies instance bindings;
//! timing, moves and effects live in the separate GSF library.

use anyhow::{Context, Result, ensure};
use crate::fcb::Object;

pub const LIBRARY: &str = r"generated\databases\generic\gsf.lib";
pub const CACHE_REL: &str = "story/gsf.lib";

#[derive(Debug, Clone, PartialEq)]
pub struct VineSettings {
    pub stand_up_move: String,
    pub camera_shake_move: String,
    pub teleport_particle: String,
    pub teleport_postfx: String,
    pub dolly_postfx: String,
    pub camera_speed: f32,
    pub horizontal_limits: [f32; 2],
    pub vertical_limits: [f32; 2],
    pub particle_after: f32,
    pub particle_before: f32,
    pub particle_offset_z: f32,
    pub postfx_before: f32,
    pub shake_before: f32,
    pub camera_blend_time: f32,
    pub upper_look: f32,
    pub look_up_time: f32,
    pub climb_max_time: f32,
    pub dolly_duration: f32,
}

/// The library's entry for the gameplay `name` (`hidName`).
pub fn entry(bytes: &[u8], name: &str) -> Result<Object> {
    let root = crate::fcb::parse(bytes)?;
    let mut entry = None;
    root.walk(&mut |o, _| {
        if entry.is_none() && o.str("hidName").as_deref() == Some(name) {
            entry = Some(o.clone());
        }
    });
    entry.with_context(|| format!("GSF library has no {name}"))
}

/// Being intoxicated (`CIntoxicatedMiniGame`): how the view sways (its
/// horizontal and vertical reach and speed, as multipliers), how fast the
/// drunken walk wanders (`MovementspeedFactor`), whether it wanders at all,
/// and how long watching the hands lasts.
#[derive(Debug, Clone, PartialEq)]
pub struct IntoxicationSettings {
    pub speed: f32,
    /// Horizontal size, vertical size, horizontal speed, vertical speed.
    pub sway: [f32; 4],
    pub drunken_control: bool,
    pub watch_hands: f32,
    /// The moves its generic events play (`WatchCough`, `WatchBubbleHands`).
    pub moves: std::collections::BTreeMap<String, String>,
}

const SWAY_FIELDS: [&str; 4] = ["SwayHSizeMultiplier", "SwayVSizeMultiplier", "SwayHSpeedMultiplier", "SwayVSpeedMultiplier"];

impl IntoxicationSettings {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let o = entry(bytes, "CIntoxicatedMiniGame")?;
        let n = |name: &str| o.f32(name).filter(|v| v.is_finite()).with_context(|| format!("intoxication setting {name} is missing"));
        Ok(Self {
            speed: n("MovementspeedFactor")?,
            sway: [n(SWAY_FIELDS[0])?, n(SWAY_FIELDS[1])?, n(SWAY_FIELDS[2])?, n(SWAY_FIELDS[3])?],
            drunken_control: !o.bool("DisableDrunkenControl").unwrap_or(false),
            watch_hands: n("WatchHandsDuration").unwrap_or(10.0),
            // (The move names are the hashed fields' companions.)
            moves: [("WatchCough", 0x0F83_34F1), ("WatchBubbleHands", 0x73E5_9597)]
                .into_iter()
                .filter_map(|(event, h)| o.str_h(h).filter(|m| !m.is_empty()).map(|m| (event.to_string(), m)))
                .collect(),
        })
    }

    /// With a script's own values over the library's (`MovementspeedFactor`,
    /// `SwayHSizeMultiplier`...).
    pub fn with(mut self, value: impl Fn(&str) -> Option<f32>) -> Self {
        if let Some(v) = value("MovementspeedFactor") {
            self.speed = v;
        }
        for (i, name) in SWAY_FIELDS.iter().enumerate() {
            if let Some(v) = value(name) {
                self.sway[i] = v;
            }
        }
        if let Some(v) = value("DisableDrunkenControl") {
            self.drunken_control = v == 0.0;
        }
        self
    }
}

/// The library archetypes the gameplays spawn by name (the Ink Monster's
/// masks, rocks, goodies and minions: `Library:Path` text fields), lower
/// case.
pub fn archetype_names(bytes: &[u8]) -> Result<std::collections::HashSet<String>> {
    let root = crate::fcb::parse(bytes)?;
    let mut texts = String::new();
    root.walk(&mut |o, _| {
        for (_, value) in &o.fields {
            let Some((0, text)) = value.split_last() else { continue };
            if !text.is_empty() && text.iter().all(|b| b.is_ascii_graphic()) {
                texts.push('"');
                texts.push_str(&String::from_utf8_lossy(text));
                texts.push('"');
            }
        }
    });
    Ok(crate::story::archetype_names_in(std::iter::once(texts)))
}

impl VineSettings {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::from_object(&entry(bytes, "CVineClimbingGameplay")?)
    }

    fn from_object(o: &Object) -> Result<Self> {
        let number = |name: &str| -> Result<f32> {
            let value = o.f32(name).with_context(|| format!("vine setting {name} is missing"))?;
            ensure!(value.is_finite(), "vine setting {name} is not finite");
            Ok(value)
        };
        let duration = |name: &str| -> Result<f32> {
            let value = number(name)?;
            ensure!(value >= 0.0, "vine duration {name} is negative");
            Ok(value)
        };
        // String companions to the library's hashed move/effect references.
        let text = |hash: u32| o.str_h(hash).filter(|s| !s.is_empty())
            .with_context(|| format!("vine reference {hash:08x} is missing"));
        let settings = Self {
            stand_up_move: text(0x9d6fa22f)?,
            camera_shake_move: text(0xb5368410)?,
            teleport_particle: text(0xfbbdf427)?,
            teleport_postfx: text(0x7ff8aedd)?,
            dolly_postfx: text(0x7ea6ff5d)?,
            camera_speed: duration("CameraMovementSpeed")?,
            horizontal_limits: [number("CameraHMinimumAngle")?, number("CameraHMaximumAngle")?],
            vertical_limits: [number("CameraVMinimumAngle")?, number("CameraVMaximumAngle")?],
            particle_after: duration("Teleport_Particle_Duration_After_Teleport")?,
            particle_before: duration("Teleport_Particle_Duration_Before_Teleport")?,
            particle_offset_z: number("Teleport_Particle_Offset_Z")?,
            // This is an offset, not a duration: the installed value is -1.
            postfx_before: number("Teleport_PostFX_Time_Offset_Before_Teleport")?,
            shake_before: duration("Camera_Shake_Offset_Before_Teleport")?,
            camera_blend_time: duration("Camera_Offset_Blend_Time")?,
            upper_look: number("Max_Upper_Look_Value")?,
            look_up_time: duration("Camera_Look_Up_Time")?,
            climb_max_time: duration("Climb_Max_Time")?,
            dolly_duration: duration("Dolly_Zoom_Duration")?,
        };
        ensure!(settings.horizontal_limits[0] <= settings.horizontal_limits[1]
            && settings.vertical_limits[0] <= settings.vertical_limits[1], "inverted vine camera limits");
        Ok(settings)
    }
}

/// A sniping challenge (`CSnipingChallenge`, WIL_050's): the player held
/// crouched at a rifle, the view kept within angles of where they are to
/// look. Angles in degrees, positive yaw turning left (counter-clockwise
/// seen from above), positive pitch up.
#[derive(Debug, Clone, PartialEq)]
pub struct SnipingSettings {
    /// The rifle lent (`weapons:FC3/M93.M93_SniperChallenge`).
    pub weapon_archetype: String,
    pub yaw: [f32; 2],
    pub pitch: [f32; 2],
    pub crouch: bool,
    /// The music state it plays.
    pub music: String,
}

impl SnipingSettings {
    pub fn parse(bytes: &[u8], name: &str) -> Result<Self> {
        Self::from_object(&entry(bytes, name)?)
    }

    fn from_object(o: &Object) -> Result<Self> {
        let number = |name: &str| o.f32(name).filter(|v| v.is_finite()).with_context(|| format!("sniping setting {name} is missing"));
        let settings = Self {
            weapon_archetype: o.str("WeaponArchetype").filter(|s| !s.is_empty()).context("sniping challenge has no weapon")?,
            yaw: [number("gCameraYawMinDeviation")?, number("gCameraYawMaxDeviation")?],
            pitch: [number("gCameraPitchMinDeviation")?, number("gCameraPitchMaxDeviation")?],
            crouch: o.bool("IsStanceCrouch").unwrap_or(false),
            music: o.str_h(0xA504_B211).unwrap_or_default(),
        };
        ensure!(settings.yaw[0] <= settings.yaw[1] && settings.pitch[0] <= settings.pitch[1], "inverted sniping view limits");
        Ok(settings)
    }

    /// A script's own value for one of the settings (its GSF box's
    /// `gCameraYawMinDeviation`, `IsStanceCrouch`...), over the library's.
    pub fn set(&mut self, key: &str, value: &str) {
        let number = value.parse::<f32>().ok().filter(|v| v.is_finite());
        match (key, number) {
            ("gCameraYawMinDeviation", Some(v)) => self.yaw[0] = v,
            ("gCameraYawMaxDeviation", Some(v)) => self.yaw[1] = v,
            ("gCameraPitchMinDeviation", Some(v)) => self.pitch[0] = v,
            ("gCameraPitchMaxDeviation", Some(v)) => self.pitch[1] = v,
            ("IsStanceCrouch", _) => self.crouch = value.eq_ignore_ascii_case("true") || value == "1",
            ("WeaponArchetype", _) if !value.is_empty() => self.weapon_archetype = value.to_string(),
            _ => {}
        }
    }

    /// The lent rifle's model folder (`m93`), which names our weapon.
    pub fn weapon_folder(&self) -> Option<String> {
        let name = self.weapon_archetype.rsplit('/').next()?;
        let folder = name.split('.').next()?.to_ascii_lowercase();
        (!folder.is_empty()).then_some(folder)
    }
}

/// CIT_030's fight with the Ink Monster (`CCitraBossFight`): the giant's
/// health and the health it goes on to each phase at, its attacks' reach and
/// harm, its moves (each a move of the move table, played facing the player:
/// `+000fw`, `+045rt`, `-045lt`), the minions it calls and the goodies laid
/// out meanwhile, and what it says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BossFightSettings {
    pub hp: f32,
    /// The health at which phases 2, 3 and 4 (knocked out) begin.
    pub phases: [f32; 3],
    pub attack_cooldown: f32,
    pub idle_between_attacks: f32,
    pub sweep_range: f32,
    pub sweep_damage: f32,
    pub sweep_radius: f32,
    pub death_hand_radius: f32,
    pub rock_damage: f32,
    pub rock_health: f32,
    pub rock_radius: f32,
    pub rock_archetype: String,
    pub breath_damage: f32,
    pub breath_radius: f32,
    pub pound_radius: f32,
    pub pound_damage: f32,
    pub minions: f32,
    pub minions_harder: f32,
    pub minion_timer: f32,
    pub minion_radius: f32,
    pub minion_archetype: String,
    /// A hit doing at least this plays the hurt move.
    pub hurt_threshold: f32,
    pub goodies: Vec<String>,
    pub goodies_range: f32,
    pub goodies_cooldown: f32,
    pub mask_archetype: String,
    pub broken_mask_archetype: String,
    /// The moves, by what they are for (`idle`, `sweep_left`, `throw1`...).
    pub moves: std::collections::BTreeMap<String, String>,
    /// The sounds (`m_sound...` without the prefix), sound event ids.
    pub sounds: std::collections::BTreeMap<String, u32>,
}

impl BossFightSettings {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::from_object(&entry(bytes, "CCitraBossFight")?)
    }

    fn from_object(o: &Object) -> Result<Self> {
        let n = |name: &str| o.f32(name).filter(|v| v.is_finite()).unwrap_or(0.0);
        let text = |name: &str| o.str(name).unwrap_or_default();
        // (The move names are the hashed fields' companions.)
        const MOVES: [(&str, u32); 23] = [
            ("hidden", 0x96BF_112F),
            ("idle", 0xEE02_6F11),
            ("sweep_left", 0xE2FD_1ACB),
            ("sweep_right", 0x5CEC_73A8),
            ("breath", 0x89D3_D4A3),
            ("breath_start", 0xA587_1968),
            ("breath_cycle", 0x8A78_9D74),
            ("breath_end", 0x1901_1594),
            ("throw1", 0x5EC3_9E3E),
            ("throw2", 0x36E6_25AC),
            ("throw3", 0xC7C7_A45B),
            ("throw1_exit", 0x388E_49CF),
            ("throw3_exit", 0x1136_EF65),
            ("throw2_exit", 0x7C38_E5B2),
            ("summon_start", 0x7E36_363F),
            ("summon_cycle", 0x51C9_B223),
            ("summon_end", 0xFF2E_181A),
            ("arrival", 0xF536_5C32),
            ("hurt_idle", 0x9C8B_61DE),
            ("hurt", 0xF5AB_DE1E),
            ("hurt_right", 0xD0B6_1A20),
            ("knockout", 0xE018_37ED),
            ("knockout_idle", 0x5B36_0AFA),
        ];
        let moves = MOVES.iter().filter_map(|(k, h)| o.str_h(*h).filter(|s| !s.is_empty()).map(|s| (k.to_string(), s))).collect();
        let mut sounds = std::collections::BTreeMap::new();
        let mut goodies = Vec::new();
        for name in [
            "RockImpact",
            "YouAreSpecial",
            "YouAreCenterOfThisWorld",
            "YouAreMakingDifference",
            "EveryoneLooksUpToYou",
            "YouWillDefeatAnyone",
            "YourSenseHasBeenCloudedByOthers",
            "ICanFeelTheFireInYou",
            "YouAreStrong",
            "Fight",
            "YouAreFearless",
            "CitraRakyat1",
            "CitraRakyat2",
            "CitraRakyat3",
            "IAmJasonBrody",
            "HolyShit",
            "BossGruntArrival",
            "BossGruntAttack",
            "BossGruntRockThrow",
            "BossGruntPainBig",
            "BossGruntPainSmall",
            "BossGruntInhale",
            "BossGruntSummonIn",
            "BossGruntSummonOut",
        ] {
            let id =
                o.str(&format!("m_sound{name}")).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).filter(|&i| i != u32::MAX && i != 0);
            if let Some(id) = id {
                sounds.insert(name.to_string(), id);
            }
        }
        for name in ["Ammo", "RocketAmmo", "RocketLauncher", "BowAmmo", "MachineGun", "Bow", "HealthPack"] {
            let a = text(&format!("m_goodies{name}Archetype"));
            if !a.is_empty() {
                goodies.push(a);
            }
        }
        let s = Self {
            hp: n("m_bossHP"),
            phases: [n("m_bossHpPhase2"), n("m_bossHpPhase3"), n("m_bossHpPhase4")],
            attack_cooldown: n("m_bossAttackCooldown"),
            idle_between_attacks: n("m_idleTimeBetweenAttacks"),
            sweep_range: n("m_sweepRange"),
            sweep_damage: n("m_sweepDmg"),
            sweep_radius: n("m_sweepHitRadius"),
            death_hand_radius: n("m_deathHandHitRadius"),
            rock_damage: n("m_rockThrowDmg"),
            rock_health: n("m_rockHealth"),
            rock_radius: n("m_rockHitRadius"),
            rock_archetype: text("m_rockArchetype"),
            breath_damage: n("m_firebreathDmg"),
            breath_radius: n("m_firebreathRadius"),
            pound_radius: n("m_summonPoundRadius"),
            pound_damage: n("m_summonPoundDmg"),
            minions: n("m_summonMinionsCount"),
            minions_harder: n("m_summonMinionsCountHarder"),
            minion_timer: n("m_summonMinionTimer"),
            minion_radius: n("m_summonMinionsRadius"),
            minion_archetype: text("m_summonMinionsArchetype"),
            hurt_threshold: n("m_hurtAnimationThreshold"),
            goodies,
            goodies_range: n("m_goodiesSpawnRange"),
            goodies_cooldown: n("m_goodiesSpawnCooldown"),
            mask_archetype: text("m_bossMaskArchetype"),
            broken_mask_archetype: text("m_bossBrokenMaskArchetype"),
            moves,
            sounds,
        };
        ensure!(s.hp > 0.0 && s.phases.iter().all(|p| *p < s.hp), "boss fight health out of order");
        Ok(s)
    }

    /// The move for a part of the fight (`idle`, `sweep_left`...).
    pub fn move_name(&self, what: &str) -> Option<&str> {
        self.moves.get(what).map(String::as_str)
    }

    /// Which phase (0..=3) the giant is in at this health.
    pub fn phase_at(&self, hp: f32) -> usize {
        self.phases.iter().filter(|&&p| hp <= p).count()
    }
}

/// A shooting gallery (the sharpshooter challenges, `CShootingGallery...`):
/// the player stands with the gallery's weapon, the view held within angles
/// of where they face, while birds released in waves fly off within a frustum
/// in front; the kill target reached, the wager comes back multiplied.
/// Angles in degrees as the sniping challenge has them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShootingGallerySettings {
    /// The weapon and the bird, by their place in the shared resources
    /// (`ShootingGalleryResources`).
    pub weapon: usize,
    pub animal: usize,
    pub yaw: [f32; 2],
    pub pitch: [f32; 2],
    /// Where birds may fly: angles left, right, up and down of the view's
    /// middle, and how far.
    pub frustum: [f32; 4],
    pub frustum_distance: f32,
    /// Seconds between birds released (least, most), and whether a wave's
    /// birds go together.
    pub spawn_delay: [f32; 2],
    pub flock: bool,
    /// The longest an automatic weapon goes without its magazine filled (s).
    pub refill: f32,
    /// The wager comes back this many times over.
    pub winnings: f32,
    /// The difficulty box's subtitle (a string id).
    pub subtitle: u32,
    /// The music state it plays.
    pub music: String,
    /// Easy, medium and hard.
    pub difficulties: Vec<GalleryDifficulty>,
    pub waves: Vec<GalleryWave>,
}

/// How a difficulty's birds fly, and what it asks and stakes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GalleryDifficulty {
    pub speed: f32,
    /// How many times a bird turns, by how much each way (least, most), how
    /// often, over how long, and how far from its box it first turns.
    pub turns: u32,
    pub horizontal_turn: [f32; 2],
    pub vertical_turn: [f32; 2],
    pub turn_every: f32,
    pub turn_time: f32,
    pub turn_from: f32,
    pub kill_target: u32,
    pub wager: u32,
}

/// A wave: how many birds, and for each difficulty how long before they come
/// and how long the wave lasts, and whether the next wave is announced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GalleryWave {
    pub birds: u32,
    pub delay: Vec<f32>,
    pub duration: Vec<f32>,
    pub announced: Vec<bool>,
}

impl ShootingGallerySettings {
    pub fn parse(bytes: &[u8], name: &str) -> Result<Self> {
        Self::from_object(&entry(bytes, name)?)
    }

    fn from_object(o: &Object) -> Result<Self> {
        let n = |o: &Object, name: &str| o.f32(name).filter(|v| v.is_finite()).unwrap_or(0.0);
        let difficulties: Vec<GalleryDifficulty> = o
            .child("DifficultySettings")
            .map(|d| d.children.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|d| GalleryDifficulty {
                speed: n(d, "BirdSpeed"),
                turns: d.u32("BirdNumberOfDirectionChanges").unwrap_or(0),
                horizontal_turn: [n(d, "BirdHorizMinTurningAngle"), n(d, "BirdHorizMaxTurningAngle")],
                vertical_turn: [n(d, "BirdVertMinTurningAngle"), n(d, "BirdVertMaxTurningAngle")],
                turn_every: n(d, "BirdTimeBetweenDirectionChanges"),
                turn_time: n(d, "BirdTurningTime"),
                turn_from: n(d, "BirdStartTurningDistance"),
                kill_target: d.u32("KillTarget").unwrap_or(0),
                wager: d.u32("WagerForChallenge").unwrap_or(0),
            })
            .collect();
        let waves: Vec<GalleryWave> = o
            .child("BirdWaveInformation")
            .map(|w| w.children.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|w| {
                let per: Vec<&Object> = w.child("WaveDifficultySettings").map(|d| d.children.iter().map(|c| c.as_ref()).collect()).unwrap_or_default();
                GalleryWave {
                    birds: w.u32("BirdsToSpawn").unwrap_or(0),
                    delay: per.iter().map(|d| n(d, "DelayBeforeSpawningBegins")).collect(),
                    duration: per.iter().map(|d| n(d, "WaveDuration")).collect(),
                    announced: per.iter().map(|d| d.bool("NextWaveMessageEnabled").unwrap_or(false)).collect(),
                }
            })
            .collect();
        let s = Self {
            weapon: o.u32("WeaponSettingsIndex").unwrap_or(0) as usize,
            animal: o.u32("AnimalSettingsIndex").unwrap_or(0) as usize,
            yaw: [n(o, "gCameraYawMinDeviation"), n(o, "gCameraYawMaxDeviation")],
            pitch: [n(o, "gCameraPitchMinDeviation"), n(o, "gCameraPitchMaxDeviation")],
            frustum: [
                n(o, "gCameraViewFrustumLeftAngle"),
                n(o, "gCameraViewFrustumRightAngle"),
                n(o, "gCameraViewFrustumUpAngle"),
                n(o, "gCameraViewFrustumDownAngle"),
            ],
            frustum_distance: n(o, "gCameraViewFrustumDistance"),
            spawn_delay: [n(o, "gDelayBetweenBirdSpawnsMin"), n(o, "gDelayBetweenBirdSpawnsMax")],
            flock: o.bool("gBirdSpawnAsFlock").unwrap_or(false),
            refill: n(o, "gMaxTimeBetweenAutomaticWeaponRefills"),
            winnings: n(o, "gWinningsMultiplierForSuccess"),
            subtitle: o.u32("DifficultyScreenSubTitle").unwrap_or(0),
            music: o.str_h(0xA504_B211).unwrap_or_default(),
            difficulties,
            waves,
        };
        ensure!(!s.difficulties.is_empty() && !s.waves.is_empty(), "shooting gallery without difficulties or waves");
        ensure!(s.yaw[0] <= s.yaw[1] && s.pitch[0] <= s.pitch[1], "inverted shooting gallery view limits");
        Ok(s)
    }

    /// A wave's start delay and length at a difficulty (0 easy, 1 medium,
    /// 2 hard).
    pub fn wave_times(&self, wave: usize, difficulty: usize) -> Option<(f32, f32)> {
        let w = self.waves.get(wave)?;
        Some((*w.delay.get(difficulty).or(w.delay.last())?, *w.duration.get(difficulty).or(w.duration.last())?))
    }
}

/// The galleries' shared resources (`CShootingGalleryResourceContainer`): the
/// weapons and birds they pick by index, and the countdown's sounds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShootingGalleryResources {
    /// A weapon archetype (`weapons:FC3/KW1911`), and whether it's mounted.
    pub weapons: Vec<(String, bool)>,
    pub animals: Vec<GalleryAnimal>,
    pub countdown_sound: u32,
    pub ready_sound: u32,
}

/// A gallery's bird: its archetype, its flight, death and impact clips, and
/// its sounds (released, killed, hitting the ground).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GalleryAnimal {
    pub archetype: String,
    pub fly: String,
    pub death: String,
    pub impact: String,
    pub release_sound: u32,
    pub death_sound: u32,
    pub impact_sound: u32,
}

/// A sound event id written as hex text (`0x001A4F1F`); none for all ones.
fn sound_id(o: &Object, name: &str) -> u32 {
    o.str(name).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).filter(|&i| i != u32::MAX).unwrap_or(0)
}

pub const SHOOTING_GALLERY_RESOURCES: &str = "CShootingGalleryResourceContainer";

impl ShootingGalleryResources {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let o = entry(bytes, SHOOTING_GALLERY_RESOURCES)?;
        let weapons = o
            .child("WeaponResources")
            .map(|w| w.children.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|w| Some((w.str("WeaponArchetype").filter(|s| !s.is_empty())?, w.u32("WeaponIsMounted").unwrap_or(0) != 0)))
            .collect();
        let animals = o
            .child("AnimalResources")
            .map(|a| a.children.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|a| {
                let audio = a.child("AnimalAudioSettings");
                let sound = |name: &str| audio.map_or(0, |s| sound_id(s, name));
                // (The clips are the hashed fields' companions.)
                GalleryAnimal {
                    archetype: a.str("AnimalArchetype").unwrap_or_default(),
                    fly: a.str_h(0xF073_6FEE).unwrap_or_default(),
                    death: a.str_h(0xEF7C_A492).unwrap_or_default(),
                    impact: a.str_h(0x3A85_10EF).unwrap_or_default(),
                    release_sound: sound("AnimalReleaseSound"),
                    death_sound: sound("AnimalDeathSound"),
                    impact_sound: sound("AnimalImpactSound"),
                }
            })
            .collect();
        let audio = o.child("AudioSettings");
        let s = Self {
            weapons,
            animals,
            countdown_sound: audio.map_or(0, |a| sound_id(a, "CountDownEachSecond")),
            ready_sound: audio.map_or(0, |a| sound_id(a, "GetReadySound")),
        };
        ensure!(!s.weapons.is_empty() && !s.animals.is_empty(), "shooting gallery resources without weapons or birds");
        Ok(s)
    }
}

/// A poker table (`CPokerMiniGame_...`, SQC_100's `..._Pokerminigame`):
/// what kind of game it is, what the player may do, who plays at it, and
/// what each difficulty stakes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PokerSettings {
    /// `PokerGameType`: 0 a bar's game, else a story's (1 JAS_140, 2 JAS_170,
    /// 3 Hoyt's, 4 SQC_100).
    pub kind: u32,
    /// Whether the player may skip ahead (through a hand they've folded)
    /// and leave the table.
    pub skip_ahead: bool,
    pub leave: bool,
    /// The difficulty box's subtitle (a string id).
    pub subtitle: u32,
    pub npcs: Vec<PokerNpc>,
    /// Practice, novice, skilled, expert.
    pub difficulties: Vec<PokerDifficulty>,
    /// The story's scenes played between hands (`ScriptedSequenceIDList`).
    pub scenes: usize,
}

/// A computer player: its character, how it plays (`PlayerType`: 0
/// unskilled, 1 skilled, 2 expert, 3 a loose cannon, 4 any, 5 as the
/// difficulty has it), the seat it wants (0..4, 4 any) and its voice
/// (`PokerDialogSet`: 0..4 a standard one, 4 Sam's, 5 Hoyt's).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PokerNpc {
    pub entity: u64,
    pub player_type: u32,
    pub seat: u32,
    pub dialog: u32,
}

/// A difficulty's row: offered or not, its buy-in, and its chips.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PokerDifficulty {
    pub enabled: bool,
    pub buy_in: u32,
    pub chip: String,
}

impl PokerSettings {
    pub fn parse(bytes: &[u8], name: &str) -> Result<Self> {
        Self::from_object(&entry(bytes, name)?)
    }

    fn from_object(o: &Object) -> Result<Self> {
        let list = |name: &str| o.child(name).map(|c| c.children.clone()).unwrap_or_default();
        let npcs = list("NPCList")
            .iter()
            .filter_map(|n| {
                let entity = n.u64("EntityID").filter(|e| *e != u64::MAX && *e != 0)?;
                Some(PokerNpc {
                    entity,
                    player_type: n.u32("PlayerType").unwrap_or(5),
                    seat: n.u32("DesiredSeatId").unwrap_or(4),
                    dialog: n.u32("PokerDialogSet").unwrap_or(0),
                })
            })
            .collect();
        let difficulties = list("DifficultySettings")
            .iter()
            .map(|d| PokerDifficulty {
                enabled: d.bool("PokerDifficultyIsEnabled").unwrap_or(true),
                buy_in: d.u32("PokerGameBuyIn").unwrap_or(0),
                chip: d.str("PokerChipArchetype").unwrap_or_default(),
            })
            .collect();
        Ok(PokerSettings {
            kind: o.u32("PokerGameType").unwrap_or(0),
            skip_ahead: o.bool("SkipAheadEnabled").unwrap_or(false),
            leave: o.bool("LeaveGameEnabled").unwrap_or(false),
            subtitle: o.u32("DifficultyScreenSubTitle").unwrap_or(0),
            npcs,
            difficulties,
            scenes: o.child("ScriptedSequenceIDList").map_or(0, |c| c.children.len().max(c.fields.len())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripts_intoxication_values_go_over_the_librarys() {
        let s = IntoxicationSettings { speed: 2.5, sway: [0.5, 0.3, 0.2, 0.55], drunken_control: true, watch_hands: 10.0, moves: Default::default() };
        let own = s.clone().with(|k| match k {
            "MovementspeedFactor" => Some(1.9),
            "SwayHSizeMultiplier" => Some(0.1),
            _ => None,
        });
        assert_eq!((own.speed, own.sway), (1.9, [0.1, 0.3, 0.2, 0.55]));
        assert!(own.drunken_control);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_intoxication_settings() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let s = IntoxicationSettings::parse(&vfs.read(LIBRARY).unwrap()).unwrap();
        assert_eq!((s.speed, s.sway, s.drunken_control, s.watch_hands), (2.5, [0.5, 0.3, 0.2, 0.55], true, 10.0));
        assert_eq!(s.moves.get("WatchCough").map(String::as_str), Some("mushseq_cough"));
        assert_eq!(s.moves.get("WatchBubbleHands").map(String::as_str), Some("player_bubblehands"));
    }

    #[test]
    fn the_ink_monster_goes_through_its_phases_by_health() {
        let s = BossFightSettings { hp: 20000.0, phases: [17000.0, 12000.0, 1000.0], ..Default::default() };
        assert_eq!([20000.0, 17000.0, 16999.0, 12000.0, 900.0].map(|h| s.phase_at(h)), [0, 1, 1, 2, 3]);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_shooting_gallery_settings() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let bytes = vfs.read(LIBRARY).unwrap();
        let s = ShootingGallerySettings::parse(&bytes, "CShootingGalleryPistol_E6").unwrap();
        assert_eq!((s.yaw, s.pitch, s.frustum, s.frustum_distance), ([-85.0, 85.0], [-20.0, 45.0], [85.0, 85.0, 45.0, 20.0], 30.0));
        assert_eq!((s.spawn_delay, s.winnings), ([0.75, 1.75], 2.0));
        assert_eq!(s.difficulties.iter().map(|d| (d.kill_target, d.wager)).collect::<Vec<_>>(), vec![(10, 100), (10, 200), (10, 250)]);
        assert_eq!((s.difficulties[0].speed, s.difficulties[2].turns), (3.0, 7));
        assert_eq!(s.waves.len(), 9);
        assert_eq!((s.waves[0].birds, s.wave_times(0, 0), s.wave_times(0, 2)), (1, Some((1.5, 14.0)), Some((0.5, 4.0))));
        let r = ShootingGalleryResources::parse(&bytes).unwrap();
        assert_eq!(r.weapons[0], ("weapons:FC3/KW1911".to_string(), false));
        assert_eq!(r.animals.len(), 3);
        assert!(r.animals[1].fly.ends_with("seagull_fulb_flycycle_+000fw_nowep.mab"), "{}", r.animals[1].fly);
        assert!(r.animals[0].death.contains("shootinggallery") && r.animals[0].release_sound != 0);
        assert!(r.countdown_sound != 0 && r.ready_sound != 0);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_poker_settings() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let bytes = vfs.read(LIBRARY).unwrap();
        let bar = PokerSettings::parse(&bytes, "CPokerMiniGame_C3").unwrap();
        assert_eq!((bar.kind, bar.skip_ahead, bar.leave, bar.subtitle), (0, true, true, 172112));
        assert_eq!(
            bar.npcs.iter().map(|n| (n.entity, n.seat)).collect::<Vec<_>>(),
            vec![(2077746065446029667, 2), (2077746064433105250, 0), (2077746063271283041, 1)]
        );
        assert_eq!(bar.difficulties.iter().map(|d| d.buy_in).collect::<Vec<_>>(), vec![0, 100, 150, 250]);
        assert!(bar.difficulties[0].chip.contains("BottleCap"));
        let hoyt = PokerSettings::parse(&bytes, "CPokerMiniGame_JAS_220_PokerGym_G3").unwrap();
        assert_eq!((hoyt.kind, hoyt.leave, hoyt.npcs.len()), (3, false, 2));
        assert_eq!((hoyt.npcs[0].player_type, hoyt.npcs[0].dialog, hoyt.npcs[1].player_type), (1, 5, 2));
        let sqc = PokerSettings::parse(&bytes, "SQC_100_OBJ20_Pokerminigame").unwrap();
        assert_eq!(sqc.difficulties.iter().map(|d| d.enabled).collect::<Vec<_>>(), vec![false, true, false, false]);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_boss_fight_settings() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let s = BossFightSettings::parse(&vfs.read(LIBRARY).unwrap()).unwrap();
        assert_eq!((s.hp, s.phases), (20000.0, [17000.0, 12000.0, 1000.0]));
        assert_eq!((s.sweep_damage, s.sweep_radius, s.rock_damage, s.rock_health), (35.0, 8.0, 40.0, 10.0));
        assert_eq!(s.move_name("sweep_left"), Some("ex_bossfight_sweepleft"));
        assert_eq!(s.move_name("knockout_idle"), Some("ex_bossfight_knockout_idle"));
        assert_eq!(s.goodies.len(), 7);
        assert!(s.sounds.contains_key("Fight") && !s.sounds.contains_key("HolyShit"));
        assert!(s.minion_archetype.contains("BossMinion"));
        // Its archetypes are among those the library names.
        let names = archetype_names(&vfs.read(LIBRARY).unwrap()).unwrap();
        for a in [&s.mask_archetype, &s.broken_mask_archetype, &s.rock_archetype, &s.goodies[0]] {
            assert!(names.contains(&a.to_ascii_lowercase()), "{a}");
        }
    }

    #[test]
    fn a_sniping_challenge_takes_the_scripts_limits_over_the_librarys() {
        let mut s = SnipingSettings {
            weapon_archetype: "weapons:FC3/M93.M93_SniperChallenge".into(),
            yaw: [-30.0, 30.0],
            pitch: [-13.0, 30.0],
            crouch: false,
            music: String::new(),
        };
        for (k, v) in [("gCameraYawMinDeviation", "-18"), ("gCameraYawMaxDeviation", "92"), ("gCameraPitchMaxDeviation", "7"), ("IsStanceCrouch", "true")] {
            s.set(k, v);
        }
        s.set("gCameraPitchMinDeviation", "nan");
        assert_eq!((s.yaw, s.pitch, s.crouch), ([-18.0, 92.0], [-13.0, 7.0], true));
        assert_eq!(s.weapon_folder().as_deref(), Some("m93"));
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_sniping_challenge_settings() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let s = SnipingSettings::parse(&vfs.read(LIBRARY).unwrap(), "CSnipingChallenge_CIT020").unwrap();
        assert_eq!(s.weapon_archetype, "weapons:FC3/M93.M93_SniperChallenge");
        assert_eq!((s.yaw, s.pitch, s.crouch), ([-30.0, 30.0], [-13.0, 30.0], true));
        assert_eq!(s.music, "MUS_MG_KnifeThrowing_LP");
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_vine_settings_preserve_authored_offsets_and_references() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let bytes = vfs.read(LIBRARY).unwrap();
        let settings = VineSettings::parse(&bytes).unwrap();
        assert_eq!(settings.climb_max_time, 6.0);
        assert_eq!(settings.dolly_duration, 2.5);
        assert_eq!(settings.postfx_before, -1.0);
        assert_eq!(settings.horizontal_limits, [-30.0, 30.0]);
        assert_eq!(settings.vertical_limits, [-60.0, 0.0]);
        assert_eq!(settings.stand_up_move, "vine_hallucination_teleport");
        assert!(settings.teleport_particle.ends_with("mushroom_puff_transition2"));
        let mut root = (*crate::fcb::parse(&bytes).unwrap()).clone();
        let entry = root.children.iter_mut().find(|o| o.str("hidName").as_deref() == Some("CVineClimbingGameplay")).unwrap();
        let entry = std::sync::Arc::make_mut(entry);
        let field = entry.fields.iter_mut().find(|(h, _)| *h == crate::hash::name_hash("Climb_Max_Time")).unwrap();
        field.1 = f32::NAN.to_le_bytes().to_vec();
        assert!(VineSettings::from_object(entry).is_err());
    }
}
