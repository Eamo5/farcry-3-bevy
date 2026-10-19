//! The game's screen post-effects: the authored sequences of
//! `cinema_postfx.xml`, played by name through the PostFX database
//! (`PostFXs:PostFXs/Database` in the entity library) on the player's view.
//! A hit's red flash, critical health's grey and blurred edges, a blast's
//! flash and punch, the speed of a zipline or glider, the drunk and mushroom
//! trips, the iron sights' blurred edges, fades to black or white. Scripts
//! start and stop them by name (`CAPI_Player.StartPostFX`); the player's
//! state starts the rest (`player_effects`).
//!
//! An effect plays its fade-in sequence (or ramps in over its fade time),
//! then its loop, then its fade-out; a loop that doesn't repeat ends the
//! effect. Each frame the strongest effect's clip of each kind (by the
//! database's priority, the latest on a tie) goes to one screen pass
//! (`post_effects.wgsl`) after tone mapping. The pass follows the game's own
//! shaders where they were found (`docs/shaders.md`, Post-effects): the
//! source warped (distortion) and split per channel (RGB separation),
//! blurred through a mask, desaturated toward a colour, hue-shifted and
//! burned, then up to two colour masks in the game's blend modes, and noise.
//! Bloom clips open the scene's exposure and the grade's bloom; camera-shake
//! clips add to the view's shake.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::uniform::{ComponentUniforms, DynamicUniformIndex, UniformComponentPlugin};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use fc3_formats::cinema::{ClipKind, PostClass, Sequence, FOV_STEP};
use fc3_formats::postfx::Effect;

use crate::player::PlayerCamera;

pub struct PostEffectsPlugin;

impl Plugin for PostEffectsPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "post_effects.wgsl");
        app.add_plugins((
            bevy::render::extract_resource::ExtractResourcePlugin::<PostLight>::default(),
            ExtractComponentPlugin::<PostView>::default(),
            UniformComponentPlugin::<PostView>::default(),
            ExtractComponentPlugin::<PostTextures>::default(),
        ))
        .init_resource::<PostFx>()
        .init_resource::<PostLight>()
        .init_resource::<PostShake>()
        .add_systems(Startup, white_texture)
        .add_systems(OnExit(crate::menu::GameState::Loading), load)
        .add_systems(Update, (fixture, player_effects, play).chain().after(crate::story::StorySet));
        let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
        render
            .init_gpu_resource::<SpecializedRenderPipelines<PostPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare.in_set(RenderSystems::Prepare))
            .add_systems(Core3d, draw.in_set(Core3dSystems::PostProcess).after(bevy::core_pipeline::tonemapping::tonemapping));
    }
}

/// The effects: the database, the sequences, and what's playing.
#[derive(Resource, Default)]
pub struct PostFx {
    effects: HashMap<String, Effect>,
    ids: HashMap<u32, String>,
    sequences: HashMap<String, Arc<Sequence>>,
    textures: HashMap<String, Handle<Image>>,
    playing: Vec<Playing>,
    requests: Vec<(String, Request)>,
    order: u64,
    /// The effects a state holds on (see [`PostFx::hold`]).
    held: std::collections::HashSet<String>,
    /// Effects started (true) and ended since last asked, by id (the CRC-32
    /// of the lower-case name, as scripts' `GetNoCaseStringID`).
    events: Vec<(bool, u32)>,
}

#[derive(Clone, Copy)]
enum Request {
    Start,
    /// From the start, playing or not (another hit).
    Restart,
    Stop,
    Intensity(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    In,
    Loop,
    Out,
}

struct Playing {
    name: String,
    effect: Effect,
    phase: Phase,
    /// Seconds into the phase's sequence.
    time: f32,
    order: u64,
    /// A timed fade's weight (1 once in), and whether it's fading out.
    weight: f32,
    leaving: bool,
    /// What the game sets on effects it drives by intensity.
    intensity: f32,
}

impl PostFx {
    /// Starts `name`: an effect of the database (by name or id), else a
    /// sequence of the library by its own name. A playing one goes on.
    pub fn start(&mut self, name: &str) {
        self.requests.push((name.to_ascii_lowercase(), Request::Start));
    }

    /// Plays `name` from its start again (a hit on a hit).
    pub fn restart(&mut self, name: &str) {
        self.requests.push((name.to_ascii_lowercase(), Request::Restart));
    }

    /// Ends `name` through its fade-out.
    pub fn stop(&mut self, name: &str) {
        self.requests.push((name.to_ascii_lowercase(), Request::Stop));
    }

    /// A script's request (`StartPostFX` or `StopPostFX`).
    pub fn request(&mut self, name: &str, start: bool) {
        if start {
            self.start(name)
        } else {
            self.stop(name)
        }
    }

    /// Keeps `name` on while `on` (a state: sprinting, under water): started
    /// when it isn't playing, ended as `on` goes if this started it (not a
    /// script's).
    pub fn hold(&mut self, name: &str, on: bool) {
        let key = self.resolve(name);
        if on {
            let live = self.playing.iter().any(|p| p.name == key && p.phase != Phase::Out && !p.leaving);
            let pending = self.requests.iter().any(|(n, r)| matches!(r, Request::Start | Request::Restart) && self.resolve(n) == key);
            if !live && !pending {
                self.start(name);
            }
            self.held.insert(key);
        } else if self.held.remove(&key) {
            self.stop(name);
        }
    }

    /// Sets the strength of an effect the game drives by intensity.
    pub fn set_intensity(&mut self, name: &str, value: f32) {
        self.requests.push((name.to_ascii_lowercase(), Request::Intensity(value.clamp(0.0, 1.0))));
    }

    /// Whether `name` is playing.
    pub fn playing(&self, name: &str) -> bool {
        let key = self.resolve(name);
        self.playing.iter().any(|p| p.name == key)
    }

    /// The effects that started and ended since last asked (for scripts'
    /// `DominoEvent_FxStart` and `DominoEvent_FxStop`).
    pub fn take_events(&mut self) -> Vec<(bool, u32)> {
        std::mem::take(&mut self.events)
    }

    /// Clears every effect (a checkpoint loaded).
    pub fn reset(&mut self) {
        self.playing.clear();
        self.requests.clear();
        self.held.clear();
    }

    /// The database's name for `name` (a name, a decimal or hex id), or the
    /// name as given (a sequence's own).
    fn resolve(&self, name: &str) -> String {
        let name = name.trim().to_ascii_lowercase();
        if self.effects.contains_key(&name) {
            return name;
        }
        let id = name.parse::<u32>().ok().or_else(|| name.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok()));
        id.and_then(|id| self.ids.get(&id).cloned()).unwrap_or_else(|| self.ids.get(&fc3_formats::hash::crc32(name.as_bytes())).cloned().unwrap_or(name))
    }

    /// The effect `name` plays: the database's, or a sequence's alone.
    fn effect(&self, name: &str) -> Option<Effect> {
        if let Some(e) = self.effects.get(name) {
            return Some(e.clone());
        }
        let seq = self.sequences.get(name)?;
        let none = fc3_formats::postfx::Fade { kind: 0, time: 0.0, sequence: None };
        Some(Effect {
            name: name.to_string(),
            id: 0,
            priority: 1000.0,
            sequence: seq.name.clone(),
            intensity_control: 0,
            fade_in: none.clone(),
            fade_out: none,
        })
    }

    fn sequence(&self, name: &str) -> Option<&Arc<Sequence>> {
        self.sequences.get(&name.to_ascii_lowercase())
    }

    /// The sequence a playing effect is in and how far (looped).
    fn current(&self, p: &Playing) -> Option<(Arc<Sequence>, f32)> {
        let name = match p.phase {
            Phase::In => p.effect.fade_in.sequence.as_deref().unwrap_or(&p.effect.sequence),
            Phase::Loop => &p.effect.sequence,
            Phase::Out => p.effect.fade_out.sequence.as_deref().unwrap_or(&p.effect.sequence),
        };
        let seq = self.sequence(name)?.clone();
        let t = if seq.looping && seq.duration > 0.0 { p.time.rem_euclid(seq.duration) } else { p.time.min(seq.duration) };
        Some((seq, t))
    }

    fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        for (name, request) in std::mem::take(&mut self.requests) {
            let key = self.resolve(&name);
            let id = fc3_formats::hash::crc32(key.as_bytes());
            match request {
                Request::Start | Request::Restart => {
                    if matches!(request, Request::Restart) && self.playing.iter().any(|p| p.name == key) {
                        self.playing.retain(|p| p.name != key);
                        self.events.push((false, id));
                    }
                    if let Some(p) = self.playing.iter_mut().find(|p| p.name == key) {
                        // Back in from its fade-out.
                        if p.phase == Phase::Out {
                            p.phase = Phase::Loop;
                            p.time = 0.0;
                        }
                        p.leaving = false;
                        continue;
                    }
                    let Some(effect) = self.effect(&key) else {
                        debug!("postfx: no effect {name:?}");
                        continue;
                    };
                    self.order += 1;
                    let phase = if effect.fade_in.kind == 1 && effect.fade_in.sequence.is_some() { Phase::In } else { Phase::Loop };
                    let weight = if effect.fade_in.kind == 2 && effect.fade_in.time > 0.0 { 0.0 } else { 1.0 };
                    debug!("postfx: start {key}");
                    self.events.push((true, id));
                    self.playing.push(Playing { name: key, effect, phase, time: 0.0, order: self.order, weight, leaving: false, intensity: 1.0 });
                }
                Request::Stop => {
                    let Some(i) = self.playing.iter().position(|p| p.name == key) else { continue };
                    let p = &mut self.playing[i];
                    match p.effect.fade_out.kind {
                        1 if p.effect.fade_out.sequence.is_some() && p.phase != Phase::Out => {
                            p.phase = Phase::Out;
                            p.time = 0.0;
                        }
                        2 if p.effect.fade_out.time > 0.0 => p.leaving = true,
                        1 => {}
                        _ => {
                            debug!("postfx: end {key}");
                            self.playing.remove(i);
                            self.events.push((false, id));
                        }
                    }
                }
                Request::Intensity(v) => {
                    if let Some(p) = self.playing.iter_mut().find(|p| p.name == key) {
                        p.intensity = v;
                    }
                }
            }
        }
        let sequences = &self.sequences;
        let length = |name: &str| sequences.get(&name.to_ascii_lowercase()).map(|s| (s.duration, s.looping));
        let mut ended = Vec::new();
        self.playing.retain_mut(|p| {
            let keep = advance(p, dt, &length);
            if !keep {
                debug!("postfx: end {}", p.name);
                ended.push((false, fc3_formats::hash::crc32(p.name.as_bytes())));
            }
            keep
        });
        self.events.extend(ended);
    }
}

/// Moves a playing effect on by `dt`; false once it has ended.
fn advance(p: &mut Playing, dt: f32, length: &dyn Fn(&str) -> Option<(f32, bool)>) -> bool {
    p.time += dt;
    if p.effect.fade_in.kind == 2 && !p.leaving {
        p.weight = (p.weight + dt / p.effect.fade_in.time.max(1e-3)).min(1.0);
    }
    if p.leaving {
        p.weight -= dt / p.effect.fade_out.time.max(1e-3);
        if p.weight <= 0.0 {
            return false;
        }
    }
    match p.phase {
        Phase::In => {
            let Some((d, _)) = p.effect.fade_in.sequence.as_deref().and_then(length) else { return false };
            if p.time >= d {
                p.time -= d;
                p.phase = Phase::Loop;
            }
            true
        }
        Phase::Loop => match length(&p.effect.sequence) {
            Some((d, looping)) => looping || p.time <= d,
            None => false,
        },
        Phase::Out => {
            let Some((d, _)) = p.effect.fade_out.sequence.as_deref().and_then(length) else { return false };
            p.time <= d
        }
    }
}

/// What the player's state last started.
#[derive(Default)]
struct Seen {
    /// The side the last animal's strike came from.
    slash_right: bool,
    landing: f32,
    boosts: Vec<crate::syringes::Syringe>,
}

/// Each syringe's effect as it goes in.
fn syringe_effect(s: crate::syringes::Syringe) -> &'static str {
    use crate::syringes::Syringe::*;
    match s {
        WildlifeVision => "wildlifevisionfx",
        EnemyVision => "enemyvisionfx",
        PlantVision => "plantvisionfx",
        Fireproof => "fireprooffx",
        NeverDrown => "neverdrownfx",
        SprintBurst => "sprintburstfx",
        AnimalDamage => "animaldamagefx",
        Invincible => "invinciblefx",
        AnimalRepellent => "animalrepellantfx",
        OneHitKill => "onehitkillfx",
        Steady => "bulletspreadreductionfx",
        HealthBoost => "healthboostfx",
    }
}

/// How far a blast reaches the player's view (its radius times this).
const BLAST_REACH: f32 = 3.0;
/// A landing this hard (m/s) jars the view, full at the second.
const LANDING_SPEEDS: [f32; 2] = [7.0, 14.0];

/// The player's own effects, as the game starts them from the player's
/// state: a hit (an animal's strike, a blow), a round passing close,
/// critical health, death, a blast near by, sprinting and sliding, a zip line's, glider's, wingsuit's
/// or vehicle's speed, a long fall and a hard landing, under water and out
/// of air, the iron sights and binoculars, a syringe going in.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn player_effects(
    state: Res<State<crate::menu::GameState>>,
    mut fx: ResMut<PostFx>,
    mut hits: MessageReader<crate::combat::DamageEvent>,
    mut blasts: MessageReader<crate::weapons::Explosion>,
    mut sounds: MessageReader<crate::sound::PlaySfx>,
    players: Query<(Entity, &crate::player::Player, &crate::combat::Health, &GlobalTransform, &crate::weapons::Inventory)>,
    animals: Query<(), With<crate::wildlife::WildAnimal>>,
    (mount, driving, vehicles): (
        Res<crate::traversal::Mount>,
        Res<crate::vehicles::Driving>,
        Query<(&crate::vehicles::Vehicle, &avian3d::prelude::LinearVelocity)>,
    ),
    (breath, boosts, bino): (Option<Res<crate::syringes::Breath>>, Res<crate::syringes::Boosts>, Res<crate::stealth::Binoculars>),
    mut seen: Local<Seen>,
) {
    use crate::combat::{DamageKind, HEALTH_SEGMENT};
    let playing = *state.get() == crate::menu::GameState::Playing;
    let Ok((pe, p, h, at, inv)) = players.single().map_err(|_| ()).and_then(|x| if playing && !fx.effects.is_empty() { Ok(x) } else { Err(()) }) else {
        hits.clear();
        blasts.clear();
        sounds.clear();
        return;
    };
    // A round passing close by (its whizz is played for it).
    if sounds.read().any(|s| matches!(s.event, crate::sound::ev::PASS_BY | crate::sound::ev::PASS_BY_UNDERWATER)) {
        fx.restart("nearmiss");
    }
    for hit in hits.read().filter(|d| d.target == pe && d.amount > 0.0) {
        match hit.kind {
            _ if hit.instigator.is_some_and(|i| animals.contains(i)) => {
                seen.slash_right = !seen.slash_right;
                fx.restart(if seen.slash_right { "animalslash_rightside" } else { "animalslash_leftside" });
            }
            DamageKind::Melee => fx.restart("meleehitfx"),
            DamageKind::Drown | DamageKind::Explosion => {}
            _ => fx.restart("sp_health_hit"),
        }
    }
    let me = at.translation();
    for b in blasts.read() {
        let reach = (b.radius * BLAST_REACH).max(8.0);
        let d = b.at.distance(me);
        if d < reach {
            fx.restart("grenade_explosionfx");
            fx.set_intensity("grenade_explosionfx", 1.0 - d / reach);
        }
    }
    fx.hold("criticalhealth", !h.dead && h.current < HEALTH_SEGMENT);
    fx.hold("death", h.dead);
    let speed = p.velocity.length();
    fx.hold("sprintingfx", p.sprinting && p.grounded && speed > 3.0);
    fx.hold("slidingfx", p.sliding.is_some());
    let zip = match *mount {
        crate::traversal::Mount::Zip { speed, .. } => Some(speed),
        _ => None,
    };
    fx.hold("ziplinespeedfx", zip.is_some());
    if let Some(s) = zip {
        fx.set_intensity("ziplinespeedfx", s / 20.0);
    }
    let ride = driving.vehicle.and_then(|v| vehicles.get(v).ok()).map(|(v, lv)| (v.kind(), lv.0.length()));
    let gliding = ride.filter(|(k, _)| *k == crate::vehicles::Kind::Glider);
    fx.hold("handgliderspeedfx", gliding.is_some());
    if let Some((_, s)) = gliding {
        fx.set_intensity("handgliderspeedfx", s / 30.0);
    }
    let fast = ride.filter(|(k, s)| *k != crate::vehicles::Kind::Glider && *s > 15.0);
    fx.hold("vehiclesspeedfx", fast.is_some());
    if let Some((_, s)) = fast {
        fx.set_intensity("vehiclesspeedfx", (s - 15.0) / 25.0);
    }
    let suit = p.air == crate::player::AirMode::Wingsuit;
    fx.hold("wingsuitspeedfx", suit);
    if suit {
        fx.set_intensity("wingsuitspeedfx", speed / 50.0);
    }
    fx.hold("fallingfx", !p.grounded && !p.swimming && !suit && driving.vehicle.is_none() && zip.is_none() && p.velocity.y < -15.0);
    if p.grounded && p.last_landing_speed != seen.landing {
        seen.landing = p.last_landing_speed;
        if seen.landing > LANDING_SPEEDS[0] {
            fx.restart("landingfx");
            fx.set_intensity("landingfx", ((seen.landing - LANDING_SPEEDS[0]) / (LANDING_SPEEDS[1] - LANDING_SPEEDS[0])).clamp(0.2, 1.0));
        }
    }
    let under = breath.as_ref().is_some_and(|b| b.under);
    fx.hold("diving", under);
    fx.hold("drowning_health", under && breath.as_ref().is_some_and(|b| b.left < b.max * 0.35));
    fx.hold("ironsightfx", inv.aim_blend > 0.5 && !inv.scoped() && driving.vehicle.is_none());
    fx.hold("binocularfx", bino.blend > 0.5);
    let now: Vec<crate::syringes::Syringe> = boosts.left.iter().filter(|(_, t)| **t > 0.0).map(|(s, _)| *s).collect();
    for s in &now {
        if !seen.boosts.contains(s) {
            fx.restart(syringe_effect(*s));
        }
    }
    seen.boosts = now;
}

/// What bloom clips do to the scene this frame: multipliers on the
/// exposure and the grade's bloom.
#[derive(Resource, Clone, Copy, bevy::render::extract_resource::ExtractResource)]
pub struct PostLight {
    pub exposure: f32,
    pub bloom: f32,
}

impl Default for PostLight {
    fn default() -> Self {
        PostLight { exposure: 1.0, bloom: 1.0 }
    }
}

/// Camera-shake clips' view offset this frame (roll, pitch, yaw; radians).
#[derive(Resource, Default, Clone, Copy)]
pub struct PostShake(pub Vec3);

/// One frame's screen pass, as the shader takes it.
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType, Default)]
pub struct PostView {
    /// Blur: factor, kind, texture alpha, mask offset; its colour; its
    /// direction, whether it has a mask.
    blur: [Vec4; 3],
    /// Two colour masks: colour and alpha; scale-offset; tiling, alpha
    /// offset, blend mode; textured, render last, on.
    masks: [Vec4; 8],
    /// Desaturation: colour, scale.
    desat: Vec4,
    /// Colour burn: strength, threshold, contrast, saturation; colour.
    burn: [Vec4; 2],
    /// RGB separation: per channel scale and offset (uv); opacity, alpha
    /// fade, masked, on.
    rgb: [Vec4; 4],
    /// Hue and saturation: the saturation's offset (-1 grey), the hue's
    /// turn (whole turns), on.
    hue: Vec4,
    /// Noise: intensity, tiling, a random offset, textured.
    noise: Vec4,
    /// Distortion: scale, scroll; blurriness, factor, threshold, on.
    distort: [Vec4; 2],
    /// One over the screen size, seconds.
    screen: Vec4,
}

impl PostView {
    fn active(&self) -> bool {
        (self.blur[0].x > 0.0 && self.blur[0].z > 0.0)
            || self.masks[3].z > 0.0
            || self.masks[7].z > 0.0
            || self.desat.w > 0.0
            || self.burn[0].x > 0.0
            || self.rgb[3].w > 0.0
            || self.hue.z > 0.0
            || self.noise.x > 0.0
            || self.distort[1].w > 0.0
    }
}

/// The pass's textures: the blur's mask, the two colour masks', the RGB
/// separation's mask, noise and distortion (white where there's none).
#[derive(Component, Clone, ExtractComponent)]
pub struct PostTextures([Handle<Image>; 6]);

#[derive(Resource)]
struct White(Handle<Image>);

fn white_texture(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = Image::new_fill(
        bevy::render::render_resource::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8Unorm,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    commands.insert_resource(White(images.add(image)));
}

fn load(mut fx: ResMut<PostFx>, info: Res<crate::data::CacheInfo>, white: Option<Res<White>>) {
    let result = (|| -> anyhow::Result<()> {
        let xml = std::fs::read_to_string(info.root.join(fc3_formats::cinema::POSTFX_CACHE_REL))?;
        fx.sequences = fc3_formats::cinema::parse_postfx_library(&xml)?.into_iter().map(|s| (s.name.to_ascii_lowercase(), Arc::new(s))).collect();
        let effects = fc3_formats::postfx::parse_library(&std::fs::read(info.root.join("story/entitylibrary.fcb"))?)?;
        for e in effects {
            let name = e.name.to_ascii_lowercase();
            fx.ids.insert(e.id, name.clone());
            fx.ids.insert(fc3_formats::hash::crc32(name.as_bytes()), name.clone());
            fx.effects.insert(name, e);
        }
        Ok(())
    })();
    // Textures the game doesn't ship (a few clips name them) stay plain.
    if let Some(white) = white {
        let named: Vec<String> = fx
            .sequences
            .values()
            .flat_map(|s| &s.groups)
            .flat_map(|g| &g.clips)
            .filter_map(|c| match &c.kind {
                ClipKind::Blur { texture, .. } | ClipKind::Post { texture, .. } => Some(texture.to_ascii_lowercase()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect();
        for t in named {
            if !info.root.join(fc3_formats::cache::texture_rel(&t)).exists() {
                fx.textures.insert(t, white.0.clone());
            }
        }
    }
    match result {
        Ok(()) => info!("post-effects: {} effects over {} sequences", fx.effects.len(), fx.sequences.len()),
        Err(e) => warn!("post-effects: {e:#}"),
    }
}

/// `FC3_POSTFX_TEST=<effect or sequence>`: plays it from the start, held
/// on; `FC3_POSTFX_TEST_TIME=<s>` holds it at that time.
fn fixture(mut fx: ResMut<PostFx>, mut started: Local<bool>) {
    let Ok(name) = std::env::var("FC3_POSTFX_TEST") else { return };
    if fx.sequences.is_empty() {
        return;
    }
    if !*started {
        *started = true;
        info!("postfx test: {name}");
    }
    if !fx.playing(&name) {
        fx.start(&name);
    }
}

/// What one clip asks of the pass, ranked by its effect's priority, the
/// clip's own and the effect's start.
type Rank = (f32, f32, u64);

#[derive(Default)]
struct Mix {
    blur: Option<(Rank, [Vec4; 3], String)>,
    masks: Vec<(Rank, [Vec4; 4], String)>,
    desat: Option<(Rank, Vec4)>,
    burn: Option<(Rank, [Vec4; 2])>,
    rgb: Option<(Rank, [Vec4; 4], String)>,
    hue: Option<(Rank, Vec4)>,
    noise: Option<(Rank, Vec4, String)>,
    distort: Option<(Rank, [Vec4; 2], String)>,
    bloom: Option<(Rank, (f32, f32))>,
    shake: Vec3,
}

fn better<T>(slot: &mut Option<(Rank, T)>, rank: Rank, value: T) {
    if slot.as_ref().is_none_or(|(r, _)| rank_cmp(&rank, r).is_ge()) {
        *slot = Some((rank, value));
    }
}

fn rank_cmp(a: &Rank, b: &Rank) -> std::cmp::Ordering {
    a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2))
}

impl Mix {
    /// Adds a playing effect's clips at `t` into its sequence, at `w`.
    fn add(&mut self, seq: &Sequence, t: f32, w: f32, priority: f32, order: u64, seconds: f32) {
        for c in seq.groups.iter().flat_map(|g| &g.clips) {
            if t < c.start || t > c.start + c.duration.max(FOV_STEP) {
                continue;
            }
            let i = ((t - c.start) / FOV_STEP) as usize;
            match &c.kind {
                ClipKind::Blur { texture, kind, priority: own, samples, .. } => {
                    let Some(v) = samples.get(i.min(samples.len().saturating_sub(1))) else { continue };
                    let rank = (priority, *own as f32, order);
                    let blur = [
                        Vec4::new(v[0].max(0.0), *kind as f32, v[7].max(0.0) * w, v[8]),
                        Vec4::new(v[1], v[2], v[3], v[4]),
                        Vec4::new(v[5], v[6], if texture.is_empty() { 0.0 } else { 1.0 }, 0.0),
                    ];
                    if self.blur.as_ref().is_none_or(|(r, _, _)| rank_cmp(&rank, r).is_ge()) {
                        self.blur = Some((rank, blur, texture.clone()));
                    }
                }
                ClipKind::Post { class, texture, settings, samples } => {
                    let Some(v) = samples.get(i.min(samples.len().saturating_sub(1))) else { continue };
                    let own = class.settings().iter().position(|s| *s == "nPriority").map_or(0.0, |i| settings[i]);
                    let rank = (priority, own, order);
                    match class {
                        PostClass::ColorMask => {
                            let textured = !texture.is_empty();
                            let mask = [
                                Vec4::new(v[0], v[1], v[2], v[3] * w),
                                Vec4::new(v[4], v[5], v[6], v[7]),
                                Vec4::new(v[8], v[9], v[10], settings[0]),
                                Vec4::new(if textured { 1.0 + settings[1] } else { 0.0 }, settings[2], 1.0, 0.0),
                            ];
                            self.masks.push(((priority, 0.0, order), mask, texture.clone()));
                        }
                        PostClass::Desaturation => better(&mut self.desat, rank, Vec4::new(v[0] * v[3], v[1] * v[3], v[2] * v[3], v[4] * w)),
                        PostClass::ColorBurn => better(&mut self.burn, rank, [Vec4::new(v[0] * w, v[5], v[6], v[4]), Vec4::new(v[1], v[2], v[3], 0.0)]),
                        PostClass::RgbSep => {
                            let rgb = [
                                Vec4::new(v[0], v[1], v[2], v[3]),
                                Vec4::new(v[4], v[5], v[6], v[7]),
                                Vec4::new(v[8], v[9], v[10], v[11]),
                                Vec4::new(1.0 - (1.0 - v[13].clamp(0.0, 1.0)) * w, v[12], if texture.is_empty() { 0.0 } else { 1.0 }, 1.0),
                            ];
                            if self.rgb.as_ref().is_none_or(|(r, _, _)| rank_cmp(&rank, r).is_ge()) {
                                self.rgb = Some((rank, rgb, texture.clone()));
                            }
                        }
                        PostClass::HueSat => better(&mut self.hue, rank, Vec4::new(v[0] * w, v[1] * w, 1.0, 0.0)),
                        PostClass::Noise => {
                            let noise = Vec4::new(v[0] * w, v[1].max(0.01), seconds.fract(), if texture.is_empty() { 0.0 } else { 1.0 });
                            if self.noise.as_ref().is_none_or(|(r, _, _)| rank_cmp(&rank, r).is_ge()) {
                                self.noise = Some((rank, noise, texture.clone()));
                            }
                        }
                        PostClass::Distortion => {
                            // (The offset scrolls on with time when it loops.)
                            let scroll = if settings[0] > 0.0 { Vec2::new(v[2], v[3]) * seconds } else { Vec2::new(v[2], v[3]) };
                            let distort = [Vec4::new(v[0], v[1], scroll.x, scroll.y), Vec4::new(v[4], v[5] * w, v[6], 1.0)];
                            if self.distort.as_ref().is_none_or(|(r, _, _)| rank_cmp(&rank, r).is_ge()) {
                                self.distort = Some((rank, distort, texture.clone()));
                            }
                        }
                        PostClass::Bloom => better(&mut self.bloom, rank, (1.0 + (v[0] - 1.0) * w, 1.0 + (v[1] - 1.0) * w)),
                        PostClass::Shake => {
                            // Amplitude in degrees, each axis its own frequency.
                            let (amp, freq) = (v[0] * w, v[1].max(0.0));
                            let wave = |f: f32, phase: f32| (std::f32::consts::TAU * f.max(0.1) * freq * seconds + phase).sin();
                            self.shake +=
                                Vec3::new(v[4] * wave(settings[2], 1.3), v[3] * wave(settings[1], 0.0), v[2] * wave(settings[3], 2.1)) * amp.to_radians();
                        }
                        PostClass::MotionBlur | PostClass::Slomo => {}
                    }
                }
                _ => {}
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn play(
    time: Res<Time>,
    state: Res<State<crate::menu::GameState>>,
    server: Res<AssetServer>,
    white: Option<Res<White>>,
    mut fx: ResMut<PostFx>,
    mut light: ResMut<PostLight>,
    mut shake: ResMut<PostShake>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<Entity, With<PlayerCamera>>,
    mut commands: Commands,
) {
    let fixed = std::env::var("FC3_POSTFX_TEST_TIME").ok().and_then(|s| s.parse::<f32>().ok());
    let playing = *state.get() == crate::menu::GameState::Playing;
    let dt = if playing || fixed.is_some() { time.delta_secs() } else { 0.0 };
    fx.tick(dt);
    if let Some(t) = fixed {
        for p in &mut fx.playing {
            p.time = t;
        }
    }
    let seconds = time.elapsed_secs();
    let mut mix = Mix::default();
    for p in &fx.playing {
        let Some((seq, t)) = fx.current(p) else { continue };
        let w = (p.weight * if p.effect.intensity_control != 0 { p.intensity } else { 1.0 }).clamp(0.0, 1.0);
        mix.add(&seq, t, w, p.effect.priority, p.order, seconds);
    }
    let (bloom, exposure) = mix.bloom.map_or((1.0, 1.0), |(_, (b, e))| (b, e));
    *light = PostLight { exposure, bloom };
    shake.0 = mix.shake;

    let Some(white) = white else { return };
    let mut texture = |path: &str| -> Handle<Image> {
        if path.is_empty() {
            return white.0.clone();
        }
        fx.textures
            .entry(path.to_string())
            .or_insert_with(|| {
                server
                    .load_builder()
                    .with_settings(|s: &mut bevy::image::ImageLoaderSettings| {
                        s.is_srgb = false;
                        s.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
                            address_mode_u: bevy::image::ImageAddressMode::Repeat,
                            address_mode_v: bevy::image::ImageAddressMode::Repeat,
                            ..bevy::image::ImageSamplerDescriptor::linear()
                        });
                    })
                    .load(crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(path)))
            })
            .clone()
    };
    let mut view = PostView::default();
    let mut textures: [Handle<Image>; 6] = std::array::from_fn(|_| white.0.clone());
    if let Some((_, blur, tex)) = mix.blur {
        view.blur = blur;
        textures[0] = texture(&tex);
    }
    // The two strongest colour masks, the one rendered last second.
    mix.masks.sort_by(|a, b| rank_cmp(&b.0, &a.0));
    mix.masks.truncate(2);
    mix.masks.sort_by(|a, b| a.1[3].y.total_cmp(&b.1[3].y).then(rank_cmp(&a.0, &b.0)));
    for (k, (_, mask, tex)) in mix.masks.iter().enumerate() {
        view.masks[k * 4..k * 4 + 4].copy_from_slice(mask);
        textures[1 + k] = texture(tex);
    }
    if let Some((_, d)) = mix.desat {
        view.desat = d;
    }
    if let Some((_, b)) = mix.burn {
        view.burn = b;
    }
    if let Some((_, rgb, tex)) = mix.rgb {
        view.rgb = rgb;
        textures[3] = texture(&tex);
    }
    if let Some((_, h)) = mix.hue {
        view.hue = h;
    }
    if let Some((_, n, tex)) = mix.noise {
        view.noise = n;
        textures[4] = texture(&tex);
    }
    if let Some((_, d, tex)) = mix.distort {
        view.distort = d;
        textures[5] = texture(&tex);
    }
    let size = windows.single().map_or(Vec2::new(1920.0, 1080.0), |w| Vec2::new(w.width().max(1.0), w.height().max(1.0)));
    view.screen = Vec4::new(1.0 / size.x, 1.0 / size.y, seconds, 0.0);
    let on = view.active();
    for camera in &cameras {
        if on {
            commands.entity(camera).insert((view, PostTextures(textures.clone())));
        } else {
            commands.entity(camera).remove::<(PostView, PostTextures)>();
        }
    }
}

#[derive(Resource)]
struct PostPipeline {
    layout: BindGroupLayoutDescriptor,
    clamp: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}

#[derive(Component, Deref)]
struct PipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let tex = || texture_2d(TextureSampleType::Float { filterable: true });
    commands.insert_resource(PostPipeline {
        layout: BindGroupLayoutDescriptor::new(
            "post-effects",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    tex(),
                    sampler(SamplerBindingType::Filtering),
                    tex(),
                    tex(),
                    tex(),
                    tex(),
                    tex(),
                    tex(),
                    sampler(SamplerBindingType::Filtering),
                    uniform_buffer::<PostView>(true),
                ),
            ),
        ),
        clamp: device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() }),
        fullscreen: fullscreen.clone(),
        shader: bevy::asset::load_embedded_asset!(server.as_ref(), "post_effects.wgsl"),
    });
}

impl SpecializedRenderPipeline for PostPipeline {
    type Key = TextureFormat;
    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("post-effects".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        }
    }
}

fn prepare(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<PostPipeline>>,
    pipeline: Res<PostPipeline>,
    views: Query<(Entity, &ExtractedView), With<PostView>>,
) {
    for (entity, view) in &views {
        commands.entity(entity).insert(PipelineId(pipelines.specialize(&cache, &pipeline, view.target_format)));
    }
}

fn draw(
    view: ViewQuery<(&ViewTarget, &PipelineId, &PostTextures, &DynamicUniformIndex<PostView>)>,
    cache: Res<PipelineCache>,
    pipeline: Res<PostPipeline>,
    uniforms: Res<ComponentUniforms<PostView>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, id, textures, index) = view.into_inner();
    let (Some(render_pipeline), Some(binding)) = (cache.get_render_pipeline(**id), uniforms.uniforms().binding()) else { return };
    let Some(t) = textures.0.iter().map(|h| images.get(h)).collect::<Option<Vec<_>>>() else { return };
    let post = target.post_process_write();
    let bind = ctx.render_device().create_bind_group(
        Some("post-effects"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((
            post.source,
            &pipeline.clamp,
            &t[0].texture_view,
            &t[1].texture_view,
            &t[2].texture_view,
            &t[3].texture_view,
            &t[4].texture_view,
            &t[5].texture_view,
            &t[4].sampler,
            binding,
        )),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("post-effects"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind, &[index.index()]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::postfx::Fade;

    fn seq(name: &str, duration: f32, looping: bool) -> Arc<Sequence> {
        Arc::new(Sequence { name: name.into(), duration, looping, ..Default::default() })
    }

    fn library() -> PostFx {
        let mut fx = PostFx::default();
        for s in [seq("postfx:in", 1.0, false), seq("postfx:loop", 2.0, true), seq("postfx:out", 0.5, false), seq("postfx:hit", 0.3, false)] {
            fx.sequences.insert(s.name.clone(), s);
        }
        let fade = |kind, time, s: Option<&str>| Fade { kind, time, sequence: s.map(str::to_string) };
        fx.effects.insert(
            "low".into(),
            Effect {
                name: "low".into(),
                id: 7,
                priority: 10.0,
                sequence: "postfx:loop".into(),
                intensity_control: 0,
                fade_in: fade(1, 0.0, Some("postfx:in")),
                fade_out: fade(1, 0.0, Some("postfx:out")),
            },
        );
        fx.effects.insert(
            "speed".into(),
            Effect {
                name: "speed".into(),
                id: 8,
                priority: 5.0,
                sequence: "postfx:loop".into(),
                intensity_control: 2,
                fade_in: fade(2, 0.5, None),
                fade_out: fade(2, 0.5, None),
            },
        );
        fx.ids.insert(7, "low".into());
        fx
    }

    #[test]
    fn effects_fade_in_loop_and_fade_out() {
        let mut fx = library();
        fx.request("7", true);
        fx.tick(0.0);
        assert_eq!(fx.playing[0].phase, Phase::In);
        fx.tick(1.2);
        assert_eq!(fx.playing[0].phase, Phase::Loop);
        fx.tick(5.0);
        assert!(fx.playing("low"));
        fx.request("LOW", false);
        fx.tick(0.1);
        assert_eq!(fx.playing[0].phase, Phase::Out);
        fx.tick(0.5);
        assert!(!fx.playing("low"));
        // Scripts hear it start and stop, by its name's id.
        let id = fc3_formats::hash::crc32(b"low");
        assert_eq!(fx.take_events(), vec![(true, id), (false, id)]);
    }

    #[test]
    fn timed_fades_ramp_and_sequences_play_alone() {
        let mut fx = library();
        fx.hold("speed", true);
        fx.tick(0.25);
        assert!((fx.playing[0].weight - 0.5).abs() < 1e-4);
        fx.hold("speed", true);
        fx.tick(1.0);
        assert_eq!(fx.playing.len(), 1);
        assert_eq!(fx.playing[0].weight, 1.0);
        fx.hold("speed", false);
        fx.tick(0.25);
        assert!(fx.playing("speed"));
        fx.tick(0.3);
        assert!(!fx.playing("speed"));
        // A sequence by its own name, once through.
        fx.start("postfx:hit");
        fx.tick(0.2);
        assert!(fx.playing("postfx:hit"));
        fx.tick(0.2);
        assert!(!fx.playing("postfx:hit"));
        // A state lets go only of what it held, not a script's.
        fx.start("low");
        fx.tick(0.0);
        fx.hold("low", false);
        fx.tick(0.1);
        assert!(fx.playing("low"));
    }

    #[test]
    fn the_strongest_clip_of_each_kind_wins() {
        let clip = |v: f32| fc3_formats::cinema::Clip {
            start: 0.0,
            duration: 1.0,
            kind: ClipKind::Post { class: PostClass::Desaturation, texture: String::new(), settings: [0.0; 4], samples: vec![vec![1.0, 1.0, 1.0, 1.0, v]] },
        };
        let s = |v| Sequence {
            name: "s".into(),
            duration: 1.0,
            groups: vec![fc3_formats::cinema::Group {
                uid: 0,
                kind: fc3_formats::cinema::GroupKind::Overlay,
                name: String::new(),
                archetype: 0,
                clips: vec![clip(v)],
            }],
            ..Default::default()
        };
        let mut mix = Mix::default();
        mix.add(&s(0.25), 0.0, 1.0, 100.0, 1, 0.0);
        mix.add(&s(0.75), 0.0, 0.5, 10.0, 2, 0.0);
        assert_eq!(mix.desat.unwrap().1.w, 0.25);
    }
}
