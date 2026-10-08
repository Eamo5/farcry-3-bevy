//! Gamepad navigation for every menu: the D-pad or left stick moves a focus
//! between the visible buttons by their position on screen, and A presses
//! the focused one (B backs out; each menu handles that itself). The focus
//! drives the buttons' ordinary `Interaction`, so menus need nothing extra.
//! Moving the mouse hands control back to it.

use bevy::input::gamepad::{Gamepad, GamepadButton};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::ui::{ComputedNode, UiGlobalTransform};

pub struct UiNavPlugin;

impl Plugin for UiNavPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NavFocus>().add_systems(PreUpdate, (nav_test, navigate).chain().after(bevy::ui::UiSystems::Focus));
    }
}

/// FC3_NAV_TEST (tests): a virtual gamepad presses down three times, then A,
/// a second apart, logging the focused button's label each time.
fn nav_test(
    time: Res<Time<Real>>,
    mut step: Local<(u32, f32, Option<Entity>)>,
    mut commands: Commands,
    mut pads: Query<&mut Gamepad>,
    focus: Res<NavFocus>,
    children: Query<&Children>,
    texts: Query<&Text>,
    state: Res<State<crate::menu::GameState>>,
) {
    if std::env::var("FC3_NAV_TEST").is_err() {
        return;
    }
    let (n, next, pad) = &mut *step;
    let Some(pad) = *pad else {
        *pad = Some(commands.spawn(Gamepad::default()).id());
        *next = time.elapsed_secs() + 6.0;
        return;
    };
    let label = |e: Entity| -> String { children.iter_descendants(e).filter_map(|c| texts.get(c).ok()).map(|t| t.0.clone()).collect::<Vec<_>>().join(" ") };
    if time.elapsed_secs() < *next || *n > 8 {
        return;
    }
    *next = time.elapsed_secs() + 1.0;
    let Ok(mut g) = pads.get_mut(pad) else { return };
    let button = match *n {
        0 => None,
        1..=3 => Some(GamepadButton::DPadDown),
        4 => Some(GamepadButton::South),
        _ => None,
    };
    info!("nav-test step {n} ({:?}): focus on {:?}, pressing {button:?}", state.get(), focus.entity.map(label));
    g.digital_mut().release_all();
    if let Some(b) = button {
        g.digital_mut().press(b);
    }
    // Let go next time round.
    *n += 1;
}

#[derive(Resource, Default)]
struct NavFocus {
    /// The gamepad was used last (the mouse takes over when it moves).
    active: bool,
    entity: Option<Entity>,
    /// Pressed last frame: released this frame.
    pressed: Option<Entity>,
    /// Seconds until a held stick moves the focus again.
    repeat: f32,
}

/// Stick deflection that counts as a push.
const STICK: f32 = 0.6;

fn navigate(
    time: Res<Time<Real>>,
    gamepads: Query<&Gamepad>,
    mouse: Res<AccumulatedMouseMotion>,
    mut focus: ResMut<NavFocus>,
    mut buttons: Query<(Entity, &mut Interaction, &ComputedNode, &UiGlobalTransform, &InheritedVisibility), With<Button>>,
) {
    // Release last frame's press.
    if let Some(p) = focus.pressed.take() {
        if let Ok((_, mut i, _, _, _)) = buttons.get_mut(p) {
            if *i == Interaction::Pressed {
                *i = Interaction::None;
            }
        }
    }
    let pad = |b: GamepadButton| gamepads.iter().any(|g| g.just_pressed(b));
    let stick = gamepads.iter().map(|g| g.left_stick()).find(|s| s.length() > STICK).unwrap_or(Vec2::ZERO);
    let any_pad = gamepads.iter().any(|g| g.get_just_pressed().next().is_some()) || stick != Vec2::ZERO;
    // The first touch after the mouse only shows where the focus is.
    let woke = any_pad && !focus.active;
    if any_pad {
        focus.active = true;
    } else if mouse.delta.length_squared() > 4.0 {
        focus.active = false;
        focus.entity = None;
    }
    if !focus.active {
        return;
    }
    // Visible, laid-out buttons and their centres (screen pixels, y down).
    let items: Vec<(Entity, Vec2)> =
        buttons.iter().filter(|(_, _, node, _, vis)| vis.get() && node.size().min_element() > 1.0).map(|(e, _, _, t, _)| (e, t.translation)).collect();
    if items.is_empty() {
        focus.entity = None;
        return;
    }
    // Keep the focus on a visible button; start top-left.
    let current = focus.entity.and_then(|f| items.iter().find(|(e, _)| *e == f)).copied();
    let (cur, at) = current.unwrap_or_else(|| *items.iter().min_by(|a, b| (a.1.y, a.1.x).partial_cmp(&(b.1.y, b.1.x)).unwrap()).unwrap());
    focus.entity = Some(cur);

    // Direction: D-pad presses at once, the stick with auto-repeat.
    focus.repeat -= time.delta_secs();
    let mut dir = Vec2::ZERO;
    if pad(GamepadButton::DPadUp) {
        dir = Vec2::NEG_Y;
    } else if pad(GamepadButton::DPadDown) {
        dir = Vec2::Y;
    } else if pad(GamepadButton::DPadLeft) {
        dir = Vec2::NEG_X;
    } else if pad(GamepadButton::DPadRight) {
        dir = Vec2::X;
    } else if stick != Vec2::ZERO && focus.repeat <= 0.0 {
        // Stick up is +y; screen up is -y. Snap to the main axis.
        dir = if stick.x.abs() > stick.y.abs() { Vec2::new(stick.x.signum(), 0.0) } else { Vec2::new(0.0, -stick.y.signum()) };
    }
    if stick == Vec2::ZERO {
        focus.repeat = 0.0;
    }
    if dir != Vec2::ZERO && !woke {
        focus.repeat = 0.22;
        let best = items
            .iter()
            .filter(|(e, _)| *e != cur)
            .filter_map(|(e, p)| {
                let v = *p - at;
                let along = v.dot(dir);
                (along > 1.0).then(|| (*e, along + v.perp_dot(dir).abs() * 2.5))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((e, _)) = best {
            focus.entity = Some(e);
        }
    }
    let Some(f) = focus.entity else { return };
    // Show the focus as a hover; A presses.
    for (e, mut i, _, _, _) in &mut buttons {
        if e == f {
            if pad(GamepadButton::South) && !woke {
                *i = Interaction::Pressed;
                focus.pressed = Some(e);
            } else if *i == Interaction::None {
                *i = Interaction::Hovered;
            }
        } else if *i == Interaction::Hovered {
            *i = Interaction::None;
        }
    }
}
