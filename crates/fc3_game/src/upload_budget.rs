//! Paces streamed models' mesh data to the GPU.
//!
//! Bevy writes each new mesh into its GPU buffers in the frame the mesh
//! arrives, on the render thread (about half a millisecond a megabyte here),
//! and a row of sectors streaming in can bring ten megabytes of models in one
//! frame. The model loader takes its meshes' size from a per-frame budget
//! before handing them over (as do the terrain's finer levels of detail), so such a burst spreads over a few frames (tens
//! of milliseconds later) instead of landing in one. Arriving where nothing
//! is in yet (starting out, or fast travel), there is no limit until the
//! sector underfoot is.
//!
//! Bevy's own per-frame limit (`throttle_uploads` in `main.rs`) holds
//! textures back but not mesh data, which its mesh allocator writes before
//! the limit is looked at.
//!
//! FC3_MESH_UPLOAD_MB sets the budget in megabytes a frame (0: no limit).

use std::sync::atomic::{AtomicI64, Ordering};

use bevy::prelude::*;
use event_listener::Event;
use fc3_formats::terrain::SectorCoord;

/// Megabytes of model meshes handed over per frame once the world is in.
const DEFAULT_MB: f32 = 3.0;
/// What "no limit" leaves in the budget.
const UNLIMITED: i64 = i64::MAX / 4;

/// Bytes left this frame (may go below zero: one model larger than what's
/// left still goes, and the overshoot comes out of the next frame).
static BUDGET: AtomicI64 = AtomicI64::new(UNLIMITED);
/// Raised when the budget is refilled.
static REFILLED: Event = Event::new();

pub struct UploadBudgetPlugin;

impl Plugin for UploadBudgetPlugin {
    fn build(&self, app: &mut App) {
        let mb = std::env::var("FC3_MESH_UPLOAD_MB").ok().and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(DEFAULT_MB);
        app.insert_resource(PerFrame(if mb > 0.0 { (mb * (1 << 20) as f32) as i64 } else { UNLIMITED })).add_systems(First, refill);
    }
}

#[derive(Resource)]
struct PerFrame(i64);

fn refill(per_frame: Res<PerFrame>, streamer: Option<Res<crate::objects::ObjectStreamer>>, mut last_in: Local<Option<SectorCoord>>) {
    let paced = match streamer.and_then(|s| s.centre()) {
        Some((c, true)) => {
            *last_in = Some(c);
            true
        }
        // Walking on into a sector not in yet: still paced (it's close).
        Some((c, false)) => last_in.is_some_and(|l| (l.x - c.x).abs() <= 1 && (l.y - c.y).abs() <= 1),
        None => false,
    };
    let left = BUDGET.load(Ordering::Acquire);
    // A frame's overshoot carries over; an unused budget doesn't.
    let budget = if paced { per_frame.0 + left.min(0) } else { UNLIMITED };
    BUDGET.store(budget, Ordering::Release);
    REFILLED.notify(usize::MAX);
}

/// Whether this frame's budget has room left, for meshes made in a system.
pub fn has_room() -> bool {
    BUDGET.load(Ordering::Acquire) > 0
}

/// Takes `bytes` of mesh data made in a system from this frame's budget.
pub fn spend(bytes: usize) {
    BUDGET.fetch_sub(bytes as i64, Ordering::AcqRel);
}

/// Waits until this frame's budget has room for `bytes` more of mesh data.
pub async fn take(bytes: usize) {
    loop {
        let left = BUDGET.load(Ordering::Acquire);
        if left > 0 {
            if BUDGET.compare_exchange_weak(left, left - bytes as i64, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return;
            }
            continue;
        }
        let listener = REFILLED.listen();
        if BUDGET.load(Ordering::Acquire) > 0 {
            continue;
        }
        listener.await;
    }
}
