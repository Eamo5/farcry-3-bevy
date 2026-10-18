//! The weapon wheel: holding the next-weapon input (Y on a gamepad, X on the
//! keyboard) opens the game's four places of weapons carried (the guns, then
//! the machete if one is left), slows the world down, and the look input
//! picks one; letting go takes it out. Under the original HUD its own wheel
//! shows (`original_ui::drive_wheel`). A short tap still just
//! returns to the previous weapon (on release), as in the game.

use bevy::prelude::*;

use crate::controls::{Action, ActionState};
use crate::player::Player;
use crate::weapons::Inventory;

pub struct WeaponWheelPlugin;

impl Plugin for WeaponWheelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponWheel>()
            .add_systems(Startup, setup_wheel)
            .add_systems(Update, (wheel_test, wheel_input, draw_wheel).chain().run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(OnExit(crate::menu::GameState::Playing), close_wheel);
    }
}

/// Held this long (real seconds), the input opens the wheel.
const HOLD: f32 = 0.3;
/// The world runs this fast while it's open.
const SLOW: f32 = 0.3;
/// Look input (radians) to wheel cursor travel.
const CURSOR_GAIN: f32 = 18.0;
/// `sndWeaponWheelOpen` / `sndWeaponWheelClose` (player archetype).
const OPEN_SOUND: u32 = 0x001A_F336;
const CLOSE_SOUND: u32 = 0x001A_F335;

#[derive(Resource, Default)]
pub struct WeaponWheel {
    /// Open: the view holds still and the trigger is off.
    pub open: bool,
    /// A short tap (previous weapon), or the slot picked on the wheel; taken by
    /// the weapon switch.
    pub tap: bool,
    pub pick: Option<usize>,
    /// Definition identity, not a slot index: carried slots may be reordered.
    pub previous: Option<usize>,
    held: f32,
    cursor: Vec2,
    /// The slot under the cursor (the original HUD's wheel lights it too).
    pub selected: Option<usize>,
}

/// The wheel's places, the game's four: top, right, bottom, left.
pub const PLACES: usize = 4;

/// The slots on the wheel, by place: the guns carried first, then the
/// machete if a place is left (its own input swings it anyway).
pub fn wheel_slots(inv: &Inventory) -> Vec<usize> {
    let melee = |i: &usize| inv.defs[inv.slots[*i].def].class == crate::weapons::WeaponClass::Melee;
    let (mut places, rest): (Vec<usize>, Vec<usize>) = (0..inv.slots.len()).partition(|i| !melee(i));
    places.extend(rest);
    places.truncate(PLACES);
    places
}

impl WeaponWheel {
    pub fn quick_swap(&self, slots: &[crate::weapons::WeaponSlot], current: usize) -> Option<usize> {
        if slots.len() < 2 { return None; }
        self.previous.and_then(|def| slots.iter().position(|s| s.def == def))
            .filter(|&i| i != current)
            .or_else(|| Some((current + 1) % slots.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_swap_returns_to_previous_identity_after_slot_reordering_or_removal() {
        let mut slots: Vec<_> = [10, 20, 30].into_iter().map(|def| crate::weapons::WeaponSlot { def, mag: 1, reserve: 2 }).collect();
        let wheel = WeaponWheel { previous: Some(10), ..default() };
        assert_eq!(wheel.quick_swap(&slots, 1), Some(0));
        slots.swap(0, 2);
        assert_eq!(wheel.quick_swap(&slots, 1), Some(2));
        slots.pop();
        assert_eq!(wheel.quick_swap(&slots, 1), Some(0));
        assert_eq!(wheel.quick_swap(&slots[..1], 0), None);
        assert_eq!(wheel.quick_swap(&[], 0), None);
    }
}

#[derive(Component)]
struct WheelRoot;

#[derive(Component)]
struct WheelLabel(usize);

fn setup_wheel(mut commands: Commands) {
    commands.spawn((
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35)),
        Visibility::Hidden,
        GlobalZIndex(6),
        WheelRoot,
    ));
}

fn wheel_input(
    real: Res<Time<Real>>,
    mut virt: ResMut<Time<Virtual>>,
    actions: Res<ActionState>,
    mut wheel: ResMut<WeaponWheel>,
    players: Query<(&Inventory, &Player, &crate::combat::Health)>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let Ok((inv, p, health)) = players.single() else { return };
    let ui = |event| crate::sound::PlaySfx { event, at: None, volume: 0.6 };
    // Not on a mounted gun, at the wheel of a vehicle, in the water or dead.
    let usable = inv.mounted_from.is_none() && !p.driving && !p.swimming && !health.dead;
    if actions.pressed(Action::NextWeapon) && (usable || wheel.open) {
        wheel.held += real.delta_secs();
        if !wheel.open && wheel.held > HOLD {
            wheel.open = true;
            wheel.cursor = Vec2::ZERO;
            wheel.selected = None;
            virt.set_relative_speed(SLOW);
            sfx.write(ui(OPEN_SOUND));
        }
    } else if wheel.held > 0.0 {
        if wheel.open {
            wheel.pick = wheel.selected;
            wheel.open = false;
            virt.set_relative_speed(1.0);
            sfx.write(ui(CLOSE_SOUND));
        } else {
            wheel.tap = true;
        }
        wheel.held = 0.0;
    }
    if !wheel.open {
        return;
    }
    // The look input steers a cursor round the ring.
    wheel.cursor = (wheel.cursor + Vec2::new(-actions.look.x, actions.look.y) * CURSOR_GAIN).clamp_length_max(1.0);
    let places = wheel_slots(inv);
    wheel.selected = (wheel.cursor.length() > 0.35)
        .then(|| {
            // The first place at the top, going clockwise.
            let angle = wheel.cursor.x.atan2(wheel.cursor.y).rem_euclid(std::f32::consts::TAU);
            ((angle / std::f32::consts::TAU * PLACES as f32 + 0.5) as usize) % PLACES
        })
        .and_then(|p| places.get(p).copied());
}

/// The ring of weapon names, the one under the cursor lit.
fn draw_wheel(
    wheel: Res<WeaponWheel>,
    players: Query<&Inventory, With<Player>>,
    mut roots: Query<(Entity, &mut Visibility, Option<&Children>), With<WheelRoot>>,
    mut labels: Query<(&WheelLabel, &mut TextColor)>,
    mut commands: Commands,
    original: Res<crate::hud::OriginalHud>,
) {
    let (Ok((root, mut vis, children)), Ok(inv)) = (roots.single_mut(), players.single()) else { return };
    // (The original HUD shows its own: `original_ui::drive_wheel`.)
    if !wheel.open || original.0 {
        if *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
            if let Some(c) = children {
                for e in c.iter() {
                    commands.entity(e).despawn();
                }
            }
        }
        return;
    }
    if *vis == Visibility::Hidden {
        *vis = Visibility::Inherited;
        for (p, &i) in wheel_slots(inv).iter().enumerate() {
            let slot = &inv.slots[i];
            let a = p as f32 / PLACES as f32 * std::f32::consts::TAU;
            let (x, y) = (50.0 + 26.0 * a.sin(), 50.0 - 30.0 * a.cos());
            let name = inv.defs[slot.def].name.to_uppercase();
            let ammo = if slot.mag + slot.reserve > 0 { format!("\n{} / {}", slot.mag, slot.reserve) } else { String::new() };
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(x - 8.0),
                    top: Val::Percent(y - 3.0),
                    width: Val::Percent(16.0),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                Text::new(format!("{name}{ammo}")),
                TextFont { font_size: bevy::text::FontSize::Px(20.0), ..default() },
                TextLayout::new(Justify::Center, bevy::text::LineBreak::WordBoundary),
                TextColor(Color::WHITE),
                WheelLabel(i),
                ChildOf(root),
            ));
        }
    }
    for (label, mut colour) in &mut labels {
        let lit = Some(label.0) == wheel.selected;
        let current = label.0 == inv.current;
        colour.0 = if lit {
            Color::srgb(1.0, 0.75, 0.2)
        } else if current {
            Color::srgb(0.75, 0.9, 1.0)
        } else {
            Color::srgba(1.0, 1.0, 1.0, 0.8)
        };
    }
}

/// `FC3_WHEEL_TEST`: 20 s in, holds the next-weapon input for two seconds
/// while steering down and to the right, then lets go; logs what happened.
fn wheel_test(
    real: Res<Time<Real>>,
    virt: Res<Time<Virtual>>,
    mut start: Local<Option<f32>>,
    mut logged: Local<u8>,
    mut actions: ResMut<ActionState>,
    wheel: Res<WeaponWheel>,
    players: Query<&Inventory, With<Player>>,
) {
    if std::env::var("FC3_WHEEL_TEST").is_err() {
        return;
    }
    let now = real.elapsed_secs();
    let t = now - *start.get_or_insert(now);
    if (20.0..22.0).contains(&t) {
        actions.hold(Action::NextWeapon);
        actions.look = Vec2::new(-0.03, 0.0);
    }
    let Ok(inv) = players.single() else { return };
    if *logged == 0 && t > 21.0 {
        *logged = 1;
        info!(
            "wheel-test: open {} selected {:?} speed {:.2} holding slot {} {}",
            wheel.open,
            wheel.selected,
            virt.relative_speed(),
            inv.current,
            inv.defs[inv.slots[inv.current].def].name
        );
    }
    if *logged == 1 && t > 23.0 {
        *logged = 2;
        info!("wheel-test: closed {} speed {:.2} now holding {}", !wheel.open, virt.relative_speed(), inv.defs[inv.slots[inv.current].def].name);
    }
}

/// Pausing (or leaving play) with the wheel open puts the world back to speed.
fn close_wheel(mut wheel: ResMut<WeaponWheel>, mut virt: ResMut<Time<Virtual>>) {
    if wheel.open {
        wheel.open = false;
        wheel.held = 0.0;
        virt.set_relative_speed(1.0);
    }
}
