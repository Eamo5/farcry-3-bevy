//! The menus' 3D item views: where a page has the engine draw the item it
//! shows (the crafting page's `3D_Crafting_MC`, the shop's
//! `3D_Weapon_MC`), a camera of its own draws the item's model into an
//! image, and the image is shown over the canvas on the stand-in clip's
//! place while the page shows that clip. The model, its turn and its sway
//! are the item's (`fc3_formats::menu_views`); it is framed to fill the
//! place, seen from the front (Dunia's +Y, Z up).

use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::window::PrimaryWindow;
use fc3_formats::menu_views::{MenuViews, View};

use super::Interface;
use crate::cache_assets::Fc3Model;

/// The layer only the views' camera and items are on.
const LAYER: usize = 6;
/// The camera's field of view (vertical), and the room left round the item.
const FOV: f32 = 0.45;
const MARGIN: f32 = 1.12;
/// The least half-size (m, across and up) an item is framed as: a grenade
/// is shown smaller than a gun, not blown up to the same size.
const MIN_HALF: f32 = 0.12;

/// The crafting page's and the shop's places for the item (the first its
/// page shows is used).
const CRAFT_PLACES: [&str; 4] = ["3D_Crafting_MC", "3D_Crafting_MC_B", "3D_Crafting_MC_C", "3D_Crafting_MC_D"];
const SHOP_PLACES: [&str; 10] = [
    "3D_Weapon_MC",
    "3D_Item_MC",
    "3D_MC",
    "3D_Weapon_MC_B",
    "3D_Weapon_MC_C",
    "3D_Weapon_MC_D",
    "Secondary3D",
    "Secondary3D_B",
    "Secondary3D_C",
    "Secondary3D_D",
];

/// The recipe the crafting page has selected (its id), as the pause menu
/// hears it.
#[derive(Resource, Default)]
pub(super) struct ItemView {
    pub(super) recipe: Option<String>,
}

/// The items' views (the cache's table).
#[derive(Resource, Default)]
pub(super) struct ViewTable(MenuViews);

pub(super) fn load_views(mut commands: Commands, info: Res<crate::data::CacheInfo>) {
    let table = std::fs::read_to_string(info.root.join(fc3_formats::menu_views::REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| MenuViews::from_text(&t))
        .unwrap_or_else(|e| {
            warn!("menu views: {e:#}");
            MenuViews::default()
        });
    commands.insert_resource(ViewTable(table));
}

#[derive(Component)]
pub(super) struct ViewNode;

#[derive(Component)]
pub(super) struct ViewItem;

/// What's set up: the image drawn into and its size, the camera, the node
/// showing it, and the item there (its view, its model, when it came).
#[derive(Default)]
pub(super) struct Stage {
    parts: Option<(Handle<Image>, UVec2, Entity, Entity)>,
    item: Option<(View, Handle<Fc3Model>, Option<Entity>, f32)>,
}

fn image(size: UVec2) -> Image {
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    image
}

/// Shows the item the page has up (the crafting page's recipe, the shop's
/// weapon) where its page has the place for it, and hides the view when
/// there's none.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn draw_views(
    state: NonSend<Interface>,
    (item_view, table, shop): (Res<ItemView>, Option<Res<ViewTable>>, Res<super::shop::ShopPage>),
    windows: Query<&Window, With<PrimaryWindow>>,
    (server, models, mut images, time, ui_scale): (Res<AssetServer>, Res<Assets<Fc3Model>>, ResMut<Assets<Image>>, Res<Time<Real>>, Res<UiScale>),
    mut nodes: Query<(&mut Node, &mut ImageNode), With<ViewNode>>,
    mut cameras: Query<(&mut Camera, &mut RenderTarget, &mut Projection), Without<ViewItem>>,
    mut items: Query<&mut Transform, With<ViewItem>>,
    mut stage: Local<Stage>,
    mut commands: Commands,
) {
    let Interface::Running(host) = &*state else { return };
    let Some(table) = table else { return };
    // FC3_MENU_VIEW_DEBUG: each place, whether its page shows it, and where.
    if std::env::var_os("FC3_MENU_VIEW_DEBUG").is_some() {
        let places: Vec<String> = CRAFT_PLACES
            .iter()
            .chain(&SHOP_PLACES)
            .chain(&["3D_CraftMirror_MC", "Loading_MC"])
            .filter_map(|p| Some(format!("{p} {} {:?}", host.dps_shown(p), host.dps_bounds(p)?.map(|v| v.round()))))
            .collect();
        debug!("menu view places: {places:?}");
    }
    // A place's bounds on the stage. The shop's whole-page place (`3D_MC`,
    // where its items are drawn) is taken as its guns' (the left half).
    let bounds = |p: &str| match p {
        "3D_MC" => host.dps_bounds("3D_Weapon_MC").or_else(|| host.dps_bounds(p).map(|[x0, y0, x1, y1]| [x0, y0, (x0 + x1) / 2.0, y1])),
        _ => host.dps_bounds(p),
    };
    // What's up, and where.
    let recipe = item_view.recipe.as_ref().and_then(|r| table.0.recipes.get(r)).map(|v| (&CRAFT_PLACES[..], v));
    let shown = shop.shown_3d().and_then(|s| match s {
        super::shop::Shown3d::Item(id) => table.0.shop.get(&id),
        super::shop::Shown3d::Category(sub) => table.0.categories.get(&sub),
    });
    let weapon = shown.map(|v| (&SHOP_PLACES[..], v));
    // FC3_MENU_VIEW=shop:<id> or recipe:<id> (tests): that item in
    // whatever place is up.
    let test = std::env::var("FC3_MENU_VIEW").ok().and_then(|t| {
        let (kind, key) = t.split_once(':')?;
        let v = match kind {
            "shop" => table.0.shop.get(&key.parse().ok()?)?,
            _ => table.0.recipes.get(key)?,
        };
        Some([(&CRAFT_PLACES[..], v), (&SHOP_PLACES[..], v)])
    });
    let place = test
        .into_iter()
        .flatten()
        .map(Some)
        .chain([recipe, weapon])
        .flatten()
        .find_map(|(places, v)| places.iter().filter(|p| host.dps_shown(p)).find_map(|p| bounds(p)).map(|b| (b, v.clone())))
        .filter(|([x0, y0, x1, y1], _)| x1 - x0 > 4.0 && y1 - y0 > 4.0);
    let Ok(window) = windows.single() else { return };
    let Some(([x0, y0, x1, y1], view)) = place else {
        if let Some((_, _, camera, node)) = stage.parts {
            if let Ok((mut c, _, _)) = cameras.get_mut(camera) {
                c.is_active = false;
            }
            if let Ok((mut n, _)) = nodes.get_mut(node) {
                n.display = Display::None;
            }
        }
        return;
    };
    // The place on the window (the stage is shown whole, centred).
    let (sw, sh) = (fc3_ui::STAGE.0 as f32, fc3_ui::STAGE.1 as f32);
    let scale = (window.width() / sw).min(window.height() / sh);
    let offset = Vec2::new(window.width() - sw * scale, window.height() - sh * scale) / 2.0;
    let (from, to) = (offset + Vec2::new(x0 as f32, y0 as f32) * scale, offset + Vec2::new(x1 as f32, y1 as f32) * scale);
    let size = ((to - from) * window.scale_factor()).as_uvec2().max(UVec2::splat(16));
    // The image, camera and node, made once (the image again for a new size).
    let (camera, node) = match stage.parts.clone() {
        Some((_, at, camera, node)) if at == size => (camera, node),
        parts => {
            let handle = images.add(image(size));
            match parts {
                Some((_, _, camera, node)) => {
                    if let Ok((_, mut target, _)) = cameras.get_mut(camera) {
                        *target = RenderTarget::from(handle.clone());
                    }
                    if let Ok((_, mut i)) = nodes.get_mut(node) {
                        i.image = handle.clone();
                    }
                    stage.parts = Some((handle, size, camera, node));
                    (camera, node)
                }
                None => {
                    let camera = commands
                        .spawn((
                            Camera3d::default(),
                            Camera { order: -1, clear_color: ClearColorConfig::Custom(Color::NONE), ..default() },
                            RenderTarget::from(handle.clone()),
                            Projection::Perspective(PerspectiveProjection { fov: FOV, near: 0.01, far: 50.0, ..default() }),
                            AmbientLight { color: Color::WHITE, brightness: 400.0, ..default() },
                            Msaa::Sample4,
                            RenderLayers::layer(LAYER),
                            Transform::IDENTITY,
                            Name::new("menu item view"),
                        ))
                        .id();
                    // A key light from above and to the left, a fill from the right.
                    for (illuminance, from) in [(3500.0, Vec3::new(-1.0, 1.5, 1.2)), (1200.0, Vec3::new(1.5, 0.3, 0.8))] {
                        commands.spawn((
                            DirectionalLight { illuminance, shadow_maps_enabled: false, ..default() },
                            Transform::from_translation(from).looking_at(Vec3::ZERO, Vec3::Y),
                            RenderLayers::layer(LAYER),
                        ));
                    }
                    let node = commands
                        .spawn((
                            ViewNode,
                            ImageNode::new(handle.clone()),
                            Node { position_type: PositionType::Absolute, ..default() },
                            // (Over the canvas.)
                            GlobalZIndex(51),
                            Pickable::IGNORE,
                        ))
                        .id();
                    stage.parts = Some((handle, size, camera, node));
                    (camera, node)
                }
            }
        }
    };
    if let Ok((mut c, _, mut projection)) = cameras.get_mut(camera) {
        c.is_active = true;
        if let Projection::Perspective(p) = &mut *projection {
            p.aspect_ratio = size.x as f32 / size.y as f32;
        }
    }
    if let Ok((mut n, _)) = nodes.get_mut(node) {
        let (a, b) = (crate::ui_nav::viewport_to_ui(from, ui_scale.0), crate::ui_nav::viewport_to_ui(to, ui_scale.0));
        n.display = Display::Flex;
        (n.left, n.top, n.width, n.height) = (Val::Px(a.x), Val::Px(a.y), Val::Px(b.x - a.x), Val::Px(b.y - a.y));
    }
    // The item: a new one in place of the last.
    if stage.item.as_ref().is_none_or(|(v, ..)| v.model != view.model) {
        if let Some((_, _, Some(e), _)) = stage.item.take() {
            commands.entity(e).despawn();
        }
        stage.item = Some((view.clone(), crate::weapons::model_handle(&server, &view.model), None, time.elapsed_secs()));
    }
    let Some((view, handle, spawned, since)) = &mut stage.item else { return };
    let Some(model) = models.get(&*handle) else { return };
    let e = *spawned.get_or_insert_with(|| {
        debug!("menu view: {} in {size}", view.model);
        let root = commands.spawn((ViewItem, Transform::default(), Visibility::Visible)).id();
        for part in model.lods.first().map(|l| l.parts.as_slice()).unwrap_or_default() {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), RenderLayers::layer(LAYER), NotShadowCaster, ChildOf(root)));
        }
        root
    });
    // Turned as the item says, swaying about the upright, framed to fill
    // the place.
    let (centre, half) = (model.center(), (model.aabb_max - model.aabb_min) * 0.5);
    let mut turn = Quat::from_array(fc3_formats::world::angles_to_bevy_quat(view.angles));
    // (One with no turn of its own that's long front to back, a syringe, is
    // turned to lie across.)
    if view.angles == [0.0; 3] && half.z > half.x * 1.5 {
        turn = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    }
    let t = time.elapsed_secs() - *since;
    let sway = view.amplitude * std::f32::consts::TAU * (std::f32::consts::TAU * view.speed * t).sin();
    let rotation = Quat::from_rotation_y(sway) * turn;
    // (The turned box's extent on screen and in depth; framed as it rests,
    // so it doesn't come and go as it sways.)
    let corners = (0..8).map(|i| {
        turn * Vec3::new(if i & 1 == 0 { -half.x } else { half.x }, if i & 2 == 0 { -half.y } else { half.y }, if i & 4 == 0 { -half.z } else { half.z })
    });
    let extent = corners.fold(Vec3::ZERO, |m, c| m.max(c.abs())).max(Vec3::new(MIN_HALF, MIN_HALF, 0.0));
    let aspect = size.x as f32 / size.y as f32;
    let tan = (FOV * 0.5).tan();
    let distance = (extent.y / tan).max(extent.x / (tan * aspect)) * MARGIN + extent.z;
    if let Ok(mut tf) = items.get_mut(e) {
        *tf = Transform { translation: Vec3::new(0.0, 0.0, -distance.max(0.05)) - rotation * centre, rotation, scale: Vec3::ONE };
    }
}
