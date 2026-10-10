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

impl VineSettings {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let root = crate::fcb::parse(bytes)?;
        let mut entry = None;
        root.walk(&mut |o, _| {
            if o.str("hidName").as_deref() == Some("CVineClimbingGameplay") {
                entry = Some(o.clone());
            }
        });
        Self::from_object(&entry.context("GSF library has no CVineClimbingGameplay")?)
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

#[cfg(test)]
mod tests {
    use super::*;

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
