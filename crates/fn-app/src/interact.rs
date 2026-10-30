//! INTERACT (2026-10-08, shared): the Use-key state every interactable hook reads (doors, pickups, containers...). Generic on purpose: no door logic here.
//! * [`set_script_use`] / [`script_use`]: a scripted run's `key=use` (script.rs `build_key`), set once per frame by app.rs; a hook ORs it with the live key.
//! The prompt itself is `HudWorld::interaction` (`fn_ui::screens::reticle_state::InteractionPrompt`); app.rs fills it from the first hook that has a focus
//! (pickups, then doors, ...), and a hook skips the Use press when an earlier one is focused.
use std::sync::atomic::{AtomicBool, Ordering};

static SCRIPT_USE: AtomicBool = AtomicBool::new(false);
/// SCRIPT: the Use key row is held this frame (`key=use` / `key=e`).
pub fn set_script_use(down: bool) { SCRIPT_USE.store(down, Ordering::Relaxed); }
pub fn script_use() -> bool { SCRIPT_USE.load(Ordering::Relaxed) }
static TAP: AtomicBool = AtomicBool::new(false);
static CURVES: std::sync::Mutex<Vec<(&'static str, f32, f32)>> = std::sync::Mutex::new(Vec::new());
pub fn note_tap() { TAP.store(true, Ordering::Relaxed); }
pub fn take_tap() -> bool { TAP.swap(false, Ordering::Relaxed) }
pub fn set_montage_curves(v: Vec<(&'static str, f32, f32)>) { if let Ok(mut c) = CURVES.lock() { *c = v; } }
pub fn montage_curve(name: &str, pose_value: f32) -> f32 {
    CURVES.lock().ok().and_then(|c| c.iter().find(|(n, _, w)| *n == name && *w > 0.).map(|&(_, sum, w)| pose_value * (1. - w.min(1.)) + sum)).unwrap_or(pose_value)
}
