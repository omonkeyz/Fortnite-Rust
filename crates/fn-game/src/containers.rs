//! CONTAINERS (fn-game, 2026-10-08): the Battle Royale loot containers as pure, renderer-free logic: the chest (`Tiered_Chest_6_Parent`) and the ammo box
//! (`Tiered_Short_Ammo_3_Parent`), their search (hold Use), the per-match spawn selection and the loot roll of the 1.8 Athena loot tables.
//! The app side (effects, sounds, prompt, the pickups the loot becomes) is `fn-app/src/chest_hook.rs`.
//!
//! DATA (cooked, read from the 1.8 paks; nothing typed in by hand):
//! * `/Game/Items/Datatables/AthenaLootTierData_Client` (`FFortLootTierData` rows) and `AthenaLootPackages_Client` (`FFortLootPackageData` rows).
//! * the tier-group redirect of `AFortGameModeAthena` (exe vfunc_341 @ 0x1404025b0): `Loot_Treasure` -> `Loot_AthenaTreasure`, `Loot_Ammo` ->
//!   `Loot_AthenaAmmoLarge`; `Loot_AthenaFloorLoot` / `Loot_AthenaAmmoLarge` / `Loot_AthenaAmmoSmall` / `Loot_AthenaSupplyDrop` pass through; every other
//!   group -> `None` (no loot): see [`athena_tier_group`].
//! * `FortAthenaMapInfo` placed in `Athena_Terrain` (`DefaultMapInfo_4`, class `DefaultMapInfo_C`): `TreasureChestClass` = `Tiered_Chest_6_Parent_C`,
//!   `TreasureChestMin/MaxSpawnPercent` = `FScalableFloat { Value 1.0, AthenaGameData Default.TreasureChest.Min/MaxSpawnPercent }` = 50 / 70 at level 0;
//!   `AmmoBoxClass` = `Tiered_Short_Ammo_3_Parent_C`, `AmmoBoxMin/MaxSpawnPercent` = `{ Value unset (0, the AFortAthenaMapInfo ctor @ 0x1403cca00 zeroes it),
//!   Default.AmmoBox.Min/MaxSpawnPercent }` (the rows are 0.65 / 0.8; FScalableFloat = Value x curve = 0).
//! * `/Game/Balance/SearchSpeed` (`SearchSpeedVerySlow` 3.5 s, `Slow` 2.3, `Normal` 1.3, `Fast` 1.0, `VeryFast` 0.65 ...).
use fn_pak::PakSet;
use fn_uasset::{PackageData, Value};
use std::collections::HashMap;

/// `AFortGameModeAthena` vfunc_341 (0x1404025b0): the Athena loot tier group of a container's `SearchLootTierGroup`; `None` = no loot.
pub fn athena_tier_group(group: &str) -> Option<&'static str> {
    match group {
        "Loot_Treasure" => Some("Loot_AthenaTreasure"),
        "Loot_Ammo" => Some("Loot_AthenaAmmoLarge"),
        "Loot_AthenaFloorLoot" => Some("Loot_AthenaFloorLoot"),
        "Loot_AthenaAmmoLarge" => Some("Loot_AthenaAmmoLarge"),
        "Loot_AthenaAmmoSmall" => Some("Loot_AthenaAmmoSmall"),
        "Loot_AthenaSupplyDrop" => Some("Loot_AthenaSupplyDrop"),
        _ => None,
    }
}

pub const TIER_TABLE: &str = "/Game/Items/Datatables/AthenaLootTierData_Client";
pub const PACKAGE_TABLE: &str = "/Game/Items/Datatables/AthenaLootPackages_Client";

/// One `FFortLootTierData` row.
#[derive(Clone, Debug, PartialEq)]
pub struct TierRow {
    pub name: String,
    pub tier_group: String,
    pub weight: f32,
    pub loot_tier: i32,
    pub loot_package: String,
    pub num_drops: f32,
    pub category_min: Vec<i32>,
    pub category_weight: Vec<f32>,
    pub category_max: Vec<i32>,
}
/// One `FFortLootPackageData` row.
#[derive(Clone, Debug, PartialEq)]
pub struct PackageRow {
    pub name: String,
    pub id: String,
    pub weight: f32,
    pub call: String,
    /// `ItemDefinition` object path (empty for a call row).
    pub item: String,
    pub count: i32,
    pub category: i32,
}

/// The two Athena loot tables.
#[derive(Clone, Debug, Default)]
pub struct LootTables {
    pub tiers: Vec<TierRow>,
    pub packages: Vec<PackageRow>,
    by_id: HashMap<String, Vec<usize>>,
}

fn rows(set: &PakSet, path: &str) -> Result<Vec<(String, serde_json::Value)>, String> {
    let pd = PackageData::load(set, path).map_err(|e| format!("{path}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{path}: {e}"))?;
    let tb = fn_uasset::classes::read_table(&pkg, 0).map_err(|e| format!("{path}: {e}"))?;
    Ok(tb.rows.into_iter().map(|(n, p)| (n, p.to_json(&pkg))).collect())
}
fn s(v: &serde_json::Value, k: &str) -> String { v[k].as_str().unwrap_or("").to_owned() }
fn f(v: &serde_json::Value, k: &str) -> f32 { v[k].as_f64().unwrap_or(0.) as f32 }
fn i(v: &serde_json::Value, k: &str) -> i32 { v[k].as_i64().unwrap_or(0) as i32 }
fn ia(v: &serde_json::Value, k: &str) -> Vec<i32> { v[k].as_array().map(|a| a.iter().map(|x| x.as_i64().unwrap_or(0) as i32).collect()).unwrap_or_default() }
fn fa(v: &serde_json::Value, k: &str) -> Vec<f32> { v[k].as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.) as f32).collect()).unwrap_or_default() }

impl LootTables {
    /// From the cooked rows (`fn-inspect props` JSON shape per row).
    pub fn from_rows(tiers: &[(String, serde_json::Value)], packages: &[(String, serde_json::Value)]) -> LootTables {
        let tiers = tiers.iter().map(|(n, r)| TierRow { name: n.clone(), tier_group: s(r, "TierGroup"), weight: f(r, "Weight"), loot_tier: i(r, "LootTier"), loot_package: s(r, "LootPackage"), num_drops: f(r, "NumLootPackageDrops"),
            category_min: ia(r, "LootPackageCategoryMinArray"), category_weight: fa(r, "LootPackageCategoryWeightArray"), category_max: ia(r, "LootPackageCategoryMaxArray") }).collect();
        let packages: Vec<PackageRow> = packages.iter().map(|(n, r)| PackageRow { name: n.clone(), id: s(r, "LootPackageID"), weight: f(r, "Weight"), call: s(r, "LootPackageCall"), item: s(r, "ItemDefinition"), count: i(r, "Count"), category: i(r, "LootPackageCategory") }).collect();
        let mut by_id: HashMap<String, Vec<usize>> = HashMap::new();
        for (k, p) in packages.iter().enumerate() { by_id.entry(p.id.clone()).or_default().push(k); }
        LootTables { tiers, packages, by_id }
    }
    /// Both tables from the paks.
    pub fn load(set: &PakSet) -> Result<LootTables, String> { Ok(Self::from_rows(&rows(set, TIER_TABLE)?, &rows(set, PACKAGE_TABLE)?)) }
    pub fn tiers_of<'a>(&'a self, group: &'a str) -> impl Iterator<Item = &'a TierRow> + 'a { self.tiers.iter().filter(move |t| t.tier_group == group) }
    pub fn package_rows(&self, id: &str) -> impl Iterator<Item = &PackageRow> { self.by_id.get(id).map(|v| v.as_slice()).unwrap_or(&[]).iter().map(|&k| &self.packages[k]) }
}

/// One `FCurveTableRowHandle`'s curve: the keys (time, value) of a cooked curve table row, linear between keys, constant outside (every key of the rows
/// used here is `RCIM_Linear` (0) / `RCCE_Constant`).
pub fn curve_row(set: &PakSet, table: &str, row: &str) -> Result<Vec<(f32, f32)>, String> {
    let table = table.split('.').next().unwrap_or(table);
    let pd = PackageData::load(set, table).map_err(|e| format!("{table}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{table}: {e}"))?;
    let tb = fn_uasset::classes::read_table(&pkg, 0).map_err(|e| format!("{table}: {e}"))?;
    let (_, p) = tb.rows.iter().find(|(n, _)| n == row).ok_or_else(|| format!("{table}: no row {row}"))?;
    Ok(p.get("Keys").and_then(Value::as_array).unwrap_or(&[]).iter().filter_map(|k| Some((k.field("Time")?.as_f32()?, k.field("Value")?.as_f32()?))).collect())
}
pub fn eval_keys(k: &[(f32, f32)], t: f32) -> f32 {
    let (Some(a), Some(b)) = (k.first(), k.last()) else { return 0. };
    if t <= a.0 { return a.1; }
    if t >= b.0 { return b.1; }
    for w in k.windows(2) { if t >= w[0].0 && t <= w[1].0 { let x = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-9); return w[0].1 + (w[1].1 - w[0].1) * x; } }
    b.1
}

/// Deterministic RNG for the rolls (xorshift64*); `next_f32` is in [0, 1).
#[derive(Clone, Debug)]
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng { Rng(seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }
    pub fn next_u64(&mut self) -> u64 { let mut x = self.0; x ^= x >> 12; x ^= x << 25; x ^= x >> 27; self.0 = x; x.wrapping_mul(0x2545_F491_4F6C_DD1D) }
    pub fn next_f32(&mut self) -> f32 { (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32 }
}

/// The Blueprint folder of the BR containers placed on the map (`Tiered_Chest_6_Parent`, `Tiered_Chest_3_Child`, `Tiered_Short_Ammo_3_Parent`, ...).
pub const CONTAINER_DIR: &str = "/Game/Building/ActorBlueprints/Containers";

/// One `FMeshSet` of a container's `AlternateMeshes` tier (`ABuildingSMActor`): what the loot tier swaps in.
#[derive(Clone, Debug, PartialEq)]
pub struct TierMeshSet { pub tier: i32, pub base_mesh: String, pub searched_mesh: String, pub search_speed_row: String, pub loot_spawn_location: [f32; 3] }

/// A container class's data as the Blueprint chain sets it (CDO properties, child first; the parent fills what the child leaves at its default).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassDef {
    pub class: String,
    /// The chain, child first (`Tiered_Chest_3_Child`, `Tiered_Chest_6_Parent`).
    pub chain: Vec<String>,
    pub tier_group: String,
    /// `SearchSpeed` (`FCurveTableRowHandle`): table + row.
    pub search_speed: (String, String),
    pub searching_loop: Option<String>,
    pub loot_repeat: Option<String>,
    pub loot_spawn_location: [f32; 3],
    pub num_items_range: [f32; 2],
    pub destroy_on_search: bool,
    pub searched_mesh: Option<String>,
    pub static_mesh: Option<String>,
    pub alternate: Vec<TierMeshSet>,
    /// The CDO properties merged over the chain (JSON, child first).
    pub props: serde_json::Map<String, serde_json::Value>,
}
impl ClassDef {
    /// Is this class (or a parent) `name` (short name, no `_C`)?
    pub fn is_a(&self, name: &str) -> bool { self.chain.iter().any(|c| c == name) }
    /// The `AlternateMeshes` set of a loot tier (exact tier).
    pub fn tier_set(&self, tier: i32) -> Option<&TierMeshSet> { self.alternate.iter().find(|t| t.tier == tier) }
}

/// Reads a container class (short name) and its Blueprint parents from the paks (`CONTAINER_DIR`, or `package` when given).
pub fn load_class(set: &PakSet, class: &str) -> Result<ClassDef, String> { load_class_at(set, &format!("{CONTAINER_DIR}/{class}"), class) }
pub fn load_class_at(set: &PakSet, package: &str, class: &str) -> Result<ClassDef, String> {
    let mut chain = vec![];
    let mut props = serde_json::Map::new();
    let mut cur = package.to_owned();
    for _ in 0..8 {
        let pd = PackageData::load(set, &cur).map_err(|e| format!("{cur}: {e}"))?;
        let pkg = pd.package().map_err(|e| format!("{cur}: {e}"))?;
        chain.push(cur.rsplit('/').next().unwrap_or("").to_owned());
        if let Some(Ok(cdo)) = pkg.read_default_object() {
            if let serde_json::Value::Object(m) = cdo.props.to_json(&pkg) { for (k, v) in m { props.entry(k).or_insert(v); } }
        }
        let sup = pkg.super_class().map(|r| r.path().to_owned()).unwrap_or_default();
        let next = sup.split('.').next().unwrap_or("").to_owned();
        if !next.starts_with("/Game/") || next == cur { break; }
        cur = next;
    }
    let st = |k: &str| props.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let v3 = |v: Option<&serde_json::Value>| -> [f32; 3] { v.and_then(|v| v.as_array()).map(|a| [0, 1, 2].map(|i| a.get(i).and_then(|x| x.as_f64()).unwrap_or(0.) as f32)).unwrap_or([0.; 3]) };
    let handle = |v: Option<&serde_json::Value>| -> (String, String) { v.map(|h| (h["CurveTable"].as_str().unwrap_or("").to_owned(), h["RowName"].as_str().unwrap_or("").to_owned())).unwrap_or_default() };
    let mut alternate = vec![];
    for t in props.get("AlternateMeshes").and_then(|v| v.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
        for m in t["MeshSets"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            alternate.push(TierMeshSet { tier: t["Tier"].as_i64().unwrap_or(0) as i32, base_mesh: m["BaseMesh"].as_str().unwrap_or("").to_owned(), searched_mesh: m["SearchedMesh"].as_str().unwrap_or("").to_owned(),
                search_speed_row: m["SearchSpeed"]["RowName"].as_str().unwrap_or("None").to_owned(), loot_spawn_location: v3(m.get("LootSpawnLocation")) });
        }
    }
    let nr = props.get("NumItemsToDropRange").and_then(|v| v.as_array()).map(|a| [a.first().and_then(|x| x.as_f64()).unwrap_or(0.) as f32, a.get(1).and_then(|x| x.as_f64()).unwrap_or(0.) as f32]).unwrap_or([0.; 2]);
    Ok(ClassDef { class: class.to_owned(), chain, tier_group: st("SearchLootTierGroup").unwrap_or_default(), search_speed: handle(props.get("SearchSpeed")), searching_loop: st("SearchingSoundCueLoop"), loot_repeat: st("LootRepeatSoundCue"),
        loot_spawn_location: v3(props.get("LootSpawnLocation")), num_items_range: nr, destroy_on_search: props.get("bDestroyContainerOnSearch").and_then(|v| v.as_bool()).unwrap_or(false),
        searched_mesh: st("SearchedMesh"), static_mesh: st("StaticMesh"), alternate, props })
}

// ---------------------------------------------------------------- search

/// The container search (`ABuildingContainer` as a hold interaction).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SearchState { #[default] Idle, Searching, Searched }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchEvent { Begin, Interrupted, Complete }
/// One frame of input for a container: the Use binding held / pressed this frame, whether the interaction trace has this container, the search time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchInput { pub use_down: bool, pub use_pressed: bool, pub focused: bool, pub duration: f32, pub cancel: bool }
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Search { pub state: SearchState, pub elapsed: f32, pub duration: f32 }
impl Search {
    pub fn active(&self) -> bool { self.state == SearchState::Searching }
    pub fn searched(&self) -> bool { self.state == SearchState::Searched }
    /// `Percentage` of the hold ring: elapsed / duration while searching, else 0.
    pub fn progress(&self) -> f32 { if self.active() && self.duration > 0. { (self.elapsed / self.duration).clamp(0., 1.) } else { 0. } }
    pub fn update(&mut self, i: &SearchInput, dt: f32) -> Vec<SearchEvent> {
        let mut ev = vec![];
        match self.state {
            SearchState::Searched => {}
            SearchState::Idle => {
                if i.focused && i.use_pressed {
                    self.state = SearchState::Searching; self.elapsed = 0.; self.duration = i.duration.max(0.); ev.push(SearchEvent::Begin);
                    if self.duration <= 0. { self.state = SearchState::Searched; ev.push(SearchEvent::Complete); }
                }
            }
            SearchState::Searching => {
                if !i.use_down || !i.focused || i.cancel { self.state = SearchState::Idle; self.elapsed = 0.; ev.push(SearchEvent::Interrupted); }
                else {
                    self.elapsed += dt;
                    if self.elapsed > self.duration { self.state = SearchState::Searched; ev.push(SearchEvent::Complete); } // PlayerTick @14083ee10: `now - start > RequiredDuration`
                }
            }
        }
        ev
    }
}

// ---------------------------------------------------------------- decompile-backed rules + CHOSEN server-side readings
//
// FROM THE CLIENT EXE (1.8 decompile, base 0x140000000):
// * Interaction (`FUN_14046ab90`): ContextText = loc `Fort.Building.Container.Defaults` / `SearchContainer` = "Search", `RequiredDuration` = `GetSearchTime()`,
//   `LongInteractSound` = `SearchingSoundCueLoop`, `bShowCountDown` false; `CanInteract` (`FUN_140464ae0`): unsearched = `IT_LongPress` only.
// * Press (`AFortPlayerController` vfunc_635 @140853950): start time = world time, BP `OnBeginSearch`, a `LongInteractAudioComponent` plays the loop on
//   the target's root. Tick (`PlayerTick` @14083ee10): complete when `now - start > RequiredDuration` -> `ServerAttemptInteract(.., IT_LongPress)`.
//   Cancel (vfunc_636 @140832230 -> BP `OnSearchInterrupted`): Use released (@140843190, `bTapInteractEnabled` off) or the focus moved to another actor /
//   nothing (`FUN_140840d70`). Cleanup `FUN_1408262e0`: the loop `FadeOut(1.0, 0)`. `InteractMultiplierForActiveInteraction` (3.0) has NO reader.
// * `GetSearchTime` @140477a70: `SearchSpeed` curve at X = 0, 0.75 s (DAT_143fe36f8) when the row does not resolve. `ABuildingSMActor::SelectMeshSet(tier)`
//   @14048c020 takes the `AlternateMeshes` set with the largest `Tier <= tier`; `ABuildingContainer::vfunc_417` @140461c10 copies its `SearchedMesh`,
//   `SearchSpeed` and `LootSpawnLocation`; nothing on the client calls it for these containers (only `AKeepItemContainer::InitializeContainer`).
//   `PostLoad` @140488910 swaps the `SearchSpeed` table for `AFortWorldSettings.SearchSpeedOverride`: `Athena_Terrain`'s FortWorldSettings sets
//   `/Game/Athena/AthenaSearchSpeed`, every row of which is 0.5 s. THE LIVE 1.8 CLIENT (offline match, fn_lightdump `get`, 2026-10-08) confirms it: chest
//   `SearchSpeed` = {AthenaSearchSpeed, SearchSpeedVerySlow}, ammo box {AthenaSearchSpeed, SearchSpeedNormal}, `ReplicatedLootTier` -1, `SearchedMesh`
//   = the class default (`TreasureChestLootTier1_Searched`): no tier mesh set is applied, so a BR search takes 0.5 s for both.
// * Athena game state (vftable 0x14406f3b0): `bAlwaysShowContainer` -> true (slot 0x8b0), `bDestroyContainerOnSearch` -> false (0x8b8): a searched BR
//   container is never destroyed, it is marked searched (`vfunc_428` @14048ccf0: `SearchedMesh` swapped in, BP `OnSetSearched`).
// NOT IN THE CLIENT (server code; `PickLootDrops` and the container's server search are the shared `return false` stub 0x142286760), CHOSEN here:
// * the spawn selection of `FortAthenaMapInfo` ([`select_spawned`]), the loot roll ([`LootTables::roll`]), the loot toss ([`toss_points`]).

/// `FortAthenaMapInfo` spawn percents at level 0 (module docs): chests `Value 1 x curve` = (50, 70); ammo boxes `Value 0 x curve` = (0, 0).
pub const TREASURE_CHEST_SPAWN_PERCENT: (f32, f32) = (50., 70.);
pub const AMMO_BOX_SPAWN_PERCENT: (f32, f32) = (0., 0.);

/// CHOSEN (server-only in 1.8): which of `n` placed containers of the map info's class spawn this match: a share drawn uniformly in
/// [min, max] percent, rounded, of random containers. A (0, 0) range culls nothing (the ammo box's FScalableFloat evaluates to 0, yet 1.8 matches had ammo
/// boxes everywhere: read as "no selection").
pub fn select_spawned(n: usize, min_pct: f32, max_pct: f32, rng: &mut Rng) -> Vec<bool> {
    if max_pct <= 0. { return vec![true; n]; }
    let pct = min_pct + (max_pct - min_pct) * rng.next_f32();
    let keep = ((n as f32) * pct / 100.).round() as usize;
    let mut idx: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() { let j = (rng.next_u64() % (i as u64 + 1)) as usize; idx.swap(i, j); }
    let mut out = vec![false; n];
    for &i in idx.iter().take(keep.min(n)) { out[i] = true; }
    out
}

/// One rolled drop: the item definition object path and the stack count.
#[derive(Clone, Debug, PartialEq)]
pub struct Drop { pub item: String, pub count: i32 }

fn pick<'a, T>(items: &'a [T], w: impl Fn(&T) -> f32, rng: &mut Rng) -> Option<&'a T> {
    let total: f32 = items.iter().map(|t| w(t).max(0.)).sum();
    if total <= 0. { return None; }
    let mut r = rng.next_f32() * total;
    for t in items { let x = w(t).max(0.); if x <= 0. { continue; } if r < x { return Some(t); } r -= x; }
    items.iter().rev().find(|t| w(t) > 0.)
}

impl LootTables {
    /// Resolves one package id to item drops (a call row recurses into its `LootPackageCall`; a weighted pick among the rows of the id).
    fn resolve(&self, id: &str, category: Option<i32>, rng: &mut Rng, out: &mut Vec<Drop>, depth: u32) {
        if depth > 8 { return; }
        let rows: Vec<&PackageRow> = self.package_rows(id).filter(|r| category.map_or(true, |c| r.category == c)).collect();
        let Some(r) = pick(&rows, |r| r.weight, rng) else { return };
        if !r.call.is_empty() { self.resolve(&r.call.clone(), None, rng, out, depth + 1); }
        else if !r.item.is_empty() && r.count > 0 { out.push(Drop { item: r.item.clone(), count: r.count }); }
    }
    /// CHOSEN (server-only in 1.8): the loot of one search of a container of Athena tier group `group`: one `FFortLootTierData` row by `Weight`; its
    /// `NumLootPackageDrops` (a fraction = the chance of one more); each category gets its `LootPackageCategoryMinArray` count, the rest go to categories
    /// by `LootPackageCategoryWeightArray`; each drop is a weighted row of `LootPackage` in that category, a `LootPackageCall` resolved the same way.
    /// The 1.8 Athena rows: a chest = 4 drops, one per category (weapon, its ammo, a consumable, a resource); an ammo box = 2 ammo drops.
    pub fn roll(&self, group: &str, rng: &mut Rng) -> Vec<Drop> {
        let tiers: Vec<&TierRow> = self.tiers_of(group).collect();
        let Some(t) = pick(&tiers, |t| t.weight, rng) else { return vec![] };
        let mut out = vec![];
        let drops = t.num_drops.floor() as i32 + (rng.next_f32() < t.num_drops.fract()) as i32;
        let mut counts = vec![0i32; t.category_min.len()];
        for (c, m) in t.category_min.iter().enumerate() { counts[c] = (*m).max(0); }
        let mut left = drops - counts.iter().sum::<i32>();
        let cats: Vec<usize> = (0..t.category_weight.len()).collect();
        while left > 0 {
            let Some(&c) = pick(&cats, |&c| t.category_weight[c], rng) else { break };
            counts[c] += 1; left -= 1;
        }
        for (c, n) in counts.iter().enumerate() { for _ in 0..*n { self.resolve(&t.loot_package, Some(c as i32), rng, &mut out, 0); } }
        out
    }
}

/// `GetSearchTime` fallback when the `SearchSpeed` row does not resolve (DAT_143fe36f8).
pub const SEARCH_TIME_FALLBACK: f32 = 0.75;
/// The selected `AlternateMeshes` set at loot tier `tier` (`SelectMeshSet`: largest `Tier <= tier`; None = the class has none, its own meshes stay).
pub fn mesh_set(d: &ClassDef, tier: i32) -> Option<&TierMeshSet> { d.alternate.iter().filter(|m| m.tier <= tier).max_by_key(|m| m.tier) }
/// The `SearchSpeed` row a container uses at loot tier `tier`: the selected set's, else the CDO's.
/// (The live client keeps the class default: `ReplicatedLootTier` stays -1, no set is selected; `tier` is kept for the reports only.)
pub fn search_speed_row(d: &ClassDef, _tier: i32) -> String { d.search_speed.1.clone() }
/// The `SearchedMesh` a container shows once searched at loot tier `tier` (the selected set's, else the class's).
pub fn searched_mesh(d: &ClassDef, _tier: i32) -> Option<String> { d.searched_mesh.clone() }
/// `GetSearchTime` (s) of a container class at its loot tier; `table` = the `SearchSpeed` rows at X = 0 ([`search_speed_table`]).
pub fn search_time(d: &ClassDef, tier: i32, table: &HashMap<String, f32>) -> f32 { table.get(&search_speed_row(d, tier)).copied().unwrap_or(SEARCH_TIME_FALLBACK) }
/// `Athena_Terrain`'s `FortWorldSettings.SearchSpeedOverride` (the table every container's `SearchSpeed` uses in BR, `PostLoad` @140488910).
pub const ATHENA_SEARCH_SPEED: &str = "/Game/Athena/AthenaSearchSpeed";
/// The BR search-speed table ([`ATHENA_SEARCH_SPEED`]) rows at X = 0.
pub fn search_speed_table(set: &PakSet) -> Result<HashMap<String, f32>, String> {
    let path = ATHENA_SEARCH_SPEED;
    let pd = PackageData::load(set, path).map_err(|e| format!("{path}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{path}: {e}"))?;
    let tb = fn_uasset::classes::read_table(&pkg, 0).map_err(|e| format!("{path}: {e}"))?;
    Ok(tb.rows.iter().map(|(n, p)| {
        let keys: Vec<(f32, f32)> = p.get("Keys").and_then(Value::as_array).unwrap_or(&[]).iter().filter_map(|k| Some((k.field("Time")?.as_f32()?, k.field("Value")?.as_f32()?))).collect();
        (n.clone(), eval_keys(&keys, 0.))
    }).collect())
}
/// The focus limit of the interaction trace from the pawn (m): `InteractDistance` 256 + `InteractTraceExtent` 15 (the pickups' / doors' reading). The
/// active interaction's 3.0 multiplier has no reader in 1.8: the same limit holds while a search runs.
pub fn interact_limit_m() -> f32 { crate::pickups::INTERACT_DISTANCE + crate::pickups::INTERACT_TRACE_EXTENT }
/// `Fort.Building.Container.Defaults` / `SearchContainer` (English).
pub fn search_text() -> &'static str { "Search" }
/// Cleanup of a finished / cancelled hold: the `LongInteractAudioComponent` `FadeOut(1.0, 0)` (`FUN_1408262e0`).
pub const SEARCH_LOOP_FADE: f32 = 1.0;

pub const JIGGLE_LOOT_MAGNITUDE: &str = "/Game/GameplayCurves/SeachLootVertexJiggleMagnitude";
pub const JIGGLE_LOOT_ANGLE: &str = "/Game/GameplayCurves/SeachLootVertexJiggleAngle";
pub const JIGGLE_EMPTY_MAGNITUDE: &str = "/Game/GameplayCurves/SeachEmptyLootVertexJiggleMagnitude";
pub const JIGGLE_EMPTY_ANGLE: &str = "/Game/GameplayCurves/SeachEmptyLootVertexJiggleAngle";
pub const BOUNCE_WINDOW: f32 = 5.0;

pub fn bounce_radius(d: &ClassDef) -> f32 {
    let o = d.props.get("SearchBounceRadiusOverride").and_then(|v| v.as_f64()).map_or(-1., |v| v as f32);
    if o > 0. { o } else { d.loot_spawn_location[2] }
}
pub fn bounce_center(d: &ClassDef, t: glam::Vec3, q: glam::Quat, s: glam::Vec3) -> glam::Vec3 { t + q * (glam::Vec3::from(d.loot_spawn_location) * s) }

pub fn jiggle(mag: &dyn Fn(f32) -> f32, mag_range: (f32, f32), angle: &dyn Fn(f32) -> f32, angle_range: (f32, f32), since: f32, pawn_yaw: f32) -> (glam::Vec3, f32) {
    let clamp = |t: f32, (lo, hi): (f32, f32)| { let mut x = lo; if lo <= t { x = t; if hi <= t { x = hi; } } x };
    let a = angle(clamp(since + angle_range.0, angle_range));
    let k = 1.0 / 0.625f32.sqrt();
    let (x0, z0) = (0.75 * k, 0.25 * k);
    let mut v = glam::Vec3::new((-x0 - x0) * a + x0, 0., z0);
    let l2 = v.length_squared();
    if l2 != 1.0 { v = if l2 >= 1e-8 { v / l2.sqrt() } else { glam::Vec3::ZERO }; }
    let r = glam::Quat::from_rotation_z((pawn_yaw + 90.).to_radians());
    let v = (r * v) * -mag(clamp(since + mag_range.0, mag_range));
    (v, mag(clamp(since, mag_range)))
}

/// `ABuildingContainer` ctor (@140458c60) defaults of the Athena toss (no Blueprint overrides them): `LootTossDirection_Athena` (pitch 65, yaw 90, roll 0),
/// `LootTossSpeed_Athena` 550 cm/s, `LootTossConeHalfAngle_Athena` 60 degrees.
pub const TOSS_PITCH: f32 = 65.;
pub const TOSS_YAW: f32 = 90.;
pub const TOSS_SPEED: f32 = 550.;
pub const TOSS_CONE_HALF_ANGLE: f32 = 60.;

/// CHOSEN (the toss is server code in 1.8): where a loot item starts and lands (FX space, UE cm). Start = the actor transform x the class's
/// `LootSpawnLocation` (the live client's value, no tier set applied); the launch direction = the actor rotation x `LootTossDirection_Athena`, jittered uniformly in the
/// `LootTossConeHalfAngle_Athena` cone, at `LootTossSpeed_Athena`; it lands where that ballistic path (gravity `gravity_cm`, cm/s^2) meets the container's
/// base height. The pickup then flies the port's toss arc to that point (`fn_game::pickups`).
pub fn toss_speed(d: Option<&ClassDef>) -> f32 { d.and_then(|d| d.props.get("LootTossSpeed_Athena")).and_then(|v| v.as_f64()).map_or_else(|| fn_live::f32("FN_TOSS_SPEED", TOSS_SPEED), |v| v as f32) }
pub fn toss_cone(d: Option<&ClassDef>) -> f32 { d.and_then(|d| d.props.get("LootTossConeHalfAngle_Athena")).and_then(|v| v.as_f64()).map_or_else(|| fn_live::f32("FN_TOSS_CONE_HALF_ANGLE", TOSS_CONE_HALF_ANGLE), |v| v as f32) }
pub fn toss_launch(d: Option<&ClassDef>, t: glam::Vec3, q: glam::Quat, s: glam::Vec3, rng: &mut Rng) -> (glam::Vec3, glam::Vec3) {
    use glam::{Mat3, Vec3};
    let loc = d.map(|d| d.loot_spawn_location).unwrap_or([0., 0., 100.]);
    let a = t + q * (Vec3::from(loc) * s);
    let base = Mat3::from_quat(q) * Mat3::from_rotation_z(fn_live::f32("FN_TOSS_YAW", TOSS_YAW).to_radians()) * Mat3::from_rotation_y(-fn_live::f32("FN_TOSS_PITCH", TOSS_PITCH).to_radians());
    let cos_max = toss_cone(d).to_radians().cos();
    let cz = 1. - rng.next_f32() * (1. - cos_max);
    let sz = (1. - cz * cz).max(0.).sqrt();
    let phi = rng.next_f32() * std::f32::consts::TAU;
    (a, base * Vec3::new(cz, sz * phi.cos(), sz * phi.sin()) * toss_speed(d))
}

pub fn toss_points(d: Option<&ClassDef>, tier: i32, t: glam::Vec3, q: glam::Quat, s: glam::Vec3, rng: &mut Rng, gravity_cm: f32) -> (glam::Vec3, glam::Vec3) {
    use glam::{Mat3, Vec3};
    let _ = tier;
    let loc = d.map(|d| d.loot_spawn_location).unwrap_or([0., 0., 100.]);
    let a = t + q * (Vec3::from(loc) * s);
    let base = Mat3::from_quat(q) * Mat3::from_rotation_z(fn_live::f32("FN_TOSS_YAW", TOSS_YAW).to_radians()) * Mat3::from_rotation_y(-fn_live::f32("FN_TOSS_PITCH", TOSS_PITCH).to_radians());
    // a uniform direction in the cone about the local X axis
    let cos_max = toss_cone(d).to_radians().cos();
    let cz = 1. - rng.next_f32() * (1. - cos_max);
    let sz = (1. - cz * cz).max(0.).sqrt();
    let phi = rng.next_f32() * std::f32::consts::TAU;
    let dir = base * Vec3::new(cz, sz * phi.cos(), sz * phi.sin());
    let v = dir * toss_speed(d);
    let g = gravity_cm.max(1.);
    // z(t) = a.z + v.z t - g t^2 / 2 = t.z
    let dz = a.z - t.z;
    let tt = (v.z + (v.z * v.z + 2. * g * dz).max(0.).sqrt()) / g;
    let b = a + Vec3::new(v.x, v.y, 0.) * tt;
    (a, Vec3::new(b.x, b.y, t.z))
}

/// `RarityData.RarityCollection[r].Color2` (linear RGBA), index = `EFortRarity`.
pub fn rarity_color2(set: &PakSet) -> Result<Vec<[f32; 4]>, String> {
    let path = "/Game/Balance/RarityData";
    let pd = PackageData::load(set, path).map_err(|e| format!("{path}: {e}"))?;
    let pkg = pd.package().map_err(|e| format!("{path}: {e}"))?;
    let o = pkg.read_export(0).map_err(|e| format!("{path}: {e}"))?;
    let mut out = vec![[1.; 4]; 10];
    for p in o.props.0.iter().filter(|p| p.name == "RarityCollection") {
        let j = p.value.to_json(&pkg);
        if let Some(c) = j["Color2"].as_array() { if (p.index as usize) < out.len() { out[p.index as usize] = [0, 1, 2, 3].map(|i| c.get(i).and_then(|x| x.as_f64()).unwrap_or(1.) as f32); } }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inp(down: bool, pressed: bool, focused: bool) -> SearchInput { SearchInput { use_down: down, use_pressed: pressed, focused, duration: 1.3, cancel: false } }

    #[test] fn a_held_search_completes_after_its_duration() {
        let mut s = Search::default();
        assert_eq!(s.update(&inp(true, true, true), 1. / 60.), vec![SearchEvent::Begin]);
        let mut done = None;
        for f in 1..200 { if s.update(&inp(true, false, true), 1. / 60.).contains(&SearchEvent::Complete) { done = Some(f); break; } }
        let f = done.expect("completes");
        assert!((f as f32 / 60. - 1.3).abs() <= 1. / 60. + 1e-4, "{f} frames");
        assert!(s.searched());
        assert!(s.update(&inp(true, true, true), 1. / 60.).is_empty(), "a searched container cannot be searched again");
    }
    #[test] fn releasing_use_or_losing_the_focus_interrupts_and_resets() {
        let mut s = Search::default();
        s.update(&inp(true, true, true), 0.016);
        for _ in 0..30 { s.update(&inp(true, false, true), 0.016); }
        assert!(s.progress() > 0.3);
        assert_eq!(s.update(&inp(false, false, true), 0.016), vec![SearchEvent::Interrupted]);
        assert_eq!(s.progress(), 0.);
        s.update(&inp(true, true, true), 0.016);
        assert_eq!(s.update(&inp(true, false, false), 0.016), vec![SearchEvent::Interrupted], "looking away");
        assert!(s.update(&inp(true, false, true), 0.016).is_empty(), "holding without a new press does not restart");
    }
    #[test] fn the_tier_group_redirect_is_the_game_modes() {
        assert_eq!(athena_tier_group("Loot_Treasure"), Some("Loot_AthenaTreasure"));
        assert_eq!(athena_tier_group("Loot_Ammo"), Some("Loot_AthenaAmmoLarge"));
        assert_eq!(athena_tier_group("Loot_CraftingIngredients"), None);
    }
    fn tables() -> Option<LootTables> {
        let root = fn_live::var("FN_TEST_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let set = PakSet::open_install(&root).ok()?;
        LootTables::load(&set).ok()
    }
    /// The roll of the cooked tables (seeded): a chest gives one item of each of the four categories (weapon, its ammo, a consumable, a resource);
    /// the weapon package frequencies follow the `Loot_AthenaTreasure` row weights.
    #[test] fn the_chest_roll_follows_the_athena_tables() {
        let Some(t) = tables() else { eprintln!("no 1.8 install: skipped"); return };
        let rows: Vec<&TierRow> = t.tiers_of("Loot_AthenaTreasure").collect();
        assert_eq!(rows.len(), 5);
        let total: f32 = rows.iter().map(|r| r.weight).sum();
        let mut rng = Rng::new(42);
        let n = 20000;
        let mut by_pkg: HashMap<String, usize> = HashMap::new();
        for _ in 0..n {
            let d = t.roll("Loot_AthenaTreasure", &mut rng);
            assert_eq!(d.len(), 4, "{d:?}");
            assert!(d[0].item.contains("/Weapons/") || d[0].item.contains("WID_"), "{d:?}");
            let w = d[0].item.rsplit('.').next().unwrap().to_owned();
            let pkg = rows.iter().find(|r| t.package_rows(&format!("WorldList.{}", r.loot_package.trim_start_matches("WorldPKG."))).any(|p| p.item.ends_with(&format!(".{w}")))).map(|r| r.loot_package.clone()).unwrap_or_default();
            *by_pkg.entry(pkg).or_default() += 1;
        }
        for r in &rows {
            let got = *by_pkg.get(&r.loot_package).unwrap_or(&0) as f32 / n as f32;
            let want = r.weight / total;
            assert!((got - want).abs() < 0.015, "{}: {got:.3} vs {want:.3}", r.loot_package);
        }
    }
    fn dist(t: &LootTables, id: &str, cat: Option<i32>, p: f64, out: &mut HashMap<(String, i32), f64>, depth: u32) {
        if depth > 8 { return; }
        let rows: Vec<&PackageRow> = t.package_rows(id).filter(|r| cat.map_or(true, |c| r.category == c) && r.weight > 0.).collect();
        let total: f64 = rows.iter().map(|r| r.weight as f64).sum();
        for r in rows {
            let q = p * r.weight as f64 / total;
            if !r.call.is_empty() { dist(t, &r.call, None, q, out, depth + 1); } else if !r.item.is_empty() && r.count > 0 { *out.entry((r.item.clone(), r.count)).or_default() += q; }
        }
    }
    fn check_group(t: &LootTables, group: &str, n: usize, seed: u64) {
        let tiers: Vec<&TierRow> = t.tiers_of(group).collect();
        let wt: f64 = tiers.iter().map(|r| r.weight as f64).sum();
        let ncat = tiers.iter().map(|r| r.category_min.iter().filter(|m| **m > 0).count()).max().unwrap_or(0);
        let mut want: Vec<HashMap<(String, i32), f64>> = vec![HashMap::new(); ncat];
        for r in &tiers {
            assert_eq!(r.num_drops.fract(), 0., "{}: whole drops", r.name);
            for c in 0..ncat { let mut m = HashMap::new(); dist(t, &r.loot_package, Some(c as i32), r.weight as f64 / wt, &mut m, 0); for (k, v) in m { *want[c].entry(k).or_default() += v * r.category_min[c] as f64; } }
        }
        let per_cat: Vec<usize> = tiers[0].category_min.iter().take(ncat).map(|m| *m as usize).collect();
        let mut got: Vec<HashMap<(String, i32), f64>> = vec![HashMap::new(); ncat];
        let mut rng = Rng::new(seed);
        for _ in 0..n {
            let d = t.roll(group, &mut rng);
            assert_eq!(d.len(), per_cat.iter().sum::<usize>(), "{group}: {d:?}");
            let mut k = 0;
            for (c, m) in per_cat.iter().enumerate() { for _ in 0..*m { *got[c].entry((d[k].item.clone(), d[k].count)).or_default() += 1.; k += 1; } }
        }
        for c in 0..ncat {
            let total_w: f64 = want[c].values().sum();
            assert!((total_w - per_cat[c] as f64).abs() < 1e-6, "{group} category {c}: expected mass {total_w}");
            for k in got[c].keys() { assert!(want[c].contains_key(k), "{group} category {c}: {k:?} is not in the tables"); }
            for (k, p) in &want[c] {
                let pn = p / per_cat[c] as f64;
                let f = got[c].get(k).copied().unwrap_or(0.) / (n * per_cat[c]) as f64;
                let sigma = (pn * (1. - pn) / (n * per_cat[c]) as f64).sqrt();
                assert!((f - pn).abs() <= 4. * sigma + 1e-3, "{group} category {c} {k:?}: {f:.4} vs {pn:.4} (4 sigma {:.4})", 4. * sigma);
            }
            eprintln!("{group} category {c}: {} outcomes match the tables", want[c].len());
        }
    }
    #[test] fn ten_thousand_rolls_follow_the_cooked_probabilities() {
        let Some(t) = tables() else { eprintln!("no 1.8 install: skipped"); return };
        check_group(&t, "Loot_AthenaTreasure", 10_000, 11);
        check_group(&t, "Loot_AthenaAmmoLarge", 10_000, 12);
    }
    #[test] fn the_ammo_box_roll_gives_ammo() {
        let Some(t) = tables() else { return };
        let mut rng = Rng::new(7);
        for _ in 0..500 {
            let d = t.roll("Loot_AthenaAmmoLarge", &mut rng);
            assert_eq!(d.len(), 2, "{d:?}");
            assert!(d.iter().all(|x| x.item.contains("Ammo")), "{d:?}");
        }
    }
    #[test] fn the_spawn_selection_keeps_the_chosen_share() {
        let mut rng = Rng::new(3);
        for _ in 0..200 {
            let k = select_spawned(271, 50., 70., &mut rng).iter().filter(|b| **b).count();
            assert!((135..=190).contains(&k), "{k}");
        }
        assert!(select_spawned(10, 0., 0., &mut rng).iter().all(|b| *b));
    }
    #[test] fn the_search_time_is_the_selected_mesh_sets() {
        let root = fn_live::var("FN_TEST_ROOT").unwrap_or_else(|_| r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame".into());
        let Ok(set) = PakSet::open_install(&root) else { return };
        let table = search_speed_table(&set).unwrap();
        assert_eq!(table.get("SearchSpeedVerySlow"), Some(&0.5), "AthenaSearchSpeed: every row 0.5 s");
        let chest = load_class(&set, "Tiered_Chest_6_Parent").unwrap();
        let ammo = load_class(&set, "Tiered_Short_Ammo_3_Parent").unwrap();
        assert_eq!(search_speed_row(&chest, 0), "SearchSpeedVerySlow");
        assert_eq!(search_speed_row(&ammo, 1), "SearchSpeedNormal");
        assert_eq!(search_time(&chest, 0, &table), 0.5);
        assert_eq!(search_time(&ammo, 1, &table), 0.5);
        assert_eq!(mesh_set(&chest, 0).map(|m| m.search_speed_row.as_str()), Some("None"), "the unused Tier 0 set (would be the 0.75 s fallback)");
        assert!(searched_mesh(&chest, 0).unwrap().ends_with("TreasureChestLootTier1_Searched"));
        assert!(searched_mesh(&ammo, 1).unwrap().ends_with("Loot_AmmoCase_Tier1_Searched"));
        assert!(load_class(&set, "Tiered_Chest_3_Child").unwrap().is_a("Tiered_Chest_6_Parent"));
    }
    #[test] fn a_change_of_equipment_interrupts_the_search() {
        let mut s = Search::default();
        s.update(&inp(true, true, true), 0.016);
        let mut c = inp(true, false, true);
        c.cancel = true;
        assert_eq!(s.update(&c, 0.016), vec![SearchEvent::Interrupted]);
    }
    #[test] fn the_jiggle_follows_the_exe_drive() {
        let mag = |t: f32| t;
        let (v, h) = jiggle(&mag, (0., 1.), &|_| 0.5, (0., 1.), 0.25, 0.);
        assert!((v - glam::Vec3::new(0., 0., -0.25)).length() < 1e-5, "{v}");
        assert!((h - 0.25).abs() < 1e-6);
        let (v, _) = jiggle(&mag, (0., 1.), &|_| 0., (0., 1.), 0.5, 0.);
        let k = 1.0 / 0.625f32.sqrt();
        assert!((v - glam::Vec3::new(0., 0.75 * k, 0.25 * k) * -0.5).length() < 1e-5, "yaw + 90 turns the tilt to +Y: {v}");
        let (v, h) = jiggle(&mag, (0.2, 1.), &|_| 0.5, (0., 1.), 5.0, 0.);
        assert!((v.z + 1.0).abs() < 1e-5 && (h - 1.0).abs() < 1e-6, "clamped to the last key");
        let (_, h) = jiggle(&mag, (0.2, 1.), &|_| 0.5, (0., 1.), 0.1, 0.);
        assert!((h - 0.2).abs() < 1e-6, "the base drive clamps without the key offset");
    }
    #[test] fn the_toss_leaves_the_front_upwards_and_lands_on_the_base() {
        let mut rng = Rng::new(9);
        let mut mean = glam::Vec2::ZERO;
        for _ in 0..200 {
            let (a, b) = toss_points(None, 0, glam::Vec3::ZERO, glam::Quat::IDENTITY, glam::Vec3::ONE, &mut rng, 2800.);
            assert_eq!(a, glam::Vec3::new(0., 0., 100.), "no class: the ctor default LootSpawnLocation");
            assert!(b.z.abs() < 1e-3);
            let h = glam::Vec2::new(b.x, b.y);
            assert!(h.length() < 400., "{h}");
            mean += h / 200.;
        }
        assert!(mean.y > 50. && mean.x.abs() < 30., "yaw 90: on average towards local +Y (the 60 degree cone about pitch 65 can tip past vertical): {mean}");
    }
}
