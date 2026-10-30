//! The player's arms: quickbar selection, the held firearm / bandage state and the montage playback they drive.
//!
//! Every NUMBER below is read from the original 1.8 data (`fn_pose::loadout::Resources`: WID + `WeaponStatHandle` stat row, `GA_Athena_Bandage`
//! CDO, the montages and their notifies). What the data does not decide is marked `ASSUMED` and listed in `ASSUMPTIONS`; behaviours with no
//! evidence (spread, hit registration, ADS, auto-reload, muzzle effects, the 1.8 ammo widget layout) are NOT implemented.
//!
//! * `exact` : clip size, firing rate, reload time, damage (`DmgPB`), bullets per cartridge, ammo cost, trigger type, individual-reload
//!   flag, interrupt flag, montage segments / lengths / blends / notify times, bandage heal cap / max amount / play rate / cost,
//!   the bandage gate and effect-level arithmetic (`fn_weapons::recovered_healing`), the reload-progress arithmetic
//!   (`fn_weapons::recovered_reload`).
//! * `ASSUMED`: see `ASSUMPTIONS`.
use fn_animgraph::Montage;
pub mod inventory_ops; // INVENTORY HOOK: pickup / drop / swap / equip rules on the quickbar (`arms/inventory_ops.rs`)
use fn_weapons::{recovered_healing, recovered_reload, reload_ammo, Definition, Trigger}; // AMMO HOOK: reload_ammo = the recovered reload / ammo-transfer chain

/// Behaviour the original data does not decide. By default these pieces are MISSING (left out and reported at load); `Policy::assume_unrecovered`
/// (script line `assume`, flag `--assume-unrecovered`) switches the stand-ins listed in `ASSUMPTIONS` on for development runs only.
pub const MISSING_BY_DEFAULT: &[&str] = &[
    "reload / ammo are recovered (see `fn_weapons::reload_ammo`): the fresh player's StartingItems grant no ammo, so the reserve starts at 0 and ammo only comes from `give_ammo` pickups; native-only leftovers: the TriggerAbility-notify callback of the reload ability, ReloadScale / alteration multipliers of the reload time, IsReloading bookkeeping and the ability-system blocking tags in CanReload",
    "no hit registration, spread, ADS, auto-reload, muzzle / tracer effects, weapon sounds or ammo widget layout (none recovered)",
];

/// Development stand-ins (only with `Policy::assume_unrecovered`).
pub const ASSUMPTIONS: &[&str] = &[
    "fire interval = 1 / FiringRate (stat row value is rounds per second; the native cooldown code is not recovered)",
    "healing: BP_ApplyGameplayEffectToOwner(effect, Level 1, Stacks = amount) adds Stacks x the effect magnitude (1.0) to Healing / CurrentShield (engine stack behaviour, not verified in the exe); the native health set clamps health and shield at 100 (the HUD bar maximum)",
    "a heal is cancelled by selecting another slot (the native interrupt rules behind the Gameplay.Action.Player.InterruptibleHealingItem tag are not recovered); a cancelled use pays no cost",
    "max health / max shield = 100 (the HUD bar maximum; the native FortHealthSet defaults are not recovered); a duration effect (PurpleStuff) ticks every Period seconds from application for Duration seconds",
    "selecting another slot cancels an unfinished reload / heal; equip montage does not gate firing",
    "no hit registration: there is no damageable target in the sandbox, DmgPB is reported in the fire event only",
    "montage blend curve: Hermite smoothstep for every blend option except 0 (linear)",
];

#[derive(Clone, Debug)]
pub struct Seg { pub clip: String, pub start: f32, pub anim_start: f32, pub anim_end: f32, pub rate: f32, pub loops: u32 }
impl Seg {
    fn loop_len(&self) -> f32 { (self.anim_end - self.anim_start).max(0.) }
    /// Montage-time length of this segment (`loops` repetitions of the played range at `rate`).
    pub fn duration(&self) -> f32 { self.loop_len() / self.rate.max(1e-6) * self.loops.max(1) as f32 }
}
#[derive(Clone, Debug)]
pub struct MontageData { pub length: f32, pub blend_in: (f32, u8), pub blend_out: (f32, u8), pub slot: &'static str, pub segs: Vec<Seg>, pub notifies: Vec<(String, f32)>, /// Sound-making notifies (`AnimNotify_PlaySound`, `AnimNotify_PlayReloadFX`) in time order.
    pub fx: Vec<FxNotify>,
    /// How the slot applies the montage's sequence (its `AdditiveAnimType`: the 1.8 fire montages are `AAT_RotationOffsetMeshSpace`).
    pub blend: fn_animgraph::SlotBlend }
/// A montage notify that makes sound (read from the notify exports, never guessed).
#[derive(Clone, Debug, PartialEq)]
pub enum Fx {
    /// `AnimNotify_PlaySound`: the SoundCue object path, `PitchMultiplier`, `VolumeMultiplier`.
    Sound { cue: String, pitch: f32, volume: f32 },
    /// `AnimNotify_PlayReloadFX_C`: `FortWeapon::PlayReloadFX(ReloadStage)` -> the weapon's `ReloadSounds[stage]`.
    Reload(fn_audio::weapon_events::ReloadStage),
    /// HEAL FX: `AnimNotify_PlayParticleEffect` (`UGameplayStatics::SpawnEmitterAttached(PSTemplate, mesh, SocketName, ...)`), e.g. `P_Consume_Item` on `b_MF_Head`
    /// at the TriggerAbility moment of the Bandage / Medkit / PurpleStuff montages.
    Particle(fn_anim::sequence::ParticleNotify),
}
#[derive(Clone, Debug, PartialEq)]
pub struct FxNotify { pub time: f32, pub fx: Fx }
/// The sound-making notifies of a montage, by time. A `PlayReloadFX` whose enum is not serialized is the class default = value 0 = `ReloadStart`.
pub fn montage_fx(m: &fn_anim::montage::AnimMontage) -> Vec<FxNotify> {
    use fn_audio::weapon_events::ReloadStage;
    let mut v: Vec<FxNotify> = m.notifies.iter().filter_map(|n| {
        let fx = if let Some(cue) = &n.sound { Fx::Sound { cue: cue.clone(), pitch: n.sound_pitch, volume: n.sound_volume } }
            else if let Some(stage) = &n.reload_stage { Fx::Reload(if stage.is_empty() { ReloadStage::Start } else { ReloadStage::from_enum_text(stage)? }) }
            else if let Some(p) = &n.particle { Fx::Particle(p.clone()) }
            else { return None };
        Some(FxNotify { time: n.link_value + n.trigger_time_offset, fx })
    }).collect();
    v.sort_by(|a, b| a.time.total_cmp(&b.time));
    v
}
fn static_slot(s: &str) -> Option<&'static str> {
    Some(match s { "UpperBody" => "UpperBody", "MeshRotationUpperBody" => "MeshRotationUpperBody", "FullBody" => "FullBody", "FullBodyInPlace" => "FullBodyInPlace", "AdditiveFullBody" => "AdditiveFullBody", "DefaultSlot" => "DefaultSlot", _ => return None })
}
fn strip(p: &str) -> String { p.split('.').next().unwrap_or(p).to_owned() }
impl MontageData {
    pub fn from_anim(m: &fn_anim::montage::AnimMontage) -> Result<MontageData, String> {
        let track = m.slots.first().ok_or_else(|| format!("{}: montage has no slot track", m.name))?;
        let slot = static_slot(&track.slot_name).ok_or_else(|| format!("{}: unsupported slot {}", m.name, track.slot_name))?;
        let segs = track.segments.iter().map(|s| Ok(Seg { clip: strip(s.animation_path.as_deref().ok_or("segment lacks an animation path")?), start: s.start_pos, anim_start: s.anim_start_time, anim_end: s.anim_end_time, rate: s.play_rate, loops: s.looping_count.max(1) as u32 })).collect::<Result<Vec<_>, String>>()?;
        Ok(MontageData { length: m.length, blend_in: (m.blend_in.time, m.blend_in.option), blend_out: (m.blend_out.time, m.blend_out.option), slot, segs, notifies: m.notifies.iter().map(|n| (n.notify_name.clone(), n.link_value)).collect(), fx: montage_fx(m), blend: fn_animgraph::SlotBlend::Pose })
    }
    /// The montage with its slot blend read from the `AdditiveAnimType` of its segments' sequences (all segments of one montage must agree).
    pub fn from_anim_in(m: &fn_anim::montage::AnimMontage, sequences: &std::collections::HashMap<String, fn_pose::loadout::Sequence>) -> Result<MontageData, String> {
        use fn_anim::AdditiveAnimType as A;
        let mut d = Self::from_anim(m)?;
        let mut kinds = d.segs.iter().map(|s| sequences.get(&s.clip).map(|q| q.source.additive.kind.clone()).ok_or_else(|| format!("{}: sequence {} was not loaded", m.name, s.clip)));
        let first = kinds.next().ok_or_else(|| format!("{}: montage has no segments", m.name))??;
        for k in kinds { if k? != first { return Err(format!("{}: segments mix additive types", m.name)); } }
        d.blend = match first { A::None => fn_animgraph::SlotBlend::Pose, A::LocalSpaceBase => fn_animgraph::SlotBlend::LocalAdditive, A::RotationOffsetMeshSpace => fn_animgraph::SlotBlend::MeshAdditive };
        Ok(d)
    }
    pub fn notify(&self, name: &str) -> Option<f32> { self.notifies.iter().find(|(n, _)| n == name).map(|n| n.1) }
    /// (sequence package, time inside the sequence) at montage time `t`.
    pub fn sample(&self, t: f32) -> Option<(String, f32)> {
        for s in &self.segs {
            if t >= s.start && t < s.start + s.duration() { return Some((s.clip.clone(), s.anim_start + ((t - s.start) * s.rate).rem_euclid(s.loop_len().max(1e-6)))); }
        }
        // past a segment's end or in a gap: hold the last segment that started
        self.segs.iter().rev().find(|s| t >= s.start).map(|s| (s.clip.clone(), s.anim_end))
    }    fn ease(opt: u8, x: f32) -> f32 { let x = x.clamp(0., 1.); if opt == 0 { x } else { x * x * (3. - 2. * x) } }
    /// Blend weight at montage time `t` (blend-in from the start, blend-out for the last `blend_out` seconds).
    pub fn weight(&self, t: f32) -> f32 {
        let wi = if self.blend_in.0 <= 1e-6 { 1. } else { Self::ease(self.blend_in.1, t / self.blend_in.0) };
        let wo = if self.blend_out.0 <= 1e-6 { 1. } else { Self::ease(self.blend_out.1, (self.length - t) / self.blend_out.0) };
        wi.min(wo)
    }
}

/// FIRE CADENCE: the stat-row columns that drive the native fire timer chain, with the exe functions that consume them as recovered by the hitreg decode
/// (`fn_hitreg::spread`, RVAs `GetFireRate` 0x924f50, `GetBurstFireRate` 0x923100, fire timer callback 0x9212d0, `GetAutoReloadDelay` 0x922310). Not re-verified by the
/// arms code; the call chain that decides WHEN these run is listed in `MISSING_BY_DEFAULT`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cadence {
    /// `BurstFiringRate` (cartridges / s inside one trigger pull; `<= 0` = `FireRate * (CartridgePerFire + 1)`, RVA 0x923100).
    pub burst_firing_rate: f32,
    /// `AutoReloadDelayOverride` (`>= 0` replaces the 0.3001 s default, RVA 0x922310).
    pub auto_reload_delay_override: f32,
    /// `bReloadInterruptIsImmediate`.
    pub interrupt_immediate: bool,
    /// `InputQueueTimePercent` (+0x960): ctor default 0.2 (0x3E4CCCCD, RVA 0x914b60); `B_Shotgun_Standard_Athena` serializes 0.4. NOT read from the Blueprint chain yet (`from_resources` uses the ctor default).
    pub input_queue_percent: f32,
}
impl Cadence {
    /// The constant `GetAutoReloadDelay` returns when the row has no override (float at VA 0x1442a0b74 = 0x3E99A6B5).
    pub const INPUT_QUEUE_DEFAULT: f32 = f32::from_bits(0x3e4c_cccd);
    pub const AUTO_RELOAD_DEFAULT: f32 = f32::from_bits(0x3e99_a6b5);
    pub fn auto_reload_delay(&self) -> f32 { if self.auto_reload_delay_override >= 0. { self.auto_reload_delay_override } else { Self::AUTO_RELOAD_DEFAULT } }
    /// Seconds between the cartridges of one burst (`1 / burst rate`, the fire timer callback re-arms with `divss 1.0, rate`).
    pub fn burst_interval(&self, fire_rate: f32, cartridges: u32) -> f32 { 1. / if self.burst_firing_rate > 0. { self.burst_firing_rate } else { fire_rate * (cartridges as f32 + 1.) } }
}

#[derive(Clone, Debug)]
pub struct GunData {
    pub def: Definition, pub cadence: Cadence, pub max_spare: u32, pub ammo_icon: String, pub core: &'static str,
    /// QUICKBAR AMMO: the ammo item's `AmmoIconBrush` (the item card's ammo icon, `fn_ui::screens::quickbar::MultiSizeBrush`).
    pub ammo_brush: fn_ui::screens::quickbar::MultiSizeBrush,
    pub equip: MontageData, pub fire: MontageData, pub fire_ads: Option<MontageData>, pub reload: MontageData,
    /// Weapon-mesh montages: `None` = the weapon's Blueprint chain leaves the property unset / null (grenade launchers, Gatling, Zapper, AMR have no fire montage; the Zapper no reload montage; rocket launchers no ADS fire).
    pub weapon_fire: Option<MontageData>, pub weapon_fire_ads: Option<MontageData>, pub weapon_reload: Option<MontageData>,
}
#[derive(Clone, Debug)]
/// A healing consumable (Bandage / Medkit / Shields / PurpleStuff) read from the item, ability and effect assets (`fn_pose::loadout_stats::HealableStats`).
pub struct HealableData { pub name: String, pub rule: fn_pose::loadout_stats::HealRule, pub trigger_duration: f32, pub montage: MontageData, pub rate: f32, pub cost: u32, pub effect: fn_pose::loadout_stats::EffectDef, pub cooldown: Option<f32>, pub max_stack: u32 }
/// AMMO HOOK: the inventory side of each firearm's ammo type, read from the ammo item definitions and the Athena game mode's `StartingItems`
/// (`fn_pose::loadout_ammo`), one entry per firearm.
#[derive(Clone, Debug, Default)]
pub struct AmmoRules {
    /// `MaxStackSize` of the firearm's ammo item (the most one inventory stack holds).
    pub max_stack: Vec<u32>,
    /// `DropCount` of the ammo item (what one floor pickup grants).
    pub drop_count: Vec<u32>,
    /// Ammo the fresh player starts with: the sum of the game mode's `StartingItems` entries that are this ammo item (the data has none).
    pub starting: Vec<u32>,
}
impl AmmoRules {
    pub fn from_setup(s: &fn_pose::loadout_ammo::AmmoSetup, guns: &[GunData]) -> AmmoRules {
        let mut r = AmmoRules::default();
        let start = s.starting_ammo();
        for g in guns {
            let i = s.def_index(&g.def.ammo);
            let max = i.map_or(u32::MAX, |i| s.defs[i].max_stack);
            r.max_stack.push(max); r.drop_count.push(i.map_or(0, |i| s.defs[i].drop_count));
            r.starting.push(start.iter().filter(|(j, _)| Some(*j) == i).map(|(_, c)| (*c).max(0) as u32).sum::<u32>().min(max));
        }
        r
    }
    fn max_stack(&self, g: usize) -> u32 { self.max_stack.get(g).copied().unwrap_or(u32::MAX) }
    fn starting(&self, g: usize) -> u32 { self.starting.get(g).copied().unwrap_or(0) }
}
#[derive(Clone, Debug)]
pub struct ArmsData { pub guns: Vec<GunData>, pub healables: Vec<HealableData>, pub start_bandages: u32, pub ammo: AmmoRules, /// `CoreAnims_Male` key of the consumables (`B_UtilityItem_Generic` CDO: TrapPlacement = the unarmed NoWep set; `loadout_consumable`).
    pub consumable_core: &'static str,
    /// CONTAINERS: `GA_DefaultPlayer_InteractSearch`'s montage (`Search`, UpperBody) played while a container search hold runs; None = not loaded (reported).
    pub search: Option<MontageData>,
    pub door: Option<MontageData>,
    pub interact_curves: [Vec<(String, Vec<(f32, f32)>)>; 2],
    pub death: Option<crate::death_hook::DeathData> }

/// The `CoreAnims_Male` key as the animation graph's static table spells it (every key of the generated table is accepted).
fn core_key(c: &str) -> Result<&'static str, String> { fn_animgraph::CORE_ANIMS_MALE.iter().map(|(k, _)| *k).find(|k| *k == c).ok_or_else(|| format!("unsupported weapon core animation {c}")) }

impl ArmsData {
    /// The runtime data of firearm `g` (gun index of `fn_pose::loadout::Resources::gun_*`: starters, then installed packs) from the retained original resources.
    pub fn gun_from_resources(r: &fn_pose::loadout::Resources, g: usize) -> Result<GunData, String> {
        let name = || r.gun_item(g).map_or_else(|| format!("gun {g}"), |i| i.definition.rsplit('/').next().unwrap_or_default().to_owned());
        let def = r.gun_definition(g).cloned().ok_or_else(|| format!("{}: firearm stat row missing", name()))?;
        let extras = r.gun_extras(g).cloned().ok_or_else(|| format!("{}: firearm stat extras missing", name()))?;
        let core = core_key(r.gun_item(g).and_then(|i| i.core_animation.as_deref()).ok_or_else(|| format!("{}: firearm lacks a core animation", name()))?)?;
        let a = def.assets.clone();
        let optional_mont = |path: &Option<String>| -> Result<Option<MontageData>, String> { path.as_ref().map(|p| r.montages.get(p).ok_or_else(|| format!("montage {p} was not loaded")).and_then(|m| MontageData::from_anim_in(m, &r.sequences))).transpose() };
        let required = |path: &Option<String>, role: &str| -> Result<MontageData, String> { optional_mont(path)?.ok_or_else(|| format!("{}: the weapon's Blueprint chain sets no {role} montage", name())) };
        let cadence = Cadence { burst_firing_rate: extras.burst_firing_rate, auto_reload_delay_override: extras.auto_reload_delay_override, interrupt_immediate: extras.reload_interrupt_immediate, input_queue_percent: Cadence::INPUT_QUEUE_DEFAULT };
        let ammo_brush = fn_ui::screens::quickbar::MultiSizeBrush(extras.ammo_icon_brush.clone().map(|b| b.map(|(size, name, _)| (size, name))));
        Ok(GunData { cadence, max_spare: extras.max_spare_ammo, ammo_icon: extras.ammo_icon.clone(), ammo_brush, core, equip: required(&a.player_equip, "player equip")?, fire: required(&a.player_fire, "player fire")?, fire_ads: optional_mont(&a.player_fire_ads)?, reload: required(&a.player_reload, "player reload")?,
            weapon_fire: optional_mont(&a.weapon_fire)?, weapon_fire_ads: optional_mont(&a.weapon_fire_ads)?, weapon_reload: optional_mont(&a.weapon_reload)?, def })
    }
    /// Builds the runtime data from the retained original resources (every installed firearm; item 3 the bandage).
    pub fn from_resources(r: &fn_pose::loadout::Resources) -> Result<ArmsData, String> {
        let mut guns = vec![];
        for g in 0..fn_pose::loadout_gun::STARTER_GUNS { guns.push(Self::gun_from_resources(r, g)?); }
        let mut healables = vec![];
        for h in &r.healables {
            let montage = MontageData::from_anim_in(r.montages.get(&h.montage).ok_or_else(|| format!("montage {} was not loaded", h.montage))?, &r.sequences)?;
            healables.push(HealableData { name: h.name.clone(), rule: h.rule.clone(), trigger_duration: h.trigger_duration, montage, rate: h.montage_rate, cost: h.cost as u32, effect: h.effect.clone(), cooldown: h.cooldown, max_stack: h.max_stack });
        }
        if healables.is_empty() { return Err("no healing consumables were loaded".into()); }
        let ammo = AmmoRules::from_setup(&r.ammo, &guns);
        let consumable_core = core_key(r.items[3].core_animation.as_deref().ok_or("consumable lacks a core animation")?)?;
        let search = match r.montages.get(fn_pose::loadout::SEARCH_MONTAGE).ok_or_else(|| format!("montage {} was not loaded", fn_pose::loadout::SEARCH_MONTAGE)).and_then(|m| MontageData::from_anim_in(m, &r.sequences)) {
            Ok(d) => Some(d), Err(e) => { eprintln!("[arms] search montage unavailable ({e}): no search animation"); None }
        };
        let door = match r.montages.get(fn_pose::loadout::DOOR_OPEN_MONTAGE).ok_or_else(|| format!("montage {} was not loaded", fn_pose::loadout::DOOR_OPEN_MONTAGE)).and_then(|m| MontageData::from_anim_in(m, &r.sequences)) {
            Ok(d) => Some(d), Err(e) => { eprintln!("[arms] door-open montage unavailable ({e}): no tap interaction animation"); None }
        };
        let interact_curves = [fn_pose::loadout::SEARCH_MONTAGE, fn_pose::loadout::DOOR_OPEN_MONTAGE].map(|m| r.montage_curves.get(m).cloned().unwrap_or_default());
        let death = match crate::death_hook::DeathData::from_resources(r) { Ok(d) => Some(d), Err(e) => { eprintln!("[arms] death / hit-react montages unavailable ({e})"); None } };
        Ok(ArmsData { guns, healables, start_bandages: r.items[3].quantity, ammo, consumable_core, search, door, interact_curves, death })
    }
}

/// One frame of player intent for the arms (the keyboard path fills it from the quickbar keys / fire / reload keys, scripts from their actions).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArmsInput { pub select: Option<usize>, pub fire_held: bool, pub reload_press: bool, pub set_health: Option<f32>, pub set_shield: Option<f32>, /// Development hook: puts `n` of healable `idx` (HEALABLE_ITEMS order) into quickbar slot 5.
    pub give: Option<(usize, u32)>,
    /// AMMO HOOK, development: picks up `n` rounds of firearm `gun`'s ammo item (an ammo pickup; the stack is capped at the item's `MaxStackSize`).
    pub give_ammo: Option<(usize, u32)> }

/// Development switches (default: everything unrecovered stays missing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy { pub assume_unrecovered: bool }

#[derive(Clone, Debug)]
enum Act {
    None,
    Equip { t: f32 },
    /// A running `UFortGameplayAbility_Reload` (`fn_weapons::reload_ammo::Run`): the recovered play rate, timer-driven ammo transfer and section jumps; `mt` = the montage time of the previous frame (sound notify window).
    Reload { run: reload_ammo::Run, mt: f32 },
    /// A consumable use: healable index, montage time `t`, seconds since the `GameplayCue.Athena.Equipping` cue was added, `committed` once the TriggerAbility step ran.
    Heal { idx: usize, t: f32, cue: f32, committed: bool },
}

/// A sound the arms make (`gun` = firearm index into `ArmsData::guns`: 0..3 the starters (`fn_audio::weapon_events::Gun`), then every weapon installed by `weapon_runtime`; its sounds come from the weapon's own
/// Blueprint, `GunData::def.assets.actor`); positions are the front-end's business.
#[derive(Clone, Debug, PartialEq)]
pub enum ArmsSound {
    /// `PrimaryFireSound` at the weapon: a shot was fired.
    Fire { gun: usize },
    /// `OutOfAmmoSound` at the weapon: the trigger was pressed on an empty magazine.
    Empty { gun: usize },
    /// FIRE CADENCE: a trigger hold on an Automatic weapon started its fire loop (first shot of the hold) / stopped it (re-fire timer gone: release, empty, switch). Exe: fire start vfunc_201 0x9355d0, stops vfunc_202 0x93e020 / vfunc_222 0x91acf0 (audio agent).
    FireStart { gun: usize }, FireStop { gun: usize },
    /// A `PlayReloadFX` notify of the reload / equip montage of the held firearm: `ReloadSounds[stage]`.
    ReloadStage { gun: usize, stage: fn_audio::weapon_events::ReloadStage },
    /// An `AnimNotify_PlaySound` of a montage (equip, healing): the cue object path read from the notify, its pitch / volume multipliers.
    Notify { cue: String, pitch: f32, volume: f32 },
}

/// A running duration `GameplayEffect` (PurpleStuff): ticks `stacks x magnitude` every `period` for `duration` seconds.
#[derive(Clone, Debug)]
struct ActiveEffect { mods: Vec<(String, f32)>, start: f32, duration: f32, period: f32, ticks: u32 }

pub struct Arms {
    pub data: ArmsData,
    pub policy: Policy,
    /// Quickbar slot 0..5: at start 0 pickaxe, 1..3 the starter firearms, 4 bandages, 5 empty; `give_gun` puts any installed firearm on it.
    pub selected: usize,
    /// The live primary quickbar.  The entries keep their original starter-item
    /// identities while their slots may change through Athena's inventory panel.
    /// `None` is the empty sixth starter slot.
    pub quickbar: [Option<QuickbarItem>; SLOT_COUNT],
    pub health: f32, pub shield: f32,
    /// Magazine / inventory ammo stack per firearm (gun index = `data.guns` index; grows with [`Arms::add_gun`]).
    pub mags: Vec<u32>, pub reserve: Vec<u32>,
    pub loose_ammo: Vec<(String, u32)>,
    /// Stack size per healable (HEALABLE_ITEMS order) and the healable occupying quickbar slot 5.
    pub stock: Vec<u32>, pub slot5: usize,
    /// INVENTORY HOOK (see `arms/inventory_ops.rs`): which healable (index into `data.healables`) and how many of it (one stack) each quickbar slot holds when it holds `QuickbarItem::Healable`;
    /// `stock[idx]` stays the total of that healable over every slot stack and the stash. `slot5` follows the selected healable slot.
    pub slot_heal: [usize; SLOT_COUNT], pub slot_stack: [u32; SLOT_COUNT],
    /// INVENTORY HOOK: items the player owns that are not on the quickbar (the backpack side of the inventory).
    pub stash: Vec<inventory_ops::Stash>,
    /// INVENTORY HOOK: the slot focused before the current one (`FQuickBar::PreviousFocusedSlot`).
    pub prev_selected: Option<usize>,
    /// INVENTORY HOOK: items the player lost to the world since the last drain (drops, the item a full-bar pickup replaced): for the world-item system (`pickups_hook::queue_drop`).
    pub drops: Vec<inventory_ops::Dropped>,
    cooldown_until: Vec<f32>, effects: Vec<ActiveEffect>,
    clock: f32, /// FIRE CADENCE: pawn `+0xc5a` (queued pull), the pull re-fire timer (+0xBE8) and the reload-interrupt retry timer (+0xA58), as clock expiry times.
    firing_loop: Option<usize>,
    queued: bool, refire: Option<f32>, interrupt_retry: Option<f32>, fire_t: Option<f32>, /// `AFortWeapon::LastFireTime` (+0x954) per firearm: ctor -10 (RVA 0x914b60), set to World time at every shot.
    last_fire: Vec<f32>, fire_prev: bool, aiming: bool, act: Act,
    /// AMMO HOOK: clock time of the last ammo transfer (`AFortWeapon +0x99c`; `CanReload` needs more than 0.3 s since).
    last_transfer: f32,
    /// FIRE CADENCE: the rest of a multi-cartridge trigger pull (firearm, cartridges still to fire, clock time of the next one; `CartridgePerFire` > 1 rows only).
    burst: Option<(usize, u32, f32)>,
    /// FIRE CADENCE, `assume_unrecovered` only: a pending auto reload (firearm, clock time it starts) armed when a shot empties the magazine.
    auto_reload: Option<(usize, f32)>,
    pub events: Vec<String>,
    /// Sound moments of this frame (drained by the front-end with [`Arms::take_sounds`]).
    pub sounds: Vec<ArmsSound>,
    /// HEAL FX: particle notifies of this frame's montages (drained by the front-end with [`Arms::take_particles`]; `heal_fx.rs` spawns them).
    pub particles: Vec<fn_anim::sequence::ParticleNotify>,
    pub throws: Vec<usize>,
    /// Counters for tests / reports.
    pub shots: Vec<u32>,
    /// BUILD ANIM HOOK: montages stopped by a blueprint equip, blending out (see [`Arms::holster`]).
    fades: Vec<Fade>,
    /// BUILD ANIM HOOK: a building / editing tool is the pawn's current weapon (no reload, fire or consumable use; see [`Arms::set_blueprint_held`]).
    blueprint: bool,
    /// CONTAINERS: the search montage instance: (montage time, `Some(seconds since the stop, weight at the stop)` while blending out).
    interact: Option<(f32, Option<(f32, f32)>)>,
    interact_use: Option<f32>,
}
/// BUILD ANIM HOOK: a montage instance stopped by `Montage_Play` of another montage of its group (UE 4.16 `FAnimMontageInstance::Stop(NewMontage->BlendIn)`):
/// it keeps advancing at its play rate while its weight blends from `w0` to 0 over `dur` with the new montage's BlendIn option.
#[derive(Clone, Debug)]
struct Fade { m: MontageData, t: f32, rate: f32, w0: f32, elapsed: f32, dur: f32, opt: u8 }

pub const SLOT_COUNT: usize = 6;

/// Stable inventory identities of the original starter loadout.  These ids
/// are also what `AthenaInventoryPanel` sends to `FortInventoryContext`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickbarItem { Pickaxe, Gun(usize), Healable }

impl QuickbarItem {
    /// Ids 0 pickaxe, 1..=3 the starter firearms, 4 the consumable, 5..=7 the build resources (never on the quickbar); firearms installed later (gun index 3..) get id `gun + 5` (= 8..), which is also
    /// their entry in the inventory screen's catalog (`inventory::InventoryUi::add_catalog_item`).
    pub fn inventory_id(self) -> u32 {
        match self { Self::Pickaxe => 0, Self::Gun(g) if g < 3 => g as u32 + 1, Self::Gun(g) => g as u32 + 5, Self::Healable => 4 }
    }
    pub fn from_inventory_id(id: u32) -> Option<Self> {
        match id { 0 => Some(Self::Pickaxe), 1..=3 => Some(Self::Gun((id - 1) as usize)), 4 => Some(Self::Healable), 8.. => Some(Self::Gun((id - 5) as usize)), _ => None }
    }
}
/// Max health / max shield: the HUD bar maximum (`fn_ui::screens::vitals::MAX_VALUE`); the native health set defaults are not recovered.
pub const MAX_HEALTH: f32 = 100.;
pub const MAX_SHIELD: f32 = 100.;

impl Arms {
    pub fn new(data: ArmsData, policy: Policy) -> Arms {
        let mags: Vec<u32> = data.guns.iter().map(|g| g.def.clip_size).collect();
        // the fresh player's inventory: the game mode's StartingItems (original data: no ammo items), capped at each ammo item's MaxStackSize
        let reserve: Vec<u32> = (0..data.guns.len()).map(|i| data.ammo.starting(i)).collect();
        let n = data.guns.len();
        let mut stock = vec![0; data.healables.len()]; stock[0] = data.start_bandages; // the starter loadout grants five bandages (healable 0)
        let cooldown_until = vec![0.; data.healables.len()];
        Arms { data, policy, selected: 0, quickbar: [Some(QuickbarItem::Pickaxe), Some(QuickbarItem::Gun(0)), Some(QuickbarItem::Gun(1)), Some(QuickbarItem::Gun(2)), Some(QuickbarItem::Healable), None], health: 100., shield: 0., mags, reserve, loose_ammo: vec![], slot_stack: [0, 0, 0, 0, stock[0], 0], slot_heal: [0; SLOT_COUNT], stash: vec![], prev_selected: None, drops: vec![], stock, slot5: 0, cooldown_until, effects: vec![], clock: 0., firing_loop: None, queued: false, refire: None, interrupt_retry: None, last_fire: vec![-10.; n], fire_t: None, fire_prev: false, aiming: false, act: Act::None, last_transfer: f32::NEG_INFINITY, burst: None, auto_reload: None, events: vec![], sounds: vec![], particles: vec![], throws: vec![], shots: vec![0; n], fades: vec![], blueprint: false, interact: None, interact_use: None }
    }
    pub fn gun_index(&self) -> Option<usize> { match self.quickbar.get(self.selected).copied().flatten() { Some(QuickbarItem::Gun(g)) => Some(g), _ => None } }
    /// Called after the recovered camera targeting driver resolves its state for this frame.
    pub fn set_aiming(&mut self, aiming: bool) { self.aiming = aiming; }
    /// Quickbar slot 5 holds the consumable.
    pub fn consumable_selected(&self) -> bool { self.quickbar.get(self.selected).copied().flatten() == Some(QuickbarItem::Healable) }
    pub fn throwable_selected(&self) -> bool { self.consumable_selected() && self.data.healables.get(self.slot5).is_some_and(|h| matches!(h.rule, fn_pose::loadout_stats::HealRule::Throw { .. })) }
    pub fn pickaxe_selected(&self) -> bool { self.quickbar.get(self.selected).copied().flatten() == Some(QuickbarItem::Pickaxe) }
    /// Appends a firearm installed after start-up (gun index = the previous `data.guns.len()`): a full magazine, the ammo stack its ammo type already has (firearms sharing an ammo item share
    /// the stack) or the starting count, and the ammo-stack rules. Does NOT put it on the quickbar (see [`Arms::give_gun`]).
    pub fn add_gun(&mut self, gun: GunData, max_stack: u32, drop_count: u32, starting: u32) -> usize {
        let g = self.data.guns.len();
        let shared = self.data.guns.iter().position(|o| o.def.ammo == gun.def.ammo).map(|h| self.reserve[h]).or_else(|| self.take_loose_ammo(&gun.def.ammo).map(|n| n.min(max_stack)));
        self.mags.push(gun.def.clip_size);
        self.reserve.push(shared.unwrap_or(starting.min(max_stack)));
        self.shots.push(0); self.last_fire.push(-10.);
        self.data.ammo.max_stack.push(max_stack); self.data.ammo.drop_count.push(drop_count); self.data.ammo.starting.push(starting);
        self.data.guns.push(gun);
        g
    }
    /// Picks up firearm `g` (the console / weapon runtime `give`): `inventory_ops::pickup_gun` applies the 1.8 rules (`fn_weapons::inventory_model`): a firearm the player already has keeps its
    /// slot and gets its magazine refilled; a new one goes to the first empty slot that accepts it while the backpack (5) has room, otherwise it replaces the slot the focus scan reaches first
    /// (the focused slot, or slot 1 when the pickaxe is held) and the replaced item is dropped. Returns (slot, the item it replaced). `select` equips it (the equip montage plays).
    pub fn give_gun(&mut self, g: usize, select: bool) -> Result<(usize, Option<QuickbarItem>), String> {
        let rep = self.pickup_gun(g, select)?;
        if let Some(r) = rep.refusal { return Err(r.text().to_owned()); }
        let slot = rep.slot.or_else(|| self.quickbar.iter().position(|q| *q == Some(QuickbarItem::Gun(g)))).ok_or("the firearm went to the backpack (no slot)")?;
        let replaced = rep.replaced.map(|i| match i { inventory_ops::Item::Pickaxe => QuickbarItem::Pickaxe, inventory_ops::Item::Gun(o) => QuickbarItem::Gun(o), inventory_ops::Item::Heal { .. } => QuickbarItem::Healable });
        Ok((slot, replaced))
    }
    pub fn slot_of_inventory_item(&self, id: u32) -> Option<usize> { self.quickbar.iter().position(|item| item.map(QuickbarItem::inventory_id) == Some(id)) }
    /// Applies the recovered `DropItemOnQuickBar(item, Primary, slot)` intent
    /// to the local starter inventory.  The original item's identity follows
    /// the move; an occupied destination swaps back into the source slot.
    pub fn move_inventory_item_to_slot(&mut self, id: u32, slot: usize) -> Result<(), String> {
        // INVENTORY HOOK: `DropItemOnQuickBar` through the slot rules (`inventory_ops::put_on_slot`): the destination must accept the item's type, the focus follows a swap.
        let item = QuickbarItem::from_inventory_id(id).ok_or_else(|| format!("unknown inventory item {id}"))?;
        let loc = match self.slot_of_inventory_item(id) {
            Some(s) => inventory_ops::Loc::Slot(s),
            None => match item { QuickbarItem::Gun(g) => self.stash.iter().position(|s| *s == inventory_ops::Stash::Gun(g)).map(inventory_ops::Loc::Stash), _ => None }.ok_or_else(|| format!("inventory item {id} is absent"))?,
        };
        self.put_on_slot(loc, slot)
    }
    /// Low-ammo state of the held firearm: magazine <= ClipSize x LowAmmoPercentage (class default 0.25, `AFortWeaponItemDefinition` ctor RVA 0x915f10, per the audio agent; not re-verified here).
    pub fn low_ammo(&self) -> bool { self.gun_index().is_some_and(|g| (self.mags[g] as f32) <= self.data.guns[g].def.clip_size as f32 * 0.25) }
    pub fn is_reloading(&self) -> bool { matches!(self.act, Act::Reload { .. }) }
    pub fn cancel_heal(&mut self, why: &str) {
        let Act::Heal { idx, t, .. } = &self.act else { return };
        let h = &self.data.healables[*idx];
        let (m, t, rate) = (h.montage.clone(), *t, h.rate);
        let w0 = m.weight(t);
        let (dur, opt) = m.blend_out;
        if w0 > 0. { self.fades.push(Fade { m, t, rate, w0, elapsed: 0., dur, opt }); }
        self.cancel_action(why);
    }
    pub fn cancel_reload_by_sprint(&mut self) {
        let Some(g) = self.gun_index() else { return };
        let Act::Reload { run, .. } = &self.act else { return };
        let (m, t, rate) = (self.data.guns[g].reload.clone(), run.montage_time(self.clock), run.rate);
        let w0 = m.weight(t);
        let (dur, opt) = m.blend_out;
        if w0 > 0. { self.fades.push(Fade { m, t, rate, w0, elapsed: 0., dur, opt }); }
        self.cancel_action("by sprint");
    }
    pub fn is_healing(&self) -> bool { matches!(self.act, Act::Heal { .. }) }
    /// The AnimBP `CoreAnims_Male` key + `bIsTargeting` for the held item (`None` = pickaxe path / bandage hands).
    pub fn held_core(&self) -> Option<(&'static str, bool)> { if self.consumable_selected() { return Some((self.data.consumable_core, false)); } self.gun_index().map(|i| (self.data.guns[i].core, false)) }
    /// The held firearm's fire clock for the 1.8 native relaxed update (FortAnimInstance RVA 0x40efb0): `LastFireTime`, reloading (`Act::Reload`), equipping (`Act::Equip`, ASSUMED the
    /// equip montage window), `World->TimeSeconds` (this clock). `None` for the pickaxe / consumables.
    pub fn weapon_clock(&self) -> Option<fn_animgraph::WeaponClock> {
        self.gun_index().map(|g| fn_animgraph::WeaponClock { world_time: self.clock, last_fire_time: self.last_fire[g], equipping: matches!(self.act, Act::Equip { .. }), reloading: self.is_reloading(), charging: false })
    }
    fn log(&mut self, s: String) { self.events.push(format!("[arms t={:.3}] {s}", self.clock)); }
    pub fn take_events(&mut self) -> Vec<String> { std::mem::take(&mut self.events) }
    pub fn take_sounds(&mut self) -> Vec<ArmsSound> { std::mem::take(&mut self.sounds) }
    pub fn take_particles(&mut self) -> Vec<fn_anim::sequence::ParticleNotify> { std::mem::take(&mut self.particles) }
    pub fn take_throws(&mut self) -> Vec<usize> { std::mem::take(&mut self.throws) }
    /// Emits the sound notifies in `fx` (a montage's) that lie in the montage-time window `(from, to]`.
    fn emit_fx(&mut self, gun: Option<usize>, fx: &[FxNotify], from: f32, to: f32) {
        for n in fx {
            if n.time > from && n.time <= to {
                match (&n.fx, gun) {
                    (Fx::Sound { cue, pitch, volume }, _) => self.sounds.push(ArmsSound::Notify { cue: cue.clone(), pitch: *pitch, volume: *volume }),
                    (Fx::Reload(stage), Some(g)) => self.sounds.push(ArmsSound::ReloadStage { gun: g, stage: *stage }),
                    (Fx::Reload(_), None) => {}
                    (Fx::Particle(p), _) => self.particles.push(p.clone()),
                }
            }
        }
    }

    fn cancel_action(&mut self, why: &str) {
        match self.act { Act::None | Act::Equip { .. } => {} _ => { let a = format!("{:?}", self.act); self.log(format!("cancel {why} ({})", a.split_whitespace().next().unwrap_or(""))); } }
        self.act = Act::None;
    }

    /// BUILD ANIM HOOK (build_anim.rs): the building / editing tool was put away and the Primary bar's firearm is equipped again (`AFortWeapon::OnEquip`): its
    /// EquipAnimation plays from the start, the same `Act::Equip` a slot change starts (the selected slot itself did not change).
    /// The re-equip resets the weapon timers exactly like a slot change ([`Arms::select`]): the firearm went through `OnUnEquip` / `OnEquip` again.
    pub fn reequip(&mut self) {
        if let Some(g) = self.gun_index() { self.cancel_action("by re-equip"); self.clear_weapon_timers(); self.act = Act::Equip { t: 0. }; let s = format!("re-equip firearm {}", self.data.guns[g].def.stat_row); self.log(s); }
    }
    /// The held weapon's timers / pending pull die with its unequip (INFERENCE: the weapon actor is destroyed / hidden on unequip).
    fn clear_weapon_timers(&mut self) {
        self.fire_t = None;
        self.burst = None; self.auto_reload = None; self.refire = None; self.interrupt_retry = None; self.queued = false;
    }
    /// BUILD ANIM HOOK: the building / editing tool becomes the pawn's current weapon (`BuildingTool::OnEquip` 0x14092f5c0 / `EditingTool::OnEquip` 0x14092f7f0)
    /// while the Primary bar item is held: that item runs `AFortWeapon::OnUnEquip` (0x1409328b0), so a running reload / heal ends and its timers die exactly as on a
    /// slot change ([`Arms::select`]). Its playing montages are not cut: `Montage_Play(Blueprint_Equip)` stops the group's montages with the new montage's BlendIn
    /// (UE 4.16 `UAnimInstance::Montage_Play` -> `StopAllMontagesByGroupName(group, NewMontage->BlendIn)`), `blend` = (BlendIn time, option).
    pub fn holster(&mut self, blend: (f32, u8)) {
        let mut stopped: Vec<(MontageData, f32, f32)> = vec![];
        if let Some(g) = self.gun_index() {
            let d = &self.data.guns[g];
            match &self.act {
                Act::Equip { t } => stopped.push((d.equip.clone(), *t, 1.)),
                Act::Reload { run, .. } => stopped.push((d.reload.clone(), run.montage_time(self.clock), run.rate)),
                _ => {}
            }
            if let Some(t) = self.fire_t { stopped.push(((if self.aiming { d.fire_ads.as_ref().unwrap_or(&d.fire) } else { &d.fire }).clone(), t, 1.)); }
        }
        if let Act::Heal { idx, t, .. } = &self.act { let h = &self.data.healables[*idx]; stopped.push((h.montage.clone(), *t, h.rate)); }
        for (m, t, rate) in stopped { let w0 = m.weight(t); if w0 > 0. { self.fades.push(Fade { m, t, rate, w0, elapsed: 0., dur: blend.0, opt: blend.1 }); } }
        self.cancel_action("by blueprint equip");
        self.clear_weapon_timers();
        self.blueprint = true;
    }
    /// BUILD ANIM HOOK: whether a blueprint (building / editing tool) is in the hands. While it is, the firearm / consumable gets no reload, fire or use input:
    /// `AFortWeap_BuildingTool` / `_EditingTool` derive from `AFortWeapon` (no ammo, no reload) and their ctor leaves `bAllowTargeting` (+0x964) 0 (`AFortWeapon::AFortWeapon` 0x140914b60).
    pub fn set_blueprint_held(&mut self, held: bool) { self.blueprint = held; }
    /// BUILD ANIM HOOK: the montages blending out after [`Arms::holster`] (the only arms montages while a blueprint is held).
    /// CONTAINERS: the container search hold runs (`GA_DefaultPlayer_InteractSearch` active). Start: `PlayMontageInfoAndWaitForTrigger(MontageInfo)` from the
    /// Default section; stop (hold completed / cancelled -> the ability ends): CHOSEN as the GAS montage task's stop on ability end, the montage blends out
    /// with its BlendOut (no client code jumps to the `End` section, so `Search_End` is not played).
    pub fn set_interact_search(&mut self, on: bool) {
        let Some(m) = self.data.search.as_ref() else { return };
        match (on, self.interact) {
            (true, None) | (true, Some((_, Some(_)))) => { self.interact = Some((0., None)); self.log("interact: Search montage starts".into()); }
            (false, Some((t, None))) => { let w = m.weight(Self::search_time(m, t)).min(Self::blend_in_weight(m, t)); self.interact = Some((t, Some((0., w)))); }
            _ => {}
        }
    }
    /// CONTAINERS: montage time of the search montage after `t` seconds: `Default` (segment 0) then `Loop` (segment 1, `NextSectionName` = Loop: it repeats).
    fn search_time(m: &MontageData, t: f32) -> f32 {
        let Some(l) = m.segs.get(1) else { return t };
        let (a, d) = (l.start, l.duration().max(1e-3));
        if t < a + d { t } else { a + (t - a).rem_euclid(d) }
    }
    fn blend_in_weight(m: &MontageData, t: f32) -> f32 { if m.blend_in.0 <= 1e-6 { 1. } else { MontageData::ease(m.blend_in.1, t / m.blend_in.0) } }
    pub fn play_interact_use(&mut self) {
        if self.data.door.is_none() || self.interact_use.is_some() { return; }
        self.interact_use = Some(0.);
        self.log("interact: DoorOpen montage starts".into());
    }
    fn interact_use_montage(&self) -> Option<Montage> {
        let (m, t) = (self.data.door.as_ref()?, self.interact_use?);
        let (clip, time) = m.sample(t.min(m.length))?;
        let w = m.weight(t);
        (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend })
    }
    pub fn interact_curve(&self, name: &str) -> Option<(f32, f32)> {
        let mut acc: Option<(f32, f32)> = None;
        let mut add = |i: usize, t: f32, w: f32| {
            let Some((_, keys)) = self.data.interact_curves[i].iter().find(|(n, _)| n == name) else { return };
            let (s, ws) = acc.unwrap_or((0., 0.)); acc = Some((s + crate::build_anim::curve_at(keys, t) * w, ws + w));
        };
        if let (Some(m), Some((t, _))) = (self.data.search.as_ref(), self.interact) { if let Some(w) = self.search_montage().map(|x| x.weight) { add(0, Self::search_time(m, t), w); } }
        if let Some(t) = self.interact_use { if let Some(w) = self.interact_use_montage().map(|x| x.weight) { add(1, t, w); } }
        acc
    }
    pub fn mask_right_arm(&self) -> f32 { self.interact_curve("MaskRightArm").map_or(0., |(s, _)| s) }
    fn search_montage(&self) -> Option<Montage> {
        let (m, (t, stop)) = (self.data.search.as_ref()?, self.interact?);
        let mt = Self::search_time(m, t);
        let (clip, time) = m.sample(mt)?;
        let w = match stop { None => Self::blend_in_weight(m, t), Some((e, w0)) => if m.blend_out.0 <= 1e-6 { 0. } else { w0 * (1. - MontageData::ease(m.blend_out.1, e / m.blend_out.0)) } };
        (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend })
    }
    pub fn fade_montages(&self) -> Vec<Montage> {
        self.fades.iter().filter_map(|f| {
            let (clip, time) = f.m.sample(f.t.min(f.m.length))?;
            let w = if f.dur <= 1e-6 { 0. } else { f.w0 * (1. - MontageData::ease(f.opt, f.elapsed / f.dur)) };
            (w > 0.).then_some(Montage { slot: f.m.slot, clip, time, weight: w, blend: f.m.blend })
        }).collect()
    }

    pub fn select(&mut self, slot: usize) {
        if slot >= SLOT_COUNT || self.quickbar[slot].is_none() || slot == self.selected { return; }
        self.cancel_action("by slot change");
        self.clear_weapon_timers(); // the weapon actor's timers die with the unequip (INFERENCE: the weapon actor is destroyed / hidden on unequip)
        if self.selected < SLOT_COUNT { self.prev_selected = Some(self.selected); } // INVENTORY HOOK: PreviousFocusedSlot
        self.selected = slot;
        if matches!(self.quickbar[slot], Some(QuickbarItem::Healable)) { self.slot5 = self.slot_heal[slot]; } // INVENTORY HOOK: the held consumable slot decides which healable is active
        match self.gun_index() {
            Some(g) => { self.act = Act::Equip { t: 0. }; let d = &self.data.guns[g].def; let s = format!("equip slot {} firearm {} (mag {}/{} reserve {})", slot + 1, d.stat_row, self.mags[g], d.clip_size, self.reserve[g]); self.log(s); }
            None if self.consumable_selected() => { let s = format!("select slot {} {} x{}", slot + 1, self.data.healables[self.slot5].name, self.stock[self.slot5]); self.log(s); }
            None => self.log(format!("select slot {} pickaxe", slot + 1)),
        }
    }

    fn trigger_known(&self, g: usize) -> bool { self.policy.assume_unrecovered || self.data.guns[g].def.trigger != Trigger::NotSerialized }
    /// `AFortWeaponRanged::vfunc_214` (CanFire, RVA 0x91aa60, vslot +0x6b0): not reloading, weapon is the pawn's current weapon and `bIsEquippingWeapon` (+0x3a0, set while the
    /// EquipAnimation montage plays, RVA 0x9345d0) is clear, ammo left. (The pawn weapon-block counter +0xb7c / flag +0xb80 and the controller flag +0xf25 are not modelled.)
    fn can_fire(&self, g: usize) -> bool { self.trigger_known(g) && self.mags[g] >= self.data.guns[g].def.ammo_cost && !self.is_reloading() && !matches!(self.act, Act::Equip { .. }) && !self.is_healing() }

    /// Sets the inventory ammo stack of firearm `g`'s ammo item (firearms sharing the same ammo item share the stack).
    fn set_reserve(&mut self, g: usize, v: u32) {
        let ammo = self.data.guns[g].def.ammo.clone();
        for h in 0..self.reserve.len() { if self.data.guns[h].def.ammo == ammo { self.reserve[h] = v; } }
    }
    /// `AFortWeapon::CanReload` then `Reload` -> `UFortGameplayAbility_Reload::ActivateAbility` (`fn_weapons::reload_ammo`).
    fn start_reload(&mut self, g: usize) {
        if self.is_healing() { return; }
        let (clip, individual, rt) = { let d = &self.data.guns[g].def; (d.clip_size, d.individual_reload, d.reload_time) };
        let inp = reload_ammo::CanReloadInputs { has_ammo_definition: true, is_reloading: self.is_reloading(), inventory_ammo: self.reserve[g], magazine: self.mags[g], clip, seconds_since_last_transfer: self.clock - self.last_transfer };
        if let Err(e) = reload_ammo::can_reload(inp) { if e != reload_ammo::Refusal::AlreadyReloading { self.log(format!("reload refused (CanReload): {e:?}")); } return; }
        let rm = &self.data.guns[g].reload;
        let montage = reload_ammo::ReloadMontage::new(rm.length, rm.segs.iter().map(|s| s.start).collect());
        let run = reload_ammo::Run::start(self.clock, montage, clip, rt, individual);
        let (rate, first) = (run.rate, reload_ammo::first_timer_delay(&run.montage, run.rate));
        self.act = Act::Reload { run, mt: 0. };
        self.log(format!("reload start (ReloadTime {rt}s, montage play rate {rate:.4}, first transfer after {first:.3}s)"));
    }

    /// `AFortWeaponRanged::vfunc_210` (RVA 0x941d60) + `AFortWeapon::vfunc_210` (RVA 0x9416d0): the trigger-pull handler. `held` = pawn `+0xc58` (fire button down).
    /// Ported: cooldown test `E < I` with the input-queue window, reload interrupt, empty-magazine reload, CanFire, FIRE (RVA 0x9212d0), re-fire / auto-reload timers.
    fn try_fire(&mut self, g: usize, held: bool) {
        if !self.trigger_known(g) { return; }
        let (rate, trigger, cost, clip, individual) = { let d = &self.data.guns[g].def; (d.firing_rate, d.trigger.clone(), d.ammo_cost, d.clip_size, d.individual_reload) };
        let (i, e) = (1. / rate, self.clock - self.last_fire[g]);
        if e < i && !self.is_reloading() { // still on cooldown: Automatic always re-arms, OnPress only inside the last InputQueueTimePercent of it
            if trigger == Trigger::Automatic || (i - e) < i * self.data.guns[g].cadence.input_queue_percent { self.queued = true; self.refire = Some(self.clock + (i - e)); }
            return;
        }
        if let Some((bg, ..)) = self.burst { if bg == g { return; } } // a burst timer (+0xBF0) is still pending: no new pull
        // reload interrupt (AFortWeapon::vfunc_210): only a shell-by-shell reload can be interrupted, and with bReloadInterruptIsImmediate only once a shell is in
        let immediate = self.data.guns[g].cadence.interrupt_immediate;
        if self.is_reloading() && individual && (!immediate || self.mags[g] >= cost) {
            self.cancel_action("by fire (reload interrupt; the non-immediate outro variant FUN_14091ad20 is not modelled)");
            self.queued = true; self.interrupt_retry = Some(self.clock + 0.01); // timer of DAT_143ff9018 = 0.01 s, then vfunc_216 re-enters if the trigger is still held
            return;
        }
        if self.mags[g] < cost && clip > 0 { // empty magazine: a pull starts the reload when CanReload allows it; the click only when no ammo is left at all
            if self.mags[g] + self.reserve[g] < cost { self.sounds.push(ArmsSound::Empty { gun: g }); return; }
            if !self.is_reloading() { self.start_reload(g); }
            return;
        }
        if !self.can_fire(g) { return; }
        if !(self.queued || (held && trigger != Trigger::OnRelease)) { return; }
        self.queued = false;
        self.fire_cartridge(g);
        // post-fire scheduling (RVA 0x941d60 tail): Automatic with ammo left re-arms the pull timer for 1 / FireRate; otherwise, when the shot emptied the magazine and the
        // reserve is not empty, the auto-reload timer starts (GetAutoReloadDelay, RVA 0x922310). Burst rows are handled by the fire callback.
        let cpf = self.data.guns[g].def.cartridges;
        if cpf > 1 { self.after_burst_cartridge(g, 1); }
        if cpf <= 1 || self.mags[g] >= 1 {
            if trigger == Trigger::Automatic && self.mags[g] >= 1 { self.refire = Some(self.clock + i.max(1e-8)); }
            else if cpf <= 1 && self.mags[g] < cost && self.mags[g] + self.reserve[g] >= 1 { self.auto_reload = Some((g, self.clock + self.data.guns[g].cadence.auto_reload_delay())); }
        }
    }
    /// One cartridge: `AmmoCostPerFire` out of the magazine, the shot counter / `LastFireTime` / fire montage / fire sound.
    fn fire_cartridge(&mut self, g: usize) {
        let (cost, rate, dmg, bpc, stat) = { let d = &self.data.guns[g].def; (d.ammo_cost, d.firing_rate, d.damage_pb, d.bullets_per_cartridge, d.stat_row.clone()) };
        self.mags[g] -= cost; self.shots[g] += 1; self.last_fire[g] = self.clock;
        self.fire_t = Some(0.);
        // FIRE LOOP (audio): Automatic holds and persistent-fire weapons (`bPersistentFireFX`, e.g. the burst rifle: one start per burst) bracket their shots with FireStart / FireStop
        let persistent = self.data.guns[g].def.assets.actor.as_deref().is_some_and(fn_audio::weapon_events::bp_persistent_fire);
        if (self.data.guns[g].def.trigger == Trigger::Automatic || persistent) && self.firing_loop != Some(g) { self.firing_loop = Some(g); self.sounds.push(ArmsSound::FireStart { gun: g }); }
        self.sounds.push(ArmsSound::Fire { gun: g });
        let m = self.mags[g];
        self.log(format!("fire {stat}: mag {m} reserve {} (DmgPB {dmg} x {bpc} bullet(s), next shot in {:.3}s)", self.reserve[g], 1. / rate));
        let _ = (m, cost);
    }
    /// Tail of the fire callback RVA 0x9212d0 for `CartridgePerFire > 1` rows after cartridge number `fired`: out of ammo (server ammo < 1) -> auto-reload timer
    /// (`GetAutoReloadDelay`), else once `CartridgePerFire` cartridges are out the burst ends, else the callback re-arms for `1 / GetBurstFireRate` (RVA 0x923100).
    fn after_burst_cartridge(&mut self, g: usize, fired: u32) {
        let (rate, cpf) = { let d = &self.data.guns[g].def; (d.firing_rate, d.cartridges) };
        if self.mags[g] < 1 { self.burst = None; self.auto_reload = Some((g, self.clock + self.data.guns[g].cadence.auto_reload_delay())); }
        else if fired >= cpf { self.burst = None; }
        else { self.burst = Some((g, cpf - fired, self.clock + self.data.guns[g].cadence.burst_interval(rate, cpf))); }
    }
    /// The timer-driven part of the fire chain, in the engine's `FTimerManager::Tick` terms (a timer fires when `InternalTime > ExpireTime`, UE4.16 TimerManager.cpp:543):
    /// burst continuation (+0xBF0), pull re-fire (+0xBE8), reload-interrupt retry (+0xA58), auto reload (+0xBF8, callback vslot 0x7d8 = CanReload then Reload).
    fn tick_fire_timers(&mut self, held: bool) {
        if let Some((g, left, due)) = self.burst {
            if self.gun_index() != Some(g) { self.burst = None; }
            else if self.clock > due {
                let cpf = self.data.guns[g].def.cartridges;
                self.burst = None;
                if self.mags[g] >= self.data.guns[g].def.ammo_cost { self.fire_cartridge(g); self.after_burst_cartridge(g, cpf - left + 1); }
            }
        }
        if let Some(due) = self.interrupt_retry { if self.clock > due { self.interrupt_retry = None; if let Some(g) = self.gun_index() { if held { self.try_fire(g, held); } } } }
        if let Some(due) = self.refire { if self.clock > due { self.refire = None; if let Some(g) = self.gun_index() { self.try_fire(g, held); } } }
        if let Some((g, due)) = self.auto_reload {
            if self.clock > due { self.auto_reload = None; if self.gun_index() == Some(g) { self.start_reload(g); } }
        }
    }

    /// Advance by `dt` seconds. Returns nothing; events are collected in `events`.
    pub fn update(&mut self, dt: f32, inp: &ArmsInput) {
        // BUILD ANIM HOOK: no reload / fire / consumable use while a blueprint is the current weapon (see `set_blueprint_held`)
        let inp = &if self.blueprint { ArmsInput { fire_held: false, reload_press: false, ..*inp } } else { *inp };
        for f in &mut self.fades { f.t += dt * f.rate; f.elapsed += dt; }
        self.fades.retain(|f| f.elapsed < f.dur);
        // CONTAINERS: the search montage advances; its sound notify (Player_Search_Start_Cue at 0.03 in Default) fires once
        if let (Some(m), Some((t, stop))) = (self.data.search.clone(), self.interact) {
            let t2 = t + dt;
            if stop.is_none() { self.emit_fx(None, &m.fx, t, t2); }
            let stop = stop.map(|(e, w)| (e + dt, w));
            self.interact = if stop.is_some_and(|(e, _)| e >= m.blend_out.0) { None } else { Some((t2, stop)) };
        }
        if let (Some(m), Some(t)) = (self.data.door.as_ref(), self.interact_use) { self.interact_use = (t + dt < m.length).then_some(t + dt); }
        if let Some(h) = inp.set_health { self.health = h.clamp(0., MAX_HEALTH); let s = format!("health set to {}", self.health); self.log(s); }
        if let Some(sh) = inp.set_shield { self.shield = sh.clamp(0., MAX_SHIELD); let s = format!("shield set to {}", self.shield); self.log(s); }
        if let Some((idx, n)) = inp.give { if idx < self.stock.len() { self.set_consumable_slot(idx, n); let s = format!("[dev] slot 5 = {} x{n}", self.data.healables[idx].name); self.log(s); } }
        if let Some((g, n)) = inp.give_ammo { if g < self.data.guns.len() { let max = self.data.ammo.max_stack(g); let v = self.reserve[g].saturating_add(n).min(max); self.set_reserve(g, v); let s = format!("[dev] ammo pickup +{n} for firearm {g}: stack {v} (MaxStackSize {max})"); self.log(s); } }
        if let Some(s) = inp.select { self.select(s); }
        self.clock += dt;
        let press = inp.fire_held && !self.fire_prev; let released = !inp.fire_held && self.fire_prev; self.fire_prev = inp.fire_held;
        // ---- trigger ----
        if let Some(g) = self.gun_index() {
            if inp.reload_press { self.start_reload(g); }
            // pawn StartFire (RVA 0x7f8ac0, `+0xc58 = 1`, then weapon vslot 0x680 -> 0x690) / StopFire (RVA 0x7f8b80, weapon vslot 0x688 = `AFortWeaponRanged::vfunc_209`, RVA 0x938060)
            if press {
                if !self.trigger_known(g) { let s = self.data.guns[g].def.stat_row.clone(); self.log(format!("fire MISSING: {s} has no serialized TriggerType and its native default is not recovered")); }
                else if !self.is_healing() { self.try_fire(g, true); }
            }
            // OnRelease triggers fire when the trigger is let go: `AFortWeapon::vfunc_209` (StopFire, RVA 0x937f80) calls the trigger-pull handler (vslot 0x690) when vslot 0x6d0 holds, and `AFortWeapon::vfunc_217`
            // (RVA 0x93b890, the press-side check) returns 0 for TriggerType 2 (OnRelease): INFERENCE that vslot 0x6d0 is the release-side twin (not decoded); MinChargeTime / MaxChargeTime of the row are not modelled.
            if released && self.data.guns[g].def.trigger == Trigger::OnRelease && self.trigger_known(g) && !self.is_healing() { self.queued = true; self.try_fire(g, false); }
            if released && self.data.guns[g].def.trigger == Trigger::Automatic {
                // Ranged::vfunc_209: releasing an Automatic trigger kills the re-fire timer unless the magazine is empty and a reload can start
                if !(self.mags[g] < 1 && self.reserve[g] > 0) { self.refire = None; }
            }
        } else if self.consumable_selected() && press { self.try_heal(); }
        let was_equipping = matches!(self.act, Act::Equip { .. });
        self.tick_fire_timers(inp.fire_held);
        if let Some(g) = self.firing_loop { if self.gun_index() != Some(g) || (self.refire.is_none() && self.burst.is_none()) { self.firing_loop = None; self.sounds.push(ArmsSound::FireStop { gun: g }); } }
        // ---- advance the running action ----
        self.advance(dt);
        self.reap_used_up_stacks(); // INVENTORY HOOK: a used-up consumable stack leaves its slot
        // equip finished (`AFortWeapon::vfunc_282` RVA 0x930830 -> vfunc_216 RVA 0x91b960): fire if the trigger is held (Automatic or queued), then reload an under-filled magazine (CanReload)
        if was_equipping && !matches!(self.act, Act::Equip { .. }) {
            if let Some(g) = self.gun_index() {
                let (trig, cost) = { let d = &self.data.guns[g].def; (d.trigger.clone(), d.ammo_cost) };
                if inp.fire_held && (self.queued || trig != Trigger::OnPress) && self.can_fire(g) { self.try_fire(g, true); }
                if self.mags[g] < cost && !self.is_reloading() { self.start_reload(g); }
            }
        }
        self.tick_effects();
        // FIRE MONTAGE NOTIFIES: the fire montage's sound notifies (the pump-action rack = `PlayReloadFX(ReloadEnd)` of Shotgun_Rifle_PABF_FireNPump02_M_Athena at 0.6818 s,
        // and any `AnimNotify_PlaySound` of another weapon's fire montage) fire as the montage plays, exactly like the equip / reload / heal montages.
        let fire_g = self.gun_index();
        let fire_m = fire_g.map(|g| { let d = &self.data.guns[g]; if self.aiming { d.fire_ads.as_ref().unwrap_or(&d.fire) } else { &d.fire } });
        let fire_len = fire_m.map(|m| m.length);
        if let (Some(t0), Some(g), Some(m)) = (self.fire_t, fire_g, fire_m) {
            let t1 = t0 + dt;
            let (fx, len) = (m.fx.clone(), m.length);
            self.emit_fx(Some(g), &fx, if t0 <= 0. { -1. } else { t0 }, t1.min(len));
        }
        if let Some(t) = &mut self.fire_t { *t += dt; if fire_len.is_none_or(|len| *t >= len) { self.fire_t = None; } }
    }

    /// K2_CanActivateAbility of the equipped healable (ported from each GA bytecode).
    fn can_use(&self, idx: usize) -> Result<(), String> {
        let h = &self.data.healables[idx];
        use fn_pose::loadout_stats::HealRule;
        if self.stock[idx] < h.cost { return Err(format!("{} refused: none left", h.name)); }
        if self.cooldown_until[idx] > self.clock { return Err(format!("{} refused: cooldown effect active for {:.1} more s", h.name, self.cooldown_until[idx] - self.clock)); }
        let ok = match &h.rule {
            HealRule::Bandage { healing_cap, .. } => recovered_healing::bandage_health_gate(self.health, *healing_cap),
            HealRule::Medkit { .. } => (self.health.ceil()) < MAX_HEALTH,
            HealRule::Shield { .. } => (self.shield.ceil()) < MAX_SHIELD,
            HealRule::Purple | HealRule::Throw { .. } => true,
        };
        if ok { Ok(()) } else { Err(format!("{} refused: Athena.Ability.Failed.Consumable (health {} shield {})", h.name, self.health, self.shield)) }
    }
    /// Stacks the ability applies at its trigger (the `BP_ApplyGameplayEffectToOwner(.., Level 1, Stacks)` argument).
    fn stacks(&self, idx: usize) -> i32 {
        use fn_pose::loadout_stats::HealRule;
        match &self.data.healables[idx].rule {
            HealRule::Bandage { healing_cap, max_healing_amount } => recovered_healing::bandage_effect_level(self.health, *healing_cap, *max_healing_amount),
            HealRule::Medkit { healing_amount } => *healing_amount as i32,
            HealRule::Shield { shielding_amount } => (MAX_SHIELD - self.shield).clamp(0., *shielding_amount).trunc() as i32,
            HealRule::Purple => 1,
            HealRule::Throw { .. } => 0,
        }
    }
    fn apply_mod(&mut self, attribute: &str, amount: f32) {
        match attribute { "Healing" => self.health = (self.health + amount).min(MAX_HEALTH), "CurrentShield" => self.shield = (self.shield + amount).min(MAX_SHIELD), _ => {} }
    }
    fn try_heal(&mut self) {
        if self.is_healing() || self.is_reloading() { return; }
        let idx = self.slot5;
        if let Err(e) = self.can_use(idx) { self.log(e); return; }
        let h = &self.data.healables[idx];
        let (name, dur, rate) = (h.name.clone(), h.trigger_duration, h.rate);
        self.act = Act::Heal { idx, t: 0., cue: 0., committed: false };
        self.log(format!("{name} use start: GameplayCue.Athena.Equipping added (RawMagnitude {dur}), montage rate {rate}"));
    }

    fn advance(&mut self, dt: f32) {
        let act = std::mem::replace(&mut self.act, Act::None);
        self.act = match act {
            Act::None => Act::None,
            Act::Equip { t } => {
                let t0 = t; let t = t + dt;
                match self.gun_index() {
                    Some(g) => { let (fx, len) = (self.data.guns[g].equip.fx.clone(), self.data.guns[g].equip.length); self.emit_fx(Some(g), &fx, t0, t.min(len)); if t < len { Act::Equip { t } } else { Act::None } }
                    None => Act::None,
                }
            }
            Act::Reload { mut run, mt: mt0 } => {
                let Some(g) = self.gun_index() else { return };
                let (mut mag, mut inv) = (self.mags[g], self.reserve[g]);
                let events = run.advance(self.clock, &mut mag, &mut inv);
                let moved = events.iter().any(|e| matches!(e, reload_ammo::Event::Moved { .. }));
                self.mags[g] = mag; self.set_reserve(g, inv);
                for e in events {
                    match e {
                        reload_ammo::Event::Moved { at, n } => { self.last_transfer = at; let (m, r) = (self.mags[g], self.reserve[g]); self.log(format!("reload transfer at {:.3}s into the reload: +{n} -> mag {m} reserve {r}", at - run.start)); }
                        reload_ammo::Event::Blocked { at } => self.log(format!("reload transfer at {:.3}s moved nothing: jump to the last montage section", at - run.start)),
                        reload_ammo::Event::Finished { at } => self.log(format!("reload finished after {:.3}s (ReloadTime {})", at - run.start, run.duration)),
                    }
                }
                let _ = moved;
                // sound notifies in the montage time the animation plays: the loop section repeats, so a wrap splits the window
                { let m = self.data.guns[g].reload.clone();
                  let t1 = run.montage_time(self.clock);
                  if t1 >= mt0 { self.emit_fx(Some(g), &m.fx, mt0, t1); } else if let Some(lp) = m.segs.get(1) { self.emit_fx(Some(g), &m.fx, mt0, lp.start + lp.duration()); self.emit_fx(Some(g), &m.fx, lp.start - 1e-5, t1); }
                  if run.active() { Act::Reload { run, mt: t1 } } else { Act::None } }
            }
            Act::Heal { idx, t, cue, mut committed } => {
                let h = self.data.healables[idx].clone();
                let t0 = t; let t = t + dt * h.rate; // montage time advances at the MontageInfo play rate (1.0 unless serialized)
                self.emit_fx(None, &h.montage.fx, t0, t.min(h.montage.length));
                let cue = if committed { cue } else { cue + dt };
                let notify = h.montage.notify("TriggerAbility").unwrap_or(h.montage.length);
                let mut return_throw = false;
                if !committed && t >= notify {
                    committed = true; // K2_CommitAbility, then BP_ApplyGameplayEffectToOwner, then K2_RemoveGameplayCue(Equipping)
                    if self.stock[idx] >= h.cost {
                        self.stock[idx] -= h.cost;
                        self.consume_from_held_stack(idx, h.cost); // INVENTORY HOOK: the held slot's stack pays the cost
                        if let Some(c) = h.cooldown { self.cooldown_until[idx] = self.clock + c; }
                        if matches!(h.rule, fn_pose::loadout_stats::HealRule::Throw { .. }) {
                            self.throws.push(idx);
                            let left = self.stock[idx];
                            self.log(format!("{} TriggerAbility notify at montage {notify:.4}: commit (-{} item, {left} left), projectile spawned", h.name, h.cost));
                            return_throw = true;
                        }
                        if !return_throw {
                        let stacks = self.stacks(idx);
                        let (hb, sb) = (self.health, self.shield);
                        let mods: Vec<(String, f32)> = h.effect.mods.iter().map(|m| (m.attribute.clone(), m.magnitude * stacks as f32)).collect();
                        match (h.effect.duration, h.effect.period) {
                            (Some(d), Some(p)) if p > 0. => self.effects.push(ActiveEffect { mods, start: self.clock, duration: d, period: p, ticks: 0 }),
                            _ => for (a, v) in &mods { self.apply_mod(a, *v); },
                        }
                        let left = self.stock[idx];
                        self.log(format!("{} TriggerAbility notify at montage {notify:.4}: commit (-{} item, {left} left), effect {} x{stacks} stacks: health {hb} -> {}, shield {sb} -> {}; cue removed after {:.2}s of {}s", h.name, h.cost, h.effect.class.rsplit('/').next().unwrap_or_default(), self.health, self.shield, cue, h.trigger_duration));
                        }
                    }
                }
                if t >= h.montage.length { let n = h.name.clone(); self.log(format!("{n} use finished")); Act::None } else { Act::Heal { idx, t, cue, committed } }
            }
        };
    }

    /// Duration effects (PurpleStuff): one execution per elapsed Period within Duration.
    fn tick_effects(&mut self) {
        let mut effects = std::mem::take(&mut self.effects);
        for e in &mut effects {
            let elapsed = (self.clock - e.start).min(e.duration);
            let due = (elapsed / e.period).floor() as u32;
            while e.ticks < due { e.ticks += 1; for (a, v) in e.mods.clone() { self.apply_mod(&a, v); } }
        }
        let t = self.clock;
        effects.retain(|e| t - e.start < e.duration || e.ticks < (e.duration / e.period).floor() as u32);
        self.effects = effects;
    }

    /// Player montages for the animation graph this frame.
    pub fn montages(&self) -> Vec<Montage> {
        let mut out = vec![];
        let mk = |m: &MontageData, t: f32| -> Option<Montage> { let (clip, time) = m.sample(t)?; let w = m.weight(t); (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend }) };
        if let Some(g) = self.gun_index() {
            let d = &self.data.guns[g];
            match &self.act {
                Act::Equip { t } => out.extend(mk(&d.equip, *t)),
                Act::Reload { run, .. } => out.extend(mk(&d.reload, run.montage_time(self.clock))),
                _ => {}
            }
            if let Some(t) = self.fire_t { out.extend(mk(if self.aiming { d.fire_ads.as_ref().unwrap_or(&d.fire) } else { &d.fire }, t)); }
        }
        if let Act::Heal { idx, t, .. } = &self.act { out.extend(mk(&self.data.healables[*idx].montage, *t)); }
        out.extend(self.fade_montages()); // BUILD ANIM HOOK
        out.extend(self.search_montage()); // CONTAINERS: GA_DefaultPlayer_InteractSearch's Search montage
        out.extend(self.interact_use_montage());
        out
    }
    /// Maps the player reload montage time onto the weapon's own reload montage by section (same section index, same offset inside it).
    fn section_mapped(from: &MontageData, to: &MontageData, t: f32) -> f32 {
        if from.segs.len() < 2 || to.segs.len() != from.segs.len() { return t; }
        let i = from.segs.iter().rposition(|s| t >= s.start).unwrap_or(0);
        to.segs[i].start + (t - from.segs[i].start)
    }
    /// The held weapon's own animation (weapon skeleton): (firearm index, sequence package, time) from its fire / reload montage.
    pub fn weapon_clip(&self) -> Option<(usize, String, f32)> {
        let g = self.gun_index()?; let d = &self.data.guns[g];
        // a weapon whose Blueprint chain sets no weapon-mesh montage for a role (grenade launchers / Gatling / Zapper / AMR fire, Zapper reload, rocket launcher ADS fire) animates nothing for it
        let from_fire = || self.fire_t.and_then(|t| (if self.aiming { d.weapon_fire_ads.as_ref().or(d.weapon_fire.as_ref()) } else { d.weapon_fire.as_ref() })?.sample(t));
        let reload = match &self.act { Act::Reload { run, .. } => d.weapon_reload.as_ref().and_then(|wr| wr.sample(Self::section_mapped(&d.reload, wr, run.montage_time(self.clock)))), _ => None };
        // a shot interrupts a shell reload; otherwise the reload wins while it plays
        reload.or_else(from_fire).map(|(c, t)| (g, c, t))
    }
    /// The numbers `AthenaEquippedItemWidget::UpdateCurrentAmmo` receives for the held firearm: LocalCount = magazine, LocalRemaining = magazine + reserve (`None` for the pickaxe / bandage).
    pub fn ammo_state(&self) -> Option<fn_ui::screens::equipped_item::EquippedAmmo> {
        self.gun_index().map(|g| fn_ui::screens::equipped_item::EquippedAmmo { magazine: self.mags[g] as i32, remaining: (self.mags[g] + self.reserve[g]) as i32, icon: self.data.guns[g].ammo_icon.clone() })
    }
    /// Reload progress 0..1 (original `GetReloadProgress` arithmetic) when reloading with the ReloadTime duration.
    pub fn reload_progress(&self) -> f32 {
        match &self.act { Act::Reload { run, .. } => recovered_reload::progress(recovered_reload::Inputs { is_reloading: true, clock_sample: self.clock, last_reload_time: run.start, time_scale: 1., current_reload_duration: run.duration }), _ => 0. }
    }
    /// QUICKBAR AMMO: the number on each slot's item card (`SFortItemCard_StackCountBlock` refresh FUN_142fe9d10, see `fn_ui::screens::quickbar`):
    /// - a firearm (its ammo item differs from itself) shows `GetRemainingAmmo` (UFortWorldItem vfunc_98 @1406b7d10) = loaded + the ammo stack in the
    ///   inventory, even 0;
    /// - a consumable shows its own stack when its definition can stack (`MaxStackSize != 1`, @14068ec40), even 1 (only Badge items hide a 1);
    /// - the pickaxe (MaxStackSize 1, no ammo item) shows none.
    /// Firearms sharing an ammo item share `reserve` (`set_reserve`).
    pub fn slot_counts(&self) -> [Option<u32>; 6] {
        std::array::from_fn(|slot| match self.quickbar[slot] {
            Some(QuickbarItem::Gun(g)) => Some(self.mags[g] + self.reserve[g]),
            Some(QuickbarItem::Healable) => Some(self.slot_stack[slot]).filter(|c| *c > 0 && self.data.healables.get(self.slot_heal[slot]).is_some_and(|h| h.max_stack != 1)), // INVENTORY HOOK: each slot shows its own stack
            Some(QuickbarItem::Pickaxe) | None => None,
        })
    }
    /// QUICKBAR AMMO: the ammo icon brush on each slot's item card (`SFortItemCard_AmmoIconImage` refresh FUN_142fe5800): the firearm's ammo item
    /// `AmmoIconBrush`; None (Collapsed) for the pickaxe and consumables, which have no separate ammo item.
    pub fn slot_ammo_brushes(&self) -> [Option<fn_ui::screens::quickbar::MultiSizeBrush>; 6] {
        std::array::from_fn(|slot| match self.quickbar[slot] { Some(QuickbarItem::Gun(g)) => self.data.guns.get(g).map(|d| d.ammo_brush.clone()), _ => None })
    }
    /// `AthenaEquipProgress` (`HUD.Athena.EquipProgress`): while a consumable ability runs and its `GameplayCue.Athena.Equipping` cue is up (added at activation with
    /// `RawMagnitude = TriggerDuration`, removed when the ability triggers or ends): (seconds since the cue started, duration).
    pub fn equip_progress(&self) -> Option<(f32, f32)> {
        match &self.act { Act::Heal { idx, cue, committed: false, .. } if self.data.healables[*idx].trigger_duration > 0. => Some((*cue, self.data.healables[*idx].trigger_duration)), _ => None }
    }
    /// WIDGETS3 HOOK, `QuickbarSlotCooldown` feed: the running cooldown of each primary quickbar slot's item. A reloading firearm is a `WeaponReloading` cooldown whose time left
    /// comes from the recovered native `AFortWeapon::GetReloadProgress` (`fn_weapons::recovered_reload`); a consumable under its cooldown GameplayEffect (PurpleStuff) is an
    /// `ItemActivation` cooldown (the widget only draws the types the Athena slot supports, see `fn_ui::screens::slot_cooldown`).
    pub fn slot_cooldowns(&self) -> [Option<fn_ui::screens::slot_cooldown::SlotCooldown>; 6] {
        use fn_ui::screens::slot_cooldown::{CooldownKind, SlotCooldown};
        let mut out = [None; 6];
        if let Act::Reload { run, .. } = &self.act {
            let p = recovered_reload::progress(recovered_reload::Inputs { is_reloading: true, clock_sample: self.clock, last_reload_time: run.start, time_scale: 1.0, current_reload_duration: run.duration });
            out[self.selected.min(5)] = Some(SlotCooldown { kind: CooldownKind::WeaponReloading, remaining: (1. - p) * run.duration, duration: run.duration });
        }
        for slot in 0..SLOT_COUNT { // INVENTORY HOOK: every consumable slot, by its own healable (the cooldown is per item type)
            if !matches!(self.quickbar[slot], Some(QuickbarItem::Healable)) { continue; }
            let h = self.slot_heal[slot];
            if let Some(until) = self.cooldown_until.get(h) { if *until > self.clock && self.slot_stack[slot] > 0 { out[slot] = Some(SlotCooldown { kind: CooldownKind::ItemActivation, remaining: until - self.clock, duration: self.data.healables[h].cooldown.unwrap_or(0.) }); } }
        }
        out
    }
    pub fn state_line(&self) -> String { format!("slot {} health {} shield {} mags {:?} reserve {:?} stock {:?} act {:?}", self.selected + 1, self.health, self.shield, self.mags, self.reserve, self.stock, self.act) }
}
fn started_dur(now: f32, started: f32) -> String { format!("{:.2}", now - started) }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fn_weapons::Assets;

    fn seg(clip: &str, start: f32, a: f32, b: f32, rate: f32, loops: u32) -> Seg { Seg { clip: clip.into(), start, anim_start: a, anim_end: b, rate, loops } }
    fn m(slot: &'static str, len: f32, segs: Vec<Seg>, notifies: Vec<(&str, f32)>) -> MontageData { MontageData { length: len, blend_in: (0.1, 2), blend_out: (0.1, 2), slot, segs, notifies: notifies.into_iter().map(|(n, t)| (n.to_owned(), t)).collect(), fx: vec![], blend: fn_animgraph::SlotBlend::Pose } }
    fn def(name: &str, clip: u32, rate: f32, reload: f32, individual: bool, trigger: Trigger) -> Definition {
        // Each fixture gun has its own ammo definition.  This keeps the
        // fixture aligned with the per-gun inventory assertions below instead
        // of accidentally merging all three `give_ammo` pickups into one stack.
        Definition { item: name.into(), display_name: None, rarity: None, stat_row: name.into(), ammo: format!("ammo-{name}"), clip_size: clip, ammo_cost: 1, cartridges: 1, bullets_per_cartridge: 1, firing_rate: rate, reload_time: reload, individual_reload: individual, damage_pb: 10., critical_multiplier: 2.5, trigger, assets: Assets::default() }
    }
    fn gun(d: Definition, spare: u32, reload: MontageData) -> GunData {
        GunData { cadence: Cadence { burst_firing_rate: 5., auto_reload_delay_override: -1., interrupt_immediate: d.individual_reload, input_queue_percent: 0.2 }, max_spare: spare, ammo_icon: "T-Items-AmmoMedium.png".into(), ammo_brush: Default::default(), core: "AssaultRifle", equip: m("UpperBody", 0.667, vec![seg("eq", 0., 0., 0.667, 1., 1)], vec![]), fire: m("MeshRotationUpperBody", 0.333, vec![seg("fire", 0., 0., 0.333, 1., 1)], vec![]), fire_ads: None, reload,
            weapon_fire: Some(m("DefaultSlot", 0.333, vec![seg("wfire", 0., 0., 0.333, 1., 1)], vec![])), weapon_fire_ads: None, weapon_reload: Some(m("DefaultSlot", 2.667, vec![seg("wreload", 0., 0., 2.667, 1., 1)], vec![])), def: d }
    }
    pub(crate) fn data() -> ArmsData {
        let ar = gun(def("AR", 30, 5.5, 2.3, false, Trigger::Automatic), 120, m("UpperBody", 2.667, vec![seg("rl", 0., 0., 2.667, 1., 1)], vec![("TriggerAbility", 2.002)]));
        let shotgun_reload = m("UpperBody", 2.733, vec![seg("intro", 0., 0., 0.7, 1., 1), seg("loop", 0.7, 0., 0.7333, 1., 1), seg("outro", 1.4333, 0., 1.3, 1., 1)], vec![("TriggerAbility", 0.6744), ("TriggerAbility", 1.4031)]);
        let sg = gun(def("SG", 5, 0.7, 4.84, true, Trigger::NATIVE_DEFAULT), 104, shotgun_reload);
        let sn = gun(def("SN", 1, 0.33, 3.0, false, Trigger::NATIVE_DEFAULT), 102, m("UpperBody", 3., vec![seg("rl3", 0., 0., 3., 1., 1)], vec![("TriggerAbility", 2.2)]));
        ArmsData { guns: vec![ar, sg, sn], healables: healables(), start_bandages: 5, ammo: AmmoRules::default(), consumable_core: "TrapPlacement", search: None, door: None, interact_curves: Default::default(), death: None }
    }
    /// The four 1.8 healables with the numbers the data holds (bandage 75 / 15, medkit 100, shields 50, purple 24 x 1 per second + a 26 s cooldown effect).
    fn healables() -> Vec<HealableData> {
        use fn_pose::loadout_stats::{EffectDef, EffectMod, HealRule};
        let fx = |class: &str, mods: &[(&str, f32)], dur: Option<f32>, per: Option<f32>| EffectDef { class: class.into(), mods: mods.iter().map(|(a, v)| EffectMod { attribute: (*a).into(), magnitude: *v }).collect(), duration: dur, period: per };
        let mk = |name: &str, rule: HealRule, trig: f32, notify: f32, len: f32, effect: EffectDef, cooldown: Option<f32>| HealableData { name: name.into(), rule, trigger_duration: trig, montage: m("UpperBody", len, vec![seg(name, 0., 0., len, 1., 1)], vec![("TriggerAbility", notify)]), rate: 1., cost: 1, effect, cooldown, max_stack: 15 };
        vec![
            mk("Bandage", HealRule::Bandage { healing_cap: 75., max_healing_amount: 15. }, 3.5, 3.5046, 3.878, fx("GE_Bandage", &[("Healing", 1.)], None, None), None),
            mk("Medkit", HealRule::Medkit { healing_amount: 100. }, 10., 9.5, 10.5, fx("GE_Bandage", &[("Healing", 1.)], None, None), None),
            mk("Shields", HealRule::Shield { shielding_amount: 50. }, 5., 4.5, 5.2, fx("GE_Shields", &[("CurrentShield", 1.)], None, None), None),
            mk("Purple", HealRule::Purple, 2., 1.9, 2.4, fx("GE_Purple", &[("Healing", 1.), ("CurrentShield", 1.)], Some(24.), Some(1.)), Some(26.)),
            mk("Grenade", HealRule::Throw { initial_speed: 4000., gravity_scale: 0.7, projectile: "B_Prj_Athena_Grenade".into(), activation_sound: None }, 0., 0.42, 1.2, fx("", &[], None, None), None),
        ]
    }
    fn run(a: &mut Arms, secs: f32, inp: ArmsInput) { let n = (secs * 60.).round() as u32; for _ in 0..n { a.update(1. / 60., &inp); } }
    const ASSUMED: Policy = Policy { assume_unrecovered: true };
    const FIRE: ArmsInput = ArmsInput { select: None, fire_held: true, reload_press: false, set_health: None, set_shield: None, give: None, give_ammo: None };
    const NONE: ArmsInput = ArmsInput { select: None, fire_held: false, reload_press: false, set_health: None, set_shield: None, give: None, give_ammo: None };
    /// Picks up `n` rounds for each firearm (the three ammo items: medium bullets, shells, heavy bullets).
    fn stocked(a: &mut Arms, n: [u32; 3]) { for g in 0..3 { a.update(0., &ArmsInput { give_ammo: Some((g, n[g])), ..NONE }); } }

    #[test] fn a_tap_interaction_plays_door_open_once_and_its_curves_release_the_hand_ik() {
        let mut d = data();
        d.door = Some(MontageData { blend_out: (0.25, 2), blend_in: (0.15, 2), ..m("UpperBody", 0.8, vec![seg("DoorOpen", 0., 0., 0.8, 1., 1)], vec![]) });
        d.interact_curves[1] = vec![("BothHandFK".into(), vec![(0., 1.)]), ("MaskRightArm".into(), vec![(0., 1.)])];
        let mut a = Arms::new(d, ASSUMED);
        assert_eq!(a.interact_curve("BothHandFK"), None);
        a.play_interact_use();
        run(&mut a, 0.4, NONE);
        let (s, w) = a.interact_curve("BothHandFK").unwrap();
        assert!((s - 1.).abs() < 1e-4 && (w - 1.).abs() < 1e-4, "{s} {w}");
        assert!(a.mask_right_arm() > 0.99);
        assert!(a.montages().iter().any(|x| x.clip == "DoorOpen"));
        a.play_interact_use();
        run(&mut a, 0.45, NONE);
        assert_eq!(a.interact_curve("BothHandFK"), None, "the retap did not restart it: it ended at 0.8 s");
        assert_eq!(a.mask_right_arm(), 0.);
    }
    #[test] fn default_policy_leaves_unrecovered_behaviour_out() {
        let mut a = Arms::new(data(), Policy::default()); assert_eq!(a.reserve, [0, 0, 0], "no ammo is granted by the starter loadout");
        a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE }); run(&mut a, 0.8, NONE); run(&mut a, 0.2, FIRE); assert_eq!(a.shots[1], 1, "TriggerType not serialized = native default OnPress: exactly one shot per press, held fire does not repeat");
        a.mags[1] = 2; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(!a.is_reloading()); assert_eq!(a.mags[1], 2);
        a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 2.0, FIRE); assert!(a.shots[0] >= 2, "the serialized Automatic AR fires (after the shotgun's 1/0.7 s shared cooldown)");
        a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(40.), ..NONE }); run(&mut a, 0.1, FIRE); run(&mut a, 5.0, NONE); assert_eq!((a.health, a.stock[0]), (55., 4), "the bandage heal needs no switch: Blueprint stacks x GE magnitude 1.0");
    }
    #[test] fn grenade_throws_once_at_the_trigger_notify_and_pays_one_without_a_progress_ring() {
        let mut a = Arms::new(data(), Policy::default());
        a.update(0., &ArmsInput { give: Some((4, 3)), ..NONE });
        a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(50.), ..NONE });
        a.update(1. / 60., &FIRE);
        assert!(a.is_healing() && a.equip_progress().is_none());
        run(&mut a, 0.3, NONE); assert!(a.take_throws().is_empty() && a.stock[4] == 3);
        run(&mut a, 0.2, NONE); assert_eq!((a.take_throws(), a.stock[4], a.health), (vec![4], 2, 50.));
        run(&mut a, 1.0, NONE); assert!(!a.is_healing() && a.take_throws().is_empty());
    }
    #[test] fn starter_state_is_full_magazines_and_five_bandages() {
        let a = Arms::new(data(), ASSUMED); assert_eq!((a.mags.clone(), a.reserve.clone(), a.stock[0], a.selected), (vec![30, 5, 1], vec![0, 0, 0], 5, 0), "the game mode's StartingItems grant no ammo");
        assert_eq!(a.slot_counts()[4], Some(5)); assert!(a.held_core().is_none());
    }
    /// QUICKBAR AMMO: the card numbers follow `SFortItemCard_StackCountBlock` (FUN_142fe9d10): a firearm shows loaded + inventory ammo (even 0), a stackable
    /// consumable its stack even at 1, the pickaxe none; only firearms carry an ammo icon brush.
    #[test] fn card_numbers_follow_the_native_stack_count_block() {
        let mut a = Arms::new(data(), ASSUMED);
        assert_eq!(a.slot_counts(), [None, Some(30), Some(5), Some(1), Some(5), None]);
        a.set_reserve(0, 42); a.mags[1] = 0;
        assert_eq!(&a.slot_counts()[1..3], &[Some(72), Some(0)], "AR 30 + 42 in the inventory; the pump shows 0 with an empty magazine and no shells");
        a.slot_stack[4] = 1; assert_eq!(a.slot_counts()[4], Some(1), "a stackable consumable shows a count of 1 (only Badge items hide it)");
        let b = a.slot_ammo_brushes(); assert!(b[1].is_some() && b[3].is_some() && b[0].is_none() && b[4].is_none() && b[5].is_none());
    }
    #[test] fn inventory_quickbar_move_swaps_the_live_item_identity_and_selection() {
        let mut a = Arms::new(data(), ASSUMED);
        a.select(1);
        a.move_inventory_item_to_slot(1, 4).unwrap(); // AR over the bandage slot
        assert_eq!(a.selected, 4, "the selected AR follows its item to the destination");
        assert_eq!(a.gun_index(), Some(0));
        assert_eq!(a.quickbar[1], Some(QuickbarItem::Healable));
        assert_eq!(a.slot_counts()[1], Some(5));
        a.select(1);
        assert!(a.consumable_selected(), "the swapped bandage is now held from slot 2");
        assert!(a.move_inventory_item_to_slot(0, 1).is_err(), "the dedicated pickaxe slot is not a primary quickbar move");
    }
    #[test] fn selecting_a_firearm_plays_equip_and_sets_the_core_animation() {
        let mut a = Arms::new(data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE });
        assert_eq!(a.held_core(), Some(("AssaultRifle", false))); let m = a.montages(); assert_eq!(m.len(), 1); assert_eq!((m[0].slot, m[0].clip.as_str()), ("UpperBody", "eq"));
        run(&mut a, 0.8, NONE); assert!(a.montages().is_empty(), "equip montage finished");
        a.update(1. / 60., &ArmsInput { select: Some(0), ..NONE }); assert!(a.held_core().is_none()); a.update(1. / 60., &ArmsInput { select: Some(4), ..NONE }); assert_eq!(a.held_core(), Some(("TrapPlacement", false)), "consumables hold the unarmed core set");
    }
    #[test] fn automatic_rifle_fires_at_the_stat_rate_and_runs_dry() {
        let mut a = Arms::new(data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE); // the equip montage gates fire (bIsEquippingWeapon)
        run(&mut a, 1.0, FIRE);
        // 5.5 rounds per second: shots at 0, 0.1818, ... -> 6 in the first second (t = 0 .. 0.909 + the frame quantum)
        assert!((5..=7).contains(&a.shots[0]), "shots in 1 s: {}", a.shots[0]); assert_eq!(a.mags[0], 30 - a.shots[0]);
        run(&mut a, 8.0, FIRE); assert_eq!(a.mags[0], 0, "empties the magazine, never below zero"); assert_eq!(a.shots[0], 30);
    }
    #[test] fn aimed_fire_uses_the_recovered_ads_player_and_weapon_montages() {
        let mut d = data();
        d.guns[0].fire_ads = Some(m("MeshRotationUpperBody", 0.333, vec![seg("fire_ads", 0., 0., 0.333, 1., 1)], vec![]));
        d.guns[0].weapon_fire_ads = Some(m("DefaultSlot", 0.333, vec![seg("weapon_fire_ads", 0., 0., 0.333, 1., 1)], vec![]));
        let mut a = Arms::new(d, ASSUMED);
        a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE);
        a.update(1. / 60., &FIRE); a.set_aiming(true);
        assert_eq!(a.montages()[0].clip, "fire_ads");
        assert_eq!(a.weapon_clip().unwrap().1, "weapon_fire_ads");
        a.set_aiming(false);
        assert_eq!(a.montages()[0].clip, "fire");
        assert_eq!(a.weapon_clip().unwrap().1, "wfire");
    }
    #[test] fn press_only_weapons_need_a_new_press_per_shot() {
        let mut a = Arms::new(data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE }); run(&mut a, 0.8, NONE);
        run(&mut a, 0.5, FIRE); assert_eq!(a.shots[1], 1, "held trigger fires once");
        run(&mut a, 0.05, NONE); run(&mut a, 0.05, FIRE); assert_eq!(a.shots[1], 1, "still on cooldown (1/0.7 s)");
        run(&mut a, 1.5, NONE); run(&mut a, 0.05, FIRE); assert_eq!(a.shots[1], 2);
    }
    // ---- FIRE CADENCE ----
    fn burst_data() -> ArmsData { let mut d = data(); d.guns[0].def.cartridges = 3; d.guns[0].def.firing_rate = 1.75; d.guns[0].cadence.burst_firing_rate = 12.; d }
    #[test] fn a_multi_cartridge_row_fires_its_cartridges_at_the_burst_rate_then_waits_the_fire_interval() {
        let mut a = Arms::new(burst_data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE);
        run(&mut a, 1. / 60., FIRE); assert_eq!(a.shots[0], 1, "the first cartridge fires with the pull");
        run(&mut a, 1. / 60., NONE); assert_eq!(a.shots[0], 1, "the second is 1/12 s later, not on the next frame");
        run(&mut a, 0.25, NONE); assert_eq!((a.shots[0], a.mags[0]), (3, 27), "released trigger: the burst still completes (timer-driven), AmmoCostPerFire per cartridge");
        // held trigger: bursts start 1 / 1.75 s apart (the shot times land on frames: 0, 5, 10 / 35, 40, 45 / ...)
        let before = a.shots[0]; run(&mut a, 0.5, NONE); run(&mut a, 1.0, FIRE); assert_eq!(a.shots[0] - before, 6, "two bursts in the first second of a held pull: {}", a.shots[0] - before);
    }
    /// BUILD ANIM HOOK: equipping a blueprint cancels the running reload and the weapon timers (like a slot change), blends the reload montage out over the new
    /// montage's BlendIn instead of cutting it, blocks reload / fire while held, and the re-equip resets the timers again.
    #[test] fn blueprint_equip_holsters_the_firearm() {
        let mut a = Arms::new(data(), ASSUMED);
        a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE);
        a.mags[0] = 10; a.set_reserve(0, 120);
        a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); run(&mut a, 0.5, NONE);
        assert!(a.is_reloading());
        let w_reload = a.montages()[0].weight;
        a.holster((0.667, 2));
        assert!(!a.is_reloading() && a.fire_t.is_none() && a.refire.is_none() && a.auto_reload.is_none());
        let f = a.montages(); assert_eq!(f.len(), 1, "the reload montage blends out"); assert!((f[0].weight - w_reload).abs() < 1e-5);
        assert_eq!(a.fade_montages().len(), 1);
        run(&mut a, 0.3, NONE);
        let w = a.fade_montages()[0].weight; assert!((w - w_reload * (1. - MontageData::ease(2, 0.3 / 0.667))).abs() < 1e-3, "{w}");
        run(&mut a, 0.4, NONE); assert!(a.fade_montages().is_empty(), "gone after 0.667 s");
        // held: reload and fire do nothing
        a.mags[0] = 10; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(!a.is_reloading());
        let shots = a.shots[0]; run(&mut a, 0.5, FIRE); assert_eq!(a.shots[0], shots);
        // put away: the firearm re-equips with fresh timers
        a.set_blueprint_held(false); a.queued = true; a.refire = Some(1e9); a.reequip();
        assert!(!a.queued && a.refire.is_none() && matches!(a.act, Act::Equip { .. }));
    }
    #[test] fn equip_gates_fire_the_queue_window_admits_a_late_press_and_an_empty_pull_reloads() {
        let mut a = Arms::new(data(), ASSUMED); stocked(&mut a, [120, 104, 102]); a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE });
        run(&mut a, 0.3, FIRE); assert_eq!(a.shots[1], 0, "bIsEquippingWeapon blocks CanFire for the whole equip montage"); run(&mut a, 0.1, NONE); run(&mut a, 0.5, NONE);
        run(&mut a, 1. / 60., FIRE); assert_eq!(a.shots[1], 1); run(&mut a, 0.05, NONE);
        run(&mut a, 1. / 60., FIRE); run(&mut a, 0.05, NONE); assert_eq!(a.shots[1], 1, "OnPress pull in the first 60 % of the cooldown (queue window is 20 %): ignored");
        run(&mut a, 0.45, NONE); // 0.7 FiringRate -> I = 1.4286 s; now E ~ 0.62; window opens at E > 0.8 * I = 1.143
        run(&mut a, 0.6, NONE); run(&mut a, 1. / 60., FIRE); run(&mut a, 0.0, NONE); assert_eq!(a.shots[1], 1); run(&mut a, 0.3, NONE);
        assert_eq!(a.shots[1], 2, "pull inside the last InputQueueTimePercent: queued, fires when the cooldown ends");
        let mut b = Arms::new(data(), ASSUMED); stocked(&mut b, [120, 104, 102]); b.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut b, 0.8, NONE);
        b.mags[0] = 0; run(&mut b, 1. / 60., FIRE); assert!(b.is_reloading() && b.take_sounds().iter().all(|s| !matches!(s, ArmsSound::Empty { .. })), "empty pull with reserve: reload, no click");
    }
    #[test] fn releasing_an_automatic_trigger_cancels_the_refire_timer() {
        let mut a = Arms::new(data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE);
        run(&mut a, 1. / 60., FIRE); run(&mut a, 1. / 60., NONE); run(&mut a, 1.0, NONE); assert_eq!(a.shots[0], 1, "one pull, released before the first re-fire");
    }
    #[test] fn burst_rate_falls_back_to_fire_rate_times_cartridges_plus_one_and_the_auto_reload_delay_to_its_constant() {
        let c = Cadence { burst_firing_rate: 0., auto_reload_delay_override: -1., interrupt_immediate: false, input_queue_percent: 0.2 };
        assert!((c.burst_interval(1.75, 3) - 1. / (1.75 * 4.)).abs() < 1e-7);
        assert!((Cadence { burst_firing_rate: 12., ..c }.burst_interval(1.75, 3) - 1. / 12.).abs() < 1e-7);
        assert!((c.auto_reload_delay() - 0.3001).abs() < 1e-4, "{}", c.auto_reload_delay()); assert_eq!(Cadence { auto_reload_delay_override: 0.8, ..c }.auto_reload_delay(), 0.8);
        assert_eq!(Cadence { auto_reload_delay_override: 0., ..c }.auto_reload_delay(), 0., "0 is an override (>= 0)");
    }
    #[test] fn auto_reload_is_a_dev_stand_in_armed_by_the_last_round_with_the_rows_delay() {
        let mut d = data(); d.guns[2].cadence.auto_reload_delay_override = 0.8;
        for policy in [Policy::default(), ASSUMED] {
            let mut a = Arms::new(d.clone(), policy); stocked(&mut a, [0, 0, 20]); a.update(1. / 60., &ArmsInput { select: Some(3), ..NONE }); run(&mut a, 0.8, NONE);
            run(&mut a, 1. / 60., FIRE); assert_eq!(a.mags[2], 0); run(&mut a, 0.7, NONE); assert!(!a.is_reloading(), "{policy:?}: before the 0.8 s delay");
            run(&mut a, 0.2, NONE); assert!(a.is_reloading(), "{policy:?}: RVA 0x941d60 arms the auto-reload timer after the shot that empties the magazine");
        }
    }
    /// `weapon_clock` (the input of the native relaxed update): LastFireTime starts at -10 (ctor 0x914b60), a shot stamps the arms clock, reload / equip are flagged, no clock for the pickaxe.
    #[test] fn weapon_clock_reports_last_fire_time_reload_and_equip() {
        let mut a = Arms::new(data(), ASSUMED); assert!(a.weapon_clock().is_none(), "pickaxe: no clock");
        stocked(&mut a, [120, 104, 102]); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE });
        let c = a.weapon_clock().unwrap(); assert_eq!(c.last_fire_time, -10.); assert!(c.equipping && !c.reloading);
        run(&mut a, 2.0, NONE); let c = a.weapon_clock().unwrap(); assert!(!c.equipping, "the equip montage window ended");
        run(&mut a, 0.05, FIRE); let c = a.weapon_clock().unwrap(); assert!(c.last_fire_time > 0. && (c.world_time - c.last_fire_time) < 0.1, "the shot stamps World time: {c:?}");
        a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(a.weapon_clock().unwrap().reloading);
    }    #[test] fn rifle_reload_transfers_at_reload_time_and_ends_with_the_montage() {
        let mut a = Arms::new(data(), ASSUMED); stocked(&mut a, [120, 104, 102]); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE });
        run(&mut a, 1.0, FIRE); let fired = a.shots[0]; assert!(fired > 0);
        a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(a.is_reloading());
        run(&mut a, 2.2, NONE); assert_eq!(a.mags[0], 30 - fired, "montage plays at rate length/ReloadTime; the timer fires when it ends (2.3 s)");
        run(&mut a, 0.2, NONE); assert_eq!(a.mags[0], 30, "transferred at ReloadTime"); assert_eq!(a.reserve[0], 120 - fired);
        assert!(!a.is_reloading(), "the ability ends with the montage");
        assert!(a.take_events().iter().any(|e| e.contains("reload finished")));
    }
    #[test] fn reload_follows_can_reload() {
        let mut a = Arms::new(data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), reload_press: true, ..NONE }); assert!(!a.is_reloading(), "magazine full");
        a.mags[0] = 3; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(!a.is_reloading(), "no ammo in the inventory: CanReload refuses");
        assert!(a.take_events().iter().any(|e| e.contains("NoInventoryAmmo")));
        stocked(&mut a, [7, 0, 0]); a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(a.is_reloading());
        run(&mut a, 2.5, NONE); assert_eq!((a.mags[0], a.reserve[0]), (10, 0), "a short stack moves what exists");
    }
    #[test] fn ammo_without_a_firearm_goes_to_the_pool_and_joins_a_later_firearm() {
        let mut a = Arms::new(data(), ASSUMED);
        let light = "/Game/Athena/Items/Ammo/AthenaAmmoDataBulletsLight";
        assert_eq!(a.pickup_ammo_item(light, 999, 60), (60, 0));
        assert_eq!(a.pickup_ammo_item(light, 999, 990), (939, 51));
        assert_eq!(a.ammo_count(light), 999);
        assert_eq!(a.pickup_ammo_item(light, 999, 5), (0, 5));
        let mut g = a.data.guns[0].clone(); g.def.ammo = format!("{light}.AthenaAmmoDataBulletsLight");
        let idx = a.add_gun(g, 999, 30, 0);
        assert_eq!(a.reserve[idx], 999);
        assert!(a.loose_ammo.is_empty());
        assert_eq!(a.pickup_ammo_item("ammo-AR", 999, 7).0, 7);
        assert_eq!(a.reserve[0], 7);
        let mut a = Arms::new(data(), ASSUMED);
        a.pickup_ammo_item(light, 999, 40);
        assert_eq!(a.drop_loose_ammo(0, 15), Ok(15));
        assert_eq!(a.ammo_count(light), 25);
    }
    #[test] fn ammo_pickups_are_capped_at_the_item_max_stack() {
        let mut d = data(); d.ammo = AmmoRules { max_stack: vec![999; 3], drop_count: vec![12, 4, 4], starting: vec![0; 3] };
        let mut a = Arms::new(d, ASSUMED); stocked(&mut a, [900, 0, 0]); stocked(&mut a, [200, 0, 0]); assert_eq!(a.reserve[0], 999);
    }
    #[test] fn shotgun_reload_inserts_one_shell_per_loop_pass_and_firing_interrupts_it() {
        let mut a = Arms::new(data(), ASSUMED); stocked(&mut a, [0, 104, 0]); a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE });
        a.mags[1] = 2; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); assert!(a.is_reloading());
        // play rate = (5 x 0.7333 + 0.7 + 1.3) / 4.84 = 1.1708: the first shell goes in when intro + one loop pass have played (1.4333 / 1.1708 = 1.224 s)
        run(&mut a, 1.1, NONE); assert_eq!(a.mags[1], 2, "no shell before the first loop pass ends");
        run(&mut a, 0.2, NONE); assert_eq!(a.mags[1], 3, "first shell at (intro + loop) / rate");
        run(&mut a, 0.63, NONE); assert_eq!(a.mags[1], 4, "then one per loop pass (0.7333 / rate = 0.626 s)");
        // a shot interrupts immediately (bReloadInterruptIsImmediate), the inserted shells stay
        a.update(1. / 60., &ArmsInput { fire_held: true, ..NONE }); assert!(!a.is_reloading()); assert_eq!(a.mags[1], 4, "the pull cancels the reload, the shot follows 0.01 s later (retry timer)"); run(&mut a, 0.05, FIRE); assert_eq!(a.mags[1], 3); run(&mut a, 0.1, NONE);
        a.mags[1] = 0; a.set_reserve(1, 104); run(&mut a, 2.0, NONE); a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); run(&mut a, 4.6, NONE); assert!(a.is_reloading(), "still in the outro");
        run(&mut a, 0.4, NONE);
        assert_eq!(a.mags[1], 5, "five shells from empty"); assert_eq!(a.reserve[1], 99); assert!(!a.is_reloading(), "a full reload lasts ReloadTime (4.84 s)");
    }
    #[test] fn bandage_gate_commit_and_stacks_follow_the_original_arithmetic() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(100.), ..NONE });
        run(&mut a, 0.1, FIRE); assert!(!a.is_healing(), "health 100 is not below HealingCap 75"); run(&mut a, 0.1, NONE);
        a.update(1. / 60., &ArmsInput { set_health: Some(40.), ..NONE }); run(&mut a, 0.1, FIRE); assert!(a.is_healing());
        run(&mut a, 3.5 - 0.3, NONE); assert_eq!((a.health, a.stock[0]), (40., 5), "nothing applied before the TriggerAbility notify (3.5046 s at rate 1.0)");
        run(&mut a, 0.5, NONE); assert_eq!((a.health, a.stock[0]), (55., 4), "min(15, 75 - 40) stacks of Healing +1, one bandage consumed");
        run(&mut a, 0.5, NONE); assert!(!a.is_healing());
        a.update(1. / 60., &ArmsInput { set_health: Some(70.), ..NONE }); run(&mut a, 0.1, NONE); run(&mut a, 0.05, FIRE); run(&mut a, 5.0, NONE); assert_eq!(a.health, 75., "capped by 75 - health = 5");
    }
    #[test] fn the_use_progress_ring_runs_for_trigger_duration_and_ends_at_the_trigger() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(40.), ..NONE });
        assert!(a.equip_progress().is_none()); run(&mut a, 0.1, FIRE);
        let (e, d) = a.equip_progress().expect("the Equipping cue is up while the ability runs"); assert_eq!(d, 3.5); assert!(e > 0. && e < 0.2);
        run(&mut a, 1.9, NONE); let (e, _) = a.equip_progress().unwrap(); assert!((e - 2.0).abs() < 0.05, "elapsed {e}");
        run(&mut a, 1.7, NONE); assert!(a.equip_progress().is_none(), "K2_RemoveGameplayCue(Equipping) when the ability triggers");
    }
    #[test] fn medkit_heals_100_up_to_max_health_and_needs_health_below_max() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((1, 2)), set_health: Some(100.), ..NONE });
        run(&mut a, 0.1, FIRE); assert!(!a.is_healing(), "ceil(health) < max health is the gate");
        a.update(1. / 60., &ArmsInput { set_health: Some(20.), ..NONE }); run(&mut a, 0.1, NONE); run(&mut a, 0.05, FIRE); assert!(a.is_healing());
        assert_eq!(a.equip_progress().unwrap().1, 10.); run(&mut a, 9.0, NONE); assert_eq!((a.health, a.stock[1]), (20., 2)); run(&mut a, 1.0, NONE);
        assert_eq!((a.health, a.stock[1]), (100., 1), "100 stacks, clamped to the 100 maximum");
    }
    #[test] fn shields_fill_up_to_50_per_use_and_stop_at_max_shield() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((2, 3)), set_shield: Some(30.), ..NONE });
        run(&mut a, 0.1, FIRE); run(&mut a, 5.5, NONE); assert_eq!((a.shield, a.stock[2], a.health), (80., 2, 100.), "stacks = min(50, 100 - 30) = 50");
        run(&mut a, 0.1, NONE); run(&mut a, 0.05, FIRE); run(&mut a, 5.5, NONE); assert_eq!((a.shield, a.stock[2]), (100., 1), "stacks = min(50, 100 - 80) = 20");
        run(&mut a, 0.1, NONE); run(&mut a, 0.05, FIRE); assert!(!a.is_healing(), "ceil(shield) < max shield is the gate");
    }
    #[test] fn purple_ticks_each_period_for_the_duration_and_then_has_a_cooldown() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((3, 2)), set_health: Some(10.), set_shield: Some(0.), ..NONE });
        run(&mut a, 0.1, FIRE); run(&mut a, 2.5, NONE); assert!(!a.is_healing()); assert_eq!(a.stock[3], 1);
        run(&mut a, 0.05, FIRE); assert!(!a.is_healing(), "the 26 s cooldown effect (started at the commit) blocks a second use");
        run(&mut a, 0.1, NONE);
        let (h0, s0) = (a.health, a.shield); run(&mut a, 10.0, NONE); assert!(a.health >= h0 + 9. && a.shield >= s0 + 9., "+1 health and +1 shield per second: {} {}", a.health, a.shield);
        run(&mut a, 20.0, NONE); assert_eq!((a.health, a.shield), (10. + 24., 24.), "24 ticks in total (Duration 24 / Period 1)");
        run(&mut a, 0.1, NONE); run(&mut a, 0.05, FIRE); assert!(a.is_healing(), "usable again once the cooldown expired");
    }    #[test] fn changing_slot_cancels_an_unfinished_heal_without_consuming() {
        let mut a = Arms::new(data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(30.), ..NONE }); run(&mut a, 0.1, FIRE); assert!(a.is_healing());
        run(&mut a, 1.0, NONE); a.update(1. / 60., &ArmsInput { select: Some(0), ..NONE }); assert!(!a.is_healing() && a.equip_progress().is_none()); assert_eq!((a.health, a.stock[0]), (30., 5));
    }    #[test] fn montage_sampling_handles_loops_rates_and_blends() {
        let s = m("UpperBody", 2.0, vec![seg("a", 0., 1., 2., 0.5, 2)], vec![]);
        assert_eq!(s.sample(0.5).unwrap(), ("a".to_string(), 1.25)); assert_eq!(s.sample(2.5).unwrap(), ("a".to_string(), 1.25), "second loop pass starts over at AnimStart");
        assert!(s.weight(0.0) == 0. && (s.weight(1.0) - 1.).abs() < 1e-6 && s.weight(2.0) == 0.);
    }

    // ---- weapon sounds (the notify times / cues below are the ones read from the 1.8 montages; see `audio.rs` for the install-checked copy) ----
    use fn_audio::weapon_events::ReloadStage as Rs;
    const EQUIP_CUE: &str = "/Game/Sounds/Fort_GamePlay_Sounds/Player/Player_Equip_Projectile_Equip_Cue.Player_Equip_Projectile_Equip_Cue";
    fn fxs(v: &[(f32, Fx)]) -> Vec<FxNotify> { v.iter().map(|(t, f)| FxNotify { time: *t, fx: f.clone() }).collect() }
    fn sound_data() -> ArmsData {
        let mut d = data();
        for g in 0..3 { d.guns[g].equip.fx = fxs(&[(0.1803, Fx::Sound { cue: EQUIP_CUE.into(), pitch: 1., volume: 1. }), (0.2410, Fx::Reload(Rs::End))]); }
        d.guns[0].reload.fx = fxs(&[(0.4447, Fx::Reload(Rs::Start)), (1.3753, Fx::Reload(Rs::Cartridge)), (1.9062, Fx::Reload(Rs::End))]);
        d.guns[1].reload.fx = fxs(&[(0.0771, Fx::Reload(Rs::Start)), (0.7406, Fx::Reload(Rs::Cartridge)), (1.9331, Fx::Reload(Rs::End))]);
        let apply = |c: &str, p: f32| Fx::Sound { cue: format!("/Game/Sounds/Fort_GamePlay_Sounds/{c}.{c}"), pitch: p, volume: 1. };
        d.healables[0].montage.fx = fxs(&[(0.0393, apply("Player/Bandage_Apply_Athena_Cue", 1.)), (3.5086, apply("Loot/EatHealth_Restore_Athena_Cue", 1.5))]);
        d
    }
    #[test] fn equip_plays_its_notify_sound_and_the_reload_end_stage_at_the_notify_times() {
        let mut a = Arms::new(sound_data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE });
        assert!(a.take_sounds().is_empty(), "nothing before 0.18 s");
        run(&mut a, 0.5, NONE);
        assert_eq!(a.take_sounds(), vec![ArmsSound::Notify { cue: EQUIP_CUE.into(), pitch: 1., volume: 1. }, ArmsSound::ReloadStage { gun: 0, stage: Rs::End }]);
    }
    #[test] fn a_shot_and_a_press_on_an_empty_magazine_make_fire_and_empty_sounds() {
        let mut a = Arms::new(sound_data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE); a.take_sounds();
        run(&mut a, 0.05, FIRE); assert_eq!(a.take_sounds(), vec![ArmsSound::FireStart { gun: 0 }, ArmsSound::Fire { gun: 0 }]);
        run(&mut a, 0.2, NONE); a.mags[0] = 0; a.take_sounds();
        run(&mut a, 0.2, FIRE); assert_eq!(a.take_sounds(), vec![ArmsSound::Empty { gun: 0 }], "one click per press, no shot");
    }
    #[test] fn rifle_reload_plays_start_cartridge_end_in_order() {
        let mut a = Arms::new(sound_data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(1), ..NONE }); run(&mut a, 0.8, NONE); a.take_sounds();
        stocked(&mut a, [120, 104, 102]); a.mags[0] = 10; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); run(&mut a, 2.6, NONE);
        let s: Vec<_> = a.take_sounds().into_iter().filter_map(|s| if let ArmsSound::ReloadStage { stage, .. } = s { Some(stage) } else { None }).collect();
        assert_eq!(s, vec![Rs::Start, Rs::Cartridge, Rs::End]);
    }
    #[test] fn shotgun_reload_plays_one_cartridge_sound_per_loop_pass() {
        let mut a = Arms::new(sound_data(), ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE }); run(&mut a, 0.8, NONE); a.take_sounds();
        stocked(&mut a, [120, 104, 102]); a.mags[1] = 2; a.update(1. / 60., &ArmsInput { reload_press: true, ..NONE }); run(&mut a, 6., NONE); // 3 shells: intro, 3 loop passes (one shell at the end of each), outro
        let s: Vec<_> = a.take_sounds().into_iter().filter_map(|s| if let ArmsSound::ReloadStage { stage, .. } = s { Some(stage) } else { None }).collect();
        assert_eq!(s, vec![Rs::Start, Rs::Cartridge, Rs::Cartridge, Rs::Cartridge, Rs::End], "{s:?}");
    }
    /// PUMP FIX: the pump-action rack is `PlayReloadFX(ReloadEnd)` at 0.6818 s of the FIRE montage (Shotgun_Rifle_PABF_FireNPump02_M_Athena, length 1.4667 s); it must play once per shot, 0.68 s after the shot.
    #[test] fn shotgun_fire_cycle_plays_the_shot_then_the_pump_rack_from_the_fire_montage_notify() {
        let mut d = sound_data(); d.guns[1].fire.length = 1.4667; d.guns[1].fire.fx = fxs(&[(0.6818, Fx::Reload(Rs::End))]);
        let mut a = Arms::new(d, ASSUMED); a.update(1. / 60., &ArmsInput { select: Some(2), ..NONE }); run(&mut a, 0.8, NONE); a.take_sounds();
        let mut log: Vec<(f32, ArmsSound)> = vec![]; let t_fire = a.clock;
        for f in 0..120 { a.update(1. / 60., &if f == 0 { FIRE } else { NONE }); let t = a.clock - t_fire; for s in a.take_sounds() { log.push((t, s)); } }
        eprintln!("shotgun fire cycle sound events: {log:?}");
        assert_eq!(log.iter().map(|x| x.1.clone()).collect::<Vec<_>>(), vec![ArmsSound::Fire { gun: 1 }, ArmsSound::ReloadStage { gun: 1, stage: Rs::End }]);
        assert!(log[0].0 < 0.02 && (log[1].0 - 0.6818).abs() < 1. / 60. + 1e-3, "{log:?}");
    }
    #[test] fn bandage_plays_apply_then_the_restore_sound_at_the_trigger() {
        let mut a = Arms::new(sound_data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), set_health: Some(40.), ..NONE });
        run(&mut a, 0.1, FIRE); run(&mut a, 4., NONE);
        let cues: Vec<_> = a.take_sounds().into_iter().filter_map(|s| if let ArmsSound::Notify { cue, pitch, .. } = s { Some((cue.rsplit('/').next().unwrap().to_owned(), pitch)) } else { None }).collect();
        assert_eq!(cues, vec![("Bandage_Apply_Athena_Cue".to_owned(), 1.), ("EatHealth_Restore_Athena_Cue".to_owned(), 1.5)]);
    }

    // ---- HEAL FX: the montage's `AnimNotify_PlayParticleEffect` (P_Consume_Item on b_MF_Head, at the TriggerAbility time; see `audio.rs` for the install-checked copy) ----
    fn consume_item() -> fn_anim::sequence::ParticleNotify {
        fn_anim::sequence::ParticleNotify { template: "/Game/Effects/Fort_Effects/Gameplay/P_Consume_Item.P_Consume_Item".into(), socket: "b_MF_Head".into(), location_offset: [0.; 3], rotation_offset: [0.; 3], attached: true }
    }
    fn heal_fx_data() -> ArmsData { let mut d = data(); d.healables[1].montage.fx = fxs(&[(9.5, Fx::Particle(consume_item()))]); d } // Medkit: TriggerAbility at 9.5 in the test montage
    #[test] fn medkit_plays_the_consume_particle_once_at_the_trigger() {
        let mut a = Arms::new(heal_fx_data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((1, 2)), set_health: Some(20.), ..NONE });
        a.update(1. / 60., &FIRE); assert!(a.is_healing()); let t0 = a.clock - 1. / 60.; // the use starts on this frame (montage time advances by its dt)
        let mut log = vec![];
        for _ in 0..(11. * 60.) as usize { a.update(1. / 60., &NONE); for p in a.take_particles() { log.push((a.clock - t0, p)); } }
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(log[0].1, consume_item());
        assert!((log[0].0 - 9.5).abs() < 1. / 60. + 1e-3, "spawned at {} s", log[0].0);
        assert_eq!(a.health, 100.);
    }
    #[test] fn a_cancelled_medkit_plays_no_consume_particle() {
        let mut a = Arms::new(heal_fx_data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((1, 2)), set_health: Some(20.), ..NONE });
        run(&mut a, 0.05, FIRE); run(&mut a, 5., NONE); a.update(1. / 60., &ArmsInput { select: Some(0), ..NONE }); run(&mut a, 6., NONE);
        assert!(a.take_particles().is_empty()); assert_eq!(a.health, 20.);
    }
    #[test] fn a_refused_medkit_at_full_health_plays_no_consume_particle() {
        let mut a = Arms::new(heal_fx_data(), Policy::default()); a.update(1. / 60., &ArmsInput { select: Some(4), give: Some((1, 2)), set_health: Some(100.), ..NONE });
        run(&mut a, 0.05, FIRE); run(&mut a, 11., NONE); assert!(a.take_particles().is_empty());
    }
}
