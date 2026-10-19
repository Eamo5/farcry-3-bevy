//! The sniper scope's mask round the view (`graphics\postfx\`, converted
//! with the textures; the high-power and illuminated scopes' sharper one),
//! a full-screen UI node drawn with `overlay.wgsl` under the HUD. The other
//! screen effects (hits, an animal's claws, low health, running out of air)
//! are the game's own post-effects (`post_effects`).

use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use fc3_formats::cache;

use crate::cache_assets::cache_path;
use crate::weapons::{Attachment, Inventory};

pub struct OverlaysPlugin;

impl Plugin for OverlaysPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "overlay.wgsl");
        app.add_plugins(UiMaterialPlugin::<OverlayMaterial>::default()).add_systems(Startup, spawn_overlays).add_systems(Update, drive_overlays);
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
}

impl Overlay {
    const ALL: [Overlay; 2] = [Overlay::Scope, Overlay::ScopeHigh];

    /// Its texture under `graphics\postfx\`, and whether its brightness is
    /// its cover.
    fn texture(self) -> (&'static str, bool) {
        match self {
            Overlay::Scope => ("sniperscope", true),
            Overlay::ScopeHigh => ("highpowered_sniperscope", true),
        }
    }
}

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

fn drive_overlays(
    state: Res<State<crate::menu::GameState>>,
    players: Query<&Inventory, With<crate::player::Player>>,
    mut nodes: Query<(&Overlay, &MaterialNode<OverlayMaterial>, &mut Visibility)>,
    mut materials: ResMut<Assets<OverlayMaterial>>,
) {
    let Ok(inv) = players.single() else { return };
    let playing = *state.get() == crate::menu::GameState::Playing;
    let scoped = inv.scoped();
    let def = inv.slot().def;
    let high = inv.has_attachment(def, Attachment::Scope) || inv.has_attachment(def, Attachment::IlluminatedScope);
    for (o, m, mut vis) in &mut nodes {
        let alpha = match o {
            Overlay::Scope => scoped && !high,
            Overlay::ScopeHigh => scoped && high,
        };
        let alpha = if playing && alpha { 1.0 } else { 0.0 };
        let want = if alpha > 0.005 { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
        if alpha > 0.005 {
            if let Some(mut mat) = materials.get_mut(&m.0) {
                mat.params.color = Vec3::ZERO.extend(alpha);
            }
        }
    }
}
