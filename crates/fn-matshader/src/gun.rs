//! The held firearm as a GPU model: per mesh section the original base-pass permutation of the section's material, its constant buffers, textures and samplers.
//!
//! Permutation (all RECOVERED / derived from the data, see the crate README of this task in `maps.rs`):
//! * shader map: `PCD3D_SM5` (D3D11 feature level SM5), quality High; the SM5 maps hold identical programs, so quality does not matter for these types;
//! * vertex factory: `TGPUSkinVertexFactoryfalse` (4 bone influences; the gun meshes have no extra influences, `fn_mesh` refuses an 8-influence LOD) - the pickaxe /
//!   guns are `bUsedWithSkeletalMesh` only, the material compiles no other skeletal factory;
//! * pass: the opaque deferred base pass: `TBasePassVSFNoLightMapPolicy` + `TBasePassPSFNoLightMapPolicySkylight`. A movable skeletal mesh has no lightmap
//!   (`FNoLightMapPolicy`); the pixel program is the one with the sky light enabled, because the Athena world has a sky light (its constants are the renderer's recovered
//!   ones). The `Simple*LightingPolicy` / `LightMapDensity` / `DebugView` / `ShadowDepth` / `HitProxy` / `Velocity` programs belong to other passes.
use crate::assemble::{assemble, layout_of, Assembled, ResKind, Stage};
use crate::frame::{self, bytes, fade_buffer, local_to_world_ue, primitive_buffer};
use crate::maps::Program;
use crate::material::ResolvedMaterial;
use crate::mesh::{self, GunMeshData};
use crate::textures::{asset_sampler, sampler_member, world_group_sampler, SamplerMember, TextureCache};
use crate::view::{self, ViewInput, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_render::gun_material::*;
use glam::Mat4;
use std::collections::HashMap;
use std::sync::Arc;

pub const VERTEX_FACTORY: &str = "TGPUSkinVertexFactoryfalse";
pub const VS_TYPE: &str = "TBasePassVSFNoLightMapPolicy";
pub const PS_TYPE: &str = "TBasePassPSFNoLightMapPolicySkylight";

/// Buffers shared by every draw of a frame.
#[derive(Clone, Copy)]
pub struct SharedBuffers { pub view: GunBufId, pub fade: GunBufId, /** GUN SHADOW: the View buffer of the shadow depth pass (`ViewInput::shadow_view_buffer_sh`) */ pub shadow_view: GunBufId }

pub struct SectionDraw {
    pub pipe: GunPipeId,
    pub bind: GunBindId,
    /// GUN SHADOW: `bind` with the shadow View buffer.
    pub shadow_bind: GunBindId,
    pub first_index: u32,
    pub index_count: u32,
    pub bone_buf: GunBufId,
    /// VELOCITY: the vertex program's `PreviousBoneMatrices` (its second bone SRV, `t1`, read only under `PerBoneMotionBlur`), when it has one.
    pub prev_bone_buf: Option<GunBufId>,
    /// Joint slot of the renderer's skin palette for every section-local bone.
    pub bone_slots: Vec<Option<usize>>,
    pub material_buf: GunBufId,
    pub material: Arc<ResolvedMaterial>,
    pub material_registers: usize,
}

pub struct GunModel {
    pub name: String,
    pub mesh: GunMeshId,
    pub sections: Vec<SectionDraw>,
    pub prim_buf: GunBufId,
    pub vf_buf: GunBufId,
    pub shared: SharedBuffers,
    pub bounds: fn_render::primitive_ub::BoxSphereBounds,
    pub mesh_origin: [f32; 3],
    pub mesh_extension: [f32; 3],
    /// VELOCITY: last frame's transform / bones ([`GunModel::frame_at`]).
    pub history: std::sync::Mutex<GunHistory>,
}

pub struct FrameInput<'a> {
    pub view: ViewInput,
    /// The renderer's model matrix of the gun (port glTF mesh space -> port world).
    pub gun_to_world: Mat4,
    /// The renderer's skin matrices (glTF space, joint-slot order).
    pub skin: &'a [Mat4],
    /// `fn_render::lighting::sky_radiance()`.
    pub sky_radiance: [f32; 3],
    pub time: f32,
    pub vectors: &'a [(&'a str, [f32; 4])],
}

/// FIRST USE (2026-10-06): the CPU half of [`GunScene::build_gun`], done on a worker thread before the UI thread builds: the mesh parse, the material instances
/// (root shader maps shared, `material::root_maps`) and their textures' mip chains (`textures::predecode_2d`). The UI-thread build then only uploads; a weapon
/// spawned / picked up / installed without a prepare builds exactly as before. Keys: long package path of the mesh, WID package path, material object path.
pub struct PreparedMesh { pub mesh: GunMeshData, /** section material object paths (the mesh's own slots) */ pub materials: Vec<Option<String>> }
static PREPARED_MESHES: std::sync::Mutex<Option<HashMap<String, Arc<PreparedMesh>>>> = std::sync::Mutex::new(None);
static PREPARED_WEAPONS: std::sync::Mutex<Option<HashMap<String, Arc<GunMeshData>>>> = std::sync::Mutex::new(None);
static PREPARED_MATERIALS: std::sync::Mutex<Option<HashMap<String, Arc<ResolvedMaterial>>>> = std::sync::Mutex::new(None);

fn prepared_material(path: &str) -> Option<Arc<ResolvedMaterial>> { PREPARED_MATERIALS.lock().ok()?.as_ref()?.get(path).cloned() }
/// The prepared skeletal mesh `mesh_path` (object or package path), if a worker prepared it.
pub fn prepared_mesh(mesh_path: &str) -> Option<Arc<PreparedMesh>> { PREPARED_MESHES.lock().ok()?.as_ref()?.get(fn_fx::data::long_package(mesh_path)).cloned() }
/// The prepared `WeaponMeshOverride` mesh of the weapon item definition `wid_package`, if a worker prepared it.
pub fn prepared_weapon(wid_package: &str) -> Option<Arc<GunMeshData>> { PREPARED_WEAPONS.lock().ok()?.as_ref()?.get(wid_package).cloned() }

/// Resolves material instances and decodes their 2D textures (worker thread). Failures are left to the build, which reports them as before.
pub fn prepare_materials(set: &PakSet, materials: &[Option<String>]) {
    for path in materials.iter().flatten() {
        let m = match prepared_material(path) {
            Some(m) => m,
            None => match ResolvedMaterial::load(set, path) {
                Ok(m) => { let m = Arc::new(m); if let Ok(mut g) = PREPARED_MATERIALS.lock() { g.get_or_insert_with(HashMap::new).insert(path.clone(), m.clone()); } m }
                Err(_) => continue,
            },
        };
        for k in 0..m.map().uniform.textures2d.len() { if let Some((t, _)) = m.texture_slot(k) { crate::textures::predecode_2d(set, &t); } }
    }
}

/// Parses the skeletal mesh `mesh_path` (object or package path, e.g. a pickup's `PickupSkeletalMesh`) and prepares its section materials (worker thread).
pub fn prepare_skeletal(set: &PakSet, mesh_path: &str) -> Result<Arc<PreparedMesh>, String> {
    if let Some(p) = prepared_mesh(mesh_path) { return Ok(p); }
    let long = fn_fx::data::long_package(mesh_path);
    let (a, x, b) = fn_fx::data::package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let sm = fn_mesh::SkeletalMesh::parse(&p).map_err(|e| format!("{long}: {e}"))?;
    let mesh = GunMeshData::from_skeletal(&sm)?;
    let materials: Vec<Option<String>> = mesh.sections.iter().map(|s| sm.materials.get(s.material_index).and_then(|m| m.material.clone())).collect();
    prepare_materials(set, &materials);
    let prepared = Arc::new(PreparedMesh { mesh, materials });
    if let Ok(mut g) = PREPARED_MESHES.lock() { g.get_or_insert_with(HashMap::new).insert(long.to_owned(), prepared.clone()); }
    Ok(prepared)
}

/// Prepares the held mesh of weapon `wid_package` (`mesh::load_weapon_mesh`) with the section materials the loadout item names (worker thread).
pub fn prepare_weapon(set: &PakSet, wid_package: &str, section_materials: &[Option<String>]) -> Result<(), String> {
    if prepared_weapon(wid_package).is_none() {
        let mesh = Arc::new(mesh::load_weapon_mesh(set, wid_package)?);
        if let Ok(mut g) = PREPARED_WEAPONS.lock() { g.get_or_insert_with(HashMap::new).insert(wid_package.to_owned(), mesh); }
    }
    prepare_materials(set, section_materials);
    Ok(())
}

/// Everything shared between the guns: textures, resolved materials, pipelines and the shared buffers.
pub struct GunScene {
    pub shared: SharedBuffers,
    textures: TextureCache,
    materials: HashMap<String, Arc<ResolvedMaterial>>,
    pipelines: HashMap<(String, usize, usize), (GunPipeId, Arc<Assembled>)>,
    pub log: Vec<String>,
}

impl GunScene {
    pub fn new(gun: &mut GunRenderer) -> GunScene {
        let shared = SharedBuffers { view: gun.create_buffer("gun View", VIEW_FLOAT4S * 16), fade: gun.create_buffer("gun PrimitiveFade", 16), shadow_view: gun.create_buffer("gun shadow View", VIEW_FLOAT4S * 16) };
        gun.register_view_buffer(shared.view); // VELOCITY: its PrevTranslatedWorldToClip / PrevPreViewTranslation come from the renderer's view history
        GunScene { shared, textures: TextureCache::default(), materials: HashMap::new(), pipelines: HashMap::new(), log: vec![] }
    }

    fn material(&mut self, set: &PakSet, path: &str) -> Result<Arc<ResolvedMaterial>, String> {
        if let Some(m) = self.materials.get(path) { return Ok(m.clone()); }
        let m = match prepared_material(path) { Some(m) => m, None => Arc::new(ResolvedMaterial::load(set, path)?) }; // FIRST USE: resolved on a worker by `prepare_*`
        self.materials.insert(path.to_owned(), m.clone());
        Ok(m)
    }

    /// Builds `mesh` with `materials[section]` (full object paths, section order). Errors name exactly what is not recoverable; nothing is guessed.
    pub fn build_gun(&mut self, set: &PakSet, gun: &mut GunRenderer, mesh: &GunMeshData, part: &fn_pose::Part, materials: &[Option<String>]) -> Result<GunModel, String> {
        if mesh.sections.len() != materials.len() { return Err(format!("{}: {} sections but {} materials", mesh.name, mesh.sections.len(), materials.len())); }
        let mesh_id = gun.create_mesh(&mesh.name, &mesh.vertices, &mesh.indices);
        let prim_buf = gun.create_buffer("gun Primitive", frame::PRIMITIVE_FLOAT4S * 16);
        let vf_buf = gun.create_buffer("gun vertex factory", frame::VF_FLOAT4S * 16);
        // reference-skeleton bone -> joint slot of the renderer's skin palette, by (lower-cased) bone name
        let slot_of: HashMap<&str, usize> = part.joints.iter().enumerate().map(|(slot, node)| (part.tree.names[*node].as_str(), slot)).collect();
        let mut sections = vec![];
        for (si, sec) in mesh.sections.iter().enumerate() {
            let path = materials[si].as_deref().ok_or_else(|| format!("{}: section {si} has no original material", mesh.name))?;
            let mat = self.material(set, path)?;
            if mat.flags.blend != "BLEND_Opaque" || mat.flags.shading != "MSM_DefaultLit" { return Err(format!("{path}: unsupported blend / shading ({} / {}): only opaque default-lit is wired", mat.flags.blend, mat.flags.shading)); }
            let map = mat.map();
            let vs = mat.maps.find(map.index, VERTEX_FACTORY, VS_TYPE).ok_or_else(|| format!("{}: no {VERTEX_FACTORY} / {VS_TYPE} program in map {}", mat.root_path, map.index))?;
            let ps = mat.maps.find(map.index, VERTEX_FACTORY, PS_TYPE).ok_or_else(|| format!("{}: no {VERTEX_FACTORY} / {PS_TYPE} program in map {}", mat.root_path, map.index))?;
            for (ty, p) in [(VS_TYPE, vs), (PS_TYPE, ps)] {
                if !mat.maps.programs_agree_across_maps(VERTEX_FACTORY, ty) { self.log.push(format!("[gun] {}: {ty} differs between the SM5 maps; map {} used", mat.root_path, map.index)); }
                let _ = p;
            }
            // pipeline (one per program pair)
            let key = (mat.root_path.clone(), vs.stream, ps.stream);
            let (pipe, asm) = match self.pipelines.get(&key) {
                Some(p) => p.clone(),
                None => {
                    let asm = Arc::new(assemble(vs, ps)?);
                    for a in &asm.vs_attributes { if !mesh::attributes().iter().any(|x| x.location == *a) { return Err(format!("{}: vertex program reads ATTRIBUTE{a}, which the GPU-skin stream does not provide", mat.root_path)); } }
                    let desc = GunPipelineDesc {
                        label: format!("gun {} {}", mat.root_path.rsplit('.').next().unwrap_or(""), VERTEX_FACTORY),
                        wgsl: asm.wgsl.clone(), vs_entry: "gun_vs_main".into(), fs_entry: "gun_fs_main".into(),
                        vertex_stride: mesh::STRIDE, vertex_attrs: mesh::attributes(), layout: layout_of(&asm.resources),
                        cull: if mat.flags.two_sided { GunCull::None } else { GunCull::BackCw },
                        // GUN SHADOW: opaque materials cast into the sun's cascades (assemble.rs `gun_shadow_vs_main`)
                        shadow_vs_entry: Some("gun_shadow_vs_main".into()),
                    };
                    let id = gun.create_pipeline(&desc)?;
                    self.pipelines.insert(key.clone(), (id, asm.clone()));
                    (id, asm)
                }
            };
            // per-section resources
            let packed = mat.packed(0.0);
            let declared = asm.resources.iter().filter(|r| r.kind == ResKind::Uniform && vs.ubo_names.get(r.slot as usize).map(|n| n == "Material").unwrap_or(false) || r.kind == ResKind::Uniform && r.stage == Stage::Pixel && ps.ubo_names.get(r.slot as usize).map(|n| n == "Material").unwrap_or(false)).map(|r| r.size as usize).max().unwrap_or(0);
            let material_registers = packed.registers.max(declared).max(1);
            let material_buf = gun.create_buffer("gun Material", material_registers * 16);
            let bone_buf = gun.create_buffer("gun bones", crate::assemble::BONE_VEC4S as usize * 16);
            let mut prev_bone_buf: Option<GunBufId> = None; // VELOCITY
            let ntex = map.uniform.textures2d.len();
            let mut entries: Vec<GunBindEntry> = vec![];
            for r in &asm.resources {
                let prog: &Program = if r.stage == Stage::Vertex { vs } else { ps };
                let res = match r.kind {
                    ResKind::Uniform => {
                        let name = prog.ubo_names.get(r.slot as usize).map(|s| s.as_str()).unwrap_or("");
                        GunRes::Buffer(match (r.stage, name) {
                            (_, "View") => self.shared.view,
                            (_, "Primitive") => prim_buf,
                            (_, "PrimitiveFade") => self.shared.fade,
                            (_, "Material") => material_buf,
                            (Stage::Vertex, "") if r.slot == 0 => vf_buf,
                            _ => return Err(format!("{}: {:?} program reads uniform buffer slot {} ('{name}') which has no recovered source", mat.root_path, r.stage, r.slot)),
                        })
                    }
                    // VELOCITY: the GPU-skin program's second bone SRV (t1) is `PreviousBoneMatrices` (GpuSkinVertexFactory.usf:155 / :161 declaration order; the
                    // translated WeaponMat_Base VS reads t1 only inside `if PerBoneMotionBlur` = vf buffer 7.w, test `gun_program_previous_bones_are_t1`)
                    ResKind::BoneBuffer if r.slot == 1 => GunRes::Buffer(*prev_bone_buf.get_or_insert_with(|| gun.create_buffer("gun previous bones", crate::assemble::BONE_VEC4S as usize * 16))),
                    ResKind::BoneBuffer => GunRes::Buffer(bone_buf),
                    ResKind::Tex2D => {
                        if r.stage == Stage::Vertex { return Err(format!("{}: vertex program samples texture t{}", mat.root_path, r.slot)); }
                        let b = prog.bindings.iter().find(|b| b.kind == fn_dxbc::unreal_packet::ResourceKind::Texture && b.binding.register as u32 == r.slot).ok_or_else(|| format!("{}: pixel texture t{} is not a material texture (scene texture?)", mat.root_path, r.slot))?;
                        let k = (b.binding.member / 2) as usize;
                        let (tpath, _) = mat.texture_slot(k).ok_or_else(|| format!("{}: 2D texture expression {k} has no resolvable texture", mat.path))?;
                        let (id, _) = self.textures.load(set, gun, &tpath)?;
                        GunRes::Texture(id)
                    }
                    ResKind::Sampler => {
                        let b = prog.bindings.iter().find(|b| b.kind == fn_dxbc::unreal_packet::ResourceKind::Sampler && b.binding.register as u32 == r.slot).ok_or_else(|| format!("{}: sampler s{} is not a material sampler", mat.root_path, r.slot))?;
                        GunRes::Sampler(match sampler_member(b.binding.member as usize, ntex)? {
                            SamplerMember::Asset(k) => {
                                let (tpath, _) = mat.texture_slot(k).ok_or_else(|| format!("{}: sampler of texture {k} unresolved", mat.path))?;
                                let (_, info) = self.textures.load(set, gun, &tpath)?;
                                asset_sampler(&info)
                            }
                            SamplerMember::WrapWorld => world_group_sampler(false),
                            SamplerMember::ClampWorld => world_group_sampler(true),
                        })
                    }
                };
                entries.push(GunBindEntry { binding: r.binding, res });
            }
            let bind = gun.create_bind_group(pipe, &entries);
            // GUN SHADOW: the same resources with the shadow View buffer
            for e in entries.iter_mut() { if let GunRes::Buffer(b) = &mut e.res { if *b == self.shared.view { *b = self.shared.shadow_view; } } }
            let shadow_bind = gun.create_bind_group(pipe, &entries);
            let bone_slots = sec.bone_map.iter().map(|&b| mesh.bone_names.get(b as usize).and_then(|n| slot_of.get(n.as_str()).copied())).collect::<Vec<_>>();
            if bone_slots.len() > crate::assemble::BONE_VEC4S as usize / 3 { return Err(format!("{}: section {si} has {} bones, over the {} the uniform palette holds", mesh.name, bone_slots.len(), crate::assemble::BONE_VEC4S / 3)); }
            sections.push(SectionDraw { pipe, bind, shadow_bind, first_index: sec.first_index, index_count: sec.index_count, bone_buf, prev_bone_buf, bone_slots, material_buf, material: mat, material_registers });
        }
        Ok(GunModel { name: mesh.name.clone(), mesh: mesh_id, sections, prim_buf, vf_buf, shared: self.shared, bounds: mesh.bounds, mesh_origin: mesh.mesh_origin, mesh_extension: mesh.mesh_extension, history: Default::default() })
    }
}

/// VELOCITY (2026-10-05): what the model drew with in its last submitted frame (`frame_id`), and the previous data that frame used (a re-draw of the same frame id
/// reuses it).
#[derive(Default)]
pub struct GunHistory { frame_id: Option<u64>, l2w: Mat4, bones: Vec<Vec<[f32; 4]>>, prev: Option<(Mat4, Vec<Vec<[f32; 4]>>)> }

impl GunModel {
    /// Appends this frame's uniform writes and draws, without a previous frame (previous transform / bones = the current ones: no motion of its own).
    pub fn frame(&self, input: &FrameInput, out: &mut GunFrame) { self.frame_at(input, None, out) }
    /// VELOCITY (2026-10-05): [`Self::frame`] for the submit number `frame_id` (consecutive per rendered frame). The base-pass vertex program's previous-frame inputs
    /// are filled as UE does for a movable skeletal mesh component: `PreviousLocalToWorld` = last frame's transform (the motion-blur info of a moved primitive,
    /// `HasVelocityOnBasePass`, VelocityRendering.cpp:432-458 with BasePassRendering.inl:44-48; the current one without it), `PreviousBoneMatrices` = last
    /// frame's bone buffer with `PerBoneMotionBlur` true whenever the program binds it (GPUSkinVertexFactory.cpp:538-564, GpuSkinVertexFactory.usf:349-363), and
    /// `SkipOutputVelocity` 0 (a camera cut skips the whole velocity pass in the renderer). Without the immediately preceding submit the current data is used.
    pub fn frame_at(&self, input: &FrameInput, frame_id: Option<u64>, out: &mut GunFrame) {
        let l2w = local_to_world_ue(input.gun_to_world);
        let bones: Vec<Vec<[f32; 4]>> = self.sections.iter().map(|s| frame::bone_buffer(input.skin, &s.bone_slots, crate::assemble::BONE_VEC4S as usize)).collect();
        let (prev_l2w, prev_bones) = {
            let mut h = self.history.lock().unwrap_or_else(|p| p.into_inner());
            let prev = match (frame_id, h.frame_id) {
                (Some(f), Some(last)) if f == last => h.prev.clone().unwrap_or((l2w, bones.clone())),
                (Some(f), Some(last)) if f == last + 1 => (h.l2w, h.bones.clone()),
                _ => (l2w, bones.clone()),
            };
            *h = GunHistory { frame_id, l2w, bones: bones.clone(), prev: Some(prev.clone()) };
            prev
        };
        // the scene's own sky light SH (fn_render::lighting::sky_sh, as lit_common.wgsl `sky_lookup`); `input.sky_radiance` was the hemisphere stand-in
        let _ = input.sky_radiance;
        if !out.buffer_writes.iter().any(|(b, _)| *b == self.shared.view) {
            let sh = fn_render::lighting::sky_sh();
            out.buffer_writes.push((self.shared.view, view::bytes(&input.view.view_buffer_sh(&sh))));
            out.buffer_writes.push((self.shared.shadow_view, view::bytes(&input.view.shadow_view_buffer_sh(&sh)))); // GUN SHADOW
            out.buffer_writes.push((self.shared.fade, bytes(&fade_buffer())));
        }
        out.buffer_writes.push((self.prim_buf, bytes(&primitive_buffer(&l2w, &self.bounds))));
        let per_bone_motion_blur = self.sections.iter().any(|s| s.prev_bone_buf.is_some());
        out.buffer_writes.push((self.vf_buf, bytes(&frame::vf_buffer_velocity(&l2w, &prev_l2w, per_bone_motion_blur, self.mesh_origin, self.mesh_extension))));
        for (k, s) in self.sections.iter().enumerate() {
            out.buffer_writes.push((s.bone_buf, bytes(&bones[k])));
            if let Some(pb) = s.prev_bone_buf { out.buffer_writes.push((pb, bytes(prev_bones.get(k).unwrap_or(&bones[k])))); } // VELOCITY: PreviousBoneMatrices
            let mut cb = Vec::with_capacity(s.material_registers);
            s.material.cb_into(input.time, input.vectors, &mut cb);
            cb.resize(s.material_registers, [0.0; 4]);
            out.buffer_writes.push((s.material_buf, bytes(&cb)));
            out.draws.push(GunDraw { pipe: s.pipe, bind: s.bind, mesh: self.mesh, first_index: s.first_index, index_count: s.index_count, shadow_bind: Some(s.shadow_bind) });
        }
    }
}

