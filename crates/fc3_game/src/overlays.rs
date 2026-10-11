//! The game's own screen overlays (`graphics\postfx\`, converted with the
//! textures): the sniper scope's mask round the view (the high-power and
//! illuminated scopes' sharper one), blood on the screen with hits and low
//! health, an animal's claws across it when one strikes, and the dark closing
//! in as the air runs out underwater. Each is a full-screen UI node drawn
//! with `overlay.wgsl`.

use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use fc3_formats::cache;

use crate::cache_assets::cache_path;
use crate::combat::{DamageEvent, Health, PlayerStats, HEALTH_SEGMENT};
use crate::player::Player;
use crate::weapons::{Attachment, Inventory};

pub struct OverlaysPlugin;

impl Plugin for OverlaysPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "overlay.wgsl");
        app.add_plugins(UiMaterialPlugin::<OverlayMaterial>::default())
            .init_resource::<Slash>()
            .add_systems(Startup, spawn_overlays)
            .add_systems(Update, (note_slashes, drive_overlays).chain());
    }
}

#[derive(Clone, Copy, ShaderType, Debug, Default)]
pub struct OverlayParams {
    color: Vec4,
    mode: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct OverlayMaterial {
    #[uniform(0)]
    params: OverlayParams,
    #[texture(1)]
    #[sampler(2)]
    image: Handle<Image>,
}

impl UiMaterial for OverlayMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("overlay.wgsl")).with_source("embedded").into()
    }
}

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
enum Overlay {
    Scope,
    ScopeHigh,
    Blood,
    SlashLeft,
    SlashRight,
    Drowning,
}

impl Overlay {
    const ALL: [Overlay; 6] = [Overlay::Scope, Overlay::ScopeHigh, Overlay::Blood, Overlay::SlashLeft, Overlay::SlashRight, Overlay::Drowning];

    /// Its texture under `graphics\postfx\`, and whether its brightness is
    /// its cover.
    fn texture(self) -> (&'static str, bool) {
        match self {
            Overlay::Scope => ("sniperscope", true),
            Overlay::ScopeHigh => ("highpowered_sniperscope", true),
            Overlay::Blood => ("screen_blood", false),
            Overlay::SlashLeft => ("animalslash_leftside02", false),
            Overlay::SlashRight => ("animalslash_rightside02", false),
            Overlay::Drowning => ("drowningvignette", false),
        }
    }
}

/// An animal's last strike on the player: which side, and seconds left.
#[derive(Resource, Default)]
struct Slash {
    right: bool,
    left_for: f32,
}

/// How long claw marks stay on the screen (s).
const SLASH_TIME: f32 = 1.1;

fn spawn_overlays(mut commands: Commands, server: Res<AssetServer>, mut materials: ResMut<Assets<OverlayMaterial>>) {
    for o in Overlay::ALL {
        let (name, mask) = o.texture();
        let image = server.load(cache_path(&cache::texture_rel(&format!("{}{name}.xbt", cache::SCREEN_OVERLAYS))));
        let material = materials.add(OverlayMaterial {
            params: OverlayParams { color: Vec4::new(0.0, 0.0, 0.0, 0.0), mode: Vec4::new(if mask { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0) },
            image,
        });
        commands.spawn((
            Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
            MaterialNode(material),
            Visibility::Hidden,
            // (Under the HUD; the scope's reticle draws over its mask.)
            GlobalZIndex(0),
            o,
            Name::new(format!("overlay {o:?}")),
        ));
    }
}

/// An animal's bite or claw on the player marks the screen.
fn note_slashes(
    time: Res<Time>,
    mut hits: MessageReader<DamageEvent>,
    players: Query<Entity, With<Player>>,
    animals: Query<(), With<crate::wildlife::WildAnimal>>,
    mut slash: ResMut<Slash>,
) {
    slash.left_for = (slash.left_for - time.delta_secs()).max(0.0);
    let Ok(pe) = players.single() else {
        hits.clear();
        return;
    };
    for h in hits.read() {
        if h.target == pe && h.amount > 0.0 && h.instigator.is_some_and(|i| animals.contains(i)) {
            slash.right = !slash.right;
            slash.left_for = SLASH_TIME;
        }
    }
}

fn drive_overlays(
    state: Res<State<crate::menu::GameState>>,
    players: Query<(&Inventory, &Health, &PlayerStats, &Player)>,
    breath: Option<Res<crate::syringes::Breath>>,
    slash: Res<Slash>,
    mut nodes: Query<(&Overlay, &MaterialNode<OverlayMaterial>, &mut Visibility)>,
    mut materials: ResMut<Assets<OverlayMaterial>>,
) {
    let Ok((inv, h, stats, p)) = players.single() else { return };
    let playing = *state.get() == crate::menu::GameState::Playing;
    let scoped = inv.scoped();
    let def = inv.slot().def;
    let high = inv.has_attachment(def, Attachment::Scope) || inv.has_attachment(def, Attachment::IlluminatedScope);
    // Blood: with each hit, and lasting as health runs low.
    let low = (1.0 - h.current / (HEALTH_SEGMENT * 1.2)).clamp(0.0, 1.0);
    let pulse = stats.hurt_dir.map_or(0.0, |(_, s)| s);
    let blood = if h.dead { 0.0 } else { (low * 0.5 + pulse * 0.45).min(0.75) };
    let air = breath.as_ref().filter(|_| p.swimming).map_or(1.0, |b| b.left / b.max.max(0.1));
    let slashed = slash.left_for / SLASH_TIME;
    for (o, m, mut vis) in &mut nodes {
        let (color, alpha) = match o {
            Overlay::Scope => (Vec3::ZERO, if scoped && !high { 1.0 } else { 0.0 }),
            Overlay::ScopeHigh => (Vec3::ZERO, if scoped && high { 1.0 } else { 0.0 }),
            Overlay::Blood => (Vec3::ONE, blood),
            Overlay::SlashLeft => (Vec3::new(0.8, 0.08, 0.06), if slash.right { 0.0 } else { slashed }),
            Overlay::SlashRight => (Vec3::new(0.8, 0.08, 0.06), if slash.right { slashed } else { 0.0 }),
            // (From half the air gone, darker as it runs out.)
            Overlay::Drowning => (Vec3::ZERO, ((0.5 - air) / 0.5).clamp(0.0, 1.0)),
        };
        let alpha = if playing { alpha } else { 0.0 };
        let want = if alpha > 0.005 { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
        if alpha > 0.005 {
            if let Some(mut mat) = materials.get_mut(&m.0) {
                mat.params.color = color.extend(alpha);
            }
        }
    }
}
