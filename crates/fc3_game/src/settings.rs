//! Player options, saved to `settings.txt` beside the cache and applied live:
//! look sensitivity, field of view, volume, view distance, shadows, window
//! mode, the original shaders and key bindings.

use std::collections::HashMap;
use std::path::PathBuf;

use bevy::audio::{GlobalVolume, Volume};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};

use crate::controls::{default_bindings, Action, Binding};
use crate::data::CacheInfo;

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Settings::default())
            .init_resource::<SettingsFile>()
            .add_systems(Update, (load_settings.run_if(resource_exists::<CacheInfo>), apply_settings, apply_effects, save_settings).chain());
    }
}

/// The game's difficulty levels: how hard enemies hit and how quickly they
/// spot the player.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Difficulty {
    Casual,
    Adventurer,
    Warrior,
    Infamous,
}

impl Difficulty {
    pub const ALL: [Difficulty; 4] = [Difficulty::Casual, Difficulty::Adventurer, Difficulty::Warrior, Difficulty::Infamous];

    pub fn name(self) -> &'static str {
        match self {
            Difficulty::Casual => "Casual",
            Difficulty::Adventurer => "Adventurer",
            Difficulty::Warrior => "Warrior",
            Difficulty::Infamous => "Infamous",
        }
    }

    /// Its place among the game's levels (casual 0 ... infamous 3).
    pub fn index(self) -> usize {
        Difficulty::ALL.iter().position(|d| *d == self).unwrap_or(1)
    }

    pub fn from_name(s: &str) -> Option<Difficulty> {
        Difficulty::ALL.into_iter().find(|d| d.name().eq_ignore_ascii_case(s))
    }

    /// Multiplier on damage the player takes, without the game's table
    /// (`combat::DamageTable`: its health bars' scale by difficulty).
    pub fn damage_taken(self) -> f32 {
        match self {
            Difficulty::Casual => 0.5,
            Difficulty::Adventurer => 1.0,
            Difficulty::Warrior => 1.6,
            Difficulty::Infamous => 3.0,
        }
    }

    /// Multiplier on how fast enemies notice the player.
    pub fn spotting(self) -> f32 {
        match self {
            Difficulty::Casual => 0.7,
            Difficulty::Adventurer => 1.0,
            Difficulty::Warrior | Difficulty::Infamous => 1.35,
        }
    }
}

/// How far the world streams in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Quality {
    Low,
    Medium,
    High,
    Ultra,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Low, Quality::Medium, Quality::High, Quality::Ultra];

    pub fn name(self) -> &'static str {
        match self {
            Quality::Low => "Low",
            Quality::Medium => "Medium",
            Quality::High => "High",
            Quality::Ultra => "Ultra",
        }
    }

    fn from_name(s: &str) -> Option<Quality> {
        Quality::ALL.into_iter().find(|q| q.name().eq_ignore_ascii_case(s))
    }

    fn index(self) -> usize {
        Quality::ALL.iter().position(|q| *q == self).unwrap_or(0)
    }
}

#[derive(Resource, Clone, PartialEq, Debug)]
pub struct Settings {
    /// Mouse look multiplier.
    pub sensitivity: f32,
    pub gamepad_sensitivity: f32,
    pub invert_y: bool,
    /// The gamepad's rumble.
    pub vibration: bool,
    /// The game's aim assist with a gamepad.
    pub aim_assist: bool,
    /// Horizontal-ish field of view in degrees (vertical FOV of the camera).
    pub fov: f32,
    pub master_volume: f32,
    /// The music's share of the volume.
    pub music_volume: f32,
    pub view_distance: Quality,
    pub shadows: Quality,
    /// Post-processing: anti-aliasing (Medium), bloom, sun shafts and ambient
    /// occlusion (High), finer anti-aliasing and occlusion (Ultra). The game
    /// has screen-space ambient occlusion on at every PC quality; here it
    /// costs about 2.5 ms a frame (its depth and normal prepass draws the
    /// scene again), so Medium leaves it off.
    pub effects: Quality,
    pub vsync: bool,
    pub fullscreen: bool,
    pub original_shaders: bool,
    pub show_fps: bool,
    /// Subtitles for spoken lines.
    pub subtitles: bool,
    /// A new game plays the story (else the island is free to roam).
    pub story: bool,
    pub difficulty: Difficulty,
    /// The calibration popup's brightness, contrast and gamma (its sliders,
    /// 0-100, 50 neutral).
    pub brightness: f32,
    pub contrast: f32,
    pub gamma: f32,
    pub bindings: HashMap<Action, Binding>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sensitivity: 1.0,
            gamepad_sensitivity: 1.0,
            invert_y: false,
            vibration: true,
            aim_assist: true,
            fov: 75.0,
            master_volume: 0.8,
            music_volume: 0.6,
            view_distance: Quality::High,
            shadows: Quality::High,
            effects: Quality::High,
            vsync: true,
            fullscreen: false,
            original_shaders: true,
            show_fps: true,
            subtitles: true,
            story: true,
            difficulty: Difficulty::Adventurer,
            brightness: 50.0,
            contrast: 50.0,
            gamma: 50.0,
            bindings: default_bindings(),
        }
    }
}

impl Settings {
    pub fn binding(&self, a: Action) -> Option<Binding> {
        self.bindings.get(&a).copied()
    }

    /// Binds `a`, swapping with whichever action had that input.
    pub fn rebind(&mut self, a: Action, b: Binding) {
        let old = self.bindings.get(&a).copied();
        if let Some(other) = self.bindings.iter().find(|(k, v)| **k != a && **v == b).map(|(k, _)| *k) {
            match old {
                Some(o) => self.bindings.insert(other, o),
                None => self.bindings.remove(&other),
            };
        }
        self.bindings.insert(a, b);
    }

    /// The calibration as the game's final passes take it
    /// (`ColorAdjustment`): an offset (brightness), a scale (contrast) and an
    /// exponent (one over gamma). The engine maps the sliders to brightness
    /// 0.9-1.1, contrast 0-2 and gamma 0.8-1.2, then the offset is a sixth
    /// of brightness - 1 and the scale 1 + (contrast - 1) / 2.
    pub fn color_adjustment(&self) -> [f32; 3] {
        let brightness = (0.9 + self.brightness * 0.002).clamp(0.9, 1.1);
        let contrast = self.contrast * 0.02;
        let gamma = (0.8 + self.gamma * 0.004).clamp(0.8, 1.2);
        [(brightness - 1.0) / 6.0, 1.0 + 0.5 * (contrast - 1.0).clamp(-1.0, 1.0), 1.0 / gamma]
    }

    /// Streaming radii scale for the view distance setting.
    pub fn distance_scale(&self) -> f32 {
        [0.65, 0.85, 1.0, 1.35][self.view_distance.index()]
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("fc3settings 1\n");
        out += &format!("sensitivity {}\ngamepad_sensitivity {}\ninvert_y {}\nfov {}\n", self.sensitivity, self.gamepad_sensitivity, self.invert_y, self.fov);
        out += &format!(
            "master_volume {}\nview_distance {}\nshadows {}\neffects {}\n",
            self.master_volume,
            self.view_distance.name(),
            self.shadows.name(),
            self.effects.name()
        );
        out += &format!("vsync {}\nfullscreen {}\noriginal_shaders {}\nshow_fps {}\n", self.vsync, self.fullscreen, self.original_shaders, self.show_fps);
        out += &format!("subtitles {}\nstory {}\ndifficulty {}\nmusic_volume {}\n", self.subtitles, self.story, self.difficulty.name(), self.music_volume);
        out += &format!("vibration {}\naim_assist {}\n", self.vibration, self.aim_assist);
        out += &format!("brightness {}\ncontrast {}\ngamma {}\n", self.brightness, self.contrast, self.gamma);
        for a in Action::ALL {
            if let Some(b) = self.binding(a) {
                out += &format!("bind {} {}\n", a.id(), b.id());
            }
        }
        out
    }

    pub fn from_text(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines().skip(1) {
            let mut it = line.split_whitespace();
            let (Some(key), Some(value)) = (it.next(), it.next()) else { continue };
            let f = |v: &str, d: f32| v.parse::<f32>().unwrap_or(d);
            let b = |v: &str, d: bool| v.parse::<bool>().unwrap_or(d);
            match key {
                "sensitivity" => s.sensitivity = f(value, 1.0).clamp(0.1, 5.0),
                "gamepad_sensitivity" => s.gamepad_sensitivity = f(value, 1.0).clamp(0.1, 5.0),
                "invert_y" => s.invert_y = b(value, false),
                "vibration" => s.vibration = b(value, true),
                "aim_assist" => s.aim_assist = b(value, true),
                "fov" => s.fov = f(value, 75.0).clamp(55.0, 110.0),
                "master_volume" => s.master_volume = f(value, 0.8).clamp(0.0, 1.0),
                "music_volume" => s.music_volume = f(value, 0.6).clamp(0.0, 1.0),
                "view_distance" => s.view_distance = Quality::from_name(value).unwrap_or(Quality::High),
                "shadows" => s.shadows = Quality::from_name(value).unwrap_or(Quality::High),
                "effects" => s.effects = Quality::from_name(value).unwrap_or(Quality::High),
                "vsync" => s.vsync = b(value, true),
                "fullscreen" => s.fullscreen = b(value, false),
                "original_shaders" => s.original_shaders = b(value, true),
                "show_fps" => s.show_fps = b(value, true),
                "subtitles" => s.subtitles = b(value, true),
                "story" => s.story = b(value, true),
                "difficulty" => s.difficulty = Difficulty::from_name(value).unwrap_or(Difficulty::Adventurer),
                "brightness" => s.brightness = f(value, 50.0).clamp(0.0, 100.0),
                "contrast" => s.contrast = f(value, 50.0).clamp(0.0, 100.0),
                "gamma" => s.gamma = f(value, 50.0).clamp(0.0, 100.0),
                "bind" => {
                    if let (Some(a), Some(binding)) = (Action::from_id(value), it.next().and_then(Binding::from_id)) {
                        s.rebind(a, binding);
                    }
                }
                _ => {}
            }
        }
        s
    }
}

#[derive(Resource, Default)]
struct SettingsFile {
    path: Option<PathBuf>,
    loaded: bool,
    /// What's on disk, to save only real changes.
    saved: Option<Settings>,
}

fn load_settings(info: Res<CacheInfo>, args: Res<crate::Args>, mut file: ResMut<SettingsFile>, mut settings: ResMut<Settings>) {
    if file.loaded {
        return;
    }
    file.loaded = true;
    let path = info.root.parent().unwrap_or(&info.root).join("settings.txt");
    if let Ok(text) = std::fs::read_to_string(&path) {
        *settings = Settings::from_text(&text);
    }
    // Command-line switches win for this session (and aren't saved).
    if !args.original_shaders {
        settings.original_shaders = false;
    }
    if args.novsync {
        settings.vsync = false;
    }
    // FC3_DIFFICULTY=Casual|Adventurer|Warrior|Infamous (tests).
    if let Some(d) = std::env::var("FC3_DIFFICULTY").ok().and_then(|v| Difficulty::from_name(&v)) {
        settings.difficulty = d;
    }
    file.saved = Some(settings.clone());
    // Automated runs never write the user's settings.
    file.path = (!args.automated()).then_some(path);
}

fn save_settings(settings: Res<Settings>, mut file: ResMut<SettingsFile>) {
    if !settings.is_changed() || file.saved.as_ref() == Some(&*settings) {
        return;
    }
    file.saved = Some(settings.clone());
    if let Some(path) = &file.path {
        if let Err(e) = std::fs::write(path, settings.to_text()) {
            warn!("could not save settings to {}: {e}", path.display());
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Puts the effects setting on the player's camera (when it changes, or the
/// camera appears).
fn apply_effects(settings: Res<Settings>, cameras: Query<(Entity, Ref<crate::player::PlayerCamera>)>, mut commands: Commands) {
    use bevy::anti_alias::smaa::{Smaa, SmaaPreset};
    use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
    use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel as SsaoQuality};
    // FC3_EFFECTS (tests) overrides the setting.
    let q = std::env::var("FC3_EFFECTS").ok().and_then(|v| Quality::from_name(&v)).unwrap_or(settings.effects);
    for (e, cam) in &cameras {
        if !settings.is_changed() && !cam.is_added() {
            continue;
        }
        let mut c = commands.entity(e);
        match q {
            Quality::Low => c.remove::<Smaa>(),
            Quality::Ultra => c.insert(Smaa { preset: SmaaPreset::High }),
            _ => c.insert(Smaa { preset: SmaaPreset::Medium }),
        };
        // Screen-space ambient occlusion, as the game has (FC3_SSAO=0 leaves it off).
        if q >= Quality::High && std::env::var("FC3_SSAO").as_deref() != Ok("0") {
            let quality_level = if q == Quality::Ultra { SsaoQuality::High } else { SsaoQuality::Medium };
            c.insert((DepthPrepass, NormalPrepass, ScreenSpaceAmbientOcclusion { quality_level, ..default() }));
        } else {
            c.remove::<(ScreenSpaceAmbientOcclusion, NormalPrepass, DepthPrepass)>();
        }
        if q >= Quality::High {
            // Bloom: the game's or Bevy's (`bloom.rs` picks).
            c.insert((crate::bloom::WantsBloom, crate::light_shafts::LightShafts::default()));
        } else {
            c.remove::<(crate::bloom::WantsBloom, crate::light_shafts::LightShafts)>();
        }
    }
}

fn apply_settings(
    settings: Res<Settings>,
    mut volume: ResMut<GlobalVolume>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut original: ResMut<crate::original_shaders::OriginalShaders>,
    mut lights: Query<(&mut DirectionalLight, &mut bevy::light::CascadeShadowConfig)>,
) {
    if !settings.is_changed() {
        return;
    }
    volume.volume = Volume::Linear(settings.master_volume);
    if let Ok(mut w) = windows.single_mut() {
        let present = if settings.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
        if w.present_mode != present {
            w.present_mode = present;
        }
        let mode = if settings.fullscreen { WindowMode::BorderlessFullscreen(MonitorSelection::Current) } else { WindowMode::Windowed };
        if w.mode != mode {
            w.mode = mode;
        }
    }
    if original.enabled != settings.original_shaders {
        original.enabled = settings.original_shaders;
    }
    for (mut light, mut cascades) in &mut lights {
        light.shadow_maps_enabled = settings.shadows != Quality::Low;
        let (count, distance) = match settings.shadows {
            Quality::Low | Quality::Medium => (2, 150.0),
            Quality::High => (4, 400.0),
            Quality::Ultra => (4, 800.0),
        };
        *cascades =
            bevy::light::CascadeShadowConfigBuilder { num_cascades: count, first_cascade_far_bound: 12.0, maximum_distance: distance, ..default() }.build();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let mut s = Settings {
            sensitivity: 1.7,
            fov: 90.0,
            invert_y: true,
            view_distance: Quality::Ultra,
            difficulty: Difficulty::Warrior,
            brightness: 70.0,
            gamma: 20.0,
            ..Default::default()
        };
        s.rebind(Action::Jump, Binding::Key(KeyCode::KeyV));
        s.rebind(Action::Fire, Binding::Key(KeyCode::KeyW));
        // Taking W from "forward" hands it fire's old binding.
        assert_eq!(s.binding(Action::Forward), Some(Binding::Mouse(MouseButton::Left)));
        assert_eq!(Settings::from_text(&s.to_text()), s);
    }

    #[test]
    fn calibration_maps_the_sliders_as_the_engine_does() {
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        // 50 everywhere leaves the picture.
        assert!(near(Settings::default().color_adjustment(), [0.0, 1.0, 1.0]));
        // The far ends: brightness 1.1 (an offset of 0.1 / 6), contrast 2
        // (a scale of 1.5), gamma 1.2 (an exponent of 1 / 1.2).
        let top = Settings { brightness: 100.0, contrast: 100.0, gamma: 100.0, ..Default::default() };
        assert!(near(top.color_adjustment(), [0.1 / 6.0, 1.5, 1.0 / 1.2]));
        let bottom = Settings { brightness: 0.0, contrast: 0.0, gamma: 0.0, ..Default::default() };
        assert!(near(bottom.color_adjustment(), [-0.1 / 6.0, 0.5, 1.0 / 0.8]));
    }
}
