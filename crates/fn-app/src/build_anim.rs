//! BUILD ANIM HOOK: the player side of building mode -- the held building blueprint ("build map") + pencil, the PaperBlueprint animation set, the
//! BuildingState pose switch and the build / place / edit montages. All data comes from `fn_pose::loadout_build` (see its module docs for every source).
//!
//! Who calls what (the other building agents own the triggers; nothing here reads keys):
//! * [`on_build_mode_enter`] / [`on_build_mode_exit`]: the building tool is equipped / unequipped (`AFortWeap_BuildingTool::OnEquip` / `OnUnEquip`).
//!   `poll` derives them from `BuildInput::is_building()` every frame, so the build HUD / input agent does not need to call them.
//! * [`on_piece_placed`]: a piece was really created (local `FUN_1408336f0` / `ServerCreateBuildingActor` success) -> `BluePrintPlaceAnimation` + the paper's glow pulse.
//!   The building core (`building_hook.rs`) should call it on success only (a refused / unaffordable placement plays nothing).
//! * [`on_edit_begin`] / [`on_edit_confirm`] / [`on_edit_cancel`]: the edit tool is equipped / the session ends. `poll` derives them from `BuildInput::is_editing()`
//!   (set by the edit hook) and the `EditConfirm` intent of that frame.
//! * [`on_edit_tile_changed`]: `AFortPlayerController::LocalOnBlueprintEdit` (the edit actor's pattern changed) -> `BluePrintEditAnimation`. The editing agent should call it.
//! * [`set_resource`]: `CurrentResourceType` (0 Wood, 1 Stone, 2 Metal) -> the paper's `Diffuse` / `EmissiveMask` (polled from `BuildInput::material()`).
//! * [`set_dbno`]: pawn `bIsDBNO` (+0x920; the port has no knocked-down state yet, so it stays false).
//!
//! Montage playback follows UE 4.16 `FAnimMontageInstance`: blend in over `BlendIn` from 0, auto blend-out at `SequenceLength - BlendOut` from the current weight,
//! `Montage_Play` stops the playing montages of the group with the new montage's `BlendIn` (time and option), `Montage_IsPlaying` is false once an instance is blending out.
//! The pawn's place / edit calls only play when `Montage_IsPlaying(montage)` is false (RVAs 0x1407f6a70 / 0x1407f69f0).
//!
//! The paper: `SK_Blueprint_Paper` skinned to its own skeleton and animated by its AnimBlueprint `Blueprint_Paper_VIM` (mesh ref pose -> 4 `AnimNode_SpringBone`,
//! ported from UE 4.16 `AnimNode_SpringBone.cpp` / `FCSPose::LocalBlendCSBoneTransforms`), drawn with the compiled `M_BluePrintArchitect` shading
//! (`MaterialKind::Architect`: EmissiveColor = EmissiveMask x EmissiveMultiply). The building tool's MID gets `EmissiveMultiply` from the
//! `BlueprintPaperPulseTimeline` (curve `HandheldCharacterBlueprintGlowAnimation`), restarted by `AFortWeap_BuildingTool::PlayWeaponFireFX` (0x140935590) on every placement.
//!
//! Dev / verification timeline (no keys involved): `FN_BUILDANIM=<frame>:<event>,...` with events `enter exit place edit tile confirm cancel wood stone metal`
//! (a resource event stays in force until the next one; without one the build input's material is used).
use crate::arms::{Arms, ArmsSound, Fx, MontageData};
use fn_animgraph::{Montage, BUILDING_STATE_EDIT, BUILDING_STATE_NONE, BUILDING_STATE_PLACEMENT};
use fn_pose::{loadout_build::{BuildToolSet, SpringBoneNode}, ClipSourceDir, ClipStore, FnClip, SampleMode, SocketDef};
use fn_render::{MaterialImages, MaterialKind, ModelId, SkinnedPrimitive, SkinnedVertex};
use glam::{Mat4, Quat, Vec3, Vec4};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event { Enter, Exit, Place, EditBegin, EditTile, EditConfirm, EditCancel }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held { None, Build, Edit }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which { EquipBuild = 0, EquipEdit = 1, Place = 2, Edit = 3 }

/// A bone of the paper's own skeleton (`SK_Blueprint_Paper` RefSkeleton; UE cm, mesh component space).
#[derive(Clone, Copy, Debug)]
struct PBone { parent: i32, t: Vec3, r: Quat, s: Vec3 }
impl PBone { fn local(&self) -> Mat4 { Mat4::from_scale_rotation_translation(self.s, self.r, self.t) } }
/// The paper's skeleton and its `Blueprint_Paper_VIM` spring chain (bone index, node) in evaluation order.
#[derive(Clone, Debug)]
pub struct PaperRig { bones: Vec<PBone>, inv_ref_cs: Vec<Mat4>, springs: Vec<(usize, SpringBoneNode)> }

/// Everything loaded from the paks (worker thread).
pub struct Data {
    pub set: BuildToolSet,
    /// [EquipBuild, EquipEdit, Place, Edit] montages and their montage-level curves.
    montages: [MontageData; 4],
    curves: [Vec<(String, Vec<(f32, f32)>)>; 4],
    /// Player-skeleton sequences of the set (full package path keys + unique base-name aliases).
    clips: Option<ClipStore>,
    /// Paper with the building tool's MID textures per `EFortResourceType` (Wood, Stone, Metal), the edit tool's paper (mesh material), the pencil.
    paper_build: Vec<Vec<SkinnedPrimitive>>,
    paper_edit: Vec<SkinnedPrimitive>,
    pencil: Vec<SkinnedPrimitive>,
    rig: PaperRig,
    /// `BlueprintPaperPulseCurve` keys (time, value; all linear), the timeline length (TL_LastKeyFrame = last key time).
    pulse: Vec<(f32, f32)>,
    sockets: [SocketDef; 2],
    core: &'static str,
    pub notes: Vec<String>,
}

struct Inst { which: Which, t: f32, stop: Option<(f32, f32, f32, u8)> }
struct Models { build: Vec<ModelId>, edit: ModelId, pencil: ModelId }

/// `FAnimNode_SpringBone` simulation state (`RemainingTime`, `BoneLocation`, `BoneVelocity`; world space, UE axes, cm). Default = the node's constructor.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SpringSim { remaining: f32, loc: Vec3, vel: Vec3 }

/// `FTimeline` of the building tool (`BlueprintPaperPulseTimeline`, +0xB10): playback position, bPlaying; `value` = the MID's `EmissiveMultiply`
/// (the MIC value 1.0 until the first update).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pulse { pos: f32, playing: bool, value: f32 }
impl Default for Pulse { fn default() -> Self { Pulse { pos: 0., playing: false, value: 1. } } }

/// Which tool weapon actor is in the hands: one `AFortWeap_BuildingTool` per building-piece item (Build quickbar slot) and the edit tool's own actor.
/// `AFortPawn::EquipWeaponDefinition` (0x1407d9590) reuses the actor whose `ItemEntryGuid` (+0x9a4) matches the item in the pawn's weapon list (+0x838) and only spawns
/// one for an item that has none; `AFortWeapon::OnUnEquip` (0x1409328b0) does not destroy it (only `AFortPawn::vfunc_302` 0x1407d52d0 destroys the list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tool { Build(usize), Edit }
/// Per-actor state that survives unequip / re-equip: the paper's `FAnimNode_SpringBone` sims (its AnimInstance; `OnUnEquip` calls
/// `WeaponMesh(+0x910)->SetComponentTickEnabled(false)` (vslot +0x300, = `UActorComponent::execSetComponentTickEnabled` 0x14269d0a0), so the sims freeze while put away)
/// and the `BlueprintPaperPulseTimeline` (actor tick, `SetActorTickEnabled(0)` in `OnUnEquip`; `AFortWeap_BuildingTool::OnEquip` 0x14092f5c0 does not restart it).
/// Empty `springs` = a newly spawned actor (its AnimInstance runs `InitAnim` on the first evaluation, see `evaluate_paper`).
#[derive(Clone, Debug, Default)]
struct ToolActor { springs: Vec<SpringSim>, pulse: Pulse }

struct State {
    load: Option<std::sync::mpsc::Receiver<Result<Data, String>>>,
    data: Option<Data>,
    failed: bool,
    held: Held,
    /// The tool to return to when the edit tool is put away (the build tool when editing started in build mode).
    edit_return: Held,
    insts: Vec<Inst>,
    events: VecDeque<Event>,
    resource: usize,
    /// `FN_BUILDANIM` resource event in force (sticky; overrides the build input's material).
    resource_dev: Option<usize>,
    installed: bool,
    models: Option<Models>,
    // poll() edge state
    was_building: bool, was_editing: bool,
    timeline: Option<Vec<(u32, String)>>,
    /// The tool weapon actors spawned so far (see [`Tool`] / [`ToolActor`]) and the Build quickbar slot (building-piece item) selected.
    tools: HashMap<Tool, ToolActor>,
    build_slot: usize,
    /// The tool actor drawn last frame (diagnostic log when the hands switch actors).
    shown: Option<Tool>,
    /// Animation delta of the last simulation step, consumed by the paper's AnimInstance update in `frame`.
    anim_dt: f32,
    dbno: bool,
    /// Montage PlaySound notifies of this step (drained by [`take_sounds`]).
    sounds: Vec<ArmsSound>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn new_state() -> State {
    State { load: None, data: None, failed: false, held: Held::None, edit_return: Held::None, insts: vec![], events: VecDeque::new(), resource: 0, resource_dev: None, installed: false, models: None,
        was_building: false, was_editing: false, timeline: None, tools: HashMap::new(), build_slot: 0, shown: None, anim_dt: 0., dbno: false, sounds: vec![] }
}
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let s = g.get_or_insert_with(new_state);
    f(s)
}

pub fn on_build_mode_enter() { with(|s| s.events.push_back(Event::Enter)); }
pub fn on_build_mode_exit() { with(|s| s.events.push_back(Event::Exit)); }
pub fn on_piece_placed() { with(|s| s.events.push_back(Event::Place)); }
pub fn on_edit_begin() { with(|s| s.events.push_back(Event::EditBegin)); }
pub fn on_edit_tile_changed() { with(|s| s.events.push_back(Event::EditTile)); }
pub fn on_edit_confirm() { with(|s| s.events.push_back(Event::EditConfirm)); }
pub fn on_edit_cancel() { with(|s| s.events.push_back(Event::EditCancel)); }
/// `CurrentResourceType` (0 Wood, 1 Stone, 2 Metal).
pub fn set_resource(r: usize) { with(|s| s.resource = r.min(2)); }
/// Pawn `bIsDBNO` (knocked down): the confirmed-edit montage is skipped while it is set (`FUN_140837140`, the body of `LocalExitEditMode`).
pub fn set_dbno(b: bool) { with(|s| s.dbno = b); }
/// A building-mode tool (blueprint) is in the hands.
pub fn is_active() -> bool { with(|s| s.held != Held::None) }
/// The `AnimNotify_PlaySound` moments of the blueprint montages since the last call (`Blueprint_Equip`: `Player_Equip_Blueprint_Cue` at 0.0352 s, bFollow).
pub fn take_sounds() -> Vec<ArmsSound> { with(|s| std::mem::take(&mut s.sounds)) }

// ------------------------------------------------------------------------------------------------------------------------------
// montage instances (UE 4.16 FAnimMontageInstance semantics, see module docs)
// ------------------------------------------------------------------------------------------------------------------------------
fn ease(opt: u8, x: f32) -> f32 { let x = x.clamp(0., 1.); match opt { 0 => x, 1 => x * x * x, _ => x * x * (3. - 2. * x) } }
fn blend_in_weight(m: &MontageData, t: f32) -> f32 { if m.blend_in.0 <= 1e-6 { 1. } else { ease(m.blend_in.1, t / m.blend_in.0) } }
fn weight(m: &MontageData, i: &Inst) -> f32 {
    match i.stop { Some((t0, w0, d, o)) => if d <= 1e-6 { 0. } else { w0 * (1. - ease(o, (i.t - t0) / d)) }, None => blend_in_weight(m, i.t) }
}
impl State {
    fn data_montage(&self, w: Which) -> Option<&MontageData> { self.data.as_ref().map(|d| &d.montages[w as usize]) }
    /// `Montage_Play`: the group's playing montages blend out with the new montage's BlendIn (`StopAllMontagesByGroupName(group, NewMontage->BlendIn)`, UE 4.16
    /// AnimInstance.cpp; `FAnimMontageInstance::Stop` -> `InitializeBlend(InBlendOut)`: its time AND option), then a new instance starts.
    fn play(&mut self, w: Which) {
        let Some(bi) = self.data_montage(w).map(|m| m.blend_in) else { return };
        eprintln!("[build-anim] montage {w:?} plays");
        self.stop_all(bi);
        self.insts.push(Inst { which: w, t: 0., stop: None });
    }
    fn stop_all(&mut self, (dur, opt): (f32, u8)) {
        for i in self.insts.iter_mut() {
            let Some(m) = self.data.as_ref().map(|d| &d.montages[i.which as usize]) else { return };
            match i.stop {
                None => { let w0 = weight(m, i); i.stop = Some((i.t, w0, dur, opt)); }
                // already stopping: only a shorter blend time replaces it (FAnimMontageInstance::Stop, `InBlendOut.GetBlendTime() < Blend.GetBlendTime()`)
                Some((t0, w0, old, o)) if dur < old => { let cur = w0 * (1. - ease(o, (i.t - t0) / old.max(1e-6))); i.stop = Some((i.t, cur, dur, o)); }
                _ => {}
            }
        }
    }
    fn is_playing(&self, w: Which) -> bool { self.insts.iter().any(|i| i.which == w && i.stop.is_none()) }
    fn tick(&mut self, dt: f32) {
        let Some(d) = self.data.as_ref() else { self.insts.clear(); return };
        for i in self.insts.iter_mut() {
            let m = &d.montages[i.which as usize];
            // montage notifies (AnimNotify_PlaySound) crossed by this advance, while the instance has weight (TriggerWeightThreshold 0 in the 1.8 data)
            let (t0, t1) = (i.t, i.t + dt.max(0.));
            if weight(m, i) > 0. {
                for n in &m.fx { if n.time > t0 && n.time <= t1.min(m.length) { if let Fx::Sound { cue, pitch, volume } = &n.fx { self.sounds.push(ArmsSound::Notify { cue: cue.clone(), pitch: *pitch, volume: *volume }); } } }
            }
            i.t = t1;
            // auto blend-out (FAnimMontageInstance::Advance: Position >= SequenceLength - BlendOut.GetBlendTime())
            let trig = (m.length - m.blend_out.0).max(0.);
            if i.stop.is_none() && i.t >= trig { let w0 = blend_in_weight(m, trig); i.stop = Some((trig, w0, m.blend_out.0, m.blend_out.1)); }
        }
        self.insts.retain(|i| match i.stop { Some((t0, _, dur, _)) => i.t < t0 + dur, None => true });
    }
    fn montages(&self) -> Vec<Montage> {
        let Some(d) = self.data.as_ref() else { return vec![] };
        self.insts.iter().filter_map(|i| {
            let m = &d.montages[i.which as usize];
            let (clip, time) = m.sample(i.t.min(m.length))?;
            let w = weight(m, i);
            (w > 0.).then_some(Montage { slot: m.slot, clip, time, weight: w, blend: m.blend })
        }).collect()
    }
    /// Montage-level curve `name` combined over the playing instances (slot pose blend: value x weight, see `montage_curve`).
    fn curve(&self, name: &str) -> Option<(f32, f32)> {
        let d = self.data.as_ref()?;
        let mut acc: Option<(f32, f32)> = None;
        for i in &self.insts {
            let Some((_, keys)) = d.curves[i.which as usize].iter().find(|(n, _)| n == name) else { continue };
            let v = curve_at(keys, i.t);
            let w = weight(&d.montages[i.which as usize], i);
            let (s, ws) = acc.unwrap_or((0., 0.)); acc = Some((s + v * w, ws + w));
        }
        acc
    }
    /// Unequip the building tool / edit tool and re-equip the Primary bar item (`AFortWeapon::OnEquip`): a firearm's / the pickaxe's EquipAnimation stops ours with
    /// its BlendIn (UE group rule). A consumable has NO EquipAnimation in the 1.8 data (`fn_pose::loadout_consumable`: neither `B_UtilityItem_Generic_Athena` nor
    /// `B_UtilityItem_Generic` nor the item definitions serialize `EquipAnimation` / `WeaponEquipMontage`), so nothing is played and nothing stops our montages:
    /// they run to their own auto blend-out (`AFortWeapon::OnUnEquip` 0x1409328b0 stops no montage).
    fn put_away(&mut self, arms: &mut Arms, inp: &mut fn_game::FrameInput) {
        let next_blend_in = if let Some(g) = arms.gun_index() { arms.reequip(); Some(arms.data.guns[g].equip.blend_in) }
            else if arms.pickaxe_selected() { inp.equip = true; Some((fn_animgraph::EQUIP_BLEND_IN.dur, 2)) } // EQUIP_BLEND_IN: Hermite (= HermiteCubic, option 2)
            else { None };
        if let Some(b) = next_blend_in { self.stop_all(b); }
    }
    /// A blueprint tool becomes the current weapon: the held Primary item is unequipped (`Arms::holster`, its montages blend out with our BlendIn). The tool's
    /// weapon actor is reused when the item already has one (`EquipWeaponDefinition` 0x1407d9590, see [`Tool`]), so its spring sims and pulse timeline carry on
    /// from where its last unequip froze them; only the first equip of an item spawns a fresh actor (`tools` has no entry yet).
    fn equip_tool(&mut self, to: Held, arms: &mut Arms, w: Which) {
        if self.held == Held::None { if let Some(bi) = self.data_montage(w).map(|m| m.blend_in) { arms.holster(bi); } }
        self.held = to;
        if let Some(t) = self.tool() { self.tools.entry(t).or_default(); }
    }
    /// The tool actor in the hands (`None` when no blueprint is held).
    fn tool(&self) -> Option<Tool> { match self.held { Held::None => None, Held::Build => Some(Tool::Build(self.build_slot)), Held::Edit => Some(Tool::Edit) } }
    fn actor(&mut self) -> Option<&mut ToolActor> { let t = self.tool()?; Some(self.tools.entry(t).or_default()) }
    fn handle(&mut self, e: Event, arms: &mut Arms, inp: &mut fn_game::FrameInput) {
        match (e, self.held) {
            (Event::Enter, Held::None) => { self.equip_tool(Held::Build, arms, Which::EquipBuild); self.play(Which::EquipBuild); }
            (Event::Exit, Held::Build) => { self.held = Held::None; self.put_away(arms, inp); }
            // ServerCreateBuildingActor_Implementation (0x140845d70): the pawn's place montage (only while the building tool is held, never restarted while playing,
            // FUN_1407f6a70), then vfunc +0x648 (slot 201 = PlayWeaponFireFX) of the current weapon when it is an AFortWeap_BuildingTool: 0x140935590 ->
            // BlueprintPaperPulseTimeline.PlayFromStart (FUN_1424da730: SetPlaybackPosition(0, bFireEvents=0, bFireUpdate=1), bReversePlayback = 0, bPlaying = 1)
            (Event::Place, Held::Build) => {
                if !self.is_playing(Which::Place) { self.play(Which::Place); }
                if let Some(v) = self.data.as_ref().map(|d| curve_at(&d.pulse, 0.)) { if let Some(a) = self.actor() { a.pulse = Pulse { pos: 0., playing: true, value: v }; } }
            }
            // build -> edit: no EquipEdit (prev is a blueprint tool, see `should_play_equip`); gun / pickaxe / nothing -> edit: EquipEdit
            (Event::EditBegin, h) if h != Held::Edit => {
                self.edit_return = h;
                self.equip_tool(Held::Edit, arms, Which::EquipEdit);
                if should_play_equip(h) { self.play(Which::EquipEdit); }
            }
            (Event::EditTile, Held::Edit) => if !self.is_playing(Which::Edit) { self.play(Which::Edit); },
            (Event::EditConfirm | Event::EditCancel, Held::Edit) => {
                // EditingTool::OnUnEquip -> LocalExitEditMode(!bEditConfirmed): FUN_140837140 plays the edit montage only when the pawn's Role < ROLE_Authority (the
                // client view of the port), the pawn is not DBNO (bIsDBNO +0x920 == 0) and the edit was not cancelled (param_2 == 0)
                if e == Event::EditConfirm && !self.dbno && !self.is_playing(Which::Edit) { self.play(Which::Edit); }
                // edit -> build: prev = the editing tool (PaperBlueprint), so no EquipBuild (`should_play_equip`)
                match self.edit_return {
                    Held::Build => { self.equip_tool(Held::Build, arms, Which::EquipBuild); if should_play_equip(Held::Edit) { self.play(Which::EquipBuild); } }
                    _ => { self.held = Held::None; self.put_away(arms, inp); }
                }
            }
            _ => {}
        }
    }
    /// `AFortWeap_BuildingTool::Tick` (0x14093f670) -> `BlueprintPaperPulseTimeline.TickTimeline(dt)` (UE 4.16 `FTimeline::TickTimeline`, not looping, length =
    /// last key): the position advances, clamps at the end (bPlaying = 0) and the interp-float update writes `EmissiveMultiply`
    /// (`OnBlueprintPaperPulseUpdate` 0x14092f190 -> `MID->SetScalarParameterValue`). The tool's actor tick is off while it is not equipped
    /// (`AFortWeapon::OnUnEquip` -> `SetActorTickEnabled(0)`), so the timeline only advances while the building tool is held.
    fn tick_pulse(&mut self, dt: f32) {
        if self.held != Held::Build || self.data.is_none() { return; }
        let t = Tool::Build(self.build_slot);
        let Some(d) = self.data.as_ref() else { return };
        advance_pulse(&mut self.tools.entry(t).or_default().pulse, &d.pulse, dt);
    }
}
/// `ShouldPlayEquipAnimation(Prev)` of the blueprint tools (vslot 0x730 of the AFortWeap_BuildingToolBase / _BuildingTool / _EditingTool vtables 0x14429d978 /
/// 0x14429e348 / 0x14429ed28 -> 0x14093b910): `EquipAnimation != null && !(Prev && Prev->WeaponData && Prev->WeaponCoreAnimation == PaperBlueprint (3))`.
/// `AFortWeapon::PlayEquipAnimation` (0x1409345d0) calls it and plays the montage only when it is true; otherwise `OnEquipFinished` runs at once and nothing
/// stops the running montages. Both tools are PaperBlueprint, so build <-> edit never plays an equip montage (docs/spec/smallfix_followups.md section 1).
/// `prev` is the blueprint tool held before the equip (`Held::None` = a gun / pickaxe / consumable / nothing, never PaperBlueprint).
fn should_play_equip(prev: Held) -> bool {
    let play = prev == Held::None;
    if !play { eprintln!("[build-anim] OnEquip(prev = {prev:?}, PaperBlueprint) -> ShouldPlayEquipAnimation false: no EquipAnimation"); }
    play
}
fn advance_pulse(p: &mut Pulse, keys: &[(f32, f32)], dt: f32) {
    if !p.playing { return; }
    let len = keys.last().map_or(0., |k| k.0);
    let mut pos = p.pos + dt;
    if pos >= len { pos = len; p.playing = false; }
    p.pos = pos;
    p.value = curve_at(keys, pos);
}
/// `UAnimInstance` curve value after the montage slot blend (UE 4.16 `SlotEvaluatePose` combines the montage's own curves into the montage pose, then cross-fades
/// it with the source by the montage weight): `pose * (1 - w) + montage * w`. Used for BothHandFK / LeftHandFK in `Character::evaluate`.
pub fn montage_curve(name: &str, pose_value: f32) -> f32 {
    with(|s| match s.curve(name) { Some((sum, w)) if w > 0. => pose_value * (1. - w.min(1.)) + sum, _ => pose_value })
}
/// `FRichCurve::Eval` of linear keys with constant pre / post extrapolation.
pub(crate) fn curve_at(keys: &[(f32, f32)], t: f32) -> f32 {
    match keys { [] => 0., [k] => k.1, _ => {
        if t <= keys[0].0 { return keys[0].1; }
        for w in keys.windows(2) { if t <= w[1].0 { let a = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-6); return w[0].1 + (w[1].1 - w[0].1) * a; } }
        keys[keys.len() - 1].1 } }
}

// ------------------------------------------------------------------------------------------------------------------------------
// the paper's AnimInstance: Blueprint_Paper_VIM (mesh ref pose -> SpringBone x4 -> component to local)
// ------------------------------------------------------------------------------------------------------------------------------
/// UE cm (x, y, z) -> renderer metres (x, z, y): the axis / unit map of every mesh of the port (`fn_mesh::gltf::ue_to_gltf_position`).
fn ue_to_render() -> Mat4 { Mat4::from_cols(Vec4::new(0.01, 0., 0., 0.), Vec4::new(0., 0., 0.01, 0.), Vec4::new(0., 0.01, 0., 0.), Vec4::W) }
fn component_space(bones: &[PBone], locals: &[Mat4]) -> Vec<Mat4> {
    let mut cs: Vec<Mat4> = Vec::with_capacity(bones.len());
    for (i, b) in bones.iter().enumerate() { let l = locals[i]; cs.push(if b.parent >= 0 { cs[b.parent as usize] * l } else { l }); }
    cs
}
/// UE 4.16 `FAnimNode_SpringBone`: `UpdateInternal` (`RemainingTime += DeltaTime`, FixedTimeStep = 1/120 x TimeDilation 1) then `EvaluateSkeletalControl_AnyThread`
/// for the bone's world position `target` (UE axes, cm). `OwnerVelocity` is 0: `PreUpdate` only reads the owner velocity when the skel component has no owner,
/// and the weapon actor owns the paper mesh. Returns the simulated world location.
fn spring_bone(p: &SpringBoneNode, s: &mut SpringSim, target: Vec3, dt: f32) -> Vec3 {
    s.remaining += dt;
    let step = 1. / 120.;
    if s.remaining == 0. { s.loc = target; s.vel = Vec3::ZERO; }
    while s.remaining > step {
        // BaseTranslation = OwnerVelocity * FixedTimeStep = 0
        if (target - s.loc).length_squared() > p.error_reset * p.error_reset { s.loc = target; s.vel = Vec3::ZERO; }
        let error = target - s.loc;
        let accel = p.stiffness * error - p.damping * s.vel;
        let cut = 1. / step;
        if p.damping > cut { s.vel += (cut / p.damping) * (accel * step); } else { s.vel += accel * step; }
        let mag = s.vel.length();
        if mag * step > p.error_reset { s.vel *= p.error_reset / (mag * step); }
        let old = s.loc;
        s.loc += s.vel * step;
        for a in 0..3 { if !p.translate[a] { s.loc[a] = target[a]; } } // CopyToVectorByFlags(BoneLocation, TargetPos, !bTranslateX, ..)
        if p.limit_displacement {
            let disp = s.loc - target;
            if disp.length_squared() > p.max_displacement * p.max_displacement { s.loc = target + p.max_displacement * disp.normalize_or_zero(); }
        }
        s.vel = (s.loc - old) / step;
        s.remaining -= step;
    }
    s.loc
}
/// One AnimInstance update + evaluation of the paper: component-space ref pose, each spring in evaluation order (`FAnimNode_SkeletalControlBase::EvaluateComponentSpace`:
/// skipped unless `ActualAlpha` is relevant, then `LocalBlendCSBoneTransforms(BoneTransforms, Alpha)`: the new component-space transform (rotation unchanged) is
/// made relative to the parent's current component-space transform and blended with the bone's local transform by Alpha, children keep their local transforms).
/// `component` = paper component -> world (UE axes, cm). Returns the skinning matrices (component space x inverse ref pose, UE cm).
/// A fresh AnimInstance (`sims` empty) first runs `USkeletalMeshComponent::InitAnim` (UE 4.16): `TickAnimation(0.f)` + `RefreshBoneTransforms()`, i.e. one evaluation with
/// `RemainingTime == 0` that puts every spring on its target.
fn evaluate_paper(rig: &PaperRig, sims: &mut Vec<SpringSim>, component: Mat4, dt: f32) -> Vec<Mat4> {
    if sims.len() != rig.springs.len() { *sims = vec![SpringSim::default(); rig.springs.len()]; evaluate_paper_once(rig, sims, component, 0.); }
    evaluate_paper_once(rig, sims, component, dt)
}
fn evaluate_paper_once(rig: &PaperRig, sims: &mut [SpringSim], component: Mat4, dt: f32) -> Vec<Mat4> {
    let mut locals: Vec<Mat4> = rig.bones.iter().map(PBone::local).collect();
    let inv = component.inverse();
    for ((bone, p), sim) in rig.springs.iter().zip(sims.iter_mut()) {
        if p.alpha <= 1e-5 || !p.translate.iter().any(|x| *x) { sim.remaining += dt; continue; } // FAnimWeight::IsRelevant / bNoOffset
        let cs = component_space(&rig.bones, &locals);
        let target = component.transform_point3(cs[*bone].w_axis.truncate());
        let loc = spring_bone(p, sim, target, dt);
        let spring_cs = inv.transform_point3(loc);
        let parent = rig.bones[*bone].parent;
        let local_spring = if parent >= 0 { cs[parent as usize].inverse().transform_point3(spring_cs) } else { spring_cs };
        let (sc, r, t) = locals[*bone].to_scale_rotation_translation();
        let t = if p.alpha >= 1. - 1e-5 { local_spring } else { t.lerp(local_spring, p.alpha) }; // FTransform::BlendWith: translation lerp (rotation / scale equal)
        locals[*bone] = Mat4::from_scale_rotation_translation(sc, r, t);
    }
    component_space(&rig.bones, &locals).iter().zip(&rig.inv_ref_cs).map(|(c, i)| *c * *i).collect()
}

// ------------------------------------------------------------------------------------------------------------------------------
// per-frame hooks
// ------------------------------------------------------------------------------------------------------------------------------
/// Starts the background load once (call every frame; `install` is only evaluated before the first load) and derives the mode events from the build input.
pub fn poll(input: &crate::build_input::BuildInput, install: impl FnOnce() -> Option<std::path::PathBuf>) {
    let building = input.is_building(); let editing = input.is_editing();
    let confirm = input.last_intents().contains(&crate::build_input::BuildIntent::EditConfirm);
    let resource = match input.material() { fn_building::pieces::Material::Wood => 0, fn_building::pieces::Material::Stone => 1, fn_building::pieces::Material::Metal => 2 };
    let start = with(|s| {
        if editing && !s.was_editing { s.events.push_back(Event::EditBegin); }
        if !editing && s.was_editing { s.events.push_back(if confirm { Event::EditConfirm } else { Event::EditCancel }); }
        if building && !s.was_building { s.events.push_back(Event::Enter); }
        if !building && s.was_building { s.events.push_back(Event::Exit); }
        s.was_building = building; s.was_editing = editing; s.resource = s.resource_dev.unwrap_or(resource);
        s.build_slot = input.building_slot(); // the building-piece item = which AFortWeap_BuildingTool actor (see `Tool`)
        s.load.is_none() && s.data.is_none() && !s.failed
    });
    if start { if let Some(dir) = install() { start_load(dir); } }
}
/// Same as `poll` for runs without the build input (scripted runs): only starts the load.
pub fn ensure_loading(install: impl FnOnce() -> Option<std::path::PathBuf>) {
    if with(|s| s.load.is_none() && s.data.is_none() && !s.failed) { if let Some(dir) = install() { start_load(dir); } }
}
fn start_load(dir: std::path::PathBuf) {
    let (tx, rx) = std::sync::mpsc::channel();
    with(|s| s.load = Some(rx));
    std::thread::Builder::new().name("build-anim-load".into()).spawn(move || { let _ = tx.send(load(&dir)); }).ok();
}

/// After the arms step (drive_arms): applies the held tool to the animation graph -- core set, BuildingState, our montages -- and keeps the pickaxe / firearm
/// paths quiet while a blueprint is held.
pub fn drive(sim: &mut fn_game::GameSim, arms: &mut Arms, inp: &mut fn_game::FrameInput, dt: f32) {
    with(|s| {
        if let Some(rx) = &s.load { if let Ok(r) = rx.try_recv() { s.load = None; match r { Ok(d) => { for n in &d.notes { eprintln!("[build-anim] {n}"); } eprintln!("[build-anim] building blueprint + PaperBlueprint set loaded"); s.data = Some(d); } Err(e) => { eprintln!("[build-anim] unavailable: {e}"); s.failed = true; } } } }
        // dev / verification timeline
        if s.timeline.is_none() { s.timeline = Some(fn_live::var("FN_BUILDANIM").ok().map(|v| v.split(',').filter_map(|e| { let (f, n) = e.split_once(':')?; Some((f.trim().parse().ok()?, n.trim().to_owned())) }).collect()).unwrap_or_default()); }
        let now = sim.frames;
        let due: Vec<String> = s.timeline.as_ref().map(|t| t.iter().filter(|(f, _)| *f == now).map(|(_, n)| n.clone()).collect()).unwrap_or_default();
        for n in due { match n.as_str() { "enter" => s.events.push_back(Event::Enter), "exit" => s.events.push_back(Event::Exit), "place" => s.events.push_back(Event::Place), "edit" => s.events.push_back(Event::EditBegin), "tile" => s.events.push_back(Event::EditTile), "confirm" => s.events.push_back(Event::EditConfirm), "cancel" => s.events.push_back(Event::EditCancel), "wood" => s.resource_dev = Some(0), "stone" => s.resource_dev = Some(1), "metal" => s.resource_dev = Some(2), o => eprintln!("[build-anim] unknown FN_BUILDANIM event {o}") } }
        if let Some(r) = s.resource_dev { s.resource = r; }
        if s.data.is_some() { while let Some(e) = s.events.pop_front() { s.handle(e, arms, inp); } }
        s.tick(dt);
        s.tick_pulse(dt);
        s.anim_dt += dt.max(0.);
        let mine = s.montages();
        arms.set_blueprint_held(s.held != Held::None);
        if s.held != Held::None {
            let core = s.data.as_ref().map(|d| d.core).unwrap_or("PaperBlueprint");
            sim.set_held(Some((core, false))); // bIsTargeting ASSUMED false (native; the PaperBlueprint set only has Targeting* assets, like the pickaxe)
            sim.set_weapon_clock(None);
            sim.anim.set_building_state(if s.held == Held::Edit { BUILDING_STATE_EDIT } else { BUILDING_STATE_PLACEMENT });
            sim.arms = arms.fade_montages(); // the firearm is not in the hands: only its montages stopped by our equip still blend out
            inp.attack = false; inp.non_melee = true; inp.equip = false;
        } else {
            sim.anim.set_building_state(BUILDING_STATE_NONE);
        }
        sim.arms.extend(mine);
    });
}
/// Before `Scene::update`: the blueprint replaces the pickaxe / firearm in the hands.
pub fn pre_scene(scene: &mut crate::scene::Scene) {
    if with(|s| s.held != Held::None) { scene.show_pickaxe = false; scene.held = None; }
}
/// After `Scene::update`: one-time install (clips, sockets, GPU models), then the paper (springs, glow) / pencil on their sockets of this frame's final pose.
pub fn frame(scene: &mut crate::scene::Scene, sim: &fn_game::GameSim) {
    with(|s| {
        let Some(d) = s.data.as_mut() else { return };
        if !s.installed {
            s.installed = true;
            if let Some(extra) = d.clips.take() {
                let base = std::mem::replace(&mut scene.character.clips, Box::new(ClipStore::new()));
                eprintln!("[build-anim] {} player clips added to the character library", extra.clips.len());
                scene.character.clips = Box::new(crate::weapon_runtime::LayeredClips { base, extra });
            }
            for so in &d.sockets {
                if scene.character.rig.parts.get(so.part).and_then(|p| p.tree.bone_index(&so.bone)).is_none() { eprintln!("[build-anim] socket bone {} missing from the character", so.bone); continue; }
                if !scene.character.rig.sockets.iter().any(|x| x.name == so.name) { scene.character.rig.sockets.push(so.clone()); }
            }
            let n = d.rig.bones.len();
            let build = d.paper_build.iter().map(|p| scene.renderer.create_model(p, n)).collect();
            let edit = scene.renderer.create_model(&d.paper_edit, n);
            let pencil = scene.renderer.create_model(&d.pencil, 1);
            d.paper_build = vec![];
            d.paper_edit = vec![];
            d.pencil = vec![];
            s.models = Some(Models { build, edit, pencil });
        }
        let dt = std::mem::take(&mut s.anim_dt);
        if s.held == Held::None { s.shown = None; }
        if s.held == Held::None || sim.cam_view.hide_pawn { return; }
        let (Some(m), Some(d)) = (s.models.as_ref(), s.data.as_ref()) else { return };
        let pose = &scene.character.pose;
        let world = crate::scene::pawn_matrix(sim);
        let (Some(paper), Some(pen)) = (pose.socket(&d.sockets[0].name), pose.socket(&d.sockets[1].name)) else { return };
        let paper_model = if s.held == Held::Edit { m.edit } else { m.build[s.resource.min(m.build.len() - 1)] };
        // the paper component follows the BluePrintPaper socket; its AnimInstance runs the spring chain in world space (UE axes, cm)
        let c = ue_to_render();
        let component = c.inverse() * world * paper * c;
        let Some(tool) = s.tool() else { return };
        if s.shown != Some(tool) { let a = s.tools.get(&tool); eprintln!("[build-anim] frame {}: tool actor {tool:?} in the hands ({}; pulse pos {:.3} value {:.2})", sim.frames, if a.map_or(true, |a| a.springs.is_empty()) { "fresh AnimInstance".to_owned() } else { "reused actor, kept spring state".to_owned() }, a.map_or(0., |a| a.pulse.pos), a.map_or(1., |a| a.pulse.value)); s.shown = Some(tool); }
        let actor = s.tools.entry(tool).or_default();
        let skin = evaluate_paper(&d.rig, &mut actor.springs, component, dt);
        let pulse = actor.pulse.value;
        let joints: Vec<Mat4> = skin.iter().map(|k| paper * c * *k * c.inverse()).collect();
        scene.renderer.update_model(paper_model, world, &joints);
        // the building tool's MID: EmissiveMultiply from the pulse timeline (the edit tool has no MID: its paper and the pencil keep the MIC's 1.0)
        if s.held == Held::Build { scene.renderer.set_model_scalar(paper_model, pulse); }
        scene.renderer.update_model(m.pencil, world, &[pen]);
        scene.models.push(paper_model);
        scene.models.push(m.pencil);
    });
}

// ------------------------------------------------------------------------------------------------------------------------------
// loading (worker thread)
// ------------------------------------------------------------------------------------------------------------------------------
fn pkg_of(r: &str) -> &str { r.split('.').next().unwrap_or(r) }
pub fn load(dir: &std::path::Path) -> Result<Data, String> {
    let paks = fn_pak::PakSet::open_install(dir).map_err(|e| e.to_string())?;
    let src = ClipSourceDir::Pak(&paks);
    let set = fn_pose::loadout_build::load(&src)?;
    let mut notes = vec![];
    // animation closure: montages -> sequences, blend spaces -> samples, sequences (+ additive ref poses)
    let mut sequences: HashMap<String, fn_pose::loadout::Sequence> = HashMap::new();
    let mut montages: HashMap<String, fn_anim::montage::AnimMontage> = HashMap::new();
    let mut skeletons: HashMap<String, fn_anim::SkeletonInfo> = HashMap::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<String> = set.anim_packages().into();
    while let Some(p) = queue.pop_front() {
        let p = pkg_of(&p).to_owned();
        if !seen.insert(p.clone()) { continue; }
        let pkg = fn_pose::loader::package(&src, &p)?;
        if pkg.find_export("AnimMontage").is_some() {
            let m = fn_anim::montage::AnimMontage::parse(&pkg).map_err(|e| format!("{p}: {e}"))?;
            for sl in &m.slots { for sg in &sl.segments { queue.push_back(sg.animation_path.clone().ok_or_else(|| format!("{p}: segment without animation"))?); } }
            montages.insert(p, m);
        } else if (0..pkg.exports.len()).any(|i| fn_anim::BlendSpaceKind::from_class(&pkg.export_class(i)).is_some()) {
            let b = fn_anim::BlendSpace::parse(&pkg).map_err(|e| format!("{p}: {e}"))?;
            for s in &b.samples { if let Some(r) = &s.animation_path { queue.push_back(r.clone()); } }
        } else if pkg.find_export("AnimSequence").is_some() {
            let source = fn_anim::AnimSequence::parse(&pkg).map_err(|e| format!("{p}: {e}"))?;
            if let Some(r) = &source.additive.ref_pose_seq_path { queue.push_back(r.clone()); }
            let skeleton = pkg_of(source.skeleton_path.as_deref().ok_or_else(|| format!("{p}: no skeleton"))?).to_owned();
            if !skeletons.contains_key(&skeleton) { skeletons.insert(skeleton.clone(), fn_anim::SkeletonInfo::from_package(&fn_pose::loader::package(&src, &skeleton)?).map_err(|e| e.to_string())?); }
            let clip = FnClip::from_sequence(&source, &skeletons[&skeleton], SampleMode::Ue);
            sequences.insert(p, fn_pose::loadout::Sequence { source, skeleton, clip });
        } else { notes.push(format!("{p}: not an animation asset, skipped")); }
    }
    let mut clips = ClipStore::new();
    let mut names: HashMap<String, usize> = HashMap::new();
    for s in sequences.values().filter(|s| s.skeleton == fn_pose::loadout::PLAYER_SKELETON) { *names.entry(s.clip.name.clone()).or_default() += 1; }
    for (path, s) in &sequences {
        if s.skeleton != fn_pose::loadout::PLAYER_SKELETON { notes.push(format!("{path}: not a player-skeleton sequence, not installed")); continue; }
        clips.insert(path, s.clip.clone());
        if names[&s.clip.name] == 1 { clips.insert(&s.clip.name, s.clip.clone()); } // the graph names the pose players / core assets by base name
    }
    let mdata = |path: &str| -> Result<MontageData, String> { let m = montages.get(pkg_of(path)).ok_or_else(|| format!("{path}: montage not loaded"))?; MontageData::from_anim_in(m, &sequences) };
    let eb = set.build.equip_montage().ok_or("building tool has no EquipAnimation")?.to_owned();
    let ee = set.edit.equip_montage().ok_or("editing tool has no EquipAnimation")?.to_owned();
    let paths = [eb.clone(), ee.clone(), set.place_montage.clone(), set.edit_montage.clone()];
    let montages_d = [mdata(&paths[0])?, mdata(&paths[1])?, mdata(&paths[2])?, mdata(&paths[3])?];
    let curves = [fn_pose::loadout_build::montage_curves(&src, &paths[0])?, fn_pose::loadout_build::montage_curves(&src, &paths[1])?, fn_pose::loadout_build::montage_curves(&src, &paths[2])?, fn_pose::loadout_build::montage_curves(&src, &paths[3])?];
    // meshes: the building tool's MID (BlueprintPaperMaterial with Diffuse / EmissiveMask = Blueprint{Diffuse,EmissiveMask}Textures[CurrentResourceType], AFortWeap_BuildingTool::Tick
    // 0x14093f670; LastResourceType starts at 4 (ctor 0x140914770, +0xBF0), so the first tick always swaps)
    let mut paper_build = vec![];
    let mut rig = None;
    for r in 0..3 {
        let diffuse = set.build.diffuse[r].clone().ok_or_else(|| format!("BlueprintDiffuseTextures[{r}] unset"))?;
        let emissive = set.build.emissive[r].clone().ok_or_else(|| format!("BlueprintEmissiveMaskTextures[{r}] unset"))?;
        let mat = set.build.paper_material.clone().ok_or("no BlueprintPaperMaterial")?;
        let (prims, bones) = mesh_prims(&src, &set.build.weapon_mesh, true, Some((mat.as_str(), diffuse.as_str(), emissive.as_str())), &mut notes)?;
        paper_build.push(prims);
        if rig.is_none() { rig = bones; }
    }
    let (paper_edit, _) = mesh_prims(&src, &set.edit.weapon_mesh, true, None, &mut notes)?;
    if set.edit.weapon_mesh != set.build.weapon_mesh || set.edit.weapon_anim_class != set.build.weapon_anim_class { return Err("build / edit tools hold different papers".into()); }
    let (pencil, _) = mesh_prims(&src, set.build.pencil_mesh.as_deref().ok_or("no pencil mesh")?, false, None, &mut notes)?;
    // the paper's AnimBlueprint (WeaponMesh0.AnimClass): its SpringBone chain on the paper skeleton
    let bones = rig.ok_or("paper mesh has no skeleton")?;
    let (chain, _) = fn_pose::loadout_build::spring_bones(&src, set.build.weapon_anim_class.as_deref().ok_or("paper mesh has no AnimClass")?)?;
    let pb: Vec<PBone> = bones.iter().map(|b| PBone { parent: b.parent, t: Vec3::from(b.ref_pos), r: Quat::from_xyzw(b.ref_rot[0], b.ref_rot[1], b.ref_rot[2], b.ref_rot[3]).normalize(), s: Vec3::from(b.ref_scale) }).collect();
    let locals: Vec<Mat4> = pb.iter().map(PBone::local).collect();
    let inv_ref_cs = component_space(&pb, &locals).iter().map(|m| m.inverse()).collect();
    let springs = chain.into_iter().map(|n| bones.iter().position(|b| b.name == n.bone).map(|i| (i, n.clone())).ok_or_else(|| format!("paper skeleton has no {}", n.bone))).collect::<Result<Vec<_>, _>>()?;
    let rig = PaperRig { bones: pb, inv_ref_cs, springs };
    // BlueprintPaperPulseCurve: the timeline's interp float (PostInitializeComponents 0x1409359b0: LengthMode TL_LastKeyFrame, not looping)
    let keys = fn_pose::loadout_build::curve_float(&src, set.build.pulse_curve.as_deref().ok_or("no BlueprintPaperPulseCurve")?)?;
    if keys.iter().any(|k| k.2 != 0) { return Err("BlueprintPaperPulseCurve has non-linear keys (not ported)".into()); }
    let pulse = keys.iter().map(|k| (k.0, k.1)).collect();
    let sockets = [SocketDef::from_original_yaw_socket(0, &set.paper_socket)?, SocketDef::from_original_yaw_socket(0, &set.pencil_socket)?];
    let core = fn_animgraph::CORE_ANIMS_MALE.iter().map(|(k, _)| *k).find(|k| *k == set.build.anims.core).ok_or_else(|| format!("{} is not a CoreAnims_Male key", set.build.anims.core))?;
    Ok(Data { set, montages: montages_d, curves, clips: Some(clips), paper_build, paper_edit, pencil, rig, pulse, sockets, core, notes })
}

/// Texture parameters of a material instance chain (child first wins), the fixed (non-parameter) `MaterialExpressionTextureSample` textures of the root
/// material (sampler type, texture) and the root material's path.
struct MatTextures { params: Vec<(String, String)>, fixed: Vec<(String, String)>, root: String }
fn material_textures(src: &ClipSourceDir, material: &str) -> Result<MatTextures, String> {
    use fn_ueio::props::{read_props, Ctx, PropValue};
    let mut out = MatTextures { params: vec![], fixed: vec![], root: String::new() };
    let mut cur = Some(material.to_owned());
    for _ in 0..16 {
        let Some(path) = cur.take() else { break };
        if !path.starts_with("/Game/") { break; }
        let long = pkg_of(&path);
        let obj = path.split_once('.').map(|x| x.1).unwrap_or_else(|| long.rsplit('/').next().unwrap_or(long));
        let pkg = fn_pose::loader::package(src, long)?;
        let idx = pkg.exports.iter().position(|e| e.object_name == obj).ok_or_else(|| format!("{path}: no export"))?;
        let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
        let read = |i: usize| { let (a, b) = pkg.export_range(i); read_props(&mut fn_ueio::Reader::at(&pkg.data[..b], a), &ctx).map_err(|e| format!("{path}: {e}")) };
        let props = read(idx)?;
        for v in props.array("TextureParameterValues").into_iter().flatten() {
            let Some(f) = v.as_fields() else { continue };
            let name = f.text("ParameterName").unwrap_or("").to_owned();
            if let Some(i) = f.object("ParameterValue").filter(|i| *i < 0) { if !out.params.iter().any(|(n, _)| *n == name) { out.params.push((name, pkg.import_path(i))); } }
        }
        out.root = path.clone();
        cur = match props.get("Parent") { Some(PropValue::Object(i)) if *i < 0 => Some(pkg.import_path(*i)), _ => None };
        if cur.is_none() && pkg.export_class(idx) == "Material" {
            for i in 0..pkg.exports.len() {
                if pkg.export_class(i) != "MaterialExpressionTextureSample" { continue; }
                let p = read(i)?;
                if let Some(t) = p.object("Texture").filter(|t| *t < 0) { out.fixed.push((p.text("SamplerType").unwrap_or("SAMPLERTYPE_Color").to_owned(), pkg.import_path(t))); }
            }
        }
    }
    Ok(out)
}
type Cooked = Option<std::sync::Arc<fn_render::mesh::CookedTexture>>;
fn image(src: &ClipSourceDir, path: &str) -> Result<(image::RgbaImage, Cooked), String> {
    let t = fn_pose::loadout_material::load_texture(src, path)?;
    let cooked = crate::br_character::cooked_texture(&t.source);
    if cooked.is_some() { return Ok((crate::br_character::stub_image(), cooked)); }
    let img = image::RgbaImage::from_raw(t.image.width as u32, t.image.height as u32, t.image.to_rgba8()).ok_or_else(|| format!("{path}: bad decoded size"))?;
    Ok((img, cooked))
}
fn split(t: Option<(image::RgbaImage, Cooked)>) -> (Option<image::RgbaImage>, Cooked) { match t { Some((i, c)) => (Some(i), c), None => (None, None) } }
/// The root material whose compiled shading `MaterialKind::Architect` reproduces.
const ARCHITECT_MATERIAL: &str = "/Game/Packages/Fortnite_UniqueMaterials_1/Character/M_BluePrintArchitect";
/// LOD 0 of a skeletal / static mesh in the mesh's own space (UE cm -> metres, glTF axes, like every held mesh of the port). A skeletal mesh is skinned to its own
/// RefSkeleton (mesh bone indices; the bones are returned), a static mesh is rigid (joint 0). `mid` = (MID parent material, Diffuse override, EmissiveMask override) for
/// every section. Sections of an `M_BluePrintArchitect` chain draw with `MaterialKind::Architect` (Diffuse, Normal, the fixed `SAMPLERTYPE_Masks` sample, EmissiveMask),
/// others with the Prop shading (Diffuse / Normal).
pub(crate) fn mesh_prims(src: &ClipSourceDir, mesh: &str, skeletal: bool, mid: Option<(&str, &str, &str)>, notes: &mut Vec<String>) -> Result<(Vec<SkinnedPrimitive>, Option<Vec<fn_mesh::Bone>>), String> {
    use fn_mesh::gltf::{ue_to_gltf_direction, ue_to_gltf_position};
    let pkg = fn_pose::loader::package(src, pkg_of(mesh))?;
    struct Sec { v: Vec<SkinnedVertex>, idx: Vec<u32>, material: Option<String> }
    let mut secs = vec![];
    type Vert = ([f32; 3], [f32; 3], [f32; 2], [u16; 4], [f32; 4]);
    let mut add = |ix: &[u32], pos: &dyn Fn(u32) -> Vert, material: Option<String>| {
        let mut map: HashMap<u32, u32> = HashMap::new();
        let mut s = Sec { v: vec![], idx: vec![], material };
        for &i in ix { let l = *map.entry(i).or_insert_with(|| { let (p, n, uv, joints, weights) = pos(i); s.v.push(SkinnedVertex { pos: p, normal: n, uv, joints, weights }); (s.v.len() - 1) as u32 }); s.idx.push(l); }
        secs.push(s);
    };
    let mut bones = None;
    if skeletal {
        let m = fn_mesh::SkeletalMesh::parse(&pkg).map_err(|e| format!("{mesh}: {e}"))?;
        let lod = m.lods.first().ok_or_else(|| format!("{mesh}: no LOD"))?;
        for (si, s) in lod.sections.iter().enumerate().filter(|(_, s)| !s.disabled) {
            let (b, n) = (s.base_index as usize, s.num_triangles as usize * 3);
            let Some(ix) = lod.indices.get(b..b + n) else { continue };
            let f = |i: u32| {
                // GPU skin weights: the vertex's influences through the section bone map, the 4 strongest renormalised (4.16 meshes of this LOD store 4)
                let mut inf = lod.resolved_influences(i as usize, si);
                inf.sort_by(|a, b| b.1.cmp(&a.1));
                let (mut joints, mut weights) = ([0u16; 4], [0f32; 4]);
                let sum: f32 = inf.iter().take(4).map(|x| x.1 as f32).sum();
                for (k, (bone, w)) in inf.iter().take(4).enumerate() { joints[k] = *bone; weights[k] = if sum > 0. { *w as f32 / sum } else { 0. }; }
                if sum <= 0. { weights[0] = 1.; }
                (ue_to_gltf_position(lod.positions[i as usize]), ue_to_gltf_direction(lod.normal(i as usize)), lod.uvs.first().and_then(|u| u.get(i as usize)).copied().unwrap_or([0., 0.]), joints, weights)
            };
            add(ix, &f, m.materials.get(s.material_index.max(0) as usize).and_then(|x| x.material.clone()));
        }
        bones = Some(m.ref_skeleton.clone());
    } else {
        let m = fn_mesh::StaticMesh::parse(&pkg).map_err(|e| format!("{mesh}: {e}"))?;
        let lod = m.lods.first().ok_or_else(|| format!("{mesh}: no LOD"))?;
        for s in &lod.sections {
            let (b, n) = (s.first_index as usize, s.num_triangles as usize * 3);
            let Some(ix) = lod.indices.get(b..b + n) else { continue };
            let f = |i: u32| (ue_to_gltf_position(lod.positions[i as usize]), ue_to_gltf_direction(lod.normal(i as usize)), lod.uvs.first().and_then(|u| u.get(i as usize)).copied().unwrap_or([0., 0.]), [0; 4], [1., 0., 0., 0.]);
            add(ix, &f, m.materials.get(s.material_index.max(0) as usize).and_then(|x| x.material.clone()));
        }
    }
    if secs.is_empty() { return Err(format!("{mesh}: no drawable section")); }
    let mut cache: HashMap<String, (MaterialImages, MaterialKind)> = HashMap::new();
    let mut out = vec![];
    for s in secs {
        let mat = match mid { Some((parent, ..)) => Some(parent.to_owned()), None => s.material.clone() };
        let (material, kind) = match &mat {
            Some(mat) => cache.entry(mat.clone()).or_insert_with(|| {
                let tex = material_textures(src, mat).unwrap_or_else(|e| { notes.push(format!("{mesh}: material {mat}: {e}")); MatTextures { params: vec![], fixed: vec![], root: String::new() } });
                let find = |n: &str| tex.params.iter().find(|(k, _)| k == n).map(|(_, p)| p.clone());
                let load = |p: Option<String>, what: &str, notes: &mut Vec<String>| p.and_then(|p| image(src, &p).map_err(|e| notes.push(format!("{mesh}: {what} {p}: {e}"))).ok());
                let (diffuse, cd) = split(load(mid.map(|m| m.1.to_owned()).or_else(|| find("Diffuse")), "Diffuse", notes));
                let (normal, cn) = split(load(find("Normal"), "Normal", notes));
                // the material's TwoSided flag (a MID keeps its parent's): both faces in the shadow depth (UE4.16 ShadowDepthRendering.cpp:1059)
                let two_sided = fn_pose::br_character::material_two_sided(src, mat).unwrap_or_else(|e| { notes.push(format!("{mesh}: material {mat}: TwoSided not resolved ({e}), one-sided assumed")); false });
                if pkg_of(&tex.root) == ARCHITECT_MATERIAL {
                    let masks = tex.fixed.iter().find(|(st, _)| st.ends_with("SAMPLERTYPE_Masks")).map(|(_, p)| p.clone());
                    if masks.is_none() { notes.push(format!("{mesh}: {ARCHITECT_MATERIAL} has no fixed Masks sample")); }
                    let (spec, cs) = split(load(masks, "SpecularMasks", notes));
                    let (mask, cm) = split(load(mid.map(|m| m.2.to_owned()).or_else(|| find("EmissiveMask")), "EmissiveMask", notes));
                    (MaterialImages { diffuse, normal, spec, mask, ue: None, two_sided, cooked: [cd, cn, cs, cm] }, MaterialKind::Architect)
                } else {
                    (MaterialImages { diffuse, normal, spec: None, mask: None, ue: None, two_sided, cooked: [cd, cn, None, None] }, MaterialKind::Prop)
                }
            }).clone(),
            None => { notes.push(format!("{mesh}: a section has no material")); (MaterialImages::default(), MaterialKind::Prop) }
        };
        out.push(SkinnedPrimitive { vertices: s.v, indices: s.idx, material, kind });
    }
    Ok((out, bones))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The whole set loads from the 1.8 paks: four montages, the pose / core clips on the player skeleton, three resource papers + edit paper + pencil with the
    /// Architect material, the paper skeleton with its four springs, the pulse curve, the equip sound notify.
    #[test] fn build_anim_data_loads() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let d = load(&root).unwrap();
        for n in &d.notes { eprintln!("note: {n}"); }
        assert_eq!(d.core, "PaperBlueprint");
        assert_eq!(d.montages.iter().map(|m| m.slot).collect::<Vec<_>>(), ["UpperBody"; 4]);
        assert!((d.montages[0].blend_in.0 - 0.667).abs() < 1e-4 && d.montages[0].blend_in.1 == 2);
        // Blueprint_Equip: AnimNotify_PlaySound "Player_Equip_Blueprint_Cue" at 0.035156 s
        assert_eq!(d.montages[0].fx.len(), 1);
        assert!((d.montages[0].fx[0].time - 0.035156).abs() < 1e-5);
        let Fx::Sound { cue, pitch, volume } = &d.montages[0].fx[0].fx else { panic!("not a sound") };
        assert!(cue.ends_with("Player_Equip_Blueprint_Cue") && *pitch == 1. && *volume == 1.);
        assert_eq!(fn_audio::weapon_events::by_cue_path(cue).map(|r| r.event), Some("build.equip"));
        let clips = d.clips.as_ref().unwrap();
        for n in ["M_BluePrint_Place", "M_BluePrint_Edit", "/Game/Animation/Game/MainPlayer/Building/Blueprint_Equip", "/Game/Animation/Game/MainPlayer/Building/BluePrint_Edit_ConfirmCheck"] { assert!(clips.clips.contains_key(n), "{n}"); }
        let core = fn_animgraph::core_anims("PaperBlueprint").unwrap();
        for n in [core.targeting_pose, core.sprint, core.crouch_targeting_pose, core.crouch_sprint] { assert!(clips.clips.contains_key(n), "core clip {n}"); }
        assert_eq!(d.paper_build.len(), 3);
        // M_BluePrintArchitect serializes TwoSided = true: the paper casts shadows from both faces (UE4.16 ShadowDepthRendering.cpp:1059)
        assert!(d.paper_build.iter().flatten().chain(&d.paper_edit).all(|p| p.material.two_sided), "the blueprint paper is two-sided");
        for p in d.paper_build.iter().chain([&d.paper_edit, &d.pencil]) {
            assert!(!p.is_empty() && p.iter().all(|s| s.kind == MaterialKind::Architect && s.material.diffuse.is_some() && s.material.normal.is_some() && s.material.spec.is_some() && s.material.mask.is_some()));
        }
        let bones = d.rig.bones.len();
        assert!(d.paper_build[0].iter().flat_map(|p| &p.vertices).all(|v| v.joints.iter().all(|j| (*j as usize) < bones)));
        assert!(d.paper_build[0].iter().flat_map(|p| &p.vertices).any(|v| v.joints[0] != 0), "the paper is skinned to its spring bones");
        assert_eq!(d.rig.springs.len(), 4);
        assert_eq!(d.pulse.len(), 3);
        assert_eq!((d.sockets[0].bone.as_str(), d.sockets[1].bone.as_str()), ("weapon_l", "weapon_r"));
        assert_eq!(d.sockets[0].local, Mat4::IDENTITY);
    }
    fn md(len: f32, bi: f32, bo: f32) -> MontageData { MontageData { length: len, blend_in: (bi, 2), blend_out: (bo, 2), slot: "UpperBody", segs: vec![crate::arms::Seg { clip: "c".into(), start: 0., anim_start: 0., anim_end: len, rate: 1., loops: 1 }], notifies: vec![], fx: vec![], blend: fn_animgraph::SlotBlend::Pose } }
    /// UE 4.16 montage instance weights: Blueprint_Equip's 0.667 s blend-in never completes; the auto blend-out starts at 0.333 s from the current weight.
    #[test] fn montage_weights_follow_ue_instances() {
        let mut s = new_state();
        let m = md(0.533333, 0.667, 0.2);
        let i = Inst { which: Which::EquipBuild, t: 0.333333, stop: None };
        let w0 = weight(&m, &i);
        assert!((w0 - ease(2, 0.333333 / 0.667)).abs() < 1e-6);
        let stopped = Inst { which: Which::EquipBuild, t: 0.433333, stop: Some((0.333333, w0, 0.2, 2)) };
        assert!((weight(&m, &stopped) - w0 * 0.5).abs() < 1e-5);
        assert!(!s.is_playing(Which::Place));
        s.insts.push(Inst { which: Which::Place, t: 0.1, stop: None }); assert!(s.is_playing(Which::Place));
        s.insts[0].stop = Some((0.1, 1., 0.09, 2)); assert!(!s.is_playing(Which::Place), "Montage_IsPlaying is false once blending out");
        assert_eq!(curve_at(&[(0., 1.)], 0.3), 1.);
    }
    fn node() -> SpringBoneNode { SpringBoneNode { bone: "Spring_1".into(), alpha: 0.5, stiffness: 100., damping: 10., limit_displacement: true, max_displacement: 40., error_reset: 256., translate: [true; 3], rotate: [false; 3] } }
    /// UE 4.16 SpringBone: fixed 120 Hz steps; a resting target stays put; a target step is followed with the semi-implicit spring (100, 10) and the 40 cm
    /// displacement limit; an error above 256 cm resets to the target.
    #[test] fn spring_bone_matches_the_ue_integrator() {
        let p = node();
        let mut s = SpringSim::default();
        // constructor BoneLocation = 0; the first update (RemainingTime = dt > 0) starts from there and the 300 cm error resets it to the target
        let t = Vec3::new(300., 0., 0.);
        assert_eq!(spring_bone(&p, &mut s, t, 1. / 60.), t);
        assert!(s.remaining <= 1. / 120. + 1e-6);
        // InitAnim's zero-delta evaluation (RemainingTime == 0) snaps onto the target
        let mut z = SpringSim::default();
        assert_eq!(spring_bone(&p, &mut z, Vec3::new(1., 2., 3.), 0.), Vec3::new(1., 2., 3.));
        // a 10 cm target step: one 1/120 s step by hand
        let t2 = t + Vec3::new(0., 10., 0.);
        let mut e = s; e.remaining += 1. / 60.;
        let step = 1. / 120.;
        let mut loc = e.loc; let mut vel = e.vel;
        while e.remaining > step { let old = loc; vel += (100. * (t2 - loc) - 10. * vel) * step; loc += vel * step; vel = (loc - old) / step; e.remaining -= step; }
        let got = spring_bone(&p, &mut s, t2, 1. / 60.);
        assert!((got - loc).length() < 1e-4, "{got} vs {loc}");
        // displacement limit: a 60 cm step lags at most 40 cm
        let mut s2 = SpringSim { remaining: 0., loc: Vec3::ZERO, vel: Vec3::ZERO };
        let got = spring_bone(&p, &mut s2, Vec3::new(0., 0., 60.), 1. / 120. + 1e-4);
        assert!((got - Vec3::new(0., 0., 20.)).length() < 1e-3, "{got}");
        // settles back onto the target
        for _ in 0..600 { spring_bone(&p, &mut s2, Vec3::new(0., 0., 60.), 1. / 60.); }
        assert!((s2.loc - Vec3::new(0., 0., 60.)).length() < 1e-3);
    }
    /// The paper's spring chain: at rest the ref pose; when the component moves the spring bones lag behind by Alpha 0.5 of the simulated offset, children follow.
    #[test] fn paper_springs_blend_half_the_lag_in_local_space() {
        let pb = |parent: i32, z: f32| PBone { parent, t: Vec3::new(0., 0., z), r: Quat::IDENTITY, s: Vec3::ONE };
        let bones = vec![pb(-1, 0.), pb(0, 10.), pb(1, 10.)];
        let locals: Vec<Mat4> = bones.iter().map(PBone::local).collect();
        let inv_ref_cs = component_space(&bones, &locals).iter().map(|m| m.inverse()).collect();
        let rig = PaperRig { bones, inv_ref_cs, springs: vec![(1, node())] };
        let mut sims = vec![];
        let at = |x: f32| Mat4::from_translation(Vec3::new(x, 0., 0.));
        let skin = evaluate_paper(&rig, &mut sims, at(0.), 1. / 60.);
        assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "rest pose");
        // move the component 4 cm along X within one 1/120 s step: the spring keeps its world spot for that step only partly
        let skin = evaluate_paper(&rig, &mut sims, at(4.), 1. / 120. + 1e-4);
        let lag = sims[0].loc - Vec3::new(4., 0., 10.);
        let d1 = skin[1].w_axis.truncate();
        assert!((d1 - 0.5 * lag).length() < 1e-4, "{d1} vs half of {lag}");
        assert!((skin[2].w_axis.truncate() - d1).length() < 1e-5, "the child keeps its local transform");
        assert!(skin[0].abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }
    /// The glow pulse: PlayFromStart on a placement while the building tool is held, linear 1 -> 7 (0.15 s) -> 1 (0.30 s), then it rests at 1.
    #[test] fn pulse_timeline_follows_the_curve() {
        let keys = [(0.00153, 1.), (0.15, 7.), (0.30, 1.)];
        let mut p = Pulse { pos: 0., playing: true, value: curve_at(&keys, 0.) };
        assert_eq!(p.value, 1.);
        advance_pulse(&mut p, &keys, 0.15); assert!((p.value - 7.).abs() < 1e-5);
        advance_pulse(&mut p, &keys, 0.075); assert!((p.value - 4.).abs() < 1e-4);
        advance_pulse(&mut p, &keys, 0.2); assert!(!p.playing && p.value == 1. && p.pos == 0.30);
        advance_pulse(&mut p, &keys, 0.2); assert_eq!(p.value, 1.);
    }
    /// One weapon actor per tool item (EquipWeaponDefinition 0x1407d9590 reuses it by item GUID): the springs / pulse of a building piece survive a switch to the
    /// edit tool or another piece and back, and every item starts from its own fresh actor.
    /// 0x14093b910: a blueprint tool equipped over a blueprint tool (build <-> edit) plays no equip montage; over anything else it does.
    #[test] fn blueprint_to_blueprint_plays_no_equip() {
        assert!(should_play_equip(Held::None), "gun / pickaxe / nothing -> build or edit: EquipBuild / EquipEdit");
        assert!(!should_play_equip(Held::Build), "build -> edit: no EquipEdit");
        assert!(!should_play_equip(Held::Edit), "edit -> build: no EquipBuild");
    }
    #[test] fn tool_state_is_kept_per_tool_actor() {
        let mut s = new_state();
        s.held = Held::Build;
        let a = s.actor().unwrap(); a.springs = vec![SpringSim { remaining: 0.004, loc: Vec3::X, vel: Vec3::Y }]; a.pulse = Pulse { pos: 0.1, playing: true, value: 5. };
        s.held = Held::Edit; assert!(s.actor().unwrap().springs.is_empty(), "the edit tool has its own actor");
        s.held = Held::Build; s.build_slot = 1; assert!(s.actor().unwrap().springs.is_empty() && s.actor().unwrap().pulse == Pulse::default(), "another building piece = another actor");
        s.build_slot = 0; let a = s.actor().unwrap();
        assert_eq!(a.springs, vec![SpringSim { remaining: 0.004, loc: Vec3::X, vel: Vec3::Y }]); assert_eq!(a.pulse.value, 5.);
        s.held = Held::None; assert!(s.actor().is_none());
    }
}
