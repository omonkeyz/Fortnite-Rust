pub mod caps;
pub mod chat;
pub mod interp;
pub mod modes;
pub mod proto;
pub mod session;

pub use session::{parse_control, Control, Event, Session};

pub fn weapon_id(row: &str) -> u8 { caps::WEAPONS.iter().position(|w| w.row == row).map_or(0, |i| i as u8) }
pub const PICKAXE: u8 = 0;

#[cfg(test)]
mod tests {
    #[test] fn weapon_ids_are_stable() {
        assert_eq!(super::weapon_id("Pickaxe"), 0);
        assert_eq!(super::caps::WEAPONS[super::weapon_id("Shotgun_Standard_Athena_C_Ore_T03") as usize].pellets, 10);
        assert_eq!(super::weapon_id("nope"), 0);
    }
}
