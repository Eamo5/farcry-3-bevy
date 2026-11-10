//! The game's blood on the screen, `UI\Singleplayer\Hud\Damage\H_Blood.feu`
//! (the engine's `CFCXHudBloodUI`; see docs/ui.md). Its splatters and its
//! vignette are drawn with Flash's multiply blend, which needs the game
//! under them: drawn into the HUD's see-through canvas they came out as
//! white squares. So it plays in a player of its own over white, at half
//! the window's size, and its canvas multiplies the scene under the HUD
//! (`blood.wgsl`, a multiplying blend): white leaves the game as it is.
//!
//! Its clips by DPS id: eight splatters round the screen (`BloodMC_<n>`,
//! from the top round to the left, 45 degrees apart), each a blood splash
//! (`HitDamage`) or an animal's claw marks (`AnimalDamage`); a hit shows
//! the one toward where it came from, which holds a moment and fades. The
//! darkening edges (`WaterEffect`, 101 frames in) close in as the breath
//! runs out under water, past the player's critical oxygen.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor};
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::ShaderRef;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::ui_render::ui_material::UiMaterialKey;
use bevy::window::PrimaryWindow;
use fc3_ui::wgpu;

const MOVIE: &str = r"UI\Singleplayer\Hud\Damage\H_Blood.feu";
const SPLATTERS: usize = 8;
/// How long (s) a splatter holds, then fades.
const HOLD: f32 = 1.0;
const FADE: f32 = 1.5;
/// The player's critical oxygen (`fCriticalOxygen`, of the most): the edges
/// close in from there.
const CRITICAL_OXYGEN: f32 = 0.4;
/// The vignette's last frame, and how fast (frames a second) it goes once
/// back in the air.
const WATER_FRAMES: f32 = 101.0;
const WATER_OUT: f32 = 120.0;

pub(super) fn build(app: &mut App) {
    bevy::asset::embedded_asset!(app, "blood.wgsl");
    app.add_plugins((ExtractResourcePlugin::<BloodCanvas>::default(), UiMaterialPlugin::<BloodMaterial>::default()))
        .insert_non_send(Layer::Waiting)
        .add_systems(Startup, make_canvas)
        .add_systems(Update, drive_blood.after(crate::hud::HudSet).run_if(resource_exists::<BloodCanvas>));
    if let Some(render) = app.get_sub_app_mut(RenderApp) {
        render.add_systems(Render, hand_back_texture.in_set(RenderSystems::PrepareResources));
    }
}

/// Multiplies the scene by the canvas (`blood.wgsl`).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
struct BloodMaterial {
    #[texture(0)]
    #[sampler(1)]
    canvas: Handle<Image>,
}

impl UiMaterial for BloodMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("blood.wgsl")).with_source("embedded").into()
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, _key: UiMaterialKey<Self>) {
        let multiply = BlendState {
            color: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::Src, operation: BlendOperation::Add },
            alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
        };
        if let Some(target) = descriptor.fragment.as_mut().and_then(|f| f.targets.first_mut()).and_then(Option::as_mut) {
            target.blend = Some(multiply);
        }
    }
}

#[derive(Resource, Clone, ExtractResource)]
struct BloodCanvas {
    image: Handle<Image>,
    texture: Arc<Mutex<Option<wgpu::Texture>>>,
}

#[derive(Component)]
struct BloodNode;

enum Layer {
    Waiting,
    Running(Box<Blood>),
    Failed,
}

struct Blood {
    host: fc3_ui::Host,
    /// When each splatter was shown (game seconds), and its alpha as sent.
    splats: [Option<(f32, u32)>; SPLATTERS],
    /// The vignette's frame now, and as sent.
    water: f32,
    water_sent: u32,
}

fn make_canvas(mut commands: Commands, mut images: ResMut<Assets<Image>>, mut materials: ResMut<Assets<BloodMaterial>>, windows: Query<&Window, With<PrimaryWindow>>) {
    let Ok(window) = windows.single() else { return };
    // (Half the window: soft splashes and edges, a quarter of the drawing.)
    let (w, h) = ((window.physical_width() / 2).max(1), (window.physical_height() / 2).max(1));
    let image = images.add(super::canvas_image(w, h));
    let material = materials.add(BloodMaterial { canvas: image.clone() });
    commands.spawn((
        MaterialNode(material),
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() },
        // (Under the HUD, over the game.)
        GlobalZIndex(49),
        Visibility::Hidden,
        Pickable::IGNORE,
        BloodNode,
    ));
    commands.insert_resource(BloodCanvas { image, texture: Arc::new(Mutex::new(None)) });
}

fn hand_back_texture(canvas: Option<Res<BloodCanvas>>, images: Res<RenderAssets<GpuImage>>) {
    let Some(canvas) = canvas else { return };
    let Ok(mut slot) = canvas.texture.lock() else { return };
    if slot.is_none() {
        if let Some(gpu) = images.get(&canvas.image) {
            *slot = Some((*gpu.texture).clone());
        }
    }
}

/// The index of the splatter toward a hit from `to` (the way to where it
/// came from, flat), the player facing `yaw`: 0 ahead, then round by the
/// left (1 ahead-left, 2 left, ... 4 behind ... 7 ahead-right).
fn splatter_toward(to: Vec3, yaw: f32) -> Option<usize> {
    let flat = Vec3::new(to.x, 0.0, to.z).normalize_or_zero();
    if flat == Vec3::ZERO {
        return None;
    }
    let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let right = Vec3::new(-fwd.z, 0.0, fwd.x);
    // (Turned to the right, positive.)
    let turn = flat.dot(right).atan2(flat.dot(fwd));
    let sector = (turn / std::f32::consts::FRAC_PI_4).round() as i32;
    Some((SPLATTERS as i32 - sector).rem_euclid(SPLATTERS as i32) as usize)
}

/// The vignette's frame for the breath left (of the most) under water.
fn water_frame(under: bool, left: f32) -> f32 {
    if !under || left >= CRITICAL_OXYGEN {
        return 1.0;
    }
    1.0 + (1.0 - left / CRITICAL_OXYGEN).clamp(0.0, 1.0) * (WATER_FRAMES - 1.0)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_blood(
    mut layer: NonSendMut<Layer>,
    canvas: Res<BloodCanvas>,
    (time, real): (Res<Time>, Res<Time<Real>>),
    info: Res<crate::data::CacheInfo>,
    instance: Option<Res<super::GpuInstance>>,
    (device, queue, adapter): (Res<RenderDevice>, Res<RenderQueue>, Res<RenderAdapter>),
    mut hits: MessageReader<crate::combat::DamageEvent>,
    players: Query<(Entity, &crate::player::Player, &GlobalTransform)>,
    animals: Query<(), With<crate::wildlife::WildAnimal>>,
    breath: Option<Res<crate::syringes::Breath>>,
    mut node: Query<&mut Visibility, With<BloodNode>>,
) {
    if let Layer::Waiting = &*layer {
        hits.clear();
        let Some(texture) = canvas.texture.lock().ok().and_then(|t| t.clone()) else { return };
        let Some(instance) = instance else {
            *layer = Layer::Failed;
            return;
        };
        let size = (texture.width(), texture.height());
        let descriptors = fc3_ui::shared_descriptors(instance.0.clone(), (**adapter.0).clone(), device.wgpu_device().clone(), (**queue.0).clone());
        let host = fc3_ui::shared_renderer(descriptors, Arc::new(Mutex::new(texture))).and_then(|r| fc3_ui::Host::new(r, &info.root, size));
        *layer = match host {
            Ok(mut host) => {
                host.answer("IsHighDefinition", "0");
                host.answer("GetLanguage", "english");
                host.answer("Platform", "PC");
                host.set_background(Some([255, 255, 255]));
                host.run_frame();
                host.load(MOVIE, 1);
                for _ in 0..3 {
                    host.run_frame();
                }
                info!("original UI: blood on the screen at {}x{}", size.0, size.1);
                Layer::Running(Box::new(Blood { host, splats: [None; SPLATTERS], water: 1.0, water_sent: 1 }))
            }
            Err(e) => {
                warn!("original UI: blood on the screen: {e:#}");
                Layer::Failed
            }
        };
        return;
    }
    let Layer::Running(blood) = &mut *layer else {
        hits.clear();
        return;
    };
    let now = time.elapsed_secs();
    let num = |n: f64| fc3_ui::ExternalValue::Number(n);
    let label = |s: &str| fc3_ui::ExternalValue::String(s.to_string());
    if let Ok((me, player, at)) = players.single() {
        for hit in hits.read().filter(|d| d.target == me && d.amount > 0.0) {
            // (Not the air running out, a fall, a fire: those come from no
            // way.)
            use crate::combat::DamageKind;
            if matches!(hit.kind, DamageKind::Drown | DamageKind::Fall | DamageKind::Fire) {
                continue;
            }
            let Some(i) = hit.source.and_then(|s| splatter_toward(s - at.translation(), player.yaw)) else { continue };
            let id = format!("BloodMC_{i}");
            let host = &blood.host;
            host.dps_visible(&id, true);
            host.dps_set(&id, "_alpha", num(100.0));
            if hit.instigator.is_some_and(|e| animals.contains(e)) {
                host.dps_play(&id, label("AnimalDamage"));
            } else {
                host.dps_frame(&id, label("HitDamage"));
            }
            blood.splats[i] = Some((now, 100));
        }
    } else {
        hits.clear();
    }
    for (i, slot) in blood.splats.iter_mut().enumerate() {
        let Some((since, sent)) = *slot else { continue };
        let id = format!("BloodMC_{i}");
        let left = 1.0 - ((now - since - HOLD) / FADE).clamp(0.0, 1.0);
        if left <= 0.0 {
            blood.host.dps_visible(&id, false);
            *slot = None;
            continue;
        }
        let alpha = (left * 100.0).round() as u32;
        if sent.abs_diff(alpha) >= 3 {
            blood.host.dps_set(&id, "_alpha", num(alpha as f64));
            *slot = Some((since, alpha));
        }
    }
    let target = breath.as_ref().map_or(1.0, |b| water_frame(b.under, b.left / b.max.max(0.01)));
    blood.water = if target >= blood.water { target } else { (blood.water - WATER_OUT * time.delta_secs()).max(target) };
    let frame = blood.water.round() as u32;
    if frame != blood.water_sent {
        blood.host.dps_frame("WaterEffect", num(frame as f64));
        blood.water_sent = frame;
    }
    // (Nothing to multiply: not drawn at all, nor the movie run: ticking it
    // cost most of a millisecond a frame.)
    let showing = blood.splats.iter().any(Option::is_some) || frame > 1;
    if let Ok(mut v) = node.single_mut() {
        let want = if showing { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if showing {
        blood.host.run(real.delta_secs_f64());
        blood.host.render_if_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hit's splatter is the one toward it, the player turned any way.
    #[test]
    fn splatters_go_toward_the_hit() {
        // (Facing -Z at yaw 0.)
        assert_eq!(splatter_toward(Vec3::NEG_Z, 0.0), Some(0));
        assert_eq!(splatter_toward(Vec3::X, 0.0), Some(6));
        assert_eq!(splatter_toward(Vec3::NEG_X, 0.0), Some(2));
        assert_eq!(splatter_toward(Vec3::Z, 0.0), Some(4));
        assert_eq!(splatter_toward(Vec3::new(1.0, 0.0, -1.0), 0.0), Some(7));
        assert_eq!(splatter_toward(Vec3::new(-1.0, 0.0, -1.0), 0.0), Some(1));
        // (Turned to face +X: a hit from +X is ahead.)
        let yaw = -std::f32::consts::FRAC_PI_2;
        assert_eq!(splatter_toward(Vec3::X, yaw), Some(0));
        assert_eq!(splatter_toward(Vec3::Y, 0.0), None);
    }

    /// The edges close in only past the critical oxygen, under water.
    #[test]
    fn the_vignette_comes_in_past_the_critical_oxygen() {
        assert_eq!(water_frame(false, 0.0), 1.0);
        assert_eq!(water_frame(true, 0.5), 1.0);
        assert_eq!(water_frame(true, 0.2), 51.0);
        assert_eq!(water_frame(true, 0.0), WATER_FRAMES);
    }
}
