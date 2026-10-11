//! Heads-up display: crosshair, hit markers, health, ammo, compass,
//! damage feedback, death screen and a controls overlay.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

use crate::combat::{Health, PlayerStats, HEALTH_SEGMENT, PLAYER_MAX_HEALTH};
use crate::player::{InputCaptured, Player};
use crate::weapons::{HitMarker, Inventory, WeaponClass};

pub struct HudPlugin;

/// The UI's typeface: Windows' Bahnschrift, a plain condensed-capable sans
/// nearer the game's own UI type (its bitmap Trade Gothic) than Bevy's
/// built-in monospace, and with the curly quotes and accents the game's text
/// uses. `FC3_UI_FONT=<path>` picks another font file; `FC3_UI_FONT=builtin`
/// keeps Bevy's.
fn ui_font(mut fonts: ResMut<Assets<Font>>) {
    let path = match std::env::var("FC3_UI_FONT") {
        Ok(p) if p == "builtin" => return,
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts").join("bahnschrift.ttf"),
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            if fonts.insert(AssetId::default(), Font::from_bytes(bytes)).is_ok() {
                info!("UI font: {}", path.display());
            }
        }
        Err(e) => info!("UI font {}: {e}; keeping the built-in one", path.display()),
    }
}

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin::default()).init_resource::<HudState>().add_systems(Startup, (ui_font, setup_hud)).add_systems(
            Update,
            (
                update_crosshair,
                update_scope,
                update_hit_marker,
                update_health,
                update_ammo,
                update_compass,
                update_damage,
                update_grenades,
                update_overlays,
                update_detection,
                apply_hud_settings,
                update_status,
                hud_visibility,
            )
                .in_set(HudSet),
        );
    }
}

/// The HUD's systems. Systems that offer an interaction prompt or a status
/// line run before it; the prompt is cleared once shown.
#[derive(SystemSet, Clone, PartialEq, Eq, Hash, Debug)]
pub struct HudSet;

#[derive(Resource, Default)]
pub struct HudState {
    hit_timer: f32,
    hit_kill: bool,
    pub show_help: bool,
    /// Transient centre-screen message (pickups, objectives).
    pub message: Option<(String, f32)>,
    /// Interaction prompt, set every frame by whoever offers one.
    pub prompt: Option<String>,
    /// Context line under the compass (e.g. inside an enemy outpost).
    pub status: Option<String>,
}

impl HudState {
    pub fn notify(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), 3.0));
    }
}

#[derive(Component)]
struct CrosshairBar(usize);
#[derive(Component)]
struct HitMarkerUi;
#[derive(Component)]
struct HealthSegment(usize);
/// A health bar's slot (shown as far as the player's health goes).
#[derive(Component)]
struct HealthSlot(usize);

/// The most health bars the HUD has room for (all the health-slot skills,
/// and a boost on top).
const HEALTH_SLOTS: usize = 12;
#[derive(Component)]
struct HealthText;
#[derive(Component)]
struct AmmoText;
#[derive(Component)]
struct WeaponText;
#[derive(Component)]
struct CompassText;
#[derive(Component)]
struct DamageVignette;
#[derive(Component)]
struct DamageArrow;
/// Points toward a live grenade near the player.
#[derive(Component)]
struct GrenadeMarker(usize);
#[derive(Component)]
struct DeathScreen;
#[derive(Component)]
struct DeathText;
#[derive(Component)]
struct HelpPanel;
#[derive(Component)]
struct HelpText;
#[derive(Component)]
struct MessageText;
#[derive(Component)]
struct FpsText;
#[derive(Component)]
struct ClickToPlay;
#[derive(Component)]
struct PromptText;
#[derive(Component)]
struct DetectText;
#[derive(Component)]
struct StatusText;
/// Gameplay HUD element, hidden in menus, the map and the journal.
#[derive(Component)]
pub struct HudElement;

const HUD_WHITE: Color = Color::srgba(0.95, 0.95, 0.92, 0.9);

fn text(s: &str, size: f32) -> (Text, TextFont, TextColor) {
    (Text::new(s), TextFont { font_size: bevy::text::FontSize::Px(size), ..default() }, TextColor(HUD_WHITE))
}

fn setup_hud(mut commands: Commands) {
    let full = Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() };
    // Damage vignette (behind everything else).
    commands.spawn((full.clone(), vignette(Color::NONE, 0.0), DamageVignette, GlobalZIndex(0)));

    // Crosshair: four bars around the centre.
    let root = commands
        .spawn((Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() }, GlobalZIndex(1)))
        .id();
    for i in 0..4 {
        commands.spawn((
            Node { position_type: PositionType::Absolute, width: Val::Px(2.0), height: Val::Px(9.0), ..default() },
            BackgroundColor(HUD_WHITE),
            CrosshairBar(i),
            ChildOf(root),
        ));
    }
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            margin: UiRect::new(Val::Px(-11.0), Val::ZERO, Val::Px(-16.0), Val::ZERO),
            ..default()
        },
        text("x", 26.0),
        Visibility::Hidden,
        HitMarkerUi,
        ChildOf(root),
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), ..default() },
        Text::new("^"),
        TextFont { font_size: bevy::text::FontSize::Px(40.0), ..default() },
        TextColor(Color::srgba(1.0, 0.2, 0.15, 0.0)),
        DamageArrow,
        ChildOf(root),
    ));
    for i in 0..GRENADE_MARKERS {
        commands.spawn((
            Node { position_type: PositionType::Absolute, left: Val::Percent(50.0), top: Val::Percent(50.0), ..default() },
            Text::new("(!)"),
            TextFont { font_size: bevy::text::FontSize::Px(26.0), ..default() },
            TextColor(Color::srgba(1.0, 0.55, 0.1, 0.0)),
            GrenadeMarker(i),
            HudElement,
            ChildOf(root),
        ));
    }

    // Health: bottom-left segmented bar.
    let health_panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(36.0),
                bottom: Val::Px(36.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                ..default()
            },
            GlobalZIndex(1),
            HudElement,
        ))
        .id();
    commands.spawn((text("HEALTH", 14.0), HealthText, ChildOf(health_panel)));
    let bar = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), ..default() }, ChildOf(health_panel))).id();
    let segments = (PLAYER_MAX_HEALTH / HEALTH_SEGMENT) as usize;
    for i in 0..HEALTH_SLOTS {
        let shown = if i < segments { Display::Flex } else { Display::None };
        let seg = commands
            .spawn((
                Node { width: Val::Px(60.0), height: Val::Px(12.0), display: shown, ..default() },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
                HealthSlot(i),
                ChildOf(bar),
            ))
            .id();
        commands.spawn((
            Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
            BackgroundColor(Color::srgb(0.85, 0.85, 0.82)),
            HealthSegment(i),
            ChildOf(seg),
        ));
    }

    // Ammo and weapon: bottom-right.
    let ammo_panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(36.0),
                bottom: Val::Px(32.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                ..default()
            },
            GlobalZIndex(1),
            HudElement,
        ))
        .id();
    commands.spawn((text("", 16.0), WeaponText, ChildOf(ammo_panel)));
    commands.spawn((text("", 34.0), AmmoText, ChildOf(ammo_panel)));

    // Compass: top centre.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(14.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        GlobalZIndex(1),
        HudElement,
        children![(text("", 18.0), CompassText)],
    ));

    // Centre message.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(62.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(1),
        children![(text("", 20.0), MessageText)],
    ));

    // Detection indicator, under the compass.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(70.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        GlobalZIndex(1),
        HudElement,
        children![(text("", 20.0), DetectText)],
    ));

    // Context status (outposts), under the detection line.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(100.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        GlobalZIndex(1),
        HudElement,
        children![(text("", 16.0), StatusText)],
    ));

    // Interaction prompt.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(56.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(1),
        children![(text("", 18.0), PromptText)],
    ));

    // FPS counter: top-right.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(8.0), right: Val::Px(10.0), ..default() },
        text("", 12.0),
        FpsText,
        GlobalZIndex(1),
    ));

    // Click-to-play prompt while the cursor is free.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Percent(30.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(2),
        ClickToPlay,
        children![text("Click to resume  -  F1 for controls  -  Esc to pause", 20.0)],
    ));

    // Death screen.
    commands.spawn((
        full.clone(),
        BackgroundColor(Color::srgba(0.12, 0.0, 0.0, 0.7)),
        Visibility::Hidden,
        DeathScreen,
        GlobalZIndex(3),
        children![(
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            children![(text("", 40.0), DeathText)],
        )],
    ));

    // Controls help (text filled from the current bindings).
    commands.spawn((
        Node { position_type: PositionType::Absolute, left: Val::Percent(10.0), top: Val::Percent(20.0), padding: UiRect::all(Val::Px(18.0)), ..default() },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
        Visibility::Hidden,
        HelpPanel,
        GlobalZIndex(4),
        children![(text("", 17.0), HelpText)],
    ));
}

/// Shows whether nearby enemies are suspicious of, or have spotted, the player.
fn update_detection(npcs: Query<&crate::npc::Npc>, mut q: Query<(&mut Text, &mut TextColor), With<DetectText>>) {
    let Ok((mut t, mut c)) = q.single_mut() else { return };
    let hostile = || npcs.iter().filter(|n| n.faction.hostile_to_player());
    let spotted = hostile().any(|n| n.state == crate::npc::NpcState::Combat && n.sees_player);
    let hunting = hostile().any(|n| matches!(n.state, crate::npc::NpcState::Combat | crate::npc::NpcState::Search) && n.foe.is_none());
    // Busy with rebels: not about the player.
    let suspicious = hostile().filter(|n| n.foe.is_none()).map(|n| n.awareness).fold(0.0f32, f32::max);
    if spotted {
        t.0 = "DETECTED".into();
        c.0 = Color::srgb(1.0, 0.25, 0.2);
    } else if hunting {
        t.0 = "SEARCHING".into();
        c.0 = Color::srgb(1.0, 0.6, 0.2);
    } else if suspicious > 0.3 {
        t.0 = format!("SUSPICIOUS {:.0}%", suspicious * 100.0);
        c.0 = Color::srgb(1.0, 0.85, 0.3);
    } else {
        t.0.clear();
    }
}

/// The view through a rifle scope: a round field ringed in black, with a
/// duplex reticle (thick posts, fine centre).
#[derive(Component)]
struct ScopeOverlay;

fn spawn_scope(commands: &mut Commands) {
    let root = commands
        .spawn((
            Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
            BackgroundGradient::from(RadialGradient::new(
                UiPosition::CENTER,
                RadialGradientShape::ClosestSide,
                vec![
                    ColorStop::percent(Color::srgba(0.0, 0.0, 0.0, 0.0), 0.0),
                    ColorStop::percent(Color::srgba(0.0, 0.0, 0.0, 0.25), 90.0),
                    ColorStop::percent(Color::srgba(0.0, 0.0, 0.0, 0.85), 97.0),
                    ColorStop::percent(Color::BLACK, 98.5),
                ],
            )),
            Visibility::Hidden,
            GlobalZIndex(0),
            ScopeOverlay,
        ))
        .id();
    let ink = Color::srgba(0.02, 0.02, 0.02, 0.95);
    let bar = |commands: &mut Commands, left: Val, top: Val, width: Val, height: Val| {
        commands.spawn((Node { position_type: PositionType::Absolute, left, top, width, height, ..default() }, BackgroundColor(ink), ChildOf(root)));
    };
    // Fine centre lines.
    bar(commands, Val::Percent(20.0), Val::Percent(50.0), Val::Percent(60.0), Val::Px(1.5));
    bar(commands, Val::Percent(50.0), Val::Percent(8.0), Val::Px(1.5), Val::Percent(84.0));
    // Thick posts toward the edge of the field.
    bar(commands, Val::Percent(10.0), Val::Percent(49.6), Val::Percent(25.0), Val::Px(6.0));
    bar(commands, Val::Percent(65.0), Val::Percent(49.6), Val::Percent(25.0), Val::Px(6.0));
    bar(commands, Val::Percent(49.85), Val::Percent(70.0), Val::Px(6.0), Val::Percent(30.0));
}

fn update_scope(players: Query<&Inventory>, mut overlay: Query<&mut Visibility, With<ScopeOverlay>>, mut spawned: Local<bool>, mut commands: Commands) {
    if !*spawned {
        *spawned = true;
        spawn_scope(&mut commands);
        return;
    }
    let Ok(inv) = players.single() else { return };
    if let Ok(mut v) = overlay.single_mut() {
        *v = if inv.scoped() { Visibility::Inherited } else { Visibility::Hidden };
    }
}

fn update_crosshair(
    game_state: Res<State<crate::menu::GameState>>,
    players: Query<(&Inventory, &Player, &Health)>,
    mut bars: Query<(&CrosshairBar, &mut Node, &mut Visibility)>,
) {
    let Ok((inv, player, health)) = players.single() else { return };
    let d = inv.def();
    let move_penalty = if player.grounded { (player.velocity.length() / 6.0).min(1.0) } else { 1.0 };
    // (As the shots stray: the game's spread where the weapon has it.)
    let cone = d
        .spread_cone(inv.aim_blend, player.crouching, !player.grounded, inv.spread_bloom, move_penalty)
        .unwrap_or(d.spread * (1.0 - 0.8 * inv.aim_blend) * (1.0 + inv.spread_bloom + move_penalty * 0.8));
    let gap = 6.0 + cone * 9.0;
    let in_menu = *game_state.get() != crate::menu::GameState::Playing;
    let hidden = in_menu || health.dead || player.noclip || player.swimming || player.driving || inv.aim_blend > 0.7;
    for (bar, mut node, mut vis) in &mut bars {
        *vis = if hidden { Visibility::Hidden } else { Visibility::Inherited };
        let (w, h, dx, dy) = match bar.0 {
            0 => (2.0, 9.0, -1.0, -gap - 9.0),
            1 => (2.0, 9.0, -1.0, gap),
            2 => (9.0, 2.0, -gap - 9.0, -1.0),
            _ => (9.0, 2.0, gap, -1.0),
        };
        node.width = Val::Px(w);
        node.height = Val::Px(h);
        node.left = Val::Percent(50.0);
        node.top = Val::Percent(50.0);
        node.margin = UiRect::new(Val::Px(dx), Val::ZERO, Val::Px(dy), Val::ZERO);
    }
}

fn update_hit_marker(
    time: Res<Time>,
    mut state: ResMut<HudState>,
    mut markers: MessageReader<HitMarker>,
    mut died: MessageReader<crate::combat::Died>,
    players: Query<Entity, With<Player>>,
    mut ui: Query<(&mut Visibility, &mut TextColor), With<HitMarkerUi>>,
) {
    for m in markers.read() {
        state.hit_timer = 0.15;
        state.hit_kill = m.headshot;
    }
    let player = players.single().ok();
    for d in died.read() {
        if d.killer == player && Some(d.entity) != player {
            state.hit_timer = 0.3;
            state.hit_kill = true;
        }
    }
    state.hit_timer -= time.delta_secs();
    let Ok((mut vis, mut color)) = ui.single_mut() else { return };
    *vis = if state.hit_timer > 0.0 { Visibility::Inherited } else { Visibility::Hidden };
    color.0 = if state.hit_kill { Color::srgb(1.0, 0.25, 0.2) } else { HUD_WHITE };
}

fn update_health(
    progress: Res<crate::progression::Progress>,
    perks: Res<crate::progression::Perks>,
    players: Query<(&Health, &PlayerStats)>,
    mut segs: Query<(&HealthSegment, &mut Node, &mut BackgroundColor), Without<HealthSlot>>,
    mut slots: Query<(&HealthSlot, &mut Node), Without<HealthSegment>>,
    mut label: Query<&mut Text, With<HealthText>>,
) {
    let Ok((h, ps)) = players.single() else { return };
    // As many bars as the player's health has (skills and boosts add them).
    let bars = (h.max / HEALTH_SEGMENT).ceil() as usize;
    for (slot, mut node) in &mut slots {
        let display = if slot.0 < bars { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
    }
    for (seg, mut node, mut bg) in &mut segs {
        let lo = seg.0 as f32 * HEALTH_SEGMENT;
        let fill = ((h.current - lo) / HEALTH_SEGMENT).clamp(0.0, 1.0);
        node.width = Val::Percent(fill * 100.0);
        // (The boost's bars, beyond the player's own, in amber.)
        bg.0 = if h.current <= HEALTH_SEGMENT {
            Color::srgb(0.9, 0.25, 0.2)
        } else if lo >= perks.max_health - 0.1 {
            Color::srgb(0.95, 0.7, 0.25)
        } else {
            Color::srgb(0.85, 0.85, 0.82)
        };
    }
    if let Ok(mut t) = label.single_mut() {
        t.0 = if progress.armour > 0.0 {
            format!("HEALTH  {:.0}      ARMOR  {:.0}      MEDKITS  {}", h.current.ceil(), progress.armour.ceil(), ps.medkits)
        } else {
            format!("HEALTH  {:.0}      MEDKITS  {}", h.current.ceil(), ps.medkits)
        };
    }
}

fn update_ammo(
    players: Query<&Inventory>,
    progress: Res<crate::progression::Progress>,
    heat: Res<crate::turrets::GunHeat>,
    mut ammo: Query<&mut Text, (With<AmmoText>, Without<WeaponText>)>,
    mut weapon: Query<&mut Text, With<WeaponText>>,
) {
    let Ok(inv) = players.single() else { return };
    let d = inv.def();
    let s = inv.slot();
    if let Ok(mut t) = ammo.single_mut() {
        t.0 = if d.class == WeaponClass::Melee {
            String::new()
        } else if d.class == WeaponClass::Mounted {
            // The gun's heat instead of a magazine.
            if heat.overheated {
                "OVERHEATED".into()
            } else {
                let bars = (heat.heat * 10.0).round() as usize;
                format!("HEAT {}{}", "|".repeat(bars), ".".repeat(10 - bars.min(10)))
            }
        } else if inv.reloading > 0.0 {
            "RELOADING".into()
        } else {
            format!("{} / {}", s.mag, s.reserve)
        };
    }
    if let Ok(mut t) = weapon.single_mut() {
        let mut extra = [("C4", inv.c4), ("MINES", inv.mines)].iter().filter(|(_, n)| *n > 0).map(|(k, n)| format!("   {k}  {n}")).collect::<String>();
        // The bow: which arrow is nocked, and the special ones carried.
        if d.class == crate::weapons::WeaponClass::Bow && inv.special_arrows.iter().any(|n| *n > 0) {
            let [fire, blast] = inv.special_arrows;
            let nocked = crate::weapons::ARROW_NAMES[inv.arrow_kind as usize].to_uppercase();
            extra += &format!("   {nocked}  (FIRE {fire}  EXPLOSIVE {blast})");
        }
        t.0 = format!("${}      {}      GRENADES  {}   MOLOTOVS  {}{extra}", progress.money, d.name.to_uppercase(), inv.grenades, inv.molotovs);
    }
}

fn update_compass(players: Query<(&Player, &Transform)>, waypoint: Res<crate::map::Waypoint>, mut q: Query<&mut Text, With<CompassText>>) {
    let Ok((p, pt)) = players.single() else { return };
    let Ok(mut t) = q.single_mut() else { return };
    // yaw 0 faces north (-Z); positive yaw turns left (west).
    let heading = (-p.yaw.to_degrees()).rem_euclid(360.0);
    let names = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    // Bearing to the waypoint, marked on the strip when in view.
    let target = waypoint.0.map(|w| {
        let d = w - pt.translation;
        let bearing = d.x.atan2(-d.z).to_degrees().rem_euclid(360.0);
        (bearing, Vec2::new(d.x, d.z).length())
    });
    let mut s = String::new();
    for k in -4i32..=4 {
        let deg = (heading / 15.0).round() * 15.0 + k as f32 * 15.0;
        let d = deg.rem_euclid(360.0);
        let marked = target.is_some_and(|(b, _)| ((b - d + 540.0).rem_euclid(360.0) - 180.0).abs() <= 7.5);
        let label = if marked {
            "[*]".to_string()
        } else if (d % 45.0).abs() < 0.01 {
            names[((d / 45.0).round() as usize) % 8].to_string()
        } else {
            "|".to_string()
        };
        s.push_str(&format!("{label:^5}"));
    }
    let distance = target.map(|(_, m)| format!("      [*] {m:.0} m")).unwrap_or_default();
    t.0 = format!("{s}\n{:>3.0}{distance}", heading);
}

/// A screen-edge vignette: clear in the middle, `colour` at the edges;
/// `reach` (0..1) pulls the colour further in.
fn vignette(colour: Color, reach: f32) -> BackgroundGradient {
    let inner = 78.0 - 38.0 * reach.clamp(0.0, 1.0);
    BackgroundGradient::from(RadialGradient::new(
        UiPosition::CENTER,
        RadialGradientShape::FarthestCorner,
        vec![ColorStop::percent(colour.with_alpha(0.0), inner), ColorStop::percent(colour, 100.0)],
    ))
}

fn update_damage(
    time: Res<Time>,
    players: Query<(&Health, &PlayerStats, &Player, &GlobalTransform)>,
    mut vignettes: Query<&mut BackgroundGradient, With<DamageVignette>>,
    mut arrow: Query<(&mut Node, &mut TextColor, &mut Text, &mut UiTransform), With<DamageArrow>>,
) {
    let Ok((h, ps, p, gt)) = players.single() else { return };
    let low = (1.0 - h.current / (HEALTH_SEGMENT * 1.2)).clamp(0.0, 1.0);
    let pulse = ps.hurt_dir.map(|(_, s)| s).unwrap_or(0.0);
    if let Ok(mut bg) = vignettes.single_mut() {
        // Low health and hits redden the edges; burning adds flickering orange.
        let burn = ps.burning.min(1.0);
        let t = time.elapsed_secs();
        let flicker = 0.75 + 0.25 * (t * 23.0).sin() * (t * 7.3).cos();
        let red = (low * 0.55 + pulse * 0.25).min(0.65);
        let orange = burn * 0.4 * flicker;
        let a = (red + orange).min(0.7);
        let mix = if a > 0.0 { orange / a } else { 0.0 };
        *bg = vignette(Color::srgba(0.55 + 0.4 * mix, 0.32 * mix, 0.0, a), low.max(burn * 0.25));
    }
    let Ok((mut node, mut color, mut text, mut xf)) = arrow.single_mut() else { return };
    match ps.hurt_dir {
        Some((dir, s)) => {
            // Angle of the damage source relative to where the player faces.
            let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
            let right = Vec3::new(-fwd.z, 0.0, fwd.x);
            let flat = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
            let ang = flat.dot(right).atan2(flat.dot(fwd));
            let r = 120.0;
            node.margin = UiRect::new(Val::Px(ang.sin() * r - 10.0), Val::ZERO, Val::Px(-ang.cos() * r - 24.0), Val::ZERO);
            text.0 = "^".into();
            xf.rotation = Rot2::radians(ang);
            color.0 = Color::srgba(1.0, 0.2, 0.15, s.min(1.0));
            let _ = gt;
        }
        None => color.0 = Color::srgba(1.0, 0.2, 0.15, 0.0),
    }
}

const GRENADE_MARKERS: usize = 4;
/// Grenades closer than this are flagged.
const GRENADE_WARN: f32 = 12.0;

/// Marks live grenades around the player, placed around the crosshair in
/// their direction and brighter the closer they are.
fn update_grenades(
    players: Query<(&Player, &Transform)>,
    grenades: Query<(&crate::weapons::Projectile, &Transform), Without<Player>>,
    mut markers: Query<(&GrenadeMarker, &mut Node, &mut TextColor)>,
) {
    let Ok((p, pt)) = players.single() else { return };
    let mut near: Vec<(f32, Vec3)> = grenades
        .iter()
        .filter(|(g, _)| g.fuse.is_some())
        .map(|(_, t)| (t.translation.distance(pt.translation), t.translation))
        .filter(|(d, _)| *d < GRENADE_WARN)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(-fwd.z, 0.0, fwd.x);
    for (m, mut node, mut colour) in &mut markers {
        let Some(&(d, at)) = near.get(m.0) else {
            colour.0 = colour.0.with_alpha(0.0);
            continue;
        };
        let flat = Vec3::new(at.x - pt.translation.x, 0.0, at.z - pt.translation.z).normalize_or(fwd);
        let ang = flat.dot(right).atan2(flat.dot(fwd));
        let r = 90.0 + d * 6.0;
        node.margin = UiRect::new(Val::Px(ang.sin() * r - 16.0), Val::ZERO, Val::Px(-ang.cos() * r - 14.0), Val::ZERO);
        colour.0 = Color::srgba(1.0, 0.55, 0.1, (1.2 - d / GRENADE_WARN).clamp(0.35, 1.0));
    }
}

fn hud_visibility(state: Res<State<crate::menu::GameState>>, scripted: Res<crate::story::ScriptedPlayer>, mut q: Query<&mut Visibility, With<HudElement>>) {
    if !state.is_changed() && !scripted.is_changed() {
        return;
    }
    // Missions hide the HUD for their scenes.
    let show = matches!(state.get(), crate::menu::GameState::Playing) && !scripted.hud_hidden;
    for mut v in &mut q {
        *v = if show { Visibility::Inherited } else { Visibility::Hidden };
    }
}

fn update_status(state: Res<HudState>, mut q: Query<&mut Text, With<StatusText>>) {
    if let Ok(mut t) = q.single_mut() {
        let s = state.status.clone().unwrap_or_default();
        if t.0 != s {
            t.0 = s;
        }
    }
}

/// Help text and FPS counter follow the settings.
fn apply_hud_settings(
    settings: Res<crate::settings::Settings>,
    mut help_text: Query<&mut Text, With<HelpText>>,
    mut fps_vis: Query<&mut Visibility, With<FpsText>>,
) {
    if !settings.is_changed() {
        return;
    }
    if let Ok(mut t) = help_text.single_mut() {
        t.0 = help_text_for(&settings);
    }
    if let Ok(mut v) = fps_vis.single_mut() {
        *v = if settings.show_fps { Visibility::Inherited } else { Visibility::Hidden };
    }
}

/// The F1 help text for the current key bindings.
fn help_text_for(s: &crate::settings::Settings) -> String {
    use crate::controls::Action as A;
    let k = |a: A| s.binding(a).map(|b| b.label()).unwrap_or_else(|| "-".into());
    let line = |items: &[(String, &str)]| items.iter().map(|(key, what)| format!("{key}  {what}")).collect::<Vec<_>>().join("      ");
    let move_keys = format!("{}{}{}{}", k(A::Forward), k(A::Left), k(A::Back), k(A::Right));
    [
        "CONTROLS".to_string(),
        line(&[
            (move_keys, "move"),
            (k(A::Sprint), "sprint / hold breath"),
            (k(A::Crouch), "crouch"),
            (k(A::Jump), "jump / swim up"),
            (k(A::SwimDown), "swim down"),
        ]),
        line(&[
            ("Mouse".into(), "look"),
            (k(A::Fire), "fire"),
            (k(A::Aim), "aim"),
            (k(A::Reload), "reload"),
            ("1-8 / wheel".into(), "switch weapon"),
            (k(A::Melee), "machete"),
        ]),
        line(&[(k(A::Grenade), "throw grenade"), (k(A::Medkit), "use medkit"), (k(A::Interact), "pick up / interact"), (k(A::Map), "map"), (k(A::Tag), "tag")]),
        line(&[
            ("F1".into(), "toggle this help"),
            ("F2".into(), "noclip (debug)"),
            ("T".into(), "advance time"),
            ("F4".into(), "original shaders"),
            ("Esc".into(), "pause / options"),
        ]),
    ]
    .join("\n")
}

#[allow(clippy::too_many_arguments)]
fn update_overlays(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    captured: Res<InputCaptured>,
    args: Res<crate::Args>,
    game_state: Res<State<crate::menu::GameState>>,
    diagnostics: Res<DiagnosticsStore>,
    mut state: ResMut<HudState>,
    players: Query<(&Health, &PlayerStats)>,
    mut death: Query<&mut Visibility, (With<DeathScreen>, Without<HelpPanel>, Without<ClickToPlay>)>,
    mut death_text: Query<&mut Text, (With<DeathText>, Without<MessageText>, Without<FpsText>, Without<PromptText>)>,
    mut help: Query<&mut Visibility, (With<HelpPanel>, Without<DeathScreen>, Without<ClickToPlay>)>,
    mut click: Query<&mut Visibility, (With<ClickToPlay>, Without<DeathScreen>, Without<HelpPanel>)>,
    mut message: Query<&mut Text, (With<MessageText>, Without<DeathText>, Without<FpsText>, Without<PromptText>)>,
    mut fps: Query<&mut Text, (With<FpsText>, Without<DeathText>, Without<MessageText>, Without<PromptText>)>,
    mut prompt: Query<&mut Text, (With<PromptText>, Without<DeathText>, Without<MessageText>, Without<FpsText>)>,
) {
    if let Ok(mut t) = prompt.single_mut() {
        t.0 = state.prompt.take().unwrap_or_default();
    }
    if keys.just_pressed(KeyCode::F1) {
        state.show_help = !state.show_help;
    }
    if let Ok(mut v) = help.single_mut() {
        *v = if state.show_help { Visibility::Inherited } else { Visibility::Hidden };
    }
    if let Ok(mut v) = click.single_mut() {
        let playing = *game_state.get() == crate::menu::GameState::Playing;
        *v = if playing && !captured.0 && !args.automated() { Visibility::Inherited } else { Visibility::Hidden };
    }
    if let Ok((h, ps)) = players.single() {
        if let Ok(mut v) = death.single_mut() {
            *v = if h.dead { Visibility::Inherited } else { Visibility::Hidden };
        }
        if let Ok(mut t) = death_text.single_mut() {
            t.0 = format!("YOU DIED\n\nRespawning in {:.0}", ps.respawn_timer.max(0.0).ceil());
        }
    }
    if let Some((_, ttl)) = &mut state.message {
        *ttl -= time.delta_secs();
    }
    if state.message.as_ref().is_some_and(|(_, ttl)| *ttl <= 0.0) {
        state.message = None;
    }
    if let Ok(mut t) = message.single_mut() {
        t.0 = state.message.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
    }
    if let Ok(mut t) = fps.single_mut() {
        if let Some(v) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()) {
            t.0 = format!("{v:.0} fps");
        }
    }
}
