//! `UAnimSequence` parsing and sampling.
//!
//! ## Serialization order of a cooked UE 4.16 `AnimSequence` export (Fortnite 1.8)
//!
//! ```text
//! tagged properties          NumFrames, SequenceLength, RateScale, Interpolation, AdditiveAnimType, RefPoseType,
//!                            RefPoseSeq, RefFrameIndex, RetargetSource, RetargetSourceAssetReferencePose,
//!                            Notifies, RawCurveData, AuthoredSyncMarkers, bEnableRootMotion,
//!                            TrackToSkeletonMapTable, ...   (None terminator)
//! int32   bSerializeGuid     0
//! FGuid   SkeletonGuid       16 bytes (UAnimationAsset)
//! FStripDataFlags            2 bytes (GlobalStripFlags, ClassStripFlags); bit0 of GlobalStripFlags = editor data stripped
//! [ only when editor data is NOT stripped:
//!   TArray<FRawAnimSequenceTrack> RawAnimationData        i32 n; n * { TArray<FVector> pos; TArray<FQuat> rot; TArray<FVector> scale }
//!   TArray<FRawAnimSequenceTrack> SourceRawAnimationData ]
//! int32   bSerializeCompressedData          (FrameworkObjectVersion 23 >= MoveCompressedAnimDataToTheDDC)
//! if true:
//!   u8 KeyEncodingFormat, u8 TranslationCompressionFormat, u8 RotationCompressionFormat, u8 ScaleCompressionFormat
//!   TArray<int32> CompressedTrackOffsets             i32 n; n * i32
//!   FCompressedOffsetData CompressedScaleOffsets     TArray<int32> OffsetData; int32 StripSize
//!   TArray<FTrackToSkeletonMap> CompressedTrackToSkeletonMapTable    i32 n; n * i32 BoneTreeIndex
//!   FRawCurveTracks CompressedCurveData              tagged property list (FloatCurves)
//!   (no CompressedRawDataSize / CompressedNumFrames: those appear from 4.17 / 4.22)
//!   int32 NumBytes; u8 CompressedByteStream[NumBytes]
//!   int32 bUseRawDataOnly
//! ```
//!
//! ## `CompressedTrackOffsets` layout
//!
//! * `AKF_ConstantKeyLerp` / `AKF_VariableKeyLerp`: 4 ints per track `[TransOffset, TransKeys, RotOffset, RotKeys]`
//!   (offsets are byte offsets into `CompressedByteStream`). `CompressedScaleOffsets.OffsetData` holds
//!   `StripSize` (2) ints per track `[ScaleOffset, ScaleKeys]`.
//!   Translation block: `[Mins f32x3, Ranges f32x3]` if the format is `IntervalFixed32NoW` and more than 1 key,
//!   then the keys (a single key is always stored as 3 x f32), then (variable only) padding to 4 bytes and
//!   `TransKeys` frame indices (`u8` when `NumFrames < 256`, else `u16`). Rotation block: same, a single
//!   key is stored as `Float96NoW`; no alignment between the key data and the frame table beyond 4 bytes.
//! * `AKF_PerTrackCompression`: 2 ints per track `[TransOffset, RotOffset]` (`-1` = no data: translation `(0,0,0)`,
//!   rotation identity); scale offsets (`StripSize` 1) in `CompressedScaleOffsets.OffsetData`.
//!   Each block starts with a `u32` header: `format = h >> 28`, `mask = (h >> 24) & 0xF`
//!   (bits 0..2 = X/Y/Z component present, bit 3 = has frame table), `num_keys = h & 0xFFFFFF`.
//!   Interval formats follow with `(min, range)` pairs for each present component, then the keys, then padding to
//!   4 bytes and, if bit 3 is set (and `num_keys > 1`), the frame table.
//!   UE 4.16 serialized the byte stream with scale data *before* translation/rotation while the offsets pointed to the
//!   original order ("ByteSwapOut" bug); the stream is re-laid-out like UEViewer's `TransferPerTrackData` when scale
//!   offsets are present.
//!
//! Keys have no explicit time (constant lerp: key `i` of `NumFrames` is frame `i`; a single key is constant) or an
//! explicit frame-index table (variable / per-track). Time `t` maps to frame position
//! `t / SequenceLength * (NumFrames - 1)`; positions are `lerp`ed and rotations `nlerp`ed (shortest arc, normalised),
//! exactly like `AEFConstantKeyLerp` / `AEFVariableKeyLerp` / `AEFPerTrackCompression`.

use crate::codec::{self, align4, CompressionFormat, KeyFormat};
use crate::skeleton::{BoneTransform, SkeletonInfo};
use crate::{err, Error, Result};
use fn_ueio::props::{read_props, Ctx, PropList, PropValue};
use fn_ueio::{Package, Reader};

/// `EAnimInterpolationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Step,
}

/// `EAdditiveAnimationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdditiveAnimType {
    None,
    LocalSpaceBase,
    RotationOffsetMeshSpace,
}

/// `EAdditiveBasePoseType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPoseType {
    None,
    /// Skeleton reference pose.
    RefPose,
    /// Pose of `RefPoseSeq`, scaled over time with the length of this sequence.
    AnimScaled,
    /// Frame `RefFrameIndex` of `RefPoseSeq`.
    AnimFrame,
}

/// How the additive base pose is obtained (only meaningful when the sequence is additive).
#[derive(Debug, Clone, PartialEq)]
pub struct AdditiveInfo {
    pub kind: AdditiveAnimType,
    pub base: RefPoseType,
    /// Object name of `RefPoseSeq` (e.g. `CommandoHeavyGun_Jog_E_Core`), resolved through the import table.
    pub ref_pose_seq: Option<String>,
    /// Full import path of `RefPoseSeq` (`/Game/.../Package.Object`).
    pub ref_pose_seq_path: Option<String>,
    pub ref_frame_index: i32,
}

/// One entry of `UAnimSequenceBase::Notifies` (`FAnimNotifyEvent`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notify {
    pub notify_name: String,
    /// `FAnimLinkableElement::LinkValue`: time in seconds from the start of the sequence.
    pub link_value: f32,
    pub trigger_time_offset: f32,
    pub end_trigger_time_offset: f32,
    pub duration: f32,
    pub trigger_weight_threshold: f32,
    pub notify_trigger_chance: f32,
    pub track_index: i32,
    /// `Notify` object reference (instanced `UAnimNotify`): export/import object name.
    pub notify_object: Option<String>,
    /// Class of `notify_object` when it is an export of this package.
    pub notify_class: Option<String>,
    pub notify_state_object: Option<String>,
    /// `Sound` of an `AnimNotify_PlaySound` export: the SoundCue object path (`/Game/.../X.X`).
    pub sound: Option<String>,
    /// `PitchMultiplier` / `VolumeMultiplier` of that `AnimNotify_PlaySound` (1.0 when not serialized).
    pub sound_pitch: f32,
    pub sound_volume: f32,
    /// `ReloadStage` (`EFortReloadFXState::...`) of an `AnimNotify_PlayReloadFX_C` export; `Some("")` when the enum is the class default (value 0).
    pub reload_stage: Option<String>,
    /// HEAL FX: an `AnimNotify_PlayParticleEffect` export (e.g. `P_Consume_Item` on `b_MF_Head` in the 1.8 consumable montages).
    pub particle: Option<ParticleNotify>,
}

/// `UAnimNotify_PlayParticleEffect` (UE4.16 AnimNotify_PlayParticleEffect.h; exe `Notify` 0x141fe85d0): `PSTemplate`, `SocketName`, `LocationOffset`,
/// `RotationOffset` (pitch, yaw, roll), `Attached` (the constructor sets it true, so an unserialized flag is true).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParticleNotify {
    /// Particle system object path (`/Game/.../P_X.P_X`).
    pub template: String,
    pub socket: String,
    pub location_offset: [f32; 3],
    pub rotation_offset: [f32; 3],
    pub attached: bool,
}

impl Notify {
    /// Time (seconds) the notify fires: `LinkValue + TriggerTimeOffset`.
    pub fn trigger_time(&self) -> f32 {
        self.link_value + self.trigger_time_offset
    }
    /// End time of a notify state.
    pub fn end_time(&self) -> f32 {
        self.trigger_time() + self.duration + self.end_trigger_time_offset
    }
}

/// `FRichCurveKey`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CurveKey {
    pub time: f32,
    pub value: f32,
    pub arrive_tangent: f32,
    pub leave_tangent: f32,
    /// `ERichCurveInterpMode` (0 linear, 1 constant, 2 cubic, 3 none).
    pub interp_mode: u8,
}

/// `FFloatCurve` (a named float curve with its `FRichCurve`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FloatCurve {
    pub name: String,
    pub flags: i32,
    pub default_value: f32,
    pub keys: Vec<CurveKey>,
}

impl FloatCurve {
    /// Evaluate with linear interpolation / constant / cubic Hermite according to the key's `interp_mode`.
    pub fn eval(&self, t: f32) -> f32 {
        let k = &self.keys;
        if k.is_empty() {
            return self.default_value;
        }
        if t <= k[0].time {
            return k[0].value;
        }
        if t >= k[k.len() - 1].time {
            return k[k.len() - 1].value;
        }
        let i = k.partition_point(|x| x.time <= t) - 1;
        let (a, b) = (k[i], k[i + 1]);
        let dt = b.time - a.time;
        if dt <= 0.0 {
            return b.value;
        }
        let alpha = (t - a.time) / dt;
        match a.interp_mode {
            1 => a.value,
            2 => {
                let (t2, t3) = (alpha * alpha, alpha * alpha * alpha);
                let (m0, m1) = (a.leave_tangent * dt, b.arrive_tangent * dt);
                (2.0 * t3 - 3.0 * t2 + 1.0) * a.value + (t3 - 2.0 * t2 + alpha) * m0 + (-2.0 * t3 + 3.0 * t2) * b.value + (t3 - t2) * m1
            }
            _ => a.value + (b.value - a.value) * alpha,
        }
    }
}

/// `FAnimSyncMarker`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncMarker {
    pub name: String,
    pub time: f32,
}

/// Decoded key data of one animation track. Times are frame indices; an empty time table means the keys are evenly
/// spaced over `[0, NumFrames - 1]` (and a single key is constant).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    /// Skeleton bone index (`BoneTreeIndex`).
    pub bone: i32,
    pub pos: Vec<[f32; 3]>,
    pub pos_times: Vec<u32>,
    pub rot: Vec<[f32; 4]>,
    pub rot_times: Vec<u32>,
    pub scale: Vec<[f32; 3]>,
    pub scale_times: Vec<u32>,
    /// Per-track storage format `(format, component mask)` of each channel; `None` when the channel has no data
    /// block or the sequence is not `PerTrackCompression`.
    pub pos_format: Option<(CompressionFormat, u8)>,
    pub rot_format: Option<(CompressionFormat, u8)>,
    pub scale_format: Option<(CompressionFormat, u8)>,
}

/// Raw per-component sample of a track at a frame position. `None` = the track has no keys for that channel.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackSample {
    pub pos: Option<[f32; 3]>,
    pub rot: Option<[f32; 4]>,
    pub scale: Option<[f32; 3]>,
}

/// Bracketing keys for `frame` (in frames): `(index0, index1, alpha)`.
pub fn find_keys(num_keys: usize, times: &[u32], num_frames: usize, frame: f32) -> (usize, usize, f32) {
    if num_keys <= 1 {
        return (0, 0, 0.0);
    }
    if times.is_empty() {
        let last_frame = (num_frames.max(2) - 1) as f32;
        let p = (frame / last_frame * (num_keys - 1) as f32).clamp(0.0, (num_keys - 1) as f32);
        let i0 = (p.floor() as usize).min(num_keys - 1);
        if i0 >= num_keys - 1 {
            return (num_keys - 1, num_keys - 1, 0.0);
        }
        return (i0, i0 + 1, p - i0 as f32);
    }
    let n = times.len().min(num_keys);
    if frame <= times[0] as f32 {
        return (0, 0, 0.0);
    }
    if frame >= times[n - 1] as f32 {
        return (n - 1, n - 1, 0.0);
    }
    // last i with times[i] <= frame
    let i = times[..n].partition_point(|&t| (t as f32) <= frame) - 1;
    let (t0, t1) = (times[i] as f32, times[i + 1] as f32);
    let a = if t1 > t0 { (frame - t0) / (t1 - t0) } else { 0.0 };
    (i, i + 1, a)
}

/// Shortest-arc normalised lerp (`FQuat::FastLerp` + `Normalize`).
pub fn nlerp(a: [f32; 4], b: [f32; 4], alpha: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let bias = if dot >= 0.0 { 1.0 } else { -1.0 };
    let mut r = [0.0f32; 4];
    for k in 0..4 {
        r[k] = b[k] * alpha + a[k] * (bias * (1.0 - alpha));
    }
    let len = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2] + r[3] * r[3]).sqrt();
    if len > 1e-8 {
        for c in r.iter_mut() {
            *c /= len;
        }
        r
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Spherical interpolation as implemented by UEViewer (`Slerp`, shortest arc). UE itself blends with [`nlerp`]; this
/// exists to reproduce the old md5anim export (which slerps between decoded keys) and to quantify the difference.
pub fn slerp(a: [f32; 4], b: [f32; 4], alpha: f32) -> [f32; 4] {
    if alpha <= 0.0 {
        return a;
    }
    if alpha >= 1.0 {
        return b;
    }
    let mut cosom = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let mut sign = 1.0f32;
    if cosom < 0.0 {
        cosom = -cosom;
        sign = -1.0;
    }
    let (sa, mut sb);
    if 1.0 - cosom > 1e-6 {
        // UEViewer evaluates the transcendental functions in double precision (C `sqrt`/`atan2`/`sin`) and rounds to
        // float at each assignment; reproduce that so results match its export to the last digit.
        let f = 1.0 - cosom * cosom;
        let sinom_inv = (1.0f64 / (f as f64).sqrt()) as f32;
        let omega = ((f * sinom_inv) as f64).atan2(cosom.abs() as f64) as f32;
        sa = ((((1.0 - alpha) * omega) as f64).sin() * sinom_inv as f64) as f32;
        sb = (((alpha * omega) as f64).sin() * sinom_inv as f64) as f32;
    } else {
        sa = 1.0 - alpha;
        sb = alpha;
    }
    sb *= sign;
    [sa * a[0] + sb * b[0], sa * a[1] + sb * b[1], sa * a[2] + sb * b[2], sa * a[3] + sb * b[3]]
}

/// How rotation keys are blended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationBlend {
    /// UE's `FQuat::FastLerp` + normalise (what `AEF*KeyLerp` does).
    Nlerp,
    /// UEViewer's slerp.
    Slerp,
}

pub fn lerp3(a: [f32; 3], b: [f32; 3], alpha: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * alpha, a[1] + (b[1] - a[1]) * alpha, a[2] + (b[2] - a[2]) * alpha]
}

/// Hamilton product `a * b` (`xyzw`).
pub fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

impl Track {
    /// Sample the track at `frame` (fractional frame index). `step`: `Interpolation::Step` (no interpolation).
    pub fn sample(&self, num_frames: usize, frame: f32, step: bool) -> TrackSample {
        self.sample_with(num_frames, frame, step, RotationBlend::Nlerp)
    }

    /// [`sample`](Self::sample) with an explicit rotation blend.
    pub fn sample_with(&self, num_frames: usize, frame: f32, step: bool, blend: RotationBlend) -> TrackSample {
        let mut s = TrackSample::default();
        if !self.pos.is_empty() {
            let (i0, i1, a) = find_keys(self.pos.len(), &self.pos_times, num_frames, frame);
            s.pos = Some(if step || a <= 0.0 { self.pos[i0] } else { lerp3(self.pos[i0], self.pos[i1], a) });
        }
        if !self.rot.is_empty() {
            let (i0, i1, a) = find_keys(self.rot.len(), &self.rot_times, num_frames, frame);
            s.rot = Some(if step || a <= 0.0 {
                self.rot[i0]
            } else if blend == RotationBlend::Nlerp {
                nlerp(self.rot[i0], self.rot[i1], a)
            } else {
                slerp(self.rot[i0], self.rot[i1], a)
            });
        }
        if !self.scale.is_empty() {
            let (i0, i1, a) = find_keys(self.scale.len(), &self.scale_times, num_frames, frame);
            s.scale = Some(if step || a <= 0.0 { self.scale[i0] } else { lerp3(self.scale[i0], self.scale[i1], a) });
        }
        s
    }
}

/// An anim sequence ready to be sampled.
#[derive(Debug, Clone)]
pub struct AnimSequence {
    /// Export object name.
    pub name: String,
    pub num_frames: usize,
    /// `SequenceLength` (seconds).
    pub length: f32,
    pub rate_scale: f32,
    pub interpolation: Interpolation,
    pub enable_root_motion: bool,
    pub additive: AdditiveInfo,
    /// `RetargetSource` name (`None` when unset).
    pub retarget_source: String,
    /// `RetargetSourceAssetReferencePose` (only used when `retarget_source == "None"`).
    pub retarget_source_asset_pose: Vec<BoneTransform>,
    /// Object name of the `Skeleton` import.
    pub skeleton_name: Option<String>,
    pub skeleton_path: Option<String>,
    pub notifies: Vec<Notify>,
    pub curves: Vec<FloatCurve>,
    pub sync_markers: Vec<SyncMarker>,
    pub key_format: KeyFormat,
    pub translation_format: CompressionFormat,
    pub rotation_format: CompressionFormat,
    pub scale_format: CompressionFormat,
    /// Size of the compressed key stream in bytes (0 for raw tracks).
    pub compressed_bytes: usize,
    /// `true` when the keys came from `RawAnimationData` rather than the compressed stream.
    pub from_raw: bool,
    /// Decoded tracks in `TrackToSkeletonMap` order.
    pub tracks: Vec<Track>,
}

fn enum_text(v: &PropValue) -> Option<String> {
    match v {
        PropValue::Enum(s) | PropValue::Name(s) => Some(s.rsplit("::").next().unwrap_or(s).to_string()),
        PropValue::Byte(b) => Some(b.to_string()),
        _ => None,
    }
}

fn float_of(v: Option<&PropValue>) -> Option<f32> {
    v.and_then(|v| v.as_f64()).map(|f| f as f32)
}

fn obj_name(pkg: &Package, idx: i32) -> Option<String> {
    if idx == 0 {
        None
    } else {
        Some(pkg.index_name(idx))
    }
}

pub(crate) fn parse_notifies(pkg: &Package, props: &PropList) -> Vec<Notify> {
    let mut out = Vec::new();
    let Some(PropValue::Array(items)) = props.get("Notifies") else { return out };
    for it in items {
        let Some(f) = it.as_fields() else { continue };
        let mut n = Notify { notify_trigger_chance: 1.0, ..Default::default() };
        n.notify_name = f.text("NotifyName").unwrap_or("").to_string();
        n.link_value = f.float("LinkValue").unwrap_or(0.0) as f32;
        if n.link_value == 0.0 {
            // old assets: DisplayTime_DEPRECATED
            n.link_value = f.float("DisplayTime_DEPRECATED").unwrap_or(0.0) as f32;
        }
        n.trigger_time_offset = f.float("TriggerTimeOffset").unwrap_or(0.0) as f32;
        n.end_trigger_time_offset = f.float("EndTriggerTimeOffset").unwrap_or(0.0) as f32;
        n.duration = f.float("Duration").unwrap_or(0.0) as f32;
        n.trigger_weight_threshold = f.float("TriggerWeightThreshold").unwrap_or(0.0) as f32;
        if let Some(c) = f.float("NotifyTriggerChance") {
            n.notify_trigger_chance = c as f32;
        }
        n.track_index = f.int("TrackIndex").unwrap_or(0) as i32;
        if let Some(o) = f.object("Notify") {
            n.notify_object = obj_name(pkg, o);
            if o > 0 {
                n.notify_class = Some(pkg.export_class(o as usize - 1));
                // the properties of the two notify classes that make weapon sound (play-sound, reload FX stage)
                let class = n.notify_class.as_deref().unwrap_or("");
                if class == "AnimNotify_PlaySound" || class == "FortAnimNotify_PlayEmoteMusic" || class == "AnimNotify_PlayReloadFX_C" {
                    let (start, end) = pkg.export_range(o as usize - 1);
                    let mut r = Reader::at(&pkg.data[..end], start);
                    let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
                    if let Ok(p) = read_props(&mut r, &ctx) {
                        if class == "AnimNotify_PlaySound" || class == "FortAnimNotify_PlayEmoteMusic" {
                            n.sound = p.object("Sound").filter(|s| *s < 0).map(|s| pkg.import_path(s));
                            n.sound_pitch = p.float("PitchMultiplier").map(|x| x as f32).unwrap_or(1.0);
                            n.sound_volume = p.float("VolumeMultiplier").map(|x| x as f32).unwrap_or(1.0);
                        } else {
                            n.reload_stage = Some(p.text("ReloadStage").unwrap_or("").to_string());
                        }
                    }
                } else if class == "AnimNotify_PlayParticleEffect" {
                    let (start, end) = pkg.export_range(o as usize - 1);
                    let mut r = Reader::at(&pkg.data[..end], start);
                    let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
                    if let Ok(p) = read_props(&mut r, &ctx) {
                        let v3 = |k: &str| match p.get(k) { Some(PropValue::Vec3(v)) => *v, _ => [0.; 3] };
                        if let Some(t) = p.object("PSTemplate").filter(|s| *s < 0).map(|s| pkg.import_path(s)) {
                            n.particle = Some(ParticleNotify { template: t, socket: p.text("SocketName").unwrap_or("None").to_string(), location_offset: v3("LocationOffset"), rotation_offset: v3("RotationOffset"), attached: p.bool("Attached").unwrap_or(true) });
                        }
                    }
                }
            }
        }
        if let Some(o) = f.object("NotifyStateClass") {
            n.notify_state_object = obj_name(pkg, o);
            if n.notify_class.is_none() && o > 0 {
                n.notify_class = Some(pkg.export_class(o as usize - 1));
            }
        }
        out.push(n);
    }
    out
}

/// `FSmartName` as stored in a 4.16 curve: `FName DisplayName` + `uint16 UID` (AnimPhysObjectVersion 3 < 5), 10 bytes.
fn smart_name(pkg: &Package, v: &PropValue) -> String {
    match v {
        PropValue::Raw(b) if b.len() >= 8 => {
            let idx = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
            let num = u32::from_le_bytes(b[4..8].try_into().unwrap());
            let base = pkg.names.get(idx).cloned().unwrap_or_default();
            if num > 0 {
                format!("{}_{}", base, num - 1)
            } else {
                base
            }
        }
        PropValue::Struct { fields, .. } => fields.text("DisplayName").unwrap_or("").to_string(),
        PropValue::Name(n) => n.clone(),
        _ => String::new(),
    }
}

/// `FRichCurve::Keys`: an array of `FRichCurveKey` whose elements are serialized natively (27 bytes each:
/// `u8 InterpMode, u8 TangentMode, u8 TangentWeightMode, f32 Time, Value, ArriveTangent, LeaveTangent,
/// ArriveTangentWeight, LeaveTangentWeight`) behind the usual array-of-struct inner tag, which makes the generic
/// property reader return the payload as `Raw`.
fn rich_keys(v: &PropValue) -> Vec<CurveKey> {
    let mut out = Vec::new();
    let PropValue::Raw(raw) = v else {
        if let PropValue::Array(items) = v {
            for k in items {
                if let Some(kf) = k.as_fields() {
                    out.push(CurveKey {
                        time: kf.float("Time").unwrap_or(0.0) as f32,
                        value: kf.float("Value").unwrap_or(0.0) as f32,
                        arrive_tangent: kf.float("ArriveTangent").unwrap_or(0.0) as f32,
                        leave_tangent: kf.float("LeaveTangent").unwrap_or(0.0) as f32,
                        interp_mode: 0,
                    });
                }
            }
        }
        return out;
    };
    let mut r = Reader::new(raw);
    let parse = |r: &mut Reader| -> Result<Vec<CurveKey>> {
        let n = r.i32()?;
        if n <= 0 {
            return Ok(Vec::new());
        }
        r.skip(16)?; // tag name + type
        let size = r.i32()?;
        r.skip(4 + 8 + 16)?; // array index, struct name, struct guid
        if r.u8()? != 0 {
            r.skip(16)?;
        }
        let per = size as usize / n as usize;
        if per != 27 {
            return err(format!("unexpected FRichCurveKey size {per}"));
        }
        let mut keys = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let interp = r.u8()?;
            r.skip(2)?;
            let (time, value, arrive, leave) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
            r.skip(8)?;
            keys.push(CurveKey { time, value, arrive_tangent: arrive, leave_tangent: leave, interp_mode: interp });
        }
        Ok(keys)
    };
    parse(&mut r).unwrap_or_default()
}

fn curve_from_fields(pkg: &Package, f: &PropList) -> FloatCurve {
    let mut c = FloatCurve::default();
    if let Some(n) = f.get("Name") {
        c.name = smart_name(pkg, n);
    }
    c.flags = f.int("CurveTypeFlags").unwrap_or(0) as i32;
    if let Some(rc) = f.fields("FloatCurve") {
        let d = rc.float("DefaultValue").unwrap_or(0.0) as f32;
        // FRichCurve::DefaultValue defaults to MAX_flt ("no default")
        c.default_value = if d >= f32::MAX * 0.99 { 0.0 } else { d };
        if let Some(k) = rc.get("Keys") {
            c.keys = rich_keys(k);
        }
    }
    c
}

fn parse_curves(pkg: &Package, list: &PropList, into: &mut Vec<FloatCurve>) {
    // RawCurveData / CompressedCurveData: struct FRawCurveTracks { FloatCurves }
    if let Some(items) = list.array("FloatCurves") {
        for it in items {
            if let Some(f) = it.as_fields() {
                into.push(curve_from_fields(pkg, f));
            }
        }
    }
}
fn read_i32_array(r: &mut Reader) -> Result<Vec<i32>> {
    let n = r.i32()?;
    if n < 0 || n as usize * 4 > r.remaining() {
        return err(format!("bad int array count {n}"));
    }
    (0..n).map(|_| r.i32().map_err(Error::from)).collect()
}

fn read_vec3_array(r: &mut Reader) -> Result<Vec<[f32; 3]>> {
    let n = r.i32()?;
    if n < 0 || n as usize * 12 > r.remaining() {
        return err(format!("bad vector array count {n}"));
    }
    (0..n).map(|_| r.vec3().map_err(Error::from)).collect()
}

fn read_quat_array(r: &mut Reader) -> Result<Vec<[f32; 4]>> {
    let n = r.i32()?;
    if n < 0 || n as usize * 16 > r.remaining() {
        return err(format!("bad quat array count {n}"));
    }
    (0..n).map(|_| r.quat().map_err(Error::from)).collect()
}

fn read_raw_tracks(r: &mut Reader) -> Result<Vec<(Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<[f32; 3]>)>> {
    let n = r.i32()?;
    if n < 0 || n as usize * 12 > r.remaining() {
        return err(format!("bad raw track count {n}"));
    }
    let mut v = Vec::new();
    for _ in 0..n {
        let p = read_vec3_array(r)?;
        let q = read_quat_array(r)?;
        let s = read_vec3_array(r)?;
        v.push((p, q, s));
    }
    Ok(v)
}

/// Offsets of a `FCompressedOffsetData`.
struct OffsetData {
    data: Vec<i32>,
    strip: usize,
}

impl OffsetData {
    fn valid(&self) -> bool {
        self.strip > 0 && !self.data.is_empty()
    }
    fn get(&self, track: usize, k: usize) -> Option<i32> {
        self.data.get(track * self.strip + k).copied()
    }
}

/// Compressed key stream plus the layout tables, shared by the three key formats.
struct Stream<'a> {
    bytes: &'a [u8],
    key_format: KeyFormat,
    tf: CompressionFormat,
    rf: CompressionFormat,
    sf: CompressionFormat,
    num_frames: usize,
    offsets: &'a [i32],
    scale_offsets: &'a OffsetData,
}

impl<'a> Stream<'a> {
    fn reader_at(&self, off: i32) -> Result<Reader<'a>> {
        if off < 0 || off as usize > self.bytes.len() {
            return err(format!("key block offset {off} outside stream of {} bytes", self.bytes.len()));
        }
        Ok(Reader::at(self.bytes, off as usize))
    }

    fn decode_track(&self, track: usize, bone: i32) -> Result<Track> {
        let mut t = Track { bone, ..Default::default() };
        match self.key_format {
            KeyFormat::PerTrackCompression => self.decode_per_track(track, &mut t)?,
            _ => self.decode_const_var(track, &mut t)?,
        }
        Ok(t)
    }

    fn decode_const_var(&self, track: usize, t: &mut Track) -> Result<()> {
        let var = self.key_format == KeyFormat::VariableKeyLerp;
        let o = track * 4;
        let get = |i: usize| self.offsets.get(o + i).copied().ok_or_else(|| Error::Format("CompressedTrackOffsets too short".into()));
        let (t_off, t_keys, r_off, r_keys) = (get(0)?, get(1)?, get(2)?, get(3)?);
        if t_keys > 0 {
            let (v, times) = self.read_vec_block(t_off, t_keys as usize, self.tf, var)?;
            t.pos = v;
            t.pos_times = times;
        }
        if r_keys > 0 {
            let mut r = self.reader_at(r_off)?;
            let n = r_keys as usize;
            let fmt = if n == 1 { CompressionFormat::Float96NoW } else { self.rf };
            let (mins, ranges) = if n > 1 && fmt == CompressionFormat::IntervalFixed32NoW { codec::read_interval(&mut r, None)? } else { ([0.0; 3], [0.0; 3]) };
            for _ in 0..n {
                t.rot.push(codec::read_quat_key(&mut r, fmt, None, mins, ranges)?);
            }
            if var {
                r.seek(align4(r.tell()))?;
                t.rot_times = codec::read_time_table(&mut r, n, self.num_frames)?;
            }
        }
        if self.scale_offsets.valid() {
            if let (Some(s_off), Some(s_keys)) = (self.scale_offsets.get(track, 0), self.scale_offsets.get(track, 1)) {
                if s_keys > 0 && s_off >= 0 {
                    let (v, times) = self.read_vec_block(s_off, s_keys as usize, self.sf, var)?;
                    t.scale = v;
                    t.scale_times = times;
                }
            }
        }
        Ok(())
    }

    fn read_vec_block(&self, off: i32, n: usize, fmt: CompressionFormat, var: bool) -> Result<(Vec<[f32; 3]>, Vec<u32>)> {
        let mut r = self.reader_at(off)?;
        let fmt = if n == 1 { CompressionFormat::None } else { fmt };
        let (mins, ranges) = if fmt == CompressionFormat::IntervalFixed32NoW { codec::read_interval(&mut r, None)? } else { ([0.0; 3], [0.0; 3]) };
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(codec::read_vec_key(&mut r, fmt, None, mins, ranges)?);
        }
        let mut times = Vec::new();
        if var {
            r.seek(align4(r.tell()))?;
            times = codec::read_time_table(&mut r, n, self.num_frames)?;
        }
        Ok((v, times))
    }

    fn per_track_header(r: &mut Reader) -> Result<(CompressionFormat, u8, usize)> {
        let h = r.u32()?;
        Ok((CompressionFormat::from_u8((h >> 28) as u8)?, ((h >> 24) & 0xf) as u8, (h & 0xff_ffff) as usize))
    }

    fn decode_per_track(&self, track: usize, t: &mut Track) -> Result<()> {
        let t_off = self.offsets.get(track * 2).copied().ok_or_else(|| Error::Format("CompressedTrackOffsets too short".into()))?;
        let r_off = self.offsets.get(track * 2 + 1).copied().ok_or_else(|| Error::Format("CompressedTrackOffsets too short".into()))?;
        let s_off = if self.scale_offsets.valid() { self.scale_offsets.get(track, 0).unwrap_or(-1) } else { -1 };
        // offset -1 = "no data": the channel keeps the default (reference / additive identity) pose. UEViewer exports
        // a single null key instead, see `md5::export`.
        if t_off != -1 {
            let (v, times, f) = self.read_per_track_vec(t_off)?;
            t.pos = v;
            t.pos_times = times;
            t.pos_format = Some(f);
        }
        if r_off != -1 {
            let mut r = self.reader_at(r_off)?;
            let (fmt, mask, n) = Self::per_track_header(&mut r)?;
            t.rot_format = Some((fmt, mask));
            let (mins, ranges) = if fmt == CompressionFormat::IntervalFixed32NoW { codec::read_interval(&mut r, Some(mask))? } else { ([0.0; 3], [0.0; 3]) };
            for _ in 0..n {
                t.rot.push(codec::read_quat_key(&mut r, fmt, Some(mask), mins, ranges)?);
            }
            r.seek(align4(r.tell()))?;
            if mask & 8 != 0 {
                t.rot_times = codec::read_time_table(&mut r, n, self.num_frames)?;
            }
        }
        if s_off != -1 {
            let (v, times, f) = self.read_per_track_vec(s_off)?;
            t.scale = v;
            t.scale_times = times;
            t.scale_format = Some(f);
        }
        Ok(())
    }

    fn read_per_track_vec(&self, off: i32) -> Result<(Vec<[f32; 3]>, Vec<u32>, (CompressionFormat, u8))> {
        let mut r = self.reader_at(off)?;
        let (fmt, mask, n) = Self::per_track_header(&mut r)?;
        let (mins, ranges) = if fmt == CompressionFormat::IntervalFixed32NoW { codec::read_interval(&mut r, Some(mask))? } else { ([0.0; 3], [0.0; 3]) };
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(codec::read_vec_key(&mut r, fmt, Some(mask), mins, ranges)?);
        }
        r.seek(align4(r.tell()))?;
        let mut times = Vec::new();
        if mask & 8 != 0 {
            times = codec::read_time_table(&mut r, n, self.num_frames)?;
        }
        Ok((v, times, (fmt, mask)))
    }
}

fn copy_run(src: &[u8], dst: &mut [u8], sp: &mut usize, dp: &mut usize, n: usize) -> Result<()> {
    if *sp + n > src.len() || *dp + n > dst.len() {
        return err("per-track stream transfer out of range");
    }
    dst[*dp..*dp + n].copy_from_slice(&src[*sp..*sp + n]);
    *sp += n;
    *dp += n;
    Ok(())
}

/// UEViewer `TransferPerTrackData`: undo the translation/rotation/scale order mix-up of the 4.16 per-track byte
/// stream when scale offsets exist.
fn transfer_per_track_data(src: &[u8], offsets: &[i32], scale: &OffsetData, num_frames: usize) -> Result<Vec<u8>> {
    let mut dst = vec![0u8; src.len()];
    let num_tracks = offsets.len() / 2;
    let mut sp = 0usize;
    for track in 0..num_tracks {
        for kind in 0..3 {
            let off = match kind {
                0 => offsets[track * 2],
                1 => offsets[track * 2 + 1],
                _ => scale.get(track, 0).unwrap_or(-1),
            };
            if off < 0 {
                continue;
            }
            let mut dp = off as usize;
            if sp + 4 > src.len() {
                return err("per-track stream transfer: truncated header");
            }
            let h = u32::from_le_bytes(src[sp..sp + 4].try_into().unwrap());
            copy_run(src, &mut dst, &mut sp, &mut dp, 4)?;
            let fmt = CompressionFormat::from_u8((h >> 28) as u8)?;
            let mask = ((h >> 24) & 0xf) as u8;
            let n = (h & 0xff_ffff) as usize;
            if fmt == CompressionFormat::IntervalFixed32NoW {
                copy_run(src, &mut dst, &mut sp, &mut dp, 4 * codec::components_for_mask(mask) * 2)?;
            }
            copy_run(src, &mut dst, &mut sp, &mut dp, codec::per_track_key_bytes(fmt, mask) * n)?;
            if mask & 8 != 0 {
                let a = align4(dp);
                if a != dp {
                    { let pad = a - dp; copy_run(src, &mut dst, &mut sp, &mut dp, pad)?; }
                }
                copy_run(src, &mut dst, &mut sp, &mut dp, if num_frames < 256 { 1 } else { 2 } * n)?;
            }
            let a = align4(dp);
            if a != dp {
                if a > dst.len() {
                    return err("per-track stream transfer: alignment past end");
                }
                { let pad = a - dp; copy_run(src, &mut dst, &mut sp, &mut dp, pad)?; }
            }
        }
    }
    Ok(dst)
}

impl AnimSequence {
    /// Parse the (first) `AnimSequence` export of a package.
    pub fn parse(pkg: &Package) -> Result<AnimSequence> {
        let idx = pkg.find_export("AnimSequence").ok_or_else(|| Error::Format("package has no AnimSequence export".into()))?;
        Self::parse_export(pkg, idx)
    }

    pub fn parse_export(pkg: &Package, export: usize) -> Result<AnimSequence> {
        let (start, end) = pkg.export_range(export);
        let mut r = Reader::at(&pkg.data[..end], start);
        let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
        let props = read_props(&mut r, &ctx)?;

        let num_frames = props.int("NumFrames").unwrap_or(0).max(0) as usize;
        let length = props.float("SequenceLength").unwrap_or(0.0) as f32;
        let rate_scale = props.float("RateScale").map(|f| f as f32).unwrap_or(1.0);
        let interpolation = match props.get("Interpolation").and_then(enum_text).as_deref() {
            Some("Step") => Interpolation::Step,
            _ => Interpolation::Linear,
        };
        let additive_kind = match props.get("AdditiveAnimType").and_then(enum_text).as_deref() {
            Some("AAT_LocalSpaceBase") => AdditiveAnimType::LocalSpaceBase,
            Some("AAT_RotationOffsetMeshSpace") => AdditiveAnimType::RotationOffsetMeshSpace,
            _ => AdditiveAnimType::None,
        };
        let ref_type = match props.get("RefPoseType").and_then(enum_text).as_deref() {
            Some("ABPT_RefPose") => RefPoseType::RefPose,
            Some("ABPT_AnimScaled") => RefPoseType::AnimScaled,
            Some("ABPT_AnimFrame") => RefPoseType::AnimFrame,
            _ => RefPoseType::None,
        };
        let ref_seq_idx = props.object("RefPoseSeq").unwrap_or(0);
        let additive = AdditiveInfo {
            kind: additive_kind,
            base: ref_type,
            ref_pose_seq: obj_name(pkg, ref_seq_idx),
            ref_pose_seq_path: if ref_seq_idx < 0 { Some(pkg.import_path(ref_seq_idx)) } else { None },
            ref_frame_index: props.int("RefFrameIndex").unwrap_or(0) as i32,
        };
        let retarget_source = props.text("RetargetSource").unwrap_or("None").to_string();
        let mut retarget_source_asset_pose = Vec::new();
        if let Some(PropValue::Array(items)) = props.get("RetargetSourceAssetReferencePose") {
            for it in items {
                if let Some(f) = it.as_fields() {
                    let rot = match f.get("Rotation") {
                        Some(PropValue::Vec4(q)) => *q,
                        _ => [0.0, 0.0, 0.0, 1.0],
                    };
                    let tr = match f.get("Translation") {
                        Some(PropValue::Vec3(v)) => *v,
                        _ => [0.0; 3],
                    };
                    let sc = match f.get("Scale3D") {
                        Some(PropValue::Vec3(v)) => *v,
                        _ => [1.0; 3],
                    };
                    retarget_source_asset_pose.push(BoneTransform { rotation: rot, translation: tr, scale: sc });
                }
            }
        }
        let (skeleton_name, skeleton_path) = match props.object("Skeleton") {
            Some(i) if i != 0 => (obj_name(pkg, i), if i < 0 { Some(pkg.import_path(i)) } else { None }),
            _ => (None, None),
        };
        let notifies = parse_notifies(pkg, &props);
        let mut curves = Vec::new();
        if let Some(c) = props.fields("RawCurveData") {
            parse_curves(pkg, c, &mut curves);
        }
        let mut sync_markers = Vec::new();
        if let Some(PropValue::Array(items)) = props.get("AuthoredSyncMarkers") {
            for it in items {
                if let Some(f) = it.as_fields() {
                    sync_markers.push(SyncMarker { name: f.text("MarkerName").unwrap_or("").to_string(), time: float_of(f.get("Time")).unwrap_or(0.0) });
                }
            }
        }
        let mut track_map: Vec<i32> = Vec::new();
        if let Some(PropValue::Array(items)) = props.get("TrackToSkeletonMapTable") {
            for it in items {
                track_map.push(it.as_fields().and_then(|f| f.int("BoneTreeIndex")).unwrap_or(0) as i32);
            }
        }

        let mut seq = AnimSequence {
            name: pkg.exports[export].object_name.clone(),
            num_frames,
            length,
            rate_scale,
            interpolation,
            enable_root_motion: props.bool("bEnableRootMotion").unwrap_or(false),
            additive,
            retarget_source,
            retarget_source_asset_pose,
            skeleton_name,
            skeleton_path,
            notifies,
            curves,
            sync_markers,
            key_format: props.text("KeyEncodingFormat").and_then(KeyFormat::from_name).unwrap_or(KeyFormat::ConstantKeyLerp),
            translation_format: props.text("TranslationCompressionFormat").and_then(CompressionFormat::from_name).unwrap_or(CompressionFormat::None),
            rotation_format: props.text("RotationCompressionFormat").and_then(CompressionFormat::from_name).unwrap_or(CompressionFormat::None),
            scale_format: props.text("ScaleCompressionFormat").and_then(CompressionFormat::from_name).unwrap_or(CompressionFormat::None),
            compressed_bytes: 0,
            from_raw: false,
            tracks: Vec::new(),
        };

        // ---- native tail ----
        let _serialize_guid = r.i32()?;
        r.skip(16)?; // SkeletonGuid
        let global_strip = r.u8()?;
        let _class_strip = r.u8()?;
        let editor_stripped = global_strip & 1 != 0;
        let mut raw = Vec::new();
        if !editor_stripped {
            raw = read_raw_tracks(&mut r)?;
            let _source = read_raw_tracks(&mut r)?;
        }
        let serialize_compressed = r.i32()? != 0;
        let mut compressed_offsets: Vec<i32> = Vec::new();
        let mut scale_offsets = OffsetData { data: Vec::new(), strip: 0 };
        let mut compressed_map: Vec<i32> = Vec::new();
        let mut stream: Vec<u8> = Vec::new();
        if serialize_compressed {
            seq.key_format = KeyFormat::from_u8(r.u8()?)?;
            seq.translation_format = CompressionFormat::from_u8(r.u8()?)?;
            seq.rotation_format = CompressionFormat::from_u8(r.u8()?)?;
            seq.scale_format = CompressionFormat::from_u8(r.u8()?)?;
            compressed_offsets = read_i32_array(&mut r)?;
            scale_offsets.data = read_i32_array(&mut r)?;
            scale_offsets.strip = r.i32()?.max(0) as usize;
            compressed_map = read_i32_array(&mut r)?;
            let curve_props = read_props(&mut r, &ctx)?;
            let mut compressed_curves = Vec::new();
            parse_curves(pkg, &curve_props, &mut compressed_curves);
            if seq.curves.is_empty() {
                seq.curves = compressed_curves;
            }
            let nbytes = r.i32()?;
            if nbytes < 0 {
                return err(format!("bad compressed byte stream size {nbytes}"));
            }
            stream = r.bytes(nbytes as usize)?.to_vec();
            let _use_raw = r.i32()?;
        }
        seq.compressed_bytes = stream.len();

        let bone_map: &Vec<i32> = if !compressed_map.is_empty() { &compressed_map } else { &track_map };
        if !compressed_offsets.is_empty() {
            let per_bone = if seq.key_format == KeyFormat::PerTrackCompression { 2 } else { 4 };
            if compressed_offsets.len() != bone_map.len() * per_bone {
                return err(format!(
                    "{}: CompressedTrackOffsets has {} entries, expected {} for {} tracks",
                    seq.name,
                    compressed_offsets.len(),
                    bone_map.len() * per_bone,
                    bone_map.len()
                ));
            }
            if seq.key_format == KeyFormat::PerTrackCompression && !scale_offsets.data.is_empty() {
                stream = transfer_per_track_data(&stream, &compressed_offsets, &scale_offsets, num_frames)?;
            }
            let st = Stream {
                bytes: &stream,
                key_format: seq.key_format,
                tf: seq.translation_format,
                rf: seq.rotation_format,
                sf: seq.scale_format,
                num_frames,
                offsets: &compressed_offsets,
                scale_offsets: &scale_offsets,
            };
            for (i, &bone) in bone_map.iter().enumerate() {
                seq.tracks.push(st.decode_track(i, bone).map_err(|e| Error::Format(format!("{} track {i} (bone {bone}): {e}", seq.name)))?);
            }
        } else if !raw.is_empty() {
            seq.from_raw = true;
            for (i, (p, q, s)) in raw.into_iter().enumerate() {
                seq.tracks.push(Track { bone: bone_map.get(i).copied().unwrap_or(i as i32), pos: p, rot: q, scale: s, ..Default::default() });
            }
        }
        Ok(seq)
    }

    /// Playback rate in frames per second as UEViewer exported it (`NumFrames / SequenceLength * RateScale`).
    /// Note UE itself samples at `(NumFrames - 1) / SequenceLength`.
    pub fn frame_rate(&self) -> f32 {
        if self.length > 0.0 {
            self.num_frames as f32 / self.length * self.rate_scale
        } else {
            0.0
        }
    }

    /// Sampling rate of the key frames (frames per second of sequence time): `(NumFrames - 1) / SequenceLength`.
    pub fn sample_rate(&self) -> f32 {
        if self.length > 0.0 && self.num_frames > 1 {
            (self.num_frames - 1) as f32 / self.length
        } else {
            0.0
        }
    }

    pub fn is_additive(&self) -> bool {
        self.additive.kind != AdditiveAnimType::None
    }

    /// Whether the sequence extracts root motion (`bEnableRootMotion`).
    pub fn extract_root_motion(&self) -> bool {
        self.enable_root_motion
    }

    /// Track for a skeleton bone (first match, like `FindTrackForBoneIndex`).
    pub fn find_track(&self, bone: usize) -> Option<&Track> {
        self.tracks.iter().find(|t| t.bone == bone as i32)
    }

    /// Frame position (`0..=NumFrames-1`) of sequence time `t` seconds (clamped, no looping).
    pub fn time_to_frame(&self, t: f32) -> f32 {
        if self.num_frames <= 1 || self.length <= 0.0 {
            return 0.0;
        }
        (t / self.length * (self.num_frames - 1) as f32).clamp(0.0, (self.num_frames - 1) as f32)
    }

    /// Local-space pose of every skeleton bone at `time_seconds`: decoded tracks where present, the skeleton
    /// reference pose (non-additive) or the additive identity (additive) for bones/channels without keys.
    /// UE axes, centimetres, no retargeting.
    pub fn pose_at(&self, time_seconds: f32, skel: &SkeletonInfo) -> Vec<BoneTransform> {
        self.pose_at_frame(self.time_to_frame(time_seconds), skel)
    }

    /// [`pose_at`](Self::pose_at) for a looping player: `time_seconds` wraps modulo `SequenceLength`.
    pub fn pose_at_looped(&self, time_seconds: f32, skel: &SkeletonInfo) -> Vec<BoneTransform> {
        let t = if self.length > 0.0 { time_seconds.rem_euclid(self.length) } else { 0.0 };
        self.pose_at(t, skel)
    }

    /// Like [`pose_at`](Self::pose_at) but with a (fractional) frame position.
    pub fn pose_at_frame(&self, frame: f32, skel: &SkeletonInfo) -> Vec<BoneTransform> {
        self.pose_at_frame_with(frame, skel, RotationBlend::Nlerp)
    }

    /// [`pose_at_frame`](Self::pose_at_frame) with an explicit rotation blend.
    pub fn pose_at_frame_with(&self, frame: f32, skel: &SkeletonInfo, blend: RotationBlend) -> Vec<BoneTransform> {
        let additive = self.is_additive();
        let mut out: Vec<BoneTransform> = (0..skel.num_bones())
            .map(|i| if additive { BoneTransform::ADDITIVE_IDENTITY } else { skel.ref_pose.get(i).copied().unwrap_or_default() })
            .collect();
        let step = self.interpolation == Interpolation::Step;
        for t in &self.tracks {
            if t.bone < 0 || t.bone as usize >= out.len() {
                continue;
            }
            let s = t.sample_with(self.num_frames, frame, step, blend);
            let b = &mut out[t.bone as usize];
            if let Some(p) = s.pos {
                b.translation = p;
            }
            if let Some(q) = s.rot {
                b.rotation = q;
            }
            if let Some(sc) = s.scale {
                b.scale = sc;
            }
        }
        out
    }

    /// Pose of integer frame `i` (clamped).
    pub fn sample_frame(&self, i: usize, skel: &SkeletonInfo) -> Vec<BoneTransform> {
        self.pose_at_frame(i.min(self.num_frames.saturating_sub(1)) as f32, skel)
    }

    /// Local transforms of every frame for every skeleton bone: `[frame][bone]`.
    pub fn bake_frames(&self, skel: &SkeletonInfo) -> Vec<Vec<BoneTransform>> {
        (0..self.num_frames).map(|f| self.pose_at_frame(f as f32, skel)).collect()
    }

    /// Evaluate all float curves at time `t`: `(name, value)`.
    pub fn eval_curves(&self, t: f32) -> Vec<(&str, f32)> {
        self.curves.iter().map(|c| (c.name.as_str(), c.eval(t))).collect()
    }

    /// Additive info shortcut.
    pub fn additive_info(&self) -> &AdditiveInfo {
        &self.additive
    }
}

/// Apply a local-space additive pose on top of a base pose (`AAT_LocalSpaceBase`): rotations compose as
/// `additive * base` (normalised), translations add, scales combine as `base * (1 + additive)` (additive scale keys
/// are deltas, see [`BoneTransform::ADDITIVE_IDENTITY`]).
pub fn apply_additive_local(base: &[BoneTransform], additive: &[BoneTransform]) -> Vec<BoneTransform> {
    base.iter()
        .zip(additive)
        .map(|(b, a)| {
            let mut q = quat_mul(a.rotation, b.rotation);
            let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            if l > 1e-8 {
                for c in q.iter_mut() {
                    *c /= l;
                }
            }
            BoneTransform { rotation: q, translation: [b.translation[0] + a.translation[0], b.translation[1] + a.translation[1], b.translation[2] + a.translation[2]], scale: [b.scale[0] * (1.0 + a.scale[0]), b.scale[1] * (1.0 + a.scale[1]), b.scale[2] * (1.0 + a.scale[2])] }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evenly_spaced_keys() {
        // 5 frames, 5 keys: key i at frame i
        assert_eq!(find_keys(5, &[], 5, 0.0), (0, 1, 0.0));
        let (a, b, f) = find_keys(5, &[], 5, 2.5);
        assert_eq!((a, b), (2, 3));
        assert!((f - 0.5).abs() < 1e-6);
        assert_eq!(find_keys(5, &[], 5, 4.0), (4, 4, 0.0));
        assert_eq!(find_keys(5, &[], 5, 99.0), (4, 4, 0.0));
        assert_eq!(find_keys(1, &[], 5, 2.0), (0, 0, 0.0));
    }

    #[test]
    fn variable_key_lookup() {
        let times = [0u32, 4, 10, 20];
        assert_eq!(find_keys(4, &times, 21, 0.0), (0, 0, 0.0));
        let (a, b, f) = find_keys(4, &times, 21, 2.0);
        assert_eq!((a, b), (0, 1));
        assert!((f - 0.5).abs() < 1e-6);
        let (a, b, f) = find_keys(4, &times, 21, 4.0);
        assert_eq!((a, b), (1, 2));
        assert!(f.abs() < 1e-6);
        let (a, b, f) = find_keys(4, &times, 21, 15.0);
        assert_eq!((a, b), (2, 3));
        assert!((f - 0.5).abs() < 1e-6);
        assert_eq!(find_keys(4, &times, 21, 20.0), (3, 3, 0.0));
    }

    #[test]
    fn nlerp_shortest_arc() {
        let a = [0.0, 0.0, 0.0, 1.0];
        let b = [0.0, 0.0, 0.0, -1.0]; // same rotation, opposite sign
        let q = nlerp(a, b, 0.5);
        assert!((q[3].abs() - 1.0).abs() < 1e-6, "{q:?}"); // same rotation, no blow-up through zero
        let s = (0.5f32).sqrt();
        let q = nlerp([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, s, s], 1.0);
        assert!((q[2] - s).abs() < 1e-6);
    }

    #[test]
    fn track_sampling_lerps_and_steps() {
        let t = Track { bone: 0, pos: vec![[0.0; 3], [10.0, 0.0, 0.0], [10.0, 20.0, 0.0]], pos_times: vec![0, 2, 6], rot: vec![[0.0, 0.0, 0.0, 1.0]], ..Default::default() };
        let s = t.sample(7, 1.0, false);
        assert_eq!(s.pos.unwrap(), [5.0, 0.0, 0.0]);
        let s = t.sample(7, 4.0, false);
        assert_eq!(s.pos.unwrap(), [10.0, 10.0, 0.0]);
        let s = t.sample(7, 4.0, true);
        assert_eq!(s.pos.unwrap(), [10.0, 0.0, 0.0]);
        assert!(s.scale.is_none());
        assert_eq!(s.rot.unwrap(), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn curve_eval_linear() {
        let c = FloatCurve { name: "x".into(), keys: vec![CurveKey { time: 0.0, value: 0.0, ..Default::default() }, CurveKey { time: 2.0, value: 4.0, ..Default::default() }], ..Default::default() };
        assert_eq!(c.eval(1.0), 2.0);
        assert_eq!(c.eval(-1.0), 0.0);
        assert_eq!(c.eval(5.0), 4.0);
    }

    #[test]
    fn additive_apply() {
        let base = [BoneTransform { translation: [1.0, 2.0, 3.0], ..BoneTransform::IDENTITY }];
        let add = [BoneTransform { translation: [1.0, 1.0, 1.0], scale: [0.5, 0.0, -0.5], ..BoneTransform::ADDITIVE_IDENTITY }];
        let r = apply_additive_local(&base, &add);
        assert_eq!(r[0].translation, [2.0, 3.0, 4.0]);
        assert_eq!(r[0].scale, [1.5, 1.0, 0.5]);
        let same = apply_additive_local(&base, &[BoneTransform::ADDITIVE_IDENTITY]);
        assert_eq!(same[0], base[0]);
        assert!((r[0].rotation[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn per_track_stream_decode() {
        // one track, Fixed48 rotation (mask X|Y|Z|time), 2 keys at frames 0 and 4, translation identity-less (-1)
        let mut s: Vec<u8> = Vec::new();
        let hdr: u32 = ((CompressionFormat::Fixed48NoW as u32) << 28) | (0xF << 24) | 2;
        s.extend_from_slice(&hdr.to_le_bytes());
        for k in [[32767u16, 32767, 32767], [49150, 32767, 32767]] {
            for c in k {
                s.extend_from_slice(&c.to_le_bytes());
            }
        }
        // 4 + 12 = 16 bytes: already aligned; frame table u8 x2, pad
        s.extend_from_slice(&[0, 4, 0, 0]);
        let offsets = [-1, 0];
        let so = OffsetData { data: vec![], strip: 0 };
        let st = Stream { bytes: &s, key_format: KeyFormat::PerTrackCompression, tf: CompressionFormat::Identity, rf: CompressionFormat::Identity, sf: CompressionFormat::Identity, num_frames: 5, offsets: &offsets, scale_offsets: &so };
        let t = st.decode_track(0, 7).unwrap();
        assert_eq!(t.bone, 7);
        assert!(t.pos.is_empty()); // offset -1: no translation data
        assert_eq!(t.rot.len(), 2);
        assert_eq!(t.rot_times, vec![0, 4]);
        assert!((t.rot[1][0] - 16383.0 / 32767.0).abs() < 1e-5);
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[test]
    fn per_track_interval_and_float32_and_identity() {
        // translation: IntervalFixed32 with mask X|Z (5): (min,range) for x and z, 2 keys, no time table
        let mut s: Vec<u8> = Vec::new();
        let hdr: u32 = ((CompressionFormat::IntervalFixed32NoW as u32) << 28) | (0x5 << 24) | 2;
        s.extend_from_slice(&hdr.to_le_bytes());
        s.extend(f32s(&[10.0, 2.0, 30.0, 4.0])); // x: min 10 range 2, z: min 30 range 4
        for (x, y, z) in [(511u32, 0u32, 1023u32), (1022, 0, 2046)] {
            s.extend_from_slice(&(x | (y << 10) | (z << 21)).to_le_bytes());
        }
        let rot_off = s.len();
        // rotation: Float32NoW, 1 key, mask 7: all-zero bits -> (1/16, 1/16, 1/16)
        let hdr: u32 = ((CompressionFormat::Float32NoW as u32) << 28) | (0x7 << 24) | 1;
        s.extend_from_slice(&hdr.to_le_bytes());
        s.extend_from_slice(&0u32.to_le_bytes());
        let scale_off = s.len();
        // scale: Identity, 3 keys, no data
        let hdr: u32 = ((CompressionFormat::Identity as u32) << 28) | 3;
        s.extend_from_slice(&hdr.to_le_bytes());
        let offsets = [0, rot_off as i32];
        let so = OffsetData { data: vec![scale_off as i32], strip: 1 };
        let st = Stream { bytes: &s, key_format: KeyFormat::PerTrackCompression, tf: CompressionFormat::Identity, rf: CompressionFormat::Identity, sf: CompressionFormat::Identity, num_frames: 2, offsets: &offsets, scale_offsets: &so };
        let t = st.decode_track(0, 1).unwrap();
        assert_eq!(t.pos.len(), 2);
        assert!((t.pos[0][0] - 10.0).abs() < 1e-4 && t.pos[0][1] == 0.0 && (t.pos[0][2] - 30.0).abs() < 1e-4);
        assert!((t.pos[1][0] - 12.0).abs() < 1e-3 && (t.pos[1][2] - 34.0).abs() < 1e-3, "{:?}", t.pos[1]);
        assert_eq!(t.pos_format, Some((CompressionFormat::IntervalFixed32NoW, 5)));
        assert_eq!(t.rot.len(), 1);
        assert!((t.rot[0][0] - 0.0625).abs() < 1e-6);
        assert_eq!(t.scale, vec![[0.0; 3]; 3]);
        assert!(t.scale_times.is_empty());
    }

    #[test]
    fn constant_key_lerp_stream_with_interval_rotation() {
        // ConstantKeyLerp: translation 1 key (stored as 3 floats even though format is Fixed48), rotation 3 keys
        // IntervalFixed32NoW with 6 floats of mins/ranges
        let mut s: Vec<u8> = f32s(&[5.0, 6.0, 7.0]);
        let rot_off = s.len();
        s.extend(f32s(&[0.0, 0.0, 0.0, 0.5, 0.5, 0.5]));
        for _ in 0..3 {
            s.extend_from_slice(&((1023u32 << 21) | (1023 << 10) | 511).to_le_bytes());
        }
        let offsets = [0, 1, rot_off as i32, 3];
        let so = OffsetData { data: vec![], strip: 0 };
        let st = Stream { bytes: &s, key_format: KeyFormat::ConstantKeyLerp, tf: CompressionFormat::Fixed48NoW, rf: CompressionFormat::IntervalFixed32NoW, sf: CompressionFormat::Identity, num_frames: 3, offsets: &offsets, scale_offsets: &so };
        let t = st.decode_track(0, 0).unwrap();
        assert_eq!(t.pos, vec![[5.0, 6.0, 7.0]]);
        assert_eq!(t.rot, vec![[0.0, 0.0, 0.0, 1.0]; 3]);
        assert!(t.rot_times.is_empty() && t.pos_times.is_empty());
        // evenly spaced: halfway between frame 0 and 1 is still identity
        assert_eq!(t.sample(3, 0.5, false).rot.unwrap(), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn per_track_stream_relocation_with_scale_data() {
        // Wire order is translation, rotation, scale (per track) while the offsets address the relocated layout in
        // which scale data comes first: offsets T=16, R=32, S=0 (UE 4.16 "ByteSwapOut" quirk).
        let mut wire: Vec<u8> = Vec::new();
        let th: u32 = ((CompressionFormat::Float96NoW as u32) << 28) | (0x7 << 24) | 1;
        wire.extend_from_slice(&th.to_le_bytes());
        wire.extend(f32s(&[1.0, 2.0, 3.0])); // 16 bytes
        let rh: u32 = ((CompressionFormat::Fixed48NoW as u32) << 28) | (0x7 << 24) | 1;
        wire.extend_from_slice(&rh.to_le_bytes());
        for c in [32767u16, 32767, 49150] {
            wire.extend_from_slice(&c.to_le_bytes());
        }
        wire.extend_from_slice(&[0, 0]); // pad to 4 -> 12 bytes
        let sh: u32 = ((CompressionFormat::Float96NoW as u32) << 28) | (0x7 << 24) | 1;
        wire.extend_from_slice(&sh.to_le_bytes());
        wire.extend(f32s(&[1.5, 1.5, 1.5])); // 16 bytes
        assert_eq!(wire.len(), 44);
        let offsets = [16, 32];
        let so = OffsetData { data: vec![0], strip: 1 };
        let fixed = transfer_per_track_data(&wire, &offsets, &so, 10).unwrap();
        assert_eq!(fixed.len(), 44);
        let st = Stream { bytes: &fixed, key_format: KeyFormat::PerTrackCompression, tf: CompressionFormat::Identity, rf: CompressionFormat::Identity, sf: CompressionFormat::Identity, num_frames: 10, offsets: &offsets, scale_offsets: &so };
        let t = st.decode_track(0, 2).unwrap();
        assert_eq!(t.pos, vec![[1.0, 2.0, 3.0]]);
        assert!((t.rot[0][2] - 16383.0 / 32767.0).abs() < 1e-5);
        assert_eq!(t.scale, vec![[1.5, 1.5, 1.5]]);
    }
    #[test]
    fn const_and_variable_stream_decode() {
        // translation: 2 keys Float96 (variable), rotation: 1 key
        let mut s: Vec<u8> = Vec::new();
        for f in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            s.extend_from_slice(&f.to_le_bytes());
        }
        s.extend_from_slice(&[0, 9, 0, 0]); // frame table u8 x2 (+pad to 4)
        let rot_off = s.len();
        for f in [0.0f32, 0.0, 0.0] {
            s.extend_from_slice(&f.to_le_bytes());
        }
        let offsets = [0, 2, rot_off as i32, 1];
        let so = OffsetData { data: vec![], strip: 0 };
        let st = Stream { bytes: &s, key_format: KeyFormat::VariableKeyLerp, tf: CompressionFormat::Float96NoW, rf: CompressionFormat::Float96NoW, sf: CompressionFormat::Identity, num_frames: 10, offsets: &offsets, scale_offsets: &so };
        let t = st.decode_track(0, 3).unwrap();
        assert_eq!(t.pos, vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        assert_eq!(t.pos_times, vec![0, 9]);
        assert_eq!(t.rot, vec![[0.0, 0.0, 0.0, 1.0]]);
        let smp = t.sample(10, 4.5, false);
        assert_eq!(smp.pos.unwrap(), [2.5, 3.5, 4.5]);
    }
}

