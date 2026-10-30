//! Recovered Fortnite 1.8 (UE 4.16) player AnimGraph, free of any renderer or clip decoder.
//!
//! * `graph`    (old anim_graph.rs + generated anim_data.rs / anim_core.rs): state machines, BlendSpaces, additive jump machine, equip
//!              montage and `AnimState::plan`, which builds the pose-node tree ([`Node`]) for one tick.
//! * [`skeleton`]: [`Skeleton`] + the pose evaluator (the GL-free part of the old `assets::Model`) and the [`ClipSource`] /
//!              [`ClipLibrary`] traits through which any clip decoder (the old md5anim loader via `fn-legacy`, the `fn-anim` decoder
//!              crate) supplies animation data. Pure pose math lives here for every consumer (`fn-pose`, the renderer): local transforms,
//!              mesh-space additive with the tracked-bone mask ([`MeshAdditiveMode`]), HeadTrack, TwoBoneIk, hand retarget.
//! * [`skin`]:  renderer-free CPU skinning of a pose ([`SkinnedMesh::pose`] = the old `Model::apply_pose`) plus the face-rigidity diagnostics.
mod graph;
pub mod enhanced_anim;
pub mod anim_dynamics; // PART ANIM HOOK: AnimNode_AnimDynamics (BR character part AnimBPs)
pub mod skeleton;
pub mod skin;

pub use graph::*;
pub use skeleton::{
    grip_axis_distance, grip_center, handle_axis_xz, sample_blended_curve, solve_two_bone_ik, take_missing_clips, two_bone, BoneXform, ClipLibrary, ClipSource,
    Pose, Skeleton, MeshAdditiveMode, HAND_IK_STATS, PICK_SOCKET_YAW,
};
pub use skin::{PosedMesh, SkinPrimitive, SkinnedMesh};
pub mod weapon_assets; // WEAPON ASSETS HOOK: generated per-weapon montage / camera / muzzle / sound bindings of every BR ranged WID (gen-anim-core)
