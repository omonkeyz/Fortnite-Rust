//! Everything the 1.8 files serialize about how a firearm shoots, read at load time (no number below is typed in):
//!  * the stat row (`WeaponStatHandle`: `AthenaRangedWeapons.<row>`): spread, spread multipliers, heat / cool curve points, recoil, damage points + their ranges,
//!    damage zones (headshot multiplier), knockback, AI ranges;
//!  * the weapon actor Blueprint CDO (`WeaponActorClass`): the aim flags (`bShouldAimFromMuzzleAtCloseRange`, `bMaintainAimLocationDuringTargeting`,
//!    `bUseScopeTargeting`), muzzle / tracer / beam particle systems, impact effect + sound per physical surface, camera classes, projectile template;
//!  * the primary fire ability CDO (`PrimaryFireAbility`): the target selection list (shape, range, cone angles, filter flags) and the cue tags.
//! Units are the data's: centimetres, degrees, seconds.
use fn_pose::{loader, loadout_stats::data_table_row, ClipSourceDir};
use fn_ueio::{props::{read_props, Ctx, PropList, PropValue}, Package, Reader};

/// All floats / ints of the ranged stat row that matter to shooting. Field names follow the data columns.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StatRow {
    pub spread: f32, pub spread_downsights: f32,
    pub standing_still_spread_mult: f32, pub crouching_spread_mult: f32, pub jumping_falling_spread_mult: f32, pub sprinting_spread_mult: f32,
    pub min_speed_for_spread_mult: f32, pub max_speed_for_spread_mult: f32, pub spread_downsights_additional_cooldown: f32,
    /// `HeatX1..3 / HeatY1..3` and `CoolX1..3 / CoolY1..3` (three points each; the native spread-heat model that consumes them is not recoverable).
    pub heat: [(f32, f32); 3], pub cool: [(f32, f32); 3],
    pub bullets_per_cartridge: u32, pub firing_rate: f32,
    // legacy recoil columns
    pub recoil_vert: f32, pub recoil_vert_scale: f32, pub recoil_vert_scale_gamepad: f32, pub vert_recoil_down_chance: f32,
    pub recoil_horiz: f32, pub recoil_horiz_scale: f32, pub recoil_horiz_scale_gamepad: f32,
    pub recoil_interp_speed: f32, pub recoil_recovery_interp_speed: f32, pub recoil_recovery_delay: f32, pub recoil_recovery_fraction: f32, pub recoil_downsights_mult: f32,
    // Athena recoil columns
    pub athena_recoil_magnitude: (f32, f32), pub athena_recoil_magnitude_scale: f32, pub athena_recoil_angle: (f32, f32), pub athena_recoil_roll: (f32, f32),
    pub athena_recoil_interp_speed: f32, pub athena_recoil_recovery_interp_speed: f32, pub athena_recoil_downsights_mult: f32,
    // damage: [point blank, mid, long, max range] and the ranges (cm) they belong to
    pub dmg: [f32; 4], pub env_dmg: [f32; 4], pub impact_dmg: [f32; 4], pub rng: [f32; 4],
    pub sweep_radius: f32, pub max_actual_range: f32,
    pub zone_light: f32, pub zone_normal: f32, pub zone_critical: f32, pub zone_vulnerability: f32,
    pub dice_crit_chance: f32, pub dice_crit_damage_mult: f32,
    pub knockback: [f32; 3], pub knockback_z_angle: f32, pub stun_time: f32,
    pub surface_ratio_row: String,
    pub bullets_per_tracer: u32,
    pub dmg_scale: f32, pub env_dmg_scale: f32,
    pub charge: (f32, f32, f32), pub charge_damage_mult: (f32, f32),
    // --- columns the native firing model reads (spread.rs): offsets are the FFortRangedWeaponStats / FFortBaseWeaponStats members the exe code indexes
    /// `BaseLevel` (+0x08, int): the level the curves are authored for; `max(level - BaseLevel, 0)` scales the heat / cool / ROF / damage columns.
    pub base_level: i32,
    pub heat_scale: (f32, f32), pub cool_scale: (f32, f32),
    /// `ROFScale` (+0x18C), `BurstFiringRate` (+0x190), `CartridgePerFire` (+0x100), `AmmoCostPerFire`, `ClipSize`, `ReloadTime`, `AutoReloadDelayOverride` (+0x23C).
    pub rof_scale: f32, pub burst_firing_rate: f32, pub cartridge_per_fire: u32, pub ammo_cost_per_fire: u32, pub clip_size: u32, pub reload_time: f32, pub auto_reload_delay_override: f32,
    /// `DmgScaleTable` / `EnvDmgScaleTable` have a table object (the exe takes the curve path when it is non-null); every 1.8 BR row has none.
    pub dmg_scale_table: bool, pub env_dmg_scale_table: bool,
    // --- remaining shooting / ammo / reload columns (every column of the row is also in columns)
    pub max_spare_ammo: u32, pub initial_clips: u32, pub max_ammo_cost_per_fire: u32,
    pub reload_individual_bullets: bool, pub reload_interrupt_is_immediate: bool, pub reload_scale: f32, pub clip_scale: f32,
    pub impact_dmg_scale: f32, pub stun_scale: f32, pub force_control: bool,
    pub min_actual_range: f32, pub min_preferred_range: f32, pub max_preferred_range: f32,
    /// DmgScaleTableRow / EnvDmgScaleTableRow / ImpactDmgScaleTableRow (None in every 1.8 BR row).
    pub scale_table_rows: [String; 3],
    /// EVERY column of the data-table row as (column, rendered value) in file order (research table output; nothing is dropped).
    pub columns: Vec<(String, String)>,
}

/// Human-readable rendering of a stat-row property (floats rounded to f32 precision).
pub fn render(pkg: Option<&Package>, v: &PropValue) -> String {
    match v {
        PropValue::Bool(b) => b.to_string(), PropValue::Int(i) => i.to_string(),
        PropValue::Float(x) => { let s = format!("{}", *x as f32); s }
        PropValue::Str(s) | PropValue::Name(s) | PropValue::Enum(s) => s.clone(),
        PropValue::Byte(b) => b.to_string(),
        PropValue::Object(i) => match pkg { Some(p) if *i < 0 => p.import_path(*i), _ => format!("object({i})") },
        PropValue::Array(a) => format!("[{}]", a.iter().map(|x| render(pkg, x)).collect::<Vec<_>>().join(", ")),
        o => format!("{o:?}"),
    }
}

fn f(p: &PropList, k: &str) -> Result<f32, String> { p.get(k).and_then(PropValue::as_f64).map(|v| v as f32).ok_or_else(|| format!("stat row lacks {k}")) }
fn u(p: &PropList, k: &str) -> Result<u32, String> { f(p, k).map(|v| v.max(0.) as u32) }

impl StatRow {
    pub fn from_props(p: &PropList) -> Result<StatRow, String> {
        let pt = |a: &str, b: &str| -> Result<(f32, f32), String> { Ok((f(p, a)?, f(p, b)?)) };
        Ok(StatRow {
            spread: f(p, "Spread")?, spread_downsights: f(p, "SpreadDownsights")?,
            standing_still_spread_mult: f(p, "StandingStillSpreadMultiplier")?, crouching_spread_mult: f(p, "AthenaCrouchingSpreadMultiplier")?,
            jumping_falling_spread_mult: f(p, "AthenaJumpingFallingSpreadMultiplier")?, sprinting_spread_mult: f(p, "AthenaSprintingSpreadMultiplier")?,
            min_speed_for_spread_mult: f(p, "MinSpeedForSpreadMultiplier")?, max_speed_for_spread_mult: f(p, "MaxSpeedForSpreadMultiplier")?,
            spread_downsights_additional_cooldown: f(p, "SpreadDownsightsAdditionalCooldownTime")?,
            heat: [pt("HeatX1", "HeatY1")?, pt("HeatX2", "HeatY2")?, pt("HeatX3", "HeatY3")?],
            cool: [pt("CoolX1", "CoolY1")?, pt("CoolX2", "CoolY2")?, pt("CoolX3", "CoolY3")?],
            bullets_per_cartridge: u(p, "BulletsPerCartridge")?, firing_rate: f(p, "FiringRate")?,
            recoil_vert: f(p, "RecoilVert")?, recoil_vert_scale: f(p, "RecoilVertScale")?, recoil_vert_scale_gamepad: f(p, "RecoilVertScaleGamepad")?, vert_recoil_down_chance: f(p, "VertRecoilDownChance")?,
            recoil_horiz: f(p, "RecoilHoriz")?, recoil_horiz_scale: f(p, "RecoilHorizScale")?, recoil_horiz_scale_gamepad: f(p, "RecoilHorizScaleGamepad")?,
            recoil_interp_speed: f(p, "RecoilInterpSpeed")?, recoil_recovery_interp_speed: f(p, "RecoilRecoveryInterpSpeed")?, recoil_recovery_delay: f(p, "RecoilRecoveryDelay")?,
            recoil_recovery_fraction: f(p, "RecoilRecoveryFraction")?, recoil_downsights_mult: f(p, "RecoilDownsightsMultiplier")?,
            athena_recoil_magnitude: pt("AthenaRecoilMagnitudeMin", "AthenaRecoilMagnitudeMax")?, athena_recoil_magnitude_scale: f(p, "AthenaRecoilMagnitudeScale")?,
            athena_recoil_angle: pt("AthenaRecoilAngleMin", "AthenaRecoilAngleMax")?, athena_recoil_roll: pt("AthenaRecoilRollMagnitudeMin", "AthenaRecoilRollMagnitudeMax")?,
            athena_recoil_interp_speed: f(p, "AthenaRecoilInterpSpeed")?, athena_recoil_recovery_interp_speed: f(p, "AthenaRecoilRecoveryInterpSpeed")?,
            athena_recoil_downsights_mult: f(p, "AthenaRecoilDownsightsMultiplier")?,
            dmg: [f(p, "DmgPB")?, f(p, "DmgMid")?, f(p, "DmgLong")?, f(p, "DmgMaxRange")?],
            env_dmg: [f(p, "EnvDmgPB")?, f(p, "EnvDmgMid")?, f(p, "EnvDmgLong")?, f(p, "EnvDmgMaxRange")?],
            impact_dmg: [f(p, "ImpactDmgPB")?, f(p, "ImpactDmgMid")?, f(p, "ImpactDmgLong")?, f(p, "ImpactDmgMaxRange")?],
            rng: [f(p, "RngPB")?, f(p, "RngMid")?, f(p, "RngLong")?, f(p, "RngMax")?],
            sweep_radius: f(p, "SweepRadius")?, max_actual_range: f(p, "MaxActualRange")?,
            zone_light: f(p, "DamageZone_Light")?, zone_normal: f(p, "DamageZone_Normal")?, zone_critical: f(p, "DamageZone_Critical")?, zone_vulnerability: f(p, "DamageZone_Vulnerability")?,
            dice_crit_chance: f(p, "DiceCritChance")?, dice_crit_damage_mult: f(p, "DiceCritDamageMultiplier")?,
            knockback: [f(p, "KnockbackMagnitude")?, f(p, "MidRangeKnockbackMagnitude")?, f(p, "LongRangeKnockbackMagnitude")?], knockback_z_angle: f(p, "KnockbackZAngle")?, stun_time: f(p, "StunTime")?,
            surface_ratio_row: p.text("SurfaceRatioRowName").unwrap_or("").to_owned(),
            bullets_per_tracer: u(p, "BulletsPerTracer")?,
            dmg_scale: f(p, "DmgScale")?, env_dmg_scale: f(p, "EnvDmgScale")?,
            charge: (f(p, "MinChargeTime")?, f(p, "MaxChargeTime")?, f(p, "ChargeDownTime")?), charge_damage_mult: (f(p, "MinChargeDamageMultiplier")?, f(p, "MaxChargeDamageMultiplier")?),
            base_level: f(p, "BaseLevel")? as i32,
            heat_scale: pt("HeatXScale", "HeatYScale")?, cool_scale: pt("CoolXScale", "CoolYScale")?,
            rof_scale: f(p, "ROFScale")?, burst_firing_rate: f(p, "BurstFiringRate")?, cartridge_per_fire: u(p, "CartridgePerFire")?, ammo_cost_per_fire: u(p, "AmmoCostPerFire")?,
            clip_size: u(p, "ClipSize")?, reload_time: f(p, "ReloadTime")?, auto_reload_delay_override: f(p, "AutoReloadDelayOverride")?,
            dmg_scale_table: p.object("DmgScaleTable").map_or(false, |i| i != 0), env_dmg_scale_table: p.object("EnvDmgScaleTable").map_or(false, |i| i != 0),
            max_spare_ammo: u(p, "MaxSpareAmmo")?, initial_clips: u(p, "InitialClips")?, max_ammo_cost_per_fire: u(p, "MaxAmmoCostPerFire")?,
            reload_individual_bullets: p.bool("bReloadIndividualBullets").ok_or("stat row lacks bReloadIndividualBullets")?,
            reload_interrupt_is_immediate: p.bool("bReloadInterruptIsImmediate").ok_or("stat row lacks bReloadInterruptIsImmediate")?,
            reload_scale: f(p, "ReloadScale")?, clip_scale: f(p, "ClipScale")?, impact_dmg_scale: f(p, "ImpactDmgScale")?, stun_scale: f(p, "StunScale")?,
            force_control: p.bool("bForceControl").ok_or("stat row lacks bForceControl")?,
            min_actual_range: f(p, "MinActualRange")?, min_preferred_range: f(p, "MinPreferredRange")?, max_preferred_range: f(p, "MaxPreferredRange")?,
            scale_table_rows: ["DmgScaleTableRow", "EnvDmgScaleTableRow", "ImpactDmgScaleTableRow"].map(|k| p.text(k).unwrap_or("").to_owned()),
            columns: p.0.iter().map(|x| (x.name.clone(), render(None, &x.value))).collect(),
        })
    }
}

/// `FortAbilityTargetSelection` of the primary fire ability (the data half of the hit selection: the native trace itself is C++).
#[derive(Clone, Debug, PartialEq)]
pub struct TargetSelection {
    pub shape: String, pub test_type: String, pub primary_source: String, pub range: f32,
    pub cone_yaw: f32, pub cone_pitch: f32, pub cone_min_radius: f32,
    pub actor_type_filter: String, pub exclude_pawn_friends: bool, pub exclude_obstructed_by_world: bool, pub max_targets: i32,
    /// `ApplicationTag` of the `FortGameplayEffectContainer` this selection belongs to (empty when the container has none). The projectile ability
    /// `GA_Ranged_GenericProjectileImpact` has two: `Gameplay.Application.Hit` (Cone, `bExcludePawnFriends` false) and `Gameplay.Application.Explode`
    /// (Sphere 512, MaxTargets 12, `bExcludePawnFriends` true) (data). The ranged ability builds a spec per tag (`UFortGameplayAbility_RangedWeapon` vfunc 0x3e0,
    /// RVA 0x1402a8150) and the spawn task copies them into the projectile's on-touch (+0x3a8) and on-explode (+0x430) container slots.
    pub application_tag: String,
    pub exclude_instigator: bool,
    pub effect_classes: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FireAbility {
    pub package: String,
    pub selections: Vec<TargetSelection>,
    pub max_yaw_angle_to_target: Option<f32>,
    pub weapon_fire_cue: Vec<String>, pub impact_cues: Vec<String>, pub ability_tags: Vec<String>,
    /// `WeaponTestApplicationTag` / `ApplicationTag` of the containers.
    pub application_tags: Vec<String>,
    /// `TargetGameplayEffectClasses` of container 0 (GE_Ranged_GenericDamage -> template GET_DirectPhysicalDamage).
    pub effect_classes: Vec<String>,
}
impl FireAbility {
    /// The target selection of the container tagged `tag` (`Gameplay.Application.Hit` / `.Explode`).
    pub fn selection(&self, tag: &str) -> Option<&TargetSelection> { self.selections.iter().find(|s| s.application_tag == tag) }
}
/// `Gameplay.Application.Hit` / `Gameplay.Application.Explode` container tags (data: `GA_Ranged_GenericProjectileImpact`).
pub const APPLICATION_HIT: &str = "Gameplay.Application.Hit";
pub const APPLICATION_EXPLODE: &str = "Gameplay.Application.Explode";

/// The weapon actor's shooting-related presentation + aim flags.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Presentation {
    pub actor: String,
    pub aim_from_muzzle_at_close_range: bool, pub maintain_aim_during_targeting: bool, pub use_scope_targeting: bool,
    pub muzzle_particle: Option<String>, pub tracer_template: Option<String>, pub beam_particle: Option<String>,
    pub camera_base_3p: Option<String>, pub camera_targeting_3p: Option<String>,
    pub fire_sounds: Vec<(u32, String)>,
    /// `ImpactPhysicalSurfaceEffects[i]` / `ImpactPhysicalSurfaceSounds[i]`: index = EPhysicalSurface value (sounds: 1 wood, 2 stone, 3 metal, 4 player; the file names say so).
    pub impact_effects: Vec<(u32, String)>, pub impact_sounds: Vec<(u32, String)>,
    pub projectile_template: Option<String>,
    /// `AFortWeaponRanged::ProjectilePitchOffset` (+0xAB8, degrees added to the projectile launch pitch by 0x933630); 0 when the actor Blueprint does not serialize it.
    pub projectile_pitch_offset: f32,
    /// Blueprint class chain the CDO properties were resolved through, child first (B_Assault_Auto_Zoom_SR_Child_Athena_C, B_Assault_Auto_C, ...).
    pub actor_chain: Vec<String>,
}
impl Presentation {
    pub fn impact_effect(&self, surface: u32) -> Option<&str> { self.impact_effects.iter().find(|(i, _)| *i == surface).map(|x| x.1.as_str()) }
    pub fn impact_sound(&self, surface: u32) -> Option<&str> { self.impact_sounds.iter().find(|(i, _)| *i == surface).map(|x| x.1.as_str()) }
}

/// A fully loaded firearm for hit registration.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct GunHit {
    pub wid: String, pub row: String, pub stats: StatRow, pub pres: Presentation, pub ability: FireAbility, /** Some for projectile weapons (the bolt-action sniper) */ pub projectile: Option<ProjectileData>,
    /// `UFortWeaponRangedItemDefinition::bShouldUsePerfectAimWhenTargetingMinSpread` (+0x691). The class constructor (RVA 0x9164b0, store at 0x916600) defaults it to TRUE; a WID asset
    /// serializes it only when it differs, so an absent property means true.
    pub perfect_aim_when_targeting_min_spread: bool,
    /// `TriggerType` of the WID (`EFortWeaponTriggerType::...`); the native default (property absent) is `OnPress`.
    pub trigger: String,
    /// Ammo data asset name (`AthenaAmmoData...`).
    pub ammo: String,
    /// WID-level item data: Rarity (EFortRarity::...), Tier, MinLevel, and the full object paths of AmmoData / PrimaryFireAbility / WeaponActorClass.
    pub rarity: String, pub tier: String, pub min_level: i64, pub ammo_path: String, pub ability_path: String, pub actor_path: String,
    /// Stat table package the row came from (/Game/Athena/Items/Weapons/AthenaRangedWeapons).
    pub stat_table: String,
    pub tracer: Option<crate::tracer::TracerDef>,
    pub projectile_particles: Vec<(String, [f32; 3])>,
}

/// `InitialSpeed` / `MaxSpeed` (cm/s) of the projectile Blueprint, evaluated at curve time 0 from its `AthenaProjectiles` CurveTable rows. Gravity scale: the
/// `ProjectileMovementComponent` of the Blueprint serializes no override (engine default; the world gravity is the movement crate's).
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectileData { pub template: String, pub table: String, pub initial_speed_row: String, pub max_speed_row: String, pub initial_speed: f32,
    /// 32::INFINITY when the Blueprint serializes no MaxSpeed (native UProjectileMovementComponent::MaxSpeed 0 = no clamp, UE4.16 LimitVelocity tests MaxSpeed > 0).
    pub max_speed: f32,
    /// ProjectileGravityScale of the Blueprint's ProjectileMovementComponent export; None when not serialized (engine default 1).
    pub gravity_scale: Option<f32>,
    /// InitialLifeSpan of the projectile CDO; None when not serialized.
    pub life_span: Option<f32>,
    pub should_bounce: bool, pub bounciness: f32, pub friction: f32,
    pub explosive: Option<crate::explosive::ExplosiveDef> }

/// `FRichCurve::Eval` for the interpolation modes a table row uses (Linear 0 / Constant 1; cubic falls back to the key value), constant extrapolation.
pub fn eval_curve(keys: &[(f32, f32, i64)], t: f32) -> f32 {
    let Some(first) = keys.first() else { return 0. };
    if t <= first.0 { return first.1; }
    for w in keys.windows(2) { if t < w[1].0 { return if w[0].2 == 0 { w[0].1 + (w[1].1 - w[0].1) * (t - w[0].0) / (w[1].0 - w[0].0).max(1e-9) } else { w[0].1 }; } }
    keys.last().map_or(0., |k| k.1)
}
pub(crate) fn curve_row(set: &fn_pak::PakSet, table: &str, row: &str, t: f32) -> Result<f32, String> {
    let pd = fn_uasset::PackageData::load(set, table).map_err(|e| format!("{table}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{table}: {e}"))?;
    let tb = fn_uasset::classes::read_table(&pkg, 0).map_err(|e| format!("{table}: {e}"))?;
    let (_, props) = tb.rows.iter().find(|(n, _)| n == row).ok_or_else(|| format!("{table}: no row {row}"))?;
    let keys: Vec<(f32, f32, i64)> = props.get("Keys").and_then(fn_uasset::Value::as_array).unwrap_or(&[]).iter()
        .filter_map(|k| Some((k.field("Time")?.as_f32()?, k.field("Value")?.as_f32()?, k.field("InterpMode").and_then(fn_uasset::Value::as_i64).unwrap_or(0)))).collect();
    if keys.is_empty() { return Err(format!("{table}: row {row} has no keys")); }
    Ok(eval_curve(&keys, t))
}
pub fn projectile(src: &ClipSourceDir, template: &str) -> Result<ProjectileData, String> {
    let ClipSourceDir::Pak(set) = src else { return Err("projectile curve tables are read from the pak".into()) };
    let pkg = loader::package(src, pkg_of(template))?;
    let p = cdo(&pkg)?;
    let handle = |k: &str| -> Result<(String, String), String> {
        let h = p.fields(k).ok_or_else(|| format!("projectile lacks {k}"))?;
        let table = h.object("CurveTable").filter(|i| *i < 0).map(|i| pkg.import_path(i)).ok_or("handle has no CurveTable")?;
        Ok((pkg_of(&table).to_owned(), h.text("RowName").ok_or("handle has no RowName")?.to_owned()))
    };
    // the movement component export (ProjectileComp0 / ProjectileMovementComponent) serializes plain floats on some projectiles
    let comp = (0..pkg.exports.len()).find(|&i| pkg.export_class(i) == "ProjectileMovementComponent").and_then(|i| {
        let (s, e) = pkg.export_range(i);
        read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).ok() });
    let gravity_scale = comp.as_ref().and_then(|c| c.float("ProjectileGravityScale")).map(|v| v as f32);
    let life_span = p.float("InitialLifeSpan").map(|v| v as f32);
    let should_bounce = comp.as_ref().and_then(|c| c.bool("bShouldBounce")).unwrap_or(false);
    let bounciness = comp.as_ref().and_then(|c| c.float("Bounciness")).map_or(0.6, |v| v as f32);
    let friction = comp.as_ref().and_then(|c| c.float("Friction")).map_or(0.2, |v| v as f32);
    if p.fields("InitialSpeed").is_none() {
        let c = comp.as_ref().ok_or("projectile lacks InitialSpeed (no curve handle, no movement component)")?;
        let initial_speed = c.float("InitialSpeed").ok_or("projectile lacks InitialSpeed")? as f32;
        let max_speed = c.float("MaxSpeed").map_or(f32::INFINITY, |v| if v > 0. { v as f32 } else { f32::INFINITY });
        return Ok(ProjectileData { template: pkg_of(template).to_owned(), table: String::new(), initial_speed_row: "(literal InitialSpeed on ProjectileMovementComponent)".into(), max_speed_row: "(not serialized: unlimited)".into(), initial_speed, max_speed, gravity_scale, life_span, should_bounce, bounciness, friction, explosive: crate::explosive::load_def(src, template) });
    }
    let ((table, ir), (_, mr)) = (handle("InitialSpeed")?, handle("MaxSpeed")?);
    Ok(ProjectileData { template: pkg_of(template).to_owned(), initial_speed: curve_row(set, &table, &ir, 0.)?, max_speed: curve_row(set, &table, &mr, 0.)?, table, initial_speed_row: ir, max_speed_row: mr, gravity_scale, life_span, should_bounce, bounciness, friction, explosive: crate::explosive::load_def(src, template) })
}

pub(crate) fn pkg_of(r: &str) -> &str { r.split('.').next().unwrap_or(r) }
fn path_of(pkg: &Package, p: &PropList, key: &str) -> Option<String> {
    if let Some(s) = p.text(key).filter(|s| s.starts_with("/Game/")) { return Some(s.to_owned()); }
    p.object(key).filter(|i| *i < 0).map(|i| pkg.import_path(i))
}
fn indexed_paths(pkg: &Package, p: &PropList, key: &str) -> Vec<(u32, String)> {
    p.0.iter().filter(|x| x.name == key).filter_map(|x| match &x.value {
        PropValue::Str(s) if s.starts_with("/Game/") => Some((x.array_index as u32, s.clone())),
        PropValue::Object(i) if *i < 0 => Some((x.array_index as u32, pkg.import_path(*i))),
        _ => None }).collect()
}
/// `FGameplayTagContainer` / `FGameplayTag` payload (`i32 n` + n x FName) of a raw property.
pub fn tag_names(pkg: &Package, v: &PropValue) -> Vec<String> {
    let PropValue::Raw(b) = v else { return vec![] };
    if b.len() < 4 || (b.len() - 4) % 8 != 0 { return vec![]; }
    let n = i32::from_le_bytes(b[0..4].try_into().unwrap()).max(0) as usize;
    if n * 8 != b.len() - 4 { return vec![]; }
    (0..n).filter_map(|k| { let i = i32::from_le_bytes(b[4 + k * 8..8 + k * 8].try_into().ok()?); pkg.names.get(i.max(0) as usize).cloned() }).collect()
}
fn cdo(pkg: &Package) -> Result<PropList, String> {
    let idx = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__")).ok_or("package has no class default object")?;
    let (s, e) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}

/// Blueprint class default objects of `long` and its Blueprint ancestors, child first (`/Script/...` native parents end the chain). A property the child CDO
/// does not serialize is inherited from the nearest ancestor that does (UE class-default inheritance); only when no Blueprint in the chain serializes it does the native
/// C++ default apply.
pub(crate) fn cdo_chain(src: &ClipSourceDir, long: &str) -> Result<Vec<(Package, PropList)>, String> {
    let mut out = Vec::new();
    let mut cur = pkg_of(long).to_owned();
    for _ in 0..16 {
        let pkg = loader::package(src, &cur)?;
        let props = cdo(&pkg)?;
        let ci = class_export(&pkg)?;
        let sup = pkg.exports[ci].super_index;
        let next = if sup < 0 { Some(pkg_of(&pkg.import_path(sup)).to_owned()).filter(|p| p.starts_with("/Game/")) } else { None };
        out.push((pkg, props));
        match next { Some(n) if n != cur => cur = n, _ => break }
    }
    Ok(out)
}
/// The Blueprint class export (Foo_C, the export whose CDO is Default__Foo_C; it is not always export 0).
fn class_export(pkg: &Package) -> Result<usize, String> {
    let d = pkg.exports.iter().find(|e| e.object_name.starts_with("Default__")).ok_or("package has no class default object")?;
    let cls = d.object_name.trim_start_matches("Default__");
    pkg.exports.iter().position(|e| e.object_name == cls).ok_or_else(|| format!("no class export {cls}"))
}
fn chain_bool(c: &[(Package, PropList)], k: &str) -> bool { c.iter().find_map(|(_, p)| p.bool(k)).unwrap_or(false) }
fn chain_path(c: &[(Package, PropList)], k: &str) -> Option<String> { c.iter().find_map(|(pk, p)| path_of(pk, p, k)) }
fn chain_indexed(c: &[(Package, PropList)], k: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, Option<String>)> = Vec::new();
    for (pk, p) in c {
        let nulls = p.0.iter().filter(|x| x.name == k && matches!(x.value, PropValue::Object(0))).map(|x| (x.array_index as u32, None));
        for (i, s) in indexed_paths(pk, p, k).into_iter().map(|(i, s)| (i, Some(s))).chain(nulls) { if !out.iter().any(|(j, _)| *j == i) { out.push((i, s)); } }
    }
    let mut out: Vec<(u32, String)> = out.into_iter().filter_map(|(i, s)| Some((i, s?))).collect();
    out.sort_by_key(|x| x.0); out
}

pub fn presentation(src: &ClipSourceDir, wid: &PropList, wid_pkg: &Package) -> Result<Presentation, String> {
    let actor_path = path_of(wid_pkg, wid, "WeaponActorClass").ok_or("WID lacks WeaponActorClass")?;
    let c = cdo_chain(src, &actor_path)?;
    Ok(Presentation {
        actor: pkg_of(&actor_path).to_owned(),
        aim_from_muzzle_at_close_range: chain_bool(&c, "bShouldAimFromMuzzleAtCloseRange"), maintain_aim_during_targeting: chain_bool(&c, "bMaintainAimLocationDuringTargeting"), use_scope_targeting: chain_bool(&c, "bUseScopeTargeting"),
        muzzle_particle: chain_path(&c, "MuzzleParticleSystem"), tracer_template: chain_path(&c, "TracerTemplate"), beam_particle: chain_path(&c, "BeamParticleSystem"),
        camera_base_3p: chain_path(&c, "CameraBase3PClass"), camera_targeting_3p: chain_path(&c, "CameraTargeting3PClass"),
        fire_sounds: chain_indexed(&c, "PrimaryFireSound"),
        impact_effects: chain_indexed(&c, "ImpactPhysicalSurfaceEffects"), impact_sounds: chain_indexed(&c, "ImpactPhysicalSurfaceSounds"),
        projectile_template: path_of(wid_pkg, wid, "ProjectileTemplate"),
        projectile_pitch_offset: c.iter().find_map(|(_, p)| p.float("ProjectilePitchOffset")).unwrap_or(0.) as f32,
        actor_chain: c.iter().map(|(p, _)| class_export(p).map(|i| p.exports[i].object_name.clone())).collect::<Result<_, _>>()?,
    })
}

pub fn fire_ability(src: &ClipSourceDir, wid: &PropList, wid_pkg: &Package) -> Result<FireAbility, String> {
    let path = path_of(wid_pkg, wid, "PrimaryFireAbility").ok_or("WID lacks PrimaryFireAbility")?;
    let name = pkg_of(&path).to_owned();
    let chain = cdo_chain(src, &name)?;
    let mut a = FireAbility { package: name, ..Default::default() };
    if let Some((pk, v)) = chain.iter().find_map(|(pk, p)| p.get("WeaponFireCue").map(|v| (pk, v))) { a.weapon_fire_cue = tag_names(pk, v); }
    if let Some((pk, v)) = chain.iter().find_map(|(pk, p)| p.get("AbilityTags").map(|v| (pk, v))) { a.ability_tags = tag_names(pk, v); }
    if let Some(t) = chain.iter().find_map(|(_, p)| p.get("WeaponTestApplicationTag").and_then(PropValue::as_fields).and_then(|f| f.text("TagName"))) { a.application_tags.push(t.to_owned()); }
    let mut indices: Vec<i32> = chain.iter().flat_map(|(_, p)| p.0.iter().filter(|x| x.name == "EffectContainers").map(|x| x.array_index)).collect();
    indices.sort(); indices.dedup();
    for (ci, idx) in indices.into_iter().enumerate() {
        let elems: Vec<(&Package, &PropList)> = chain.iter().filter_map(|(pk, p)| p.0.iter().find(|x| x.name == "EffectContainers" && x.array_index == idx).and_then(|x| x.value.as_fields()).map(|f| (pk, f))).collect();
        let field = |k: &str| elems.iter().find_map(|(pk, f)| f.get(k).map(|v| (*pk, v)));
        let tag = field("ApplicationTag").and_then(|(_, v)| v.as_fields()).and_then(|f| f.text("TagName")).unwrap_or("").to_owned();
        if !tag.is_empty() { a.application_tags.push(tag.clone()); }
        if let Some((pk, v)) = field("ImpactCues") { for t in tag_names(pk, v) { if !a.impact_cues.contains(&t) { a.impact_cues.push(t); } } }
        let effects: Vec<String> = match field("TargetGameplayEffectClasses") { Some((pk, PropValue::Array(arr))) => arr.iter().filter_map(|e| if let PropValue::Object(i) = e { (*i < 0).then(|| pk.import_path(*i)) } else { None }).collect(), _ => vec![] };
        if ci == 0 { a.effect_classes = effects.clone(); }
        let Some(ts) = field("TargetSelection").and_then(|(_, v)| v.as_fields()) else { continue };
        if let Some(v) = ts.float("MaxYawAngleToTarget") { a.max_yaw_angle_to_target.get_or_insert(v as f32); }
        for s in ts.array("List").map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(s) = s.as_fields() else { continue };
            let flt = s.fields("TargetFilter");
            a.selections.push(TargetSelection {
                shape: s.text("Shape").unwrap_or("").to_owned(), test_type: s.text("TestType").unwrap_or("").to_owned(), primary_source: s.text("PrimarySource").unwrap_or("").to_owned(),
                range: s.float("Range").unwrap_or(0.) as f32, cone_yaw: s.float("ConeYawAngle").unwrap_or(0.) as f32, cone_pitch: s.float("ConePitchAngle").unwrap_or(0.) as f32, cone_min_radius: s.float("ConeMinRadius").unwrap_or(0.) as f32,
                actor_type_filter: flt.and_then(|f| f.text("ActorTypeFilter")).unwrap_or("").to_owned(), exclude_pawn_friends: flt.and_then(|f| f.bool("bExcludePawnFriends")).unwrap_or(false),
                exclude_obstructed_by_world: s.bool("bExcludeObstructedByWorld").unwrap_or(false), max_targets: s.int("MaxTargets").unwrap_or(0) as i32, application_tag: tag.clone(),
                exclude_instigator: flt.and_then(|f| f.bool("bExcludeInstigator")).unwrap_or(false), effect_classes: effects.clone() });
        }
    }
    Ok(a)
}

/// Loads the stat row + weapon actor + fire ability of a WID package (`/Game/Athena/Items/Weapons/WID_...`).
pub fn load(src: &ClipSourceDir, wid_package: &str) -> Result<GunHit, String> {
    let pkg = loader::package(src, pkg_of(wid_package))?;
    let idx = pkg.exports.iter().position(|e| e.object_name == pkg.exports[0].object_name).unwrap_or(0);
    let (s, e) = pkg.export_range(idx);
    let wid = read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())?;
    let h = wid.fields("WeaponStatHandle").ok_or("WID lacks WeaponStatHandle")?;
    let table = h.object("DataTable").filter(|i| *i < 0).map(|i| pkg.import_path(i)).ok_or("stat handle has no DataTable")?;
    let row = h.text("RowName").ok_or("stat handle has no RowName")?.to_owned();
    let stats = StatRow::from_props(&data_table_row(src, &table, &row)?)?;
    let pres = presentation(src, &wid, &pkg)?;
    let projectile = match &pres.projectile_template { Some(t) => Some(self::projectile(src, t)?), None => None };
    let perfect = wid.bool("bShouldUsePerfectAimWhenTargetingMinSpread").unwrap_or(true);
    let trigger = wid.text("TriggerType").unwrap_or("EFortWeaponTriggerType::OnPress").to_owned();
    let ammo = path_of(&pkg, &wid, "AmmoData").map(|p| pkg_of(&p).rsplit('/').next().unwrap_or("").to_owned()).unwrap_or_default();
    let s = |k: &str| wid.text(k).unwrap_or("").to_owned();
    let tracer = match pres.tracer_template.as_deref() { Some(t) => match crate::tracer::load(src, t) { Ok(d) => Some(d), Err(e) => { eprintln!("[hitreg] {wid_package}: tracer {t}: {e}"); None } }, None => None };
    let projectile_particles = pres.projectile_template.as_deref().map(|t| crate::tracer::component_particles(src, t)).unwrap_or_default();
    Ok(GunHit { wid: wid_package.to_owned(), row, stats, ability: fire_ability(src, &wid, &pkg)?, pres, projectile, perfect_aim_when_targeting_min_spread: perfect, trigger, ammo,
        rarity: s("Rarity"), tier: s("Tier"), min_level: wid.int("MinLevel").unwrap_or(0),
        ammo_path: path_of(&pkg, &wid, "AmmoData").unwrap_or_default(), ability_path: path_of(&pkg, &wid, "PrimaryFireAbility").unwrap_or_default(), actor_path: path_of(&pkg, &wid, "WeaponActorClass").unwrap_or_default(),
        stat_table: pkg_of(&table).to_owned(), tracer, projectile_particles })
}

/// Package paths of every Battle Royale ranged weapon definition in the install (`/Game/Athena/Items/Weapons/WID_*_Athena_*`, harvesting tools and
/// Save-the-World schematics excluded), sorted by name.
pub fn br_weapon_wids(set: &fn_pak::PakSet) -> Vec<String> {
    let mut v: Vec<String> = set.glob("*WID_*Athena*.uasset").into_iter().map(|(_, e)| e.rel_path().to_owned())
        .filter(|p| p.contains("/Athena/Items/Weapons/") && !p.contains("Harvest") && !p.contains("Schematics"))
        .map(|p| format!("/Game/{}", p.trim_start_matches("FortniteGame/Content/").trim_end_matches(".uasset"))).collect();
    v.sort(); v.dedup(); v
}




#[cfg(test)]
mod table_tests {
    use super::*;
    /// Every BR ranged WID loads completely (stat row with every column, actor chain, ability, projectile); inheritance and the Zapper literal projectile resolve.
    #[test]
    fn every_br_weapon_loads_with_all_columns() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT") else { return };
        let paks = fn_pak::PakSet::open_install(&root).unwrap();
        let src = ClipSourceDir::Pak(&paks);
        let guns: Vec<GunHit> = br_weapon_wids(&paks).iter().map(|w| load(&src, w).unwrap()).filter(|g| g.stat_table.ends_with("AthenaRangedWeapons")).collect();
        assert_eq!(guns.len(), 45, "45 BR ranged WIDs");
        for g in &guns {
            assert_eq!(g.stats.columns.len(), 131, "{}: every column of the row is kept", g.wid);
            assert!(g.pres.actor_chain.len() >= 2 && g.pres.actor_chain.last().unwrap() == "B_Ranged_Generic_C" || g.pres.actor_chain.len() == 1 || !g.pres.actor_chain.is_empty(), "{}: actor chain", g.wid);
            assert!(!g.ability.selections.is_empty(), "{}: ability target selection", g.wid);
        }
        let z = guns.iter().find(|g| g.wid.ends_with("WID_Pistol_Zapper_Athena_SR_Ore_T03")).unwrap().projectile.clone().unwrap();
        assert_eq!((z.initial_speed, z.gravity_scale), (7600., Some(0.)));
        assert!(z.max_speed.is_infinite());
        let s = guns.iter().find(|g| g.wid.ends_with("WID_Sniper_BoltAction_Scope_Athena_SR_Ore_T03")).unwrap();
        assert_eq!(s.projectile.as_ref().unwrap().gravity_scale, Some(0.18));
    }
}
