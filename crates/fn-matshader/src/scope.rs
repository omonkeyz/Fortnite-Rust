//! The sniper scope as the original 1.8 data draws it: a post-process material (`DownScopePostProcess_Athena`, parent `DownScopePostProcess`, domain `MD_PostProcess`,
//! blend `BLEND_Translucent`, evaluated `AfterTonemapping`) run over the finished image, plus the static mesh `S_SniperScope_Proto` of the weapon Blueprint's
//! `ScopeMesh1P` component, attached to the camera by the material function `AttachScope`.
//!
//! RECOVERED (every number comes from the package data or from the original DXBC):
//! * the post program is the cooked `FFPostProcessMaterialPS_HighEnd` of the SM5 shader map, translated from DXBC by `fn-dxbc` and run unchanged; its four constant
//!   buffers are named by the program's own table: `""` (pass parameters, `PostprocessInput0MinMax` at float4 9), `View`, `MaterialCollection0`
//!   (`FortniteMaterialParameters`, 54 float4), `Material` (the instance chain evaluated by `ResolvedMaterial::packed`). t0 / t1 / t2 are the material's `Texture2D_0`,
//!   `Texture2D_1` (the instance's `Reticle` texture parameter) and its cube map; t3 / s3 (not in the material's tables) are `PostprocessInput0` / its sampler.
//! * what the program reads of `View` (float4 slots; the 1.8 `View` struct has one more matrix than 4.16 before `ViewToClip`): 12..14 `ViewToTranslatedWorld`,
//!   28..31 `ViewToClip`, 58 `ScreenPositionScaleBias`, 120 `ViewRectMin`, 121 `ViewSizeAndInvSize`, 122 `BufferSizeAndInvSize`, 152 `AmbientCubemapTint`
//!   (exe `FPostProcessSettings` ctor default white, the Day volume overrides it with the same value: `reports/postfx-evidence.json`).
//! * the collection values `B_Ranged_Generic.ExecuteUbergraph` (InitializeScopeVariables / the targeting event) writes: `TopLeftScope`, `BottomRightScope` = the
//!   `GetSocketLocation` of the mesh's two sockets, `WeaponSightsCameraOffset` = `Scope Camera Offset Amount`, `DownScopePostProcessAmount` = the scope timeline curve.
//!   The program projects `(-x, z, y + offset)` of those with `ViewToClip` (the same mapping the mesh's vertex program applies, see [`RING_WGSL`]) to find the lens box.
//! * the mesh is drawn with its vertex program's `AttachScope` offset: local `(x, y, z)` -> view space `(-x, z, y + offset)`.
//!
//! INFERRED: `ScopeMesh1P` has `bAbsoluteLocation/Rotation/Scale = true` and no serialized values, so its component-to-world is the identity and `GetSocketLocation` is
//! the socket's `RelativeLocation` (the DXBC treats the values as mesh-local, which agrees).
//! NOT DONE: the mesh's own base-pass shading (GBuffer BaseColor 0.05, Metallic 1 -> no diffuse, only environment / sun specular, not evaluated) is replaced by black; the
//! sniper's stage-1 post-process settings (vignette 1, scene fringe 1, near DOF blur, `Target Scope Vignette Blur Screen Percentage`) are not run (no such passes).
use crate::material::ResolvedMaterial;
use crate::textures::{asset_sampler, decode_2d, TexInfo};
use crate::view::{dir_to_ue, ViewInput};
use fn_fx::data::{long_package, package_files, PackageJson};
use fn_pak::PakSet;
use fn_render::gun_material::{GunSamplerDesc, GunTextureDesc};
use fn_render::scope_pass::*;

pub const POST_PROGRAM: &str = "FFPostProcessMaterialPS_HighEnd";
pub const MPC_PACKAGE: &str = "/Game/Packages/Fortress_SharedMaterials/GlobalMaterialParameters/FortniteMaterialParameters";
/// Cube map faces in the D3D / WebGPU order the original `TextureCube` slices are stored in (+X -X +Y -Y +Z -Z).
const FACES: usize = 6;
/// `AmbientCubemapTint` (exe `FPostProcessSettings` constructor value, RECOVERED, see the module docs).
pub const AMBIENT_CUBEMAP_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Full-screen triangle vertex program appended to the translated pixel program (UE draws a rectangle with `FPostProcessMaterialVS`, `SV_Position.w` = 1).
const FULLSCREEN_VS: &str = r#"
@vertex fn scope_vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// `ScopeMesh1P` vertex program reduced to what draws pixels. `u.m = (1 / tan(half fov x), 1 / tan(half fov y), Scope Camera Offset Amount, 0)`. RECOVERED from the base-pass
/// VS DXBC of `M_SniperScope_MeshMaterial_proto`: `AttachScope` WPO places local `(x, y, z)` at view-space `(-x, z, y + offset)` (view space: x right, y up, z forward, cm).
/// The pixel colour is black (NOT DONE: the original base-pass lighting of a BaseColor 0.05 / Metallic 1 surface, see the module docs).
pub const RING_WGSL: &str = r#"
struct RingU { m: vec4<f32>, };
@group(0) @binding(0) var<uniform> u: RingU;
@vertex fn ring_vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    let v = vec3<f32>(-p.x, p.z, p.y + u.m.z);
    return vec4<f32>(v.x * u.m.x, v.y * u.m.y, v.z * 0.5, v.z);
}
@fragment fn ring_fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role { Pass, View, Collection, Material }
#[derive(Clone, Copy, Debug)]
enum Pend { Cb(Role), Tex(usize), Cube, Samp(GunSamplerDesc), Scene, SceneSamp }
struct PendEntry { group: u32, binding: u32, what: Pend }

/// float4 slot / component of the collection values the program reads (from the `FortniteMaterialParameters` layout: scalars packed 4 per float4, then the vectors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionSlots { pub amount: (usize, usize), pub camera_offset: (usize, usize), pub top_left: usize, pub bottom_right: usize, pub float4s: usize }

/// Everything decoded from the packages; no GPU yet.
pub struct ScopeAssets {
    pub material_path: String,
    wgsl: String,
    entries: Vec<PendEntry>,
    sizes: [usize; 4],
    textures: Vec<GunTextureDesc>,
    cube: ScopeCubeDesc,
    pub slots: CollectionSlots,
    material_cb: Vec<[f32; 4]>,
    /// The mesh's sockets `TopLeftScope` / `BottomRightScope` (`RelativeLocation`, cm).
    pub top_left: [f32; 3],
    pub bottom_right: [f32; 3],
    /// `Scope Camera Offset Amount` (cm).
    pub camera_offset: f32,
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn f3(v: &serde_json::Value) -> Option<[f32; 3]> { let a = v.as_array()?; Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32]) }

/// Slots of the four collection values inside the `MaterialCollection0` buffer (UE 4.16 `FMaterialParameterCollection` uniform layout: scalar `i` = float4 `i / 4`
/// component `i % 4`, then vector `j` = float4 `ceil(scalars / 4) + j`).
pub fn collection_slots(set: &PakSet) -> Result<CollectionSlots, String> {
    let pkg = PackageJson::load(set, MPC_PACKAGE)?;
    let e = pkg.exports.iter().find(|e| e.class == "MaterialParameterCollection").ok_or("FortniteMaterialParameters has no collection export")?;
    let names = |k: &str| -> Vec<String> { e.props.get(k).and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|p| p.get("ParameterName").and_then(|n| n.as_str()).map(str::to_owned)).collect()).unwrap_or_default() };
    let (scalars, vectors) = (names("ScalarParameters"), names("VectorParameters"));
    let s = |n: &str| scalars.iter().position(|x| x == n).map(|i| (i / 4, i % 4)).ok_or_else(|| format!("collection has no scalar {n}"));
    let base = scalars.len().div_ceil(4);
    let v = |n: &str| vectors.iter().position(|x| x == n).map(|i| base + i).ok_or_else(|| format!("collection has no vector {n}"));
    Ok(CollectionSlots { amount: s("DownScopePostProcessAmount")?, camera_offset: s("WeaponSightsCameraOffset")?, top_left: v("TopLeftScope")?, bottom_right: v("BottomRightScope")?, float4s: base + vectors.len() })
}

fn decode_cube(set: &PakSet, object_path: &str) -> Result<(ScopeCubeDesc, TexInfo), String> {
    let long = long_package(object_path);
    let (a, x, b) = package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let t = fn_texture::Texture2D::parse(&p).map_err(|e| format!("{long}: {e}"))?;
    if !t.is_cube() { return Err(format!("{long}: not a cube map")); }
    let mut levels = vec![];
    for m in t.available_mips() {
        let mut faces: Vec<Vec<u16>> = vec![];
        let mut edge = 0;
        for f in 0..FACES {
            let img = t.decode_slice(m, f, fn_texture::DecodeOptions::default()).map_err(|e| format!("{long}: mip {m} face {f}: {e}"))?;
            if img.width != img.height { return Err(format!("{long}: non-square cube face")); }
            edge = img.width as u32;
            match img.pixels { fn_texture::Pixels::RgbaF16(v) => faces.push(v), fn_texture::Pixels::Rgba8(_) => return Err(format!("{long}: expected a half-float cube map ({})", t.compression_settings)) }
        }
        levels.push((edge, <[Vec<u16>; 6]>::try_from(faces).map_err(|_| "cube faces")?));
    }
    if levels.is_empty() { return Err(format!("{long}: no cube mip available")); }
    let info = TexInfo { filter: t.props.text("Filter").map(|s| s.rsplit("::").next().unwrap_or(s).to_owned()).unwrap_or_else(|| "TF_Default".into()), address_x: t.address_x.clone(), address_y: t.address_y.clone(), srgb: t.srgb, width: levels[0].0 as usize, height: levels[0].0 as usize, levels: levels.len(), compression: t.compression_settings.clone() };
    Ok((ScopeCubeDesc { label: t.name.clone(), levels }, info))
}

/// `@group(g) @binding(b) var...` declarations of a translated module: (group, binding, rest of the declaration).
fn declarations(wgsl: &str) -> Vec<(u32, u32, String)> {
    wgsl.lines().filter_map(|l| {
        let r = l.strip_prefix("@group(")?;
        let (g, r) = r.split_once(") @binding(")?;
        let (b, r) = r.split_once(") ")?;
        Some((g.parse().ok()?, b.parse().ok()?, r.to_owned()))
    }).collect()
}
fn slot_of(decl: &str, prefix: &str) -> Option<usize> {
    let name = decl.split("var").nth(1)?.split(':').next()?.trim().rsplit(' ').next()?;
    name.strip_prefix(prefix)?.parse().ok()
}

impl ScopeAssets {
    /// `material_path` = the sniper's `DownScopePostProcess` blendable instance, `mesh_path` = `ScopeMesh1P.StaticMesh`, `camera_offset` = `Scope Camera Offset Amount`.
    pub fn load(set: &PakSet, material_path: &str, mesh_path: &str, camera_offset: f32) -> Result<ScopeAssets, String> {
        let mat = ResolvedMaterial::load(set, material_path)?;
        if mat.flags.blend != "BLEND_Translucent" { return Err(format!("{material_path}: expected a translucent post-process material, found {}", mat.flags.blend)); }
        let prog = mat.maps.find(mat.map_index, "", POST_PROGRAM).ok_or_else(|| format!("{}: no {POST_PROGRAM} program in map {}", mat.root_path, mat.map_index))?;
        let body = fn_dxbc::codegen::translate_wgsl(&prog.shader, "ps_fx").map_err(|e| e.0)?;
        let n2d = mat.map().uniform.textures2d.len();
        // the cube map: the material's single cube texture parameter (the instance may override it)
        let root = PackageJson::load(set, long_package(&mat.root_path))?;
        let cubes: Vec<_> = root.of_class("MaterialExpressionTextureSampleParameterCube").collect();
        let [cube_expr] = cubes.as_slice() else { return Err(format!("{}: expected one cube texture parameter, found {}", mat.root_path, cubes.len())) };
        let pname = cube_expr.props.get("ParameterName").and_then(|v| v.as_str()).unwrap_or_default();
        let cube_path = mat.fx.texture_overrides.get(pname).cloned().or_else(|| cube_expr.props.get("Texture").and_then(|v| v.as_str()).map(str::to_owned)).ok_or("cube texture parameter has no texture")?;
        let (cube, cube_info) = decode_cube(set, &cube_path)?;
        let mut textures: Vec<GunTextureDesc> = vec![];
        let mut infos: Vec<TexInfo> = vec![];
        let mut tex_of_register: std::collections::HashMap<usize, usize> = Default::default();
        let mut entries = vec![];
        let mut sizes = [0usize; 4];
        let mut scene_tex = None;
        for (g, b, decl) in declarations(&body) {
            if decl.starts_with("var<uniform>") {
                let n = slot_of(&decl, "cb").ok_or("uniform buffer name")?;
                let size: usize = decl.split("vec4<f32>, ").nth(1).and_then(|r| r.split('>').next()).and_then(|n| n.parse().ok()).ok_or("uniform size")?;
                let role = match prog.ubo_names.get(n).map(String::as_str) {
                    Some("") => Role::Pass, Some("View") => Role::View, Some("MaterialCollection0") => Role::Collection, Some("Material") => Role::Material,
                    other => return Err(format!("{POST_PROGRAM}: uniform buffer slot {n} is {other:?}")),
                };
                sizes[role as usize] = size;
                entries.push(PendEntry { group: g, binding: b, what: Pend::Cb(role) });
            } else if decl.contains("texture_2d<f32>") || decl.contains("texture_cube<f32>") {
                let n = slot_of(&decl, "t").ok_or("texture name")?;
                let bound = prog.bindings.iter().find(|x| x.kind == fn_dxbc::unreal_packet::ResourceKind::Texture && x.binding.register as usize == n);
                match bound {
                    None => { if scene_tex.replace(n).is_some() { return Err(format!("{POST_PROGRAM}: two unbound textures")); } entries.push(PendEntry { group: g, binding: b, what: Pend::Scene }); }
                    Some(x) if decl.contains("texture_cube") => { if x.binding.member as usize != 2 * n2d { return Err(format!("cube member {} is not the first cube slot {}", x.binding.member, 2 * n2d)); } entries.push(PendEntry { group: g, binding: b, what: Pend::Cube }); }
                    Some(x) => {
                        let k = x.binding.member as usize / 2;
                        let (path, _) = mat.texture_slot(k).ok_or_else(|| format!("{}: 2D texture expression {k} unresolved", mat.path))?;
                        let (desc, info) = decode_2d(set, &path)?;
                        tex_of_register.insert(n, textures.len());
                        entries.push(PendEntry { group: g, binding: b, what: Pend::Tex(textures.len()) });
                        textures.push(desc); infos.push(info);
                    }
                }
            } else if decl.contains(": sampler;") {
                let n = slot_of(&decl, "s").ok_or("sampler name")?;
                let bound = prog.bindings.iter().find(|x| x.kind == fn_dxbc::unreal_packet::ResourceKind::Sampler && x.binding.register as usize == n);
                match bound {
                    None => entries.push(PendEntry { group: g, binding: b, what: Pend::SceneSamp }),
                    Some(x) => {
                        let m = x.binding.member as usize;
                        let desc = if m < 2 * n2d && m % 2 == 1 {
                            // the asset sampler of 2D texture m/2: that texture's own state; find its decoded info by the texture register with the same member - 1
                            let tb = prog.bindings.iter().find(|t| t.kind == fn_dxbc::unreal_packet::ResourceKind::Texture && t.binding.member as usize == m - 1).ok_or("sampler without its texture")?;
                            let ti = *tex_of_register.get(&(tb.binding.register as usize)).ok_or("sampler listed before its texture")?;
                            asset_sampler(&infos[ti])
                        } else if m == 2 * n2d + 1 { asset_sampler(&cube_info) } else { return Err(format!("{POST_PROGRAM}: sampler member {m} is not an asset sampler")) };
                        entries.push(PendEntry { group: g, binding: b, what: Pend::Samp(desc) });
                    }
                }
            } else { return Err(format!("unsupported declaration: {decl}")); }
        }
        if scene_tex.is_none() { return Err(format!("{POST_PROGRAM} does not read PostprocessInput0")); }
        let wgsl = format!("{body}\n{FULLSCREEN_VS}");
        // mesh + sockets
        let slots = collection_slots(set)?;
        let mesh_long = long_package(mesh_path);
        let (a, x, bb) = package_files(set, mesh_long)?;
        let mp = fn_ueio::Package::parse(&a, &x, &bb).map_err(|e| format!("{mesh_long}: {e}"))?;
        let mesh = fn_mesh::StaticMesh::parse(&mp).map_err(|e| format!("{mesh_long}: {e}"))?;
        let lod = mesh.lods.first().ok_or("scope mesh has no LOD")?;
        let mj = PackageJson::load(set, mesh_long)?;
        let socket = |name: &str| mj.of_class("StaticMeshSocket").find(|e| e.props.get("SocketName").and_then(|v| v.as_str()) == Some(name)).and_then(|e| e.props.get("RelativeLocation").and_then(f3)).ok_or_else(|| format!("{mesh_long}: socket {name} missing"));
        Ok(ScopeAssets {
            material_path: material_path.to_owned(), wgsl, entries, sizes, textures, cube, slots, material_cb: mat.packed(0.0).cb,
            top_left: socket("TopLeftScope")?, bottom_right: socket("BottomRightScope")?, camera_offset,
            positions: lod.positions.clone(), indices: lod.indices.clone(),
        })
    }

    /// Uploads textures / buffers, creates the pipelines.
    pub fn install(self, r: &mut ScopeRenderer) -> Result<ScopeScene, String> {
        let tex_ids: Vec<ScopeTexId> = self.textures.iter().map(|t| r.create_texture(t)).collect();
        let cube_id = r.create_cube(&self.cube);
        let bufs: [ScopeBufId; 4] = [r.create_buffer("scope pass", self.sizes[Role::Pass as usize] * 16), r.create_buffer("scope View", self.sizes[Role::View as usize] * 16), r.create_buffer("scope collection", self.sizes[Role::Collection as usize] * 16), r.create_buffer("scope Material", self.sizes[Role::Material as usize] * 16)];
        let entries: Vec<ScopeBindEntry> = self.entries.iter().map(|e| ScopeBindEntry { group: e.group, binding: e.binding, bind: match e.what {
            Pend::Cb(role) => ScopeBind::Uniform(bufs[role as usize]), Pend::Tex(i) => ScopeBind::Tex2D(tex_ids[i]), Pend::Cube => ScopeBind::Cube(cube_id),
            Pend::Samp(s) => ScopeBind::Sampler(s), Pend::Scene => ScopeBind::Scene, Pend::SceneSamp => ScopeBind::SceneSampler,
        } }).collect();
        r.create_post(ScopePostDesc { label: format!("scope post {}", self.material_path), wgsl: self.wgsl, vs_entry: "scope_vs".into(), fs_entry: "ps_fx_main".into(), entries })?;
        let ring_buf = r.create_buffer("scope ring", 16);
        r.create_ring(ScopeRingDesc { label: "scope ring".into(), wgsl: RING_WGSL.into(), vs_entry: "ring_vs".into(), fs_entry: "ring_fs".into(), positions: self.positions, indices: self.indices, uniform: ring_buf })?;
        Ok(ScopeScene { bufs, ring_buf, slots: self.slots, material_cb: self.material_cb, sizes: self.sizes, top_left: self.top_left, bottom_right: self.bottom_right, camera_offset: self.camera_offset })
    }
}

/// Per-frame input.
#[derive(Clone, Copy, Debug)]
pub struct ScopeInput {
    pub view: ViewInput,
    /// `DownScopePostProcessAmount` (the scope timeline value, `AdsMachine::scope_amount`).
    pub amount: f32,
    /// `ScopeMesh1P` is visible (the timeline finished forward while still targeting, `AdsMachine::scope_mesh_visible`).
    pub mesh_visible: bool,
}

pub struct ScopeScene {
    bufs: [ScopeBufId; 4],
    ring_buf: ScopeBufId,
    slots: CollectionSlots,
    material_cb: Vec<[f32; 4]>,
    sizes: [usize; 4],
    top_left: [f32; 3],
    bottom_right: [f32; 3],
    camera_offset: f32,
}

fn bytes(v: &[[f32; 4]]) -> Vec<u8> { let mut b = Vec::with_capacity(v.len() * 16); for r in v { for f in r { b.extend_from_slice(&f.to_le_bytes()); } } b }

/// Orthonormal view basis in UE axes (cm, Z up): right, up, forward (the same construction as `ViewInput::view_buffer`).
fn basis(v: &ViewInput) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let fwd = dir_to_ue(v.forward).normalize();
    let right = dir_to_ue(v.up).normalize().cross(fwd).normalize();
    let up = fwd.cross(right).normalize();
    let a = |d: glam::DVec3| [d.x as f32, d.y as f32, d.z as f32];
    (a(right), a(up), a(fwd))
}

impl ScopeScene {
    /// The `View` buffer slots the post program reads (see the module docs); `size` = float4s the program declares.
    pub fn view_buffer(v: &ViewInput, size: usize) -> Vec<[f32; 4]> {
        let mut o = vec![[0f32; 4]; size.max(153)];
        let (right, up, fwd) = basis(v);
        for (i, r) in [right, up, fwd].iter().enumerate() { o[12 + i] = [r[0], r[1], r[2], 0.0]; }
        o[15] = [0.0, 0.0, 0.0, 1.0];
        // ViewToClip, UE reversed-Z infinite far (row vectors): x * (1 / tan) , y * (1 / tan), w = view depth
        let (sx, sy) = (1.0 / v.tan_half.0, 1.0 / v.tan_half.1);
        o[28] = [sx, 0.0, 0.0, 0.0]; o[29] = [0.0, sy, 0.0, 0.0]; o[30] = [0.0, 0.0, 0.0, 1.0]; o[31] = [0.0, 0.0, v.near_m * 100.0, 0.0];
        let (w, h) = (v.width.max(1) as f32, v.height.max(1) as f32);
        o[58] = [0.5, -0.5, 0.5, 0.5];
        o[120] = [0.0; 4];
        o[121] = [w, h, 1.0 / w, 1.0 / h];
        o[122] = [w, h, 1.0 / w, 1.0 / h];
        o[152] = AMBIENT_CUBEMAP_TINT;
        o
    }

    /// `FPostProcessPassParameters`: `PostprocessInput0MinMax` = (viewport min / size, viewport max / size - one input texel) at float4 9 (UE 4.16 `RenderingCompositionGraph.cpp`).
    pub fn pass_buffer(v: &ViewInput, size: usize) -> Vec<[f32; 4]> {
        let mut o = vec![[0f32; 4]; size.max(10)];
        let (w, h) = (v.width.max(1) as f32, v.height.max(1) as f32);
        o[9] = [0.0, 0.0, 1.0 - 1.0 / w, 1.0 - 1.0 / h];
        o
    }

    pub fn collection_buffer(&self, amount: f32) -> Vec<[f32; 4]> {
        let mut o = vec![[0f32; 4]; self.sizes[Role::Collection as usize].max(self.slots.float4s)];
        o[self.slots.amount.0][self.slots.amount.1] = amount;
        o[self.slots.camera_offset.0][self.slots.camera_offset.1] = self.camera_offset;
        o[self.slots.top_left] = [self.top_left[0], self.top_left[1], self.top_left[2], 0.0];
        o[self.slots.bottom_right] = [self.bottom_right[0], self.bottom_right[1], self.bottom_right[2], 0.0];
        o
    }

    /// This frame's scope draws. Nothing is drawn while `DownScopePostProcessAmount` is 0 (UE: the component's `BlendWeight` is 0).
    pub fn frame(&self, i: &ScopeInput) -> ScopeFrame {
        if i.amount <= 0.0 { return ScopeFrame::default(); }
        let mut mat = self.material_cb.clone();
        mat.resize(self.sizes[Role::Material as usize].max(mat.len()), [0.0; 4]);
        ScopeFrame {
            buffer_writes: vec![
                (self.bufs[Role::Pass as usize], bytes(&Self::pass_buffer(&i.view, self.sizes[Role::Pass as usize]))),
                (self.bufs[Role::View as usize], bytes(&Self::view_buffer(&i.view, self.sizes[Role::View as usize]))),
                (self.bufs[Role::Collection as usize], bytes(&self.collection_buffer(i.amount))),
                (self.bufs[Role::Material as usize], bytes(&mat)),
                (self.ring_buf, bytes(&[[1.0 / i.view.tan_half.0, 1.0 / i.view.tan_half.1, self.camera_offset, 0.0]])),
            ],
            post: true, ring: i.mesh_visible,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    fn set() -> PakSet { PakSet::open_install(fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install")).unwrap() }
    const MATERIAL: &str = "/Game/Athena/HUD/reticles/DownScopePostProcess_Athena.DownScopePostProcess_Athena";
    const MESH: &str = "/Game/Weapons/FORT_Rifles/SniperScope/S_SniperScope_Proto.S_SniperScope_Proto";
    fn view(w: u32, h: u32) -> ViewInput { ViewInput { eye: Vec3::new(1., 2., 3.), forward: Vec3::new(0.3, -0.1, -1.).normalize(), up: Vec3::Y, tan_half: { let ty = 15f32.to_radians().mul_add(0.5, 0.0).tan() / (16. / 9.); (ty * w as f32 / h as f32, ty) }, near_m: 0.1, far_m: 3000., width: w, height: h, time: 0. } }

    #[test]
    fn the_collection_layout_matches_the_slots_the_original_program_reads() {
        let s = collection_slots(&set()).unwrap();
        // shader_4746 / 36116: DownScopePostProcessAmount = cb2[9].z, WeaponSightsCameraOffset = cb2[9].w, TopLeftScope = cb2[52], BottomRightScope = cb2[53]
        assert_eq!((s.amount, s.camera_offset, s.top_left, s.bottom_right, s.float4s), ((9, 2), (9, 3), 52, 53, 63));
    }

    #[test]
    fn the_scope_assets_resolve_from_the_1_8_data() {
        let set = set();
        let a = ScopeAssets::load(&set, MATERIAL, MESH, 30.).unwrap();
        assert_eq!(a.top_left, [2.5, -0.765987, 2.5]); assert_eq!(a.bottom_right, [-2.5, -0.765987, -2.5]);
        assert_eq!(a.sizes, [10, 153, 54, 8], "pass / View / MaterialCollection0 / Material, as declared by the original program");
        assert_eq!(a.textures.len(), 2); assert!(!a.textures[0].srgb, "SphereNormalsUncomp is SRGB false"); assert!(a.textures[1].srgb, "the Reticle texture keeps the default sRGB");
        assert!(a.cube.levels[0].1.iter().all(|f| f.len() == (a.cube.levels[0].0 as usize).pow(2) * 4), "6 square RGBA16F faces");
        // Material buffer: Lens Coloration, Reticle Color, RefractionAmount, Lens AO Radius / Value, Scope Reflection Intensity (instance chain)
        assert_eq!(&a.material_cb[2][..3], &[0.9075, 1.067917, 1.1]); assert_eq!(a.material_cb[4], [0.0; 4]); assert_eq!(a.material_cb[6][0], -0.1); assert_eq!(a.material_cb[7][..2], [0.443119, 4.055136]);
        assert!(a.wgsl.contains("fn ps_fx_main") && a.wgsl.contains("fn scope_vs"));
        assert!(a.positions.len() == 1418 && a.indices.len() == 4116, "LOD 0 of S_SniperScope_Proto");
    }

    #[test]
    fn the_lens_box_projects_the_two_sockets_through_the_original_mapping() {
        // the program's mapping (-x, z, y + offset) of the sockets through ViewToClip: at 16:9 and FOV 15 the box is 0.6496 NDC wide (x) and 1.155 tall (y) = square in pixels
        let (tl, br, off) = ([2.5f32, -0.765987, 2.5], [-2.5f32, -0.765987, -2.5], 30.0f32);
        let v = view(1920, 1080);
        let p = |s: [f32; 3]| { let (x, y, z) = (-s[0], s[2], s[1] + off); (x / z / v.tan_half.0, y / z / v.tan_half.1) };
        let (a, b) = (p(tl), p(br));
        assert!((b.0 - a.0 - 1.2991).abs() < 1e-3 && (a.1 - b.1 - 2.3105).abs() < 1e-3, "{a:?} {b:?}");
        let px = ((b.0 - a.0) * 960., (a.1 - b.1) * 540.);
        assert!((px.0 - px.1).abs() < 0.5, "the lens box is square in pixels at any aspect: {px:?}");
        let v2 = view(2560, 1080);
        let p2 = |s: [f32; 3]| { let (x, y, z) = (-s[0], s[2], s[1] + off); (x / z / v2.tan_half.0, y / z / v2.tan_half.1) };
        let (a2, b2) = (p2(tl), p2(br));
        assert!(((b2.0 - a2.0) * 1280. - (a2.1 - b2.1) * 540.).abs() < 0.5);
    }

    #[test]
    fn view_buffer_carries_the_projection_the_program_divides_by() {
        let v = view(1280, 720); let b = ScopeScene::view_buffer(&v, 153);
        assert_eq!(b.len(), 153); assert!((b[28][0] - 1.0 / v.tan_half.0).abs() < 1e-5 && (b[29][1] - 1.0 / v.tan_half.1).abs() < 1e-5 && b[30][3] == 1.0);
        assert_eq!(b[121], [1280., 720., 1. / 1280., 1. / 720.]); assert_eq!(b[152], [1.; 4]);
        let p = ScopeScene::pass_buffer(&v, 10); assert!((p[9][2] - (1. - 1. / 1280.)).abs() < 1e-6);
    }

    /// Runs the real post program + mesh on a synthetic LDR scene (a plain gradient: the test needs no game scene) and checks the structure of the picture the original
    /// data produces. `FNRT_SCOPE_PNG=<dir>` also writes the pictures.
    #[test]
    fn the_scope_pass_draws_a_lens_with_the_world_inside_and_a_black_outline_around_it() {
        use fn_render::wgpu;
        let gpu = match fn_render::Gpu::headless() { Ok(g) => g, Err(e) => { eprintln!("no GPU adapter, skipped: {e}"); return; } };
        let set = set();
        let mut r = ScopeRenderer::new(&gpu.device, &gpu.queue);
        let scene = ScopeAssets::load(&set, MATERIAL, MESH, 30.).unwrap().install(&mut r).unwrap();
        let (w, h) = (640u32, 360u32);
        let fmt = wgpu::TextureFormat::Rgba8Unorm;
        let src = gpu.device.create_texture(&wgpu::TextureDescriptor { label: Some("synthetic scene"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: fmt, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        // sky-ish grey-blue over grass-ish green with a 40 px grid so refraction / magnification differences are visible
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h { for x in 0..w { let o = ((y * w + x) * 4) as usize; let sky = y < h / 2; let grid = x % 40 == 0 || y % 40 == 0; let c = if grid { [255, 255, 255] } else if sky { [120, 150, 200] } else { [90, 140, 70] }; px[o..o + 3].copy_from_slice(&c); px[o + 3] = 255; } }
        gpu.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &px, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
        let src_view = src.create_view(&Default::default());
        let run = |r: &mut ScopeRenderer, amount: f32, mesh: bool| -> Vec<u8> {
            // restore the clean scene (the ring pass draws into it)
            gpu.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &px, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
            let out = fn_render::OffscreenTarget::new(&gpu.device, w, h, fmt);
            r.set_frame(scene.frame(&ScopeInput { view: view(w, h), amount, mesh_visible: mesh }));
            r.upload(); r.prepare(&src_view, fmt, fmt);
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            r.encode_ring(&mut enc, &src_view, fmt); r.encode_post(&mut enc, &out.view, fmt);
            gpu.queue.submit([enc.finish()]);
            out.read_rgba8(&gpu)
        };
        let at = |img: &[u8], x: u32, y: u32| { let o = ((y * w + x) * 4) as usize; [img[o] as f32, img[o + 1] as f32, img[o + 2] as f32] };
        let full = run(&mut r, 1.0, true);
        let half = run(&mut r, 0.5, false);
        assert!(scene.frame(&ScopeInput { view: view(w, h), amount: 0.0, mesh_visible: true }).post == false, "amount 0 draws nothing");
        if let Some(dir) = std::env::var_os("FNRT_SCOPE_PNG") {
            for (n, img) in [("scope_full", &full), ("scope_half_no_mesh", &half)] { let d = fn_texture::DecodedImage { width: w as usize, height: h as usize, pixels: fn_texture::Pixels::Rgba8(img.clone()) }; d.write_png(std::path::Path::new(&dir).join(format!("{n}.png"))).unwrap(); }
        }
        // outline: the mesh ring (irregular outer shape, S_SniperScope_Proto) is black at the sides of the lens at mid height
        for x in [80u32, 545] { assert_eq!(at(&full, x, 180), [0., 0., 0.], "ring pixel {x}"); }
        // ... and the world is visible outside the ring (only darkened by the program's vignette), and without the mesh nothing is black there
        assert!(at(&full, 6, 60).iter().all(|&c| c > 20.) && at(&half, 80, 180).iter().all(|&c| c > 20.));
        // lens: the world is visible (not black) around the centre, and the lens colouration (R 0.9075, G 1.068, B 1.1) tints it blue-ward
        let mut sum = [0f32; 3]; let mut n = 0.;
        for y in 100..260 { for x in 200..440 { let p = at(&full, x, y); for k in 0..3 { sum[k] += p[k]; } n += 1.; } }
        let m = sum.map(|s| s / n);
        assert!(m[0] > 20. && m[1] > 20. && m[2] > 20., "the lens shows the scene: {m:?}");
        // top / bottom edge at the centre column: the lens box is 2.31 NDC tall at 16:9 / FOV 15, so the lens (not the mesh) covers the full height there
        assert!(at(&full, 330, 2).iter().all(|&c| c > 0.), "no mesh above / below the lens at 16:9");
        // blend timing: the program computes out = r2 + amount * (effect - r2) (r2 independent of the amount once sat(amount * 5) = 1), so the output is linear in the amount
        let quarter = run(&mut r, 0.25, false);
        for (x, y) in [(250u32, 120u32), (330, 200), (400, 100), (300, 250)] {
            let (q, hf, fl) = (at(&quarter, x, y), at(&half, x, y), at(&full, x, y));
            for k in 0..3 { assert!(((hf[k] - q[k]) * 2. - (fl[k] - hf[k])).abs() <= 3., "pixel ({x},{y}) channel {k}: 0.25 {q:?} 0.5 {hf:?} 1.0 {fl:?}"); }
        }
    }
}
