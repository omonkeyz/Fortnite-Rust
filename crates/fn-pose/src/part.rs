//! Character parts: a skinned mesh's skeleton (the part's glTF node hierarchy, bone names lower-cased, **node 0 is the mesh node**), the skin
//! joint list + inverse bind matrices and, optionally, the vertex data for CPU skinning (verification / debugging).
//!
//! In 1.8 the body (`SK_M_Med_Soldier_04`), the head (`SK_M_MED_Head_01_ATH`) and any further outfit mesh are *followers* of the master mesh
//! `Fortnite_M_Avg_Player` and share its skeleton. Clips and the AnimBP's TwoBoneIK / HandIKRetargeting nodes work on the master's seven IK
//! bones (`ik_foot_*`, `ik_hand_*`), which a part mesh's own skeleton lacks (it only has skinned bones): [`Part::with_master_ik_bones`]
//! appends them exactly as the old runtime does at load (bind transforms from the master, no skin weights).
//!
//! Loading goes through glTF, the format the mesh exporter writes: either a `.gltf` file ([`Part::from_gltf_path`], the old exports) or a
//! decoded [`fn_mesh::SkeletalMesh`] ([`Part::from_skeletal_mesh`], which runs `fn_mesh::gltf`'s exporter in memory so the node order, bone
//! scale bake, rotation conjugation and inverse bind matrices are the exporter's, bit for bit).
use crate::skel::{BoneTree, BoneXform};
use glam::{Mat4, Quat, Vec3};
use std::path::Path;

/// One skinned primitive (a mesh section): glTF-space positions / normals, 4 joints (skin joint slots) and weights per vertex.
#[derive(Clone, Debug, Default)]
pub struct SkinPrim {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub material: String,
    /// COLOR_0 (RGBA) of the glTF primitive, empty when the mesh has no vertex colours (BR CHARACTER MATERIAL HOOK: the eyeshadow mask is painted in R).
    pub colors: Vec<[f32; 4]>,
    pub tangents: Vec<[f32; 4]>,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub tree: BoneTree,
    /// Skeleton bone index of every skin joint slot (`JOINTS_0` values index this list; so does the GPU bone palette).
    pub joints: Vec<usize>,
    /// Inverse bind matrix per joint slot.
    pub inverse_bind: Vec<Mat4>,
    pub prims: Vec<SkinPrim>,
}

impl Part {
    /// Builds a part from a parsed glTF document and its buffers (`mesh` data is read when present).
    pub fn from_gltf(name: &str, doc: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Result<Part, String> {
        let bad = |what: &str| format!("cannot load model {name}: {what}");
        let count = doc.nodes().count();
        let mut parents = vec![None; count];
        let mut names = vec![String::new(); count];
        let mut local = vec![(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE); count];
        for node in doc.nodes() {
            let i = node.index();
            names[i] = node.name().unwrap_or("").to_lowercase();
            let (t, r, s) = node.transform().decomposed();
            local[i] = (Vec3::from(t), Quat::from_array(r), Vec3::from(s));
            for child in node.children() { parents[child.index()] = Some(i); }
        }
        let skin = doc.skins().next().ok_or_else(|| bad("no skin"))?;
        let joints: Vec<usize> = skin.joints().map(|n| n.index()).collect();
        let reader = |b: gltf::Buffer<'_>| buffers.get(b.index()).map(|d| &d.0[..]);
        let inverse_bind: Vec<Mat4> = skin.reader(reader).read_inverse_bind_matrices().ok_or_else(|| bad("no inverse bind matrices"))?.map(|m| Mat4::from_cols_array_2d(&m)).collect();
        let mut prims = vec![];
        for mesh in doc.meshes() {
            for p in mesh.primitives() {
                let r = p.reader(reader);
                let (Some(pos), Some(nrm), Some(uv), Some(j), Some(w), Some(ix)) =
                    (r.read_positions(), r.read_normals(), r.read_tex_coords(0), r.read_joints(0), r.read_weights(0), r.read_indices()) else { return Err(bad("a mesh primitive lacks positions/normals/uvs/joints/weights/indices")) };
                prims.push(SkinPrim { positions: pos.collect(), normals: nrm.collect(), uvs: uv.into_f32().collect(), joints: j.into_u16().collect(), weights: w.into_f32().collect(), indices: ix.into_u32().collect(), material: p.material().name().unwrap_or("").to_owned(), colors: r.read_colors(0).map(|c| c.into_rgba_f32().collect()).unwrap_or_default(), tangents: r.read_tangents().map(|t| t.collect()).unwrap_or_default() });
            }
        }
        Ok(Part { name: name.to_owned(), tree: BoneTree::new(names, parents, local), joints, inverse_bind, prims })
    }

    /// From a `.gltf` file written by the old UEViewer exporter (or `fn-export`).
    pub fn from_gltf_path(path: &Path) -> Result<Part, String> {
        let (doc, buffers, _) = gltf::import(path).map_err(|e| format!("cannot load model {}: {e}", path.display()))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Part::from_gltf(&name, &doc, &buffers)
    }

    /// From a decoded skeletal mesh (LOD `lod`), through the in-memory glTF export of `fn-mesh`.
    pub fn from_skeletal_mesh(name: &str, mesh: &fn_mesh::SkeletalMesh, lod: usize) -> Result<Part, String> {
        let (json, bin) = fn_mesh::gltf::skeletal_mesh_lod_to_gltf(mesh, lod, name).map_err(|e| format!("{name}: {e}"))?;
        let g = gltf::Gltf::from_slice(json.as_bytes()).map_err(|e| format!("{name}: {e}"))?;
        Part::from_gltf(name, &g.document, &[gltf::buffer::Data(bin)])
    }

    /// Appends the master skeleton's missing bones (the IK bones) when this part has a pelvis but no `ik_hand_l` (the old `Model::load` rule).
    /// Returns the number of bones added.
    pub fn with_master_ik_bones(mut self, master: &BoneTree) -> Part { self.append_master_ik_bones(master); self }
    pub fn append_master_ik_bones(&mut self, master: &BoneTree) -> usize {
        if self.tree.bone_index("pelvis").is_some() && self.tree.bone_index("ik_hand_l").is_none() { self.tree.append_missing_from(master) } else { 0 }
    }

    /// Number of matrices of the GPU bone palette (`joints.len()`).
    pub fn palette_len(&self) -> usize { self.joints.len() }

    /// Skin matrices `globals[joint] * inverse_bind` in joint-slot order (what `vs_skinned` indexes with `JOINTS_0`).
    pub fn skin_matrices(&self, globals: &[Mat4]) -> Vec<Mat4> {
        let mut v = Vec::with_capacity(self.joints.len());
        self.skin_matrices_into(globals, &mut v);
        v
    }
    pub fn skin_matrices_into(&self, globals: &[Mat4], out: &mut Vec<Mat4>) {
        out.clear();
        out.extend(self.joints.iter().zip(&self.inverse_bind).map(|(j, ib)| globals[*j] * *ib));
    }

    /// CPU skinning exactly as the old `Model::skin_and_cache`: per primitive, positions and normals skinned with the 4 influences.
    pub fn skin_cpu(&self, skin: &[Mat4]) -> Vec<(Vec<Vec3>, Vec<Vec3>)> {
        self.prims.iter().map(|prim| {
            prim.positions.iter().enumerate().map(|(i, p)| {
                let (mut pos, mut normal) = (Vec3::ZERO, Vec3::ZERO);
                for k in 0..4 {
                    let m = skin[prim.joints[i][k] as usize];
                    pos += m.transform_point3(Vec3::from(*p)) * prim.weights[i][k];
                    normal += m.transform_vector3(Vec3::from(prim.normals[i])) * prim.weights[i][k];
                }
                (pos, normal.normalize_or_zero())
            }).unzip()
        }).collect()
    }

    /// Names of every bone some vertex is skinned to (weight > 0).
    pub fn skinned_bone_names(&self) -> Vec<String> {
        let mut used = std::collections::BTreeSet::new();
        for p in &self.prims { for i in 0..p.positions.len() { for k in 0..4 { if p.weights[i][k] > 0. { used.insert(self.joints[p.joints[i][k] as usize]); } } } }
        used.into_iter().map(|b| self.tree.names[b].clone()).collect()
    }
}

/// Local pose of the bind skeleton (convenience for tools).
pub fn bind_pose(p: &Part) -> Vec<BoneXform> { p.tree.bind_pose() }

#[cfg(test)]
mod winding_tests {
    use super::*;
    /// Winding of the port's skinned vertex data (render space: `fn_mesh::gltf::ue_to_gltf_position`, a y/z swap = a mirror, indices kept in the cooked order):
    /// for the real 1.8 meshes, how many triangles wind counter-clockwise about their outward vertex normals (right-handed cross product). Proves which
    /// `wgpu::FrontFace` the renderer's skinned pipelines need. Result: render space CCW (the y/z swap mirrors the cooked CW-about-TangentZ data), and the
    /// light view (`look_at_rh` + `orthographic_rh`, lighting.rs) is a proper right-handed map, so the skinned shadow pipeline's default `FrontFace::Ccw` +
    /// cull Back keeps the lit faces (correct). The gun pass draws the cooked UE-space data through a left-handed view onto the same image: also CCW on
    /// screen (UE `CM_CW` culls CW, D3D11State.cpp:41/:200), see `fn_render::gun_material::GunCull`. Needs FN_TEST_ROOT.
    #[test] fn skinned_vertex_data_winds_ccw_about_the_normal() {
        let Some(root) = fn_live::var_os("FN_TEST_ROOT").map(std::path::PathBuf::from) else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
        let set = fn_pak::PakSet::open_install(&fn_pak::find_paks_dir(&root).expect("paks")).unwrap();
        let src = crate::ClipSourceDir::Pak(&set);
        for pkg in [crate::loader::BODY_PKG, crate::loader::HEAD_PKG, crate::loader::PICK_PKG] {
            let p = crate::loader::package(&src, pkg).unwrap();
            let mesh = fn_mesh::SkeletalMesh::parse(&p).unwrap();
            let part = Part::from_skeletal_mesh("w", &mesh, 0).unwrap();
            let (mut ccw, mut cw) = (0usize, 0usize);
            for pr in &part.prims {
                for t in pr.indices.chunks_exact(3) {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(pr.positions[i as usize]));
                    let n: Vec3 = [t[0], t[1], t[2]].iter().map(|&i| Vec3::from(pr.normals[i as usize])).sum();
                    let d = (b - a).cross(c - a).dot(n);
                    if d > 0. { ccw += 1 } else if d < 0. { cw += 1 }
                }
            }
            // the same triangles in the cooked UE space (left handed, cm): raw positions, TangentZ (packed u8 normal), cooked index order
            let lod = &mesh.lods[0];
            let (mut ue_ccw, mut ue_cw) = (0usize, 0usize);
            for t in lod.indices.chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(lod.positions[i as usize]));
                let n: Vec3 = [t[0], t[1], t[2]].iter().map(|&i| { let z = lod.tangent_z[i as usize]; Vec3::new(z[0] as f32 / 127.5 - 1., z[1] as f32 / 127.5 - 1., z[2] as f32 / 127.5 - 1.) }).sum();
                let d = (b - a).cross(c - a).dot(n);
                if d > 0. { ue_ccw += 1 } else if d < 0. { ue_cw += 1 }
            }
            eprintln!("{pkg}: render space {ccw} CCW / {cw} CW about the normal; cooked UE space {ue_ccw} CCW / {ue_cw} CW");
            assert!(ccw > 20 * cw, "{pkg}: render-space data must wind CCW about the outward normal (ccw {ccw}, cw {cw})");
            assert!(ue_cw > 20 * ue_ccw, "{pkg}: the cooked UE-space data winds CW about TangentZ (ccw {ue_ccw}, cw {ue_cw})");
        }
    }
}
