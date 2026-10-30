//! `fn_animgraph::ClipSource` over `fn-anim`: [`FnClip`] samples a decoded UE 4.16 `AnimSequence` (or a `.fnanim`, which stores the same
//! decoded channels at integer frames) for the pose evaluator.
//!
//! Values are converted once, at load, into the evaluator's glTF space exactly like the old UEViewer md5anim export + `Clip::load` did:
//! position `(x, y, z) cm -> (x, z, y) * 0.01 m` after multiplying by the accumulated ancestor `Scale3D` of the retarget / reference pose
//! (`BAKE_BONE_SCALES`, which the glTF mesh export applies to the bone translations too), rotation `(x, y, z, w) -> (x, z, y, -w)`.
//! Animated scale tracks are ignored (as in the old runtime).
//!
//! Per bone the old `Clip::fit` rules run on the sampled value, with the bone's *target* bind transform (`bind`, the mesh skeleton's local
//! bind) and the clip's retarget metadata: `Skeleton` (1) -> bind translation (0 for additive clips; the retarget base pose's translation when
//! the sequence carries an explicit one), `AnimationScaled` (2) -> `anim * |bind| / |source|`, everything else (`Animation` 0,
//! **`AnimationRelative` 3 is left unimplemented as in UEViewer / the old runtime**) keeps the animated translation; `OrientAndScale` 4 rotates and scales it from the source pose onto the bind (`FindBetweenNormals`).
//! Additive clips: the cooked data already holds the delta (`GetBonePose_Additive`'s base is baked in by the cooker; `ADDITIVE_IDENTITY` is
//! the scale-0 pose), so a bone without a track is the identity delta. [`FnClip::additive_kind`] tells whether the graph must apply the
//! clip as local-space (`ApplyAdditive`) or mesh-space rotation offset (`ApplyMeshAdditive` / `AimOffset`); that choice is the graph's.
//!
//! Two sampling modes:
//! * [`SampleMode::Ue`] (default for the new runtime): UE key spacing `(NumFrames-1)/SequenceLength`, linear translation, nlerp rotation,
//!   looping wraps `t` modulo `SequenceLength`; a channel without keys falls back to the bind pose (identity delta when additive).
//! * [`SampleMode::LegacyMd5`]: emulation of the OLD (pre engine-exact-sampling) exports + loader, kept ONLY for explicit comparisons / documentation of the bug the old tree fixed (the old runtime now samples exactly, so the parity tests use `Ue`): frames sampled at
//!   `NumFrames/SequenceLength*RateScale` per second (the old runtime plays every clip `NumFrames/(NumFrames-1)` too fast), slerp,
//!   modulo `NumFrames` wrap, a bone with any track has both channels present (UEViewer's null key: zero translation / identity rotation).
use fn_anim::{AnimSequence, BoneTransform, FloatCurve, FnAnim, RefPoseType, SkeletonInfo, TranslationRetarget};
use fn_animgraph::{BoneXform, ClipLibrary, ClipSource};
use glam::{Quat, Vec3};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SampleMode {
    /// Exact UE sampling.
    #[default]
    Ue,
    /// The old md5anim runtime's sampling (parity tests against the old logs).
    LegacyMd5,
}

/// How a cooked additive clip is meant to be applied (`UAnimSequence::AdditiveAnimType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdditiveKind { None, LocalSpace, MeshSpaceRotationOffset }

#[derive(Clone, Debug)]
struct ClipBone {
    pos: Option<Vec<Vec3>>,
    rot: Option<Vec<Quat>>,
    /// Any channel (incl. scale) present: the old export's "bone has a track" (null keys fill missing channels).
    has_track: bool,
    mode: TranslationRetarget,
    /// Translation of the bone in the retarget (source) pose, glTF space, metres.
    source: Vec3,
}

/// One decoded animation sequence, converted to the evaluator's space, as a [`ClipSource`].
#[derive(Clone, Debug)]
pub struct FnClip {
    pub name: String,
    pub mode: SampleMode,
    index: HashMap<String, usize>,
    bones: Vec<ClipBone>,
    frames: usize,
    length: f32,
    rate_scale: f32,
    /// `NumFrames / SequenceLength * RateScale`: the frame rate the old md5anim carried.
    md5_rate: f32,
    additive: bool,
    additive_kind: AdditiveKind,
    ref_pose_type: RefPoseType,
    step: bool,
    retarget_base: bool,
    curves: Vec<FloatCurve>,
}

fn pos_to_gltf(p: [f32; 3], scale: Option<[f32; 3]>) -> Vec3 {
    let mut p = p;
    if let Some(s) = scale {
        if (s[0] - 1.0).abs() > 0.001 || (s[1] - 1.0).abs() > 0.001 || (s[2] - 1.0).abs() > 0.001 { p = [p[0] * s[0], p[1] * s[1], p[2] * s[2]]; }
    }
    Vec3::new(p[0], p[2], p[1]) * 0.01
}
/// UE `(x, y, z, w)` -> glTF-space quaternion, with the sign convention of the md5anim loader (`w` reconstructed positive from `xyz`).
fn rot_to_gltf(q: [f32; 4]) -> Quat {
    let s = if q[3] > 0.0 { 1.0 } else { -1.0 };
    Quat::from_xyzw(s * q[0], s * q[2], s * q[1], -s * q[3]).normalize()
}
fn key<T: Copy>(k: &[T], i: usize) -> T { k[i.min(k.len() - 1)] }
fn nlerp(a: Quat, b: Quat, t: f32) -> Quat {
    let bias = if a.dot(b) >= 0.0 { 1.0 } else { -1.0 };
    let r = b * t + a * (bias * (1.0 - t));
    let l = r.length();
    if l > 1e-8 { r / l } else { a }
}

impl FnClip {
    /// From a baked [`FnAnim`] (`.fnanim` content) for the target skeleton `skel` (bone parents / reference pose for the scale bake).
    /// The bone scale source is the sequence's named retarget pose when `skel` has it, else `skel.ref_pose` (a sequence's own
    /// `RetargetSourceAssetReferencePose` is not stored in `.fnanim`; use [`FnClip::from_sequence`] to honour it).
    pub fn from_fnanim(fa: &FnAnim, skel: &SkeletonInfo, mode: SampleMode) -> FnClip { Self::build(fa, skel, None, mode) }

    /// From a decoded sequence.
    pub fn from_sequence(seq: &AnimSequence, skel: &SkeletonInfo, mode: SampleMode) -> FnClip {
        let fa = FnAnim::from_sequence(seq, skel);
        let asset = if seq.retarget_source_asset_pose.is_empty() { None } else { Some(seq.retarget_source_asset_pose.as_slice()) };
        Self::build(&fa, skel, asset, mode)
    }

    fn build(fa: &FnAnim, skel: &SkeletonInfo, asset_pose: Option<&[BoneTransform]>, mode: SampleMode) -> FnClip {
        let n = skel.num_bones();
        let explicit: Option<&[BoneTransform]> = if fa.retarget_source == "None" { asset_pose } else { skel.find_retarget_source(&fa.retarget_source).map(|s| s.pose.as_slice()) };
        let raw: &[BoneTransform] = explicit.unwrap_or(&skel.ref_pose);
        let scale_src = if raw.len() == n { Some(raw) } else { None };
        let mut index = HashMap::with_capacity(fa.bones.len());
        let bones = fa.bones.iter().enumerate().map(|(b, fb)| {
            index.entry(fb.name.to_lowercase()).or_insert(b);
            let scale = scale_src.filter(|_| b < n).map(|p| skel.ancestor_scale(p, b));
            ClipBone {
                pos: fb.pos.as_ref().map(|k| k.iter().map(|p| pos_to_gltf(*p, scale)).collect()),
                rot: fb.rot.as_ref().map(|k| k.iter().map(|q| rot_to_gltf(*q)).collect()),
                has_track: fb.pos.is_some() || fb.rot.is_some() || fb.scale.is_some(),
                mode: if b == 0 { TranslationRetarget::Animation } else { fb.retarget },
                source: Vec3::new(fb.retarget_source[0], fb.retarget_source[2], fb.retarget_source[1]) * 0.01,
            }
        }).collect();
        FnClip {
            name: fa.name.clone(), mode, index, bones, frames: fa.num_frames.max(1), length: fa.length, rate_scale: fa.rate_scale, md5_rate: fa.frame_rate,
            additive: fa.additive,
            additive_kind: match fa.additive_kind { 1 => AdditiveKind::LocalSpace, 2 => AdditiveKind::MeshSpaceRotationOffset, _ => AdditiveKind::None },
            ref_pose_type: match fa.ref_pose_type { 1 => RefPoseType::RefPose, 2 => RefPoseType::AnimScaled, 3 => RefPoseType::AnimFrame, _ => RefPoseType::None },
            step: fa.step, retarget_base: fa.retarget_base, curves: fa.curves.clone(),
        }
    }
    pub fn additive_kind(&self) -> AdditiveKind { self.additive_kind }
    pub fn ref_pose_type(&self) -> RefPoseType { self.ref_pose_type }
    pub fn num_frames(&self) -> usize { self.frames }
    /// `SequenceLength` in seconds.
    pub fn sequence_length(&self) -> f32 { self.length }
    pub fn num_bones(&self) -> usize { self.bones.len() }
    /// The sequence's float curves (UE `FFloatCurve`s), as decoded.
    pub fn float_curves(&self) -> &[FloatCurve] { &self.curves }
    fn curve_time(&self, t: f32, looping: bool) -> f32 {
        if looping && self.length > 0. { t.rem_euclid(self.length) } else { t.clamp(0., self.length.max(0.)) }
    }

    /// Sampled, converted, un-retargeted value of a bone.
    fn raw(&self, b: &ClipBone, t: f32, looping: bool) -> (Option<Vec3>, Option<Quat>) {
        let n = self.frames;
        match self.mode {
            SampleMode::LegacyMd5 => {
                let rate = self.md5_rate;
                let f = if looping { t * rate } else { (t * rate).min((n - 1) as f32) };
                let a = f.floor() as usize % n;
                let c = if looping { (a + 1) % n } else { (a + 1).min(n - 1) };
                let fr = f.fract();
                let null = b.has_track;
                let p = match &b.pos { Some(k) => Some(key(k, a).lerp(key(k, c), fr)), None if null => Some(Vec3::ZERO), None => None };
                let q = match &b.rot { Some(k) => Some(key(k, a).slerp(key(k, c), fr)), None if null => Some(Quat::IDENTITY), None => None };
                (p, q)
            }
            SampleMode::Ue => {
                let len = self.length;
                let tt = if looping { if len > 0.0 { t.rem_euclid(len) } else { 0.0 } } else { t.clamp(0.0, len.max(0.0)) };
                let fpos = if n > 1 && len > 0.0 { tt / len * (n - 1) as f32 } else { 0.0 };
                let i0 = (fpos.floor() as usize).min(n - 1);
                let i1 = (i0 + 1).min(n - 1);
                let a = if i0 >= n - 1 || self.step { 0.0 } else { fpos - i0 as f32 };
                let p = b.pos.as_ref().map(|k| if a > 0.0 && k.len() > 1 { key(k, i0).lerp(key(k, i1), a) } else { key(k, i0) });
                let q = b.rot.as_ref().map(|k| if a > 0.0 && k.len() > 1 { nlerp(key(k, i0), key(k, i1), a) } else { key(k, i0) });
                (p, q)
            }
        }
    }

    /// The old `Clip::fit`: channel presence, then the bone's translation retarget mode.
    fn fit(&self, b: &ClipBone, (p, q): (Option<Vec3>, Option<Quat>), bind: BoneXform) -> BoneXform {
        let fallback = if self.additive { (Vec3::ZERO, Quat::IDENTITY) } else { bind };
        if p.is_none() && q.is_none() { return fallback; }
        let (p, q) = (p.unwrap_or(fallback.0), q.unwrap_or(fallback.1));
        match b.mode {
            TranslationRetarget::Skeleton => (if self.additive { Vec3::ZERO } else if self.retarget_base { b.source } else { bind.0 }, q),
            TranslationRetarget::AnimationScaled => {
                let length = b.source.length();
                if length > 0.00001 { (p * (bind.0.length() / length), q) } else if self.additive { (Vec3::ZERO, Quat::IDENTITY) } else { bind }
            }
            TranslationRetarget::OrientAndScale if !self.additive => {
                let (s, t) = (b.source, bind.0);
                let (ls, lt) = (s.length(), t.length());
                if (s - t).length_squared() > 1e-12 && ls > 1e-8 && lt > 1e-8 { (Quat::from_rotation_arc(s / ls, t / lt) * p * (lt / ls), q) } else { (p, q) }
            }
            // Animation, AnimationRelative (unimplemented, as before), OrientAndScale on additive clips: the animated translation
            _ => (p, q),
        }
    }
}

impl ClipSource for FnClip {
    fn sample_bone(&self, bone: &str, t: f32, looping: bool, bind: BoneXform) -> Option<BoneXform> {
        let b = &self.bones[*self.index.get(bone)?];
        Some(self.fit(b, self.raw(b, t, looping), bind))
    }
    fn is_additive(&self) -> bool { self.additive }
    fn duration(&self) -> f32 { if self.rate_scale.abs() > 1e-9 { self.length / self.rate_scale } else { self.length } }
    fn has_bone(&self, bone: &str) -> bool { self.index.contains_key(bone) }
    fn sample_float_curve(&self, name: &str, t: f32, looping: bool) -> f32 {
        let tt = self.curve_time(t, looping);
        self.curves.iter().find(|curve| curve.name == name).map_or(0., |curve| curve.eval(tt))
    }
    fn has_float_curve(&self, name: &str) -> bool { self.curves.iter().any(|curve| curve.name == name) }
}

/// Name -> clip lookup.
#[derive(Default)]
pub struct ClipStore { pub clips: HashMap<String, FnClip> }
impl ClipLibrary for ClipStore {
    fn clip(&self, name: &str) -> Option<&dyn ClipSource> { self.clips.get(name).map(|c| c as &dyn ClipSource) }
}

/// Where the sequence packages come from.
pub enum ClipSourceDir<'a> {
    /// Raw extracted packages below `root` (`FortniteGame/Content/...uasset/.uexp/.ubulk`), as the fn-anim tests use.
    RawPackages(&'a Path),
    /// A directory (searched recursively) of `.fnanim` files named `<clip>.fnanim` (what `fn-export` writes).
    FnAnimDir(&'a Path),
    /// A mounted pak set (the Fortnite install).
    Pak(&'a fn_pak::PakSet),
}

const SKELETON_PKG: &str = "/Game/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Skeleton";

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk_files(&p, out); } else { out.push(p); }
    }
}

/// `USkeleton` of the player (`Fortnite_M_Avg_Player_Skeleton`): bone names, parents, reference pose, retarget modes and retarget poses.
pub fn load_player_skeleton_info(src: &ClipSourceDir) -> Result<SkeletonInfo, String> {
    let pkg = match src {
        ClipSourceDir::RawPackages(root) => {
            let base = root.join("FortniteGame/Content/Characters/Player/Male/Male_Avg_Base/Fortnite_M_Avg_Player_Skeleton");
            let rd = |e: &str| std::fs::read(base.with_extension(e)).unwrap_or_default();
            let a = std::fs::read(base.with_extension("uasset")).map_err(|e| format!("{}: {e}", base.display()))?;
            fn_ueio::Package::parse(&a, &rd("uexp"), &rd("ubulk")).map_err(|e| e.to_string())?
        }
        ClipSourceDir::Pak(set) => {
            let a = set.read_game(SKELETON_PKG, "uasset").map_err(|e| e.to_string())?;
            let x = set.read_game(SKELETON_PKG, "uexp").map(|c| c.into_owned()).unwrap_or_default();
            let b = set.read_game(SKELETON_PKG, "ubulk").map(|c| c.into_owned()).unwrap_or_default();
            fn_ueio::Package::parse(&a, &x, &b).map_err(|e| e.to_string())?
        }
        ClipSourceDir::FnAnimDir(_) => return Err("a .fnanim directory carries no skeleton; load it from packages or the pak".into()),
    };
    SkeletonInfo::from_package(&pkg).map_err(|e| e.to_string())
}

impl ClipStore {
    pub fn new() -> ClipStore { ClipStore::default() }
    pub fn insert(&mut self, name: &str, clip: FnClip) { self.clips.insert(name.to_owned(), clip); }

    /// Loads every named clip (`names` are sequence asset names without extension, e.g. `"M_Jog_AR_Core"`). `Err` names the first clip
    /// that cannot be found or decoded.
    pub fn load(src: &ClipSourceDir, names: &[String], skel: &SkeletonInfo, mode: SampleMode) -> Result<ClipStore, String> {
        let mut store = ClipStore::new();
        // paks: every clip is an independent glob + read + decode, so they run on a small thread pool (results keep the order of `names`, the first error wins)
        if let ClipSourceDir::Pak(set) = src {
            let mut uniq: Vec<&String> = vec![];
            for n in names { if !uniq.contains(&n) { uniq.push(n); } }
            let n = uniq.len();
            let out: Vec<std::sync::Mutex<Option<Result<FnClip, String>>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
            let next = std::sync::atomic::AtomicUsize::new(0);
            let threads = std::thread::available_parallelism().map(|p| p.get()).unwrap_or(4).clamp(2, 12).min(n.max(1));
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n { break; }
                        let name = uniq[i];
                        let r = (|| {
                            let pat = match name.strip_prefix("/Game/") { Some(rest) => format!("fortnitegame/content/{}.uasset", rest.to_lowercase()), None => format!("fortnitegame/content/**/{}.uasset", name.to_lowercase()) };
                            let hit = set.glob(&pat);
                            let (_, e) = hit.first().ok_or_else(|| format!("sequence {name} not in the paks"))?;
                            let base = e.path.trim_end_matches(".uasset").to_owned();
                            let rd = |ext: &str| set.read(&format!("{base}.{ext}")).map(|c| c.into_owned()).unwrap_or_default();
                            let pkg = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")).map_err(|e| format!("{name}: {e}"))?;
                            let seq = AnimSequence::parse(&pkg).map_err(|e| format!("{name}: {e}"))?;
                            Ok::<FnClip, String>(FnClip::from_sequence(&seq, skel, mode))
                        })();
                        *out[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    });
                }
            });
            for (name, slot) in uniq.iter().zip(out) {
                let r = slot.into_inner().unwrap_or_else(|e| e.into_inner()).ok_or_else(|| format!("{name}: not decoded"))?;
                store.clips.insert((*name).clone(), r?);
            }
            return Ok(store);
        }
        let mut files = vec![];
        match src {
            ClipSourceDir::RawPackages(root) => walk_files(&root.join("FortniteGame/Content"), &mut files),
            ClipSourceDir::FnAnimDir(dir) => walk_files(dir, &mut files),
            ClipSourceDir::Pak(_) => {}
        }
        let find_file = |name: &str, ext: &str| files.iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) && p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name))).cloned();
        for name in names {
            if store.clips.contains_key(name) { continue; }
            let clip = match src {
                ClipSourceDir::RawPackages(_) => {
                    let p = find_file(name, "uasset").ok_or_else(|| format!("sequence package {name}.uasset not found"))?;
                    let rd = |e: &str| std::fs::read(p.with_extension(e)).unwrap_or_default();
                    let pkg = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")).map_err(|e| format!("{name}: {e}"))?;
                    let seq = AnimSequence::parse(&pkg).map_err(|e| format!("{name}: {e}"))?;
                    FnClip::from_sequence(&seq, skel, mode)
                }
                ClipSourceDir::FnAnimDir(_) => {
                    let p = find_file(name, "fnanim").ok_or_else(|| format!("{name}.fnanim not found"))?;
                    let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                    let fa = FnAnim::read(&bytes).map_err(|e| format!("{name}: {e}"))?;
                    FnClip::from_fnanim(&fa, skel, mode)
                }
                ClipSourceDir::Pak(set) => {
                    let pat = match name.strip_prefix("/Game/") { Some(rest) => format!("fortnitegame/content/{}.uasset", rest.to_lowercase()), None => format!("fortnitegame/content/**/{}.uasset", name.to_lowercase()) };
                    let hit = set.glob(&pat);
                    let (_, e) = hit.first().ok_or_else(|| format!("sequence {name} not in the paks"))?;
                    let base = e.path.trim_end_matches(".uasset").to_owned();
                    let rd = |ext: &str| set.read(&format!("{base}.{ext}")).map(|c| c.into_owned()).unwrap_or_default();
                    let pkg = fn_ueio::Package::parse(&rd("uasset"), &rd("uexp"), &rd("ubulk")).map_err(|e| format!("{name}: {e}"))?;
                    let seq = AnimSequence::parse(&pkg).map_err(|e| format!("{name}: {e}"))?;
                    FnClip::from_sequence(&seq, skel, mode)
                }
            };
            store.clips.insert(name.clone(), clip);
        }
        Ok(store)
    }
}
