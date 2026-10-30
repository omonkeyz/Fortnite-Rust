//! DOORS (2026-10-08): the door of an `ABuildingWall`, shared by the map's door walls (`WoodHouse_DoorCenter_*_C`, `ShutterHouse_Door*_C`, ... in the POI /
//! house worlds) and the player-built door edits (`PBWA_W1_DoorC_C`, `PBWA_*_DoorSide_C`, ...). Pure logic (no GPU, no audio): the app's door hook feeds it
//! the use key, the frame time and the pawn-overlap test, and draws / collides / plays what it returns.
//!
//! 1.8 is native C++ only (no Blueprint graph, no timeline asset): every value below is from the 1.8 exe (addresses: `C:\FortniteRef\decomp\named`,
//! base 0x140000000; constants read from the exe image) and the class defaults.
//! * Data (SDK `ABuildingWall`, FN_FortniteGame_classes.hpp): `DoorOffset` +0xFF8, `DoorOpeningSound` +0x1028, `DoorClosingSound` +0x1030,
//!   `DoorAnimatingMaterial` +0x1038, `DoorMesh` +0x1040, `DoorComponent` +0x1048, `bDoorOpen` +0x1070 bit 0, `bDoorCollisionDisabled` bit 1,
//!   `DoorDesiredRotation` +0x1074; native-only (no UPROPERTY): the closed rotation +0x1080, the open delta +0x108C (ctor @14049fe20: (0, 90, 0), the 90 =
//!   DAT_14404e220), the auto-close timer handle +0x1098.
//! * The door component (FUN_1404abb40, from `PreInitializeComponents` @1404c0210, only when `DoorMesh` is set): a movable `UStaticMeshComponent`
//!   "DoorComponent" attached to the root, mesh = `DoorMesh`, material slot 0 = `DoorAnimatingMaterial` (null = the mesh's own), relative location =
//!   `DoorOffset`, collision profile `FortBuildingMesh` (QueryOnly, blocks Pawn / weapon / interaction traces: DefaultEngine.ini), CastShadow +
//!   bCastDynamicShadow + bCastStaticShadow, `CanCharacterStepUpOn` = Owner, cull distance = `ABuildingActor::CullDistance` (+0x6C8, transient 0 = none).
//!   Closed rotation = desired rotation = the component's world rotation after registering.
//! * Interact (`IFortInteractInterface` of ABuildingWall at +0x388: FUN_1404c3a60): only when the interaction hit component IS the DoorComponent; with the
//!   Athena game state (`AFortGameStateAthena` vtable +0x8A0 and +0x8A8 = FUN_141d36860 `return 1`; the Zone base returns 0) an open door closes, a closed
//!   one opens, the instigator = the interacting pawn.
//! * Prompt text (FUN_1404adc10): `Close` (`Fort.Building.Door.Defaults`/`Close`, DAT_14521ff98) when the Athena door is open, else `Open` (DAT_14521ff80).
//! * Set open / closed (FUN_1404c4be0, see [`Door::set_open`]): the 0.4 s timers are FUN_1404c2ed0 with DAT_1451804b4 = 0.4; the Zone auto-close
//!   (LAB_1404aa030 after DAT_1451804b8 = 3.0 s) never runs in Athena (+0x8A0 = 1).
//! * Swing (`ABuildingWall::Tick` @1404c88e0): `Yaw = FMath::FixedTurn(Yaw, DesiredYaw, OpenDeltaYaw / 0.4 * DeltaSeconds)` on the component's world
//!   rotation: a constant-rate turn (no curve) over 0.4 s, the shortest way round.
//! * Sounds (vfunc_421 @1404bd860 -> FUN_1404bee50): on every set (before the blocked test) `DoorOpeningSound` when the new state is open, else
//!   `DoorClosingSound`, `PlaySoundAtLocation` at the door component's world location, volume 1, pitch 1 (DAT_143fe36fc).
//! * Visibility: the door follows the wall's mesh (vfunc_292 / vfunc_337 / vfunc_338: SetVisibility on the DoorComponent with the main mesh, hidden while
//!   edited); a destroyed wall takes it with it (it is a component of the wall actor; the wall's health is the only health).
use glam::Vec3;
use serde_json::Value;

/// `DAT_1451804b4`: the swing time (s), also the delay of the collision timer FUN_1404c2ed0.
pub const SWING_TIME: f32 = 0.4;
/// `DAT_1451804b8`: the Zone (non-Athena) auto-close delay (s). Not used in BR (the Athena game state's +0x8A0 is true).
pub const ZONE_AUTO_CLOSE_DELAY: f32 = 3.0;
/// `DAT_14404e220`: the open delta yaw of the ctor (+0x1090), degrees.
pub const OPEN_DELTA_YAW: f32 = 90.0;
/// `DAT_143feb1ec` = -1: the in-place flip of the open delta.
pub const FLIP: f32 = -1.0;
/// `Fort.Building.Door.Defaults` texts.
pub const TEXT_OPEN: &str = "Open";
pub const TEXT_CLOSE: &str = "Close";

/// The door data of a wall class (CDO, instance values overlaid by the caller for placed actors).
#[derive(Clone, Debug, PartialEq)]
pub struct DoorDef {
    /// `DoorMesh` (object path).
    pub mesh: String,
    /// `DoorOffset` (root-relative, UE cm).
    pub offset: Vec3,
    /// `DoorAnimatingMaterial` (slot 0 of the door component; `None` = the mesh's own materials).
    pub animating_material: Option<String>,
    pub opening_sound: Option<String>,
    pub closing_sound: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> { v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(|x| x.to_owned()) }

impl DoorDef {
    /// From a wall's (merged) properties: `None` when `DoorMesh` is null (no door component is created, FUN_1404abb40).
    pub fn from_props(props: &Value) -> Option<DoorDef> {
        let mesh = s(props, "DoorMesh")?;
        let offset = props.get("DoorOffset").and_then(|v| v.as_array()).filter(|a| a.len() >= 3)
            .map(|a| Vec3::new(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32)).unwrap_or(Vec3::ZERO);
        Some(DoorDef { mesh, offset, animating_material: s(props, "DoorAnimatingMaterial"), opening_sound: s(props, "DoorOpeningSound"), closing_sound: s(props, "DoorClosingSound") })
    }
}

/// `FRotator::ClampAxis`: [0, 360).
pub fn clamp_axis(a: f32) -> f32 { let a = a % 360.0; if a < 0.0 { a + 360.0 } else { a } }
/// `FRotator::NormalizeAxis`: (-180, 180].
pub fn normalize_axis(a: f32) -> f32 { let a = clamp_axis(a); if a > 180.0 { a - 360.0 } else { a } }

/// UE4.16 `FMath::FixedTurn` (UnrealMath.cpp:3162), translated.
pub fn fixed_turn(current: f32, desired: f32, delta_rate: f32) -> f32 {
    if delta_rate == 0.0 { return clamp_axis(current); }
    if delta_rate >= 360.0 { return clamp_axis(desired); }
    let mut result = clamp_axis(current);
    let current = result;
    let desired = clamp_axis(desired);
    let rate = delta_rate.abs();
    if current > desired {
        if current - desired < 180.0 { result -= (current - desired).min(rate); } else { result += (desired + 360.0 - current).min(rate); }
    } else if desired - current < 180.0 { result += (desired - current).min(rate); } else { result -= (current + 360.0 - desired).min(rate); }
    clamp_axis(result)
}

/// What the host plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DoorEvent {
    /// FUN_1404bee50: `DoorOpeningSound` (`opening`) / `DoorClosingSound` at the door component.
    Sound { opening: bool },
}

/// One door's state (the door fields of one `ABuildingWall`). Yaws are the door component's WORLD yaw (degrees, UE).
#[derive(Clone, Debug, PartialEq)]
pub struct Door {
    /// +0x1084: the closed world yaw (pitch / roll: the actor's, unchanged by the door).
    pub closed_yaw: f32,
    /// +0x1090: the open delta yaw, flipped in place by [`Door::set_open`].
    pub delta_yaw: f32,
    /// +0x1078 (`DoorDesiredRotation.Yaw`).
    pub desired_yaw: f32,
    /// The door component's current world yaw (turned by [`Door::tick`]).
    pub yaw: f32,
    /// `bDoorOpen`.
    pub open: bool,
    /// `bDoorCollisionDisabled` (the replicated flag).
    pub collision_disabled_flag: bool,
    /// The door component's `SetCollisionEnabled` state (QueryOnly = true, NoCollision = false): blocks the pawn / weapons and takes the interaction trace.
    pub collision: bool,
    /// `ABuildingActor::bMirrored` (+0x780 bit 1).
    pub mirrored: bool,
    /// Remaining seconds of each pending FUN_1404c2ed0 timer (each set starts its own: the handle is a local of FUN_1404c4be0).
    pub timers: Vec<f32>,
}

impl Door {
    /// A freshly registered door: closed, collision on, desired = closed (FUN_1404abb40).
    pub fn new(closed_yaw: f32, mirrored: bool) -> Door {
        let y = normalize_axis(closed_yaw);
        Door { closed_yaw: y, delta_yaw: OPEN_DELTA_YAW, desired_yaw: y, yaw: y, open: false, collision_disabled_flag: false, collision: true, mirrored, timers: vec![] }
    }

    /// FUN_1404adc10 (Athena): the interaction text.
    pub fn prompt(&self) -> &'static str { if self.open { TEXT_CLOSE } else { TEXT_OPEN } }

    /// Can the interaction trace hit the door (its component collision is on)?
    pub fn interactable(&self) -> bool { self.collision }

    /// FUN_1404c3a60 (Athena): the interacting pawn's use. `instigator_forward` = the pawn's actor forward (UE); `blocked_at(yaw)` = vfunc_364's pawn
    /// overlap test with the door component at that world yaw.
    pub fn interact(&mut self, instigator_forward: Vec3, blocked_at: &mut dyn FnMut(f32) -> bool) -> Vec<DoorEvent> {
        let open = !self.open;
        self.set_open(open, Some(instigator_forward), blocked_at)
    }

    /// FUN_1404c4be0 (Authority, Athena game state): `bDoorOpen = open`, the sound (vfunc_421, before anything else is decided), then
    /// * close: the component is turned to the closed rotation with collision on and vfunc_364 tests the pawns (`bCollisionBlockedByPawns`); free: desired =
    ///   closed and a 0.4 s timer re-enables the collision; blocked: `bDoorOpen` flips back to open and desired = closed + delta (1.8 arms no timer here and the collision stays off for good; the port arms the same 0.4 s timer so a pawn in the way never leaves the door unusable
    ///   afterwards). Either way the component's collision is then off for the swing.
    /// * open: collision off, `bDoorCollisionDisabled`, a 0.4 s timer; with an instigator the delta flips (x -1, kept) when the instigator's forward points
    ///   against `FRotator(desired + delta).Vector()` (FUN_1412f4470), so the door swings the way the pawn faces; then a mirrored wall flips it again;
    ///   desired = normalize(closed + delta).
    pub fn set_open(&mut self, open: bool, instigator_forward: Option<Vec3>, blocked_at: &mut dyn FnMut(f32) -> bool) -> Vec<DoorEvent> {
        self.open = open;
        let ev = vec![DoorEvent::Sound { opening: open }];
        if !open {
            let blocked = blocked_at(self.closed_yaw);
            if !blocked {
                self.timers.push(SWING_TIME);
                self.desired_yaw = self.closed_yaw;
            } else {
                self.desired_yaw = normalize_axis(self.closed_yaw + self.delta_yaw);
                self.open = true;
                self.timers.push(SWING_TIME);
            }
            self.collision = false;
        } else {
            self.collision = false;
            self.collision_disabled_flag = true;
            self.timers.push(SWING_TIME);
            if let Some(f) = instigator_forward {
                let a = (self.desired_yaw + self.delta_yaw).to_radians();
                let v = Vec3::new(a.cos(), a.sin(), 0.0);
                if f.dot(v) < 0.0 { self.delta_yaw *= FLIP; }
            }
            if self.mirrored { self.delta_yaw *= FLIP; }
            self.desired_yaw = normalize_axis(self.closed_yaw + self.delta_yaw);
        }
        ev
    }

    /// One frame: the due FUN_1404c2ed0 timers (collision back on, `bDoorCollisionDisabled` cleared, vfunc_364 at the current yaw: a blocked door is set open
    /// again without an instigator), then the swing (`ABuildingWall::Tick`).
    pub fn tick(&mut self, dt: f32, blocked_at: &mut dyn FnMut(f32) -> bool) -> Vec<DoorEvent> {
        let mut ev = vec![];
        let mut due = 0;
        for t in self.timers.iter_mut() { *t -= dt; if *t <= 0.0 { due += 1; } }
        self.timers.retain(|t| *t > 0.0);
        for _ in 0..due {
            self.collision = true;
            self.collision_disabled_flag = false;
            if blocked_at(self.yaw) { ev.extend(self.set_open(true, None, blocked_at)); }
        }
        self.yaw = fixed_turn(self.yaw, self.desired_yaw, self.delta_yaw / SWING_TIME * dt);
        ev
    }

    /// The door turned from its closed pose (degrees, the component's local swing about the pivot).
    pub fn swing_yaw(&self) -> f32 { normalize_axis(self.yaw - self.closed_yaw) }
    /// Is the door still turning towards its desired yaw?
    pub fn moving(&self) -> bool { normalize_axis(self.desired_yaw - self.yaw).abs() > 1e-3 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn free(_: f32) -> bool { false }
    fn run(d: &mut Door, secs: f32, blocked: &mut dyn FnMut(f32) -> bool) -> Vec<DoorEvent> { let mut ev = vec![]; let n = (secs * 60.0).round() as usize; for _ in 0..n { ev.extend(d.tick(1.0 / 60.0, blocked)); } ev }
    fn near(a: f32, b: f32) -> bool { normalize_axis(a - b).abs() < 1e-3 }

    #[test] fn fixed_turn_matches_ue416() {
        assert_eq!(fixed_turn(10.0, 50.0, 0.0), 10.0);
        assert_eq!(fixed_turn(10.0, 50.0, 400.0), 50.0);
        assert_eq!(fixed_turn(10.0, 50.0, 15.0), 25.0);
        assert_eq!(fixed_turn(10.0, 50.0, -15.0), 25.0, "the rate is taken absolute");
        assert_eq!(fixed_turn(350.0, 10.0, 15.0), 5.0, "the short way across 0");
        assert_eq!(fixed_turn(10.0, 350.0, 15.0), 355.0);
        assert_eq!(fixed_turn(-90.0, 0.0, 100.0), 0.0, "clamped to the desired yaw");
    }

    #[test] fn a_new_door_is_closed_with_collision_and_says_open() {
        let d = Door::new(90.0, false);
        assert!(!d.open && d.collision && d.interactable() && !d.moving());
        assert_eq!(d.prompt(), TEXT_OPEN);
        assert_eq!(d.swing_yaw(), 0.0);
    }

    #[test] fn open_swings_90_degrees_in_0_4_s_away_from_the_pawn_then_collides_again() {
        // closed yaw 0: FRotator(0, 0 + 90, 0).Vector() = +Y. A pawn facing +Y keeps the delta (+90).
        let mut d = Door::new(0.0, false);
        let ev = d.interact(Vec3::Y, &mut free);
        assert_eq!(ev, vec![DoorEvent::Sound { opening: true }]);
        assert!(d.open && !d.collision && d.collision_disabled_flag && !d.interactable());
        assert_eq!(d.prompt(), TEXT_CLOSE);
        assert!(near(d.desired_yaw, 90.0));
        run(&mut d, 0.2, &mut free);
        assert!((d.swing_yaw() - 45.0).abs() < 1.6, "half way after half the time: {}", d.swing_yaw());
        assert!(!d.collision, "collision stays off during the swing");
        run(&mut d, 0.25, &mut free);
        assert!(near(d.yaw, 90.0) && !d.moving());
        assert!(d.collision && !d.collision_disabled_flag && d.timers.is_empty(), "FUN_1404c2ed0 after 0.4 s");
    }

    #[test] fn a_pawn_facing_the_other_way_flips_the_delta_and_the_flip_is_kept() {
        let mut d = Door::new(0.0, false);
        d.interact(Vec3::NEG_Y, &mut free);
        assert_eq!(d.delta_yaw, -90.0);
        assert!(near(d.desired_yaw, -90.0));
        run(&mut d, 0.5, &mut free);
        assert!(near(d.yaw, -90.0));
        // close, then open from the +Y side: Vector(0 + -90) = -Y against +Y -> flips back to +90
        d.interact(Vec3::Y, &mut free);
        assert!(!d.open && near(d.desired_yaw, 0.0));
        run(&mut d, 0.5, &mut free);
        assert!(near(d.yaw, 0.0) && d.collision);
        d.interact(Vec3::Y, &mut free);
        assert_eq!(d.delta_yaw, 90.0);
        // and from -Y again: flips to -90
        run(&mut d, 0.5, &mut free); d.interact(Vec3::Y, &mut free); run(&mut d, 0.5, &mut free);
        d.interact(Vec3::NEG_Y, &mut free);
        assert_eq!(d.delta_yaw, -90.0);
    }

    #[test] fn a_mirrored_wall_flips_after_the_instigator_test_every_open() {
        let mut d = Door::new(0.0, true);
        d.interact(Vec3::Y, &mut free);
        assert_eq!(d.delta_yaw, -90.0, "facing +Y keeps +90, then bMirrored flips it");
        run(&mut d, 0.5, &mut free);
        d.interact(Vec3::Y, &mut free); run(&mut d, 0.5, &mut free);
        // reopen from +Y: Vector(0 + -90) = -Y against +Y -> +90, mirrored -> -90 again
        d.interact(Vec3::Y, &mut free);
        assert_eq!(d.delta_yaw, -90.0);
        assert!(near(d.desired_yaw, -90.0));
    }

    #[test] fn closing_plays_the_close_sound_and_turns_back_in_0_4_s() {
        let mut d = Door::new(180.0, false);
        d.interact(Vec3::NEG_Y, &mut free); // Vector(180 + 90 = 270) = -Y: kept
        run(&mut d, 0.5, &mut free);
        assert!(near(d.yaw, 270.0));
        let ev = d.interact(Vec3::X, &mut free);
        assert_eq!(ev, vec![DoorEvent::Sound { opening: false }]);
        assert!(!d.open && !d.collision && d.prompt() == TEXT_OPEN);
        run(&mut d, 0.45, &mut free);
        assert!(near(d.yaw, 180.0) && d.collision);
    }

    #[test] fn a_close_blocked_by_the_pawn_is_refused_after_its_sound_and_the_door_stays_usable() {
        let mut d = Door::new(0.0, false);
        d.interact(Vec3::Y, &mut free);
        run(&mut d, 0.5, &mut free);
        let mut tested = vec![];
        let ev = d.interact(Vec3::Y, &mut |y| { tested.push(y); true });
        assert_eq!(tested, vec![0.0], "vfunc_364 runs with the door at the closed rotation");
        assert_eq!(ev, vec![DoorEvent::Sound { opening: false }], "vfunc_421 played before the test");
        assert!(d.open && near(d.desired_yaw, 90.0), "stays open");
        assert!(!d.collision && !d.interactable() && d.timers.len() == 1, "the collision is off for the swing back");
        run(&mut d, 1.0, &mut free);
        assert!(near(d.yaw, 90.0) && d.collision && d.interactable() && d.open, "a pawn in the way never leaves the door unusable");
        let ev = d.interact(Vec3::Y, &mut free);
        assert_eq!(ev, vec![DoorEvent::Sound { opening: false }]);
        assert!(!d.open && near(d.desired_yaw, 0.0));
    }

    #[test] fn a_door_that_ends_its_close_on_the_pawn_bounces_open_without_an_instigator() {
        let mut d = Door::new(0.0, false);
        d.interact(Vec3::Y, &mut free);
        run(&mut d, 0.5, &mut free);
        d.interact(Vec3::Y, &mut free); // free at the closed pose when asked
        // the pawn steps in during the swing: the timer's vfunc_364 finds it at the current (closed) yaw
        let ev = run(&mut d, 0.5, &mut |y| normalize_axis(y).abs() < 10.0);
        assert_eq!(ev, vec![DoorEvent::Sound { opening: true }]);
        assert!(d.open && near(d.desired_yaw, 90.0) && !d.collision);
        // then the pawn has left: the new timer turns the collision on
        run(&mut d, 0.5, &mut free);
        assert!(d.collision && near(d.yaw, 90.0));
    }

    #[test] fn door_def_reads_the_class_defaults() {
        let v: Value = serde_json::json!({"DoorMesh": "/Game/Packages/PBW/Wood/L1/PBW_W1_Door.PBW_W1_Door", "DoorOffset": [-72.0, 0.0, 0.0],
            "DoorAnimatingMaterial": "/Game/Packages/PBW/Wood/Materials/Wood_lvl1_Door.Wood_lvl1_Door",
            "DoorOpeningSound": "/Game/Sounds/Fort_Doors/Constructed/Fort_Build_Door_Small_Cue.Fort_Build_Door_Small_Cue"});
        let d = DoorDef::from_props(&v).unwrap();
        assert_eq!(d.offset, Vec3::new(-72.0, 0.0, 0.0));
        assert!(d.closing_sound.is_none() && d.animating_material.is_some());
        assert!(DoorDef::from_props(&serde_json::json!({"StaticMesh": "x"})).is_none(), "no DoorMesh, no door component");
    }
}
