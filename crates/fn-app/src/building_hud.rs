//! BUILDING HUD (fn-app): which building the Target Info bar (`BuildingInfoIndicator` / `BuildingBar`, drawn by `fn_ui::screens::target_info`) shows, and
//! what it shows for the player's pieces, the build preview and the props. `harvest_fx::feed` calls [`aim`] once per presented frame.
//!
//! 1.8 (addresses in the 1.8 exe):
//! * `AFortPlayerController::TargetedBuilding` (+0x1890) is set by FUN_1408537d0, called from the held weapon's `DetermineReticleColor`:
//!   1. the building at +0x1790 (the edit tool's building) unless it is the preview marker;
//!   2. else the weapon's target: melee (`AFortWeapon` @14091ee60) the ability's target data (the swing's target, within its reach); ranged
//!      (`AFortWeaponRanged` @14091f690) the first actor of the camera trace when it is a building actor within `GetRange` (@140929630:
//!      max(RngLong, RngMax)); the building tool (@14091ebd0) the same with `AFortWeapon::GetRange` = 0 (@1420b5390 `xorps; ret`), so never;
//!   3. else `BuildPreviewMarker` (+0x1788); 4. else `ActorUnderReticle` (+0x790).
//!   A change broadcasts +0x1630 -> `UFortHUDContext` queues the focus (FUN_142e37a30) -> `OnBuildingFocused`.
//! * `UFortHUDContext::GetFocusedBuildingInfo` @142e267c0 fills `FFortFocusedBuildingInfo`:
//!   - `HealthDisplayRule` = Never when `bSurpressHealthBar` (vfunc_231 = +0x4E8 bit1) or `bDestroyed`, else Allowed; Always when the building
//!     `IsPlayerBuilt` (@14047f850: TeamIndex != -1 or `bPlayerPlaced`) and the interaction is not ConfirmEdit;
//!   - `InteractionType`: ConfirmEdit (6) while the edit-mode input component is pushed (FUN_140834ae0); BeingModified (5) while
//!     `bUnderConstruction` (vfunc_344 = +0xE90 bit6) or `CurBuildingAnimType` is Building / Placement (vfunc_346); in build mode
//!     (FUN_140834c10) on a building that is not `bIsPlayerBuildable` (the preview marker) Build (1); otherwise Edit (4) when the player can edit it
//!     (every player piece: `bIsPlayerBuildable`, `ABuildingSMActor` vfunc_331); STW-only Repair / Upgrade never apply in BR;
//!   - `MaxHealth` = `GetMaxHealth` (the preview: `ABuildingPlayerPrimitivePreview::GetMaxHealth` @140475870, the buildable's MaxHealth);
//!   - `IndicatorRelativeLocation` = vfunc_245 @14046c990 = `CentroidOffset`: the bar sits on the building's centroid.
//! * `BuildingInfoIndicator_C` is a `FortActorIndicatorWidget` with `MaxDistance` 1000 (+0x284), but nothing reads that field in 1.8: no load of
//!   +0x284 in the FortniteUI code (exe scan of every disp32 0x284 access in 0x142d00000..0x143100000: only unrelated FVector2D members), the
//!   indicator interface (vtable 0x144ae7210) has no getter for it, and the Blueprint never reads it. So the bar has no distance limit.
//! The props keep their harvest rule (not player built: Allowed = shown after damage, `fn_combat::harvest_fx`).
use fn_building::world::PieceId;
use fn_combat::harvest_fx;
use fn_game::GameSim;
use fn_ui::screens::reticle_state::{HealthDisplayRule, TargetInfo};
use glam::Vec3;

/// Target Info identity of the build preview marker (one actor, re-classed when the buildable changes).
pub const PREVIEW_ID: u64 = (1 << 62) | (1 << 61);

/// `EFortBuildingInteraction`.
pub const BUILD: u8 = 1;
pub const EDIT: u8 = 4;
pub const BEING_MODIFIED: u8 = 5;
pub const CONFIRM_EDIT: u8 = 6;

/// The targeted building of this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus { Piece(PieceId), Prop(usize), Preview }

/// What the held tool sees this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Aim {
    pub focus: Option<Focus>,
    /// Melee `DetermineReticleColor`: the target is a building actor (piece or prop) -> `ReticleBuildingColor`.
    pub reticle_building: bool,
}

/// The held tool, as `DetermineReticleColor` dispatches on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tool { Pickaxe, Gun { range_m: f32 }, Other }

/// Melee reach test of a point on the camera ray (`RangeVSBuildings2D` / `Z` of the pickaxe row, as `fn_combat::reticle::aim_on_target`).
fn in_melee_reach(p: Vec3, pawn: Vec3) -> bool {
    let flat = ((p.x - pawn.x).powi(2) + (p.z - pawn.z).powi(2)).sqrt();
    flat <= fn_combat::reticle::RANGE_2D && (p.y - pawn.y).abs() <= fn_combat::reticle::RANGE_Z
}

pub fn prop_aim_hit(sim: &GameSim, eye: Vec3, fwd: Vec3) -> Option<fn_combat::TraceHit> {
    thread_local! { static LAST: std::cell::Cell<Option<(u32, usize, [u32; 6], Option<fn_combat::TraceHit>)>> = const { std::cell::Cell::new(None) }; }
    let d = fwd.normalize_or_zero();
    let key = (sim.frames, sim.world.props.len(), [eye.x.to_bits(), eye.y.to_bits(), eye.z.to_bits(), d.x.to_bits(), d.y.to_bits(), d.z.to_bits()]);
    if let Some((f, n, k, hit)) = LAST.with(|l| l.get()) { if (f, n, k) == key { return hit; } }
    let len = fn_combat::reticle::AIM_TRACE;
    let hit = match fn_props::segment_ids(sim.fresh_prop_index(), eye, eye + d * len, 0.) {
        Some(ids) => fn_combat::trace_cylinders_ids(&sim.world.props, &ids, eye, d, len, 0.),
        None => fn_combat::trace_cylinders(&sim.world.props, eye, d, len, 0.),
    };
    LAST.with(|l| l.set(Some((key.0, key.1, key.2, hit))));
    hit
}

/// FUN_1408537d0 for this frame (see the module docs). `eye` / `fwd` = the camera view (sim metres), `pawn` = the pawn's chest point.
pub fn aim(sim: &GameSim, tool: Tool, eye: Vec3, fwd: Vec3, pawn: Vec3) -> Aim {
    // 1. the edit tool's building
    if let Some(id) = crate::building_hook::editing_piece().or_else(crate::building_hook::preview_focus) { return Aim { focus: Some(Focus::Piece(id)), reticle_building: false }; }
    // 2. / 3. build mode: the building tool's range is 0, so the preview marker
    if crate::building_hook::preview_status().is_some() { return Aim { focus: Some(Focus::Preview), reticle_building: false }; }
    let d = fwd.normalize_or_zero();
    let len = fn_combat::reticle::AIM_TRACE;
    let piece = crate::building_damage_hook::trace_pieces(eye, d, len);
    let prop = prop_aim_hit(sim, eye, d).map(|h| (h.index, h.fraction * len));
    match tool {
        Tool::Pickaxe => {
            let piece = piece.filter(|&(_, t)| in_melee_reach(eye + d * t, pawn));
            let prop = prop.filter(|&(_, t)| in_melee_reach(eye + d * t, pawn));
            let focus = match (piece, prop) {
                (Some((id, tp)), Some((_, tr))) if tp <= tr => Some(Focus::Piece(id)),
                (Some((id, _)), None) => Some(Focus::Piece(id)),
                (_, Some((i, _))) => Some(Focus::Prop(i)),
                (None, None) => None,
            };
            Aim { focus, reticle_building: focus.is_some() }
        }
        Tool::Gun { range_m } => {
            // the first actor of the trace; the landscape (here the baseplate) is an actor too and ends it
            let ground = fn_game::camera::trace_ground(eye, eye + d * len, 0.).map(|f| f * len).unwrap_or(f32::MAX);
            let first = match (piece, prop) {
                (Some((id, tp)), Some((_, tr))) if tp <= tr => Some((Focus::Piece(id), tp)),
                (Some((id, tp)), None) => Some((Focus::Piece(id), tp)),
                (_, Some((i, tr))) => Some((Focus::Prop(i), tr)),
                (None, None) => None,
            };
            Aim { focus: first.filter(|&(_, t)| t <= ground && t <= range_m).map(|(f, _)| f), reticle_building: false }
        }
        Tool::Other => Aim::default(),
    }
}

/// `FFortFocusedBuildingInfo` of the focus as the Target Info input, with its world anchor (sim metres). None when the building is gone.
pub fn info(sim: &GameSim, focus: Focus) -> Option<(TargetInfo, Vec3)> {
    let target = |id: u64, health: f32, max_health: f32, rule: HealthDisplayRule, interaction: u8| TargetInfo { id, health, max_health, rule, interaction, screen: None };
    match focus {
        Focus::Piece(id) => {
            let st = crate::building_hook::piece_status(id)?;
            let editing = crate::building_hook::editing_piece() == Some(id);
            let interaction = if editing { CONFIRM_EDIT } else if st.under_construction { BEING_MODIFIED } else { EDIT };
            // player built: Always, except while confirming an edit (the rule stays Allowed)
            let rule = if interaction == CONFIRM_EDIT { HealthDisplayRule::OnDamage } else { HealthDisplayRule::Always };
            Some((target(crate::building_damage_hook::BUILDING_OBJECT | id as u64, st.health.max(0.), st.max_health, rule, interaction), crate::building_hook::world_to_sim(st.centroid)))
        }
        Focus::Preview => {
            let (max_health, centroid) = crate::building_hook::preview_status()?;
            // ABuildingPlayerPrimitivePreview is not bIsPlayerBuildable: Build (1); the bar shows "{MaxHealth} HP"
            Some((target(PREVIEW_ID, max_health, max_health, HealthDisplayRule::Always, BUILD), crate::building_hook::world_to_sim(centroid)))
        }
        Focus::Prop(i) => {
            let p = sim.world.props.get(i).filter(|p| p.alive())?;
            Some((target(harvest_fx::object_id(i, p), p.health.max(0.), p.max_hp, HealthDisplayRule::OnDamage, 0), harvest_fx::indicator_anchor(p)))
        }
    }
}

/// The Target Info input of this frame: `aim` + `info`, projected (no distance limit, see the module docs).
pub fn target(sim: &GameSim, tool: Tool, eye: Vec3, fwd: Vec3, pawn: Vec3, project: impl Fn(Vec3) -> Option<(f32, f32)>) -> (Aim, Option<TargetInfo>) {
    let a = aim(sim, tool, eye, fwd, pawn);
    let t = a.focus.and_then(|f| info(sim, f)).and_then(|(mut t, anchor)| {
        t.screen = Some(project(anchor)?);
        Some(t)
    });
    (a, t)
}

static RANGED_TARGETING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The app's ADS step of this frame (ADS needs a held firearm, so this is `HandlePlayerTargetingChanged` with a `FortWeaponRanged`).
pub fn set_ranged_targeting(on: bool) { RANGED_TARGETING.store(on, std::sync::atomic::Ordering::Relaxed); }
pub fn ranged_targeting() -> bool { RANGED_TARGETING.load(std::sync::atomic::Ordering::Relaxed) }

/// `UpdateBuildingInfo_InteractionWidgets` inputs for the focused building's interaction (key names from the bindings).
/// `CompleteBuildingEditInteraction` has no binding row of its own and shares the Building Edit keys (build_input.rs).
pub fn prompt(sim: &GameSim, b: &fn_settings::Bindings, interaction: u8) -> Option<fn_ui::screens::target_info::BuildingPrompt> {
    use fn_ui::screens::reticle_state::HarvestResource as R;
    if !(BUILD..=CONFIRM_EDIT).contains(&interaction) { return None; }
    let key = |a: usize| { let k = b.get(a)[0]; if k.name() == "None" { String::new() } else { k.name().to_owned() } };
    let mut p = fn_ui::screens::target_info::BuildingPrompt {
        fire: key(fn_settings::A_FIRE), change_material: key(fn_settings::A_CHANGE_MATERIAL), edit: key(fn_settings::A_PERFORM_BUILDING_EDIT_INTERACTION),
        rotate: key(fn_settings::A_ROTATE), confirm_edit: key(fn_settings::A_PERFORM_BUILDING_EDIT_INTERACTION), edit_reset: key(fn_settings::A_BUILDING_EDIT_RESET),
        // every player piece: bCanBePlayerEdited, no trap attached (traps are not in this build)
        edit_available: true, ..Default::default()
    };
    if interaction == BUILD {
        let (cost, m, have) = crate::building_hook::preview_cost(sim)?;
        p.cost = cost; p.can_afford = cost <= have;
        p.material = match m { fn_building::pieces::Material::Wood => R::Wood, fn_building::pieces::Material::Stone => R::Stone, fn_building::pieces::Material::Metal => R::Metal };
    }
    Some(p)
}
