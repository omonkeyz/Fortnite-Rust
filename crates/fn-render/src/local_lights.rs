use glam::{Mat4, Vec3, Vec4};

pub const GRID_PIXEL_SIZE: u32 = 64;
pub const GRID_PIXEL_SHIFT: u32 = 6;
pub const GRID_SIZE_Z: u32 = 32;
pub const MIN_SCREEN_RADIUS_FOR_LIGHTS: f32 = 0.03;
pub const LOCAL_LIGHT_MIN_ROUGHNESS: f32 = 0.08;
pub const MAX_GPU_LIGHTS: usize = 4096;
pub const GRID_HEADER: usize = 8;
pub const GRID_CAPACITY: usize = 1 << 21;
pub const IES_WIDTH: u32 = 256;

pub const BINDING_LIGHTS: u32 = 23;
pub const BINDING_GRID: u32 = 24;
pub const BINDING_IES: u32 = 25;

#[derive(Clone, Debug, PartialEq)]
pub struct LocalLight {
    pub position_m: Vec3,
    pub forward: Vec3,
    pub color: [f32; 3],
    pub attenuation_radius_cm: f32,
    pub falloff_exponent: f32,
    pub inverse_squared: bool,
    pub spot_cone_deg: Option<(f32, f32)>,
    pub source_radius_cm: f32,
    pub source_length_cm: f32,
    pub ies_row: Option<u32>,
    pub cast_shadows: bool,
    pub max_draw_distance_cm: f32,
    pub fade_range_cm: f32,
    pub min_roughness: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LocalLightGpu {
    pub pos_inv_radius: [f32; 4],
    pub color_falloff: [f32; 4],
    pub dir_min_rough: [f32; 4],
    pub spot_source: [f32; 4],
    pub flags: [f32; 4],
}

pub fn spot_angles(inner_deg: f32, outer_deg: f32) -> (f32, f32, f32) {
    let inner = inner_deg.clamp(0., 89.).to_radians();
    let outer = (outer_deg.to_radians()).clamp(inner + 0.001, 89f32.to_radians() + 0.001);
    let (co, ci) = (outer.cos(), inner.cos());
    (co, 1. / (ci - co), outer)
}

impl LocalLight {
    pub fn spot_params(&self) -> (f32, f32) {
        match self.spot_cone_deg { Some((i, o)) => { let (c, d, _) = spot_angles(i, o); (c, d) } None => (-2., 1.) }
    }
    pub fn bounding_sphere_cm(&self) -> (Vec3, f32) {
        let p = self.position_m * 100.;
        let r = self.attenuation_radius_cm;
        match self.spot_cone_deg {
            Some((i, o)) => {
                let (cos_outer, _, _) = spot_angles(i, o);
                (p + 0.5 * self.forward.normalize_or_zero() * r, (1.25 * r * r - r * r * cos_outer).max(0.).sqrt())
            }
            None => (p, r),
        }
    }
    pub fn gpu(&self, fade: f32) -> LocalLightGpu {
        let p = self.position_m * 100.;
        let (sa, sb) = self.spot_params();
        let k = if self.inverse_squared { 16. } else { 1. } * fade;
        let d = -self.forward.normalize_or_zero();
        LocalLightGpu {
            pos_inv_radius: [p.x, p.y, p.z, 1. / self.attenuation_radius_cm.max(1e-4)],
            color_falloff: [self.color[0] * k, self.color[1] * k, self.color[2] * k, self.falloff_exponent],
            dir_min_rough: [d.x, d.y, d.z, self.min_roughness.max(0.04)],
            spot_source: [sa, sb, self.source_radius_cm, self.source_length_cm],
            flags: [if self.inverse_squared { 1. } else { 0. }, self.ies_row.map_or(-1., |r| r as f32), if self.cast_shadows { 1. } else { 0. }, 0.],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LightView {
    pub eye_m: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub tan_x: f32,
    pub tan_y: f32,
    pub near_cm: f32,
    pub width: u32,
    pub height: u32,
    pub view_proj: Mat4,
}

pub fn light_view(cam: &crate::camera::CameraParams) -> LightView {
    let f = cam.forward.normalize();
    let r = f.cross(cam.up).normalize();
    let u = r.cross(f);
    let (tx, ty) = cam.tan_half();
    LightView { eye_m: cam.eye, forward: f, right: r, up: u, tan_x: tx, tan_y: ty, near_cm: cam.near * 100., width: cam.width.max(1), height: cam.height.max(1), view_proj: cam.proj_matrix() * cam.view_matrix() }
}

pub fn in_frustum(v: &LightView, center_cm: Vec3, radius_cm: f32) -> bool {
    let rel = center_cm - v.eye_m * 100.;
    let z = rel.dot(v.forward);
    let x = rel.dot(v.right);
    let y = rel.dot(v.up);
    if z < v.near_cm - radius_cm { return false; }
    let side = |c: f32, t: f32| { let n = 1. / (1. + t * t).sqrt(); (c - t * z) * n <= radius_cm && (-c - t * z) * n <= radius_cm };
    side(x, v.tan_x) && side(y, v.tan_y)
}

pub fn light_fade_factor(distance_cm: f32, bounds_radius_cm: f32, max_draw_distance_cm: f32, fade_range_cm: f32, max_draw_distance_scale: f32, lod_distance_factor: f32) -> f32 {
    let s = (0.0002f32.min(MIN_SCREEN_RADIUS_FOR_LIGHTS / bounds_radius_cm.max(1e-6)) * lod_distance_factor).powi(2) * distance_cm * distance_cm;
    let size_fade = (6. - 6. * s).clamp(0., 1.);
    let max_dist = max_draw_distance_cm * max_draw_distance_scale;
    let distance_fade = if max_dist != 0. { ((max_dist - distance_cm) / fade_range_cm).clamp(0., 1.) } else { 1. };
    size_fade * distance_fade
}

pub fn light_drawn(distance_cm: f32, bounds_radius_cm: f32, max_draw_distance_cm: f32, max_draw_distance_scale: f32, lod_distance_factor: f32) -> bool {
    let d2 = distance_cm * distance_cm;
    let max2 = (max_draw_distance_cm * max_draw_distance_scale).powi(2);
    (0.0002f32.min(MIN_SCREEN_RADIUS_FOR_LIGHTS / bounds_radius_cm.max(1e-6)) * lod_distance_factor).powi(2) * d2 < 1. && (max2 == 0. || d2 < max2)
}

pub fn grid_z_params(near_cm: f32, far_cm: f32) -> [f32; 3] {
    let near_offset = 0.095 * 100.;
    let s = 4.05f64;
    let n = near_cm as f64 + near_offset;
    let f = far_cm as f64;
    let o = (f - n * ((GRID_SIZE_Z as f64 - 1.) / s).exp2()) / (f - n);
    let b = (1. - o) / n;
    [b as f32, o as f32, s as f32]
}

pub fn grid_slice(depth_cm: f32, z: [f32; 3]) -> u32 {
    let s = ((depth_cm * z[0] + z[1]).log2() * z[2]).max(0.);
    if !s.is_finite() { return 0; }
    (s as u32).min(GRID_SIZE_Z - 1)
}

#[derive(Default)]
pub struct GridBuild {
    pub words: Vec<u32>,
    pub lights: Vec<LocalLightGpu>,
    pub dims: [u32; 3],
    pub visible: usize,
    pub overflow: bool,
}

pub fn build_grid(v: &LightView, lights: &[LocalLight], max_draw_distance_scale: f32, lod_distance_factor: f32) -> GridBuild {
    let nx = v.width.div_ceil(GRID_PIXEL_SIZE);
    let ny = v.height.div_ceil(GRID_PIXEL_SIZE);
    let cells = (nx * ny * GRID_SIZE_Z) as usize;
    let eye_cm = v.eye_m * 100.;
    let mut vis: Vec<(LocalLightGpu, Vec3, f32)> = vec![];
    let mut furthest = 0f32;
    for l in lights {
        if vis.len() >= MAX_GPU_LIGHTS { break; }
        if l.attenuation_radius_cm <= 0. { continue; }
        if l.color.iter().all(|c| *c <= 0.) { continue; }
        if !in_frustum(v, l.position_m * 100., l.attenuation_radius_cm) { continue; }
        let (bc, br) = l.bounding_sphere_cm();
        let dist = (bc - eye_cm).length();
        if !light_drawn(dist, br, l.max_draw_distance_cm, max_draw_distance_scale, lod_distance_factor) { continue; }
        let fade = light_fade_factor(dist, br, l.max_draw_distance_cm, l.fade_range_cm, max_draw_distance_scale, lod_distance_factor);
        if fade <= 0. { continue; }
        let p = l.position_m * 100.;
        let r = l.attenuation_radius_cm;
        furthest = furthest.max((p - eye_cm).length() + r);
        vis.push((l.gpu(fade), p, r));
    }
    let z = grid_z_params(v.near_cm, furthest.max(v.near_cm + 100.) + 10.);
    let mut ranges: Vec<[u32; 6]> = Vec::with_capacity(vis.len());
    for (_, p, r) in &vis {
        let rel = *p - eye_cm;
        let (cx, cy, cz) = (rel.dot(v.right), rel.dot(v.up), rel.dot(v.forward));
        let z0 = (cz - r).max(v.near_cm);
        let z1 = cz + r;
        let (s0, s1) = (grid_slice(z0, z), grid_slice(z1, z));
        let (mut x0, mut x1, mut y0, mut y1) = (0u32, nx - 1, 0u32, ny - 1);
        if cz - r > v.near_cm {
            let mut lo = Vec4::splat(f32::INFINITY);
            let mut hi = Vec4::splat(f32::NEG_INFINITY);
            for k in 0..8 {
                let dx = if k & 1 == 0 { -r } else { *r };
                let dy = if k & 2 == 0 { -r } else { *r };
                let dz = if k & 4 == 0 { -r } else { *r };
                let zz = cz + dz;
                let px = (cx + dx) / (zz * v.tan_x);
                let py = (cy + dy) / (zz * v.tan_y);
                lo = lo.min(Vec4::new(px, py, 0., 0.));
                hi = hi.max(Vec4::new(px, py, 0., 0.));
            }
            let to_px = |n: f32, size: u32| ((n * 0.5 + 0.5) * size as f32).floor();
            let fx0 = to_px(lo.x, v.width).max(0.);
            let fx1 = to_px(hi.x, v.width).min(v.width as f32 - 1.);
            let fy0 = ((0.5 - hi.y * 0.5) * v.height as f32).floor().max(0.);
            let fy1 = ((0.5 - lo.y * 0.5) * v.height as f32).floor().min(v.height as f32 - 1.);
            if fx1 < 0. || fy1 < 0. || fx0 > v.width as f32 - 1. || fy0 > v.height as f32 - 1. || fx0 > fx1 || fy0 > fy1 { ranges.push([1, 0, 1, 0, 1, 0]); continue; }
            x0 = (fx0 as u32) >> GRID_PIXEL_SHIFT; x1 = ((fx1 as u32) >> GRID_PIXEL_SHIFT).min(nx - 1);
            y0 = (fy0 as u32) >> GRID_PIXEL_SHIFT; y1 = ((fy1 as u32) >> GRID_PIXEL_SHIFT).min(ny - 1);
        }
        ranges.push([x0, x1, y0, y1, s0, s1]);
    }
    let mut counts = vec![0u32; cells];
    for rg in &ranges {
        if rg[0] > rg[1] { continue; }
        for zs in rg[4]..=rg[5] { for y in rg[2]..=rg[3] { for x in rg[0]..=rg[1] { counts[((zs * ny + y) * nx + x) as usize] += 1; } } }
    }
    let total: usize = counts.iter().map(|c| *c as usize).sum();
    let base = GRID_HEADER + cells * 2;
    let mut out = GridBuild { dims: [nx, ny, GRID_SIZE_Z], visible: vis.len(), ..Default::default() };
    if base + total > GRID_CAPACITY {
        out.overflow = true;
        out.words = vec![nx, ny, GRID_SIZE_Z, GRID_PIXEL_SHIFT, z[0].to_bits(), z[1].to_bits(), z[2].to_bits(), 0];
        out.words.resize(base, 0);
        return out;
    }
    let mut words = vec![0u32; base + total];
    words[..GRID_HEADER].copy_from_slice(&[nx, ny, GRID_SIZE_Z, GRID_PIXEL_SHIFT, z[0].to_bits(), z[1].to_bits(), z[2].to_bits(), vis.len() as u32]);
    let mut at = base as u32;
    for (c, n) in counts.iter().enumerate() { words[GRID_HEADER + 2 * c] = at; words[GRID_HEADER + 2 * c + 1] = 0; at += n; }
    for (li, rg) in ranges.iter().enumerate() {
        if rg[0] > rg[1] { continue; }
        for zs in rg[4]..=rg[5] { for y in rg[2]..=rg[3] { for x in rg[0]..=rg[1] {
            let c = ((zs * ny + y) * nx + x) as usize;
            let slot = words[GRID_HEADER + 2 * c] + words[GRID_HEADER + 2 * c + 1];
            words[slot as usize] = li as u32;
            words[GRID_HEADER + 2 * c + 1] += 1;
        } } }
    }
    out.words = words;
    out.lights = vis.into_iter().map(|v| v.0).collect();
    out
}

pub fn globals_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let f = wgpu::ShaderStages::FRAGMENT;
    let storage = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: f, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None };
    vec![
        storage(BINDING_LIGHTS),
        storage(BINDING_GRID),
        wgpu::BindGroupLayoutEntry { binding: BINDING_IES, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
    ]
}

pub struct LocalLightsGpu {
    pub lights_buf: wgpu::Buffer,
    pub grid_buf: wgpu::Buffer,
    pub ies_view: wgpu::TextureView,
    pub ies_rows: u32,
    pub list: Vec<LocalLight>,
    pub last_visible: usize,
    pub last_overflow: bool,
    pub last_dims: [u32; 3],
    sent_lights: Vec<u8>,
    sent_words: Vec<u32>,
}

fn ies_texture(device: &wgpu::Device, queue: &wgpu::Queue, rows: &[Vec<f32>]) -> wgpu::TextureView {
    let h = rows.len().max(1) as u32;
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("IES light profiles"), size: wgpu::Extent3d { width: IES_WIDTH, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba16Float, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
    });
    let mut data: Vec<u16> = Vec::with_capacity((IES_WIDTH * h * 4) as usize);
    for y in 0..h as usize {
        for x in 0..IES_WIDTH as usize {
            let v = rows.get(y).and_then(|r| r.get(x)).copied().unwrap_or(1.);
            let hv = crate::sky_reflection::f32_to_f16(v);
            data.extend_from_slice(&[hv, hv, hv, crate::sky_reflection::f32_to_f16(1.)]);
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        bytemuck::cast_slice(&data),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(IES_WIDTH * 8), rows_per_image: Some(h) },
        wgpu::Extent3d { width: IES_WIDTH, height: h, depth_or_array_layers: 1 },
    );
    tex.create_view(&Default::default())
}

impl LocalLightsGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let lights_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("local lights"), size: (MAX_GPU_LIGHTS * std::mem::size_of::<LocalLightGpu>()) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let grid_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("local light grid"), size: (GRID_CAPACITY * 4) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        queue.write_buffer(&grid_buf, 0, bytemuck::cast_slice(&[1u32, 1, GRID_SIZE_Z, GRID_PIXEL_SHIFT, 0, 0, 0, 0]));
        LocalLightsGpu { lights_buf, grid_buf, ies_view: ies_texture(device, queue, &[]), ies_rows: 0, list: vec![], last_visible: 0, last_overflow: false, last_dims: [0; 3], sent_lights: vec![], sent_words: vec![] }
    }
    pub fn set_ies_profiles(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, rows: &[Vec<f32>]) {
        self.ies_view = ies_texture(device, queue, rows);
        self.ies_rows = rows.len() as u32;
    }
    pub fn bind_entries(&self) -> Vec<wgpu::BindGroupEntry<'_>> {
        vec![
            wgpu::BindGroupEntry { binding: BINDING_LIGHTS, resource: self.lights_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: BINDING_GRID, resource: self.grid_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: BINDING_IES, resource: wgpu::BindingResource::TextureView(&self.ies_view) },
        ]
    }
    pub fn prepare(&mut self, queue: &wgpu::Queue, cam: &crate::camera::CameraParams, max_draw_distance_scale: f32, lod_distance_factor: f32) {
        let v = light_view(cam);
        let g = build_grid(&v, &self.list, max_draw_distance_scale, lod_distance_factor);
        if g.overflow && !self.last_overflow { eprintln!("[local-lights] light grid overflow: {} visible lights need more than {} index words; local lights skipped this frame", g.visible, GRID_CAPACITY); }
        self.last_overflow = g.overflow;
        self.last_visible = g.visible;
        self.last_dims = g.dims;
        let lb: &[u8] = bytemuck::cast_slice(&g.lights);
        if !lb.is_empty() && self.sent_lights.as_slice() != lb { queue.write_buffer(&self.lights_buf, 0, lb); self.sent_lights.clear(); self.sent_lights.extend_from_slice(lb); }
        if self.sent_words != g.words { queue.write_buffer(&self.grid_buf, 0, bytemuck::cast_slice(&g.words)); self.sent_words = g.words; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> LightView {
        let cam = crate::camera::CameraParams::from_yaw_pitch(Vec3::ZERO, 0., 0., 1280, 720);
        light_view(&cam)
    }
    fn point(p: Vec3, r: f32) -> LocalLight {
        LocalLight { position_m: p, forward: Vec3::X, color: [1., 1., 1.], attenuation_radius_cm: r, falloff_exponent: 8., inverse_squared: true, spot_cone_deg: None, source_radius_cm: 0., source_length_cm: 0., ies_row: None, cast_shadows: true, max_draw_distance_cm: 0., fade_range_cm: 0., min_roughness: LOCAL_LIGHT_MIN_ROUGHNESS }
    }
    #[test] fn spot_clamps_like_spot_light_component() {
        let (c, d, o) = spot_angles(2.449541, 48.110115);
        assert!((o - 48.110115f32.to_radians()).abs() < 1e-6);
        assert!((c - 48.110115f32.to_radians().cos()).abs() < 1e-6);
        assert!((d - 1. / (2.449541f32.to_radians().cos() - c)).abs() < 1e-3);
        let (_, _, o2) = spot_angles(30., 10.);
        assert!((o2 - (30f32.to_radians() + 0.001)).abs() < 1e-6);
    }
    #[test] fn spot_bounds_are_the_cone_sphere() {
        let mut l = point(Vec3::ZERO, 1000.);
        l.spot_cone_deg = Some((0., 44.));
        let (c, r) = l.bounding_sphere_cm();
        assert!((c - Vec3::new(500., 0., 0.)).length() < 1e-3);
        assert!((r - (1.25f32 * 1e6 - 1e6 * 44f32.to_radians().cos()).sqrt()).abs() < 1e-2);
    }
    #[test] fn fade_matches_get_light_fade_factor() {
        assert_eq!(light_fade_factor(1000., 300., 7652.82, 2048., 1., 1.), 1.);
        let d = 7652.82 - 1024.;
        assert!((light_fade_factor(d, 300., 7652.82, 2048., 1., 1.) - 0.5).abs() < 1e-4);
        assert_eq!(light_fade_factor(8000., 300., 7652.82, 2048., 1., 1.), 0.);
        let k = 0.0002f32.min(0.03 / 1600.);
        let near_size = (5f32 / 6.).sqrt() / k;
        assert!((light_fade_factor(near_size, 1600., 0., 0., 1., 1.) - 1.).abs() < 1e-3);
        assert!((light_fade_factor(near_size * 1.04, 1600., 0., 0., 1., 1.) - (6. - 6. * (1.04f32 * 1.04 * 5. / 6.))).abs() < 1e-3);
        assert!(!light_drawn(1. / k + 1., 1600., 0., 1., 1.));
        assert!(light_drawn(1. / k - 1., 1600., 0., 1., 1.));
        assert!(!light_drawn(1. / 0.0002 + 1., 100., 0., 1., 1.));
    }
    #[test] fn inverse_squared_lights_get_x16() {
        let l = point(Vec3::new(0., 0., -5.), 300.);
        assert_eq!(l.gpu(1.).color_falloff[0], 16.);
        let mut l2 = l.clone();
        l2.inverse_squared = false;
        assert_eq!(l2.gpu(0.5).color_falloff[0], 0.5);
        assert_eq!(l.gpu(1.).spot_source[0], -2.);
    }
    #[test] fn z_params_put_far_in_the_last_slice() {
        let z = grid_z_params(10., 10000.);
        assert_eq!(grid_slice(10000., z), GRID_SIZE_Z - 1);
        assert_eq!(grid_slice(10., z), 0);
        let mut last = 0;
        for d in (20..10000).step_by(37) { let s = grid_slice(d as f32, z); assert!(s >= last); last = s; }
    }
    #[test] fn every_pixel_cell_lists_every_light_that_reaches_it() {
        let v = view();
        let lights: Vec<LocalLight> = (0..40).map(|i| point(Vec3::new((i % 8) as f32 * 3. - 12., (i / 8) as f32 * 2. - 4., -5. - i as f32), 300. + 50. * (i % 5) as f32)).collect();
        let g = build_grid(&v, &lights, 1., 1.);
        assert!(!g.overflow);
        let z = [f32::from_bits(g.words[4]), f32::from_bits(g.words[5]), f32::from_bits(g.words[6])];
        let (nx, ny) = (g.dims[0], g.dims[1]);
        for py in (0..720).step_by(23) { for px in (0..1280).step_by(29) {
            for depth_m in [0.5f32, 2., 5., 9., 17., 30., 60.] {
                let ndc_x = (px as f32 + 0.5) / 1280. * 2. - 1.;
                let ndc_y = 1. - (py as f32 + 0.5) / 720. * 2.;
                let world = v.eye_m + v.forward * depth_m + v.right * ndc_x * v.tan_x * depth_m + v.up * ndc_y * v.tan_y * depth_m;
                let s = grid_slice(depth_m * 100., z);
                let c = ((s * ny + (py >> GRID_PIXEL_SHIFT)) * nx + (px >> GRID_PIXEL_SHIFT)) as usize;
                let (off, n) = (g.words[GRID_HEADER + 2 * c] as usize, g.words[GRID_HEADER + 2 * c + 1] as usize);
                let listed: Vec<u32> = g.words[off..off + n].to_vec();
                for (li, gl) in g.lights.iter().enumerate() {
                    let p = Vec3::new(gl.pos_inv_radius[0], gl.pos_inv_radius[1], gl.pos_inv_radius[2]) / 100.;
                    if (p - world).length() * 100. < 1. / gl.pos_inv_radius[3] { assert!(listed.contains(&(li as u32)), "pixel {px},{py} depth {depth_m} misses light {li}"); }
                }
            }
        } }
    }
    struct RefCase { light: LocalLight, p: Vec3, n: Vec3, v: Vec3, diffuse: Vec3, spec: Vec3, rough: f32, ies: Option<Vec<f32>> }
    fn sat(x: f64) -> f64 { x.clamp(0., 1.) }
    fn ue_reference(c: &RefCase) -> [f64; 3] {
        let g = c.light.gpu(1.);
        let d3 = |a: [f32; 4]| [a[0] as f64, a[1] as f64, a[2] as f64];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let add = |a: [f64; 3], b: [f64; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        let mul = |a: [f64; 3], k: f64| [a[0] * k, a[1] * k, a[2] * k];
        let norm = |a: [f64; 3]| { let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(); [a[0] / l, a[1] / l, a[2] / l] };
        let lp = d3(g.pos_inv_radius);
        let inv_r = g.pos_inv_radius[3] as f64;
        let p = [c.p.x as f64 * 100., c.p.y as f64 * 100., c.p.z as f64 * 100.];
        let n = [c.n.x as f64, c.n.y as f64, c.n.z as f64];
        let v = [c.v.x as f64, c.v.y as f64, c.v.z as f64];
        let ldir = d3(g.dir_min_rough);
        let (src_r, src_len) = (g.spot_source[2] as f64, g.spot_source[3] as f64);
        let mut to_light = sub(lp, p);
        let d2 = dot(to_light, to_light);
        let mut l = mul(to_light, 1. / d2.sqrt());
        let (dist_att, mut nol, mask);
        if c.light.inverse_squared {
            if src_len > 0. {
                let l01 = mul(ldir, src_len);
                let t0 = sub(to_light, mul(l01, 0.5));
                let t1 = add(to_light, mul(l01, 0.5));
                let (a0, a1) = (dot(t0, t0).sqrt(), dot(t1, t1).sqrt());
                dist_att = 1. / ((a0 * a1 + dot(t0, t1)) * 0.5 + 1.);
                nol = sat(0.5 * (dot(n, t0) / a0 + dot(n, t1) / a1));
            } else {
                dist_att = 1. / (d2 + 1.);
                nol = sat(dot(n, l));
                if src_r > 0. {
                    nol = dot(n, l);
                    let sin_a2 = sat(src_r * src_r / d2);
                    let sin_a = sin_a2.sqrt();
                    if nol < sin_a { nol = nol.max(-sin_a); nol = (sin_a + nol).powi(2) / (4. * sin_a); }
                }
            }
            mask = sat(1. - (d2 * inv_r * inv_r).powi(2)).powi(2);
        } else {
            dist_att = 1.;
            nol = sat(dot(n, l));
            let w = mul(to_light, inv_r);
            mask = (1. - sat(dot(w, w))).powf(g.color_falloff[3] as f64);
        }
        let spot = sat((dot(l, ldir) - g.spot_source[0] as f64) * g.spot_source[1] as f64).powi(2);
        if !(mask > 0. && spot > 0.) { return [0.; 3]; }
        let atten = dist_att * mask * spot;
        let lr = (c.rough as f64).max(g.dir_min_rough[3] as f64);
        let m = lr * lr;
        let mut energy = 1.;
        let mut r = sub(mul(n, 2. * dot(v, n)), v);
        let inv_dist = 1. / dot(to_light, to_light).sqrt();
        let k = (1. - m) * ((1. - m).sqrt() + m);
        r = norm(add(mul(n, 1. - k), mul(r, k)));
        if src_len > 0. {
            let la = sat(src_len * inv_dist);
            energy *= m / sat(m + 0.5 * la);
            let l01 = mul(ldir, src_len);
            let l0 = sub(to_light, mul(l01, 0.5));
            let a = src_len * src_len;
            let b = dot(r, l01);
            let t = sat(dot(l0, sub(mul(r, b), l01)) / (a - b * b));
            to_light = add(l0, mul(l01, t));
        }
        if src_r > 0. {
            let sa = sat(src_r * inv_dist);
            energy *= (m / sat(m + 0.5 * sa)).powi(2);
            let cp = mul(r, dot(to_light, r));
            let c2r = sub(cp, to_light);
            to_light = add(to_light, mul(c2r, sat(src_r / dot(c2r, c2r).sqrt())));
        }
        l = norm(to_light);
        let h = norm(add(v, l));
        let nol_s = sat(dot(n, l));
        let nov = sat(dot(n, v).abs() + 1e-5);
        let noh = sat(dot(n, h));
        let voh = sat(dot(v, h));
        let a2 = m * m;
        let dd = (noh * a2 - noh) * noh + 1.;
        let dterm = a2 / (std::f64::consts::PI * dd * dd) * energy;
        let vis = 0.5 / (nol_s * (nov * (1. - m) + m) + nov * (nol_s * (1. - m) + m));
        let fc = (1. - voh).powi(5);
        let sp = [c.spec.x as f64, c.spec.y as f64, c.spec.z as f64];
        let f = [sat(50. * sp[1]) * fc + (1. - fc) * sp[0], sat(50. * sp[1]) * fc + (1. - fc) * sp[1], sat(50. * sp[1]) * fc + (1. - fc) * sp[2]];
        let col = d3(g.color_falloff);
        let mut profile = 1.;
        if let Some(row) = &c.ies {
            let neg = norm(sub(p, lp));
            let u = dot(neg, ldir).clamp(-1., 1.).asin() / std::f64::consts::PI + 0.5;
            let x = (u * row.len() as f64 - 0.5).clamp(0., row.len() as f64 - 1.);
            let (i0, fr) = (x.floor() as usize, x - x.floor());
            profile = row[i0] as f64 * (1. - fr) + row[(i0 + 1).min(row.len() - 1)] as f64 * fr;
        }
        let dif = [c.diffuse.x as f64, c.diffuse.y as f64, c.diffuse.z as f64];
        let mut out = [0.; 3];
        for k in 0..3 { out[k] = (dif[k] / std::f64::consts::PI + dterm * vis * f[k]) * col[k] * nol * atten * profile; }
        out
    }
    const TEST_WGSL: &str = r#"
struct TestCase { p: vec4<f32>, n: vec4<f32>, v: vec4<f32>, diff: vec4<f32>, spec_rough: vec4<f32> }
@group(1) @binding(0) var<storage, read> tc: array<TestCase>;
@group(1) @binding(1) var<storage, read_write> tout: array<vec4<f32>>;
@compute @workgroup_size(1) fn test_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = tc[id.x];
    tout[id.x] = vec4<f32>(local_lighting(c.p.xyz, c.n.xyz, c.v.xyz, c.diff.xyz, c.spec_rough.xyz, c.spec_rough.w, 1u, vec4<f32>(0.0), 1.0), 0.0);
}
"#;
    #[test] fn local_lighting_matches_the_ue_reference() {
        let Ok(g) = crate::gpu::Gpu::headless() else { eprintln!("no GPU adapter: skipped"); return };
        let cam = crate::camera::CameraParams::from_yaw_pitch(Vec3::ZERO, 0., 0., 256, 256);
        let base = |p: Vec3| LocalLight { position_m: p, forward: Vec3::new(0., -1., 0.), color: [3., 2., 1.], attenuation_radius_cm: 600., falloff_exponent: 8., inverse_squared: true, spot_cone_deg: None, source_radius_cm: 0., source_length_cm: 0., ies_row: None, cast_shadows: true, max_draw_distance_cm: 0., fade_range_cm: 0., min_roughness: LOCAL_LIGHT_MIN_ROUGHNESS };
        let surf = Vec3::new(0.3, -0.5, -3.);
        let v = (Vec3::ZERO - surf).normalize();
        let ramp: Vec<f32> = (0..IES_WIDTH).map(|i| 0.2 + 0.8 * i as f32 / (IES_WIDTH - 1) as f32).collect();
        let mut cases = vec![];
        cases.push(RefCase { light: base(Vec3::new(0.5, 1.0, -2.6)), p: surf, n: Vec3::new(0.1, 1., 0.2).normalize(), v, diffuse: Vec3::new(0.5, 0.4, 0.3), spec: Vec3::splat(0.04), rough: 0.5, ies: None });
        let mut spot = base(Vec3::new(0.3, 1.7, -3.));
        spot.inverse_squared = false; spot.falloff_exponent = 4.; spot.spot_cone_deg = Some((2.449541, 48.110115)); spot.forward = Vec3::new(0.5, -1., 0.1).normalize(); spot.attenuation_radius_cm = 1188.2867;
        cases.push(RefCase { light: spot, p: surf, n: Vec3::Y, v, diffuse: Vec3::new(0.6, 0.6, 0.6), spec: Vec3::splat(0.02), rough: 0.7, ies: None });
        let mut horizon = base(Vec3::new(1.2, -0.45, -3.));
        horizon.source_radius_cm = 30.;
        cases.push(RefCase { light: horizon, p: surf, n: Vec3::new(-0.05, 1., 0.).normalize(), v, diffuse: Vec3::new(0.3, 0.5, 0.2), spec: Vec3::new(0.9, 0.7, 0.5), rough: 0.2, ies: None });
        let mut line = base(Vec3::new(0.3, 0.4, -3.2));
        line.source_length_cm = 80.; line.forward = Vec3::new(1., 0., 0.);
        cases.push(RefCase { light: line, p: surf, n: Vec3::new(0.2, 1., -0.1).normalize(), v, diffuse: Vec3::new(0.4, 0.4, 0.4), spec: Vec3::splat(0.04), rough: 0.35, ies: None });
        let mut ies = base(Vec3::new(-0.4, 1.4, -2.7));
        ies.ies_row = Some(0); ies.forward = Vec3::new(0.3, -1., -0.2).normalize();
        cases.push(RefCase { light: ies, p: surf, n: Vec3::Y, v, diffuse: Vec3::new(0.5, 0.5, 0.5), spec: Vec3::splat(0.04), rough: 0.6, ies: Some(ramp.clone()) });
        let src = [fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"), TEST_WGSL].join("\n");
        let module = g.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("local lighting test"), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let pipe = g.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: None, layout: None, module: &module, entry_point: Some("test_main"), compilation_options: Default::default(), cache: None });
        let mut ll = LocalLightsGpu::new(&g.device, &g.queue);
        ll.set_ies_profiles(&g.device, &g.queue, &[ramp.clone()]);
        let cam_buf = crate::texture::buffer_init(&g.device, &[cam.uniform(0.)], wgpu::BufferUsages::UNIFORM, "cam");
        let samp = g.device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        for (i, c) in cases.iter().enumerate() {
            ll.list = vec![c.light.clone()];
            ll.prepare(&g.queue, &cam, 1., 1.);
            assert_eq!(ll.last_visible, 1, "case {i}: light not visible");
            let data: [f32; 20] = [c.p.x, c.p.y, c.p.z, 0., c.n.x, c.n.y, c.n.z, 0., c.v.x, c.v.y, c.v.z, 0., c.diffuse.x, c.diffuse.y, c.diffuse.z, 0., c.spec.x, c.spec.y, c.spec.z, c.rough];
            let cb = crate::texture::buffer_init(&g.device, &data, wgpu::BufferUsages::STORAGE, "case");
            let ob = crate::texture::buffer_init(&g.device, &[0f32; 4], wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, "out");
            let bg0 = g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipe.get_bind_group_layout(0), entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: cam_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::Sampler(&samp) },
                wgpu::BindGroupEntry { binding: BINDING_LIGHTS, resource: ll.lights_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: BINDING_GRID, resource: ll.grid_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: BINDING_IES, resource: wgpu::BindingResource::TextureView(&ll.ies_view) },
            ] });
            let bg1 = g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipe.get_bind_group_layout(1), entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: cb.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: ob.as_entire_binding() },
            ] });
            let mut enc = g.device.create_command_encoder(&Default::default());
            { let mut pass = enc.begin_compute_pass(&Default::default()); pass.set_pipeline(&pipe); pass.set_bind_group(0, &bg0, &[]); pass.set_bind_group(1, &bg1, &[]); pass.dispatch_workgroups(1, 1, 1); }
            let staging = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            enc.copy_buffer_to_buffer(&ob, 0, &staging, 0, 16);
            g.queue.submit([enc.finish()]);
            let slice = staging.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            rx.recv().unwrap().unwrap();
            let got: Vec<f32> = bytemuck::cast_slice(&slice.get_mapped_range().unwrap()).to_vec();
            staging.unmap();
            let want = ue_reference(c);
            assert!(want.iter().any(|w| *w > 1e-6), "case {i}: the reference is zero");
            for k in 0..3 { assert!((got[k] as f64 - want[k]).abs() <= 2e-3 * want[k].abs().max(1e-4), "case {i} channel {k}: gpu {} vs ue {}", got[k], want[k]); }
        }
    }
}
