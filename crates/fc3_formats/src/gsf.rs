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

#[cfg(test)]
mod tests {
    use super::*;

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
