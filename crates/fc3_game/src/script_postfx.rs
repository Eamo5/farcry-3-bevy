//! Opt-in script post-effects. Authored sequence fades run on gameplay time;
//! rendering currently handles masked radial blur only.
use std::collections::HashMap;
use bevy::prelude::*;
use fc3_formats::{cinema::Sequence, postfx::Effect};
use crate::screen_blur::{BlurMask, ScreenBlur};

pub struct PostFxPlugin;
impl Plugin for PostFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PostEffects>().add_systems(Startup, load)
            .add_systems(Update, update.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase { In, Loop, Out }
struct Playing { effect: Effect, phase: Phase, time: f32, order: u64 }
impl Playing {
    fn sequence(&self) -> &str {
        match self.phase {
            Phase::In => self.effect.fade_in.sequence.as_deref().unwrap_or(&self.effect.sequence),
            Phase::Loop => &self.effect.sequence,
            Phase::Out => self.effect.fade_out.sequence.as_deref().unwrap_or(&self.effect.sequence),
        }
    }
}

#[derive(Resource, Default)]
pub struct PostEffects {
    definitions: HashMap<String, Effect>,
    sequences: HashMap<String, Sequence>,
    masks: HashMap<String, Handle<Image>>,
    active: HashMap<String, Playing>,
    requests: Vec<(String, bool)>,
    order: u64,
    enabled: bool,
}
impl PostEffects {
    pub fn request(&mut self, name: &str, start: bool) {
        if self.enabled { self.requests.push((name.to_ascii_lowercase(), start)); }
    }
    pub fn reset(&mut self) { self.active.clear(); self.requests.clear(); }

    fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.active.retain(|_, playing| {
            playing.time += dt;
            let Some(sequence) = self.sequences.get(&playing.sequence().to_ascii_lowercase()) else { return false };
            if playing.time < sequence.duration { return true; }
            match playing.phase {
                Phase::In => {
                    playing.time -= sequence.duration;
                    playing.phase = Phase::Loop;
                    let Some(sequence) = self.sequences.get(&playing.sequence().to_ascii_lowercase()) else { return false };
                    if sequence.looping && sequence.duration > 0.0 { playing.time %= sequence.duration; true }
                    else { playing.time <= sequence.duration }
                }
                Phase::Loop if sequence.looping && sequence.duration > 0.0 => { playing.time %= sequence.duration; true }
                _ => false,
            }
        });
        for (name, start) in std::mem::take(&mut self.requests) {
            if start {
                if self.active.get(&name).is_some_and(|p| p.phase != Phase::Out) { continue; }
                let Some(effect) = self.definitions.get(&name).cloned() else { continue };
                self.order += 1;
                let phase = if effect.fade_in.kind == 1 { Phase::In } else { Phase::Loop };
                info!("postfx: start {} ({phase:?})", effect.name);
                self.active.insert(name, Playing { effect, phase, time: 0.0, order: self.order });
            } else if let Some(playing) = self.active.get_mut(&name) {
                if playing.phase == Phase::Out { continue; }
                if playing.effect.fade_out.kind == 1 {
                    playing.phase = Phase::Out;
                    playing.time = 0.0;
                    info!("postfx: fade out {}", playing.effect.name);
                } else {
                    info!("postfx: stop {}", playing.effect.name);
                    self.active.remove(&name);
                }
            }
        }
    }
}

fn load(mut effects: ResMut<PostEffects>, server: Res<AssetServer>) {
    if std::env::var("FC3_POSTFX_GAMEPLAY_TEST").as_deref() != Ok("1") { return; }
    let result = (|| -> anyhow::Result<()> {
        let root = fc3_formats::cache::default_cache_root();
        effects.sequences = fc3_formats::cinema::parse_postfx_library(&std::fs::read_to_string(
            root.join(fc3_formats::cinema::POSTFX_CACHE_REL))?)?.into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
        for effect in fc3_formats::postfx::parse_library(&std::fs::read(root.join("story/entitylibrary.fcb"))?)? {
            // Limit this experimental path to the authored mushroom family.
            // Unsupported intensity/fade modes do not silently approximate.
            if !effect.sequence.to_ascii_lowercase().starts_with("postfx:mush_postfx.")
                || effect.intensity_control != 0 || effect.fade_in.kind > 1 || effect.fade_out.kind > 1 { continue; }
            let references = std::iter::once(&effect.sequence).chain(effect.fade_in.sequence.iter()).chain(effect.fade_out.sequence.iter());
            if !references.clone().all(|s| effects.sequences.contains_key(&s.to_ascii_lowercase())) { continue; }
            effects.definitions.insert(effect.name.to_ascii_lowercase(), effect);
        }
        let textures: Vec<String> = effects.sequences.values().flat_map(|s| &s.groups).flat_map(|g| &g.clips).filter_map(|c| match &c.kind {
            fc3_formats::cinema::ClipKind::Blur { texture, kind: 1, .. } if !texture.is_empty() => Some(texture.clone()), _ => None,
        }).collect();
        for texture in textures {
            effects.masks.entry(texture.clone()).or_insert_with(|| server.load_builder()
                .with_settings(|s: &mut bevy::image::ImageLoaderSettings| { s.is_srgb = false; })
                .load(crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(&texture))));
        }
        effects.enabled = true;
        Ok(())
    })();
    if let Err(e) = result { warn!("postfx: cannot load authored effects: {e:#}"); }
}

fn update(time: Res<Time>, mut effects: ResMut<PostEffects>, cameras: Query<Entity, With<crate::player::PlayerCamera>>, mut commands: Commands) {
    if !effects.enabled || std::env::var_os("FC3_POSTFX_TEST").is_some() { return; }
    effects.tick(time.delta_secs());
    let chosen = effects.active.values().filter_map(|playing| {
        let sequence = effects.sequences.get(&playing.sequence().to_ascii_lowercase())?;
        let (blur, texture) = crate::screen_blur::sampled(sequence, playing.time)?;
        let mask = effects.masks.get(texture)?;
        Some((playing.effect.priority, playing.order, blur, mask.clone()))
    }).max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for entity in &cameras {
        match &chosen {
            Some((_, _, blur, mask)) => { commands.entity(entity).insert((*blur, BlurMask(mask.clone()))); }
            None => { commands.entity(entity).remove::<(ScreenBlur, BlurMask)>(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playback() -> PostEffects {
        let effect = Effect { name: "mush".into(), id: 1, priority: 1.0, intensity_control: 0, sequence: "loop".into(),
            fade_in: fc3_formats::postfx::Fade { kind: 1, time: 0.0, sequence: Some("in".into()) },
            fade_out: fc3_formats::postfx::Fade { kind: 1, time: 0.0, sequence: Some("out".into()) } };
        PostEffects { enabled: true, definitions: HashMap::from([("mush".into(), effect)]),
            sequences: [("in", 3.0, false), ("loop", 2.0, true), ("out", 3.0, false)].into_iter().map(|(name, duration, looping)|
                (name.into(), Sequence { name: name.into(), duration, looping, ..default() })).collect(), ..default() }
    }

    #[test]
    fn authored_fade_lifecycle_preserves_overshoot_and_stop_is_idempotent() {
        let mut fx = playback();
        fx.request("MUSH", true);
        fx.tick(0.5);
        assert_eq!(fx.active["mush"].phase, Phase::In);
        assert_eq!(fx.active["mush"].time, 0.0);
        fx.tick(7.5);
        assert_eq!(fx.active["mush"].phase, Phase::Loop);
        assert_eq!(fx.active["mush"].time, 0.5);
        fx.request("mush", true);
        fx.tick(0.0);
        assert_eq!(fx.active["mush"].time, 0.5);
        fx.request("mush", false);
        fx.tick(0.0);
        assert_eq!(fx.active["mush"].phase, Phase::Out);
        fx.tick(1.0);
        fx.request("mush", false);
        fx.tick(0.0);
        assert_eq!(fx.active["mush"].time, 1.0);
        fx.tick(2.0);
        assert!(fx.active.is_empty());
        fx.request("mush", true);
        fx.tick(0.0);
        fx.request("mush", false);
        fx.tick(0.0);
        assert_eq!(fx.active["mush"].phase, Phase::Out);
        fx.request("mush", true);
        fx.tick(0.0);
        assert_eq!(fx.active["mush"].phase, Phase::In);
        fx.request("mush", true);
        fx.reset();
        fx.tick(1.0);
        assert!(fx.active.is_empty() && fx.requests.is_empty());
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with mushroom effect definitions"]
    fn installed_mushroom_script_start_stop_and_retry_remove_render_ownership() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let mut fx = PostEffects { enabled: true, ..default() };
        fx.definitions = fc3_formats::postfx::parse_library(&std::fs::read(root.join("story/entitylibrary.fcb")).unwrap()).unwrap()
            .into_iter().map(|e| (e.name.to_ascii_lowercase(), e)).collect();
        fx.sequences = fc3_formats::cinema::parse_postfx_library(&std::fs::read_to_string(root.join(fc3_formats::cinema::POSTFX_CACHE_REL)).unwrap())
            .unwrap().into_iter().map(|s| (s.name.to_ascii_lowercase(), s)).collect();
        fx.masks.insert(r"graphics\postfx\mushbuzz_vignette_01.xbt".into(), Handle::default());
        let mut app = App::new();
        app.init_resource::<Time>().insert_resource(fx).add_systems(Update, update);
        let camera = app.world_mut().spawn(crate::player::PlayerCamera).id();
        app.world_mut().resource_mut::<PostEffects>().request("postfx:mush_postfx.02", true);
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_some());
        app.world_mut().resource_mut::<PostEffects>().request("postfx:mush_postfx.02", false);
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_none());
        app.world_mut().resource_mut::<PostEffects>().request("postfx:mush_postfx.03", true);
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_some());
        app.world_mut().resource_mut::<PostEffects>().reset();
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_none());
        assert!(app.world().get::<BlurMask>(camera).is_none());
    }
}
