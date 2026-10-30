//! CPU-side mesh / material descriptions handed to the renderer (the glTF loading lives in the viewer / asset crates).
use bytemuck::{Pod, Zeroable};
use image::RgbaImage;

/// Skinned vertex: 56 bytes, matches the vertex layout of `vs_skinned` in scene.wgsl / shadow.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct SkinnedVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialKind {
    /// Untextured white surface lit by sun + sky light.
    Generic = 0,
    /// Terrain grass (T_Grasslands_D x colour variation).
    Ground = 1,
    /// Character body / head: diffuse + normal + specular-mask + mask textures.
    Character = 2,
    /// Prop (pickaxe): character shading with constant specular / mask values.
    Prop = 3,
    /// BUILD ANIM HOOK: `M_BluePrintArchitect` (the building blueprint paper and the pencil, MIC `ArchitectTools`), as its compiled base-pass pixel shaders
    /// read it (all 50 PS permutations of the material's shader map, `fn-dxbc material`): BaseColor = `Diffuse`.rgb, Metallic / Specular / Roughness = G / R / B of
    /// the fixed `T_ArchitectTools_S` (`spec` slot, sampled `.yxz`), EmissiveColor = max(`EmissiveMask`.rgb (`mask` slot) x `EmissiveMultiply` (cb2[2].x) +
    /// `SelectionColor`.rgb (cb2[1], 0 at runtime), 0). `EmissiveMultiply` is the per-model scalar of [`crate::Renderer::set_model_scalar`] (default 1.0 = the MIC).
    Architect = 4,
}

/// Material textures (RGBA8). Missing slots fall back to neutral 1x1 values (flat normal, no spec, no mask).
#[derive(Clone, Debug, Default)]
pub struct MaterialImages {
    pub diffuse: Option<RgbaImage>,
    pub normal: Option<RgbaImage>,
    pub spec: Option<RgbaImage>,
    pub mask: Option<RgbaImage>,
    /// CHAR MATERIAL HOOK: the original compiled material of a BR character primitive (see char_material): constants + fixed textures; None = the generic character shading.
    pub ue: Option<std::sync::Arc<crate::char_material::UeMaterial>>,
    /// The material's resolved `TwoSided` flag (root `UMaterial::TwoSided`, MIC `BasePropertyOverrides` only with `bOverride_TwoSided`): the shadow depth pass
    /// draws both faces of a two-sided material (UE4.16 ShadowDepthRendering.cpp:1059), one-sided ones cull back faces.
    pub two_sided: bool,
    /// TEXTURE MEMORY (2026-10-06): the cooked mip chains of `diffuse`, `normal`, `spec`, `mask` (in that order). A present chain is uploaded instead of the
    /// image and the mips generated from it: the mips UE draws, BC blocks where the device takes them.
    pub cooked: [Option<std::sync::Arc<CookedTexture>>; 4],
}

/// TEXTURE MEMORY (2026-10-06): a texture's cooked mip chain as the GPU takes it: per level from the largest (width, height, data) in `format` (RGBA8 pixels,
/// or the BC blocks as cooked, see [`crate::fx::FxTexFormat`]).
#[derive(Clone, Debug)]
pub struct CookedTexture { pub format: crate::fx::FxTexFormat, pub levels: Vec<(u32, u32, Vec<u8>)> }
impl CookedTexture {
    /// Uploads the chain (`srgb`: the `_UNORM_SRGB` view for RGBA8 / BC1 / BC2 / BC3 / BC7; BC4 / BC5 are always UNORM, `FxTexFormat::wgpu`). A BC chain
    /// the device cannot take (no `TEXTURE_COMPRESSION_BC`, level 0 not a multiple of 4) is the caller's to decode first.
    pub fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, srgb: bool, label: &str) -> wgpu::Texture {
        let (w0, h0, _) = self.levels[0];
        let f = self.format;
        let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some(label), size: wgpu::Extent3d { width: w0, height: h0, depth_or_array_layers: 1 }, mip_level_count: self.levels.len() as u32, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: f.wgpu(srgb), usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        for (i, (w, h, data)) in self.levels.iter().enumerate() {
            let at = wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All };
            if f.is_bc() {
                let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
                queue.write_texture(at, &data[..f.level_bytes(*w, *h)], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bw * f.unit_bytes() as u32), rows_per_image: Some(bh) }, wgpu::Extent3d { width: bw * 4, height: bh * 4, depth_or_array_layers: 1 });
            } else {
                queue.write_texture(at, data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(*h) }, wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 });
            }
        }
        tex
    }
    /// Bytes of all levels.
    pub fn bytes(&self) -> u64 { self.levels.iter().map(|(w, h, _)| self.format.level_bytes(*w, *h) as u64).sum() }
}

pub fn prepare_mip_chains(prims: &mut [SkinnedPrimitive]) {
    for p in prims {
        let m = &mut p.material;
        for (img, c) in [&m.diffuse, &m.normal, &m.spec, &m.mask].into_iter().zip(m.cooked.iter_mut()) {
            if let (Some(i), None) = (img, c.as_ref()) {
                let levels = crate::texture::build_mips(i, crate::texture::MipFilter::Triangle).into_iter().map(|l| (l.width(), l.height(), l.into_raw())).collect();
                *c = Some(std::sync::Arc::new(CookedTexture { format: crate::fx::FxTexFormat::Rgba8, levels }));
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkinnedPrimitive {
    pub vertices: Vec<SkinnedVertex>,
    pub indices: Vec<u32>,
    pub material: MaterialImages,
    pub kind: MaterialKind,
}
impl SkinnedPrimitive {
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for v in &self.vertices { for i in 0..3 { lo[i] = lo[i].min(v.pos[i]); hi[i] = hi[i].max(v.pos[i]); } }
        (lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vertex_is_56_bytes_without_padding() {
        assert_eq!(std::mem::size_of::<SkinnedVertex>(), 56);
        assert_eq!(std::mem::align_of::<SkinnedVertex>(), 4);
    }
    #[test]
    fn bounds_cover_all_vertices() {
        let p = SkinnedPrimitive { vertices: vec![SkinnedVertex { pos: [1., -2., 3.], ..Default::default() }, SkinnedVertex { pos: [-1., 5., 0.], ..Default::default() }], indices: vec![0, 1, 0], material: MaterialImages::default(), kind: MaterialKind::Character };
        assert_eq!(p.bounds(), ([-1., -2., 0.], [1., 5., 3.]));
    }
}
