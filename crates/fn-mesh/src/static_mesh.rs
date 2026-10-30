//! `UStaticMesh` decoding for UE 4.16 / Fortnite 1.8 (UEViewer `fn18`; static meshes follow the plain 4.16 layout,
//! except that the area-weighted sampler data added later in 4.16 is **not** serialised).
//!
//! # Native layout after the tagged properties (+ `bHasGuid` i32), cooked (`FStripDataFlags` global bit0 set)
//!
//! ```text
//! FStripDataFlags (2)
//! i32 bCooked
//! i32 BodySetup (object ref), i32 NavCollision (object ref)
//! [editor data stripped -> no HighResSourceMeshName/CRC]
//! FGuid LightingGuid (16)
//! TArray<UStaticMeshSocket*> Sockets      i32 n, n x i32 object refs (also a tagged property)
//! TArray<FStaticMeshLODResources> LODs    i32 n; each:
//!     FStripDataFlags (2)
//!     TArray<FStaticMeshSection> Sections   i32 n; each 28 bytes: i32 MaterialIndex, FirstIndex, NumTriangles,
//!                                           MinVertexIndex, MaxVertexIndex, bool bEnableCollision, bool bCastShadow
//!     f32 MaxDeviation
//!     [!server stripped && !classStripped(2)]
//!     FPositionVertexBuffer    i32 Stride(12), i32 NumVertices, bulk array of FVector
//!     FStaticMeshVertexBuffer  FStripDataFlags(2), i32 NumTexCoords, i32 Stride, i32 NumVertices, bool bUseFullPrecisionUVs,
//!                              bool bUseHighPrecisionTangentBasis, bulk array of { TangentX, TangentZ (4 x u8 each, or
//!                              4 x u16 each with high precision), UV[NumTexCoords] (2 x f16 or 2 x f32) }
//!     FColorVertexBuffer       FStripDataFlags(2), i32 Stride, i32 NumVertices, [NumVertices>0] bulk array of FColor
//!     FRawStaticIndexBuffer IndexBuffer              i32 bIs32Bit, bulk array of bytes
//!     FRawStaticIndexBuffer ReversedIndexBuffer, DepthOnlyIndexBuffer, ReversedDepthOnlyIndexBuffer   (unless classStripped(4))
//!     FRawStaticIndexBuffer WireframeIndexBuffer     (unless editor data stripped)
//!     FRawStaticIndexBuffer AdjacencyIndexBuffer     (unless classStripped(1))
//!     (fn18: no FStaticMeshSectionAreaWeightedTriangleSampler data)
//! FStripDataFlags (2); per LOD: i32 bHasDistanceFieldData [+ FDistanceFieldVolumeData when set]
//!     FDistanceFieldVolumeData (4.16): bulk-less TArray<u8> CompressedDistanceFieldVolume (i32 n + bytes),
//!         FIntVector Size(12), FBox LocalBoundingBox (min 12, max 12, u8 valid), FVector2D DistanceMinMax(8),
//!         bool bMeshWasClosed, bool bBuiltAsIfTwoSided, bool bMeshWasPlane
//! FBoxSphereBounds Bounds (28)
//! f32 ScreenSize[8]                       (fn18 quirk: no bLODsShareStaticLighting before it)
//! i32 bHasSpeedTreeWind (0)
//! TArray<FStaticMaterial> StaticMaterials   i32 n; { i32 objectRef, FName slotName, FMeshUVChannelInfo (24) }
//!                                         (the export ends here)
//! ```
//!
//! The fn18 snapshot is older than the final 4.16: it predates `bLODsShareStaticLighting` (UEViewer's version
//! gates assume the release 4.16 layout; that field was never reached there because the decoder dropped the rest of
//! the export). Verified against the raw bytes of the sky meshes: `Bounds` is followed directly by
//! `ScreenSize[0] = 1.0`, and the `StaticMaterials` count follows the 8 screen sizes and the SpeedTree flag.
//!
//! The `FRawStaticIndexBuffer` byte array is `i32 elemSize(1) i32 count` then `count` bytes holding u16 or u32
//! indices depending on `bIs32Bit`.

use crate::common::*;
use crate::skeletal::UvChannelInfo;
use fn_ueio::{f16_to_f32, invalid, Package, Reader, Result};

/// `FStaticMeshSection`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticSection {
    pub material_index: i32,
    /// First index in the LOD index buffer.
    pub first_index: u32,
    pub num_triangles: u32,
    pub min_vertex_index: u32,
    pub max_vertex_index: u32,
    pub enable_collision: bool,
    pub cast_shadow: bool,
}

/// `FStaticMaterial`.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMaterial {
    pub slot_name: String,
    /// Object path of the material, `None` for a null reference (the sky dome uses `None`).
    pub material: Option<String>,
    pub material_name: Option<String>,
    pub uv_channel: UvChannelInfo,
}

/// One static mesh LOD (`FStaticMeshLODResources`), struct-of-arrays vertices.
#[derive(Debug, Clone, Default)]
pub struct StaticLod {
    pub sections: Vec<StaticSection>,
    /// Positions in UE space (cm).
    pub positions: Vec<[f32; 3]>,
    /// Packed TangentX, bytes as stored (`x*127.5+127.5`), converted from 16 bit when the file uses high precision.
    pub tangent_x: Vec<[u8; 4]>,
    /// Packed TangentZ (normal); byte 3 is the binormal sign.
    pub tangent_z: Vec<[u8; 4]>,
    /// `uvs[channel][vertex]`.
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub num_tex_coords: usize,
    pub uv_full_precision: bool,
    pub high_precision_tangents: bool,
    /// Vertex colours (memory order of `FColor`: B, G, R, A), empty when the mesh has none.
    pub colors: Vec<[u8; 4]>,
    /// Triangle list indices of the main index buffer (widened).
    pub indices: Vec<u32>,
    pub indices_32bit: bool,
    pub max_deviation: f32,
}

impl StaticLod {
    /// Unit vertex normal (TangentZ) in UE space.
    pub fn normal(&self, vertex: usize) -> [f32; 3] {
        unpack_normal_unit(self.tangent_z[vertex])
    }
    /// Unit tangent (TangentX) in UE space.
    pub fn tangent(&self, vertex: usize) -> [f32; 3] {
        unpack_normal_unit(self.tangent_x[vertex])
    }
    /// Binormal sign (+1 / -1) stored in TangentZ.w.
    pub fn binormal_sign(&self, vertex: usize) -> f32 {
        tangent_sign(self.tangent_z[vertex])
    }
}

/// Decoded `UStaticMesh`.
#[derive(Debug, Clone)]
pub struct StaticMesh {
    pub name: String,
    pub bounds: Bounds,
    pub materials: Vec<StaticMaterial>,
    pub lods: Vec<StaticLod>,
    pub screen_size: [f32; 8],
    pub lighting_guid: [u8; 16],
    /// `UBodySetup` reference of the cooked mesh: an export name (same package) or an import path. `None` = no collision.
    pub body_setup: Option<String>,
    /// `UNavCollision` reference, same convention.
    pub nav_collision: Option<String>,
    /// `UStaticMeshSocket` references (exports of the same package).
    pub sockets: Vec<String>,
    /// Cooked `FDistanceFieldVolumeData` per LOD (`None` when the LOD has none; only LOD 0 is ever built, StaticMesh.cpp
    /// `FStaticMeshRenderData::Cache`).
    pub distance_fields: Vec<Option<DistanceFieldVolumeData>>,
    /// `LODForCollision` tagged property (index of the LOD the simple/complex collision is built from, default 0).
    pub lod_for_collision: i32,
    /// `LightMapCoordinateIndex` tagged property (UV channel of the lightmap, default 0).
    pub light_map_coordinate_index: i32,
}

impl StaticMesh {
    /// Decode the first `StaticMesh` export of the package.
    pub fn parse(pkg: &Package) -> Result<StaticMesh> { Self::parse_export(pkg, find_class(pkg, "StaticMesh")?) }
    /// Decode the `StaticMesh` export named `name` (a package with several meshes: the HLOD proxies of a level's `*_HLOD` package), else the first one.
    pub fn parse_named(pkg: &Package, name: Option<&str>) -> Result<StaticMesh> {
        match name.and_then(|n| (0..pkg.exports.len()).find(|&i| pkg.exports[i].object_name == n && pkg.export_class(i).eq_ignore_ascii_case("StaticMesh"))) { Some(i) => Self::parse_export(pkg, i), None => Self::parse(pkg) }
    }
    /// Decode the `StaticMesh` export `idx`.
    pub fn parse_export(pkg: &Package, idx: usize) -> Result<StaticMesh> {
        let ExportBody { props, mut reader, end } = export_body(pkg, idx)?;
        let names = &pkg.names;
        let r = &mut reader;

        let strip = StripFlags::read(r)?;
        let cooked = bool32(r)?;
        let body_setup = r.i32()?;
        let nav_collision = r.i32()?;
        if !strip.editor_stripped() {
            r.fstring()?; // HighResSourceMeshName
            r.u32()?; // HighResSourceMeshCRC
        }
        let lighting_guid = r.guid()?;
        let n_sockets = array_count(r, 4)?;
        let mut sockets = Vec::with_capacity(n_sockets);
        for _ in 0..n_sockets {
            if let Some(s) = object_path(pkg, r.i32()?) {
                sockets.push(s);
            }
        }
        if !strip.editor_stripped() {
            return invalid("StaticMesh with editor data (source models) is not supported");
        }
        if !cooked {
            return invalid("uncooked StaticMesh is not supported");
        }

        let n_lods = array_count(r, 4)?;
        let mut lods = Vec::with_capacity(n_lods);
        for li in 0..n_lods {
            lods.push(read_lod(r).map_err(|e| fn_ueio::Error::Invalid(format!("LOD {li}: {e}")))?);
        }

        // Distance field data (UE4.16 StaticMesh.cpp FStaticMeshRenderData::Serialize, "Inline the distance field derived data for cooked builds")
        let mut distance_fields = vec![None; n_lods];
        let dstrip = StripFlags::read(r)?;
        if !dstrip.server_stripped() {
            for df in distance_fields.iter_mut() {
                if bool32(r)? {
                    *df = Some(DistanceFieldVolumeData::read(r)?);
                }
            }
        }
        let bounds = Bounds::read(r)?;
        // fn18 quirk: this snapshot has no `bLODsShareStaticLighting` here (verified on the sky dome and sun/moon
        // meshes: Bounds is followed directly by ScreenSize[0] = 1.0).
        let mut screen_size = [0.0f32; 8];
        for s in screen_size.iter_mut() {
            *s = r.f32()?;
        }
        if bool32(r)? {
            return invalid("StaticMesh with SpeedTree wind data is not supported");
        }
        let n = array_count(r, 4)?;
        let mut materials = Vec::with_capacity(n);
        for _ in 0..n {
            let mref = r.i32()?;
            let slot_name = fname(r, names)?;
            let uv_channel = UvChannelInfo::read(r)?;
            materials.push(StaticMaterial { slot_name, material: object_path(pkg, mref), material_name: object_name(pkg, mref), uv_channel });
        }
        if r.tell() != end {
            return invalid(format!("StaticMesh export not fully consumed: stopped at {} of {}", r.tell() - pkg.export_range(idx).0, end - pkg.export_range(idx).0));
        }
        Ok(StaticMesh {
            name: pkg.exports[idx].object_name.clone(),
            bounds,
            materials,
            lods,
            screen_size,
            lighting_guid,
            body_setup: object_path(pkg, body_setup),
            nav_collision: object_path(pkg, nav_collision),
            sockets,
            distance_fields,
            lod_for_collision: props.int("LODForCollision").unwrap_or(0) as i32,
            light_map_coordinate_index: props.int("LightMapCoordinateIndex").unwrap_or(0) as i32,
        })
    }
}

/// `FDistanceFieldVolumeData` (UE4.16 DistanceFieldAtlas.h `operator<<`): `CompressedDistanceFieldVolume << Size << LocalBoundingBox
/// << DistanceMinMax << bMeshWasClosed << bBuiltAsIfTwoSided << bMeshWasPlane`. Despite the name the volume is raw unless the cook ran with
/// `r.DistanceFieldBuild.Compress=1` (default 0); the voxel format is PF_R16F unless `r.DistanceFieldBuild.EightBit=1` (default 0). See
/// [`DistanceFieldVolumeData::voxels`] for how the format is detected from the byte count.
#[derive(Debug, Clone, PartialEq)]
pub struct DistanceFieldVolumeData {
    pub compressed_volume: Vec<u8>,
    pub size: [i32; 3],
    /// `FBox LocalBoundingBox` min / max (cm, mesh local space) and `IsValid`.
    pub local_box_min: [f32; 3],
    pub local_box_max: [f32; 3],
    pub local_box_valid: bool,
    pub distance_min_max: [f32; 2],
    pub mesh_was_closed: bool,
    pub built_as_if_two_sided: bool,
    pub mesh_was_plane: bool,
}

impl DistanceFieldVolumeData {
    fn read(r: &mut Reader) -> Result<Self> {
        let n = array_count(r, 1)?;
        let compressed_volume = r.bytes(n)?.to_vec();
        let size = [r.i32()?, r.i32()?, r.i32()?];
        let local_box_min = r.vec3()?;
        let local_box_max = r.vec3()?;
        let local_box_valid = r.bytes(1)?[0] != 0;
        let distance_min_max = [r.f32()?, r.f32()?];
        Ok(DistanceFieldVolumeData {
            compressed_volume,
            size,
            local_box_min,
            local_box_max,
            local_box_valid,
            distance_min_max,
            mesh_was_closed: bool32(r)?,
            built_as_if_two_sided: bool32(r)?,
            mesh_was_plane: bool32(r)?,
        })
    }

    /// Number of voxels (`Size.X * Size.Y * Size.Z`).
    pub fn voxel_count(&self) -> usize {
        self.size.iter().map(|&s| s.max(0) as usize).product()
    }

    /// The atlas texel bytes (`X*Y*Z*FormatSize`, X fastest, then Y, then Z) and the format: the stored array is zlib-inflated when it is
    /// not already the raw size (`r.DistanceFieldBuild.Compress=1`, `FCompression::UncompressMemory(COMPRESS_ZLIB)` in
    /// DistanceFieldAtlas.cpp UpdateAllocations). The format follows from the inflated size: `X*Y*Z` bytes = PF_G8 (`EightBit=1`),
    /// `2*X*Y*Z` = PF_R16F.
    pub fn texels(&self) -> Option<(Vec<u8>, DistanceFieldFormat)> {
        let n = self.voxel_count();
        if n == 0 || self.compressed_volume.is_empty() {
            return None;
        }
        let raw = if self.compressed_volume.len() == n || self.compressed_volume.len() == n * 2 {
            self.compressed_volume.clone()
        } else {
            { let mut v = miniz_oxide::inflate::decompress_to_vec_zlib(&self.compressed_volume).ok()?; v.shrink_to_fit(); v }
        };
        let fmt = if raw.len() == n {
            DistanceFieldFormat::G8
        } else if raw.len() == n * 2 {
            DistanceFieldFormat::R16F
        } else {
            return None;
        };
        Some((raw, fmt))
    }

    /// Decoded volume-space distances (MeshDistanceFieldUtilities.cpp: `MinDistance / VolumeBounds.GetExtent().GetMax()`), the value the
    /// shaders read from the atlas (8 bit: `DistanceMinMax.x + v * (DistanceMinMax.y - DistanceMinMax.x)`, the inverse of the build's
    /// `(d - Min) / (Max - Min)` quantisation; R16F: the half value).
    pub fn voxels(&self) -> Option<Vec<f32>> {
        let (raw, fmt) = self.texels()?;
        Some(match fmt {
            DistanceFieldFormat::G8 => {
                let (a, b) = (self.distance_min_max[0], self.distance_min_max[1]);
                raw.iter().map(|&v| a + (v as f32 / 255.0) * (b - a)).collect()
            }
            DistanceFieldFormat::R16F => raw.chunks_exact(2).map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]]))).collect(),
        })
    }
}

/// Atlas texel format of a mesh distance field (DistanceFieldAtlas.cpp `InitializeIfNeeded`: `bEightBitFixedPoint ? PF_G8 : PF_R16F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceFieldFormat {
    G8,
    R16F,
}

/// `FRawStaticIndexBuffer`: returns `(indices, is_32bit)`.
fn read_raw_index_buffer(r: &mut Reader) -> Result<(Vec<u32>, bool)> {
    let is32 = bool32(r)?;
    let (esize, count) = bulk_header(r)?;
    if esize != 1 {
        return invalid(format!("raw index buffer element size {esize} != 1"));
    }
    let bytes = r.bytes(count)?;
    let idx: Vec<u32> = if is32 {
        bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    } else {
        bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect()
    };
    Ok((idx, is32))
}

fn read_lod(r: &mut Reader) -> Result<StaticLod> {
    let mut lod = StaticLod::default();
    let strip = StripFlags::read(r)?;
    let n = array_count(r, 28)?;
    for _ in 0..n {
        let s = StaticSection {
            material_index: r.i32()?,
            first_index: r.u32()?,
            num_triangles: r.u32()?,
            min_vertex_index: r.u32()?,
            max_vertex_index: r.u32()?,
            enable_collision: bool32(r)?,
            cast_shadow: bool32(r)?,
        };
        lod.sections.push(s);
    }
    lod.max_deviation = r.f32()?;
    if strip.server_stripped() || strip.class_stripped(2) {
        return Ok(lod);
    }

    // FPositionVertexBuffer
    let _stride = r.i32()?;
    let num_verts = r.i32()?;
    let (esize, count) = bulk_header(r)?;
    if esize != 12 || count != num_verts.max(0) as usize {
        return invalid(format!("position buffer {count}x{esize} (header {num_verts})"));
    }
    lod.positions.reserve(count);
    for _ in 0..count {
        lod.positions.push(r.vec3()?);
    }

    // FStaticMeshVertexBuffer
    let vstrip = StripFlags::read(r)?;
    let num_tex = r.i32()?;
    let _vstride = r.i32()?;
    let vnum = r.i32()?;
    let full = bool32(r)?;
    let high = bool32(r)?;
    if !(0..=8).contains(&num_tex) {
        return invalid(format!("implausible NumTexCoords {num_tex}"));
    }
    let num_tex = num_tex as usize;
    lod.num_tex_coords = num_tex;
    lod.uv_full_precision = full;
    lod.high_precision_tangents = high;
    if !vstrip.server_stripped() {
        let (esize, count) = bulk_header(r)?;
        let tan = if high { 16 } else { 8 };
        let uv = if full { 8 } else { 4 } * num_tex;
        if esize != tan + uv || count != vnum.max(0) as usize {
            return invalid(format!("static vertex buffer {count}x{esize} (header {vnum}); expected stride {}", tan + uv));
        }
        lod.uvs = vec![Vec::with_capacity(count); num_tex];
        for _ in 0..count {
            if high {
                let mut t = [[0u8; 4]; 2];
                for p in t.iter_mut() {
                    // FPackedRGBA16N -> FPackedNormal: v = (u - 32767.5)/32767.5, byte = int((v+1)*127.5)
                    for c in p.iter_mut() {
                        let u = r.u16()?;
                        let v = (u as f32 - 32767.5) / 32767.5;
                        *c = ((v + 1.0) * 127.5) as i32 as u8;
                    }
                }
                lod.tangent_x.push(t[0]);
                lod.tangent_z.push(t[1]);
            } else {
                let a = r.bytes(4)?;
                let b = r.bytes(4)?;
                lod.tangent_x.push([a[0], a[1], a[2], a[3]]);
                lod.tangent_z.push([b[0], b[1], b[2], b[3]]);
            }
            for u in lod.uvs.iter_mut() {
                if full {
                    u.push([r.f32()?, r.f32()?]);
                } else {
                    u.push([f16_to_f32(r.u16()?), f16_to_f32(r.u16()?)]);
                }
            }
        }
    }

    // FColorVertexBuffer
    let cstrip = StripFlags::read(r)?;
    let _cstride = r.i32()?;
    let cnum = r.i32()?;
    if !cstrip.server_stripped() && cnum > 0 {
        let (esize, count) = bulk_header(r)?;
        if esize != 4 {
            return invalid(format!("colour stride {esize}"));
        }
        for _ in 0..count {
            let c = r.bytes(4)?;
            lod.colors.push([c[0], c[1], c[2], c[3]]);
        }
    }

    let (indices, is32) = read_raw_index_buffer(r)?;
    lod.indices = indices;
    lod.indices_32bit = is32;
    if !strip.class_stripped(4) {
        read_raw_index_buffer(r)?; // ReversedIndexBuffer
        read_raw_index_buffer(r)?; // DepthOnlyIndexBuffer
        read_raw_index_buffer(r)?; // ReversedDepthOnlyIndexBuffer
    } else {
        read_raw_index_buffer(r)?; // DepthOnlyIndexBuffer
    }
    if !strip.editor_stripped() {
        read_raw_index_buffer(r)?; // WireframeIndexBuffer
    }
    if !strip.class_stripped(1) {
        read_raw_index_buffer(r)?; // AdjacencyIndexBuffer
    }
    Ok(lod)
}


/// Result of [`StaticMesh::validate`] for one LOD: the numbers the cooked metadata promises vs. what the buffers hold.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LodCheck {
    pub vertices: usize,
    /// Triangles according to the section table (`sum(NumTriangles)`).
    pub section_triangles: usize,
    /// Triangles according to the index buffer length / 3.
    pub index_triangles: usize,
    /// Every section's `MinVertexIndex..=MaxVertexIndex` equals the real min/max of its indices.
    pub section_ranges_exact: bool,
    pub problems: Vec<String>,
}

impl StaticLod {
    /// Number of triangles in the section table.
    pub fn section_triangles(&self) -> usize {
        self.sections.iter().map(|s| s.num_triangles as usize).sum()
    }
    /// Indices `[first_index, first_index + 3 * num_triangles)` of a section.
    pub fn section_indices(&self, section: usize) -> &[u32] {
        let s = &self.sections[section];
        let a = s.first_index as usize;
        &self.indices[a..a + 3 * s.num_triangles as usize]
    }
}

impl StaticMesh {
    /// Material slot used by section `section` of LOD `lod`.
    pub fn section_material(&self, lod: usize, section: usize) -> Option<&StaticMaterial> {
        let s = self.lods.get(lod)?.sections.get(section)?;
        self.materials.get(usize::try_from(s.material_index).ok()?)
    }

    /// Axis aligned box of the LOD's positions (UE space), `None` for an empty LOD.
    pub fn position_bounds(&self, lod: usize) -> Option<([f32; 3], [f32; 3])> {
        let l = self.lods.get(lod)?;
        let mut it = l.positions.iter();
        let first = *it.next()?;
        let (mut mn, mut mx) = (first, first);
        for p in it {
            for c in 0..3 {
                mn[c] = mn[c].min(p[c]);
                mx[c] = mx[c].max(p[c]);
            }
        }
        Some((mn, mx))
    }

    /// Cross-check each LOD's section table, index buffer, vertex streams and material slots against each other (the
    /// uasset's own metadata is the section table, the buffer headers and `FBoxSphereBounds`).
    pub fn validate(&self) -> Vec<LodCheck> {
        let mut out = vec![];
        for (li, l) in self.lods.iter().enumerate() {
            let n = l.positions.len();
            let mut c = LodCheck { vertices: n, section_triangles: l.section_triangles(), index_triangles: l.indices.len() / 3, section_ranges_exact: true, problems: vec![] };
            if l.indices.len() % 3 != 0 {
                c.problems.push(format!("LOD{li}: index count {} is not a multiple of 3", l.indices.len()));
            }
            if c.section_triangles != c.index_triangles {
                c.problems.push(format!("LOD{li}: sections hold {} triangles, index buffer {}", c.section_triangles, c.index_triangles));
            }
            if l.tangent_x.len() != n || l.tangent_z.len() != n || l.uvs.iter().any(|u| u.len() != n) || (!l.colors.is_empty() && l.colors.len() != n) {
                c.problems.push(format!("LOD{li}: vertex streams differ in length (positions {n})"));
            }
            if l.uvs.len() != l.num_tex_coords {
                c.problems.push(format!("LOD{li}: {} UV sets decoded, header says {}", l.uvs.len(), l.num_tex_coords));
            }
            let mut covered = 0usize;
            for (si, s) in l.sections.iter().enumerate() {
                let a = s.first_index as usize;
                let b = a + 3 * s.num_triangles as usize;
                if b > l.indices.len() {
                    c.problems.push(format!("LOD{li} section {si}: index range {a}..{b} exceeds {}", l.indices.len()));
                    continue;
                }
                covered += b - a;
                let idx = &l.indices[a..b];
                if let (Some(&mn), Some(&mx)) = (idx.iter().min(), idx.iter().max()) {
                    if mx as usize >= n {
                        c.problems.push(format!("LOD{li} section {si}: index {mx} >= {n} vertices"));
                    }
                    if mn != s.min_vertex_index || mx != s.max_vertex_index {
                        c.section_ranges_exact = false;
                        c.problems.push(format!("LOD{li} section {si}: header vertex range {}..{} but indices span {mn}..{mx}", s.min_vertex_index, s.max_vertex_index));
                    }
                }
                if s.material_index < 0 || s.material_index as usize >= self.materials.len() {
                    c.problems.push(format!("LOD{li} section {si}: material index {} outside the {} slots", s.material_index, self.materials.len()));
                }
            }
            if covered != l.indices.len() {
                c.problems.push(format!("LOD{li}: sections cover {covered} of {} indices", l.indices.len()));
            }
            out.push(c);
        }
        out
    }
}
