//! The end credits. The story asks for them as the game ends
//! (`PopUpEndOfGame`, from `EndGame.lua`, as JAS_240's last scene fades to
//! black) and waits for the engine to say they're over (`CreditsFinished`).
//! The original interface rolls them (`original_ui::drive_credits`); the
//! skip keys cut them short (they fade out), as tests' autopilot does once
//! it has watched a while. With nothing to roll them, the story goes on.

use bevy::input::gamepad::{Gamepad, GamepadButton};
use bevy::prelude::*;

use crate::story::StoryHost;

pub struct CreditsPlugin;

impl Plugin for CreditsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EndCredits>().add_systems(Update, run_credits.after(crate::story::StorySet));
    }
}

/// How long the interface has to take the credits up before the story goes
/// on without them (s).
const NO_ROLL_WAIT: f32 = 3.0;
/// How long a skip's fade has to end them (s).
const SKIP_WAIT: f32 = 3.0;
/// The skip keys aren't heard this soon after the credits start (s): the
/// last scene's skip.
const SKIP_GRACE: f32 = 1.0;
/// How much of them tests' autopilot watches (s).
const AUTOPILOT_WATCH: f32 = 20.0;
/// When tests capture them rolling (`FC3_CREDITS_SHOT=<png>`, s).
const SHOT_AT: f32 = 12.0;

/// The end credits, while the story waits for them (real time: they roll
/// on with the game paused).
#[derive(Resource, Default)]
pub struct EndCredits {
    /// Asked for, this long ago (s).
    pub asked: Option<f32>,
    /// The interface rolls them.
    pub rolling: bool,
    /// Cut short, this long ago (s).
    pub skipped: Option<f32>,
    /// They're over: the story is told.
    pub over: bool,
}

impl EndCredits {
    pub fn on(&self) -> bool {
        self.asked.is_some()
    }

    /// Time going by (`dt`, s), with a skip key pressed or tests' autopilot
    /// on: whether they're now over.
    fn tick(&mut self, dt: f32, pressed: bool, autopilot: bool) -> bool {
        let Some(t) = self.asked.map(|a| a + dt) else { return false };
        self.asked = Some(t);
        self.skipped = self.skipped.map(|s| s + dt);
        if self.skipped.is_none() && ((pressed && t > SKIP_GRACE) || (autopilot && t > AUTOPILOT_WATCH)) {
            info!("end credits: skipped");
            self.skipped = Some(0.0);
        }
        if !self.rolling && !self.over && (t > NO_ROLL_WAIT || self.skipped.is_some()) {
            warn!("end credits: nothing rolls them; the story goes on");
            self.over = true;
        }
        self.over |= self.skipped.is_some_and(|s| s > SKIP_WAIT);
        self.over
    }
}

fn run_credits(
    host: Option<NonSendMut<StoryHost>>,
    mut credits: ResMut<EndCredits>,
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    // (Asked again as they roll, by the game's last mission as the ending's
    // gives way to it, they roll on.)
    if std::mem::take(&mut host.end_of_game) && !credits.on() {
        info!("end credits");
        *credits = EndCredits { asked: Some(0.0), ..default() };
    }
    let Some(t) = credits.asked else { return };
    let dt = time.delta_secs();
    if let Some(path) = std::env::var_os("FC3_CREDITS_SHOT").filter(|_| t < SHOT_AT && t + dt >= SHOT_AT) {
        use bevy::render::view::window::screenshot::{save_to_disk, Screenshot};
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(std::path::PathBuf::from(path)));
    }
    let pressed = keys.any_just_pressed([KeyCode::Space, KeyCode::Enter, KeyCode::Escape])
        || gamepads.iter().any(|g| g.just_pressed(GamepadButton::South) || g.just_pressed(GamepadButton::Start));
    if credits.tick(dt, pressed, std::env::var_os("FC3_STORY_AUTOPILOT").is_some()) {
        info!("end credits over");
        host.story.message("CreditsFinished");
        *credits = EndCredits::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(rolling: bool) -> EndCredits {
        EndCredits { asked: Some(0.0), rolling, ..default() }
    }

    /// With nothing to roll them, the credits are over in a moment; rolled,
    /// they last until the interface says so, or a skip's fade has had its
    /// time (a key pressed as they start is the last scene's, not heard).
    #[test]
    fn credits_end_when_rolled_by_skipped_or_with_nothing_to_roll_them() {
        let mut c = asked(false);
        assert!(!c.tick(1.0, false, false));
        assert!(c.tick(NO_ROLL_WAIT, false, false));

        let mut c = asked(true);
        assert!(!c.tick(60.0, false, false));
        c.over = true;
        assert!(c.tick(0.1, false, false));

        let mut c = asked(true);
        assert!(!c.tick(0.1, true, false) && c.skipped.is_none());
        assert!(!c.tick(SKIP_GRACE, true, false) && c.skipped.is_some());
        assert!(c.tick(SKIP_WAIT + 0.1, false, false));

        let mut c = asked(true);
        assert!(!c.tick(AUTOPILOT_WATCH + 0.1, false, true) && c.skipped.is_some());
    }
}
