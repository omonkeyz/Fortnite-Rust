use crate::graph::{ease, Blend, BlendList, Ease, Node, Profile, Sm, Tr};

pub const PREFIX: &str = "Enh30/";
pub fn key(n: &str) -> String { format!("{PREFIX}{n}") }

pub const DEAD_BLEND_KEY: &str = "Enh30/__dead_blend_source";
pub const DEAD_HALF_LIFE: f32 = 1.0;
pub const DEAD_HALF_LIFE_MIN: f32 = 0.05;
pub const DEAD_HALF_LIFE_MAX: f32 = 1.0;
pub const DEAD_MAX_TRANSLATION_VELOCITY_CM: f32 = 500.;
pub const DEAD_MAX_ROTATION_VELOCITY_DEG: f32 = 360.;
static DEAD_REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DEAD_SOURCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn dead_request_id() -> u64 { DEAD_REQUESTS.load(std::sync::atomic::Ordering::Acquire) }
pub fn set_dead_source_ready(on: bool) { DEAD_SOURCE.store(on, std::sync::atomic::Ordering::Release); }
pub fn dead_source_ready() -> bool { DEAD_SOURCE.load(std::sync::atomic::Ordering::Acquire) }

static LENGTHS: std::sync::RwLock<Vec<(String, f32)>> = std::sync::RwLock::new(Vec::new());
static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_clip_lengths(v: Vec<(String, f32)>) { if let Ok(mut g) = LENGTHS.write() { *g = v; } READY.store(true, std::sync::atomic::Ordering::Release); }
pub fn clips_ready() -> bool { READY.load(std::sync::atomic::Ordering::Acquire) }
pub fn clip_length(k: &str) -> f32 { LENGTHS.read().ok().and_then(|g| g.iter().find(|(n, _)| n == k).map(|x| x.1)).unwrap_or(1.) }

pub const TAC_ENTER: Tr = Tr { dur: 0.4, ease: Ease::Hermite };
pub const TAC_EXIT: Tr = Tr { dur: 0.2, ease: Ease::Hermite };
pub const TAC_START_TO_LOOP: f32 = 0.25;
pub const TAC_LAND_TO_LOOP: f32 = 0.2;
pub const TAC_JUMP_START_TO_FALL: f32 = 0.7;
pub const TAC_FALL_TO_JUMP: f32 = 0.1;
pub const TAC_DROP: f32 = 0.2;
pub const TAC_FALL_TO_ENTER: f32 = 0.2;
pub const TAC_START_ENTER: f32 = 0.1;
pub const TAC_LAND_ENTER: f32 = 0.4;
pub const TAC_JUMP_ENTER: f32 = 0.1;
pub const TAC_FALL_TO_LOOP: f32 = 0.2;
pub const TAC_HURDLE_FALL: f32 = 1.0;
pub const TAC_REJUMP_FROM_DROP: f32 = 0.4;
pub const JUMP_LEAN_ALPHA_BLEND: Tr = Tr { dur: 0.1, ease: Ease::Hermite };
pub const JUMP_LEAN_INTERP_TIME: f32 = 0.8;
pub const JUMP_LEAN_DAMPING: f32 = 0.3;
pub const SPRINT_LEAN_SCALE: f32 = 0.05;
pub const SPRINT_LEAN_MAX: f32 = 60.;
pub const SPRINT_LEAN_INTERP: f32 = 15.;
pub const SPRINT_LEAN_DECAY: f32 = 8.;
pub const ANGULAR_VELOCITY_YAW_RATE: f32 = 20.;
pub const SLIDE_ADDITIVE_IN: Tr = Tr { dur: 0.3, ease: Ease::Hermite };
pub const SLIDE_ADDITIVE_OUT: Tr = Tr { dur: 0.2, ease: Ease::Hermite };

pub const LEANS_BS: [(&str, f32, f32); 6] = [("TacSprint_Lean_Pose_CC", 0., 375.), ("TacSprint_Lean_Pose_LC", -45., 375.), ("TacSprint_Lean_Pose_RC", 45., 375.), ("TacSprint_Lean_Pose_CC", 0., 125.), ("TacSprint_LeanLowSpeed_Pose_LC", -45., 125.), ("TacSprint_LeanLowSpeed_Pose_RC", 45., 125.)];
pub const JUMP_LEANS_BS: [(&str, f32, f32); 6] = [("TacSprint_Jump_Lean_Pose_CC", 0., 500.), ("TacSprint_Jump_Lean_Pose_LC", -45., 500.), ("TacSprint_Jump_Lean_Pose_RC", 45., 500.), ("TacSprint_Jump_Lean_Pose_CC", 0., 0.), ("TacSprint_Jump_Lean_Pose_CC", -45., 0.), ("TacSprint_Jump_Lean_Pose_CC", 45., 0.)];
pub const LOOKAT_BS: [(&str, f32, f32); 3] = [("Sprint_LookAt_CC_CMM", 0., 0.), ("Sprint_LookAt_CL_CMM", 45., 0.), ("Sprint_LookAt_CR_CMM", -45., 0.)];
pub const SLIDE_CENTER_ADDITIVE: &str = "Slide_Center_CMM_Additive";
pub const SPINE_TWEAK_SEQUENCE: &str = "BaseballSlideInitiate_SpineTweak";
pub const SPINE_TWEAK_WINDOW_SCALE: f32 = 1.25;
pub const SPINE_TWEAK_ALPHA_RANGE: f32 = 90.;
pub const SPINE_TWEAK_LEAN_DEADZONE: f32 = 0.01;
pub const SPINE_TWEAK_BS: [(&str, f32, f32); 5] = [("SlidingSpineTweakPose_Base", -90., 0.), ("SlidingSpineTweakPose", 0., 0.), ("SlidingSpineTweakPose_Base", 90., 0.), ("SlidingSpineTweakPose_Base", 180., 0.), ("SlidingSpineTweakPose_Base", -180., 0.)];
pub const SPINE_TWEAK_LEAN_BS: [(&str, f32, f32); 3] = [("SlidingSpineTweakPose", 1., 0.), ("SlidingSpineTweakPose_LeanL", -1., 0.), ("SlidingSpineTweakPose_Base", 0., 0.)];
pub fn slide_pose_bs(weapon: &str) -> [(String, f32, f32); 9] {
    let w = match weapon { "Pistol" => "_Pistol", "AssaultRifle" | "Rifle" | "Shotgun" | "TacticalShotgun" | "GrenadeLauncher" | "RocketLauncher" | "ShoulderLauncher" | "GoingCommando" => "", _ => "_NoWep" };
    let p = |d: &str| format!("Slide_{d}{w}_CMM_Pose");
    [(p("S"), 0., 45.), (p("Center"), 0., 0.), (p("NW"), 45., -45.), (p("SW"), 45., 45.), (p("W"), 45., 0.), (p("E"), -45., 0.), (p("N"), 0., -45.), (p("NE"), -45., -45.), (p("SE"), -45., 45.)]
}

pub fn fullbody_tactical(tactical_sprinting: bool, sprinting: bool, sliding: bool, accelerating: bool, on_ground: bool, jumping: bool, falling: bool) -> bool {
    tactical_sprinting && ((sprinting && !sliding && accelerating && on_ground) || jumping || falling)
}

pub fn grid_weights<S: AsRef<str>>(samples: &[(S, f32, f32)], x: f32, y: f32) -> Vec<(String, f32)> {
    let mut xs: Vec<f32> = samples.iter().map(|s| s.1).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup();
    let mut ys: Vec<f32> = samples.iter().map(|s| s.2).collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ys.dedup();
    let seg = |v: &[f32], q: f32| -> (usize, usize, f32) {
        if v.len() < 2 { return (0, 0, 0.); }
        let q = q.clamp(v[0], v[v.len() - 1]);
        let i = (0..v.len() - 1).find(|&i| q <= v[i + 1]).unwrap_or(v.len() - 2);
        (i, i + 1, (q - v[i]) / (v[i + 1] - v[i]).max(1e-6))
    };
    let (x0, x1, tx) = seg(&xs, x);
    let (y0, y1, ty) = seg(&ys, y);
    let mut out: Vec<(String, f32)> = vec![];
    for (xi, wx) in [(x0, 1. - tx), (x1, tx)] {
        for (yi, wy) in [(y0, 1. - ty), (y1, ty)] {
            let w = wx * wy;
            if w <= 1e-5 { continue; }
            if let Some(s) = samples.iter().find(|s| s.1 == xs[xi] && s.2 == ys[yi]) {
                let n = s.0.as_ref();
                if let Some(e) = out.iter_mut().find(|e| e.0 == n) { e.1 += w; } else { out.push((n.to_owned(), w)); }
            }
        }
    }
    out
}
pub const TAC_AIR_BLEND: f32 = 0.1;
pub const SLIDE_XFADE: f32 = 0.2;
pub const SLIDE_LAYER_BLEND: Tr = Tr { dur: 0.2, ease: Ease::Hermite };
pub const KNEE_SLIDE_VELOCITY_ANGLE: f32 = 100.;
pub const KNEE_SLIDE_SLOPE_ANGLE: f32 = -10.;

pub const TAC_CLIPS: [&str; 7] = ["JogToTacSprint", "JogToTacSprint_CMF", "Tactical_Sprint", "Tactical_Sprint_CMF", "Fall_to_TacticalSprint", "TacJump_start", "Tac_JumpFall"];
pub const SLIDE_CLIPS: [&str; 8] = ["BaseballSlideInitiate", "KneeSlideInitiate", "BaseballSliding", "KneeSliding", "BaseballToKneeSlidingTransitionC", "BaseballSlidingSlopeAngleC", "KneeSlidingSlopeAngleC", "BaseballToKneeSlidingTransition45N"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HurdleUp { Up50, Up100, Up200 }

#[derive(Clone, Copy, Debug)]
pub struct MontageSeg { pub clip: &'static str, pub start: f32, pub anim_start: f32, pub anim_end: f32, pub play_rate: f32 }

#[derive(Clone, Copy, Debug)]
pub struct MontageDef { pub segs: &'static [MontageSeg], pub rate: f32, pub blend_in: Tr, pub blend_out: Tr }

#[derive(Clone, Copy, Debug)]
pub struct HurdleMontageAnim { pub name: &'static str, pub def: MontageDef, pub start_end: f32, pub near: (f32, f32), pub far: (f32, f32), pub land_near: (HurdleUp, bool), pub land_far: (HurdleUp, bool) }
pub const HURDLE_MONTAGES: [HurdleMontageAnim; 10] = [
    HurdleMontageAnim { name: "Hurdle_60up_M", def: MontageDef { rate: 1.0, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.2, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_60Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.2 }, MontageSeg { clip: "Hurdle_Over_100Fwd_60Up", start: 0.13888888, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_60Up", start: 0.7388889, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.13888885, near: (0.13888885, 0.7388976), far: (0.7388976, 1.3388889), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_100up_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_100Up", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_100Up", start: 0.7666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666667000000001), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_100Up_Mirrored_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.2, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up_Mirrored", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_100Up_Mirrored", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_100Up_Mirrored", start: 0.7666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.16666667, near: (0.16666667, 0.7666667), far: (0.7666667, 1.3666667000000001), land_near: (HurdleUp::Up100, true), land_far: (HurdleUp::Up100, true) },
    HurdleMontageAnim { name: "Hurdle_100up_Proto_V2_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.1 }, MontageSeg { clip: "Hurdle_Over_100Fwd_100Up_v2", start: 0.15151516, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_100Up", start: 0.75151515, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.15321009, near: (0.15321009, 0.7516597), far: (0.7516597, 1.35151515), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_100up_Proto_V2_Mirrored_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_100Up_v2_Mirrored", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_100Up", start: 0.7666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666667000000001), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_200up_Proto_M", def: MontageDef { rate: 1.0, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.2, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_200Up", start: 0.0, anim_start: 0.0, anim_end: 0.36666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_200Up", start: 0.36666667, anim_start: 0.0, anim_end: 0.7, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_200Up", start: 1.0666666, anim_start: 0.0, anim_end: 0.76666665, play_rate: 1.0 }] }, start_end: 0.36666667, near: (0.36666667, 1.066667), far: (1.066667, 1.83333325), land_near: (HurdleUp::Up200, false), land_far: (HurdleUp::Up200, false) },
    HurdleMontageAnim { name: "Hurdle_100up_Narrow_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_Kong_100Fwd_100Up", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 0.85 }] }, start_end: 0.16666667, near: (0.16666667, 0.8725490229411765), far: (0.16666667, 0.8725490229411765), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_100up_Narrow_V2_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_Kong_100Fwd_100Up_v2", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 0.85 }] }, start_end: 0.16666667, near: (0.16666667, 0.8725490229411765), far: (0.16666667, 0.8725490229411765), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_Crouch_100up_M", def: MontageDef { rate: 1.15, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.1, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_Crouch_100Fwd_100Up", start: 0.0, anim_start: 0.0, anim_end: 0.16666667, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_100Up", start: 0.16666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_100Up_Targeted", start: 0.7666667, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_Kong_100Fwd_100Up", start: 1.3666668, anim_start: 0.0, anim_end: 0.6, play_rate: 1.0 }] }, start_end: 0.16666667, near: (0.16666667, 0.7666641), far: (0.7666641, 1.3666668), land_near: (HurdleUp::Up100, false), land_far: (HurdleUp::Up100, false) },
    HurdleMontageAnim { name: "Hurdle_Crouch_200up_Proto_M", def: MontageDef { rate: 1.0, blend_in: Tr { dur: 0.1, ease: Ease::Linear }, blend_out: Tr { dur: 0.2, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Hurdle_Start_Crouch_200Up", start: 0.0, anim_start: 0.0, anim_end: 0.3, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_100Fwd_200Up", start: 0.3, anim_start: 0.0, anim_end: 0.7, play_rate: 1.0 }, MontageSeg { clip: "Hurdle_Over_250Fwd_200Up", start: 1.0, anim_start: 0.0, anim_end: 0.76666665, play_rate: 1.0 }] }, start_end: 0.3, near: (0.3, 1.0), far: (1.0, 1.76666665), land_near: (HurdleUp::Up200, false), land_far: (HurdleUp::Up200, false) },
];

pub const CLAMBER_50: MontageDef = MontageDef { rate: 1., blend_in: Tr { dur: 0.05, ease: Ease::Hermite }, blend_out: Tr { dur: 0.1, ease: Ease::Hermite }, segs: &[MontageSeg { clip: "Clamber_Wall_50up", start: 0., anim_start: 0., anim_end: 0.2, play_rate: 1. }] };
pub const CLAMBER_100: MontageDef = MontageDef { rate: CLAMBER_PLAY_RATE, blend_in: Tr { dur: 0.2, ease: Ease::Hermite }, blend_out: Tr { dur: 0.25, ease: Ease::Hermite }, segs: &[MontageSeg { clip: "Clamber_Wall_100up", start: 0., anim_start: 0.1, anim_end: 0.63, play_rate: 1. }] };
pub const CLAMBER_150: MontageDef = MontageDef { rate: CLAMBER_PLAY_RATE, blend_in: Tr { dur: 0.2, ease: Ease::Hermite }, blend_out: Tr { dur: 0.25, ease: Ease::Hermite }, segs: &[MontageSeg { clip: "Clamber_Wall_150up", start: 0., anim_start: 0.1, anim_end: 0.63, play_rate: 1. }] };
pub const CLAMBER_250: MontageDef = MontageDef { rate: CLAMBER_PLAY_RATE, blend_in: Tr { dur: 0.2, ease: Ease::Linear }, blend_out: Tr { dur: 0.25, ease: Ease::Linear }, segs: &[MontageSeg { clip: "Clamber_Wall_250up", start: 0., anim_start: 0., anim_end: 0.645, play_rate: 1. }] };

pub const CLAMBER_PLAY_RATE: f32 = 0.645 / (0.6 + 0.1);
pub const CLAMBER_TARGET_Z_250UP: f32 = 175.;
pub const CLAMBER_TARGET_Z_150UP: f32 = 100.;
pub fn clamber_def(target_distance_z: f32) -> &'static MontageDef { if target_distance_z >= CLAMBER_TARGET_Z_250UP { &CLAMBER_250 } else if target_distance_z >= CLAMBER_TARGET_Z_150UP { &CLAMBER_150 } else { &CLAMBER_100 } }

pub fn land_clip(up: HurdleUp, far: bool, female: bool, mirrored: bool) -> String {
    let fwd = if far { "250Fwd" } else { "100Fwd" };
    let h = match up { HurdleUp::Up50 => "50Up", HurdleUp::Up100 => "100Up", HurdleUp::Up200 => "200Up" };
    let m = if mirrored && up == HurdleUp::Up100 { "_Mirrored" } else { "" };
    format!("Hurdle_LandToSprint_{fwd}_{h}{}{m}", if female { "_CMF" } else { "" })
}

pub fn all_clips() -> Vec<String> {
    let mut v: Vec<String> = TAC_CLIPS.iter().chain(SLIDE_CLIPS.iter()).map(|s| s.to_string()).collect();
    for s in LEANS_BS.iter().chain(JUMP_LEANS_BS.iter()).chain(LOOKAT_BS.iter()) { v.push(s.0.to_owned()); }
    for w in ["AssaultRifle", "Pistol", "Melee"] { for s in slide_pose_bs(w) { v.push(s.0); } }
    v.push(SLIDE_CENTER_ADDITIVE.to_owned());
    v.push(SPINE_TWEAK_SEQUENCE.to_owned());
    for s in SPINE_TWEAK_BS.iter().chain(SPINE_TWEAK_LEAN_BS.iter()) { v.push(s.0.to_owned()); }
    for d in HURDLE_MONTAGES.iter().map(|m| &m.def).chain([&CLAMBER_50, &CLAMBER_100, &CLAMBER_150, &CLAMBER_250]) { for s in d.segs { v.push(s.clip.to_owned()); } }
    for up in [HurdleUp::Up50, HurdleUp::Up100, HurdleUp::Up200] { for far in [false, true] { for f in [false, true] { for m in [false, true] { v.push(land_clip(up, far, f, m)); } } } }
    v.sort(); v.dedup(); v
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MontageReq { Hurdle { montage: u8, far: bool }, Clamber { target_distance_z: f32 } }

#[derive(Clone, Copy, Debug, Default)]
pub struct EnhancedInput {
    pub enabled: bool,
    pub female: bool,
    pub tactical: bool,
    pub in_air: bool,
    pub jumped: bool,
    pub sliding: bool,
    pub slide_slope_angle: f32,
    pub slide_velocity_angle: f32,
    pub slide_velocity_yaw: f32,
    pub sprint_yaw: f32,
    pub montage_start: Option<MontageReq>,
    pub jump_now: bool,
    pub yaw_rate: f32,
    pub yaw_delta: f32,
    pub speed2d: f32,
    pub lean_alpha: f32,
    pub weapon: &'static str,
    pub slope_pitch: f32,
    pub slope_roll: f32,
    pub moving: bool,
    pub lateral_accel: f32,
}

#[derive(Clone, Debug)]
struct MontagePlay { def: &'static MontageDef, name: &'static str, pos: f32, end: f32, jump: Option<(f32, f32)>, w: Blend, out: bool, land: Option<(HurdleUp, bool, bool)>, fresh: bool, ranges: [(f32, f32); 2], nranges: usize }

#[derive(Clone, Debug, PartialEq)]
pub struct NotifyTrack { pub slot: u8, pub name: String, pub time: f32, pub weight: f32, pub looping: bool }

pub fn clamber_montage_name(target_distance_z: f32) -> &'static str { if target_distance_z >= CLAMBER_TARGET_Z_250UP { "Clamber_Wall_250up_M" } else if target_distance_z >= CLAMBER_TARGET_Z_150UP { "Clamber_Wall_150up_M" } else { "Clamber_Wall_100up_M" } }

pub mod tac { pub const START: usize = 0; pub const LOOP: usize = 1; pub const LAND: usize = 2; pub const FALL_TO: usize = 3; pub const COUNT: usize = 4; }
pub mod air { pub const JUMP_START: usize = 0; pub const FALL: usize = 1; pub const DROP: usize = 2; pub const COUNT: usize = 3; }
pub mod slide { pub const INITIATE: usize = 0; pub const BASEBALL: usize = 1; pub const KNEE: usize = 2; pub const TO_KNEE: usize = 3; pub const COUNT: usize = 4; }

#[derive(Clone, Debug)]
pub struct EnhancedAnim {
    pub input: EnhancedInput,
    pub seen: (u32, u32),
    pub seen_jumps: u32,
    pub steps: u64,
    tac_w: Blend,
    tac: Sm,
    land: Option<(HurdleUp, bool, bool)>,
    air: Sm,
    air_on: BlendList,
    slide_w: Blend,
    slide: Sm,
    slide_uphill: bool,
    knee_latch: bool,
    montage: Option<MontagePlay>,
    air_full: bool,
    lean: f32,
    jump_lean: [f32; 2],
    jump_lean_v: [f32; 2],
    jump_lean_w: Blend,
    add_t: f32,
    fba_w: Blend,
    fba_t: f32,
    dead: Option<(f32, Tr)>,
    yaw_speed: f32,
}

impl Default for EnhancedAnim { fn default() -> Self { Self::new() } }

fn auto_due(t: f32, len: f32, xfade: f32) -> bool { len - t <= xfade }

impl EnhancedAnim {
    pub fn new() -> Self {
        EnhancedAnim { input: EnhancedInput::default(), seen: (0, 0), seen_jumps: 0, steps: 0, tac_w: Blend::new(0.), tac: Sm::new(tac::COUNT, tac::START), land: None, air: Sm::new(air::COUNT, air::FALL), air_on: BlendList::new(2, 0, Ease::Linear),
            slide_w: Blend::new(0.), slide: Sm::new(slide::COUNT, slide::INITIATE), slide_uphill: false, knee_latch: false, montage: None,
            air_full: false, lean: 0., jump_lean: [0.; 2], jump_lean_v: [0.; 2], jump_lean_w: Blend::new(0.), add_t: 0., fba_w: Blend::new(0.), fba_t: 0., dead: None, yaw_speed: 0. }
    }
    pub fn state_key(&self) -> (bool, usize, bool, bool, usize, Option<&'static str>) { (self.tac_w.target() > 0.5, self.tac.cur, self.air_on.active() == 1, self.slide_w.target() > 0.5, self.slide.cur, self.montage.as_ref().map(|m| m.def.segs[0].clip)) }
    pub fn active(&self) -> bool { self.tac_w.value > 1e-4 || self.slide_w.value > 1e-4 || self.montage.is_some() }
    pub fn tactical_weight(&self) -> f32 { self.tac_w.value }
    pub fn slide_weight(&self) -> f32 { self.slide_w.value }
    pub fn montage_weight(&self) -> f32 { self.montage.as_ref().map_or(0., |m| m.w.value) }
    pub fn describe(&self) -> String {
        format!("in slope {:.1} vel {:.1} | tac w {:.2} st {} t {:.2} air {:.2} | slide w {:.2} st {} t {:.2} | montage {}", self.input.slide_slope_angle, self.input.slide_velocity_angle, self.tac_w.value, self.tac.cur, self.tac.t[self.tac.cur], self.air_on.weights()[1], self.slide_w.value, self.slide.cur, self.slide.t[self.slide.cur],
            self.montage.as_ref().map_or("-".to_owned(), |m| format!("{} pos {:.2} w {:.2}", m.def.segs[0].clip, m.pos, m.w.value)))
    }

    fn request_dead(&mut self, tr: Tr) {
        if tr.dur <= 1e-6 { return; }
        let keep = self.dead.filter(|(t, d)| *t <= 1e-6 && d.dur <= tr.dur);
        if keep.is_some() { return; }
        DEAD_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.dead = Some((0., tr));
    }

    pub fn dead_blend(&self, base: Node) -> Node {
        let Some((t, tr)) = self.dead else { return base };
        if !dead_source_ready() { return base; }
        let a = ease(tr.ease, t / tr.dur.max(1e-6));
        if a >= 0.9999 { return base; }
        Node::Mix(vec![(Node::clip(DEAD_BLEND_KEY, t, false), 1. - a), (base, a)])
    }

    pub fn step(&mut self, i: &EnhancedInput, dt: f32, lens: &dyn Fn(&str) -> f32) {
        if !i.enabled { let (seen, sj, st) = (self.seen, self.seen_jumps, self.steps); *self = Self::new(); self.seen = seen; self.seen_jumps = sj; self.steps = st; return; }
        self.steps = self.steps.wrapping_add(1);
        self.input = *i;
        let dt = dt.max(0.);
        if let Some((t, tr)) = self.dead.as_mut() { *t += dt; if *t >= tr.dur { self.dead = None; } }
        if let Some(req) = i.montage_start { self.start_montage(req); }
        let landing = self.step_montage(dt);
        let montage_full = self.montage.as_ref().is_some_and(|m| !m.out && m.w.value >= 0.999);
        let tac_target = if i.tactical { 1. } else { 0. };
        let entering = tac_target > 0. && self.tac_w.value <= 1e-4;
        if tac_target > 0. && (entering || montage_full || landing) { self.reinit_tac(); }
        if landing && tac_target > 0. { self.request_dead(Tr { dur: TAC_LAND_ENTER, ease: Ease::Hermite }); }
        self.tac_w.go(tac_target, if tac_target > 0. { TAC_ENTER } else { TAC_EXIT });
        self.tac_w.step(dt);
        self.step_tac(i, dt, lens);
        if i.sliding && self.slide_w.target() < 0.5 { self.reinit_slide(i); }
        if !i.sliding { self.knee_latch = false; }
        self.slide_w.go(if i.sliding { 1. } else { 0. }, SLIDE_LAYER_BLEND);
        self.slide_w.step(dt);
        self.step_slide(i, dt, lens);
        if i.sliding && self.fba_w.target() < 0.5 { self.fba_t = 0.; self.fba_w = Blend::new(1.); self.request_dead(SLIDE_ADDITIVE_IN); }
        self.fba_w.go(if i.sliding { 1. } else { 0. }, if i.sliding { SLIDE_ADDITIVE_IN } else { SLIDE_ADDITIVE_OUT });
        self.fba_w.step(dt);
        self.fba_t += dt;
        self.add_t += dt;
        self.yaw_speed = if i.moving { self.yaw_speed + (ANGULAR_VELOCITY_YAW_RATE * dt).clamp(0., 1.) * (i.yaw_rate - self.yaw_speed) } else { 0. };
        let lean_target = (SPRINT_LEAN_SCALE * self.yaw_speed).clamp(-SPRINT_LEAN_MAX, SPRINT_LEAN_MAX);
        self.lean = if i.moving { self.lean + (lean_target - self.lean) * (SPRINT_LEAN_INTERP * dt).clamp(0., 1.) } else { crate::graph::finterp_to(self.lean, 0., dt, SPRINT_LEAN_DECAY) };
        let omega = std::f32::consts::TAU / JUMP_LEAN_INTERP_TIME;
        for (k, target) in [i.yaw_delta, i.speed2d].into_iter().enumerate() {
            let a = omega * omega * (target - self.jump_lean[k]) - 2. * JUMP_LEAN_DAMPING * omega * self.jump_lean_v[k];
            self.jump_lean_v[k] += a * dt;
            self.jump_lean[k] += self.jump_lean_v[k] * dt;
        }
        self.jump_lean_w.go(if i.in_air { 1. } else { 0. }, JUMP_LEAN_ALPHA_BLEND);
        self.jump_lean_w.step(dt);
    }

    fn start_montage(&mut self, req: MontageReq) {
        let (def, name, jump, land, end) = match req {
            MontageReq::Hurdle { montage, far } => {
                let m = &HURDLE_MONTAGES[(montage as usize).min(HURDLE_MONTAGES.len() - 1)];
                let (sec, l) = if far { (m.far, m.land_far) } else { (m.near, m.land_near) };
                (&m.def, m.name, Some((m.start_end, sec.0)), Some((l.0, far, l.1)), sec.1)
            }
            MontageReq::Clamber { target_distance_z } => { let d = clamber_def(target_distance_z); (d, clamber_montage_name(target_distance_z), None, None, d.segs.iter().map(|s| s.start + (s.anim_end - s.anim_start) / s.play_rate).fold(0., f32::max)) }
        };
        let w = Blend::new(1.);
        self.request_dead(def.blend_in);
        self.montage = Some(MontagePlay { def, name, pos: 0., end, jump, w, out: false, land, fresh: true, ranges: [(0., 0.); 2], nranges: 0 });
    }

    fn step_montage(&mut self, dt: f32) -> bool {
        let mut landing = false;
        let Some(m) = self.montage.as_mut() else { return false };
        let before = m.pos;
        m.pos += dt * m.def.rate;
        m.ranges = [(if m.fresh { -1e-6 } else { before }, m.pos), (0., 0.)];
        m.nranges = 1;
        m.fresh = false;
        if let Some((at, to)) = m.jump { if before < at && m.pos >= at { m.pos = to + (m.pos - at); m.jump = None; m.ranges[0].1 = at; m.ranges[1] = (to, m.pos); m.nranges = 2; } }
        let remaining = (m.end - m.pos) / m.def.rate.max(1e-6);
        let mut out_req = None;
        if !m.out && remaining <= m.def.blend_out.dur { m.out = true; m.w = Blend::new(0.); out_req = Some(m.def.blend_out); if m.land.is_some() { landing = true; } }
        m.w.step(dt);
        let land = m.land;
        if m.out && m.w.value <= 1e-4 { self.montage = None; }
        if let Some(tr) = out_req { self.request_dead(tr); }
        if landing { self.land = land; }
        landing
    }

    fn reinit_tac(&mut self) {
        let start = if self.land.is_some() { tac::LAND } else { tac::START };
        self.tac = Sm::new(tac::COUNT, start);
        self.air = Sm::new(air::COUNT, air::FALL);
        self.air_on = BlendList::new(2, 0, Ease::Linear);
    }

    fn tac_clip(&self, state: usize) -> String {
        let f = self.input.female;
        match state {
            tac::FALL_TO => "Fall_to_TacticalSprint".to_owned(),
            tac::START => if f { "JogToTacSprint_CMF" } else { "JogToTacSprint" }.to_owned(),
            tac::LAND => self.land.map_or_else(|| "Tactical_Sprint".to_owned(), |(u, far, m)| land_clip(u, far, f, m)),
            _ => if f { "Tactical_Sprint_CMF" } else { "Tactical_Sprint" }.to_owned(),
        }
    }

    fn step_tac(&mut self, i: &EnhancedInput, dt: f32, lens: &dyn Fn(&str) -> f32) {
        if self.tac_w.value <= 1e-4 && self.tac_w.target() <= 0. { self.land = None; return; }
        let cur = self.tac.cur;
        let t = self.tac.t[cur];
        if cur == tac::START && auto_due(t, lens(&key(&self.tac_clip(tac::START))), TAC_START_TO_LOOP) { self.tac.go(tac::LOOP, Tr { dur: 0., ease: Ease::Hermite }, Profile::None); self.request_dead(Tr { dur: TAC_START_TO_LOOP, ease: Ease::Hermite }); }
        if cur == tac::LAND && auto_due(t, lens(&key(&self.tac_clip(tac::LAND))), TAC_LAND_TO_LOOP) { self.tac.go(tac::LOOP, Tr { dur: TAC_LAND_TO_LOOP, ease: Ease::Hermite }, Profile::None); }
        if cur == tac::FALL_TO && auto_due(t, lens(&key("Fall_to_TacticalSprint")), TAC_FALL_TO_LOOP) { self.tac.go(tac::LOOP, Tr { dur: 0., ease: Ease::Hermite }, Profile::None); self.request_dead(Tr { dur: TAC_FALL_TO_LOOP, ease: Ease::Hermite }); }
        self.tac.tick(dt);
        if self.tac.cur == tac::LOOP && !self.tac.fading() { self.land = None; }
        let air_was = self.air_on.active() == 1;
        let mut req = None;
        if i.in_air && !air_was {
            let st = if self.land.is_some() { air::FALL } else if i.jumped || i.jump_now { air::JUMP_START } else { air::DROP };
            self.air = Sm::new(air::COUNT, st);
            if st == air::JUMP_START { req = Some(TAC_JUMP_ENTER); }
            self.air_full = false;
        }
        if i.in_air && i.jump_now && air_was {
            if self.air.cur == air::FALL { self.air.go(air::JUMP_START, Tr { dur: 0., ease: Ease::Hermite }, Profile::None); req = Some(TAC_FALL_TO_JUMP); }
            else if self.air.cur == air::DROP { self.air.go(air::JUMP_START, Tr { dur: TAC_REJUMP_FROM_DROP, ease: Ease::Hermite }, Profile::None); }
        }
        if self.air.cur == air::JUMP_START && auto_due(self.air.t[air::JUMP_START], lens(&key("TacJump_start")), TAC_JUMP_START_TO_FALL) { self.air.go(air::FALL, Tr { dur: TAC_JUMP_START_TO_FALL, ease: Ease::Hermite }, Profile::None); }
        self.air.tick(dt);
        self.air_on.step(if i.in_air { 1 } else { 0 }, TAC_AIR_BLEND, dt);
        if i.in_air && self.air_on.weights()[1] >= 0.999 { self.air_full = true; }
        if !i.in_air && air_was && self.air_full {
            let hard = self.air.w[air::JUMP_START] + self.air.w[air::FALL] >= 0.5;
            let st = if self.land.is_some() { tac::LAND } else if hard { tac::FALL_TO } else { tac::START };
            self.tac = Sm::new(tac::COUNT, st);
            req = Some(match st { tac::LAND => TAC_LAND_ENTER, tac::FALL_TO => TAC_FALL_TO_ENTER, _ => TAC_START_ENTER });
            self.air_full = false;
        }
        if let Some(d) = req { self.request_dead(Tr { dur: d, ease: Ease::Hermite }); }
    }

    fn reinit_slide(&mut self, i: &EnhancedInput) {
        self.slide_uphill = i.slide_slope_angle < KNEE_SLIDE_SLOPE_ANGLE;
        self.knee_latch = false;
        self.slide = Sm::new(slide::COUNT, slide::INITIATE);
    }

    fn step_slide(&mut self, i: &EnhancedInput, dt: f32, lens: &dyn Fn(&str) -> f32) {
        if self.slide_w.value <= 1e-4 && self.slide_w.target() <= 0. { return; }
        if i.sliding && (i.slide_velocity_angle > KNEE_SLIDE_VELOCITY_ANGLE || i.slide_slope_angle < KNEE_SLIDE_SLOPE_ANGLE || self.slide_uphill) { self.knee_latch = true; }
        let x = Tr { dur: SLIDE_XFADE, ease: Ease::Hermite };
        let cur = self.slide.cur;
        let t = self.slide.t[cur];
        match cur {
            slide::INITIATE => {
                let init = if self.slide_uphill { "KneeSlideInitiate" } else { "BaseballSlideInitiate" };
                if auto_due(t, lens(&key(init)), SLIDE_XFADE) { self.slide.go(if self.slide_uphill { slide::KNEE } else { slide::BASEBALL }, x, Profile::None); }
            }
            slide::BASEBALL => if self.knee_latch { self.slide.go(slide::TO_KNEE, x, Profile::None); },
            slide::TO_KNEE => if auto_due(t, lens(&key("BaseballToKneeSlidingTransitionC")), SLIDE_XFADE) { self.slide.go(slide::KNEE, x, Profile::None); },
            _ => {}
        }
        self.slide.tick(dt);
    }

    fn clip(n: &str, t: f32, looping: bool) -> Node { Node::clip(&key(n), t, looping) }

    fn tac_node(&self) -> Node {
        let ground = mix((0..tac::COUNT).filter(|s| self.tac.w[*s] > 1e-4).map(|s| (Self::clip(&self.tac_clip(s), self.tac.t[s], s == tac::LOOP), self.tac.w[s])).collect());
        let ground = Node::RotateRoot { base: Box::new(ground), yaw: self.input.sprint_yaw, pitch: 0. };
        let ground = Node::ApplyAdditive { base: Box::new(ground), add: Box::new(Self::bs(&LOOKAT_BS, self.input.sprint_yaw, 0., self.add_t)), alpha: 1. };
        let ground = if self.input.lean_alpha <= 1e-4 { ground } else { Node::ApplyAdditive { base: Box::new(ground), add: Box::new(Self::bs(&LEANS_BS, self.lean, self.input.speed2d, 0.)), alpha: self.input.lean_alpha } };
        let aw = self.air_on.weights()[1];
        let pose = if aw <= 1e-4 { ground } else {
            let air = mix((0..air::COUNT).filter(|s| self.air.w[*s] > 1e-4).map(|s| (Self::clip(match s { air::JUMP_START => "TacJump_start", air::FALL => "Tac_JumpFall", _ => self.sprint_loop() }, self.air.t[s], s != air::JUMP_START), self.air.w[s])).collect());
            let jw = self.jump_lean_w.value;
            let air = if jw <= 1e-4 { air } else { Node::ApplyAdditive { base: Box::new(air), add: Box::new(Self::bs(&JUMP_LEANS_BS, self.jump_lean[0], self.jump_lean[1], 0.)), alpha: jw } };
            if aw >= 0.9999 { air } else { Node::Mix(vec![(ground, 1. - aw), (air, aw)]) }
        };
        pose
    }

    pub fn montage_notify_ranges(&self) -> Option<(&'static str, Vec<(f32, f32)>, f32)> {
        let m = self.montage.as_ref()?;
        Some((m.name, m.ranges[..m.nranges].to_vec(), m.w.value))
    }

    pub fn notify_tracks(&self, out: &mut Vec<NotifyTrack>) {
        out.clear();
        let free = 1. - self.montage_weight();
        let tw = self.tac_w.value * free;
        if tw > 1e-6 {
            let aw = self.air_on.weights()[1];
            for s in 0..tac::COUNT {
                let w = tw * (1. - aw) * self.tac.w[s];
                if w > 1e-6 { out.push(NotifyTrack { slot: s as u8, name: self.tac_clip(s), time: self.tac.t[s], weight: w, looping: s == tac::LOOP }); }
            }
            for s in 0..air::COUNT {
                let w = tw * aw * self.air.w[s];
                if w > 1e-6 { out.push(NotifyTrack { slot: (tac::COUNT + s) as u8, name: match s { air::JUMP_START => "TacJump_start", air::FALL => "Tac_JumpFall", _ => self.sprint_loop() }.to_owned(), time: self.air.t[s], weight: w, looping: s != air::JUMP_START }); }
            }
        }
        let sw = self.slide_w.value * free;
        if sw > 1e-6 {
            for s in 0..slide::COUNT {
                let w = sw * self.slide.w[s];
                if w <= 1e-6 { continue; }
                let (clip, looping) = match s {
                    slide::INITIATE => (if self.slide_uphill { "KneeSlideInitiate" } else { "BaseballSlideInitiate" }, false),
                    slide::BASEBALL => ("BaseballSliding", true),
                    slide::TO_KNEE => ("BaseballToKneeSlidingTransitionC", false),
                    _ => ("KneeSliding", true),
                };
                out.push(NotifyTrack { slot: (tac::COUNT + air::COUNT + s) as u8, name: clip.to_owned(), time: self.slide.t[s], weight: w, looping });
            }
        }
    }

    fn sprint_loop(&self) -> &'static str { if self.input.female { "Tactical_Sprint_CMF" } else { "Tactical_Sprint" } }

    fn bs<S: AsRef<str>>(samples: &[(S, f32, f32)], x: f32, y: f32, t: f32) -> Node { mix(grid_weights(samples, x, y).into_iter().map(|(n, w)| (Self::clip(&n, t, true), w)).collect()) }

    fn spine_tweak(&self, base: Node) -> Node {
        let w = self.slide_w.value;
        if w <= 1e-4 { return base; }
        let i = &self.input;
        let t = self.fba_t;
        let play = t < SPINE_TWEAK_WINDOW_SCALE * clip_length(&key(SPINE_TWEAK_SEQUENCE));
        let base = if play { Node::ApplyAdditive { base: Box::new(base), add: Box::new(Self::clip(SPINE_TWEAK_SEQUENCE, t, false)), alpha: w } } else { base };
        let base = if self.knee_latch { base } else { Node::ApplyMeshAdditive { base: Box::new(base), add: Box::new(Self::bs(&SPINE_TWEAK_BS, i.slide_velocity_yaw, i.slide_slope_angle.max(0.), 0.)), alpha: w } };
        let lean = if i.lateral_accel > SPINE_TWEAK_LEAN_DEADZONE { 1. } else if i.lateral_accel < -SPINE_TWEAK_LEAN_DEADZONE { -1. } else { 0. };
        let alpha = (1. - (i.slide_velocity_yaw.abs() / SPINE_TWEAK_ALPHA_RANGE).clamp(0., 1.)) * w;
        if alpha <= 1e-4 { return base; }
        Node::ApplyMeshAdditive { base: Box::new(base), add: Box::new(Self::bs(&SPINE_TWEAK_LEAN_BS, lean, i.slide_slope_angle.max(0.), 0.)), alpha }
    }

    fn slide_additive(&self, base: Node) -> Node {
        let base = self.spine_tweak(base);
        let w = self.fba_w.value;
        if w <= 1e-4 { return base; }
        let poses = Node::ApplyAdditive { base: Box::new(base), add: Box::new(Self::bs(&slide_pose_bs(self.input.weapon), self.input.slope_roll, self.input.slope_pitch, 0.)), alpha: w };
        Node::ApplyAdditive { base: Box::new(poses), add: Box::new(Self::clip(SLIDE_CENTER_ADDITIVE, self.fba_t, true)), alpha: w }
    }

    fn slide_state_node(&self, s: usize) -> Node {
        let t = self.slide.t[s];
        let (clip, add, looping) = match s {
            slide::INITIATE => (if self.slide_uphill { "KneeSlideInitiate" } else { "BaseballSlideInitiate" }, if self.slide_uphill { "KneeSlidingSlopeAngleC" } else { "BaseballSlidingSlopeAngleC" }, false),
            slide::BASEBALL => ("BaseballSliding", "BaseballSlidingSlopeAngleC", true),
            slide::TO_KNEE => ("BaseballToKneeSlidingTransitionC", "", false),
            _ => ("KneeSliding", "KneeSlidingSlopeAngleC", true),
        };
        let base = Self::clip(clip, t, looping);
        if add.is_empty() { base } else { Node::ApplyMeshAdditive { base: Box::new(base), add: Box::new(Self::clip(add, 0., true)), alpha: 1. } }
    }

    pub fn tactical_override(&self, base: Node) -> Node {
        let w = self.tac_w.value;
        let pose = if w <= 1e-4 { base } else if w >= 0.9999 { self.tac_node() } else { Node::Mix(vec![(base, 1. - w), (self.tac_node(), w)]) };
        self.slide_additive(pose)
    }

    pub fn lower_body_override(&self, base: Node) -> Node {
        let w = self.slide_w.value;
        if w <= 1e-4 { return base; }
        let pose = mix((0..slide::COUNT).filter(|s| self.slide.w[*s] > 1e-4).map(|s| (self.slide_state_node(s), self.slide.w[s])).collect());
        let pose = Node::RotateRoot { base: Box::new(pose), yaw: self.input.slide_velocity_yaw, pitch: 0. };
        if w >= 0.9999 { pose } else { Node::Mix(vec![(base, 1. - w), (pose, w)]) }
    }

    pub fn full_body_override(&self, base: Node) -> Node {
        let Some(m) = self.montage.as_ref() else { return base };
        let w = m.w.value;
        if w <= 1e-4 { return base; }
        let seg = m.def.segs.iter().rev().find(|s| m.pos >= s.start).unwrap_or(&m.def.segs[0]);
        let t = (seg.anim_start + (m.pos - seg.start) * seg.play_rate).clamp(seg.anim_start, seg.anim_end);
        let pose = Self::clip(seg.clip, t, false);
        if w >= 0.9999 { pose } else { Node::Mix(vec![(base, 1. - w), (pose, w)]) }
    }
}

fn mix(mut v: Vec<(Node, f32)>) -> Node { if v.len() == 1 { v.pop().unwrap().0 } else if v.is_empty() { Node::Ref } else { Node::Mix(v) } }

pub fn hermite(t: f32) -> f32 { ease(Ease::Hermite, t) }

#[cfg(test)]
mod tests {
    use super::*;
    fn lens(n: &str) -> f32 { match n.trim_start_matches(PREFIX) { "JogToTacSprint" => 1.2666667, "Tactical_Sprint" => 2.3333333, "BaseballSlideInitiate" => 1., "BaseballToKneeSlidingTransitionC" => 0.6666667, "TacJump_start" => 1.9666667, _ => 1. } }
    fn on() -> EnhancedInput { EnhancedInput { enabled: true, ..Default::default() } }

    #[test] fn disabled_leaves_the_tree_untouched() {
        let mut a = EnhancedAnim::new();
        a.step(&EnhancedInput { enabled: false, tactical: true, sliding: true, ..Default::default() }, 0.1, &lens);
        assert!(!a.active());
        assert!(matches!(a.tactical_override(Node::Ref), Node::Ref));
        assert!(matches!(a.lower_body_override(Node::Ref), Node::Ref));
        assert!(matches!(a.full_body_override(Node::Ref), Node::Ref));
    }

    #[test] fn tactical_enters_over_0_4s_starts_then_loops() {
        let mut a = EnhancedAnim::new();
        let i = EnhancedInput { tactical: true, ..on() };
        a.step(&i, 0.2, &lens);
        assert!((a.tactical_weight() - hermite(0.5)).abs() < 1e-4);
        for _ in 0..10 { a.step(&i, 0.1, &lens); }
        assert!((a.tactical_weight() - 1.).abs() < 1e-6);
        assert_eq!(a.tac.cur, tac::LOOP);
        a.step(&EnhancedInput { tactical: false, ..on() }, 0.1, &lens);
        assert!((a.tactical_weight() - (1. - hermite(0.5))).abs() < 1e-4);
    }

    #[test] fn slide_initiates_then_baseball_then_knee_after_turning() {
        let mut a = EnhancedAnim::new();
        let mut i = EnhancedInput { sliding: true, ..on() };
        for _ in 0..12 { a.step(&i, 0.1, &lens); }
        assert_eq!(a.slide.cur, slide::BASEBALL);
        i.slide_velocity_angle = 120.;
        for _ in 0..12 { a.step(&i, 0.1, &lens); }
        assert_eq!(a.slide.cur, slide::KNEE);
    }

    #[test] fn tac_jump_lands_into_fall_to_sprint_and_a_ledge_drop_restarts_the_jog_to_sprint() {
        let mut a = EnhancedAnim::new();
        let run = EnhancedInput { tactical: true, ..on() };
        for _ in 0..30 { a.step(&run, 0.1, &lens); }
        a.step(&EnhancedInput { in_air: true, jump_now: true, jumped: true, ..run }, 0.05, &lens);
        assert_eq!(a.air.cur, air::JUMP_START);
        for _ in 0..10 { a.step(&EnhancedInput { in_air: true, ..run }, 0.05, &lens); }
        a.step(&run, 0.05, &lens);
        assert_eq!(a.tac.cur, tac::FALL_TO);
        for _ in 0..30 { a.step(&run, 0.1, &lens); }
        assert_eq!(a.tac.cur, tac::LOOP);
        for _ in 0..10 { a.step(&EnhancedInput { in_air: true, ..run }, 0.05, &lens); }
        assert_eq!(a.air.cur, air::DROP);
        a.step(&run, 0.05, &lens);
        assert_eq!(a.tac.cur, tac::START);
    }

    #[test] fn inertial_transitions_switch_instantly_and_dead_blend_from_the_last_pose() {
        set_dead_source_ready(true);
        let mut a = EnhancedAnim::new();
        let run = EnhancedInput { tactical: true, ..on() };
        let mut seen = false;
        for _ in 0..12 { a.step(&run, 0.1, &lens); if a.tac.cur == tac::LOOP && a.dead.is_some() { seen = true; assert_eq!(a.tac.w[tac::LOOP], 1.); match a.dead_blend(Node::Ref) { Node::Mix(v) => assert!(matches!(&v[0].0, Node::Clip { name, .. } if name == DEAD_BLEND_KEY)), _ => panic!("no dead blend") } } }
        assert!(seen);
        for _ in 0..5 { a.step(&run, 0.1, &lens); }
        assert!(matches!(a.dead_blend(Node::Ref), Node::Ref));
    }

    #[test] fn fullbody_tactical_state_follows_the_native_condition() {
        assert!(fullbody_tactical(true, true, false, true, true, false, false));
        assert!(!fullbody_tactical(true, true, true, true, true, false, false));
        assert!(!fullbody_tactical(true, true, false, false, true, false, false));
        assert!(fullbody_tactical(true, false, false, false, false, false, true));
        assert!(!fullbody_tactical(false, true, false, true, true, true, true));
    }

    #[test] fn grid_weights_are_bilinear_and_sum_to_one() {
        let w = grid_weights(&LEANS_BS, 22.5, 250.);
        let sum: f32 = w.iter().map(|x| x.1).sum();
        assert!((sum - 1.).abs() < 1e-5);
        assert!(w.iter().any(|x| x.0 == "TacSprint_Lean_Pose_RC" && (x.1 - 0.25).abs() < 1e-5));
        let c = grid_weights(&slide_pose_bs("Pistol"), 0., 0.);
        assert_eq!(c, vec![("Slide_Center_Pistol_CMM_Pose".to_owned(), 1.)]);
    }

    #[test] fn hurdle_montage_jumps_to_the_far_section_and_lands_into_the_land_clip() {
        let mut a = EnhancedAnim::new();
        let mut i = EnhancedInput { tactical: true, montage_start: Some(MontageReq::Hurdle { montage: 1, far: true }), ..on() };
        a.step(&i, 0.01, &lens);
        i.montage_start = None;
        for _ in 0..30 { a.step(&i, 0.01, &lens); }
        assert!(a.montage.as_ref().unwrap().pos > 0.7666);
        for _ in 0..200 { a.step(&i, 0.01, &lens); }
        assert!(a.montage.is_none());
        assert!(a.tac.cur == tac::LAND || a.tac.cur == tac::LOOP);
    }
}
