//! PICKUP ITEM LIGHTING (fn-app): the weapon mesh of a world item drawn with its ORIGINAL material, exactly like the held firearm of `gun_orig.rs`: the cooked
//! `WeaponMat_Base` base-pass programs (SM5, `TGPUSkinVertexFactory<false>`, `FNoLightMapPolicy` + sky light) of every section's material instance, run by
//! fn-matshader, resolved by fn-render's gun pass with the scene's sun / cascade shadows / sky light / fog (`gun_resolve.wgsl`).
//!
//! * mesh: the item's `PickupSkeletalMesh` (the same mesh the hologram draws), LOD 0 GPU-skin streams as cooked; materials = the mesh's own slots (`B_Pickups` sets no
//!   override material on the item's mesh component; only the second, hologram component gets the MIDs of `Hologram Material`).
//! * pose: the Blueprint's `SkeletalMeshComponent` plays no animation, so every bone matrix is `RefBasesInvMatrix * RefPose = I` (`frame::bone_buffer` writes identity
//!   for bones without a palette slot, which is all of them here).
//! * transform: the same as the renderer model of `pickups_hook::frame`: actor location + yaw, the mesh moved so that its `Bounds.Origin` sits on the actor.
//! * used whenever the held gun runs its original material (the default; `--standin-gun-material` turns both off), so a dropped and a held weapon always share one
//!   lighting path. A mesh whose material cannot be built falls back to its renderer model (logged once per mesh, `draw`).
//!   `PickupStaticMesh` items (ammo, consumables, resources) run their own cooked materials through the `FLocalVertexFactory` permutation in the FX pass
//!   (`pickups_dxbc::Dxbc::draw_static_item`, STATIC ITEM 2026-10-05); they keep the renderer model only when those cannot be built.
//! Lighting (same as the held gun, fn-render gun_resolve.wgsl): sun + CSM / DF shadows received, CSM shadow cast (gun_material.rs `draw_shadow`), sky SH diffuse with
//! DFAO + SSAO, the sky light's specular (sky_reflection.rs). NOT reproduced: screen space reflections (Athena has no reflection captures to port).
use crate::gun_orig::OrigGuns;
use fn_matshader::gun::{FrameInput, GunModel};
use fn_matshader::view::ViewInput;
use fn_render::gun_material::GunFrame;
use fn_render::{CameraParams, Renderer};
use glam::{Mat4, Vec3};
use std::collections::HashMap;
use std::sync::Mutex;

/// The models are built in the held guns' scene (`OrigGuns::build_pickup`): one texture / material / pipeline cache for held and dropped weapons (FIRST USE).
struct State {
    /// Built models per mesh path (one per pickup drawn this frame: every model owns its `Primitive` buffer).
    models: HashMap<String, Result<Vec<GunModel>, String>>,
    /// This frame's draws: (mesh path, gun_to_world).
    pending: Vec<(String, Mat4, Option<[f32; 4]>)>,
    log: Vec<String>,
}

static HOST: Mutex<Option<State>> = Mutex::new(None);

/// A mesh-less fn-pose part: no joint slots, so every bone of the cooked sections draws with the identity matrix (the reference pose).
fn ref_pose_part(name: &str) -> fn_pose::Part {
    fn_pose::Part { name: name.to_owned(), tree: fn_pose::skel::BoneTree::new(vec![], vec![], vec![]), joints: vec![], inverse_bind: vec![], prims: vec![] }
}

fn build(orig: &mut OrigGuns, renderer: &mut Renderer, mesh_path: &str) -> Result<GunModel, String> {
    // FIRST USE: parsed, resolved and decoded by the pickups mesh worker (`pickups_hook::start_load`); on this thread only when no worker prepared it
    let prepared = match fn_matshader::gun::prepared_mesh(mesh_path) { Some(p) => p, None => fn_matshader::gun::prepare_skeletal(orig.set(), mesh_path)? };
    orig.build_pickup(renderer, &prepared.mesh, &ref_pose_part(&prepared.mesh.name), &prepared.materials)
}

/// Queues the original-material draw of a skeletal pickup this frame. `false` = not available (no original gun pass, a static mesh, or the material could not be
/// recovered): the caller draws its renderer model instead.
pub fn draw(renderer: &mut Renderer, orig: Option<&mut OrigGuns>, enabled: bool, mesh_path: &str, skeletal: bool, gun_to_world: Mat4) -> bool { draw_with(renderer, orig, enabled, mesh_path, skeletal, gun_to_world, None) }
pub fn draw_with(renderer: &mut Renderer, orig: Option<&mut OrigGuns>, enabled: bool, mesh_path: &str, skeletal: bool, gun_to_world: Mat4, black_hole: Option<[f32; 4]>) -> bool {
    if !enabled || !skeletal { return false; }
    let Some(orig) = orig else { return false };
    let mut g = HOST.lock().unwrap_or_else(|p| p.into_inner());
    let st = g.get_or_insert_with(|| State { models: HashMap::new(), pending: vec![], log: vec![] });
    let used = st.pending.iter().filter(|(m, _, _)| m == mesh_path).count();
    let have = match st.models.get(mesh_path) { Some(Ok(v)) => v.len(), Some(Err(_)) => return false, None => 0 };
    if used >= have {
        match build(orig, renderer, mesh_path) {
            Ok(m) => {
                if have == 0 { eprintln!("[pickups] {mesh_path}: original material ({} sections, {}) through the gun pass", m.sections.len(), m.sections.iter().map(|s| s.material.path.rsplit('.').next().unwrap_or("")).collect::<Vec<_>>().join(", ")); }
                st.models.entry(mesh_path.to_owned()).or_insert_with(|| Ok(vec![])).as_mut().map(|v| v.push(m)).ok();
            }
            Err(e) => {
                let line = format!("[pickups] {mesh_path}: original material NOT available, the renderer model is drawn: {e}");
                eprintln!("{line}"); st.log.push(line);
                if have == 0 { st.models.insert(mesh_path.to_owned(), Err(e)); }
                return false;
            }
        }
    }
    st.pending.push((mesh_path.to_owned(), gun_to_world, black_hole));
    true
}

/// Starts a frame's queue (called once per simulated frame before the pickups queue their draws).
pub fn begin_frame() { if let Some(st) = HOST.lock().unwrap_or_else(|p| p.into_inner()).as_mut() { st.pending.clear(); } }

/// Appends the queued draws to the renderer's gun frame (call after `OrigGuns::submit`, which sets the frame). The queue is kept: a frame can be drawn more than once
/// (screenshot captures re-draw it).
pub fn submit(renderer: &mut Renderer, camera: &CameraParams, time: f32) {
    let mut g = HOST.lock().unwrap_or_else(|p| p.into_inner());
    let Some(st) = g.as_mut() else { return };
    if st.pending.is_empty() { return; }
    let (tx, ty) = camera.tan_half();
    let view = ViewInput { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time };
    let mut frame = GunFrame::default();
    let mut next: HashMap<&str, usize> = HashMap::new();
    let fwd = camera.forward.normalize_or_zero();
    let right = fwd.cross(camera.up).normalize_or_zero();
    let up = right.cross(fwd).normalize_or_zero();
    let (kx, ky) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
    for (path, to_world, black_hole) in st.pending.iter() {
        let Some(Ok(models)) = st.models.get(path) else { continue };
        let i = next.entry(path.as_str()).or_insert(0);
        let Some(m) = models.get(*i) else { continue };
        *i += 1;
        let c = to_world.transform_point3(Vec3::from(fn_mesh::gltf::ue_to_gltf_position(m.bounds.origin))) - camera.eye;
        let r = m.bounds.radius * 0.01 * to_world.x_axis.truncate().length().max(to_world.y_axis.truncate().length()).max(to_world.z_axis.truncate().length()) + PICKUP_CULL_MARGIN_M;
        let (z, x, y) = (c.dot(fwd), c.dot(right), c.dot(up));
        if z < -r || x.abs() > z * tx + r * kx || y.abs() > z * ty + r * ky { continue; }
        let to_world = *to_world;
        let black_hole = *black_hole;
        let vectors: Vec<(&str, [f32; 4])> = black_hole.map(|v| (BLACK_HOLE_PARAM, v)).into_iter().collect();
        let input = FrameInput { view, gun_to_world: to_world, skin: &[], sky_radiance: fn_render::lighting::sky_radiance(), time, vectors: &vectors };
        m.frame(&input, &mut frame);
    }
    renderer.gun().append_frame(frame);
}

const PICKUP_CULL_MARGIN_M: f32 = 5.0;

pub const BLACK_HOLE_PARAM: &str = "WorldPostionOfBlackHole";

/// The renderer model matrix of a pickup (`pickups_hook::frame`): actor location + yaw, the mesh's bounds origin (glTF metres) moved onto the actor.
pub fn to_world(pos: Vec3, yaw: f32, origin_cm: [f32; 3]) -> Mat4 {
    Mat4::from_translation(pos) * Mat4::from_rotation_y(yaw) * Mat4::from_translation(-Vec3::from(fn_mesh::gltf::ue_to_gltf_position(origin_cm)))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// `UMaterial::AppendReferencedTextures` order (fn-fx `collect_textures`): the texture-parameter indices of the cooked `WeaponMat_Base` uniform expressions land on
    /// their own default textures (SpecularMasks 6 -> T_HammerShotGun_S, Normals 7 -> _N, Diffuse 8 -> _D, EmissiveTexture 9 -> T_Black). Needs FN_TEST_ROOT.
    #[test] fn weapon_material_texture_slots_follow_the_engine_order() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let set = fn_pak::PakSet::open_install(&fn_pak::find_paks_dir(&root).expect("paks")).unwrap();
        let m = fn_matshader::material::ResolvedMaterial::load(&set, "/Game/Weapons/FORT_Rifles/Materials/MI_FORT_Rifles_MachineGun_1.MI_FORT_Rifles_MachineGun_1").unwrap();
        let r = &m.fx.referenced_textures;
        assert!(r[6].ends_with("T_HammerShotGun_S") && r[7].ends_with("T_HammerShotGun_N") && r[8].ends_with("T_HammerShotGun_D") && r[9].ends_with("T_Black"), "{r:?}");
        let slots: Vec<String> = (0..m.map().uniform.textures2d.len()).filter_map(|k| m.texture_slot(k).map(|s| s.0)).collect();
        assert!(slots.iter().any(|s| s.ends_with("T_Black")) && !slots.iter().any(|s| s.contains("ShieldPowerAnim")), "{slots:?}");
        assert!(slots.iter().any(|s| s.ends_with("T_MachineGun_1_D")) && slots.iter().any(|s| s.ends_with("T_MachineGun_1_N")) && slots.iter().any(|s| s.ends_with("T_MachineGun_1_S")), "{slots:?}");
    }
    /// The original-material transform puts the bounds origin on the actor, the same as the renderer model built from the re-centred vertices.
    #[test] fn the_bounds_origin_lands_on_the_actor() {
        let (pos, yaw, origin) = (Vec3::new(1.0, 0.8, -3.0), 0.7f32, [12.0, -40.0, 5.0]);
        let m = to_world(pos, yaw, origin);
        let o = fn_mesh::gltf::ue_to_gltf_position(origin);
        assert!((m.transform_point3(Vec3::from(o)) - pos).length() < 1e-5);
        let v = [30.0, 2.0, -7.0];
        let g = Vec3::from(fn_mesh::gltf::ue_to_gltf_position(v));
        let model = Mat4::from_translation(pos) * Mat4::from_rotation_y(yaw);
        assert!((m.transform_point3(g) - model.transform_point3(g - Vec3::from(o))).length() < 1e-5);
    }
}
