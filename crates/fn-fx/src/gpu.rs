//! The renderer-facing half: uploads the decoded textures / meshes of the original packages to `fn_render::fx`, builds one pipeline + bind groups per material from the
//! assembled original programs ([`crate::program::assemble`]) and the material flags, and describes the vertex streams of the two vertex factories used here.
use crate::material::{Material, MaterialFlags};
use crate::program::{assemble, Assembled, ResKind, Stage, PIXEL_BINDING_BASE};
use fn_pak::PakSet;
use fn_render::fx::*;
use std::collections::HashMap;

/// The vertex factories the FX use (the FName of the type in the shader map).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vf { MeshParticle, Local, Sprite, /// `TGPUSkinVertexFactory<false>` (the name is stored without the template brackets).
    Skin,
    SkinExtra,
    /// GRASS (2026-10-06): `FInstancedStaticMeshVertexFactory` (instanced / hierarchical instanced static meshes: the foliage), the local vertex stream +
    /// the per-instance `FInstanceStream16` ([`ism_instance`], [`ISM_INSTANCE_STRIDE`]).
    InstancedStatic,
    /// LANDSCAPE MATERIAL (2026-10-07): `FLandscapeVertexFactory`: one float4 stream (`ATTRIBUTE0` = vertex x, y in the LOD's quads, subsection x, y;
    /// [`LANDSCAPE_VERTEX_STRIDE`]), the height and normal from the component's heightmap in the programs.
    Landscape,
    /// ROADS / SPLINES (2026-10-08): `FSplineMeshVertexFactory` (UE4.16 SplineMeshComponent.cpp:109: LocalVertexFactory.usf compiled with USE_SPLINEDEFORM):
    /// the same vertex stream as [`Vf::Local`] (`FSplineMeshSceneProxy::InitVertexFactory` builds `FLocalVertexFactory::FDataType`, :120-260), the deformation
    /// in the vertex programs from the per-component `$Globals` (fn-app spline_mesh.rs).
    SplineMesh,
    BeamTrail }
/// LANDSCAPE MATERIAL: bytes per landscape vertex in the FX path (`FLandscapeVertex` = 4 x uint8 `VET_UByte4`, LandscapeRender.cpp; carried as Float32x4 with the
/// same integer values: the programs read ATTRIBUTE0 as a float4).
pub const LANDSCAPE_VERTEX_STRIDE: u32 = 16;
impl Vf {
    pub fn type_name(self) -> &'static str { match self { Vf::MeshParticle => "FMeshParticleVertexFactory", Vf::Local => "FLocalVertexFactory", Vf::Sprite => "FParticleSpriteVertexFactory", Vf::Skin => "TGPUSkinVertexFactoryfalse", Vf::SkinExtra => "TGPUSkinVertexFactorytrue", Vf::InstancedStatic => "FInstancedStaticMeshVertexFactory", Vf::Landscape => "FLandscapeVertexFactory", Vf::SplineMesh => "FSplineMeshVertexFactory", Vf::BeamTrail => "FParticleBeamTrailVertexFactory" } }
}

/// GRASS: bytes per `FInstanceStream16` (UE4.16 StaticMeshResources.h:883; the D3D11 RHI supports `VET_Half2`, so `FStaticMeshInstanceData` uses the half
/// stream, InstancedStaticMesh.cpp:471): `InstanceOrigin` float4 (xyz = instance translation, w = PerInstanceRandom), `InstanceTransform1..3` half4 (rows 0..2 of
/// the instance matrix, w = hit proxy / selection = 0 in game), `InstanceLightmapAndShadowMapUVBias` short4N.
pub const ISM_INSTANCE_STRIDE: u32 = 48;
/// GRASS: `FInstancedStaticMeshVertexFactory::InitRHI` (InstancedStaticMesh.cpp:310-327) binds the UV streams to ATTRIBUTE4.. and repeats the LAST one up to
/// `(InstancedStaticMeshMaxTexCoord + 1) / 2` = 4 attributes (`InstancedStaticMeshMaxTexCoord` = 8, :27), so a program may read more UV pairs than the mesh has.
pub const ISM_UV_ATTRIBUTES: usize = 4;

/// GRASS: `FFloat16::Set` (UE4.16 Float16.h:133): sign copied, an exponent <= 112 (zero / denormal / too small) flushes to 0, >= 143 (too large, INF, NaN)
/// clamps to 65504, else the exponent is rebiased and the mantissa TRUNCATED (`>> 13`, no rounding).
pub fn ffloat16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 31) as u16) << 15;
    let exp = (b >> 23) & 0xff;
    if exp <= 112 { return sign; }
    if exp >= 143 { return sign | (30 << 10) | 1023; }
    sign | (((exp + 15 - 127) as u16) << 10) | ((b & 0x7f_ffff) >> 13) as u16
}

/// GRASS: one `FInstanceStream16` as `FInstanceStream::SetInstance(Transform, RandomInstanceID)` writes it (StaticMeshResources.h:795): `InstanceOrigin` =
/// (`Transform.M[3][0..2]`, random), `InstanceTransform1..3` = `Transform.M[0..2][0..2]` as halves with w = 0, the lightmap / shadow-map bias 0 (dynamic lighting).
/// `rows` = the UE row-vector matrix rows (row i = the image of local axis i: rotation * scale), `origin` = the translation (component space, cm).
pub fn ism_instance(origin: [f32; 3], random: f32, rows: [[f32; 3]; 3]) -> [u8; ISM_INSTANCE_STRIDE as usize] {
    let mut o = [0u8; ISM_INSTANCE_STRIDE as usize];
    for (k, f) in [origin[0], origin[1], origin[2], random].iter().enumerate() { o[4 * k..4 * k + 4].copy_from_slice(&f.to_le_bytes()); }
    for (r, row) in rows.iter().enumerate() {
        for (c, f) in row.iter().enumerate() { let at = 16 + 8 * r + 2 * c; o[at..at + 2].copy_from_slice(&ffloat16(*f).to_le_bytes()); }
    }
    o
}

/// Bytes per mesh-particle instance (`FMeshParticleInstanceVertex`): Transform[3] (3 x float4), SubUVParams (int4), SubUVLerpAndRelTime (float4, xy used),
/// DynamicParameter (float4), Color (float4), Velocity (float4).
pub const INSTANCE_STRIDE: u32 = 128;
/// Attribute indices of the mesh-particle instance stream (the `ATTRIBUTE n` of the vertex declaration the shaders were compiled against).
pub mod inst_attr { pub const TRANSFORM0: u32 = 8; pub const TRANSFORM1: u32 = 9; pub const TRANSFORM2: u32 = 10; pub const SUBUV: u32 = 11; pub const SUBUV_LERP_REL_TIME: u32 = 12; pub const DYNAMIC_PARAMETER: u32 = 13; pub const COLOR: u32 = 14; pub const VELOCITY: u32 = 15; }

/// Interleaved mesh vertex: position f32x3, TangentX / TangentZ / colour unorm8x4 as stored, then the UV sets.
pub fn mesh_stride(vf: Vf, num_uv: usize) -> u32 { if vf == Vf::BeamTrail { return BEAM_TRAIL_VERTEX_STRIDE; } if vf == Vf::Landscape { return LANDSCAPE_VERTEX_STRIDE; } if vf == Vf::Sprite { return SPRITE_VERTEX_STRIDE; } if vf == Vf::Skin { return SKIN_VERTEX_STRIDE; } if vf == Vf::SkinExtra { return SKIN_EXTRA_VERTEX_STRIDE; } 24 + match vf { Vf::MeshParticle => 8 * num_uv as u32, _ => 16 * num_uv.div_ceil(2) as u32 } }
/// Bytes per GPU-skin vertex (`fn_matshader::mesh::STRIDE`): position f32x3, TangentX, TangentZ, bone indices (u8x4, section local), bone weights (unorm8x4), UV0 f32x2, vertex colour (BGRA).
/// Vertex declaration of the vertex factories: at most 4 UV attributes (`ATTRIBUTE4..7`); the mesh-particle instance stream starts at `ATTRIBUTE8` (MeshParticleVertexFactory.usf), a mesh with more UV sets keeps the rest in its stride only.
pub const MAX_UV_ATTRS: usize = 4;
pub const SKIN_VERTEX_STRIDE: u32 = 40;
pub const SKIN_EXTRA_VERTEX_STRIDE: u32 = 48;
/// `FParticleSpriteVertex` + corner texture coordinate: Position+RelativeTime, OldPosition+ParticleId, Size.xy+Rotation+SubImageIndex, Color (4 x float4), TexCoord (float2).
pub const SPRITE_VERTEX_STRIDE: u32 = 72;

pub const BEAM_TRAIL_VERTEX_STRIDE: u32 = 96;
pub fn vertex_buffers(vf: Vf, num_uv: usize, instanced: bool) -> Vec<FxVertexBuffer> {
    if vf == Vf::BeamTrail {
        return vec![FxVertexBuffer { stride: BEAM_TRAIL_VERTEX_STRIDE, instance: false, attrs: vec![FxAttr { location: 0, offset: 0, format: FxFmt::Float32x4 }, FxAttr { location: 1, offset: 16, format: FxFmt::Float32x3 }, FxAttr { location: 2, offset: 32, format: FxFmt::Float32x4 },
            FxAttr { location: 3, offset: 48, format: FxFmt::Float32x4 }, FxAttr { location: 4, offset: 64, format: FxFmt::Float32x4 }, FxAttr { location: 5, offset: 80, format: FxFmt::Float32x4 }] }];
    }
    if vf == Vf::Landscape { return vec![FxVertexBuffer { stride: LANDSCAPE_VERTEX_STRIDE, instance: false, attrs: vec![FxAttr { location: 0, offset: 0, format: FxFmt::Float32x4 }] }]; }
    if vf == Vf::SkinExtra {
        let mut v = vertex_buffers(Vf::Skin, num_uv, instanced);
        v[0].stride = SKIN_EXTRA_VERTEX_STRIDE;
        v[0].attrs.push(FxAttr { location: 14, offset: 40, format: FxFmt::Uint8x4 });
        v[0].attrs.push(FxAttr { location: 15, offset: 44, format: FxFmt::Unorm8x4 });
        return v;
    }
    if vf == Vf::Skin { return vec![FxVertexBuffer { stride: SKIN_VERTEX_STRIDE, instance: false, attrs: vec![
        FxAttr { location: 0, offset: 0, format: FxFmt::Float32x3 }, FxAttr { location: 1, offset: 12, format: FxFmt::Unorm8x4 }, FxAttr { location: 2, offset: 16, format: FxFmt::Unorm8x4 }, FxAttr { location: 3, offset: 20, format: FxFmt::Uint8x4 },
        FxAttr { location: 4, offset: 24, format: FxFmt::Unorm8x4 }, FxAttr { location: 5, offset: 28, format: FxFmt::Float32x2 }, FxAttr { location: 13, offset: 36, format: FxFmt::Unorm8x4Bgra }] }]; }
    if vf == Vf::Sprite {
        return vec![FxVertexBuffer { stride: SPRITE_VERTEX_STRIDE, instance: false, attrs: vec![FxAttr { location: 0, offset: 0, format: FxFmt::Float32x4 }, FxAttr { location: 1, offset: 16, format: FxFmt::Float32x4 }, FxAttr { location: 2, offset: 32, format: FxFmt::Float32x4 }, FxAttr { location: 3, offset: 48, format: FxFmt::Float32x4 }, FxAttr { location: 4, offset: 64, format: FxFmt::Float32x2 }] }];
    }
    let mut a = vec![FxAttr { location: 0, offset: 0, format: FxFmt::Float32x3 }, FxAttr { location: 1, offset: 12, format: FxFmt::Unorm8x4 }, FxAttr { location: 2, offset: 16, format: FxFmt::Unorm8x4 }, FxAttr { location: 3, offset: 20, format: FxFmt::Unorm8x4Bgra }];
    match vf {
        Vf::MeshParticle => for i in 0..num_uv.min(MAX_UV_ATTRS) { a.push(FxAttr { location: 4 + i as u32, offset: 24 + 8 * i as u32, format: FxFmt::Float32x2 }); },
        // GRASS: the UV pairs, the last one repeated up to ISM_UV_ATTRIBUTES (InstancedStaticMesh.cpp:321); an odd last UV is zero-padded (z = 0 as D3D's
        // R32G32 / R16G16 element; w is not read by the programs)
        // INTEGRATION (2026-10-07): FLocalVertexFactory does the same (LocalVertexFactory.cpp:158-163 repeats the last pair up to MAX_STATIC_TEXCOORDS / 2 = 4,
        // Components.h:15): a program reading more UV pairs than the mesh has (MI_HeroTree_02's DEPRECATED_AnimatedTreeMaterial, ATTRIBUTE5) otherwise failed to build
        // ROADS / SPLINES: the spline VF's stream is the local one
        Vf::InstancedStatic | Vf::Local | Vf::SplineMesh => { let pairs = num_uv.div_ceil(2).min(ISM_UV_ATTRIBUTES); if pairs > 0 { for i in 0..ISM_UV_ATTRIBUTES { a.push(FxAttr { location: 4 + i as u32, offset: 24 + 16 * i.min(pairs - 1) as u32, format: FxFmt::Float32x4 }); } } }
        _ => for i in 0..num_uv.div_ceil(2).min(MAX_UV_ATTRS) { a.push(FxAttr { location: 4 + i as u32, offset: 24 + 16 * i as u32, format: FxFmt::Float32x4 }); },
    }
    let mut v = vec![FxVertexBuffer { stride: mesh_stride(vf, num_uv), instance: false, attrs: a }];
    if vf == Vf::InstancedStatic {
        // GRASS: ATTRIBUTE8..12 of the instance stream (InstancedStaticMesh.cpp:342-346 / :475-509: VET_Float4, 3 x VET_Half4, VET_Short4N)
        v.push(FxVertexBuffer { stride: ISM_INSTANCE_STRIDE, instance: true, attrs: vec![
            FxAttr { location: 8, offset: 0, format: FxFmt::Float32x4 }, FxAttr { location: 9, offset: 16, format: FxFmt::Float16x4 }, FxAttr { location: 10, offset: 24, format: FxFmt::Float16x4 },
            FxAttr { location: 11, offset: 32, format: FxFmt::Float16x4 }, FxAttr { location: 12, offset: 40, format: FxFmt::Snorm16x4 },
        ] });
        return v;
    }
    if instanced {
        v.push(FxVertexBuffer { stride: INSTANCE_STRIDE, instance: true, attrs: vec![
            FxAttr { location: inst_attr::TRANSFORM0, offset: 0, format: FxFmt::Float32x4 }, FxAttr { location: inst_attr::TRANSFORM1, offset: 16, format: FxFmt::Float32x4 }, FxAttr { location: inst_attr::TRANSFORM2, offset: 32, format: FxFmt::Float32x4 },
            FxAttr { location: inst_attr::SUBUV, offset: 48, format: FxFmt::Sint32x4 }, FxAttr { location: inst_attr::SUBUV_LERP_REL_TIME, offset: 64, format: FxFmt::Float32x4 },
            FxAttr { location: inst_attr::DYNAMIC_PARAMETER, offset: 80, format: FxFmt::Float32x4 }, FxAttr { location: inst_attr::COLOR, offset: 96, format: FxFmt::Float32x4 }, FxAttr { location: inst_attr::VELOCITY, offset: 112, format: FxFmt::Float32x4 },
        ] });
    }
    v
}

/// One uploaded static mesh (LOD 0, all sections: the FX meshes have one material slot) in the layout of [`vertex_buffers`].
pub struct MeshGpu {
    pub id: FxMeshId,
    pub num_uv: usize,
    pub vertex_count: usize,
    pub bounds_cm: ([f32; 3], [f32; 3]),
    pub name: String,
    pub material_paths: Vec<Option<String>>,
    /// (material slot, first index, index count) per section of LOD 0.
    pub sections: Vec<(usize, u32, u32)>,
    /// `UStaticMesh::GetBounds()` = `ExtendedBounds` (StaticMesh.cpp:1548; the tagged property, else the cooked render-data `Bounds` it is computed from,
    /// StaticMesh.cpp:2212): the local bounds the `Primitive` buffer is built from (`fn_render::primitive_ub`), cooked sphere radius included.
    pub bounds: fn_render::primitive_ub::BoxSphereBounds,
    /// LOD 0 carries a cooked distance field with `IsValidDistanceFieldVolume` (`Size.GetMax() > 0`, DistanceFieldAtlas.cpp:491): with `CastShadow` (UMeshComponent default
    /// true, MeshComponent.cpp:16), `bCastDynamicShadow` and `bAffectDistanceFieldLighting` (defaults true, PrimitiveComponent.cpp:171-173) this is
    /// `FStaticMeshSceneProxy::HasDistanceFieldRepresentation` (StaticMeshRender.cpp:1374), the `PerObjectGBufferData` bit of the Primitive buffer.
    pub has_distance_field: bool,
}

/// `ExtendedBounds` of the first `StaticMesh` export as tagged-property JSON (`{Origin, BoxExtent, SphereRadius}`), `None` when not serialized.
fn extended_bounds(set: &PakSet, long: &str) -> Option<fn_render::primitive_ub::BoxSphereBounds> {
    let pd = fn_uasset::PackageData::load(set, long).ok()?;
    let p = pd.package().ok()?;
    let i = (0..p.exports.len()).find(|&i| p.export_class_name(i) == "StaticMesh")?;
    let j = p.read_export(i).ok()?.props.to_json(&p);
    let e = j.get("ExtendedBounds")?;
    let v3 = |v: &serde_json::Value| -> Option<[f32; 3]> {
        if let Some(a) = v.as_array() { return (a.len() == 3).then(|| [a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32]); }
        Some([v.get("X")?.as_f64()? as f32, v.get("Y")?.as_f64()? as f32, v.get("Z")?.as_f64()? as f32])
    };
    Some(fn_render::primitive_ub::BoxSphereBounds { origin: v3(e.get("Origin")?)?, extent: v3(e.get("BoxExtent")?)?, radius: e.get("SphereRadius")?.as_f64()? as f32 })
}

pub fn load_mesh(set: &PakSet, fx: &mut FxRenderer, vf: Vf, object_path: &str) -> Result<MeshGpu, String> {
    let long = crate::data::long_package(object_path);
    let (a, x, b) = crate::data::package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let m = fn_mesh::StaticMesh::parse(&p).map_err(|e| format!("{long}: {e}"))?;
    let lod = m.lods.first().ok_or_else(|| format!("{long}: no LOD"))?;
    let num_uv = lod.num_tex_coords;
    let stride = mesh_stride(vf, num_uv) as usize;
    let n = lod.positions.len();
    let mut bytes = vec![0u8; n * stride];
    for i in 0..n {
        let o = i * stride;
        for (k, f) in lod.positions[i].iter().enumerate() { bytes[o + 4 * k..o + 4 * k + 4].copy_from_slice(&f.to_le_bytes()); }
        bytes[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
        bytes[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
        // vertex colour: the stream is read as BGRA; meshes without colours use white (the engine's null colour stream)
        let c = lod.colors.get(i).copied().unwrap_or([255; 4]);
        bytes[o + 20..o + 24].copy_from_slice(&c);
        for u in 0..num_uv {
            let uv = lod.uvs.get(u).and_then(|s| s.get(i)).copied().unwrap_or([0.; 2]);
            let at = match vf { Vf::MeshParticle => o + 24 + 8 * u, _ => o + 24 + 16 * (u / 2) + 8 * (u % 2) };
            bytes[at..at + 4].copy_from_slice(&uv[0].to_le_bytes()); bytes[at + 4..at + 8].copy_from_slice(&uv[1].to_le_bytes());
        }
    }
    let id = fx.create_mesh(&FxMeshDesc { label: m.name.clone(), vertices: bytes, indices: lod.indices.clone() });
    let (mn, mx) = m.position_bounds(0).unwrap_or(([0.; 3], [0.; 3]));
    let sections = lod.sections.iter().map(|s| (s.material_index.max(0) as usize, s.first_index, s.num_triangles * 3)).collect();
    // cooked bounds for the Primitive buffer (ExtendedBounds, else the render-data bounds: equal when no bounds extension is set)
    let bounds = extended_bounds(set, long).unwrap_or(fn_render::primitive_ub::BoxSphereBounds { origin: m.bounds.origin, extent: m.bounds.extent, radius: m.bounds.radius });
    let has_distance_field = m.distance_fields.first().cloned().flatten().map_or(false, |d| d.size.iter().copied().max().unwrap_or(0) > 0);
    Ok(MeshGpu { id, num_uv, vertex_count: n, bounds_cm: (mn, mx), name: m.name.clone(), material_paths: m.materials.iter().map(|x| x.material.clone()).collect(), sections, bounds, has_distance_field })
}

/// Textures of the original packages uploaded with their stored mip chain.
#[derive(Default)]
pub struct TextureCache { map: HashMap<String, (FxTexId, TexInfo)>, dummy_cube: Option<FxTexId>, dummy_3d: Option<FxTexId>, dummy_2d: Option<FxTexId>, tracked: bool, defer: bool }
#[derive(Clone, Debug)]
pub struct TexInfo { pub filter: String, pub address_x: String, pub address_y: String, pub srgb: bool, pub width: usize, pub height: usize, pub levels: usize, pub compression: String }

fn strip_enum(s: &str) -> String { s.rsplit("::").next().unwrap_or(s).to_owned() }

/// A texture's stored mip chain as the GPU takes it: (label, info, format, levels). RGBA8 decoded on the CPU, or (TEXTURE MEMORY, 2026-10-06) the cooked
/// BC blocks as they are ([`raw_bc_levels`]). The CPU half of [`TextureCache::load`].
pub type Decoded = (String, TexInfo, FxTexFormat, Vec<FxLevel>);
/// LOAD TIME (2026-10-06): mip chains decoded ahead of [`TextureCache::load`] by [`predecode_texture`] (object path -> result), consumed once.
static PREDECODED: std::sync::Mutex<Option<HashMap<String, Result<(Decoded, bool), String>>>> = std::sync::Mutex::new(None);

/// TEXTURE MEMORY (2026-10-06, BC passthrough): the GPU format that takes a cooked pixel format's 4x4 blocks unchanged (UE4.16 D3D11: PF_DXT1 / DXT3 / DXT5 /
/// BC4 / BC5 / BC7 = DXGI BC1 / BC2 / BC3 / BC4 / BC5 / BC7, D3D11Device.cpp:118-175).
fn bc_format(f: &fn_texture::PixelFormat) -> Option<FxTexFormat> {
    use fn_texture::PixelFormat as P;
    match f { P::Dxt1 => Some(FxTexFormat::Bc1), P::Dxt3 => Some(FxTexFormat::Bc2), P::Dxt5 => Some(FxTexFormat::Bc3), P::Bc4 => Some(FxTexFormat::Bc4), P::Bc5 => Some(FxTexFormat::Bc5), P::Bc7 => Some(FxTexFormat::Bc7), _ => None }
}
/// TEXTURE MEMORY: the blocks of the mips `mips` as they are, when the renderer takes BC (`fn_render::fx::bc_native_hint`), the cooked format has a BC GPU
/// format and the first level's size is a multiple of 4 (wgpu's rule for a BC texture, WebGPU included). Each level is cut to its `ceil(w / 4) * ceil(h / 4)`
/// blocks. `None` (any of that missing, or a level shorter than its blocks) = decode to RGBA8 as before.
pub fn raw_bc_levels(t: &fn_texture::Texture2D, mips: &[usize]) -> Option<(FxTexFormat, Vec<FxLevel>)> {
    if !fn_render::fx::bc_native_hint() { return None; }
    let format = bc_format(&t.format)?;
    let m0 = t.mips.get(*mips.first()?)?;
    if m0.width % 4 != 0 || m0.height % 4 != 0 { return None; }
    let mut levels = Vec::with_capacity(mips.len());
    for &i in mips {
        let m = t.mips.get(i)?;
        let n = t.format.mip_bytes(m.width, m.height)?;
        levels.push(FxLevel { width: m.width as u32, height: m.height as u32, depth: 1, rgba8: m.data()?.get(..n)?.to_vec() });
    }
    Some((format, levels))
}

/// TEXTURE MEMORY: BC levels from [`raw_bc_levels`] decoded to RGBA8 with the decode the RGBA path uses (`fn_texture::decode_mip`, default options: the
/// same bytes as `Texture2D::decode` of those mips).
pub fn decode_bc_levels(format: FxTexFormat, levels: &[FxLevel]) -> Result<Vec<FxLevel>, String> {
    use fn_texture::PixelFormat as P;
    let pf = match format { FxTexFormat::Bc1 => P::Dxt1, FxTexFormat::Bc2 => P::Dxt3, FxTexFormat::Bc3 => P::Dxt5, FxTexFormat::Bc4 => P::Bc4, FxTexFormat::Bc5 => P::Bc5, FxTexFormat::Bc7 => P::Bc7, FxTexFormat::Rgba8 => return Ok(levels.to_vec()) };
    levels.iter().map(|l| fn_texture::decode_mip(&pf, &l.rgba8, l.width as usize, l.height as usize, &fn_texture::DecodeOptions::default())
        .map(|img| FxLevel { width: l.width, height: l.height, depth: 1, rgba8: img.to_rgba8() }).map_err(|e| format!("{}x{} level: {e}", l.width, l.height))).collect()
}

fn decode_tiered(set: &PakSet, object_path: &str) -> Result<(Decoded, bool), String> {
    if crate::webtex::streaming() { if let Some((d, complete)) = crate::webtex::load_low(object_path) { return Ok((d, !complete)); } }
    decode_texture(set, object_path).map(|d| (d, false))
}

struct Upgrades { queue: std::collections::VecDeque<(FxTexId, String)>, ready: std::collections::VecDeque<(FxTexId, Decoded)>, ready_bytes: usize, enabled: bool, threads: usize, done: usize, bytes: u64, busy: usize }
static UPGRADES: std::sync::Mutex<Upgrades> = std::sync::Mutex::new(Upgrades { queue: std::collections::VecDeque::new(), ready: std::collections::VecDeque::new(), ready_bytes: 0, enabled: false, threads: 0, done: 0, bytes: 0, busy: 0 });
static PLAY_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DEFERRED_IN_PLAY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub fn note_play_started() { PLAY_STARTED.store(true, std::sync::atomic::Ordering::Relaxed); }
pub fn deferred_in_play() -> usize { DEFERRED_IN_PLAY.load(std::sync::atomic::Ordering::Relaxed) }
fn note_deferred(object_path: &str) {
    if !PLAY_STARTED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    let n = DEFERRED_IN_PLAY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n < 24 || (n + 1) % 25 == 0 { eprintln!("[webtex] deferred in play #{}: {object_path}", n + 1); }
}
pub fn upgrades_idle() -> bool { let g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner()); g.queue.is_empty() && g.ready.is_empty() && g.busy == 0 }
static UPGRADE_WAKE: std::sync::Condvar = std::sync::Condvar::new();
const UPGRADE_READY_CAP: usize = 48 << 20;
const UPGRADE_THREADS: usize = if cfg!(target_arch = "wasm32") { 4 } else { 2 };
static UPGRADE_PAKS: std::sync::OnceLock<std::sync::Arc<PakSet>> = std::sync::OnceLock::new();
pub fn set_upgrade_paks(set: std::sync::Arc<PakSet>) { let _ = UPGRADE_PAKS.set(set); }
fn deferred_misses() -> bool { cfg!(target_arch = "wasm32") && UPGRADE_PAKS.get().is_some() }

static TEX_SOURCE: std::sync::Mutex<Option<HashMap<usize, String>>> = std::sync::Mutex::new(None);
fn register_source(fx: &mut FxRenderer, id: FxTexId, object_path: &str) {
    if !cfg!(target_arch = "wasm32") || object_path.contains("Terrain") || object_path.contains("/Maps/") { return; }
    if let Ok(mut g) = TEX_SOURCE.lock() { g.get_or_insert_with(HashMap::new).insert(id.0, object_path.to_owned()); }
    fx.set_evictable(id);
}
fn queue_wanted(fx: &mut FxRenderer) {
    let w = fx.take_wanted_textures();
    if w.is_empty() { return; }
    let paths: Vec<(FxTexId, String)> = TEX_SOURCE.lock().ok().and_then(|g| g.as_ref().map(|m| w.iter().filter_map(|id| m.get(&id.0).map(|p| (*id, p.clone()))).collect())).unwrap_or_default();
    for (id, p) in paths { queue_upgrade(id, &p); }
}
fn queue_upgrade(id: FxTexId, object_path: &str) {
    let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner());
    if g.enabled { g.queue.push_front((id, object_path.to_owned())); } else { g.queue.push_back((id, object_path.to_owned())); }
    start_upgraders(&mut g);
    UPGRADE_WAKE.notify_one();
}
fn start_upgraders(g: &mut Upgrades) {
    if !g.enabled { return; }
    while g.threads < UPGRADE_THREADS {
        g.threads += 1;
        let spawned = std::thread::Builder::new().name("webtex upgrade".into()).spawn(upgrade_worker);
        if spawned.is_err() { g.threads -= 1; break; }
    }
}
fn upgrade_worker() {
    loop {
        let (id, path) = {
            let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if g.ready_bytes < UPGRADE_READY_CAP { if let Some(j) = g.queue.pop_front() { g.busy += 1; break j; } }
                g = UPGRADE_WAKE.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        let d = match crate::webtex::load(&path) { Some(Ok(d)) => d, _ => match UPGRADE_PAKS.get().map(|s| decode_texture_from_paks(s, &path)) { Some(Ok(d)) => d, _ => { let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner()); g.busy = g.busy.saturating_sub(1); continue } } };
        let n: usize = d.3.iter().map(|l| l.rgba8.len()).sum();
        let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner());
        g.busy = g.busy.saturating_sub(1);
        g.ready_bytes += n;
        g.ready.push_back((id, d));
    }
}
/// Starts the full-resolution reads of the textures created from their low mips (call when play starts).
pub fn enable_upgrades() {
    let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner());
    if g.enabled { return; }
    g.enabled = true;
    start_upgraders(&mut g);
    UPGRADE_WAKE.notify_all();
}
/// Swaps in up to `budget` bytes of full-resolution textures read by the upgrade threads; returns how many were swapped.
pub fn upgrade_step(fx: &mut FxRenderer, budget: usize) -> usize {
    queue_wanted(fx);
    let mut n = 0;
    let mut used = 0usize;
    while used < budget {
        let item = { let mut g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner()); let it = g.ready.pop_front(); if let Some((_, d)) = &it { let b: usize = d.3.iter().map(|l| l.rgba8.len()).sum(); g.ready_bytes = g.ready_bytes.saturating_sub(b); g.done += 1; g.bytes += b as u64; } it };
        let Some((id, (label, info, format, levels))) = item else { break };
        UPGRADE_WAKE.notify_all();
        let b: usize = levels.iter().map(|l| l.rgba8.len()).sum();
        let (format, levels) = if format.is_bc() && !fx.accepts_bc(FxTexDim::D2, levels[0].width, levels[0].height) { match decode_bc_levels(format, &levels) { Ok(l) => (FxTexFormat::Rgba8, l), Err(_) => continue } } else { (format, levels) };
        fx.replace_texture(id, &FxTextureDesc { label, dim: FxTexDim::D2, srgb: info.srgb, levels, format });
        used += b;
        n += 1;
    }
    n
}
/// (queued, ready, swapped, MB swapped)
pub fn upgrade_stats() -> (usize, usize, usize, f64) { let g = UPGRADES.lock().unwrap_or_else(|e| e.into_inner()); (g.queue.len(), g.ready.len(), g.done, g.bytes as f64 / 1048576.) }

fn decode_texture(set: &PakSet, object_path: &str) -> Result<Decoded, String> {
    if let Some(r) = crate::webtex::load(object_path) { return r; }
    decode_texture_from_paks(set, object_path)
}

pub fn decode_texture_from_paks(set: &PakSet, object_path: &str) -> Result<Decoded, String> {
    let long = crate::data::long_package(object_path);
    let (a, x, b) = crate::data::package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    // LANDSCAPE MATERIAL (2026-10-07): a texture export of a map package (the landscape sublevels' heightmaps / weightmaps `Athena_Terrain_LS_xx.Texture2D_k`)
    // is read by its object name; a texture package by its one texture
    let leaf = object_path.rsplit('.').next().unwrap_or(object_path);
    let pkg_leaf = long.rsplit('/').next().unwrap_or(long);
    let in_map = set.read_game(long, "uasset").is_err() || (leaf != pkg_leaf && (0..p.exports.len()).any(|i| p.exports[i].object_name == leaf && p.export_class(i) == "Texture2D"));
    let t = if in_map {
        let idx = (0..p.exports.len()).find(|&i| p.exports[i].object_name == leaf && p.export_class(i) == "Texture2D").ok_or_else(|| format!("{object_path}: no Texture2D export"))?;
        fn_texture::Texture2D::parse_export(&p, idx).map_err(|e| format!("{object_path}: {e}"))?
    } else { fn_texture::Texture2D::parse(&p).map_err(|e| format!("{long}: {e}"))? };
    if t.is_cube() { return Err(format!("{long}: cube map unsupported here")); }
    let mips = t.available_mips();
    let (format, levels) = match raw_bc_levels(&t, &mips) {
        Some(r) => r,
        None => {
            let mut levels = vec![];
            for i in mips {
                let img = t.decode(i, fn_texture::DecodeOptions::default()).map_err(|e| format!("{long}: mip {i}: {e}"))?;
                levels.push(FxLevel { width: img.width as u32, height: img.height as u32, depth: 1, rgba8: img.to_rgba8() });
            }
            (FxTexFormat::Rgba8, levels)
        }
    };
    if levels.is_empty() { return Err(format!("{long}: no mip available")); }
    let info = TexInfo { filter: t.props.text("Filter").map(strip_enum).unwrap_or_else(|| "TF_Default".into()), address_x: t.address_x.clone(), address_y: t.address_y.clone(), srgb: t.srgb, width: levels[0].width as usize, height: levels[0].height as usize, levels: levels.len(), compression: t.compression_settings.clone() };
    // TEXTURE MEMORY (debug-only): `FN_TEX_LOG=1` lists every texture read here: cooked format, size, the GPU format it gets
    if fn_live::var("FN_TEX_LOG").map_or(false, |v| v == "1") {
        let bytes: usize = levels.iter().map(|l| l.rgba8.len()).sum();
        eprintln!("[tex] {long} {} {}x{} mips {} srgb {} {} -> {format:?} ({:.2} MB)", t.format.name(), info.width, info.height, info.levels, info.srgb, info.compression, bytes as f64 / 1048576.);
    }
    Ok((t.name.clone(), info, format, levels))
}

/// Byte budget of the mip chains [`predecode_texture`] holds (decoded RGBA8, or the raw blocks of a BC texture: 4-8x smaller). Past it a texture decodes
/// on demand as before.
/// 128 MB in the browser build (wasm32): the held RGBA8 chains raised its peak heap ~0.5 GB (verification/browser/launch3), but with the BC
/// passthrough they are the raw blocks (~5x smaller), and reading them on the worker spares the page's main thread its texture reads during the
/// loading screen (browser agent, 2026-10-06).
/// BROWSER PORT (2026-10-07): 768 MB in the browser: the POI streaming prefetch (fn-web stream_web.rs) decodes the textures of the kinds it reads ahead
/// here, so the main thread only uploads them (the shader maps no longer fill the heap: fn-fx cooked.rs decodes programs on first use).
const PREDECODE_BUDGET: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 512 << 20 };
static PREDECODED_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub fn predecoded_bytes() -> usize { PREDECODED_BYTES.load(std::sync::atomic::Ordering::Relaxed) }
fn decoded_bytes(r: &Result<(Decoded, bool), String>) -> usize { r.as_ref().map_or(0, |((_, _, _, l), _)| l.iter().map(|x| x.rgba8.len()).sum()) }

/// LOAD TIME (2026-10-06): decodes `object_path`'s stored mip chain (TEXTURE MEMORY: or copies its BC blocks, [`raw_bc_levels`]) on the calling (worker) thread so that a later [`TextureCache::load`] of the same
/// path only uploads it. The same decode, moved earlier: callers prewarm what the first frame is about to load (`statics_render::prewarm`).
/// Bounded by [`PREDECODE_BUDGET`].
pub fn predecode_texture(set: &PakSet, object_path: &str) { predecode(set, object_path, false) }
pub fn predecode_texture_full(set: &PakSet, object_path: &str) { predecode(set, object_path, true) }
pub fn predecode_texture_urgent(set: &PakSet, object_path: &str) { predecode_inner(set, object_path, false, true) }
fn predecode(set: &PakSet, object_path: &str, full: bool) { predecode_inner(set, object_path, full, false) }
fn predecode_inner(set: &PakSet, object_path: &str, full: bool, urgent: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if !urgent && PREDECODED_BYTES.load(Relaxed) >= PREDECODE_BUDGET { return; }
    if PREDECODED.lock().ok().and_then(|g| g.as_ref().map(|m| m.contains_key(object_path))).unwrap_or(false) { return; }
    let r = if full { decode_texture(set, object_path).map(|d| (d, false)).or_else(|_| decode_tiered(set, object_path)) } else { decode_tiered(set, object_path) };
    if let Ok(mut g) = PREDECODED.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if !m.contains_key(object_path) { PREDECODED_BYTES.fetch_add(decoded_bytes(&r), Relaxed); m.insert(object_path.to_owned(), r); }
    }
}
static UPLOADED: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
pub fn is_uploaded(object_path: &str) -> bool { UPLOADED.lock().ok().and_then(|g| g.as_ref().map(|s| s.contains(object_path))).unwrap_or(false) }
pub fn is_predecoded(object_path: &str) -> bool { PREDECODED.lock().ok().and_then(|g| g.as_ref().map(|m| m.contains_key(object_path))).unwrap_or(false) }
/// Drops the predecoded chains of `paths` that nobody loaded (a prewarm frees its own leftovers once its build is done).
pub fn drop_predecoded(paths: &[String]) {
    if let Ok(mut g) = PREDECODED.lock() { if let Some(m) = g.as_mut() { for p in paths { if let Some(r) = m.remove(p) { PREDECODED_BYTES.fetch_sub(decoded_bytes(&r), std::sync::atomic::Ordering::Relaxed); } } } }
}
fn take_predecoded(object_path: &str) -> Option<Result<(Decoded, bool), String>> {
    let r = PREDECODED.lock().ok()?.as_mut()?.remove(object_path)?;
    PREDECODED_BYTES.fetch_sub(decoded_bytes(&r), std::sync::atomic::Ordering::Relaxed);
    Some(r)
}

static SHARED_TEX: std::sync::Mutex<Option<HashMap<(u64, String), (FxTexId, TexInfo)>>> = std::sync::Mutex::new(None);
fn shared_get(fx: &FxRenderer, object_path: &str) -> Option<(FxTexId, TexInfo)> { SHARED_TEX.lock().ok()?.as_ref()?.get(&(fx.instance(), object_path.to_owned())).cloned() }
fn shared_put(fx: &FxRenderer, object_path: &str, v: &(FxTexId, TexInfo)) {
    if let Ok(mut g) = SHARED_TEX.lock() { g.get_or_insert_with(HashMap::new).entry((fx.instance(), object_path.to_owned())).or_insert_with(|| v.clone()); }
    if let Ok(mut g) = UPLOADED.lock() { g.get_or_insert_with(Default::default).insert(object_path.to_owned()); }
}

impl TextureCache {
    pub fn tracked() -> TextureCache { TextureCache { tracked: true, ..Default::default() } }
    pub fn deferred() -> TextureCache { TextureCache { defer: true, ..Default::default() } }
    pub fn load(&mut self, set: &PakSet, fx: &mut FxRenderer, object_path: &str) -> Result<(FxTexId, TexInfo), String> {
        if let Some(t) = self.map.get(object_path) { if PREDECODED_BYTES.load(std::sync::atomic::Ordering::Relaxed) > 0 { let _ = take_predecoded(object_path); } return Ok(t.clone()); }
        if let Some(t) = shared_get(fx, object_path) { let _ = take_predecoded(object_path); self.map.insert(object_path.to_owned(), t.clone()); return Ok(t); }
        let pre = take_predecoded(object_path);
        if pre.is_none() && (self.tracked || self.defer) && deferred_misses() {
            let info = crate::webtex::index().and_then(|i| i.find(object_path)).map(|e| e.info.clone()).unwrap_or_else(|| TexInfo { filter: "TF_Default".into(), address_x: "TA_Wrap".into(), address_y: "TA_Wrap".into(), srgb: false, width: 4, height: 4, levels: 1, compression: String::new() });
            note_deferred(object_path);
            let id = fx.create_texture(&FxTextureDesc { label: object_path.rsplit('.').next().unwrap_or(object_path).to_owned(), dim: FxTexDim::D2, srgb: info.srgb, levels: vec![FxLevel { width: 4, height: 4, depth: 1, rgba8: [128u8, 128, 128, 255].repeat(16) }], format: FxTexFormat::Rgba8 });
            queue_upgrade(id, object_path);
            if self.tracked { register_source(fx, id, object_path); }
            if let Ok(mut g) = UPLOADED.lock() { g.get_or_insert_with(Default::default).insert(object_path.to_owned()); }
            self.map.insert(object_path.to_owned(), (id, info.clone()));
            shared_put(fx, object_path, &(id, info.clone()));
            return Ok((id, info));
        }
        let ((label, info, format, levels), upgrade) = match pre { Some(r) => r?, None => { let t0 = std::time::Instant::now(); let r = decode_tiered(set, object_path)?; let ms = t0.elapsed().as_secs_f32() * 1000.; if ms > 20. { eprintln!("[texmiss] {} {object_path} {ms:.0} ms, predecoded {} MB", if self.tracked { "statics" } else { "other" }, PREDECODED_BYTES.load(std::sync::atomic::Ordering::Relaxed) >> 20); } r } };
        // TEXTURE MEMORY: blocks read while the hint said BC (predecode) for a renderer that does not take them: today's decode, here
        let (format, levels) = if format.is_bc() && !fx.accepts_bc(FxTexDim::D2, levels[0].width, levels[0].height) { (FxTexFormat::Rgba8, decode_bc_levels(format, &levels).map_err(|e| format!("{label}: {e}"))?) } else { (format, levels) };
        let id = fx.create_texture(&FxTextureDesc { label, dim: FxTexDim::D2, srgb: info.srgb, levels, format });
        if upgrade { queue_upgrade(id, object_path); }
        if self.tracked { register_source(fx, id, object_path); }
        if self.tracked { if let Ok(mut g) = UPLOADED.lock() { g.get_or_insert_with(Default::default).insert(object_path.to_owned()); } }
        self.map.insert(object_path.to_owned(), (id, info.clone()));
        shared_put(fx, object_path, &(id, info.clone()));
        Ok((id, info))
    }
    /// `GWhiteTexture` (`FColoredTexture<255,255,255,255>`, UE4.16 RenderUtils.cpp:208, 1x1 RGBA8 white): what `FUniformExpressionSet::FillUniformBuffer` binds for
    /// a material texture expression whose value is NULL (MaterialUniformExpressions.cpp:394-399), e.g. an out-of-range `TextureIndex` (`GetIndexedTexture`,
    /// MaterialUniformExpressions.h:268-286).
    pub fn white_2d(&mut self, fx: &mut FxRenderer) -> FxTexId {
        if let Some(t) = self.map.get("<GWhiteTexture>") { return t.0; }
        let id = fx.create_texture(&FxTextureDesc { label: "GWhiteTexture".into(), dim: FxTexDim::D2, srgb: false, levels: vec![FxLevel { width: 1, height: 1, depth: 1, rgba8: vec![255; 4] }], format: FxTexFormat::Rgba8 });
        self.map.insert("<GWhiteTexture>".into(), (id, TexInfo { filter: "TF_Nearest".into(), address_x: "TA_Wrap".into(), address_y: "TA_Wrap".into(), srgb: false, width: 1, height: 1, levels: 1, compression: String::new() }));
        id
    }
    pub fn dummy(&mut self, fx: &mut FxRenderer, dim: FxTexDim) -> FxTexId {
        let slot = match dim { FxTexDim::D2 => &mut self.dummy_2d, FxTexDim::D3 => &mut self.dummy_3d, FxTexDim::Cube => &mut self.dummy_cube };
        if let Some(t) = slot { return *t; }
        let layers = if dim == FxTexDim::Cube { 6 } else { 1 };
        let id = fx.create_texture(&FxTextureDesc { label: "fx dummy".into(), dim, srgb: false, levels: vec![FxLevel { width: 1, height: 1, depth: 1, rgba8: vec![0; 4 * layers] }], format: FxTexFormat::Rgba8 });
        *slot = Some(id);
        id
    }
}

/// The sampler state of a material texture slot: addressing from the texture asset (`sampler_source` 0) or the world group wrap / clamp (1 / 2); filtering from the
/// texture's `Filter` (`TF_Default` = the world texture group's anisotropic trilinear).
pub fn sampler_for(info: &TexInfo, sampler_source: i32) -> FxSamplerDesc {
    let addr = |s: &str| match s { "TA_Clamp" => FxAddress::Clamp, "TA_Mirror" => FxAddress::Mirror, _ => FxAddress::Wrap };
    let address = match sampler_source { 1 => FxAddress::Wrap, 2 => FxAddress::Clamp, _ => addr(&info.address_x) };
    match info.filter.as_str() {
        "TF_Nearest" => FxSamplerDesc { mag: FxFilter::Nearest, min: FxFilter::Nearest, mip: FxFilter::Nearest, address, aniso: 1 },
        "TF_Bilinear" => FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Nearest, address, aniso: 1 },
        "TF_Trilinear" => FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Linear, address, aniso: 1 },
        _ => FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Linear, address, aniso: 8 },
    }
}

/// Uniform buffers shared by every draw of a frame.
#[derive(Clone, Copy, Debug)]
pub struct SharedBuffers {
    pub view: FxBufId,
    pub primitive: FxBufId,
    /// `FParticleSpriteUniformParameters` ([`crate::view::sprite_vf_buffer`]), written once.
    pub sprite_vf: FxBufId,
    /// Lighting of lit FX materials (`FxLight`: to-sun direction, sun radiance, sky radiance; port world).
    pub light: FxBufId,
}

pub struct MaterialGpu {
    pub pipe: FxPipeId,
    pub binds: Vec<(u32, FxBindId)>,
    /// The `Material` uniform buffer of this material instance (rewrite it with [`Material::packed`] when a parameter changes).
    pub material_buf: FxBufId,
    pub material_registers: usize,
    /// The pixel shader's unnamed globals buffer holding the per-frame expressions: (buffer, float index of scalar expression 0 under the OLD inference, float4 count).
    /// The offsets now come from the program's serialized `FShaderParameter`s ([`Self::globals`], `crate::shader_params`; on `M_SafeZoneSphere` they confirm floats 9 / 13 / 17 / 19);
    /// the "last declared globals" inference is only the fallback for a program whose parameters did not parse (the middle value is meaningful only then).
    pub per_frame: Option<(FxBufId, usize, usize)>,
    /// Material-owned unnamed (`$Globals`) constant buffers laid out from the serialized parameters: per-frame expressions (scalars, vectors, previous-frame ones) of
    /// either stage and the base-pass VS velocity globals (`PreviousLocalToWorld`, `SkipOutputVelocity`). Filled by [`Self::per_frame_writes`] / [`Self::vs_globals_bytes`].
    pub globals: Vec<StageGlobals>,
    /// (group, entry) of the vertex stage's unnamed globals buffer, to swap a per-primitive one in ([`Self::bind_variant_globals`]).
    vs_globals_slots: Vec<(usize, usize)>,
    /// Shared with every material of the same shader map and permutation (`ASSEMBLED`).
    pub assembled: std::sync::Arc<Assembled>,
    pub flags: MaterialFlags,
    pub translucent: bool,
    /// Bind group entries of the groups 0..3 and the (group, entry index) slots holding the `Primitive` buffer, to create one bind group set per primitive buffer.
    entries: Vec<Vec<FxBindEntry>>,
    primitive_slots: Vec<(usize, usize)>,
    material_slots: Vec<(usize, usize)>,
    spritevf_slots: Vec<(usize, usize)>,
    group_count: usize,
    /// FOG INPUTS (2026-10-05): the height-fog values the vertex program binds in its `$Globals` buffer (registered with the FX pass for every buffer bound
    /// there, also the per-primitive ones of [`Self::bind_variant_globals`]).
    vs_fog: Vec<FxFogSlot>,
    /// LANDSCAPE MATERIAL (2026-10-07): (group, entry, kind) of the per-component bindings of the landscape vertex factory: [`LandscapeSlot`].
    landscape_slots: Vec<(usize, usize, LandscapeSlot)>,
}
/// FOG INPUTS (2026-10-05): the program's `FHeightFogShaderParameters` (`crate::shader_params::HeightFogParams`) when its serialized layout validates.
fn height_fog_of(prog: &crate::cooked::Program) -> Option<crate::shader_params::HeightFogParams> { prog.params.as_ref().and_then(|p| p.height_fog(&prog.shader_type)) }
/// FOG INPUTS: the bound height-fog values of `prog` that live in its constant buffer `slot`, as FX pass slots (`fn_render::fog::shader_constants` order).
fn fog_slots(prog: &crate::cooked::Program, slot: u32) -> Vec<FxFogSlot> {
    let Some(f) = height_fog_of(prog) else { return vec![] };
    f.values.iter().enumerate().filter(|(_, v)| v.is_bound() && v.buffer_index as u32 == slot).map(|(k, v)| FxFogSlot { field: k as u8, byte_offset: v.base_index as u32, num_bytes: v.num_bytes as u32 }).collect()
}
/// One program's unnamed (`$Globals`) constant buffer owned by the material (see [`MaterialGpu::globals`]).
#[derive(Clone, Debug)]
pub struct StageGlobals {
    pub vertex: bool,
    pub buf: FxBufId,
    /// Constant buffer slot (`FShaderParameter::BufferIndex`) and its size in float4s.
    pub slot: u16,
    pub float4s: usize,
    pub shader_type: String,
    pub params: crate::shader_params::MaterialShaderParams,
}

impl MaterialGpu {
    /// [`Self::bind_variant_ext`] that also swaps the vertex stage's globals buffer (a per-primitive buffer filled with [`Self::vs_globals_bytes`]: the base-pass VS
    /// reads `PreviousLocalToWorld` there, which is per primitive).
    pub fn bind_variant_globals(&self, fx: &mut FxRenderer, primitive: FxBufId, material: Option<FxBufId>, vs_globals: Option<FxBufId>) -> Vec<(u32, FxBindId)> {
        if let Some(v) = vs_globals { fx.register_height_fog(v, self.vs_fog.clone()); } // FOG INPUTS: a per-primitive VS globals buffer gets the fog values too
        let mut out = vec![];
        for (g, e) in self.entries.iter().enumerate().take(self.group_count) {
            let mut e = e.clone();
            for (sg, si) in &self.primitive_slots { if *sg == g { e[*si].res = FxRes::Buffer(primitive); } }
            if let Some(m) = material { for (sg, si) in &self.material_slots { if *sg == g { e[*si].res = FxRes::Buffer(m); } } }
            if let Some(v) = vs_globals { for (sg, si) in &self.vs_globals_slots { if *sg == g { e[*si].res = FxRes::Buffer(v); } } }
            out.push((g as u32, fx.create_bind_group(self.pipe, g as u32, e)));
        }
        out
    }
    /// The vertex stage's material-owned globals layout, when it has one (create a per-primitive buffer of `float4s * 16` bytes for [`Self::bind_variant_globals`]).
    pub fn vs_globals(&self) -> Option<&StageGlobals> { self.globals.iter().find(|g| g.vertex) }
    /// Contents of the vertex stage's globals for one primitive: the per-frame expressions at `t` and the base-pass velocity globals as
    /// `TBasePassVertexShaderPolicyParamType::SetMesh` sets them (BasePassRendering.inl:31): `prev_local_to_world` = `Some(previous frame's LocalToWorld)` when the
    /// primitive has velocity on the base pass (`SkipOutputVelocity` 0), `None` = identity and `SkipOutputVelocity` 1 (camera cut / no velocity).
    pub fn vs_globals_bytes(&self, mat: &Material, t: crate::uniform::MaterialTime, prev_local_to_world: Option<glam::Mat4>) -> Option<Vec<u8>> {
        let g = self.vs_globals()?;
        let mut floats = vec![0f32; g.float4s * 4];
        crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, t);
        write_base_pass_velocity(&mut floats, g, prev_local_to_world);
        write_instancing(&mut floats, g, &InstancingParams::default()); // GRASS: an instanced program without batch data draws as SetMesh without user data
        Some(crate::view::f32_bytes(&floats))
    }
    /// GRASS: [`Self::vs_globals_bytes`] of an `FInstancedStaticMeshVertexFactory` program for one batch element: + the vertex factory's `SetMesh` values.
    pub fn vs_globals_bytes_instanced(&self, mat: &Material, t: crate::uniform::MaterialTime, prev_local_to_world: Option<glam::Mat4>, inst: &InstancingParams) -> Option<Vec<u8>> {
        let g = self.vs_globals()?;
        let mut floats = vec![0f32; g.float4s * 4];
        crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, t);
        write_base_pass_velocity(&mut floats, g, prev_local_to_world);
        write_instancing(&mut floats, g, inst);
        Some(crate::view::f32_bytes(&floats))
    }
    /// Bind groups of this material that read `primitive` as their `Primitive` uniform buffer.
    pub fn bind_variant(&self, fx: &mut FxRenderer, primitive: FxBufId, material: Option<FxBufId>) -> Vec<(u32, FxBindId)> { self.bind_variant_ext(fx, primitive, material, None) }
    /// [`Self::bind_variant`] that can also swap the `SpriteVF` uniform buffer (the per-emitter `FParticleSpriteUniformParameters`).
    pub fn bind_variant_ext(&self, fx: &mut FxRenderer, primitive: FxBufId, material: Option<FxBufId>, sprite_vf: Option<FxBufId>) -> Vec<(u32, FxBindId)> {
        let mut out = vec![];
        for (g, e) in self.entries.iter().enumerate().take(self.group_count) {
            let mut e = e.clone();
            for (sg, si) in &self.primitive_slots { if *sg == g { e[*si].res = FxRes::Buffer(primitive); } }
            if let Some(m) = material { for (sg, si) in &self.material_slots { if *sg == g { e[*si].res = FxRes::Buffer(m); } } }
            if let Some(s) = sprite_vf { for (sg, si) in &self.spritevf_slots { if *sg == g { e[*si].res = FxRes::Buffer(s); } } }
            out.push((g as u32, fx.create_bind_group(self.pipe, g as u32, e)));
        }
        out
    }
}

/// GRASS: UE's `MIN_flt` / `MAX_flt` (NumericLimits.h: FLT_MIN 1.175494351e-38, the smallest POSITIVE normal float, and FLT_MAX).
pub const UE_MIN_FLT: f32 = 1.175494351e-38;
pub const UE_MAX_FLT: f32 = 3.402823466e+38;

/// GRASS: the loose `$Globals` `FInstancedStaticMeshVertexFactoryShaderParameters::SetMesh` writes for one batch element (UE4.16 InstancedStaticMesh.cpp:1903-2045),
/// in the program's slots ([`crate::shader_params::InstancedVfParams`]). [`Default`] = SetMesh without user data / LOD range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstancingParams { pub fade_out: [f32; 4], pub view_z_compare_zero: [f32; 4], pub view_z_compare_one: [f32; 4], pub view_z_constant: [f32; 4], pub world_view_origin_zero: [f32; 4], pub world_view_origin_one: [f32; 4] }
impl Default for InstancingParams {
    fn default() -> Self {
        InstancingParams { fade_out: [UE_MAX_FLT, 0., 1., 1.], view_z_compare_zero: [UE_MIN_FLT, UE_MIN_FLT, UE_MAX_FLT, 1.], view_z_compare_one: [UE_MIN_FLT, UE_MIN_FLT, UE_MAX_FLT, 0.],
            view_z_constant: [0.; 4], world_view_origin_zero: [0.; 4], world_view_origin_one: [0., 0., 0., 1.] }
    }
}
/// GRASS: `ComputeBoundsDrawDistance` (UE4.16 SceneManagement.cpp): the distance at which a sphere of `sphere_radius` has the projected diameter `screen_size`;
/// `proj_scale` = (ProjMatrix.M[0][0], M[1][1]) = (1 / tan half fov x, 1 / tan half fov y).
pub fn compute_bounds_draw_distance(screen_size: f32, sphere_radius: f32, proj_scale: (f32, f32)) -> f32 {
    let multiple = (0.5 * proj_scale.0).max(0.5 * proj_scale.1);
    let screen_radius = (screen_size * 0.5).max(1.0e-8); // SMALL_NUMBER
    multiple * sphere_radius / screen_radius
}
/// GRASS: what `SetMesh` reads for a hierarchical instanced batch element ([`InstancingParams::hism`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HismBatch {
    /// `InstancingUserData->StartCullDistance` / `EndCullDistance` (the component's `InstanceStartCullDistance` / `InstanceEndCullDistance`, cm).
    pub start_cull_cm: f32,
    pub end_cull_cm: f32,
    /// `GetCachedScalabilityCVars().ViewDistanceScale` (`r.ViewDistanceScale`).
    pub view_distance_scale: f32,
    /// `BatchElement.InstancedLODIndex`, `InstancingUserData->MinLOD` (FirstLOD) and the render data's LOD count / `ScreenSize[]` / `Bounds.SphereRadius`.
    pub lod: usize,
    pub first_lod: usize,
    pub screen_size: [f32; 8],
    pub num_lods: usize,
    pub sphere_radius: f32,
    pub proj_scale: (f32, f32),
    /// `foliage.MinimumScreenSize` (0.000005), `foliage.LODDistanceScale` (1), `foliage.RandomLODRange` (0): HierarchicalInstancedStaticMesh.cpp:72-85 defaults, not
    /// set by the 1.8 cooked config.
    pub min_screen_size: f32,
    pub lod_scale: f32,
    pub lod_random: f32,
    /// `View.GetTemporalLODOrigin(0 / 1)` (world cm) and `View.GetTemporalLODTransition()` ([`TemporalLod`]).
    pub temporal_origin: [[f32; 3]; 2],
    pub temporal_alpha: f32,
    /// `BatchElement.InstancedLODRange` != 0: the HISM proxy sets it when its LODs blend (`bDitheredLODTransitions` and `foliage.DitheredLOD` 1,
    /// HierarchicalInstancedStaticMesh.cpp:1233 / :1338).
    pub dithered: bool,
}
impl InstancingParams {
    /// `SetMesh` (InstancedStaticMesh.cpp:1909-2045) for a hierarchical instanced batch element in game (`bRenderSelected` / `bRenderUnselected` both set:
    /// selection outlines are editor-only; `foliage.CullAllInVertexShader` 0). The dithered branch keeps the engine's arithmetic as written, including
    /// `FinalCull = min(.., EndCullDistance * MaxDrawDistanceScale) * MaxDrawDistanceScale` (the scale applies twice to the vertex-shader cut).
    pub fn hism(b: &HismBatch) -> InstancingParams {
        let mut p = InstancingParams::default();
        if b.dithered {
            let s = b.view_distance_scale;
            p.view_z_constant = [if b.lod > 0 { -1.0 } else { 0.0 }, 0.0, 1.0, 0.0];
            p.view_z_constant[1] -= p.view_z_constant[0];
            p.view_z_constant[2] -= p.view_z_constant[0] + p.view_z_constant[1];
            for sample in 0..2 {
                let c = if sample == 0 { &mut p.view_z_compare_zero } else { &mut p.view_z_compare_one };
                let mut final_cull = UE_MAX_FLT;
                if b.min_screen_size > 0.0 { final_cull = compute_bounds_draw_distance(b.min_screen_size, b.sphere_radius, b.proj_scale) * b.lod_scale; }
                if b.end_cull_cm > 0.0 { final_cull = final_cull.min(b.end_cull_cm * s); }
                final_cull *= s;
                c[2] = final_cull;
                if b.lod + 1 < b.num_lods {
                    let next = compute_bounds_draw_distance(b.screen_size[b.lod + 1], b.sphere_radius, b.proj_scale) * b.lod_scale;
                    c[2] = next.min(final_cull);
                }
                c[0] = UE_MIN_FLT;
                if b.lod > b.first_lod {
                    let cur = compute_bounds_draw_distance(b.screen_size[b.lod], b.sphere_radius, b.proj_scale) * b.lod_scale;
                    if cur < final_cull { c[1] = cur; } else { c[1] = UE_MIN_FLT; c[2] = UE_MIN_FLT; }
                } else {
                    c[1] = UE_MIN_FLT;
                }
            }
            let [o0, o1] = b.temporal_origin;
            p.world_view_origin_zero = [o0[0], o0[1], o0[2], 1.0 - b.temporal_alpha];
            p.world_view_origin_one = [o1[0], o1[1], o1[2], b.temporal_alpha];
            p.view_z_compare_zero[3] = b.lod_random;
        }
        p.fade_out[0] = b.start_cull_cm;
        p.fade_out[1] = if b.end_cull_cm > 0.0 { if b.end_cull_cm > b.start_cull_cm { 1.0 / (b.end_cull_cm - b.start_cull_cm) } else { 1.0 } } else { 0.0 };
        p.fade_out[2] = 1.0;
        p.fade_out[3] = 1.0;
        p
    }
}

/// GRASS: `FTemporalLODState` (UE4.16 SceneManagement.h:52-84 / SceneManagement.cpp:21-58) of one view: two lagged view origins, sampled every `lod.TemporalLag`
/// (0.5 s, not set by the 1.8 config) of `LastRenderTime` (= `ViewFamily.CurrentRealTime`, ScenePrivate.h:856), updated once per frame after visibility
/// (SceneVisibility.cpp:2346). The initial state is the engine's (origins 0, times 0, lag 0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemporalLod { pub origin: [[f32; 3]; 2], pub time: [f32; 2], pub lag: f32 }
/// `lod.TemporalLag` default (SceneManagement.cpp:16).
pub const LOD_TEMPORAL_LAG: f32 = 0.5;
impl TemporalLod {
    /// `UpdateTemporalLODTransition(View, LastRenderTime)` with `View.bDisableDistanceBasedFadeTransitions` false (no camera cut).
    pub fn update(&mut self, view_origin: [f32; 3], last_render_time: f32) {
        let mut ok = true;
        self.lag = LOD_TEMPORAL_LAG;
        if self.time[1] < last_render_time - self.lag {
            if self.time[0] < self.time[1] { self.origin[0] = self.origin[1]; self.time[0] = self.time[1]; }
            self.origin[1] = view_origin;
            self.time[1] = last_render_time;
            if self.time[1] <= self.time[0] { ok = false; }
        }
        if !ok { self.origin = [view_origin; 2]; self.time = [last_render_time; 2]; self.lag = 0.0; }
    }
    /// `GetTemporalLODTransition(LastRenderTime)` (SceneManagement.h:74).
    pub fn transition(&self, last_render_time: f32) -> f32 {
        if self.lag == 0.0 { return 0.0; }
        ((last_render_time - self.lag - self.time[0]) / (self.time[1] - self.time[0])).clamp(0.0, 1.0)
    }
    /// `FSceneView::GetTemporalLODOrigin(i)` (SceneView.cpp:832): the lagged origin while a lag is active, else the view origin.
    pub fn origin(&self, i: usize, view_origin: [f32; 3]) -> [f32; 3] { if self.lag != 0.0 { self.origin[i] } else { view_origin } }
}

/// GRASS: the instanced vertex factory's loose values into a vertex globals buffer (`SetShaderValue` of each bound parameter, InstancedStaticMesh.cpp:2006-2044).
fn write_instancing(floats: &mut [f32], g: &StageGlobals, p: &InstancingParams) {
    let Some(v) = g.params.instanced_vf() else { return };
    for (sp, val) in [(v.fade_out_params, p.fade_out), (v.view_z_compare_zero, p.view_z_compare_zero), (v.view_z_compare_one, p.view_z_compare_one), (v.view_z_constant, p.view_z_constant), (v.world_view_origin_zero, p.world_view_origin_zero), (v.world_view_origin_one, p.world_view_origin_one)] {
        if !sp.is_bound() || sp.buffer_index != g.slot { continue; }
        for (k, f) in val.iter().enumerate().take(sp.num_bytes as usize / 4) { if let Some(x) = floats.get_mut(sp.float_index() + k) { *x = *f; } }
    }
}

/// `PreviousLocalToWorld` (float4x4, row-vector rows = the glam columns) and `SkipOutputVelocity` of a base-pass VS globals buffer (BasePassRendering.inl:31..66).
fn write_base_pass_velocity(floats: &mut [f32], g: &StageGlobals, prev: Option<glam::Mat4>) {
    let Some(v) = g.params.base_pass_vs(&g.shader_type) else { return };
    let (m, skip) = match prev { Some(m) => (m, 0.0), None => (glam::Mat4::IDENTITY, 1.0) };
    let p = v.previous_local_to_world;
    if p.is_bound() && p.buffer_index == g.slot { for (k, f) in m.to_cols_array().iter().enumerate().take(p.num_bytes as usize / 4) { if let Some(x) = floats.get_mut(p.float_index() + k) { *x = *f; } } }
    let s = v.skip_output_velocity;
    if s.is_bound() && s.buffer_index == g.slot { if let Some(x) = floats.get_mut(s.float_index()) { *x = skip; } }
}

fn blend_of(flags: &MaterialFlags) -> Result<FxBlend, String> {
    Ok(match flags.blend.as_str() { "BLEND_Opaque" | "BLEND_Masked" => FxBlend::Opaque, "BLEND_Translucent" => FxBlend::Translucent, "BLEND_Additive" => FxBlend::Additive, "BLEND_Modulate" => FxBlend::Modulate, o => return Err(format!("unsupported blend mode {o}")) })
}

/// Pipeline + bind groups of `mat` for the (vertex shader, pixel shader) type names of its `vf` permutation.
pub fn build_material(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, mat: &Material, vf: Vf, vs_type: &str, ps_type: &str, num_uv: usize, instanced: bool, shared: SharedBuffers) -> Result<MaterialGpu, String> {
    build_material_ext(set, fx, cache, mat, vf, vs_type, ps_type, num_uv, instanced, shared, &[])
}

/// MATERIAL PARAMETERS (2026-10-06): float4 floor of a shared collection buffer, the zero buffer's size (a program declares at most the float4s it reads; a
/// declaration past the collection's own size would mean a shader map compiled against another version of it, logged).
const COLLECTION_MIN_FLOAT4S: usize = 512;
/// MATERIAL PARAMETERS: (collection path -> number of material programs bound to it), for the load report ([`collection_bind_counts`]).
static COLLECTION_BINDS: std::sync::Mutex<std::collections::BTreeMap<String, usize>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
/// MATERIAL PARAMETERS: how many material uniform-buffer bindings went to each parameter collection (path; `"<unresolved>"` = a `MaterialCollectionN` whose
/// StateId resolved to no collection, bound to the zero buffer).
pub fn collection_bind_counts() -> Vec<(String, usize)> { COLLECTION_BINDS.lock().map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect()).unwrap_or_default() }

/// MATERIAL PARAMETERS: the buffer bound as a program's `MaterialCollectionN` (`FUniformExpressionSet::ParameterCollections[N]`, resolved to a collection by
/// [`Material::collections`]): the caller's `named` buffer when it gives one (the storm's `MPC_SafeZoneClouds`), else the FX renderer's one shared buffer of
/// that collection, created on first use with its defaults (`UMaterialParameterCollection::GetDefaultParameterData`; the per-frame writers replace them with the
/// world's instance values). `None`: not a collection buffer, or unresolved (the zero buffer).
fn collection_binding(set: &PakSet, fx: &mut FxRenderer, mat: &Material, name: &str, declared_float4s: usize, named: &[(&str, FxBufId)]) -> Option<FxBufId> {
    let n: usize = name.strip_prefix("MaterialCollection")?.parse().ok()?;
    let path = mat.collections.get(n).cloned().flatten();
    let count = |key: &str| { if let Ok(mut m) = COLLECTION_BINDS.lock() { *m.entry(key.to_owned()).or_insert(0) += 1; } };
    if let Some(&(_, b)) = named.iter().find(|(k, _)| *k == name) {
        match path.as_deref().map(|p| (p, fx.collection_buffer(p))) {
            Some((p, Some(r))) if r != b => eprintln!("[mpc] {}: {name} = {p}, but the caller's buffer is not that collection's registered buffer", mat.path),
            Some((p, None)) => eprintln!("[mpc] {}: {name} = {p}: the caller's buffer is not registered for it", mat.path),
            None => eprintln!("[mpc] {}: {name} has no resolved collection; the caller's buffer is bound", mat.path),
            _ => {}
        }
        count(path.as_deref().unwrap_or("<unresolved>"));
        return Some(b);
    }
    let Some(path) = path else {
        eprintln!("[mpc] {}: {name} (StateId {:?}) resolves to no collection: zero buffer", mat.path, mat.uniform.parameter_collections.get(n));
        count("<unresolved>");
        return None;
    };
    let buf = match fx.collection_buffer(&path) {
        Some(b) => b,
        None => {
            let c = match crate::mpc::load_cached(set, &path) { Ok(c) => c, Err(e) => { eprintln!("[mpc] {}: {name} = {path}: {e}: zero buffer", mat.path); count("<unresolved>"); return None; } };
            let b = fx.create_buffer(&format!("fx MaterialCollection {}", path.rsplit('.').next().unwrap_or(&path)), c.float4s().max(COLLECTION_MIN_FLOAT4S) * 16);
            fx.write_buffer(b, &c.data(&crate::mpc::Instance::default()).iter().flatten().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>());
            fx.register_collection_buffer(&path, b);
            eprintln!("[mpc] {path}: shared buffer created ({} scalars, {} vectors = {} float4s, defaults; first reader {})", c.scalars.len(), c.vectors.len(), c.float4s(), mat.path);
            if declared_float4s > c.float4s() { eprintln!("[mpc] {}: {name} declares {declared_float4s} float4s, the collection has {} (shader map of another collection version?)", mat.path, c.float4s()); }
            b
        }
    };
    count(&path);
    Some(buf)
}

/// [`build_material`] with extra named uniform buffers (the program's own buffer table names them, e.g. `MaterialCollection0`): a buffer whose name is in `named` is bound to that FX buffer instead of the zero buffer.
pub fn build_material_ext(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, mat: &Material, vf: Vf, vs_type: &str, ps_type: &str, num_uv: usize, instanced: bool, shared: SharedBuffers, named: &[(&str, FxBufId)]) -> Result<MaterialGpu, String> {
    build_material_layout(set, fx, cache, mat, vf, vs_type, ps_type, num_uv, instanced, shared, named, LayoutOpts::default())
}

/// FIRST USE (2026-10-06): the DXBC -> WGSL assembly of every (shader map package, vertex factory, vertex / pixel / clip program streams, assembly kind,
/// scene prelude) built so far. Materials that share a shader map and a permutation translate once: the pickup hologram per weapon mesh (13-16 ms each
/// natively, verification/first_use/run5), the blueprint MID per piece class, the instances of one parent. The output is a pure function of the key.
#[derive(Clone, PartialEq, Eq, Hash)]
struct AsmKey { map: String, vf: &'static str, vs: usize, ps: Option<usize>, clip: Option<usize>, kind: u8, inst: bool, prelude: u64 }
static ASSEMBLED: std::sync::Mutex<Option<HashMap<AsmKey, (std::sync::Arc<Assembled>, std::sync::Arc<Vec<u8>>, Option<u32>)>>> = std::sync::Mutex::new(None);
fn asm_meta(a: &Assembled) -> std::sync::Arc<Assembled> {
    std::sync::Arc::new(Assembled { wgsl: String::new(), resources: a.resources.clone(), vs_inputs: a.vs_inputs.clone(), varyings: a.varyings.clone(), targets: a.targets, ps_uses_position: a.ps_uses_position })
}
fn asm_get(key: &AsmKey) -> Option<(std::sync::Arc<Assembled>, Option<u32>)> {
    let (m, z, attr) = ASSEMBLED.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(key).cloned()))?;
    let wgsl = String::from_utf8(miniz_oxide::inflate::decompress_to_vec(&z).ok()?).ok()?;
    Some((std::sync::Arc::new(Assembled { wgsl, resources: m.resources.clone(), vs_inputs: m.vs_inputs.clone(), varyings: m.varyings.clone(), targets: m.targets, ps_uses_position: m.ps_uses_position }), attr))
}
fn asm_put(key: AsmKey, a: &Assembled, attr: Option<u32>) {
    let mut z = miniz_oxide::deflate::compress_to_vec(a.wgsl.as_bytes(), 1);
    z.shrink_to_fit();
    if let Ok(mut g) = ASSEMBLED.lock() { g.get_or_insert_with(HashMap::new).insert(key, (asm_meta(a), std::sync::Arc::new(z), attr)); }
}

struct AsmOut<'a> { vs: &'a crate::cooked::Program, ps_opt: Option<&'a crate::cooked::Program>, clip: Option<&'a crate::cooked::Program>, lit: bool, scene: bool, asm: std::sync::Arc<Assembled>, inst_attr: Option<u32> }
static SCENE_PRELUDE: std::sync::Mutex<Option<std::sync::Arc<String>>> = std::sync::Mutex::new(None);

pub fn preassemble(mat: &Material, vf: Vf, vs_type: &str, ps_type: &str, instance_primitive: bool) {
    let Some(prelude) = SCENE_PRELUDE.lock().ok().and_then(|g| g.clone()) else { return };
    let _ = assembled(mat, vf, vs_type, ps_type, None, Some(prelude.as_str()), instance_primitive, false);
    if let Ok(Some((None, vs, ps))) = shadow_programs(mat, None, vf) { let _ = assembled(mat, vf, vs, ps, Some(mat.flags.two_sided), None, instance_primitive, false); }
}

#[allow(clippy::too_many_arguments)]
fn assembled<'a>(mat: &'a Material, vf: Vf, vs_type: &str, ps_type: &str, shadow: Option<bool>, prelude: Option<&str>, instance_primitive: bool, log: bool) -> Result<AsmOut<'a>, String> {
    // a lit (DefaultLit) opaque / masked material writes the deferred GBuffer: see `assemble`. Translucent ones (MGreen, MPreviewPulse) write SV_Target0 only
    // (MaterialFlags::uses_gbuffer, UE4.16 BasePassCommon.usf:20); treating them as lit read o[1] / o[3] past the end of the output array (naga validation error).
    let lit = mat.flags.uses_gbuffer() && shadow.is_none();
    let vs = mat.cooked.find(vf.type_name(), vs_type).ok_or_else(|| format!("{}: no {} / {vs_type} program", mat.base_path, vf.type_name()))?;
    // FX SHADOW: a shadow pipeline may have no pixel program (UE's null pixel shader)
    let ps_opt = if shadow.is_some() && ps_type.is_empty() { None } else { Some(mat.cooked.find(vf.type_name(), ps_type).ok_or_else(|| format!("{}: no {} / {ps_type} program", mat.base_path, vf.type_name()))?) };
    // masked material whose base-pass pixel shader has no discard: the depth-only pixel shader clips (see `assemble`). It is the material's own `FDepthOnlyPS`
    // (`FDepthDrawingPolicy` with `bNeedsPixelShader = !WritesEveryPixel()`, UE4.16 DepthRendering.cpp:281 / :704; cached only for such materials, :103).
    // MIRRORFIX: a shader map without it was compiled `WritesEveryPixel` (opaque): UE's depth pass then draws the default material's position-only VS with no
    // pixel shader (DepthRendering.cpp:652-669) and nothing clips, so the draw goes on unclipped instead of failing (logged; `Material::load` now resolves the
    // instance's real blend mode and shader map, the cause of the old `Wood_lvl1_Static` failure).
    let clip = match ps_opt { Some(ps) if shadow.is_none() && mat.flags.blend == "BLEND_Masked" && !ps.discards => {
        let d = mat.cooked.find(vf.type_name(), "FDepthOnlyPS");
        if d.is_none() && log { eprintln!("[fx] {}: masked flags but the shader map has no FDepthOnlyPS (compiled WritesEveryPixel): drawn unclipped like UE's position-only depth path", mat.path); }
        d
    } _ => None };
    // FX RECEIVE (2026-10-05): with the renderer's scene lighting installed, a lit (GBuffer) material is resolved by the static meshes' lighting (CSM PCF + DF
    // shadow, light function, DFAO sky diffuse, sky reflection, fog) in the scene pass (fn_render::fx::FxRenderer::create_scene_lit_pipeline); standalone
    // FX renderers keep the simple sun + sky tail (group 3 FxLight)
    // FX ORDER (2026-10-05): an opaque / masked UNLIT material is a base-pass draw too (before AO / reflections / fog in UE4.16): with the scene lighting
    // installed it joins the scene pass through the same prelude with the unlit tail (program::assemble_scene_unlit); it used to be drawn after the AO and fog
    let opaque_unlit = !lit && shadow.is_none() && matches!(mat.flags.blend.as_str(), "BLEND_Opaque" | "BLEND_Masked");
    let scene_prelude = if lit || opaque_unlit { prelude.map(str::to_owned) } else { None };
    let scene = scene_prelude.is_some();
    // FIRST USE (2026-10-06): one translation per (shader map, permutation, assembly), see `ASSEMBLED`
    let kind: u8 = match ps_opt { Some(_) if shadow.is_none() => match &scene_prelude { Some(_) if opaque_unlit => 1, Some(_) => 2, None => if lit { 3 } else { 4 } }, _ => 5 };
    let key_i = AsmKey { map: mat.shader_map_package.clone(), vf: vf.type_name(), vs: vs.stream, ps: ps_opt.map(|p| p.stream), clip: clip.map(|c| c.stream), kind, inst: false, prelude: scene_prelude.as_deref().map_or(0, |p| { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); p.hash(&mut h); h.finish() }) };
    let key = key_i.clone();
    // `FN_FX_NO_ASM_CACHE=1`: translate every time (A/B check of the cache)
    let cached = if fn_live::var_os("FN_FX_NO_ASM_CACHE").is_some() { None } else { asm_get(&key).map(|x| x.0) };
    let asm: std::sync::Arc<Assembled> = match cached {
        Some(a) => a,
        None => {
            let a = std::sync::Arc::new(match ps_opt {
                Some(ps) if shadow.is_none() => match &scene_prelude {
                    Some(p) if opaque_unlit => crate::program::assemble_scene_unlit(vs, ps, clip, p)?,
                    Some(p) => crate::program::assemble_scene_lit(vs, ps, clip, p)?,
                    None => assemble(vs, ps, lit, clip)?,
                },
                _ => crate::program::assemble_shadow(vs, ps_opt, FX_SHADOW_WGSL)?,
            });
            asm_put(key, &a, None);
            a
        }
    };
    // INSTANCING (2026-10-07): the module with its `Primitive` cbuffers read per instance (`crate::program::instance_primitive`), and the instance id's attribute
    let ikey = AsmKey { inst: true, ..key_i };
    if instance_primitive { if let Some(a) = asm_get(&ikey) { return Ok(AsmOut { vs, ps_opt, clip, lit, scene, asm: a.0, inst_attr: a.1 }); } }
    let (asm, inst_attr) = if instance_primitive {
        let prog_of = |st: Stage| match st { Stage::Vertex => Some(vs), Stage::Pixel => ps_opt, Stage::ClipPixel => clip };
        let prim: Vec<String> = asm.resources.iter().filter(|r| r.kind == ResKind::Uniform && prog_of(r.stage).and_then(|p| p.uniform_buffer_names.as_ref()).and_then(|n| n.get(r.slot as usize)).is_some_and(|n| n == "Primitive"))
            .map(|r| format!("{}cb{}", match r.stage { Stage::Vertex => "v", Stage::Pixel => "p", Stage::ClipPixel => "d" }, r.slot)).collect();
        // the base-pass VS's PreviousLocalToWorld (the velocity globals, `write_base_pass_velocity`) is the instance's LocalToWorld
        let prev = vs.params.as_ref().and_then(|p| p.base_pass_vs(&vs.shader_type)).map(|v| v.previous_local_to_world).filter(|p| p.is_bound());
        if prev.is_some_and(|p| p.float_index() % 4 != 0) { return Err(format!("{}: PreviousLocalToWorld not float4 aligned", mat.path)); }
        let prev_name = prev.map(|p| (format!("vcb{}", p.buffer_index), (p.float_index() / 4) as u32));
        let (wgsl, attr) = crate::program::instance_primitive(&asm, &prim, prev_name.as_ref().map(|(n, r)| (n.as_str(), *r))).map_err(|e| format!("{}: not instanced ({e})", mat.path))?;
        let a = std::sync::Arc::new(Assembled { wgsl, resources: asm.resources.clone(), vs_inputs: asm.vs_inputs.clone(), varyings: asm.varyings.clone(), targets: asm.targets, ps_uses_position: asm.ps_uses_position });
        asm_put(ikey, &a, Some(attr));
        (a, Some(attr))
    } else { (asm, None) };
    Ok(AsmOut { vs, ps_opt, clip, lit, scene, asm, inst_attr })
}

/// Options of [`build_material_layout`]: the vertex buffer layout (`None` = [`vertex_buffers`]), the face winding, the buffer bound to the vertex program's `Buffer<float4>` SRV (skin bone matrices) and to its unnamed globals buffer (vertex factory loose parameters).
#[derive(Clone, Default)]
pub struct LayoutOpts { pub vertex_layout: Option<Vec<FxVertexBuffer>>, pub front_cw: bool, pub vertex_storage: Option<FxBufId>, pub vs_globals: Option<FxBufId>,
    /// FX SHADOW: build the depth-only cascade pipeline of the (`vs_type`, `ps_type`) shadow-depth pair instead of a base-pass one (`ps_type` empty = no pixel
    /// shader), see [`build_shadow`]; `Some(two_sided)` = the culling of the ORIGINAL material (`Material->IsTwoSided()` is taken before the default-material
    /// override, ShadowDepthRendering.cpp:1059 / :1063).
    pub shadow: Option<bool>,
    /// INSTANCING (2026-10-07): the `Primitive` buffer is a storage array of per-instance records read by an instance id (`crate::program::instance_primitive`;
    /// vertex buffer 1 = one `u32` per instance): one draw per mesh section for many placements. Falls back to an error when the module cannot be rewritten.
    pub instance_primitive: bool }

/// [`build_material_ext`] with an explicit vertex buffer layout (`vertex_layout`: `None` = [`vertex_buffers`]; the particle path uses it for the dynamic-parameter stream of sprites) and the face winding: `front_cw` flips the default (front = counter-clockwise in framebuffer space, UE4.16 D3D11State.cpp:200 `FrontCounterClockwise = true`, DrawingPolicy.cpp:45 `CM_CW`).
pub fn build_material_layout(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, mat: &Material, vf: Vf, vs_type: &str, ps_type: &str, num_uv: usize, instanced: bool, shared: SharedBuffers, named: &[(&str, FxBufId)], opts: LayoutOpts) -> Result<MaterialGpu, String> {
    let t_start = std::time::Instant::now();
    let LayoutOpts { vertex_layout, front_cw, vertex_storage, vs_globals, shadow, instance_primitive } = opts;
    let prelude = fx.scene_lit_prelude().map(str::to_owned);
    if let Some(p) = &prelude { if let Ok(mut g) = SCENE_PRELUDE.lock() { if g.is_none() { *g = Some(std::sync::Arc::new(p.clone())); } } }
    let AsmOut { vs, ps_opt, clip, lit, scene, asm, inst_attr } = assembled(mat, vf, vs_type, ps_type, shadow, prelude.as_deref(), instance_primitive, true)?;
    let lit = lit && !scene; // from here: `lit` = the group-3 FxLight buffer of the simple tail
    let flags = mat.flags.clone();
    let blend = blend_of(&flags)?;
    let packed = mat.packed(0.0);
    let declared_max = asm.resources.iter().filter(|r| r.kind == ResKind::Uniform).map(|r| r.size as usize).max().unwrap_or(0);
    let material_buf = fx.create_buffer("fx material cb", packed.registers.max(declared_max).max(1) * 16);
    let zero_buf = fx.create_buffer("fx zero cb", 512 * 16);
    let mut layouts: [Vec<FxLayoutEntry>; 4] = [vec![], vec![], vec![], vec![]];
    let mut entries: [Vec<FxBindEntry>; 4] = [vec![], vec![], vec![], vec![]];
    if lit {
        layouts[3].push(FxLayoutEntry { binding: 0, ty: FxBindTy::Uniform, vertex: false, fragment: true });
        entries[3].push(FxBindEntry { binding: 0, res: FxRes::Buffer(shared.light) });
    }
    let mut primitive_slots: Vec<(usize, usize)> = vec![];
    let mut material_slots: Vec<(usize, usize)> = vec![];
    let mut spritevf_slots: Vec<(usize, usize)> = vec![];
    let mut per_frame: Option<(FxBufId, usize, usize)> = None;
    // material-owned $Globals buffers from the serialized parameters (see `MaterialGpu::globals`)
    let mut globals: Vec<StageGlobals> = vec![];
    let mut vs_globals_slots: Vec<(usize, usize)> = vec![];
    let mut vs_fog: Vec<FxFogSlot> = vec![]; // FOG INPUTS
    let mut landscape_slots: Vec<(usize, usize, LandscapeSlot)> = vec![]; // LANDSCAPE MATERIAL
    // LANDSCAPE MATERIAL: the landscape VF resources of a program (heightmap t/s of the vertex stage, normal map t/s of the pixel stage)
    let landscape_res = |stage: Stage, prog: &crate::cooked::Program, slot: u32, sampler: bool| -> Option<LandscapeSlot> {
        let p = prog.params.as_ref()?;
        match stage {
            Stage::Vertex => { let v = p.landscape_vs()?; let r = if sampler { v.heightmap_sampler } else { v.heightmap }; (r.is_bound() && r.base_index as u32 == slot).then_some(if sampler { LandscapeSlot::HeightmapSampler } else { LandscapeSlot::Heightmap }) }
            Stage::Pixel => { let v = p.landscape_ps()?; let r = if sampler { v.normalmap_sampler } else { v.normalmap }; (r.is_bound() && r.base_index as u32 == slot).then_some(if sampler { LandscapeSlot::NormalmapSampler } else { LandscapeSlot::Normalmap }) }
            _ => None,
        }
    };
    // which unbound 2D textures (scene depth) exist per stage
    let unbound_2d = |prog: &crate::cooked::Program, slot: u32| !prog.bindings.iter().any(|b| b.kind == fn_dxbc::unreal_packet::ResourceKind::Texture && b.binding.register as u32 == slot);
    // TRANSLUCENCY VOLUME (MIRRORFIX 2026-10-05): the pixel program's `FTranslucentLightingVolumeParameters` (shader_params.rs), kept only when every bound texture
    // register is a Texture3D declaration of the program. A translucent lit material (TLM_Volumetric*) reads the volume of `fn_render::translucent_lighting`
    // through them (`TBasePassPixelShaderPolicyParamType::SetParameters` -> `TranslucentLightingParameters.Set`, BasePassRendering.h:764; bilinear clamp samplers, :255).
    let volume = ps_opt.and_then(|ps| ps.params.as_ref().and_then(|p| p.translucency_volume(&ps.shader_type)).map(|v| (ps, v))).filter(|(ps, v)| v.textures.iter().filter(|t| t.is_bound()).all(|t| ps.shader.decls.resources.iter().any(|d| d.slot == t.base_index as u32 && d.dim == 5))).map(|(_, v)| v);
    let volume_texture = |stage: Stage, slot: u32| -> Option<u8> { if stage != Stage::Pixel { return None; } volume.and_then(|v| v.textures.iter().position(|t| t.is_bound() && t.base_index as u32 == slot)).map(|k| k as u8) };
    let volume_sampler = |stage: Stage, slot: u32| stage == Stage::Pixel && volume.map_or(false, |v| v.samplers.iter().any(|s| s.is_bound() && s.base_index as u32 == slot));
    for r in &asm.resources {
        let prog = match r.stage { Stage::Vertex => vs, Stage::Pixel => ps_opt.ok_or("pixel resource without a pixel program")?, Stage::ClipPixel => clip.unwrap() };
        let (vertex, fragment) = (r.stage == Stage::Vertex, r.stage != Stage::Vertex);
        let g = r.group as usize;
        match r.kind {
            ResKind::Uniform => {
                let name = prog.uniform_buffer_names.as_ref().and_then(|n| n.get(r.slot as usize)).cloned().unwrap_or_default();
                // the unnamed ($Globals) buffer: laid out from the program's serialized parameters (shader_params.rs) when it holds per-frame expressions or the
                // base-pass VS velocity globals; a caller-provided vertex globals buffer (GPU-skin VF loose parameters) still wins for the vertex stage
                let caller_vs = r.stage == Stage::Vertex && name.is_empty() && vs_globals.is_some();
                let parsed_here = name.is_empty() && !caller_vs && r.stage != Stage::ClipPixel && prog.params.as_ref().map_or(false, |p| p.bytes_used_in(r.slot as u16, &prog.shader_type) > 0);
                if parsed_here {
                    let b = fx.create_buffer(if r.stage == Stage::Vertex { "fx vertex globals" } else { "fx per-frame globals" }, r.size as usize * 16);
                    let sg = StageGlobals { vertex: r.stage == Stage::Vertex, buf: b, slot: r.slot as u16, float4s: r.size as usize, shader_type: prog.shader_type.clone(), params: prog.params.clone().unwrap() };
                    // initial contents: per-frame expressions at time 0, velocity globals as for "no velocity" (identity, SkipOutputVelocity 1)
                    let mut floats = vec![0f32; sg.float4s * 4];
                    crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &sg.params, sg.slot, crate::uniform::MaterialTime::default());
                    write_base_pass_velocity(&mut floats, &sg, None); write_instancing(&mut floats, &sg, &InstancingParams::default()); write_landscape_ps(&mut floats, &sg); // GRASS: instanced VF defaults
                    fx.write_buffer(b, &crate::view::f32_bytes(&floats));
                    if r.stage == Stage::Pixel && per_frame.is_none() { per_frame = Some((b, 0, r.size as usize)); }
                    globals.push(sg);
                }
                // fallback (labelled inference, only when the program's parameters did not parse): per-frame scalars taken as the LAST declared pixel globals
                let pf_here = !parsed_here && r.stage == Stage::Pixel && name.is_empty() && prog.params.is_none() && !mat.uniform.per_frame_scalars.is_empty() && mat.uniform.per_frame_vectors.is_empty();
                if pf_here { let n = mat.uniform.per_frame_scalars.len(); let b = fx.create_buffer("fx per-frame globals (inferred)", r.size as usize * 16); per_frame = Some((b, (r.size as usize * 4).saturating_sub(n), r.size as usize)); }
                let buf = if parsed_here { globals.last().unwrap().buf } else if pf_here { per_frame.unwrap().0 } else if caller_vs { vs_globals.unwrap() } else { match name.as_str() { "View" => shared.view, "Primitive" => shared.primitive, "Material" => material_buf, "SpriteVF" => shared.sprite_vf, other => collection_binding(set, fx, mat, other, r.size as usize, named).or_else(|| named.iter().find(|(n, _)| *n == other).map(|(_, b)| *b)).unwrap_or(zero_buf) } }; // MATERIAL PARAMETERS: MaterialCollectionN = the shared collection buffer
                if parsed_here && r.stage == Stage::Vertex { vs_globals_slots.push((g, entries[g].len())); }
                // FOG INPUTS (2026-10-05): the $Globals buffer of a program that binds `FHeightFogShaderParameters` gets the view's fog values every frame
                // (`HeightFogParameters.Set`, BasePassRendering.h:377; fn_render::fx::FxRenderer::register_height_fog); the View buffer its volumetric-fog slots
                if (parsed_here || caller_vs) && r.stage != Stage::ClipPixel {
                    let fs = fog_slots(prog, r.slot);
                    if r.stage == Stage::Vertex { vs_fog = fs.clone(); }
                    fx.register_height_fog(buf, fs);
                }
                if name == "View" { fx.register_view_buffer(buf); }
                layouts[g].push(FxLayoutEntry { binding: r.binding, ty: if inst_attr.is_some() && name == "Primitive" { FxBindTy::StorageRead } else { FxBindTy::Uniform }, vertex, fragment }); // INSTANCING: the per-instance records
                if name == "Primitive" { primitive_slots.push((g, entries[g].len())); }
                if name == "Material" { material_slots.push((g, entries[g].len())); }
                if name == "SpriteVF" { spritevf_slots.push((g, entries[g].len())); }
                if name == "LandscapeParameters" { landscape_slots.push((g, entries[g].len(), LandscapeSlot::Uniform)); } // LANDSCAPE MATERIAL: per component
                entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Buffer(buf) });
            }
            ResKind::Tex2D => {
                let binding = prog.bindings.iter().find(|b| b.kind == fn_dxbc::unreal_packet::ResourceKind::Texture && b.binding.register as u32 == r.slot);
                match binding {
                    Some(b) => {
                        let k = (b.binding.member / 2) as usize;
                        // STATIC ITEM: a NULL texture value draws GWhiteTexture, as the engine does (Material::texture_slot_is_null)
                        let tex = if mat.texture_slot(k).is_none() && mat.texture_slot_is_null(k) { cache.white_2d(fx) } else {
                            let (path, _) = mat.texture_slot(k).ok_or_else(|| format!("{}: texture slot {k} unresolved", mat.path))?;
                            let (id, info) = cache.load(set, fx, &path)?;
                            // DEBUG (debug-only, with FN_FX_DUMP_WGSL): which texture each program binding reads (TEXTURE MEMORY: BC5 normal-map channel check)
                            if fn_live::var_os("FN_FX_DUMP_WGSL").is_some() { eprintln!("[fx tex] {} ({}) {:?} binding {} (t{}) <- {path} ({}, srgb {})", mat.path.rsplit('.').next().unwrap_or(""), mat.base_path, r.stage, r.binding, r.slot, info.compression, info.srgb); }
                            id
                        };
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Tex2D { filterable: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Texture(tex) });
                    }
                    // LANDSCAPE MATERIAL: the component's heightmap (VS) / normal map (PS): a placeholder here, swapped per component ([`MaterialGpu::bind_variant_landscape`])
                    None if landscape_res(r.stage, prog, r.slot, false).is_some() => {
                        landscape_slots.push((g, entries[g].len(), landscape_res(r.stage, prog, r.slot, false).unwrap()));
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Tex2D { filterable: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Texture(cache.white_2d(fx)) });
                    }
                    None => {
                        if r.stage == Stage::Vertex { return Err(format!("{}: vertex shader texture t{} is not a material texture", mat.path, r.slot)); }
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::SceneDepth, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::SceneDepth });
                    }
                }
            }
            ResKind::StorageBuf => {
                // `Buffer<float4>` SRVs of the programs (cut-out geometry tables) are not fed by these materials: an empty buffer keeps the layout valid
                layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::StorageRead, vertex, fragment });
                entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Buffer(if r.stage == Stage::Vertex { vertex_storage.unwrap_or(zero_buf) } else { zero_buf }) });
            }
            ResKind::Tex3D | ResKind::Cube => {
                let dim = if r.kind == ResKind::Cube { FxTexDim::Cube } else { FxTexDim::D3 };
                let ty = if dim == FxTexDim::Cube { FxBindTy::Cube { filterable: true } } else { FxBindTy::Tex3D { filterable: true } };
                // TRANSLUCENCY VOLUME: a volume register reads this frame's translucency lighting volume (fn_render::fx::FxRes::TranslucencyVolume)
                // FOG INPUTS: the height-fog cube (`FogInscatteringColorCubemap` = GWhiteTextureCube, FogRendering.h:35) and the volumetric fog's
                // `IntegratedLightScattering` (VolumetricFog.h:51) at the registers the program serialized for them
                let fog = if r.stage == Stage::ClipPixel { None } else { height_fog_of(prog) };
                let fog_res = fog.and_then(|f| if dim == FxTexDim::Cube && f.cubemap.is_bound() && f.cubemap.base_index as u32 == r.slot { Some(FxRes::FogCube) } else if dim == FxTexDim::D3 && f.volume.is_bound() && f.volume.base_index as u32 == r.slot { Some(FxRes::FogVolume) } else { None });
                let res = match (fog_res, volume_texture(r.stage, r.slot).filter(|_| dim == FxTexDim::D3)) { (Some(f), _) => f, (None, Some(k)) => FxRes::TranslucencyVolume(k), (None, None) => FxRes::Texture(cache.dummy(fx, dim)) };
                layouts[g].push(FxLayoutEntry { binding: r.binding, ty, vertex, fragment });
                entries[g].push(FxBindEntry { binding: r.binding, res });
            }
            ResKind::Sampler => {
                let binding = prog.bindings.iter().find(|b| b.kind == fn_dxbc::unreal_packet::ResourceKind::Sampler && b.binding.register as u32 == r.slot);
                match binding {
                    Some(b) if b.binding.member as usize >= 2 * mat.uniform.textures2d.len() => {
                        // `Wrap_WorldGroupSettings` (member 2 N) / `Clamp_WorldGroupSettings` (member 2 N + 1): the world texture group's anisotropic trilinear filter, wrap / clamp
                        let clamp = b.binding.member as usize == 2 * mat.uniform.textures2d.len() + 1;
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Linear, address: if clamp { FxAddress::Clamp } else { FxAddress::Wrap }, aniso: 8 }) });
                    }
                    Some(b) if mat.texture_slot((b.binding.member / 2) as usize).is_none() && mat.texture_slot_is_null((b.binding.member / 2) as usize) => {
                        // STATIC ITEM: GWhiteTexture's own sampler, SF_Point / AM_Wrap (RenderUtils.cpp:191; FillUniformBuffer binds it with the white texture)
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(FxSamplerDesc { mag: FxFilter::Nearest, min: FxFilter::Nearest, mip: FxFilter::Nearest, address: FxAddress::Wrap, aniso: 1 }) });
                    }
                    Some(b) => {
                        let k = (b.binding.member / 2) as usize;
                        let (path, src) = mat.texture_slot(k).ok_or_else(|| format!("{}: sampler slot {k} unresolved", mat.path))?;
                        let (_, info) = cache.load(set, fx, &path)?;
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(sampler_for(&info, src)) });
                    }
                    None if landscape_res(r.stage, prog, r.slot, true).is_some() => {
                        // LANDSCAPE MATERIAL: HeightmapTextureSampler = TStaticSamplerState<SF_Point> (LandscapeRender.cpp:2345, AM_Wrap default); the normal map's
                        // sampler is the texture resource's own (:2453), set per component
                        landscape_slots.push((g, entries[g].len(), landscape_res(r.stage, prog, r.slot, true).unwrap()));
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(FxSamplerDesc { mag: FxFilter::Nearest, min: FxFilter::Nearest, mip: FxFilter::Nearest, address: FxAddress::Wrap, aniso: 1 }) });
                    }
                    None if r.stage != Stage::ClipPixel && height_fog_of(prog).map_or(false, |f| [f.cubemap_sampler, f.volume_sampler].iter().any(|s| s.is_bound() && s.base_index as u32 == r.slot)) => {
                        // FOG INPUTS: FogInscatteringColorSampler = TStaticSamplerState<SF_Trilinear, AM_Clamp x3> (FogRendering.h:47), IntegratedLightScatteringSampler =
                        // TStaticSamplerState<SF_Bilinear, AM_Clamp x3> (VolumetricFog.h:57)
                        let cube = height_fog_of(prog).map_or(false, |f| f.cubemap_sampler.is_bound() && f.cubemap_sampler.base_index as u32 == r.slot);
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: if cube { FxFilter::Linear } else { FxFilter::Nearest }, address: FxAddress::Clamp, aniso: 1 }) });
                    }
                    None if volume_sampler(r.stage, r.slot) => {
                        // TRANSLUCENCY VOLUME: TStaticSamplerState<SF_Bilinear, AM_Clamp, AM_Clamp, AM_Clamp> (BasePassRendering.h:255)
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: true }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Nearest, address: FxAddress::Clamp, aniso: 1 }) });
                    }
                    None => {
                        // an unbound sampler belongs to the unbound (scene depth) texture: the point sampler the engine uses for scene textures (the old rule matched equal slot numbers only)
                        let depth = r.stage != Stage::Vertex && (unbound_2d(prog, r.slot) && prog.shader.decls.resources.iter().any(|d| d.slot == r.slot && d.dim == 3) || asm.resources.iter().any(|x| x.stage == r.stage && x.kind == ResKind::Tex2D && unbound_2d(prog, x.slot)));
                        layouts[g].push(FxLayoutEntry { binding: r.binding, ty: FxBindTy::Sampler { filtering: !depth }, vertex, fragment });
                        entries[g].push(FxBindEntry { binding: r.binding, res: FxRes::Sampler(if depth { FxSamplerDesc::NEAREST_CLAMP } else { FxSamplerDesc::LINEAR_CLAMP }) });
                    }
                }
            }
        }
    }
    let _ = PIXEL_BINDING_BASE;
    let mut wgsl = asm.wgsl.clone();
    if cfg!(target_arch = "wasm32") {
        let n_groups = if lit { 4 } else { 3 };
        merge_equal_samplers(&mut wgsl, &mut layouts, &mut entries, n_groups, WEB_FRAGMENT_SAMPLERS, &mut [&mut primitive_slots, &mut material_slots, &mut spritevf_slots, &mut vs_globals_slots], &mut landscape_slots);
    }
    let translucent = blend != FxBlend::Opaque;
    let desc = FxPipelineDesc {
        label: format!("fx {} {}", mat.base_path.rsplit('.').next().unwrap_or(""), vf.type_name()),
        wgsl, vs_entry: "vs_main".into(), fs_entry: "fs_main".into(),
        vertex_buffers: { let mut v = vertex_layout.unwrap_or_else(|| vertex_buffers(vf, num_uv, instanced)); if let Some(l) = inst_attr { v.push(FxVertexBuffer { stride: 4, instance: true, attrs: vec![FxAttr { location: l, offset: 0, format: FxFmt::Uint32 }] }); } v }, // INSTANCING: the instance ids
        groups: layouts.iter().take(if lit { 4 } else { 3 }).cloned().collect(),
        // DEPTH TEST (water agent, 2026-10-07): `bDisableDepthTest` is read only by the TRANSLUCENT pass (`FTranslucencyDrawingPolicyFactory::DrawMesh`,
        // UE4.16 TranslucentRendering.cpp:530 / :559 `TStaticDepthStencilState<false, CF_Always>`; MaterialShared.cpp:802); the opaque / masked base pass and
        // depth prepass keep the default depth state whatever the flag says. Honouring it here drew the masked stream water (Stream_Flowmaps_01*, which sets
        // bDisableDepthTest) through the Lonely Lodge walls.
        blend, depth: FxDepth { test: !(translucent && flags.disable_depth_test), write: !translucent },
        // UE4 D3D11: `FrontCounterClockwise = TRUE`, `CM_CW` -> cull back: front faces are the counter-clockwise ones (framebuffer space)
        cull: if flags.two_sided || std::env::var_os("FXDEBUG_NOCULL").is_some() { FxCull::None } else if front_cw || std::env::var_os("FXDEBUG_CW").is_some() { FxCull::BackCw } else { FxCull::BackCcw },
        translucent,
        // UMaterial::bEnableSeparateTranslucency (default true): the separate translucency pass (fn_render::fx, TranslucentRendering.cpp:889)
        separate_translucency: flags.separate_translucency,
        // STORM TAA (2026-10-07): bEnableResponsiveAA tags the translucent draw's pixels for the responsive TAA blend (TranslucentRendering.cpp:531-557)
        responsive_aa: translucent && shadow.is_none() && flags.responsive_aa,
    };
    if desc.responsive_aa { eprintln!("[fx] {}: bEnableResponsiveAA (translucent pixels take the responsive TAA blend)", mat.path); } // STORM TAA
    // DEBUG (debug-only): `FN_FX_DUMP_WGSL=<dir>` writes the assembled program of every material (<dir>/<material>_<vf>.wgsl; see the `[fx tex]` lines for its
    // texture bindings)
    if let Some(dir) = fn_live::var_os("FN_FX_DUMP_WGSL") {
        let name: String = format!("{}_{}{}", mat.path.rsplit('.').next().unwrap_or(""), vf.type_name(), if shadow.is_some() { "_shadow" } else { "" }).chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{name}.wgsl")), &desc.wgsl);
    }
    let (t_pipe, wgsl_kb) = (std::time::Instant::now(), desc.wgsl.len() / 1024); // LOAD TIME probe (FN_FX_TIMING=1): translation vs pipeline creation
    let pipe = match shadow {
        None if scene => fx.create_scene_lit_pipeline(&desc)?, // FX RECEIVE: drawn in the scene pass, globals at group 3
        None => fx.create_pipeline(&desc)?,
        // FX SHADOW: depth-only cascade pipeline (fn_render::fx::FxRenderer::create_shadow_pipeline), culled by the original material's TwoSided
        Some(two_sided) => {
            let cull = if two_sided { FxCull::None } else if front_cw { FxCull::BackCw } else { FxCull::BackCcw };
            fx.create_shadow_pipeline(&FxPipelineDesc { label: format!("{} shadow", desc.label), fs_entry: if ps_opt.is_some() { "fs_main".into() } else { String::new() }, cull, translucent: false, ..desc })?
        }
    };
    if fn_live::var_os("FN_FX_TIMING").is_some() { eprintln!("[fx-timing] {}: wgsl {} KB, before pipeline {:.1} ms, pipeline {:.1} ms", mat.path.rsplit('.').next().unwrap_or(""), wgsl_kb, (t_pipe - t_start).as_secs_f32() * 1000., t_pipe.elapsed().as_secs_f32() * 1000.); }
    let mut binds = vec![];
    for (g, e) in entries.iter().enumerate().take(if lit { 4 } else { 3 }) { binds.push((g as u32, fx.create_bind_group(pipe, g as u32, e.clone()))); }
    fx.write_buffer(material_buf, &crate::view::bytes(&packed.cb));
    Ok(MaterialGpu { pipe, binds, material_buf, material_registers: packed.registers, per_frame, globals, vs_globals_slots, assembled: asm_meta(&asm), flags, translucent, entries: entries.into_iter().collect(), primitive_slots, material_slots, spritevf_slots, group_count: if lit { 4 } else { 3 }, vs_fog, landscape_slots })
}










impl MaterialGpu {
    /// This frame's contents of the per-frame globals buffer (`UE_Material_PerFrameScalarExpressionN` at `time`), when the material has them: (buffer, bytes).
    pub fn per_frame_write(&self, mat: &Material, time: f32) -> Option<(FxBufId, Vec<u8>)> {
        // parsed layout (shader_params.rs) first: the pixel globals at the serialized offsets; `time` is used for both clocks (see `per_frame_writes`)
        if let Some(g) = self.globals.iter().find(|g| !g.vertex) {
            let mut floats = vec![0f32; g.float4s * 4];
            crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, crate::uniform::MaterialTime::single(time)); write_landscape_ps(&mut floats, g);
            return Some((g.buf, crate::view::f32_bytes(&floats)));
        }
        let (buf, base, size) = self.per_frame?;
        let (scalars, _) = crate::uniform::evaluate_per_frame(&mat.uniform, &mat.overrides, time);
        let mut floats = vec![0f32; size * 4];
        for (i, v) in scalars.iter().enumerate() { if let Some(s) = floats.get_mut(base + i) { *s = *v; } }
        Some((buf, crate::view::f32_bytes(&floats)))
    }
    /// Every material-owned globals buffer of this frame (pixel AND vertex stage, scalars, vectors and the previous-frame expressions) at the UE material times
    /// `t` (world time / real time / world delta, ShaderBaseClasses.cpp:287). The vertex buffer written here is the material's own (no per-primitive velocity
    /// data: `None` = identity / `SkipOutputVelocity` 1); draws that need `PreviousLocalToWorld` bind a per-primitive buffer from [`Self::vs_globals_bytes`].
    pub fn per_frame_writes(&self, mat: &Material, t: crate::uniform::MaterialTime) -> Vec<(FxBufId, Vec<u8>)> {
        let mut out = vec![];
        for g in &self.globals {
            let mut floats = vec![0f32; g.float4s * 4];
            crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, t);
            if g.vertex { write_base_pass_velocity(&mut floats, g, None); write_instancing(&mut floats, g, &InstancingParams::default()); } else { write_landscape_ps(&mut floats, g); }
            out.push((g.buf, crate::view::f32_bytes(&floats)));
        }
        if self.globals.iter().all(|g| g.vertex) { if let Some(w) = self.per_frame_write(mat, t.world) { out.push(w); } } // inferred fallback
        out
    }
}

/// FX SHADOW (2026-10-05): shader types of the whole-scene directional shadow depth (`FShadowDepthDrawingPolicy` with `bDirectionalLight`, not one-pass point
/// light, not RSM: VertexShadowDepth_OutputDepth / PixelShadowDepth_NonPerspectiveCorrect, UE4.16 ShadowDepthRendering.cpp:794-831; the type names are the
/// `IMPLEMENT_SHADOW_DEPTH_SHADERMODE_SHADERS` typedefs, :432 / :453 / :628).
pub const SHADOW_VS: &str = "TShadowDepthVSVertexShadowDepth_OutputDepthfalse";
pub const SHADOW_VS_POSITION_ONLY: &str = "TShadowDepthVSVertexShadowDepth_OutputDepthPositionOnly";
pub const SHADOW_PS: &str = "TShadowDepthPSPixelShadowDepth_NonPerspectiveCorrectfalse";
/// `UMaterial::GetDefaultMaterial(MD_Surface)` (the engine config's `DefaultMaterialName`): the material `OverrideWithDefaultMaterialForShadowDepth` draws opaque,
/// non-WPO materials with (ShadowDepthRendering.cpp:653).
pub const DEFAULT_SURFACE_MATERIAL: &str = "/Engine/EngineMaterials/WorldGridMaterial.WorldGridMaterial";

/// FX SHADOW: which programs draw `mat` into the sun's cascades, as `FShadowDepthDrawingPolicyFactory::AddStaticMesh` + the policy ctor choose them:
/// * translucent or unlit: none (`bProcess`, ShadowDepthRendering.cpp:1048);
/// * the material's own `TShadowDepthVS<OutputDepth, false, false>` when its shader map has one: cached only for masked or WPO materials (`ShouldCache`, :190),
///   which are exactly the ones `OverrideWithDefaultMaterialForShadowDepth` (:653) keeps; with the masked material's `TShadowDepthPS<NonPerspectiveCorrect>`
///   (:830), no pixel shader for an opaque WPO one (`WritesEveryPixel` + `SupportsNullPixelShader`, :813);
/// * otherwise the DEFAULT material's position-only VS (`bUsePositionOnlyVS`, :735 / :798), no pixel shader.
/// Returns `(material whose programs run: None = `mat`, Some = the default material, vs type, ps type or "")`.
pub fn shadow_programs<'a>(mat: &Material, default_mat: Option<&'a Material>, vf: Vf) -> Result<Option<(Option<&'a Material>, &'static str, &'static str)>, String> {
    if !matches!(mat.flags.blend.as_str(), "BLEND_Opaque" | "BLEND_Masked") || mat.flags.shading == "MSM_Unlit" { return Ok(None); }
    let masked = mat.flags.blend == "BLEND_Masked";
    if mat.cooked.find(vf.type_name(), SHADOW_VS).is_some() {
        if !masked { return Ok(Some((None, SHADOW_VS, ""))); }
        if mat.cooked.find(vf.type_name(), SHADOW_PS).is_none() { return Err(format!("{}: masked, but its shader map has no {SHADOW_PS}", mat.path)); }
        return Ok(Some((None, SHADOW_VS, SHADOW_PS)));
    }
    if masked { return Err(format!("{}: masked, but its shader map has no {SHADOW_VS}", mat.path)); }
    let d = default_mat.ok_or_else(|| format!("{}: opaque without WPO: needs the default material {DEFAULT_SURFACE_MATERIAL}", mat.path))?;
    if d.cooked.find(vf.type_name(), SHADOW_VS_POSITION_ONLY).is_none() { return Err(format!("{DEFAULT_SURFACE_MATERIAL}: no {SHADOW_VS_POSITION_ONLY}")); }
    Ok(Some((Some(d), SHADOW_VS_POSITION_ONLY, "")))
}

/// FX SHADOW: the shadow-depth pipeline + bind groups of `mat` (see [`shadow_programs`]); `None` = the material casts no shadow. Bind the per-draw `Primitive` /
/// `Material` buffers with [`MaterialGpu::bind_variant_globals`] (the material buffer only when the programs are `mat`'s own), and write
/// [`MaterialGpu::shadow_globals_writes`] every frame.
pub fn build_shadow(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, mat: &Material, default_mat: Option<&Material>, vf: Vf, num_uv: usize, shared: SharedBuffers, named: &[(&str, FxBufId)], front_cw: bool) -> Result<Option<(MaterialGpu, bool)>, String> {
    build_shadow_ext(set, fx, cache, mat, default_mat, vf, num_uv, shared, named, front_cw, false)
}
/// [`build_shadow`] with INSTANCING (2026-10-07): `instance_primitive` as [`LayoutOpts::instance_primitive`].
#[allow(clippy::too_many_arguments)]
pub fn build_shadow_ext(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, mat: &Material, default_mat: Option<&Material>, vf: Vf, num_uv: usize, shared: SharedBuffers, named: &[(&str, FxBufId)], front_cw: bool, instance_primitive: bool) -> Result<Option<(MaterialGpu, bool)>, String> {
    let Some((sel, vs, ps)) = shadow_programs(mat, default_mat, vf)? else { return Ok(None) };
    let own = sel.is_none();
    let m = sel.unwrap_or(mat);
    let gpu = build_material_layout(set, fx, cache, m, vf, vs, ps, num_uv, false, shared, named, LayoutOpts { front_cw, shadow: Some(mat.flags.two_sided), instance_primitive, ..LayoutOpts::default() })?;
    Ok(Some((gpu, own)))
}

impl MaterialGpu {
    /// FX SHADOW: this frame's contents of the shadow programs' material-owned globals: the per-frame expressions at `t` and the `FShadowDepthShaderParameters`
    /// of the VS. `ProjectionMatrix` (row-vector rows, as the base-pass globals) maps the translated UE world (cm; PreViewTranslation = -eye) to the PORT world
    /// (m, w = 1): `(t.x, t.z, t.y) * 0.01 + eye_m`; `ShadowParams` = (DepthBias 0, InvMaxSubjectDepth 1) and `bClampToNearPlane` = 0, so the program's
    /// `SetShadowDepthOutputs` (ShadowDepthVertexShader.usf:20) returns the world position and `fx_shadow_depth_outputs` (fn_render::fx::FX_SHADOW_WGSL) applies the
    /// cascade's `ProjectionMatrix`, the near clamp and `ShadowDepthBias` exactly as the one UE matrix (`FTranslationMatrix(PreShadowTranslation -
    /// PreViewTranslation) * SubjectAndReceiverMatrix`, ShadowRendering.h:1091) would.
    pub fn shadow_globals_writes(&self, mat: &Material, t: crate::uniform::MaterialTime, eye_m: [f32; 3]) -> Vec<(FxBufId, Vec<u8>)> {
        let mut out = vec![];
        for g in &self.globals {
            let mut floats = vec![0f32; g.float4s * 4];
            crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, t);
            if let Some(s) = g.params.shadow_depth_vs(&g.shader_type) {
                let rows: [[f32; 4]; 4] = [[0.01, 0., 0., 0.], [0., 0., 0.01, 0.], [0., 0.01, 0., 0.], [eye_m[0], eye_m[1], eye_m[2], 1.]];
                let mut put = |p: crate::shader_params::ShaderParam, v: &[f32]| { if p.is_bound() && p.buffer_index == g.slot { for (k, x) in v.iter().enumerate().take(p.num_bytes as usize / 4) { if let Some(f) = floats.get_mut(p.float_index() + k) { *f = *x; } } } };
                put(s.projection_matrix, &rows.concat());
                put(s.shadow_params, &[0., 1.]);
                put(s.clamp_to_near_plane, &[0.]);
            }
            out.push((g.buf, crate::view::f32_bytes(&floats)));
        }
        out
    }
}

/// FX SHADOW: the cooked distance field of a static mesh's LOD 0 (`FDistanceFieldVolumeData`, PF_G8) for the renderer's distance-field scene, as fn-app
/// `df_hook::load_one` reads the props'. `affects` = the component's `bAffectDistanceFieldLighting && CastShadow` (StaticMeshRender.cpp:1374).
pub fn load_mesh_distance_field(set: &PakSet, object_path: &str, affects: bool) -> Result<fn_render::distance_field::MeshDistanceField, String> {
    let long = crate::data::long_package(object_path);
    let (a, x, b) = crate::data::package_files(set, long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let m = fn_mesh::StaticMesh::parse_named(&p, object_path.split_once('.').map(|x| x.1)).map_err(|e| format!("{long}: {e}"))?; // the named mesh (a package may hold several)
    let d = m.distance_fields.first().cloned().flatten().ok_or_else(|| format!("{long}: no cooked distance field for LOD 0"))?;
    let (raw, fmt) = d.texels().ok_or_else(|| format!("{long}: empty / undecodable distance field volume"))?;
    if !matches!(fmt, fn_mesh::static_mesh::DistanceFieldFormat::G8) { return Err(format!("{long}: PF_R16F distance field (the 1.8 cook is PF_G8)")); }
    Ok(fn_render::distance_field::MeshDistanceField {
        size: [d.size[0].max(0) as u32, d.size[1].max(0) as u32, d.size[2].max(0) as u32], texels: raw, box_min: d.local_box_min, box_max: d.local_box_max,
        distance_min_max: d.distance_min_max, built_as_if_two_sided: d.built_as_if_two_sided, mesh_was_plane: d.mesh_was_plane,
        self_shadow_bias: fn_render::distance_field::ue::DEFAULT_SELF_SHADOW_BIAS, affects_distance_field_lighting: affects,
    })
}

/// Bytes per sprite vertex with the dynamic parameter stream folded in: [`SPRITE_VERTEX_STRIDE`] + `DynamicParameter` (float4).
pub const SPRITE_DYN_VERTEX_STRIDE: u32 = 88;
/// Attribute of the sprite vertex factory's `DynamicParameter` (`FVertexFactoryInput.DynamicParameter : ATTRIBUTE5`, UE4.16 ParticleSpriteVertexFactory.usf).
pub const SPRITE_DYNAMIC_ATTRIBUTE: u32 = 5;
/// The sprite layout of [`vertex_buffers`] plus the dynamic parameter float4 (the engine streams it from a second per-instance buffer; here it is interleaved).
pub fn sprite_vertex_buffers_dyn() -> Vec<FxVertexBuffer> {
    let mut v = vertex_buffers(Vf::Sprite, 0, false);
    v[0].stride = SPRITE_DYN_VERTEX_STRIDE;
    v[0].attrs.push(FxAttr { location: SPRITE_DYNAMIC_ATTRIBUTE, offset: SPRITE_VERTEX_STRIDE, format: FxFmt::Float32x4 });
    v
}

#[cfg(test)]
mod grass_tests {
    use super::*;
    /// `FFloat16::Set` truncates the mantissa (no rounding), flushes tiny values to 0 and clamps large ones to 65504.
    #[test] fn ffloat16_matches_the_engine_conversion() {
        assert_eq!(ffloat16(1.0), 0x3c00);
        assert_eq!(ffloat16(-2.0), 0xc000);
        assert_eq!(ffloat16(1.0 + 1.5 / 1024.0), 0x3c01, "1 + 1.5 ulp truncates to 1 ulp (round-to-nearest would give 0x3c02)");
        assert_eq!(ffloat16(1.0e-6), 0, "exponent <= 112: flushed");
        assert_eq!(ffloat16(-1.0e-6), 0x8000, "the sign survives the flush");
        assert_eq!(ffloat16(1.0e6), 0x7bff, "too large: 65504");
        assert_eq!(ffloat16(0.0), 0);
        assert_eq!((ffloat16(0.5), ffloat16(-0.5), ffloat16(6.1035156e-5)), (0x3800, 0xb800, 0x0400), "exponents 113..126 (below 1.0) rebias without overflow");
    }
    /// `FInstanceStream16::SetInstance`: float4 origin + random, three half4 rows (w 0), zero lightmap bias.
    #[test] fn ism_instance_layout() {
        let b = ism_instance([1.0, -2.0, 300.0], 0.25, [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, -0.5]]);
        let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let h = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        assert_eq!((f(0), f(4), f(8), f(12)), (1.0, -2.0, 300.0, 0.25));
        assert_eq!([h(16), h(18), h(20), h(22)], [0x3c00, 0, 0, 0]);
        assert_eq!([h(24), h(26), h(28), h(30)], [0, 0x4000, 0, 0]);
        assert_eq!([h(32), h(34), h(36), h(38)], [0, 0, 0xb800, 0]);
        assert!(b[40..48].iter().all(|x| *x == 0));
    }
    /// The instanced layout: UV pairs repeated to four attributes (the last pair), the 48-byte instance stream at ATTRIBUTE8..12.
    #[test] fn instanced_static_vertex_layout() {
        let v = vertex_buffers(Vf::InstancedStatic, 3, false);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].stride, 24 + 32);
        let uv: Vec<(u32, u32)> = v[0].attrs.iter().filter(|a| a.location >= 4).map(|a| (a.location, a.offset)).collect();
        assert_eq!(uv, vec![(4, 24), (5, 40), (6, 40), (7, 40)]);
        assert!(v[1].instance && v[1].stride == ISM_INSTANCE_STRIDE);
        assert_eq!(v[1].attrs.iter().map(|a| (a.location, a.offset)).collect::<Vec<_>>(), vec![(8, 0), (9, 16), (10, 24), (11, 32), (12, 40)]);
    }
    fn grass_batch(s: f32) -> HismBatch {
        HismBatch { start_cull_cm: 4096., end_cull_cm: 5120., view_distance_scale: s, lod: 0, first_lod: 0, screen_size: [1.0, 0., 0., 0., 0., 0., 0., 0.], num_lods: 1, sphere_radius: 62.947,
            proj_scale: (1.0, 1.7778), min_screen_size: 0.000005, lod_scale: 1.0, lod_random: 0.0, temporal_origin: [[1., 2., 3.], [4., 5., 6.]], temporal_alpha: 0.25, dithered: true }
    }
    /// SetMesh for the one-LOD grass: no fade-in (constant (0, 0, 1)), the cut at EndCullDistance x scale x scale, the fade parameters, the temporal origins.
    #[test] fn hism_set_mesh_values() {
        let p = InstancingParams::hism(&grass_batch(1.0));
        assert_eq!(p.fade_out, [4096., 1. / 1024., 1., 1.]);
        assert_eq!(p.view_z_constant, [0., 0., 1., 0.]);
        assert_eq!(p.view_z_compare_zero, [UE_MIN_FLT, UE_MIN_FLT, 5120., 0.]);
        assert_eq!(p.view_z_compare_one, [UE_MIN_FLT, UE_MIN_FLT, 5120., 0.]);
        assert_eq!(p.world_view_origin_zero, [1., 2., 3., 0.75]);
        assert_eq!(p.world_view_origin_one, [4., 5., 6., 0.25]);
        let q = InstancingParams::hism(&grass_batch(0.83));
        assert!((q.view_z_compare_zero[2] - 5120. * 0.83 * 0.83).abs() < 1e-2, "{q:?}");
        assert_eq!(q.fade_out[0], 4096., "the fade start is not scaled");
        let n = InstancingParams::hism(&HismBatch { dithered: false, ..grass_batch(1.0) });
        assert_eq!((n.view_z_constant, n.view_z_compare_zero, n.world_view_origin_one), ([0.; 4], [UE_MIN_FLT, UE_MIN_FLT, UE_MAX_FLT, 1.], [0., 0., 0., 1.]), "no LOD range: the defaults");
    }
    /// FTemporalLODState: the origin is resampled every lag; the transition runs 0..1 over the lag after it.
    #[test] fn temporal_lod_state() {
        let mut t = TemporalLod::default();
        t.update([10., 0., 0.], 1.0);
        assert_eq!((t.origin, t.time, t.lag), ([[0.; 3], [10., 0., 0.]], [0., 1.0], 0.5), "first sample: origin 0 kept as the old sample");
        t.update([20., 0., 0.], 1.2);
        assert_eq!(t.origin[1], [10., 0., 0.], "inside the lag: no new sample");
        assert!((t.transition(1.2) - (1.2 - 0.5) / 1.0).abs() < 1e-6);
        t.update([30., 0., 0.], 1.6);
        assert_eq!((t.origin, t.time), ([[10., 0., 0.], [30., 0., 0.]], [1.0, 1.6]));
        assert_eq!(t.transition(1.6), (1.6f32 - 0.5 - 1.0) / 0.6);
        assert_eq!(t.origin(0, [99.; 3]), [10., 0., 0.]);
        let mut paused = TemporalLod::default();
        paused.update([5., 5., 5.], 0.0);
        assert_eq!((paused.lag, paused.origin(1, [7.; 3])), (0.5, [0.; 3]), "time 0: no sample yet, the lag stays");
    }
}

/// LANDSCAPE MATERIAL (2026-10-07): the per-component bindings of a landscape material ([`MaterialGpu::bind_variant_landscape`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandscapeSlot { Heightmap, HeightmapSampler, Normalmap, NormalmapSampler, Uniform }

/// LANDSCAPE MATERIAL: the loose `$Globals` values `FLandscapeVertexFactoryVertexShaderParameters::SetMesh` writes for one batch element (UE4.16
/// LandscapeRender.cpp:2328-2405): LodBias (z = the heightmap's first resident mip), LodValues (CurrentLOD, -, LOD subsection quads, 1 / quads), SectionLods
/// (fractional LOD per subsection), NeighborSectionLod[4].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LandscapeVfValues { pub lod_bias: [f32; 4], pub lod_values: [f32; 4], pub section_lods: [f32; 4], pub neighbor_section_lod: [[f32; 4]; 4] }

/// LANDSCAPE MATERIAL: `LocalToWorldNoScaling` of the pixel stage (`SetMesh`, LandscapeRender.cpp:2448): the component transform without scale. The Athena landscape
/// components are not rotated (the exporter refuses rotated ones), and the program reads only the 3x3 rows (the per-pixel tangent basis): identity.
fn write_landscape_ps(floats: &mut [f32], g: &StageGlobals) {
    if g.vertex { return; }
    let Some(v) = g.params.landscape_ps() else { return };
    let p = v.local_to_world_no_scaling;
    if !p.is_bound() || p.buffer_index != g.slot { return; }
    for (k, f) in glam::Mat4::IDENTITY.to_cols_array().iter().enumerate().take(p.num_bytes as usize / 4) { if let Some(x) = floats.get_mut(p.float_index() + k) { *x = *f; } }
}
fn write_landscape_vs(floats: &mut [f32], g: &StageGlobals, l: &LandscapeVfValues) {
    let Some(v) = g.params.landscape_vs() else { return };
    let nb: Vec<f32> = l.neighbor_section_lod.iter().flatten().copied().collect();
    for (sp, val) in [(v.lod_bias, &l.lod_bias[..]), (v.lod_values, &l.lod_values[..]), (v.section_lods, &l.section_lods[..]), (v.neighbor_section_lod, &nb[..])] {
        if !sp.is_bound() || sp.buffer_index != g.slot { continue; }
        for (k, f) in val.iter().enumerate().take(sp.num_bytes as usize / 4) { if let Some(x) = floats.get_mut(sp.float_index() + k) { *x = *f; } }
    }
}

impl MaterialGpu {
    /// LANDSCAPE MATERIAL: whether this material was built for the landscape vertex factory with its per-component bindings.
    pub fn has_landscape_slots(&self) -> bool { !self.landscape_slots.is_empty() }
    /// LANDSCAPE MATERIAL: [`Self::vs_globals_bytes`] of a landscape program for one component: + the vertex factory's `SetMesh` values.
    pub fn vs_globals_bytes_landscape(&self, mat: &Material, t: crate::uniform::MaterialTime, prev_local_to_world: Option<glam::Mat4>, l: &LandscapeVfValues) -> Option<Vec<u8>> {
        let g = self.vs_globals()?;
        let mut floats = vec![0f32; g.float4s * 4];
        crate::uniform::write_per_frame(&mut floats, &mat.uniform, &mat.overrides, &g.params, g.slot, t);
        write_base_pass_velocity(&mut floats, g, prev_local_to_world);
        write_landscape_vs(&mut floats, g, l);
        Some(crate::view::f32_bytes(&floats))
    }
    /// LANDSCAPE MATERIAL: the bind groups of one landscape component: its Primitive buffer, its VS globals ([`Self::vs_globals_bytes_landscape`]), its
    /// `LandscapeParameters` uniform buffer, its heightmap (vertex stage, point sampler) and normal map (= the heightmap, pixel stage, `normal_sampler`).
    pub fn bind_variant_landscape(&self, fx: &mut FxRenderer, primitive: FxBufId, material: Option<FxBufId>, vs_globals: Option<FxBufId>, landscape: FxBufId, heightmap: FxTexId, normal_sampler: FxSamplerDesc) -> Vec<(u32, FxBindId)> {
        if let Some(v) = vs_globals { fx.register_height_fog(v, self.vs_fog.clone()); }
        let mut out = vec![];
        for (g, e) in self.entries.iter().enumerate().take(self.group_count) {
            let mut e = e.clone();
            for (sg, si) in &self.primitive_slots { if *sg == g { e[*si].res = FxRes::Buffer(primitive); } }
            if let Some(m) = material { for (sg, si) in &self.material_slots { if *sg == g { e[*si].res = FxRes::Buffer(m); } } }
            if let Some(v) = vs_globals { for (sg, si) in &self.vs_globals_slots { if *sg == g { e[*si].res = FxRes::Buffer(v); } } }
            for (sg, si, k) in &self.landscape_slots {
                if *sg != g { continue; }
                e[*si].res = match k { LandscapeSlot::Uniform => FxRes::Buffer(landscape), LandscapeSlot::Heightmap | LandscapeSlot::Normalmap => FxRes::Texture(heightmap), LandscapeSlot::NormalmapSampler => FxRes::Sampler(normal_sampler), LandscapeSlot::HeightmapSampler => continue };
            }
            out.push((g as u32, fx.create_bind_group(self.pipe, g as u32, e)));
        }
        out
    }
}

pub const WEB_FRAGMENT_SAMPLERS: usize = 16;

fn merge_equal_samplers(wgsl: &mut String, layouts: &mut [Vec<FxLayoutEntry>], entries: &mut [Vec<FxBindEntry>], n_groups: usize, limit: usize, slots: &mut [&mut Vec<(usize, usize)>], landscape: &mut Vec<(usize, usize, LandscapeSlot)>) {
    let is_sampler = |e: &FxLayoutEntry| matches!(e.ty, FxBindTy::Sampler { .. });
    let count = |w: &str, ls: &[Vec<FxLayoutEntry>]| { let tag = format!("@group({}) ", fn_render::fx::FX_SCENE_GROUP); ls.iter().take(n_groups).flatten().filter(|e| e.fragment && matches!(e.ty, FxBindTy::Sampler { .. })).count() + w.lines().map(str::trim_start).filter(|l| l.starts_with(&tag) && l.contains(" var ") && l.contains(": sampler")).count() };
    if count(wgsl, layouts) <= limit { return; }
    let var_name = |wgsl: &str, g: usize, b: u32| -> Option<String> {
        let tag = format!("@group({g}) @binding({b}) var ");
        wgsl.lines().map(str::trim_start).find(|l| l.starts_with(&tag) && l.contains(": sampler;")).and_then(|l| l[tag.len()..].split(':').next()).map(|n| n.trim().to_string())
    };
    for g in 0..n_groups.min(layouts.len()).min(entries.len()) {
        if layouts[g].len() != entries[g].len() { continue; }
        let mut i = 0;
        while i < layouts[g].len() {
            if count(wgsl, layouts) <= limit { return; }
            let (l, e) = (layouts[g][i].clone(), entries[g][i].clone());
            let FxRes::Sampler(desc) = e.res else { i += 1; continue };
            if !is_sampler(&l) || landscape.iter().any(|x| x.0 == g && x.1 == i) { i += 1; continue; }
            let keep = (0..i).find(|&k| layouts[g][k].ty == l.ty && layouts[g][k].vertex == l.vertex && layouts[g][k].fragment == l.fragment && matches!(entries[g][k].res, FxRes::Sampler(d) if d == desc) && !landscape.iter().any(|x| x.0 == g && x.1 == k));
            let (Some(k), Some(dup)) = (keep, var_name(wgsl, g, l.binding)) else { i += 1; continue };
            let Some(kept) = var_name(wgsl, g, layouts[g][k].binding) else { i += 1; continue };
            let decl = format!("@group({g}) @binding({}) var {dup}:", l.binding);
            let mut out = String::with_capacity(wgsl.len());
            for line in wgsl.split_inclusive('\n') { if !line.trim_start().starts_with(&decl) { out.push_str(line); } }
            *wgsl = replace_word(&out, &dup, &kept);
            layouts[g].remove(i);
            entries[g].remove(i);
            for v in slots.iter_mut() { for s in v.iter_mut() { if s.0 == g && s.1 > i { s.1 -= 1; } } }
            for s in landscape.iter_mut() { if s.0 == g && s.1 > i { s.1 -= 1; } }
        }
    }
    if count(wgsl, layouts) > limit {
        let descs: Vec<String> = entries.iter().enumerate().take(n_groups).flat_map(|(g, v)| v.iter().filter_map(move |e| if let FxRes::Sampler(d) = e.res { Some(format!("g{g}b{} {:?}", e.binding, d)) } else { None })).collect();
        let decls: Vec<&str> = wgsl.lines().map(str::trim_start).filter(|l| l.starts_with("@group(") && l.contains(": sampler;")).collect();
        eprintln!("[fx samplers] still {} > {limit}: {descs:?} decls {decls:?}", count(wgsl, layouts));
    }
}

fn replace_word(s: &str, from: &str, to: &str) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find(from) {
        let before = rest[..p].chars().next_back();
        let after = rest[p + from.len()..].chars().next();
        out.push_str(&rest[..p]);
        if before.map_or(false, ident) || after.map_or(false, ident) { out.push_str(from); } else { out.push_str(to); }
        rest = &rest[p + from.len()..];
    }
    out.push_str(rest);
    out
}
