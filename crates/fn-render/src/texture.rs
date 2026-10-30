//! Texture / buffer upload helpers.
//!
//! Colour textures are uploaded as 8-bit UNORM with a CPU-built mip chain (like the prototype, filtered in the stored colour
//! space; the scene shader decodes sRGB itself after filtering). Textures that the sky material marks as sRGB use the
//! hardware sRGB format (the prototype used SRGB8_ALPHA8 there too).
use image::RgbaImage;
use wgpu::util::DeviceExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MipFilter {
    /// `image::imageops::resize(.., Triangle)` from the previous level (scene textures).
    Triangle,
    /// Plain 2x2 box average (sky textures, like the editor's mip generation).
    Box,
}

/// Number of mip levels down to 1x1.
pub fn mip_levels(w: u32, h: u32) -> u32 { w.max(h).max(1).ilog2() + 1 }

fn box_down(src: &RgbaImage) -> RgbaImage {
    let (w, h) = ((src.width() / 2).max(1), (src.height() / 2).max(1));
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut s = [0u32; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = src.get_pixel((x * 2 + dx).min(src.width() - 1), (y * 2 + dy).min(src.height() - 1));
                    for c in 0..4 { s[c] += p[c] as u32; }
                }
            }
            out.put_pixel(x, y, image::Rgba([(s[0] / 4) as u8, (s[1] / 4) as u8, (s[2] / 4) as u8, (s[3] / 4) as u8]));
        }
    }
    out
}

/// Builds all mip levels of an image (level 0 is the image itself).
pub fn build_mips(img: &RgbaImage, filter: MipFilter) -> Vec<RgbaImage> {
    let mut levels = vec![img.clone()];
    while levels.last().map_or(false, |l| l.width() > 1 || l.height() > 1) {
        let prev = levels.last().unwrap();
        let next = match filter {
            MipFilter::Triangle => image::imageops::resize(prev, (prev.width() / 2).max(1), (prev.height() / 2).max(1), image::imageops::FilterType::Triangle),
            MipFilter::Box => box_down(prev),
        };
        levels.push(next);
    }
    levels
}

/// Uploads an RGBA8 image with a full mip chain. `srgb`: the texture's `SRGB` flag (hardware decode on sampling, like UE's `*_UNORM_SRGB` SRV); its
/// mips are then averaged in LINEAR light (UE 4.16 TextureCompressorModule.cpp:2030-2086 builds every mip from the RGBA32F `EGammaSpace::Linear` copy of the
/// image), so `filter` only applies to linear textures.
pub fn mip_chain(img: &RgbaImage, srgb: bool, filter: MipFilter) -> Vec<RgbaImage> { if srgb { crate::static_mesh::build_mips_srgb(img, None) } else { build_mips(img, filter) } }
pub fn upload_rgba8(device: &wgpu::Device, queue: &wgpu::Queue, img: &RgbaImage, srgb: bool, filter: MipFilter, label: &str) -> wgpu::Texture {
    upload_mip_chain(device, queue, img, &mip_chain(img, srgb, filter), srgb, label)
}
pub fn upload_mip_chain(device: &wgpu::Device, queue: &wgpu::Queue, img: &RgbaImage, levels: &[RgbaImage], srgb: bool, label: &str) -> wgpu::Texture {
    let format = if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: img.width(), height: img.height(), depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (i, level) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            level.as_raw(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * level.width()), rows_per_image: Some(level.height()) },
            wgpu::Extent3d { width: level.width(), height: level.height(), depth_or_array_layers: 1 },
        );
    }
    texture
}

enum Deferred { Buf(wgpu::Buffer, Vec<u8>, usize), Tex(wgpu::Texture, u32, RgbaImage) }
thread_local! { static DEFERRED: std::cell::RefCell<std::collections::VecDeque<Deferred>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) }; }

pub fn buffer_deferred<T: bytemuck::NoUninit>(device: &wgpu::Device, data: &[T], usage: wgpu::BufferUsages, label: &str) -> wgpu::Buffer {
    let mut bytes = bytemuck::cast_slice::<T, u8>(data).to_vec();
    bytes.resize(bytes.len().next_multiple_of(4).max(4), 0);
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.len() as u64, usage: usage | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    DEFERRED.with(|q| q.borrow_mut().push_back(Deferred::Buf(buf.clone(), bytes, 0)));
    buf
}

pub fn upload_mip_chain_deferred(device: &wgpu::Device, levels: Vec<RgbaImage>, srgb: bool, label: &str) -> wgpu::Texture {
    let format = if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
    let (w, h) = levels.first().map_or((1, 1), |l| (l.width(), l.height()));
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels.len().max(1) as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    DEFERRED.with(|q| { let mut q = q.borrow_mut(); for (i, l) in levels.into_iter().enumerate() { q.push_back(Deferred::Tex(texture.clone(), i as u32, l)); } });
    texture
}

pub fn pump_deferred(queue: &wgpu::Queue, budget_bytes: usize) -> bool {
    DEFERRED.with(|q| {
        let mut q = q.borrow_mut();
        let mut spent = 0usize;
        while spent < budget_bytes {
            let Some(front) = q.front_mut() else { break };
            match front {
                Deferred::Buf(buf, bytes, at) => {
                    let n = (bytes.len() - *at).min((budget_bytes - spent).next_multiple_of(4).max(4 << 20));
                    queue.write_buffer(buf, *at as u64, &bytes[*at..*at + n]);
                    *at += n; spent += n;
                    if *at >= bytes.len() { q.pop_front(); }
                }
                Deferred::Tex(tex, mip, level) => {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: *mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        level.as_raw(),
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * level.width()), rows_per_image: Some(level.height()) },
                        wgpu::Extent3d { width: level.width(), height: level.height(), depth_or_array_layers: 1 },
                    );
                    spent += level.as_raw().len();
                    q.pop_front();
                }
            }
        }
        q.is_empty()
    })
}

pub fn deferred_bytes() -> usize { DEFERRED.with(|q| q.borrow().iter().map(|d| match d { Deferred::Buf(_, b, at) => b.len() - at, Deferred::Tex(_, _, l) => l.as_raw().len() }).sum()) }

/// 1x1 texture of a constant colour (neutral material slots).
pub fn solid_rgba8(device: &wgpu::Device, queue: &wgpu::Queue, px: [u8; 4], label: &str) -> wgpu::Texture {
    upload_rgba8(device, queue, &RgbaImage::from_pixel(1, 1, image::Rgba(px)), false, MipFilter::Box, label)
}

/// Trilinear repeating sampler with anisotropic filtering.
pub fn repeat_sampler(device: &wgpu::Device, anisotropy: u16, label: &str) -> wgpu::Sampler { repeat_sampler_ex(device, anisotropy, 0, label) }
/// As [`repeat_sampler`], additionally never sampling the top `skip_mips` mip levels (Textures quality: minimum LOD clamp).
pub fn repeat_sampler_ex(device: &wgpu::Device, anisotropy: u16, skip_mips: u32, label: &str) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        lod_min_clamp: skip_mips as f32,
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        anisotropy_clamp: anisotropy.max(1),
        ..Default::default()
    })
}

/// Vertex / index / uniform buffer with initial contents.
pub fn buffer_init<T: bytemuck::NoUninit>(device: &wgpu::Device, data: &[T], usage: wgpu::BufferUsages, label: &str) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytemuck::cast_slice(data), usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mip_chain_has_expected_sizes() {
        let img = RgbaImage::from_pixel(8, 4, image::Rgba([10, 20, 30, 255]));
        let m = build_mips(&img, MipFilter::Triangle);
        let sizes: Vec<_> = m.iter().map(|l| (l.width(), l.height())).collect();
        assert_eq!(sizes, vec![(8, 4), (4, 2), (2, 1), (1, 1)]);
        assert_eq!(mip_levels(8, 4), 4);
        assert_eq!(m.last().unwrap().get_pixel(0, 0).0, [10, 20, 30, 255]);
    }
    #[test]
    fn box_filter_averages_2x2_blocks() {
        let mut img = RgbaImage::new(2, 2);
        for (i, p) in img.pixels_mut().enumerate() { *p = image::Rgba([(i as u8) * 40, 0, 0, 255]); }
        let m = build_mips(&img, MipFilter::Box);
        assert_eq!(m[1].get_pixel(0, 0)[0], (0 + 40 + 80 + 120) / 4);
    }
}
