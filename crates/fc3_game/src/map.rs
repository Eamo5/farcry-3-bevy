//! World map and minimap.
//!
//! Both draw the island map baked by the converter (`map/world.dds`, see
//! `fc3_formats::worldmap`) with a UI material (`map.wgsl`): the minimap turns
//! with the player; the full map (M) pauses the game and can be zoomed with
//! the wheel, dragged, and right-clicked to set a waypoint. Ground the player
//! hasn't been near is shown dimmed; what has been explored is saved.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::input::gamepad::{Gamepad, GamepadButton};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::window::PrimaryWindow;
use fc3_formats::terrain::{SectorCoord, SECTOR_SIZE, WORLD_SECTORS};
use fc3_formats::worldmap::{self, MAP_SIZE};

use crate::data::CacheInfo;
use crate::menu::GameState;
use crate::player::Player;

/// Ground within this distance of the player counts as explored.
const REVEAL_RADIUS: f32 = 320.0;
/// Minimap diameter (pixels) and the ground radius it shows (metres).
const MINIMAP_SIZE: f32 = 230.0;
const MINIMAP_RADIUS_M: f32 = 160.0;

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "map.wgsl");
        app.add_plugins(UiMaterialPlugin::<MapMaterial>::default())
            .init_resource::<Explored>()
            .init_resource::<Waypoint>()
            .init_resource::<FullMapView>()
            .init_resource::<MapPad>()
            .init_resource::<Gps>()
            .add_systems(Startup, setup_fog)
            .add_systems(
                Update,
                (
                    setup_map_ui.run_if(resource_exists::<CacheInfo>),
                    toggle_map,
                    explore,
                    plan_route,
                    update_minimap,
                    update_full_map,
                    update_minimap_icons,
                    full_map_icons,
                )
                    .chain(),
            );
    }
}

/// Map UV (0..1, north up) of a Bevy-space position.
pub fn map_uv(p: Vec3) -> Vec2 {
    let (x, y) = worldmap::to_map(p.x, -p.z);
    Vec2::new(x, y) / MAP_SIZE as f32
}

/// Bevy-space ground position (y = 0) at a map UV.
fn world_at(uv: Vec2) -> Vec3 {
    let (x, y) = worldmap::from_map(uv.x * MAP_SIZE as f32, uv.y * MAP_SIZE as f32);
    Vec3::new(x, 0.0, -y)
}

/// Player heading in radians clockwise from north.
fn heading(p: &Player) -> f32 {
    -p.yaw
}

/// Route points the maps' shader takes (two to a vec4).
const ROUTE_POINTS: usize = 64;

#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct MapParams {
    pub center: Vec2,
    pub half_extent: Vec2,
    pub player: Vec2,
    pub heading: f32,
    pub rotation: f32,
    pub waypoint: Vec4,
    /// The GPS route (map UV), two points to each; x of `route_len`: how many.
    pub route: [Vec4; ROUTE_POINTS / 2],
    pub route_len: Vec4,
}

/// The GPS: the way to the waypoint along the roads (Bevy x, z), from near
/// the player, replanned when the waypoint moves or the player strays.
#[derive(Resource, Default)]
pub struct Gps {
    route: Vec<Vec2>,
    planned_for: Option<Vec3>,
}

/// The GPS looks for a road this near the player and the waypoint; off the
/// route by more than this, it plans again.
const GPS_REACH: f32 = 400.0;
const GPS_STRAY: f32 = 45.0;

impl Gps {
    /// The route as the shader takes it: from the point nearest the player,
    /// as far as `reach` metres along (all of it for the full map), thinned
    /// to fit.
    fn packed(&self, player: Vec3, reach: f32) -> MapParams {
        let mut out = MapParams::default();
        if self.route.len() < 2 {
            return out;
        }
        let me = Vec2::new(player.x, player.z);
        let start = (0..self.route.len()).min_by(|&a, &b| self.route[a].distance(me).total_cmp(&self.route[b].distance(me))).unwrap_or(0);
        let mut pts = vec![me];
        let mut along = 0.0;
        for p in &self.route[start..] {
            along += pts.last().map_or(0.0, |q: &Vec2| q.distance(*p));
            pts.push(*p);
            if along > reach {
                break;
            }
        }
        // Thinned evenly to fit.
        let n = pts.len();
        let keep: Vec<Vec2> = if n <= ROUTE_POINTS { pts } else { (0..ROUTE_POINTS).map(|i| pts[i * (n - 1) / (ROUTE_POINTS - 1)]).collect() };
        for (i, p) in keep.iter().enumerate() {
            let uv = map_uv(Vec3::new(p.x, 0.0, p.y));
            let v = &mut out.route[i / 2];
            if i % 2 == 0 {
                v.x = uv.x;
                v.y = uv.y;
            } else {
                v.z = uv.x;
                v.w = uv.y;
            }
        }
        out.route_len = Vec4::new(keep.len() as f32, 0.0, 0.0, 0.0);
        out
    }
}

/// Plans the GPS route when the waypoint changes or the player leaves it:
/// along the roads from the one nearest the player to the one nearest the
/// waypoint, then straight on to it.
fn plan_route(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    waypoint: Res<Waypoint>,
    graph: Option<Res<crate::splines::RoadGraph>>,
    players: Query<&Transform, With<Player>>,
    mut gps: ResMut<Gps>,
) {
    // FC3_WAYPOINT=x,y (Dunia; tests): a waypoint there from the start.
    let test = std::env::var("FC3_WAYPOINT").ok().and_then(|v| {
        let f: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        (f.len() == 2).then(|| Vec3::new(f[0], 0.0, -f[1]))
    });
    let Some(w) = waypoint.0.or(test) else {
        gps.route.clear();
        gps.planned_for = None;
        return;
    };
    let Ok(pt) = players.single() else { return };
    let me = Vec2::new(pt.translation.x, pt.translation.z);
    let changed = gps.planned_for != Some(w);
    if !changed && time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 1.0;
    let strayed = gps.route.iter().map(|p| p.distance(me)).fold(f32::MAX, f32::min) > GPS_STRAY;
    if !changed && !strayed {
        return;
    }
    gps.planned_for = Some(w);
    let goal = Vec2::new(w.x, w.z);
    let Some(graph) = graph else {
        gps.route = vec![me, goal];
        return;
    };
    // Roads only when they save going cross-country.
    let path = match (graph.nearest(me, GPS_REACH), graph.nearest(goal, GPS_REACH)) {
        (Some(a), Some(b)) if me.distance(goal) > 150.0 => graph.route(a, b),
        _ => None,
    };
    let ends = (graph.nearest(me, GPS_REACH), graph.nearest(goal, GPS_REACH));
    let summary = path.as_ref().map_or(format!("no road route (road points {ends:?}, {} in the network)", graph.points.len()), |p| {
        let length: f32 = p.windows(2).map(|w| w[0].distance(w[1])).sum();
        let jumps = p.windows(2).filter(|w| w[0].distance(w[1]) > 20.0).count();
        format!("{} points, {length:.0} m along the roads ({:.0} m straight), {jumps} gaps jumped", p.len(), me.distance(goal))
    });
    if std::env::var("FC3_GPS_DEBUG").is_ok() {
        info!("gps: route {summary}");
    } else {
        debug!("gps: route {summary}");
    }
    gps.route = match path {
        Some(mut p) => {
            p.push(goal);
            p
        }
        None => vec![me, goal],
    };
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct MapMaterial {
    #[uniform(0)]
    pub params: MapParams,
    #[texture(1)]
    #[sampler(2)]
    pub map: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub fog: Handle<Image>,
}

impl UiMaterial for MapMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("map.wgsl")).with_source("embedded").into()
    }
}

/// Sectors the player has been near (one byte per sector, 0 or 255), shown
/// at full colour on the maps.
#[derive(Resource)]
pub struct Explored {
    pub cells: Vec<u8>,
    image: Handle<Image>,
    dirty: bool,
}

impl Default for Explored {
    fn default() -> Self {
        Explored { cells: vec![0; (WORLD_SECTORS * WORLD_SECTORS) as usize], image: Handle::default(), dirty: true }
    }
}

impl Explored {
    /// Marks every sector within `radius` of a point as explored.
    pub fn reveal(&mut self, centre: Vec3, radius: f32) {
        let (x, y) = (centre.x, -centre.z);
        let reach = (radius / SECTOR_SIZE).ceil() as i32 + 1;
        let here = SectorCoord::containing(x, y);
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let c = SectorCoord { x: here.x + dx, y: here.y + dy };
                if !c.valid() {
                    continue;
                }
                let [ox, oy] = c.origin();
                let mid = Vec2::new(ox + SECTOR_SIZE / 2.0, oy + SECTOR_SIZE / 2.0);
                if mid.distance(Vec2::new(x, y)) <= radius && self.cells[c.id() as usize] == 0 {
                    self.cells[c.id() as usize] = 255;
                    self.dirty = true;
                }
            }
        }
    }

    /// Packed as one bit per sector, for saving.
    pub fn to_bits(&self) -> Vec<u8> {
        self.cells.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |b, (i, v)| b | (u8::from(*v > 0) << i))).collect()
    }

    pub fn set_bits(&mut self, bits: &[u8]) {
        for (i, cell) in self.cells.iter_mut().enumerate() {
            *cell = if bits.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1) { 255 } else { 0 };
        }
        self.dirty = true;
    }
}

/// The player's map marker, if any (Bevy-space ground position).
#[derive(Resource, Default)]
pub struct Waypoint(pub Option<Vec3>);

/// Full map pan and zoom (zoom = map UV half-height shown).
#[derive(Resource)]
struct FullMapView {
    center: Vec2,
    zoom: f32,
}

impl Default for FullMapView {
    fn default() -> Self {
        FullMapView { center: Vec2::splat(0.5), zoom: 0.1 }
    }
}

#[derive(Component)]
struct Minimap;

#[derive(Component)]
struct FullMap;

#[derive(Component)]
struct FullMapHint;

/// A dot on the minimap for a nearby enemy.
#[derive(Component)]
struct MinimapDot;

/// An icon on the full map.
#[derive(Component)]
struct FullMapIcon;

fn setup_fog(mut images: ResMut<Assets<Image>>, mut explored: ResMut<Explored>) {
    let n = WORLD_SECTORS as u32;
    let mut image = Image::new_fill(
        Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0],
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    explored.image = images.add(image);
}

fn setup_map_ui(
    mut done: Local<bool>,
    server: Res<AssetServer>,
    explored: Res<Explored>,
    mut materials: ResMut<Assets<MapMaterial>>,
    mut commands: Commands,
) {
    if *done {
        return;
    }
    *done = true;
    let map: Handle<Image> = server.load_builder().with_settings(|s: &mut ImageLoaderSettings| {
        s.is_srgb = true;
        s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::ClampToEdge,
            address_mode_v: ImageAddressMode::ClampToEdge,
            mag_filter: ImageFilterMode::Linear,
            min_filter: ImageFilterMode::Linear,
            mipmap_filter: ImageFilterMode::Linear,
            anisotropy_clamp: 4,
            ..default()
        });
        s.asset_usage = RenderAssetUsages::RENDER_WORLD;
    })
    .load(crate::cache_assets::cache_path(fc3_formats::cache::WORLD_MAP_REL));
    let material = |params: MapParams| MapMaterial { params, map: map.clone(), fog: explored.image.clone() };
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(30.0),
            bottom: Val::Px(84.0),
            width: Val::Px(MINIMAP_SIZE),
            height: Val::Px(MINIMAP_SIZE),
            ..default()
        },
        MaterialNode(materials.add(material(MapParams::default()))),
        Visibility::Hidden,
        GlobalZIndex(1),
        Minimap,
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
        MaterialNode(materials.add(material(MapParams { waypoint: Vec4::new(0.0, 0.0, 0.0, 1.0), ..default() }))),
        Visibility::Hidden,
        GlobalZIndex(5),
        FullMap,
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, bottom: Val::Px(18.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        Visibility::Hidden,
        GlobalZIndex(6),
        FullMapHint,
        children![(
            Text::new("Wheel  zoom      Drag  pan      Right click  set / clear waypoint      C  centre on player      M / Esc  close\nGamepad: left stick  pan      LT / RT  zoom      X  waypoint      A  fast travel      Y  centre      B  close"),
            TextFont { font_size: bevy::text::FontSize::Px(17.0), ..default() },
            TextColor(Color::srgba(0.95, 0.95, 0.9, 0.9)),
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        )],
    ));
}

fn toggle_map(
    keys: Res<ButtonInput<KeyCode>>,
    actions: Res<crate::controls::ActionState>,
    gamepads: Query<&Gamepad>,
    args: Res<crate::Args>,
    mut opened: Local<bool>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    players: Query<(&Transform, &Player)>,
    mut view: ResMut<FullMapView>,
    settings: Res<crate::settings::Settings>,
) {
    // --open-map (tests): open once the player has landed.
    let test_open = args.open_map && !*opened && players.single().is_ok_and(|(_, p)| !p.spawning);
    if test_open {
        *opened = true;
    }
    // The map key works in both directions (in the map the game doesn't
    // capture input, so check the binding and the gamepad directly).
    let map_key = match settings.binding(crate::controls::Action::Map) {
        Some(crate::controls::Binding::Key(k)) => keys.just_pressed(k),
        _ => false,
    };
    let pad = gamepads.iter().any(|g| g.just_pressed(GamepadButton::Select));
    if !(actions.just_pressed(crate::controls::Action::Map) || map_key || pad || test_open) {
        return;
    }
    match state.get() {
        GameState::Playing => {
            if let Ok((t, _)) = players.single() {
                view.center = map_uv(t.translation);
            }
            next.set(GameState::Map);
        }
        GameState::Map => next.set(GameState::Playing),
        _ => {}
    }
}

fn explore(
    time: Res<Time<Real>>,
    mut timer: Local<f32>,
    players: Query<(&Transform, &Player)>,
    mut explored: ResMut<Explored>,
    mut images: ResMut<Assets<Image>>,
) {
    *timer -= time.delta_secs();
    if *timer <= 0.0 {
        *timer = 0.5;
        if let Ok((t, p)) = players.single() {
            if !p.spawning {
                explored.reveal(t.translation, REVEAL_RADIUS);
            }
        }
    }
    if !explored.dirty {
        return;
    }
    let handle = explored.image.clone();
    let Some(mut image) = images.get_mut(&handle) else { return };
    // Image rows run north to south; sector rows run south to north.
    let n = WORLD_SECTORS as usize;
    let mut data = vec![0u8; n * n];
    for sy in 0..n {
        for sx in 0..n {
            data[(n - 1 - sy) * n + sx] = explored.cells[sy * n + sx];
        }
    }
    image.data = Some(data);
    explored.dirty = false;
}

#[allow(clippy::too_many_arguments)]
fn update_minimap(
    state: Res<State<GameState>>,
    waypoint: Res<Waypoint>,
    gps: Res<Gps>,
    players: Query<(&Transform, &Player, &crate::combat::Health)>,
    mut minimap: Query<(&MaterialNode<MapMaterial>, &mut Visibility), With<Minimap>>,
    mut materials: ResMut<Assets<MapMaterial>>,
) {
    let Ok((node, mut vis)) = minimap.single_mut() else { return };
    let Ok((t, p, h)) = players.single() else { return };
    let show = *state.get() == GameState::Playing && !h.dead;
    *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
    if !show {
        return;
    }
    let Some(mut m) = materials.get_mut(&node.0) else { return };
    let player = map_uv(t.translation);
    let half = MINIMAP_RADIUS_M / worldmap::METRES_PER_PIXEL / MAP_SIZE as f32;
    m.params = MapParams {
        center: player,
        half_extent: Vec2::splat(half),
        player,
        heading: heading(p),
        rotation: heading(p),
        waypoint: waypoint.0.map(|w| map_uv(w).extend(1.0).extend(0.0)).unwrap_or(Vec4::ZERO),
        ..gps.packed(t.translation, MINIMAP_RADIUS_M * 1.6)
    };
}

#[allow(clippy::too_many_arguments)]
/// Gamepad use on the full map: the centre of the view acts as the cursor.
#[derive(Resource, Default)]
pub struct MapPad(pub bool);

#[allow(clippy::too_many_arguments)]
fn update_full_map(
    time: Res<Time<Real>>,
    gamepads: Query<&Gamepad>,
    mut map_pad: ResMut<MapPad>,
    state: Res<State<GameState>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut view: ResMut<FullMapView>,
    (mut waypoint, gps): (ResMut<Waypoint>, Res<Gps>),
    players: Query<(&Transform, &Player)>,
    mut full: Query<(&MaterialNode<MapMaterial>, &mut Visibility, &ComputedNode), (With<FullMap>, Without<FullMapHint>)>,
    mut hint: Query<&mut Visibility, (With<FullMapHint>, Without<FullMap>)>,
    mut materials: ResMut<Assets<MapMaterial>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let open = *state.get() == GameState::Map;
    let Ok((node, mut vis, computed)) = full.single_mut() else { return };
    *vis = if open { Visibility::Inherited } else { Visibility::Hidden };
    if let Ok(mut v) = hint.single_mut() {
        *v = *vis;
    }
    if !open {
        return;
    }
    let Ok((t, p)) = players.single() else { return };
    let size = computed.size() * computed.inverse_scale_factor();
    let aspect = size.x / size.y.max(1.0);
    // Gamepad: left stick pans, the triggers zoom, Y recentres, X sets a waypoint.
    let dt = time.delta_secs();
    let stick = gamepads.iter().map(|g| g.left_stick()).find(|s| s.length() > 0.15).unwrap_or(Vec2::ZERO);
    let zoom_in = gamepads.iter().map(|g| g.get(GamepadButton::RightTrigger2).unwrap_or(0.0)).fold(0.0f32, f32::max);
    let zoom_out = gamepads.iter().map(|g| g.get(GamepadButton::LeftTrigger2).unwrap_or(0.0)).fold(0.0f32, f32::max);
    let pad = |b: GamepadButton| gamepads.iter().any(|g| g.just_pressed(b));
    if stick != Vec2::ZERO || zoom_in > 0.1 || zoom_out > 0.1 || gamepads.iter().any(|g| g.get_just_pressed().next().is_some()) {
        map_pad.0 = true;
    } else if motion.delta.length_squared() > 4.0 || scroll.delta.y != 0.0 {
        map_pad.0 = false;
    }
    if map_pad.0 {
        let pan = Vec2::new(stick.x, -stick.y) * view.zoom * 1.4 * dt;
        view.center += pan;
        view.zoom = (view.zoom * (1.0 + (zoom_out - zoom_in) * 1.5 * dt)).clamp(0.008, 0.6);
        if pad(GamepadButton::North) {
            view.center = map_uv(t.translation);
        }
    }
    // Zoom about the cursor (the centre of the view on a gamepad).
    let cursor = if map_pad.0 { Some(size * 0.5) } else { windows.single().ok().and_then(|w| w.cursor_position()) };
    let uv_at = |view: &FullMapView, c: Vec2| {
        let local = (c / size) * 2.0 - Vec2::ONE;
        view.center + local * Vec2::new(view.zoom * aspect, view.zoom)
    };
    if scroll.delta.y != 0.0 {
        let before = cursor.map(|c| uv_at(&view, c));
        view.zoom = (view.zoom * 0.85f32.powf(scroll.delta.y.signum())).clamp(0.008, 0.6);
        if let (Some(c), Some(b)) = (cursor, before) {
            let after = uv_at(&view, c);
            view.center += b - after;
        }
    }
    if mouse.pressed(MouseButton::Left) && motion.delta != Vec2::ZERO {
        let per_px = 2.0 * Vec2::new(view.zoom * aspect, view.zoom) / size;
        view.center -= motion.delta * per_px;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        view.center = map_uv(t.translation);
    }
    view.center = view.center.clamp(Vec2::splat(0.0), Vec2::splat(1.0));
    if mouse.just_pressed(MouseButton::Right) || (map_pad.0 && pad(GamepadButton::West)) {
        if let Some(c) = cursor {
            let uv = uv_at(&view, c);
            let target = world_at(uv);
            // Clicking near the current waypoint clears it.
            let clear = waypoint.0.is_some_and(|w| (map_uv(w) - uv).length() < view.zoom * 0.03);
            waypoint.0 = if clear { None } else { Some(target) };
            sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
        }
    }
    let Some(mut m) = materials.get_mut(&node.0) else { return };
    m.params = MapParams {
        center: view.center,
        half_extent: Vec2::new(view.zoom * aspect, view.zoom),
        player: map_uv(t.translation),
        heading: heading(p),
        rotation: 0.0,
        waypoint: waypoint.0.map(|w| map_uv(w).extend(1.0).extend(1.0)).unwrap_or(Vec4::new(0.0, 0.0, 0.0, 1.0)),
        ..gps.packed(t.translation, f32::INFINITY)
    };
}

/// Red dots for enemies that are hunting the player, on the minimap.
#[allow(clippy::too_many_arguments)]
fn update_minimap_icons(
    state: Res<State<GameState>>,
    outposts: Res<crate::outposts::Outposts>,
    ways: Res<crate::traversal::Ways>,
    progress: Res<crate::progression::Progress>,
    players: Query<(&Transform, &Player)>,
    npcs: Query<(&GlobalTransform, &crate::npc::Npc, &crate::combat::Health, Has<crate::stealth::Tagged>)>,
    minimap: Query<Entity, With<Minimap>>,
    dots: Query<Entity, With<MinimapDot>>,
    mut commands: Commands,
) {
    for e in &dots {
        commands.entity(e).despawn();
    }
    if *state.get() != GameState::Playing {
        return;
    }
    let (Ok((t, p)), Ok(mini)) = (players.single(), minimap.single()) else { return };
    let rot = Rot2::radians(heading(p));
    // Outposts in range: red while enemy-held, blue once liberated.
    for o in &outposts.list {
        let d = o.centre - t.translation;
        let local = rot.inverse() * Vec2::new(d.x, d.z) / MINIMAP_RADIUS_M;
        if local.length() > 0.9 {
            continue;
        }
        let px = (local * 0.5 + Vec2::splat(0.5)) * MINIMAP_SIZE;
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - 7.0),
                top: Val::Px(px.y - 7.0),
                width: Val::Px(14.0),
                height: Val::Px(14.0),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(if o.liberated { Color::srgb(0.2, 0.5, 0.95) } else { Color::srgb(0.75, 0.1, 0.08) }),
            BorderColor::all(Color::srgb(0.05, 0.05, 0.05)),
            MinimapDot,
            ChildOf(mini),
        ));
    }
    // Radio towers: amber until taken, then green.
    for (i, at) in ways.data.towers.iter().enumerate() {
        let d = Vec3::from(*at) - t.translation;
        let local = rot.inverse() * Vec2::new(d.x, d.z) / MINIMAP_RADIUS_M;
        if local.length() > 0.9 {
            continue;
        }
        let px = (local * 0.5 + Vec2::splat(0.5)) * MINIMAP_SIZE;
        commands.spawn((tower_icon(px, 1.0, progress.towers.contains(&(i as u32))), MinimapDot, ChildOf(mini)));
    }
    for (gt, npc, h, tagged) in &npcs {
        if h.dead || !npc.faction.hostile_to_player() {
            continue;
        }
        let aware = npc.awareness > 0.3 || matches!(npc.state, crate::npc::NpcState::Combat | crate::npc::NpcState::Search);
        if !aware && !tagged {
            continue;
        }
        let d = gt.translation() - t.translation;
        // Map space: x east, y south.
        let offset = Vec2::new(d.x, d.z);
        let local = rot.inverse() * offset / MINIMAP_RADIUS_M;
        if local.length() > 0.92 {
            continue;
        }
        let px = (local * 0.5 + Vec2::splat(0.5)) * MINIMAP_SIZE;
        let colour = match npc.state {
            crate::npc::NpcState::Combat => Color::srgb(0.95, 0.15, 0.1),
            _ if aware => Color::srgb(1.0, 0.65, 0.15),
            _ => Color::srgb(0.9, 0.9, 0.85),
        };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - 4.0),
                top: Val::Px(px.y - 4.0),
                width: Val::Px(8.0),
                height: Val::Px(8.0),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BackgroundColor(colour),
            MinimapDot,
            ChildOf(mini),
        ));
    }
}

/// A radio tower marker: a mast with a cross-bar, centred on `px`.
fn tower_icon(px: Vec2, scale: f32, taken: bool) -> impl Bundle {
    let colour = if taken { Color::srgb(0.25, 0.85, 0.35) } else { Color::srgb(0.95, 0.7, 0.15) };
    let (w, h) = (5.0 * scale, 18.0 * scale);
    (
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(px.x - w / 2.0),
            top: Val::Px(px.y - h / 2.0),
            width: Val::Px(w),
            height: Val::Px(h),
            ..default()
        },
        BackgroundColor(colour),
        BorderColor::all(Color::srgb(0.05, 0.05, 0.05)),
        children![(
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(-w),
                top: Val::Px(h * 0.25),
                width: Val::Px(w * 3.0),
                height: Val::Px(3.0 * scale),
                ..default()
            },
            BackgroundColor(colour),
        )],
    )
}

/// Outpost icons on the full map; clicking a liberated one fast-travels there.
#[allow(clippy::too_many_arguments)]
fn full_map_icons(
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    view: Res<FullMapView>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut drag: Local<f32>,
    windows: Query<&Window, With<PrimaryWindow>>,
    outposts: Res<crate::outposts::Outposts>,
    towers: (Res<crate::traversal::Ways>, Res<crate::progression::Progress>),
    full: Query<(Entity, &ComputedNode), With<FullMap>>,
    icons: Query<Entity, With<FullMapIcon>>,
    mut players: Query<(&mut Transform, &mut Player)>,
    mut commands: Commands,
    (map_pad, gamepads): (Res<MapPad>, Query<&Gamepad>),
) {
    for e in &icons {
        commands.entity(e).despawn();
    }
    if *state.get() != GameState::Map {
        return;
    }
    let Ok((root, computed)) = full.single() else { return };
    let size = computed.size() * computed.inverse_scale_factor();
    let aspect = size.x / size.y.max(1.0);
    let half = Vec2::new(view.zoom * aspect, view.zoom);
    let to_px = |uv: Vec2| ((uv - view.center) / half * 0.5 + Vec2::splat(0.5)) * size;
    let cursor = if map_pad.0 { Some(size * 0.5) } else { windows.single().ok().and_then(|w| w.cursor_position()) };
    if mouse.pressed(MouseButton::Left) {
        *drag += motion.delta.length();
    }
    let clicked = (mouse.just_released(MouseButton::Left) && *drag < 6.0) || (map_pad.0 && gamepads.iter().any(|g| g.just_pressed(GamepadButton::South)));
    if map_pad.0 {
        // The gamepad cursor.
        commands.spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(size.x * 0.5 - 9.0), top: Val::Px(size.y * 0.5 - 16.0), ..default() },
            Text::new("+"),
            TextFont { font_size: bevy::text::FontSize::Px(28.0), ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.3)),
            FullMapIcon,
            ChildOf(root),
        ));
    }
    if !mouse.pressed(MouseButton::Left) {
        *drag = 0.0;
    }
    let (ways, progress) = towers;
    for (i, at) in ways.data.towers.iter().enumerate() {
        let px = to_px(map_uv(Vec3::from(*at)));
        if px.x < -20.0 || px.y < -20.0 || px.x > size.x + 20.0 || px.y > size.y + 20.0 {
            continue;
        }
        let taken = progress.towers.contains(&(i as u32));
        commands.spawn((tower_icon(px, 1.2, taken), FullMapIcon, ChildOf(root)));
        if cursor.is_some_and(|c| c.distance(px) < 12.0) {
            let label = if taken { "Radio tower (taken)" } else { "Radio tower: climb it and take it over" };
            commands.spawn((
                Node { position_type: PositionType::Absolute, left: Val::Px(px.x + 16.0), top: Val::Px(px.y - 12.0), padding: UiRect::all(Val::Px(6.0)), ..default() },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
                FullMapIcon,
                ChildOf(root),
                children![(Text::new(label), TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() }, TextColor(Color::srgb(0.95, 0.95, 0.9)))],
            ));
        }
    }
    let mut hovered = None;
    for (i, o) in outposts.list.iter().enumerate() {
        let px = to_px(map_uv(o.centre));
        if px.x < -20.0 || px.y < -20.0 || px.x > size.x + 20.0 || px.y > size.y + 20.0 {
            continue;
        }
        let over = cursor.is_some_and(|c| c.distance(px) < 12.0);
        if over {
            hovered = Some(i);
        }
        let r = if over { 10.0 } else { 8.0 };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - r),
                top: Val::Px(px.y - r),
                width: Val::Px(r * 2.0),
                height: Val::Px(r * 2.0),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(if o.liberated { Color::srgb(0.2, 0.5, 0.95) } else { Color::srgb(0.8, 0.12, 0.08) }),
            BorderColor::all(Color::srgb(0.95, 0.95, 0.9)),
            FullMapIcon,
            ChildOf(root),
        ));
    }
    if let Some(i) = hovered {
        let o = &outposts.list[i];
        let px = to_px(map_uv(o.centre));
        let label = match (o.liberated, map_pad.0) {
            (true, false) => "Liberated outpost\nclick to fast travel",
            (true, true) => "Liberated outpost\nA  fast travel",
            _ => "Enemy outpost",
        };
        commands.spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(px.x + 16.0), top: Val::Px(px.y - 12.0), padding: UiRect::all(Val::Px(6.0)), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
            FullMapIcon,
            ChildOf(root),
            children![(Text::new(label), TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() }, TextColor(Color::srgb(0.95, 0.95, 0.9)))],
        ));
        if clicked && o.liberated {
            if let Ok((mut t, mut p)) = players.single_mut() {
                if crate::outposts::fast_travel(&outposts, i, &mut t, &mut p) {
                    next.set(GameState::Playing);
                }
            }
        }
    }
}
