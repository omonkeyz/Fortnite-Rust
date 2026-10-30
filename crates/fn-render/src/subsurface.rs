#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SssProfile { pub scatter_radius: f32, pub subsurface_color: [f32; 3], pub falloff_color: [f32; 3] }
impl Default for SssProfile {
    fn default() -> Self { SssProfile { scatter_radius: 1.2, subsurface_color: [0.48, 0.41, 0.28], falloff_color: [1.0, 0.37, 0.3] } }
}

pub const KERNEL_SIZES: [usize; 3] = [13, 9, 6];
pub const TABLE_WIDTH: usize = 32;
pub const RADIUS_SCALE: f32 = 1024.;
pub const KERNEL_SIZE: f32 = 3.;
pub const MAX_PROFILES: usize = 64;

fn gaussian(variance: f32, r: f32, falloff: [f32; 3]) -> [f32; 3] {
    let mut out = [0.; 3];
    for i in 0..3 {
        let rr = r / (0.001 + falloff[i]);
        out[i] = (-(rr * rr) / (2.0 * variance)).exp() / (2.0 * 3.14 * variance);
    }
    out
}
fn profile(r: f32, falloff: [f32; 3]) -> [f32; 3] {
    let terms = [(0.100f32, 0.0484f32), (0.118, 0.187), (0.113, 0.567), (0.358, 1.99), (0.078, 7.41)];
    let mut out = [0.; 3];
    for (w, v) in terms { let g = gaussian(v, r, falloff); for i in 0..3 { out[i] += w * g[i]; } }
    out
}

pub fn mirrored_kernel(n: usize, falloff: [f32; 3]) -> Vec<[f32; 4]> {
    let total = n * 2 - 1;
    let mut k = vec![[0f32; 4]; total];
    let range = if total > 20 { 3.0f32 } else { 2.0 };
    let exponent = 2.0f32;
    let step = 2.0 * range / (total as f32 - 1.);
    for (i, e) in k.iter_mut().enumerate() {
        let o = -range + i as f32 * step;
        let sign = if o < 0. { -1. } else { 1. };
        e[3] = range * sign * o.powf(exponent).abs() / range.powf(exponent);
    }
    for i in 0..total {
        let w0 = if i > 0 { (k[i][3] - k[i - 1][3]).abs() } else { 0. };
        let w1 = if i < total - 1 { (k[i][3] - k[i + 1][3]).abs() } else { 0. };
        let area = (w0 + w1) / 2.;
        let t = profile(k[i][3], falloff);
        k[i][0] = area * t[0]; k[i][1] = area * t[1]; k[i][2] = area * t[2];
    }
    let t = k[total / 2];
    for i in (1..=total / 2).rev() { k[i] = k[i - 1]; }
    k[0] = t;
    let mut sum = [0f32; 3];
    for e in &k { for c in 0..3 { sum[c] += e[c]; } }
    for e in k.iter_mut() { for c in 0..3 { e[c] /= sum[c]; } }
    let mut out = vec![k[0]];
    for i in 0..n - 1 { out.push(k[n + i]); }
    out
}

fn next_smaller_positive(x: f32) -> f32 { f32::from_bits(x.to_bits() - 1) }

pub fn profile_row(p: &SssProfile) -> [[f32; 4]; TABLE_WIDTH] {
    let bias = 0.009f32;
    let sc = p.subsurface_color.map(|c| c.clamp(0., 1.));
    let fc = p.falloff_color.map(|c| c.clamp(bias, 1.));
    let mut kernel = vec![[sc[0], sc[1], sc[2], 0.]];
    for n in KERNEL_SIZES { kernel.extend(mirrored_kernel(n, fc)); }
    let float_scale = next_smaller_positive(65536.);
    let table_max = [1f32, 1., 1., 3.];
    let mut row = [[0f32; 4]; TABLE_WIDTH];
    for (pos, k) in kernel.iter().enumerate() {
        let mut c = [k[0] / table_max[0], k[1] / table_max[1], k[2] / table_max[2], k[3] / table_max[3]];
        c[3] *= p.scatter_radius / RADIUS_SCALE;
        for ch in 0..4 {
            let q = (c[ch] * float_scale) as u16;
            row[pos][ch] = q as f32 / 65535. * [1., 1., 1., KERNEL_SIZE][ch];
        }
    }
    row
}

fn debug_mode() -> u32 { static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_SSS_DEBUG").ok().and_then(|v| v.parse().ok()).unwrap_or(0)) }

pub fn sample_set_range(sample_set: u32) -> (u32, u32) {
    match sample_set { 0 => (1 + 13 + 9, 6), 1 => (1 + 13, 9), _ => (1, 13) }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct SssU { size: [f32; 4], p0: [f32; 4], p1: [f32; 4], p2: [u32; 4] }

pub struct SssTargets {
    pub width: u32,
    pub height: u32,
    pub half: bool,
    pub inputs: [wgpu::TextureView; 4],
    pub scene: wgpu::TextureView,
    pub ping: wgpu::TextureView,
    pub pong: wgpu::TextureView,
}

pub struct SssFrame<'a> {
    pub depth: &'a wgpu::TextureView,
    pub ao: Option<&'a wgpu::TextureView>,
    pub refl_sky: &'a wgpu::TextureView,
    pub refl_brdf: &'a wgpu::TextureView,
    pub ssr: Option<&'a wgpu::TextureView>,
    pub hdr: &'a wgpu::TextureView,
}

pub struct Subsurface {
    device: wgpu::Device,
    bgl: wgpu::BindGroupLayout,
    compose: wgpu::RenderPipeline,
    setup: wgpu::RenderPipeline,
    blur_x: wgpu::RenderPipeline,
    blur_y: wgpu::RenderPipeline,
    recombine: wgpu::RenderPipeline,
    ubuf: wgpu::Buffer,
    pbuf: wgpu::Buffer,
    dummy: wgpu::TextureView,
    pub targets: Option<SssTargets>,
    pub profile_count: usize,
}

fn target(device: &wgpu::Device, w: u32, h: u32, label: &str) -> wgpu::TextureView {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label), size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
    }).create_view(&Default::default())
}

impl Subsurface {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, hdr: wgpu::TextureFormat) -> Subsurface {
        let f = wgpu::ShaderStages::FRAGMENT;
        let tex = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: f, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: f, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 6, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
        ];
        for b in [2, 3, 4, 5, 7, 8, 9, 10, 11, 12] { entries.push(tex(b)); }
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("subsurface bgl"), entries: &entries });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("subsurface"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("subsurface"), source: wgpu::ShaderSource::Wgsl(fn_live::wgsl!("../shaders/subsurface.wgsl").into()) });
        let pipe = |entry: &str, format: wgpu::TextureFormat, write: wgpu::ColorWrites| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry), layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: write })] }),
            multiview_mask: None, cache: None,
        });
        let r16 = wgpu::TextureFormat::Rgba16Float;
        let compose = pipe("fs_compose", r16, wgpu::ColorWrites::ALL);
        let setup = pipe("fs_setup", r16, wgpu::ColorWrites::ALL);
        let blur_x = pipe("fs_blur_x", r16, wgpu::ColorWrites::ALL);
        let blur_y = pipe("fs_blur_y", r16, wgpu::ColorWrites::ALL);
        let recombine = pipe("fs_recombine", hdr, wgpu::ColorWrites::COLOR);
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("subsurface params"), size: std::mem::size_of::<SssU>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let pbuf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("subsurface profiles"), size: (MAX_PROFILES * TABLE_WIDTH * 16) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let dummy = crate::texture::solid_rgba8(device, queue, [255, 255, 255, 255], "subsurface dummy").create_view(&Default::default());
        let mut s = Subsurface { device: device.clone(), bgl, compose, setup, blur_x, blur_y, recombine, ubuf, pbuf, dummy, targets: None, profile_count: 0 };
        s.set_profiles(queue, &[SssProfile::default()]);
        s
    }
    pub fn set_profiles(&mut self, queue: &wgpu::Queue, profiles: &[SssProfile]) {
        let rows: Vec<[f32; 4]> = profiles.iter().take(MAX_PROFILES).flat_map(|p| profile_row(p)).collect();
        if !rows.is_empty() { queue.write_buffer(&self.pbuf, 0, bytemuck::cast_slice(&rows)); }
        self.profile_count = profiles.len().min(MAX_PROFILES);
    }
    pub fn ensure_targets(&mut self, w: u32, h: u32, half: bool) {
        if self.targets.as_ref().is_some_and(|t| t.width == w && t.height == h && t.half == half) { return; }
        let d = &self.device;
        let (hw, hh) = if half { (w.div_ceil(2), h.div_ceil(2)) } else { (w, h) };
        self.targets = Some(SssTargets {
            width: w, height: h, half,
            inputs: [target(d, w, h, "sss lit"), target(d, w, h, "sss sky"), target(d, w, h, "sss base"), target(d, w, h, "sss fog")],
            scene: target(d, w, h, "sss scene"), ping: target(d, hw, hh, "sss setup / blur y"), pong: target(d, hw, hh, "sss blur x"),
        });
    }
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(&self, queue: &wgpu::Queue, tan_half_x: f32, scale: f32, sample_set: u32, near_m: f32, far_m: f32, ssr: bool, ao: bool) {
        let Some(t) = self.targets.as_ref() else { return };
        let (hw, hh) = if t.half { (t.width.div_ceil(2), t.height.div_ceil(2)) } else { (t.width, t.height) };
        let z = scale.max(0.) / tan_half_x.max(1e-6);
        let x = z / KERNEL_SIZE * 0.5;
        let (off, count) = sample_set_range(sample_set);
        let u = SssU {
            size: [t.width as f32, t.height as f32, hw as f32, hh as f32],
            p0: [x, z, t.width as f32 / t.height.max(1) as f32, if t.half { 1. } else { 0. }],
            p1: [near_m, far_m, if ssr { 1. } else { 0. }, if ao { 1. } else { 0. }],
            p2: [off, count, if scale > 0. { 1 } else { 0 }, debug_mode()],
        };
        queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(&u));
    }
    fn bind(&self, f: &SssFrame, scene: Option<&wgpu::TextureView>, half: Option<&wgpu::TextureView>) -> wgpu::BindGroup {
        let t = self.targets.as_ref().unwrap();
        fn tv(b: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> { wgpu::BindGroupEntry { binding: b, resource: wgpu::BindingResource::TextureView(v) } }
        let d = &self.dummy;
        let entries = [
            wgpu::BindGroupEntry { binding: 0, resource: self.ubuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: self.pbuf.as_entire_binding() },
            tv(2, &t.inputs[0]), tv(3, &t.inputs[1]), tv(4, &t.inputs[2]), tv(5, &t.inputs[3]), tv(6, f.depth),
            tv(7, f.ao.unwrap_or(d)), tv(8, f.refl_sky), tv(9, f.refl_brdf), tv(10, f.ssr.unwrap_or(d)), tv(11, scene.unwrap_or(d)), tv(12, half.unwrap_or(d)),
        ];
        self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("subsurface bg"), layout: &self.bgl, entries: &entries })
    }
    pub fn input_attachments(&self) -> Option<[Option<wgpu::RenderPassColorAttachment<'_>>; 4]> {
        let t = self.targets.as_ref()?;
        let a = |v| Some(wgpu::RenderPassColorAttachment { view: v, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } });
        Some([a(&t.inputs[0]), a(&t.inputs[1]), a(&t.inputs[2]), a(&t.inputs[3])])
    }
    pub fn encode(&self, enc: &mut wgpu::CommandEncoder, f: &SssFrame) {
        let Some(t) = self.targets.as_ref() else { return };
        let mut pass = |label: &str, view: &wgpu::TextureView, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup, load: wgpu::LoadOp<wgpu::Color>| {
            let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            p.set_pipeline(pipe);
            p.set_bind_group(0, bg, &[]);
            p.draw(0..3, 0..1);
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);
        let bg = self.bind(f, None, None);
        pass("sss compose", &t.scene, &self.compose, &bg, clear);
        let bg = self.bind(f, Some(&t.scene), None);
        pass("sss setup", &t.ping, &self.setup, &bg, clear);
        let bg = self.bind(f, Some(&t.scene), Some(&t.ping));
        pass("sss blur x", &t.pong, &self.blur_x, &bg, clear);
        let bg = self.bind(f, Some(&t.scene), Some(&t.pong));
        pass("sss blur y", &t.ping, &self.blur_y, &bg, clear);
        let bg = self.bind(f, Some(&t.scene), Some(&t.ping));
        pass("sss recombine", f.hdr, &self.recombine, &bg, wgpu::LoadOp::Load);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn kernel_is_normalized_and_mirrored() {
        for n in KERNEL_SIZES {
            let k = mirrored_kernel(n, [1.0, 0.565, 0.471]);
            assert_eq!(k.len(), n);
            assert_eq!(k[0][3], 0.);
            for c in 0..3 {
                let sum = k[0][c] + 2. * k[1..].iter().map(|e| e[c]).sum::<f32>();
                assert!((sum - 1.).abs() < 1e-4, "n {n} channel {c}: {sum}");
            }
            for w in k.windows(2).skip(1) { assert!(w[1][3] > w[0][3]); }
            let range = if 2 * n - 1 > 20 { 3. } else { 2. };
            assert!((k[n - 1][3] - range).abs() < 1e-5);
        }
    }
    #[test] fn row_layout_matches_the_profile_texture() {
        let p = SssProfile { scatter_radius: 2.5, subsurface_color: [0.6, 0.513726, 0.34902], falloff_color: [1.0, 0.564706, 0.470588] };
        let r = profile_row(&p);
        assert!((r[0][0] - 0.6).abs() < 1e-4 && r[0][3] == 0.);
        let k = mirrored_kernel(13, p.falloff_color);
        assert!((r[13][3] - ((k[12][3] / 3. * 2.5 / 1024. * 65535.996) as u16) as f32 / 65535. * 3.).abs() < 1e-7, "{} vs {}", r[13][3], k[12][3] * 2.5 / 1024.);
        assert!((r[1][0] - k[0][0]).abs() < 2e-5);
        assert_eq!(sample_set_range(2), (1, 13));
        assert_eq!(sample_set_range(0), (23, 6));
    }
    #[test] fn pipelines_create() {
        let Ok(g) = crate::gpu::Gpu::headless() else { eprintln!("no GPU adapter: skipped"); return };
        let mut s = Subsurface::new(&g.device, &g.queue, crate::renderer::HDR_FORMAT);
        s.ensure_targets(64, 48, true);
        s.prepare(&g.queue, 0.8, 1., 2, 0.1, 3000., false, false);
        g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
    fn run_case(half: bool, a: [f64; 4], b: [f64; 4], f: [f64; 4]) -> Option<Vec<[f32; 4]>> {
        let g = crate::gpu::Gpu::headless().ok()?;
        let (w, h) = (32u32, 8u32);
        let mut s = Subsurface::new(&g.device, &g.queue, crate::renderer::HDR_FORMAT);
        s.set_profiles(&g.queue, &[SssProfile::default(), SssProfile { scatter_radius: 2.5, subsurface_color: [0.6, 0.513726, 0.34902], falloff_color: [1.0, 0.564706, 0.470588] }]);
        s.ensure_targets(w, h, half);
        s.prepare(&g.queue, 0.8, 1., 2, 0.1, 3000., false, false);
        let tex = |format, usage, label| g.device.create_texture(&wgpu::TextureDescriptor { label: Some(label), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
        let depth = tex(wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, "depth");
        let hdr = tex(crate::renderer::HDR_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, "hdr");
        let zero = tex(wgpu::TextureFormat::Rgba16Float, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, "zero");
        let (dv, hv, zv) = (depth.create_view(&Default::default()), hdr.create_view(&Default::default()), zero.create_view(&Default::default()));
        let mut enc = g.device.create_command_encoder(&Default::default());
        {
            let t = s.targets.as_ref().unwrap();
            let col = |c: [f64; 4]| wgpu::Color { r: c[0], g: c[1], b: c[2], a: c[3] };
            let att = |v, c| Some(wgpu::RenderPassColorAttachment { view: v, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(c), store: wgpu::StoreOp::Store } });
            let _p = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &[att(&t.inputs[0], col(a)), att(&t.inputs[1], col([0., 0., 0., 1.])), att(&t.inputs[2], col(b)), att(&t.inputs[3], col(f))],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &dv, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.99), store: wgpu::StoreOp::Store }), stencil_ops: None }), timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        }
        {
            let _p = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &hv, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.7, g: 0.7, b: 0.7, a: 1. }), store: wgpu::StoreOp::Store } }), Some(wgpu::RenderPassColorAttachment { view: &zv, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        }
        s.encode(&mut enc, &SssFrame { depth: &dv, ao: None, refl_sky: &zv, refl_brdf: &zv, ssr: None, hdr: &hv });
        let buf = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (w * h * 8) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_texture_to_buffer(wgpu::TexelCopyTextureInfo { texture: &hdr, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) } }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
        g.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let raw: Vec<u16> = bytemuck::cast_slice(&slice.get_mapped_range().unwrap()).to_vec();
        Some(raw.chunks_exact(4).map(|p| [fn_ueio_f16(p[0]), fn_ueio_f16(p[1]), fn_ueio_f16(p[2]), fn_ueio_f16(p[3])]).collect())
    }
    fn fn_ueio_f16(h: u16) -> f32 {
        let s = if h & 0x8000 != 0 { -1. } else { 1. };
        let e = ((h >> 10) & 0x1f) as i32;
        let m = (h & 0x3ff) as f32;
        if e == 0 { s * m * 2f32.powi(-24) } else { s * (1. + m / 1024.) * 2f32.powi(e - 15) }
    }
    fn close(a: [f32; 4], b: [f32; 3]) -> bool { (0..3).all(|i| (a[i] - b[i]).abs() < 4e-3) }
    #[test] fn flat_subsurface_input_recombines_to_diffuse_times_base_plus_specular() {
        let c = [0.5, 0.4, 0.3];
        let luma = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
        let base = [0.8, 0.6, 0.4];
        for half in [true, false] {
            let Some(px) = run_case(half, [c[0], c[1], c[2], luma], [base[0], base[1], base[2], 2.5], [0., 0., 0., 1.]) else { eprintln!("no GPU adapter: skipped"); return };
            let want = [(c[0] * base[0]) as f32, (c[1] * base[1]) as f32, (c[2] * base[2]) as f32];
            assert!(px.iter().all(|p| close(*p, want)), "half {half}: {:?} vs {want:?}", px[0]);
            let px = run_case(half, [c[0], c[1], c[2], luma * 0.5], [base[0], base[1], base[2], 2.5], [0.1, 0.2, 0.3, 0.5]).unwrap();
            let want = [((c[0] * (0.5 * base[0] + 0.5)) * 0.5 + 0.1) as f32, ((c[1] * (0.5 * base[1] + 0.5)) * 0.5 + 0.2) as f32, ((c[2] * (0.5 * base[2] + 0.5)) * 0.5 + 0.3) as f32];
            assert!(px.iter().all(|p| close(*p, want)), "half {half} split + fog: {:?} vs {want:?}", px[0]);
            let px = run_case(half, [c[0], c[1], c[2], luma], [base[0], base[1], base[2], 0.], [0., 0., 0., 1.]).unwrap();
            assert!(px.iter().all(|p| close(*p, [0.7, 0.7, 0.7])), "half {half}: non-profile pixels must stay untouched: {:?}", px[0]);
        }
    }
}

