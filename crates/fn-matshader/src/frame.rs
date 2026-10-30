//! Per-frame constant data of the original gun programs: the `Primitive` and `PrimitiveFade` uniform buffers, the GPU-skin vertex factory's loose parameters and the bone
//! matrices, all in Unreal conventions (cm, Z up, row-vector matrices) derived from the renderer's matrices (metres, Y up).
//!
//! Spaces: `C` maps UE mesh space (cm) to the port's glTF mesh space (the renderer's model space): `(x, y, z) -> (x, z, y) * 0.01`. The renderer draws the gun with a
//! matrix `G` (glTF mesh space -> port world), so the original programs get
//! * `LocalToWorld = C^-1 * G * C ... ` expressed from UE mesh space to UE world: `W * G * C` with `W = C^-1` (port world -> UE world, `(x, y, z) -> (x, z, y) * 100`),
//! * bone matrices `C^-1 * M_gltf * C` (the renderer's skin matrices act on glTF-space positions).
use glam::{Mat4, Vec4}; // (Vec3 only in the tests since the Primitive builder moved to fn_render::primitive_ub)

/// UE mesh space (cm, Z up) -> port glTF space (m, Y up): `(x, y, z) -> (x, z, y) * 0.01`.
pub fn ue_to_port() -> Mat4 { Mat4::from_cols(Vec4::new(0.01, 0.0, 0.0, 0.0), Vec4::new(0.0, 0.0, 0.01, 0.0), Vec4::new(0.0, 0.01, 0.0, 0.0), Vec4::W) }
/// Port (m, Y up) -> UE (cm, Z up): `(x, y, z) -> (x, z, y) * 100`.
pub fn port_to_ue() -> Mat4 { Mat4::from_cols(Vec4::new(100.0, 0.0, 0.0, 0.0), Vec4::new(0.0, 0.0, 100.0, 0.0), Vec4::new(0.0, 100.0, 0.0, 0.0), Vec4::W) }

/// `LocalToWorld` of the original programs (UE mesh space -> UE world) from the renderer's model matrix `g` (port glTF mesh space -> port world).
pub fn local_to_world_ue(g: Mat4) -> Mat4 { port_to_ue() * g * ue_to_port() }

fn rows3(m: &Mat4) -> [[f32; 4]; 3] { std::array::from_fn(|i| { let c = [m.x_axis, m.y_axis, m.z_axis, m.w_axis]; [c[0][i], c[1][i], c[2][i], c[3][i]] }) }

/// 18 float4s: the shared `Primitive` builder (`fn_render::primitive_ub`, UE4.16 `GetPrimitiveUniformShaderParameters`); this module's old copy (16 float4s, orientation
/// in 9.xyz, slots 11..13 / 15..17 zero) was replaced by it.
pub const PRIMITIVE_FLOAT4S: usize = fn_render::primitive_ub::PRIMITIVE_FLOAT4S;
/// The `Primitive` uniform buffer of a mesh component from its cooked local bounds (`UStaticMesh::GetBounds` / the skeletal mesh bounds), actor at the component origin.
pub fn primitive_buffer_bounds(l2w: &Mat4, bounds: &fn_render::primitive_ub::BoxSphereBounds) -> Vec<[f32; 4]> {
    fn_render::primitive_ub::mesh_primitive(l2w, bounds, l2w.w_axis.truncate().to_array(), &fn_render::primitive_ub::PrimitiveFlags::default())
}
/// The `Primitive` buffer of a mesh component from the mesh's COOKED bounds (`FBoxSphereBounds`: origin, box extent, sphere radius, e.g. `USkeletalMesh::GetBounds` /
/// `UStaticMesh::GetBounds`). The old sphere-only variant built the box from the bounding sphere (a cube of the radius), which is not the cooked box
/// (`ObjectBounds` 9.xyz and `LocalObjectBounds` 15 / 16 read it).
pub fn primitive_buffer(l2w: &Mat4, bounds: &fn_render::primitive_ub::BoxSphereBounds) -> Vec<[f32; 4]> { primitive_buffer_bounds(l2w, bounds) }

/// `PrimitiveFade`: `FadeTimeScaleBias` = (0, 1): the primitive is not fading (the program computes `saturate(time * x + y)` = 1 = fully present).
pub fn fade_buffer() -> Vec<[f32; 4]> { vec![[0.0, 1.0, 0.0, 0.0]] }

pub const VF_FLOAT4S: usize = 9;
/// The GPU-skin vertex factory's loose parameters (the unnamed buffer 0 of the vertex program): 2..5 `PreviousLocalToWorld` (rows + translation, set to the current
/// transform: only the VELOCITY output reads them), 6 `MeshOrigin`, 7 `MeshExtension` (xyz) + the previous-bone-matrices flag (`.w` = 0: no motion blur data), 8.x motion blur scale 0.
pub fn vf_buffer(l2w: &Mat4, mesh_origin: [f32; 3], mesh_extension: [f32; 3]) -> Vec<[f32; 4]> {
    let mut v = vec![[0f32; 4]; VF_FLOAT4S];
    let cols = [l2w.x_axis, l2w.y_axis, l2w.z_axis, l2w.w_axis];
    for i in 0..4 { v[2 + i] = [cols[i].x, cols[i].y, cols[i].z, if i == 3 { 1.0 } else { 0.0 }]; }
    v[6] = [mesh_origin[0], mesh_origin[1], mesh_origin[2], 0.0];
    v[7] = [mesh_extension[0], mesh_extension[1], mesh_extension[2], 0.0];
    v
}

/// VELOCITY (2026-10-05): [`vf_buffer`] with the base-pass VS velocity inputs as UE sets them for a movable GPU-skinned component: 2..5 `PreviousLocalToWorld` =
/// `prev_l2w` (TBasePassVertexShaderPolicyParamType::SetMesh, BasePassRendering.inl:31-66), 7.w `PerBoneMotionBlur` = true (a bool written as uint 1) when the program
/// binds `PreviousBoneMatrices` (GPUSkinVertexFactory.cpp:548-564), 8.x `SkipOutputVelocity` 0 (the translated VS writes `VELOCITY_PREV_POS.z = 1 - cb0[8].x`).
pub fn vf_buffer_velocity(l2w: &Mat4, prev_l2w: &Mat4, per_bone_motion_blur: bool, mesh_origin: [f32; 3], mesh_extension: [f32; 3]) -> Vec<[f32; 4]> {
    let mut v = vf_buffer(l2w, mesh_origin, mesh_extension);
    let cols = [prev_l2w.x_axis, prev_l2w.y_axis, prev_l2w.z_axis, prev_l2w.w_axis];
    for i in 0..4 { v[2 + i] = [cols[i].x, cols[i].y, cols[i].z, if i == 3 { 1.0 } else { 0.0 }]; }
    v[7][3] = if per_bone_motion_blur { f32::from_bits(1) } else { 0.0 };
    v[8][0] = 0.0;
    v
}

/// Bone matrices of one section in the vertex factory's `float3x4` rows (`BoneMatrices[3 * b + r]`, column-vector convention `p' = M p`), indexed by the SECTION-LOCAL
/// bone index. `skin` = the renderer's skin matrices (glTF space, joint-slot order), `slots[b]` the slot of section bone `b`; missing slots stay identity.
pub fn bone_buffer(skin: &[Mat4], slots: &[Option<usize>], vec4s: usize) -> Vec<[f32; 4]> {
    let (to_port, to_ue) = (ue_to_port(), port_to_ue());
    let mut out = vec![[0f32; 4]; vec4s];
    for b in 0..vec4s / 3 {
        let m = match slots.get(b).copied().flatten().and_then(|s| skin.get(s)) { Some(m) => to_ue * *m * to_port, None => Mat4::IDENTITY };
        let r = rows3(&m);
        for k in 0..3 { out[3 * b + k] = r[k]; }
    }
    out
}

pub fn bytes(v: &[[f32; 4]]) -> Vec<u8> { let mut b = Vec::with_capacity(v.len() * 16); for r in v { for f in r { b.extend_from_slice(&f.to_le_bytes()); } } b }

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};
    /// The original program's skinned, transformed position equals what the renderer's `vs_skinned` computes: `G * M_gltf * p_gltf`.
    #[test] fn original_program_math_equals_the_renderer_transform() {
        let g = Mat4::from_rotation_translation(Quat::from_rotation_y(0.7) * Quat::from_rotation_x(0.2), Vec3::new(3., 1., -2.));
        let m = Mat4::from_rotation_translation(Quat::from_rotation_z(0.4), Vec3::new(0.01, 0.02, -0.03));
        let p_ue = Vec3::new(12.0, -7.5, 30.0);
        let p_gltf = ue_to_port().transform_point3(p_ue);
        let want = g.transform_point3(m.transform_point3(p_gltf));
        // program: bone rows (p' = M p), then row-vector LocalToWorld, then back to port metres
        let rows = bone_buffer(&[m], &[Some(0)], 3);
        let skinned = Vec3::new(
            rows[0][0] * p_ue.x + rows[0][1] * p_ue.y + rows[0][2] * p_ue.z + rows[0][3],
            rows[1][0] * p_ue.x + rows[1][1] * p_ue.y + rows[1][2] * p_ue.z + rows[1][3],
            rows[2][0] * p_ue.x + rows[2][1] * p_ue.y + rows[2][2] * p_ue.z + rows[2][3]);
        let prim = primitive_buffer(&local_to_world_ue(g), &fn_render::primitive_ub::BoxSphereBounds { origin: [0.; 3], extent: [1.; 3], radius: 1. });
        let w = Vec3::new(skinned.x, skinned.y, skinned.z);
        let world_ue = w.x * Vec3::new(prim[0][0], prim[0][1], prim[0][2]) + w.y * Vec3::new(prim[1][0], prim[1][1], prim[1][2]) + w.z * Vec3::new(prim[2][0], prim[2][1], prim[2][2]) + Vec3::new(prim[3][0], prim[3][1], prim[3][2]);
        let got = Vec3::new(world_ue.x, world_ue.z, world_ue.y) * 0.01;
        assert!((got - want).length() < 1e-4, "{got:?} vs {want:?}");
        assert_eq!(prim[9][3], 1.0, "S G S keeps the handedness");
        for k in 0..3 { assert!((prim[14][k] - 1.0).abs() < 1e-4); }
    }
}
