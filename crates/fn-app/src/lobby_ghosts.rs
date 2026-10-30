use super::*;
use fn_anim::sequence::AnimSequence;
use fn_anim::skeleton::{BoneTransform, SkeletonInfo};
use glam::{Mat4, Quat};

pub struct GhostMesh {
    pub name: String,
    mesh: FxMeshId,
    sections: Vec<(usize, u32, u32)>,
    bone_maps: Vec<Vec<u16>>,
    bones: Vec<FxBufId>,
    vs_globals: FxBufId,
    parents: Vec<i32>,
    ref_local: Vec<BoneTransform>,
    inv_ref: Vec<Mat4>,
    bounds: fn_render::primitive_ub::BoxSphereBounds,
    pub extra_influences: bool,
    pub ref_names: Vec<String>,
    skeleton: SkeletonInfo,
    anim_map: Vec<Option<usize>>,
}

fn xf_mat(b: &BoneTransform) -> Mat4 {
    Mat4::from_scale_rotation_translation(Vec3::from(b.scale), Quat::from_array(b.rotation), Vec3::from(b.translation))
}

fn component_space(parents: &[i32], local: &[BoneTransform]) -> Vec<Mat4> {
    let mut out: Vec<Mat4> = Vec::with_capacity(local.len());
    for (i, l) in local.iter().enumerate() {
        let m = xf_mat(l);
        let p = parents[i];
        out.push(if p >= 0 && (p as usize) < i { out[p as usize] * m } else { m });
    }
    out
}

fn vertex_bytes(lod: &fn_mesh::SkeletalLod) -> Vec<u8> {
    let n = lod.num_vertices;
    let stride = if lod.extra_bone_influences { fn_fx::gpu::SKIN_EXTRA_VERTEX_STRIDE } else { fn_matshader::mesh::STRIDE } as usize;
    let uv0 = lod.uvs.first().filter(|u| u.len() == n);
    let mut bytes = vec![0u8; n * stride];
    for i in 0..n {
        let o = i * stride;
        for k in 0..3 { bytes[o + 4 * k..o + 4 * k + 4].copy_from_slice(&lod.positions[i][k].to_le_bytes()); }
        bytes[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
        bytes[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
        let inf = &lod.influences[i];
        let (bones, weights) = ([inf.bones[0], inf.bones[1], inf.bones[2], inf.bones[3]], [inf.weights[0], inf.weights[1], inf.weights[2], inf.weights[3]]);
        bytes[o + 20..o + 24].copy_from_slice(&bones);
        bytes[o + 24..o + 28].copy_from_slice(&weights);
        if lod.extra_bone_influences {
            bytes[o + 40..o + 44].copy_from_slice(&[inf.bones[4], inf.bones[5], inf.bones[6], inf.bones[7]]);
            bytes[o + 44..o + 48].copy_from_slice(&[inf.weights[4], inf.weights[5], inf.weights[6], inf.weights[7]]);
        }
        let uv = uv0.map(|u| u[i]).unwrap_or([0.0; 2]);
        bytes[o + 28..o + 32].copy_from_slice(&uv[0].to_le_bytes());
        bytes[o + 32..o + 36].copy_from_slice(&uv[1].to_le_bytes());
        bytes[o + 36..o + 40].copy_from_slice(&lod.colors.get(i).copied().unwrap_or([255; 4]));
    }
    bytes
}

static MATERIALS: std::sync::Mutex<Option<HashMap<String, Material>>> = std::sync::Mutex::new(None);
pub fn prewarm_material(set: &PakSet, path: &str) -> Result<Material, String> {
    if let Some(m) = MATERIALS.lock().ok().and_then(|g| g.as_ref().and_then(|c| c.get(path).cloned())) { return Ok(m); }
    let m = Material::load(set, path)?;
    if let Ok(mut g) = MATERIALS.lock() { g.get_or_insert_with(HashMap::new).insert(path.to_owned(), m.clone()); }
    Ok(m)
}

fn package(set: &PakSet, long: &str) -> Result<fn_ueio::Package, String> {
    let (a, x, b) = fn_fx::data::package_files(set, long)?;
    fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))
}

impl Dxbc {
    pub fn ghost_mesh(&mut self, fx: &mut FxRenderer, mesh_path: &str) -> Result<GhostMesh, String> {
        let long = fn_fx::data::long_package(mesh_path);
        let sm = fn_mesh::SkeletalMesh::parse(&package(&self.set, long)?).map_err(|e| format!("{long}: {e}"))?;
        let lod = sm.lods.first().ok_or_else(|| format!("{long}: no LOD"))?;
        let n = lod.num_vertices;
        if lod.positions.len() != n || lod.tangent_x.len() != n || lod.tangent_z.len() != n || lod.influences.len() != n { return Err(format!("{long}: vertex stream lengths disagree")); }
        let mesh = fx.create_mesh(&FxMeshDesc { label: sm.name.clone(), vertices: vertex_bytes(lod), indices: lod.indices.clone() });
        let sections = lod.sections.iter().map(|s| (s.material_index as usize, s.base_index, s.num_triangles * 3)).collect();
        let bone_maps: Vec<Vec<u16>> = lod.sections.iter().map(|s| s.bone_map.clone()).collect();
        let bones = bone_maps.iter().map(|_| fx.create_buffer("lobby ghost bones", BONE_FLOAT4S * 16)).collect();
        let mut globals = vec![[0f32; 4]; SKIN_VS_GLOBALS_FLOAT4S];
        globals[6] = [lod.mesh_origin[0], lod.mesh_origin[1], lod.mesh_origin[2], 0.0];
        globals[7] = [lod.mesh_extension[0], lod.mesh_extension[1], lod.mesh_extension[2], 0.0];
        let vs_globals = fx.create_buffer("lobby ghost skin VF globals", SKIN_VS_GLOBALS_FLOAT4S * 16);
        fx.write_buffer(vs_globals, &bytes(&globals));
        let names: Vec<String> = sm.ref_skeleton.iter().map(|b| b.name.to_lowercase()).collect();
        let parents: Vec<i32> = sm.ref_skeleton.iter().map(|b| b.parent).collect();
        let ref_local: Vec<BoneTransform> = sm.ref_skeleton.iter().map(|b| BoneTransform { rotation: b.ref_rot, translation: b.ref_pos, scale: b.ref_scale }).collect();
        let inv_ref = component_space(&parents, &ref_local).iter().map(|m| m.inverse()).collect();
        let skel_path = sm.skeleton_path.clone().ok_or_else(|| format!("{long}: no Skeleton"))?;
        let skel_pkg = package(&self.set, fn_fx::data::long_package(&skel_path))?;
        let skeleton = SkeletonInfo::from_package(&skel_pkg).map_err(|e| format!("{skel_path}: {e}"))?;
        let anim_map: Vec<Option<usize>> = names.iter().map(|n| skeleton.bone_names.iter().position(|s| s.eq_ignore_ascii_case(n))).collect();
        let b = &sm.bounds;
        Ok(GhostMesh { name: sm.name.clone(), mesh, sections, bone_maps, bones, vs_globals, parents, ref_local, inv_ref,
            bounds: fn_render::primitive_ub::BoxSphereBounds { origin: b.origin, extent: b.extent, radius: b.radius }, extra_influences: lod.extra_bone_influences, ref_names: names, skeleton, anim_map })
    }

    pub fn ghost_anim(&self, seq_path: &str) -> Result<AnimSequence, String> {
        let long = fn_fx::data::long_package(seq_path);
        AnimSequence::parse(&package(&self.set, long)?).map_err(|e| format!("{long}: {e}"))
    }

    pub fn ghost_pose(g: &GhostMesh, anim: Option<(&AnimSequence, f32)>) -> Vec<Mat4> {
        let local: Vec<BoneTransform> = match anim {
            Some((seq, t)) => {
                let pose = seq.pose_at(t, &g.skeleton);
                g.anim_map.iter().zip(&g.ref_local).map(|(m, r)| m.and_then(|i| pose.get(i).copied()).map(|p| BoneTransform { scale: r.scale, ..p }).unwrap_or(*r)).collect()
            }
            None => g.ref_local.clone(),
        };
        let comp = component_space(&g.parents, &local);
        comp.iter().zip(&g.inv_ref).map(|(c, inv)| *c * *inv).collect()
    }

    pub fn draw_ghost(&mut self, fx: &mut FxRenderer, g: &GhostMesh, materials: &[Option<String>], skin: &[Mat4], l2w: Mat4, actor_pos: Vec3) -> Result<(), String> {
        self.draw_ghost_with(fx, g, materials, skin, l2w, actor_pos, &[])
    }

    pub fn draw_ghost_with(&mut self, fx: &mut FxRenderer, g: &GhostMesh, materials: &[Option<String>], skin: &[Mat4], l2w: Mat4, actor_pos: Vec3, extra: &[fn_fx::uniform::Overrides]) -> Result<(), String> {
        let now = self.time;
        let prim = fn_render::primitive_ub::mesh_primitive(&l2w, &g.bounds, actor_pos.to_array(), &fn_render::primitive_ub::PrimitiveFlags::default());
        for (si, &(slot_i, first, count)) in g.sections.iter().enumerate() {
            let Some(Some(path)) = materials.get(slot_i) else { continue };
            let mut rows = vec![[0f32; 4]; BONE_FLOAT4S];
            for (k, &b) in g.bone_maps[si].iter().enumerate().take(BONE_FLOAT4S / 3) {
                let m = skin.get(b as usize).copied().unwrap_or(Mat4::IDENTITY);
                rows[3 * k] = m.row(0).to_array(); rows[3 * k + 1] = m.row(1).to_array(); rows[3 * k + 2] = m.row(2).to_array();
            }
            self.frame.buffer_writes.push((g.bones[si], bytes(&rows)));
            let key = (format!("ghost|{path}|{}|{si}", g.name), Kind::HoloSkin);
            let e = match self.index.get(&key) {
                Some(r) => r.clone()?,
                None => {
                    let (bones, vs_globals) = (g.bones[si], g.vs_globals);
                    let r = (|| -> Result<usize, String> {
                        let mat = prewarm_material(&self.set, path)?;
                        let vf = if g.extra_influences { Vf::SkinExtra } else { Vf::Skin };
                        let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, vf, BASEPASS_VS, BASEPASS_PS, 1, false, self.shared, &[("PrimitiveFade", self.fade_buf)], LayoutOpts { vertex_storage: Some(bones), vs_globals: Some(vs_globals), ..Default::default() })?;
                        let line = format!("lobby ghost {} section {si}: {} on {} blend {} shading {}{}", g.name, path.rsplit('.').next().unwrap_or(path), vf.type_name(), mat.flags.blend, mat.flags.shading, if g.extra_influences { " (8 influences)" } else { "" });
                        eprintln!("[lobby fx] {line}");
                        self.log.push(line);
                        self.entries.push(Entry { path: path.clone(), mat, gpu, kind: Kind::HoloSkin });
                        Ok(self.entries.len() - 1)
                    })();
                    self.index.insert(key, r.clone());
                    r?
                }
            };
            let ov = match extra.get(si) { Some(x) => { let mut o = self.entries[e].mat.overrides.clone(); o.vectors.extend(x.vectors.iter().map(|(k, v)| (k.clone(), *v))); o.scalars.extend(x.scalars.iter().map(|(k, v)| (k.clone(), *v))); o } None => self.entries[e].mat.overrides.clone() };
            let packed = fn_fx::uniform::evaluate(&self.entries[e].mat.uniform, &ov, now);
            let slot = self.slot(fx);
            self.frame.buffer_writes.push((self.pool[slot].prim, bytes(&prim)));
            let regs = packed.cb.len().max(self.entries[e].gpu.material_registers);
            let mut cb = packed.cb; cb.resize(regs, [0.0; 4]);
            self.frame.buffer_writes.push((self.pool[slot].mat, bytes(&cb)));
            let (pb, mb) = (self.pool[slot].prim, self.pool[slot].mat);
            let binds = match self.binds.get(&(e, slot)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_ext(fx, pb, Some(mb), None); self.binds.insert((e, slot), b.clone()); b } };
            self.touched.insert(e);
            let sort = self.sort(g.bounds.transform_by(&l2w).origin)?;
            self.frame.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh: g.mesh, first_index: first, index_count: count }, instances: None, sort });
        }
        Ok(())
    }
}
