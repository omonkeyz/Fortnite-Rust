//! Gun hit registration recovered from the 1.8 data and exe (see `data` for what is read, `spread` for the decoded native firing model with RVAs, `shoot` for the fire
//! function, aim selection and trace applied to the prop world).
pub mod data;
pub mod explosive;
pub mod shoot;
pub mod spread;
pub mod surface;
pub mod tracer;
pub use data::{br_weapon_wids, load, GunHit, StatRow};
pub use shoot::{fire, FireContext, Impact, Policy, Projectile, Shot, ShotReport, Zone};
pub use spread::{DamageKind, RandomStream, SpreadInputs, SpreadState};

#[cfg(test)]
mod tests;
