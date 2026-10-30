//! # fn-vfx
//!
//! The muzzle flash of the Fortnite 1.8 Battle Royale weapons.
//!
//! * [`table`]: what the 1.8 weapon Blueprints say (`WID_*` -> `WeaponActorClass` -> `B_*_Athena_*` chain -> `MuzzleParticleSystem`, `MuzzleSocketName`, `bPersistentFireFX`, ...),
//!   written by `fn-vfx-export` (feature `tools`) to `<particles-muzzle>/muzzle.json`.
//! * [`flash`]: the `Muzzle(Empty)` `UParticleSystemComponent` of `B_Ranged_Generic` (recovered from its Kismet bytecode): `Activate(true)` per shot, the random relative scale,
//!   the scope rule, `Deactivate()` on stop; the Cascade simulation itself is `fn-particles`.
//! * [`socket`]: the weapon mesh socket -> emitter transform (UE space, `fn_particles::Transform`).
//!
//! What is NOT here: the native call that decides WHEN `OnPlayWeaponFireFX` runs (and with which `bPersistentFire`) lives in the exe (`AFortWeapon::PlayWeaponFireFX`); the
//! host calls [`flash::MuzzleFlash::on_play_weapon_fire_fx`] where the shot is fired.
//!
//! Pending exe items (not recoverable from the packages): the call site / condition of `OnPlayWeaponFireFX` and its `bPersistentFire` argument, the native `IsUsingScope()`,
//! the native default of `MuzzleSocketName` (`Muzzle` is inferred), the `RarityColor` instance parameter some muzzle systems read (`Constant` of the parameter distribution is used),
//! the dynamic `ParticleModuleLight` rendering (decoded + simulated, `Instance::lights`; the in-game renderer has no point lights yet).
pub mod flash;
pub mod shells;
pub mod socket;
pub mod table;

pub use flash::{FireContext, MuzzleFlash};
pub use shells::{ReloadSmoke, ShellEject};
pub use table::{MuzzleTable, ReloadSmokeData, ShellsData, SocketInfo, WeaponMuzzle};
