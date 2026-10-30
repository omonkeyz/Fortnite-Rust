//! Movement stack of the Fortnite 1.8 runtime (UE 4.16). Pure logic: no OS, no renderer, no input devices.
//!
//! * [`params`]     constants for the Athena pawn (recovered / exe / assumed), UE units (cm, s)
//! * [`scene`]      capsule-vs-world collision (baseplate + solids of revolution), plus the player [`scene::Blocker`]
//! * [`cmc`]        the `UCharacterMovementComponent` port (walking, falling, step-up, slopes, perching, crouch, jump)
//! * [`controller`] metres / Y-up [`Controller`] wrapper plus the facing/turn helpers the camera and animation use
//!
//! Old file mapping: movement_cmc.rs -> cmc.rs, movement_scene.rs -> scene.rs, movement_params.rs -> params.rs, movement.rs -> controller.rs,
//! movement_tests.rs -> tests.rs (cfg(test)).
pub mod clamber_root;
pub mod cmc;
pub mod controller;
pub mod enhanced;
pub mod params;
pub mod scene;
pub mod skydive;
#[cfg(test)]
mod tests;

pub use controller::*;
