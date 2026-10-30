//! Original weapon numbers read from the install at load time (no values are typed in here): the weapon item definition (WID) names its
//! `WeaponStatHandle` (DataTable + row), the table row carries clip size / firing rate / reload time / damage; the bandage ability
//! Blueprint (`GA_Athena_Bandage` CDO) carries its heal cap, max heal amount, play rate and montage.
use crate::{loader, ClipSourceDir};
use fn_ueio::{props::{read_props, Ctx, PropList, PropValue}, Package, Reader};
use fn_weapons::{Assets, Definition, Trigger};

/// Values of the stat row that `fn_weapons::Definition` has no field for.
#[derive(Clone, Debug, PartialEq)]
pub struct StatExtras {
    /// `MaxSpareAmmo` (a cap on the reserve, not a grant).
    pub max_spare_ammo: u32,
    /// `InitialClips`.
    pub initial_clips: u32,
    /// `EnvDmgPB` (damage to buildings / environment).
    pub env_damage_pb: f32,
    /// `Spread` / `RecoilVert` are not used by the runtime yet; kept as read.
    pub spread: f32,
    /// FIRE CADENCE: `ROFScale` (stat+0x18C), `BurstFiringRate` (stat+0x190), `AutoReloadDelayOverride` (stat+0x23C), `bReloadInterruptIsImmediate`, charge times
    /// (`MinChargeTime`, `MaxChargeTime`, `ChargeDownTime`). Read as serialized; the exe functions that consume them are cited in `arms.rs`.
    pub rof_scale: f32, pub burst_firing_rate: f32, pub auto_reload_delay_override: f32, pub reload_interrupt_immediate: bool, pub charge: (f32, f32, f32),
    /// The ammo item definition's SmallPreviewImage (what GetItemDefinitionSmallPreviewImageBrush shows in the HUD): object path and PNG name.
    pub ammo_icon_path: String,
    pub ammo_icon: String,
    /// QUICKBAR AMMO: the ammo item definition's `AmmoIconBrush` (`FFortMultiSizeBrush` at UFortAmmoItemDefinition+0x4D8), which the item card's
    /// `SFortItemCard_AmmoIconImage` draws (`fn_ui::screens::quickbar`). Brush_XXS .. Brush_XL: (ImageSize, PNG name, texture object path), None when the
    /// sub-brush names no resource. A sub-brush without a serialized ImageSize keeps the ctor's 32 x 32 (FFortMultiSizeBrush ctor @14062e410).
    pub ammo_icon_brush: [Option<([f32; 2], String, String)>; 6],
}

/// QUICKBAR AMMO: `FFortMultiSizeBrush` member names in size order (XXS, XS, S, M, L, XL; struct offsets 0x0 .. 0x2D0, 0x90 each).
pub const MULTI_SIZE_BRUSHES: [&str; 6] = ["Brush_XXS", "Brush_XS", "Brush_S", "Brush_M", "Brush_L", "Brush_XL"];

/// QUICKBAR AMMO: the sub-brushes of an `FFortMultiSizeBrush` property (see [`StatExtras::ammo_icon_brush`]).
pub fn multi_size_brush(pkg: &Package, p: &PropList, key: &str) -> [Option<([f32; 2], String, String)>; 6] {
    MULTI_SIZE_BRUSHES.map(|b| {
        let brush = p.fields(key)?.fields(b)?;
        let path = ref_path(pkg, brush, "ResourceObject")?;
        let size = match brush.get("ImageSize") { Some(PropValue::Vec2(v)) => *v, _ => [32., 32.] };
        Some((size, format!("{}.png", path.split('.').next_back().unwrap_or_default()), path))
    })
}

/// What the healable's Blueprint ability computes (ported from the bytecode of each `GA_Athena_*`; the numbers inside come from the data).
#[derive(Clone, Debug, PartialEq)]
pub enum HealRule {
    /// `GA_Athena_Bandage`: usable while `ceil(health) < HealingCap`; stacks = `trunc(clamp(HealingCap - health, 0, MaxHealingAmount))`.
    Bandage { healing_cap: f32, max_healing_amount: f32 },
    /// `GA_Athena_Medkit`: usable while `ceil(health) < max health`; stacks = `HealingAmount` (the native Healing attribute clamps at max health).
    Medkit { healing_amount: f32 },
    /// `GA_Athena_Shields`: usable while `ceil(shield) < max shield`; stacks = `trunc(clamp(max shield - shield, 0, ShieldingAmount))`.
    Shield { shielding_amount: f32 },
    /// `GA_Athena_PurpleStuff`: always usable (cooldown permitting); one stack of a duration effect.
    Purple,
    Throw { initial_speed: f32, gravity_scale: f32, projectile: String, activation_sound: Option<String> },
}
/// One `GameplayEffect` modifier: attribute name (`Healing`, `CurrentShield`) and its additive `ScalableFloat` magnitude (no curve in the data).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectMod { pub attribute: String, pub magnitude: f32 }
/// The `GameplayEffect` a healable applies (`BP_ApplyGameplayEffectToOwner(effect, Level 1, Stacks)`).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectDef { pub class: String, pub mods: Vec<EffectMod>, pub duration: Option<f32>, pub period: Option<f32> }
/// A consumable that restores health / shield, read generically from its item definition, ability CDO and effect CDOs.
#[derive(Clone, Debug, PartialEq)]
pub struct HealableStats {
    /// Item definition package and its short name (`Athena_Bandage`).
    pub item: String, pub name: String,
    pub ability: String,
    pub rule: HealRule,
    /// `TriggerDuration`: the `RawMagnitude` of the `GameplayCue.Athena.Equipping` cue the ability adds on activation (the use-progress ring's duration).
    pub trigger_duration: f32,
    /// `MontageInfo.MontageToPlay` (the struct the bytecode hands `PlayMontageInfoAndWaitForTrigger`; NOT the unused `AnimMontageInfo`) and its play rate (1.0 when not serialized).
    pub montage: String, pub montage_rate: f32,
    /// `AbilityCosts[0].CostValue`.
    pub cost: f32,
    pub effect: EffectDef,
    /// `CooldownGameplayEffectClass` duration in seconds.
    pub cooldown: Option<f32>,
    /// Item definition `MaxStackSize`.
    pub max_stack: u32,
}

/// Brush textures of the AthenaEquipProgress ring (the use-progress indicator shown while a consumable is used).
pub const EQUIP_PROGRESS_TEXTURES: [&str; 2] = [
    "/Game/UI/ZoneCompletion/NewVersion/TeleportPad/Textures/Results_TimerBacking.Results_TimerBacking",
    "/Game/UI/ZoneCompletion/NewVersion/TeleportPad/Textures/Results_TimerFill.Results_TimerFill",
];

/// HUD FIX (verification/building/hud/fix): brush textures the ripped Athena HUD widgets reference that are not in the exported UI folder, so they are decoded
/// from the packages like [`EQUIP_PROGRESS_TEXTURES`] (served to the UI under `<object>.png`). Sources (`fn-inspect props / disasm`):
/// `AthenaQuickbarSecondary` `[Image] Switch Action` Brush ResourceObject + `SetBrushFromTexture(Icon-QuickbarMode-Build-M / -Fight-M)` in its minimise /
/// maximise Kismet; `[Image] Separator` Brush ResourceObject `Rule-Dotted-HUD`; `Border-KeyBase-Center-M` (the interaction prompt's CommonBorder style)
/// Background ResourceObject `Keybase-Center-L`.
pub const HUD_PAK_TEXTURES: [&str; 12] = [
    "/Game/UI/Icons/Icon-QuickbarMode-Build-M.Icon-QuickbarMode-Build-M",
    "/Game/UI/Icons/Icon-QuickbarMode-Fight-M.Icon-QuickbarMode-Fight-M",
    "/Game/UI/Foundation/Textures/Rule/Rule-Dotted-HUD.Rule-Dotted-HUD",
    "/Game/UI/HUD/Textures/Keybase-Center-L.Keybase-Center-L",
    // DefaultGameData.IconKeyList (PC) Brush_S of the KeybindWidget (Target Info prompts); Mouse1 is in the UI export
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-Mouse2-S.Keybind-Mouse2-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-Mouse3-S.Keybind-Mouse3-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-Mouse4-S.Keybind-Mouse4-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-Mouse5-S.Keybind-Mouse5-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-CapsLock-S.Keybind-CapsLock-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-PageUp-S.Keybind-PageUp-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-PageDown-S.Keybind-PageDown-S",
    "/Game/UI/InGame/KeybindWidget/Textures/Keybind-Backspace-S.Keybind-Backspace-S",
];

/// Item definitions of every healing consumable in the 1.8 data (`/Game/Athena/Items/Consumables`: Bandage, Medkit, Shields, PurpleStuff).
pub const HEALABLE_ITEMS: [&str; 5] = [
    "/Game/Athena/Items/Consumables/Bandage/Athena_Bandage",
    "/Game/Athena/Items/Consumables/Medkit/Athena_Medkit",
    "/Game/Athena/Items/Consumables/Shields/Athena_Shields",
    "/Game/Athena/Items/Consumables/PurpleStuff/Athena_PurpleStuff",
    "/Game/Athena/Items/Consumables/Grenade/Athena_Grenade",
];
fn package_name(r: &str) -> &str { r.split('.').next().unwrap_or(r) }
fn f32_of(p: &PropList, key: &str) -> Result<f32, String> { p.get(key).and_then(PropValue::as_f64).map(|v| v as f32).ok_or_else(|| format!("stat row lacks {key}")) }
fn u32_of(p: &PropList, key: &str) -> Result<u32, String> { let v = p.get(key).and_then(PropValue::as_f64).ok_or_else(|| format!("stat row lacks {key}"))?; if v < 0. || v.fract() != 0. { return Err(format!("{key} is not a count")); } Ok(v as u32) }

/// One row of a `DataTable` export: `i32 n`, then `n x { RowName FName, tagged property list }` after the table's own tagged properties.
pub fn data_table_row(src: &ClipSourceDir, table_package: &str, row: &str) -> Result<PropList, String> {
    let pkg = loader::package(src, package_name(table_package))?;
    let idx = pkg.find_export("DataTable").ok_or_else(|| format!("{table_package}: no DataTable export"))?;
    let (start, end) = pkg.export_range(idx);
    let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
    let mut r = Reader::at(&pkg.data[..end], start);
    read_props(&mut r, &ctx).map_err(|e| format!("{table_package}: {e}"))?;
    // UObject::Serialize ends with the object-guid flag (a 4-byte bool; a 16-byte guid follows when set), then the table's own data.
    if r.i32().map_err(|e| e.to_string())? != 0 { r.bytes(16).map_err(|e| e.to_string())?; }
    let n = r.i32().map_err(|e| e.to_string())?;
    if !(0..=100_000).contains(&n) { return Err(format!("{table_package}: implausible row count {n}")); }
    for _ in 0..n {
        let name = r.i32().map_err(|e| e.to_string())?;
        let number = r.i32().map_err(|e| e.to_string())?;
        let base = pkg.names.get(usize::try_from(name).map_err(|_| "negative row name index")?).ok_or("row name out of bounds")?;
        let props = read_props(&mut r, &ctx).map_err(|e| format!("{table_package}: row {base}: {e}"))?;
        let full = if number > 0 { format!("{base}_{}", number - 1) } else { base.clone() };
        if full == row { return Ok(props); }
    }
    Err(format!("{table_package}: no row {row} (of {n})"))
}

/// (`DataTable` package path, row name) of a `DataTableRowHandle` struct property of an item (the table is an import of the item's package).
fn handle(pkg: &Package, properties: &PropList, key: &str) -> Result<(String, String), String> {
    let h = properties.fields(key).ok_or_else(|| format!("item lacks {key}"))?;
    let table = h.object("DataTable").filter(|i| *i < 0).ok_or("stat handle has no external DataTable")?;
    let row = h.text("RowName").ok_or("stat handle has no RowName")?.to_owned();
    Ok((pkg.import_path(table), row))
}

fn props_of(pkg: &Package, class: &str) -> Result<PropList, String> {
    let idx = pkg.find_export(class).ok_or_else(|| format!("missing {class}"))?;
    let (start, end) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..end], start), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}

/// A HUD texture from the packages: (PNG name = object name + .png, width, height, RGBA8 of the largest available mip).
pub fn decode_ui_texture(src: &ClipSourceDir, object_path: &str) -> Result<(String, u32, u32, Vec<u8>), String> {
    let object = object_path.split_once('.').map(|x| x.1).ok_or("texture requires a full object path")?;
    let t = loader::texture_export(src, object_path)?;
    let mip = t.best_mip().ok_or_else(|| format!("{object_path}: no mip"))?;
    let im = t.decode(mip, fn_texture::DecodeOptions::default()).map_err(|e| format!("{object_path}: {e}"))?;
    Ok((format!("{object}.png"), im.width as u32, im.height as u32, im.to_rgba8()))
}

fn ref_path(pkg: &Package, p: &PropList, key: &str) -> Option<String> {
    if let Some(s) = p.text(key).filter(|s| s.starts_with("/Game/")) { return Some(s.to_owned()); }
    p.object(key).filter(|i| *i < 0).map(|i| pkg.import_path(i))
}

/// The firearm definition from the WID + its stat row. Everything numeric is read; `assets` carries the recovered bindings (filled by the caller).
pub fn ranged_definition(src: &ClipSourceDir, item_path: &str, pkg: &Package, properties: &PropList, display: Option<String>, rarity: &str) -> Result<(Definition, StatExtras), String> {
    let (table, row) = handle(pkg, properties, "WeaponStatHandle")?;
    let r = data_table_row(src, &table, &row)?;
    let trigger = match properties.text("TriggerType") {
        None => Trigger::NATIVE_DEFAULT, // not serialized = the native class default (OnPress), see fn_weapons::Trigger::NATIVE_DEFAULT
        Some(t) => match t.rsplit("::").next().unwrap_or(t) { "Automatic" => Trigger::Automatic, "OnPress" => Trigger::OnPress, "OnRelease" => Trigger::OnRelease, other => Trigger::Unsupported(other.to_owned()) },
    };
    let def = Definition {
        item: item_path.to_owned(), display_name: display, rarity: Some(rarity.to_owned()), stat_row: row,
        ammo: ref_path(pkg, properties, "AmmoData").ok_or("item lacks AmmoData")?,
        clip_size: u32_of(&r, "ClipSize")?, ammo_cost: u32_of(&r, "AmmoCostPerFire")?, cartridges: u32_of(&r, "CartridgePerFire")?, bullets_per_cartridge: u32_of(&r, "BulletsPerCartridge")?,
        firing_rate: f32_of(&r, "FiringRate")?, reload_time: f32_of(&r, "ReloadTime")?, individual_reload: r.bool("bReloadIndividualBullets").ok_or("stat row lacks bReloadIndividualBullets")?,
        damage_pb: f32_of(&r, "DmgPB")?, critical_multiplier: f32_of(&r, "DamageZone_Critical")?, trigger, assets: Assets::default(),
    };
    def.validate().map_err(|e| format!("{item_path}: {e}"))?;
    let ammo_pkg = loader::package(src, package_name(&def.ammo))?;
    let ammo_class = ammo_pkg.exports.iter().enumerate().find_map(|(i, _)| { let c = ammo_pkg.export_class(i); c.contains("AmmoItemDefinition").then_some(c) }).ok_or("ammo definition has no FortAmmoItemDefinition export")?;
    let aprops = props_of(&ammo_pkg, &ammo_class)?;
    let ammo_icon_path = ref_path(&ammo_pkg, &aprops, "SmallPreviewImage").ok_or("ammo definition lacks SmallPreviewImage")?;
    let ammo_icon = format!("{}.png", package_name(&ammo_icon_path).rsplit('/').next().unwrap_or_default());
    let ammo_icon_brush = multi_size_brush(&ammo_pkg, &aprops, "AmmoIconBrush");
    let extras = StatExtras { ammo_icon_path, ammo_icon, ammo_icon_brush, max_spare_ammo: u32_of(&r, "MaxSpareAmmo")?, initial_clips: u32_of(&r, "InitialClips")?, env_damage_pb: f32_of(&r, "EnvDmgPB")?, spread: f32_of(&r, "Spread")?,
        rof_scale: f32_of(&r, "ROFScale")?, burst_firing_rate: f32_of(&r, "BurstFiringRate")?, auto_reload_delay_override: f32_of(&r, "AutoReloadDelayOverride")?,
        reload_interrupt_immediate: r.bool("bReloadInterruptIsImmediate").ok_or("stat row lacks bReloadInterruptIsImmediate")?,
        charge: (f32_of(&r, "MinChargeTime")?, f32_of(&r, "MaxChargeTime")?, f32_of(&r, "ChargeDownTime")?) };
    Ok((def, extras))
}

fn cdo_props(pkg: &Package) -> Result<PropList, String> {
    let idx = pkg.exports.iter().position(|e| e.object_name.starts_with("Default__")).ok_or("package has no class default object")?;
    let (start, end) = pkg.export_range(idx);
    read_props(&mut Reader::at(&pkg.data[..end], start), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).map_err(|e| e.to_string())
}

/// `GameplayEffect` CDO read with the complete property decoder (`fn-uasset`: `GameplayModifierInfo` is a nested struct array): additive modifiers with a plain `ScalableFloat`
/// magnitude, duration policy / magnitude and period.
pub fn effect_def(src: &ClipSourceDir, class_package: &str) -> Result<EffectDef, String> {
    use fn_uasset::Value;
    let (asset, exp) = match src {
        ClipSourceDir::Pak(set) => (set.read_game(class_package, "uasset").map_err(|e| format!("{class_package}: {e}"))?.into_owned(), set.read_game(class_package, "uexp").map(|c| c.into_owned()).unwrap_or_default()),
        ClipSourceDir::RawPackages(root) => { let base = root.join("FortniteGame/Content").join(class_package.trim_start_matches("/Game/")); (std::fs::read(format!("{}.uasset", base.display())).map_err(|e| e.to_string())?, std::fs::read(format!("{}.uexp", base.display())).unwrap_or_default()) }
        ClipSourceDir::FnAnimDir(_) => return Err("a .fnanim directory has no effects".into()),
    };
    let pkg = fn_uasset::Package::parse(&asset, &exp).map_err(|e| format!("{class_package}: {e}"))?;
    let idx = pkg.exports.iter().position(|e| e.name.starts_with("Default__")).ok_or("effect has no class default object")?;
    let obj = pkg.read_export(idx).map_err(|e| format!("{class_package}: {e}"))?;
    let p = &obj.props;
    let sf = |v: &Value, key: &str| -> Option<f32> { v.field(key)?.field("Value")?.as_f32() };
    let mut mods = vec![];
    for m in p.get("Modifiers").and_then(Value::as_array).unwrap_or(&[]) {
        let attr = m.path("Attribute.AttributeName").and_then(Value::as_str).ok_or("modifier lacks an attribute")?.to_owned();
        if m.field("ModifierOp").and_then(Value::as_str).map_or(true, |o| !o.ends_with("Additive")) { return Err(format!("{class_package}: unsupported modifier op")); }
        let mag = m.field("ModifierMagnitude").ok_or("modifier lacks ModifierMagnitude")?;
        if mag.field("MagnitudeCalculationType").and_then(Value::as_str).map_or(true, |t| !t.ends_with("ScalableFloat")) { return Err(format!("{class_package}: unsupported magnitude type")); }
        let s = mag.field("ScalableFloatMagnitude").ok_or("lacks ScalableFloatMagnitude")?;
        if s.path("Curve.RowName").and_then(Value::as_str).map_or(false, |r| r != "None") { return Err(format!("{class_package}: curve-scaled magnitudes are not supported")); }
        mods.push(EffectMod { attribute: attr, magnitude: s.field("Value").and_then(Value::as_f32).ok_or("magnitude has no value")? });
    }
    let has_duration = p.get("DurationPolicy").and_then(Value::as_str).map_or(false, |d| d.ends_with("HasDuration"));
    Ok(EffectDef { class: class_package.to_owned(), mods, duration: has_duration.then(|| p.get("DurationMagnitude").and_then(|d| sf(d, "ScalableFloatMagnitude"))).flatten(), period: p.get("Period").and_then(|v| v.field("Value")).and_then(Value::as_f32) })
}
/// A healing consumable from its item definition: the ability CDO values, the effect it applies (per the ability's bytecode) and its cooldown effect.
pub fn healable_stats(src: &ClipSourceDir, item_path: &str, item_pkg: &Package, item: &PropList) -> Result<HealableStats, String> {
    let ability = ref_path(item_pkg, item, "PrimaryFireAbility").ok_or("consumable lacks PrimaryFireAbility")?;
    let pkg = loader::package(src, package_name(&ability))?;
    let p = cdo_props(&pkg)?;
    let name = package_name(&ability).rsplit('/').next().unwrap_or_default().to_owned();
    // (rule, the GameplayEffect class the bytecode applies) per ability Blueprint. Medkit's bytecode applies the Bandage heal effect (its own container effect is unused).
    let (rule, effect_pkg) = match name.as_str() {
        "GA_Athena_Bandage" => (HealRule::Bandage { healing_cap: f32_of(&p, "HealingCap")?, max_healing_amount: f32_of(&p, "MaxHealingAmount")? }, "/Game/Athena/Items/Consumables/Bandage/GE_Athena_Bandage_Heal"),
        "GA_Athena_Medkit" => (HealRule::Medkit { healing_amount: f32_of(&p, "HealingAmount")? }, "/Game/Athena/Items/Consumables/Bandage/GE_Athena_Bandage_Heal"),
        "GA_Athena_Shields" => (HealRule::Shield { shielding_amount: f32_of(&p, "ShieldingAmount")? }, "/Game/Athena/Items/Consumables/Shields/GE_Athena_Shields"),
        "GA_Athena_PurpleStuff" => (HealRule::Purple, "/Game/Athena/Items/Consumables/PurpleStuff/GE_Athena_PurpleStuff"),
        "GA_Athena_Grenade" => (HealRule::Throw { initial_speed: f32_of(&p, "InitialSpeed")?, gravity_scale: f32_of(&p, "Athena_Grenade_Gravity")?,
            projectile: ref_path(&pkg, &p, "Prj_Athena_Grenade").ok_or("GA_Athena_Grenade lacks Prj_Athena_Grenade")?, activation_sound: ref_path(&pkg, &p, "ActivationSound") }, ""),
        other => return Err(format!("unsupported healing ability {other}")),
    };
    let info = p.fields("MontageInfo").ok_or("ability lacks MontageInfo")?;
    let montage = ref_path(&pkg, info, "MontageToPlay").ok_or("MontageInfo lacks MontageToPlay")?;
    let montage_rate = info.get("AnimPlayRate").and_then(PropValue::as_f64).map_or(1., |v| v as f32);
    let costs = p.array("AbilityCosts").ok_or("ability lacks AbilityCosts")?;
    let cost = costs.first().and_then(PropValue::as_fields).and_then(|c| c.fields("CostValue")).and_then(|v| v.get("Value")).and_then(PropValue::as_f64).ok_or("ability cost value missing")? as f32;
    let cooldown = match ref_path(&pkg, &p, "CooldownGameplayEffectClass") { Some(c) => effect_def(src, package_name(&c))?.duration, None => None };
    Ok(HealableStats {
        item: item_path.to_owned(), name: package_name(item_path).rsplit('/').next().unwrap_or_default().to_owned(), ability: package_name(&ability).to_owned(), rule,
        trigger_duration: if effect_pkg.is_empty() { 0. } else { f32_of(&p, "TriggerDuration")? }, montage: package_name(&montage).to_owned(), montage_rate, cost,
        effect: if effect_pkg.is_empty() { EffectDef { class: String::new(), mods: vec![], duration: None, period: None } } else { effect_def(src, effect_pkg)? }, cooldown, max_stack: item.int("MaxStackSize").map_or(1, |v| v.max(1) as u32),
    })
}