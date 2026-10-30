//! Per-weapon muzzle data as read from the 1.8 paks (`fn-vfx-export`), JSON `<particles-muzzle>/muzzle.json`.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A socket of the weapon skeletal mesh (or its skeleton asset): `FTransform` of `USkeletalMeshSocket` relative to `bone`, UE units (cm, degrees `FRotator` pitch / yaw / roll).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SocketInfo {
    pub name: String,
    pub bone: String,
    pub location: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    /// `mesh` (`USkeletalMesh::Sockets`) or `skeleton` (`USkeleton::Sockets`).
    pub owner: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WeaponMuzzle {
    /// `WID_*` long package path.
    pub wid: String,
    /// `WeaponActorClass` package path (without the `_C`).
    pub actor_class: String,
    /// The Blueprint chain, child first, up to (and including) `B_Ranged_Generic`.
    pub chain: Vec<String>,
    /// `MuzzleParticleSystem` of the first Blueprint in the chain that sets it (package path); `None` = the Blueprint chain sets none.
    pub muzzle_system: Option<String>,
    /// `MuzzleSocketName`: the value a Blueprint sets, else `Muzzle` (INFERENCE: the native default is in the exe; every BR weapon mesh has a `Muzzle` socket).
    pub muzzle_socket: String,
    /// true when a Blueprint CDO sets `MuzzleSocketName` (not inferred).
    pub socket_from_data: bool,
    /// `bPersistentFireFX` (looping muzzle flash: `Muzzle Flash FX(true)` + the stop timer instead of `Activate` per shot).
    pub persistent_fire: bool,
    /// `WeaponMeshOverride` skeletal mesh package path.
    pub mesh: String,
    /// The muzzle socket of the mesh (mesh sockets first, then the skeleton's), `None` when neither has it.
    pub socket: Option<SocketInfo>,
    /// Every other particle-system reference of the Blueprint CDOs in the chain (`MuzzleFlash(Charged)`, `BeamParticleSystem`, ...): property -> package path.
    pub other_particle_refs: BTreeMap<String, String>,
    /// Numeric CDO properties named `*Light*` (`Muzzle Flash Light Intensity (Charged)` ...).
    pub light_properties: BTreeMap<String, f32>,
    #[serde(default)]
    pub shells: Option<ShellsData>,
    #[serde(default)]
    pub reload_smoke: Option<ReloadSmokeData>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShellsData {
    pub system: String,
    pub socket_name: String,
    pub socket: Option<SocketInfo>,
    pub on_fire: bool,
    pub on_reload: bool,
    pub on_pump: bool,
    pub shell_type: u8,
    pub spawn_rate_scale: f32,
    pub rotation_rate: [f32; 3],
    pub velocity: [f32; 3],
    pub gravity: [f32; 3],
    pub lifetime: f32,
    pub size: [f32; 3],
    pub inherit_parent_velocity: f32,
    pub cylinder_radius: f32,
    pub cylinder_height: f32,
    pub time_dilation: f32,
    pub pump_notify_times: Vec<f32>,
    #[serde(default)]
    pub reload_shell_delay: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReloadSmokeData {
    pub system: String,
    pub socket_name: String,
    pub socket: Option<SocketInfo>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MuzzleTable {
    pub schema: u32,
    pub notes: Vec<String>,
    /// by `WID_*` long package path.
    pub weapons: BTreeMap<String, WeaponMuzzle>,
}

impl MuzzleTable {
    pub fn load(path: &Path) -> Result<MuzzleTable, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
    }
    /// A weapon by `WID_*` package path (`/Game/Athena/Items/Weapons/WID_X`, with or without `.WID_X`) or short name.
    pub fn weapon(&self, wid: &str) -> Option<&WeaponMuzzle> {
        let pkg = wid.split('.').next().unwrap_or(wid);
        self.weapons.get(pkg).or_else(|| { let short = pkg.rsplit('/').next().unwrap_or(pkg); self.weapons.values().find(|w| w.wid.rsplit('/').next() == Some(short)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn weapon_lookup_accepts_package_object_and_short_names() {
        let mut t = MuzzleTable::default();
        t.weapons.insert("/Game/Athena/Items/Weapons/WID_A".into(), WeaponMuzzle { wid: "/Game/Athena/Items/Weapons/WID_A".into(), ..Default::default() });
        assert!(t.weapon("/Game/Athena/Items/Weapons/WID_A").is_some());
        assert!(t.weapon("/Game/Athena/Items/Weapons/WID_A.WID_A").is_some());
        assert!(t.weapon("WID_A").is_some());
        assert!(t.weapon("WID_B").is_none());
    }
}
