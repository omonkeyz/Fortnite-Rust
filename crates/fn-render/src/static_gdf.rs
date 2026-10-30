use crate::distance_field::{build_object, pack_atlas, MeshDistanceField, OBJECT_VEC4S};
use crate::distance_field_ao::{clipmap_extent, cone_half_angle, ue};
use glam::{Mat4, Vec3};
use std::sync::Arc;

pub const VIEW_GLOBAL_VOLUME_CENTER_AND_EXTENT: usize = 166;
pub const VIEW_GLOBAL_VOLUME_WORLD_TO_UV_ADD_AND_MUL: usize = 170;
pub const VIEW_GLOBAL_VOLUME_DIMENSION: usize = 174;
const CELL: u32 = 16;

const WGSL: &str = r#"
struct GU { vmin: vec4<f32>, misc: vec4<f32> }
@group(0) @binding(0) var<uniform> gu: GU;
@group(0) @binding(1) var<storage, read> objects: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> lists: array<u32>;
@group(0) @binding(3) var df_atlas: texture_3d<f32>;
@group(0) @binding(4) var df_samp: sampler;
@group(0) @binding(5) var gdf_out: texture_storage_3d<rgba16float, write>;
const OBJ_STRIDE: u32 = 9u;
fn box_distance(mins: vec3<f32>, maxs: vec3<f32>, p: vec3<f32>) -> f32 {
    let dmin = select(vec3<f32>(0.0), abs(p - mins), p < mins);
    let dmax = select(vec3<f32>(0.0), abs(p - maxs), p > maxs);
    return dot(dmin, vec3<f32>(1.0)) + dot(dmax, vec3<f32>(1.0));
}
fn obj_w2v(i: u32) -> mat4x4<f32> {
    let b = i * OBJ_STRIDE;
    return mat4x4<f32>(objects[b + 1u], objects[b + 2u], objects[b + 3u], objects[b + 4u]);
}
fn sample_mesh_df(uv: vec3<f32>, mad: vec2<f32>) -> f32 {
    return textureSampleLevel(df_atlas, df_samp, uv, 0.0).x * mad.x + mad.y;
}
@compute @workgroup_size(4, 4, 4)
fn cs_static_gdf(@builtin(global_invocation_id) id: vec3<u32>) {
    let res = u32(gu.misc.x);
    if (id.x >= res || id.y >= res || id.z >= res) { return; }
    let extent = gu.vmin.w;
    let wpos = gu.vmin.xyz + (vec3<f32>(id) + 0.5) / gu.misc.x * extent * 2.0;
    let radius = gu.misc.y;
    let n = u32(gu.misc.z);
    let cell = id / 16u;
    let ci = (cell.z * n + cell.y) * n + cell.x;
    let first = lists[ci * 2u];
    let cnt = lists[ci * 2u + 1u];
    var min_d = radius;
    for (var k = 0u; k < cnt; k = k + 1u) {
        let oi = lists[first + k];
        let b = oi * OBJ_STRIDE;
        let lpe = objects[b + 5u].xyz;
        let uvs = objects[b + 6u];
        let vs = abs(uvs.w);
        let vp = (obj_w2v(oi) * vec4<f32>(wpos, 1.0)).xyz;
        let bd = box_distance(-lpe, lpe, vp) * vs;
        if (bd < radius) {
            let cl = clamp(vp, -lpe, lpe);
            let dc = length(vp - cl);
            let d = (sample_mesh_df(cl * uvs.xyz + objects[b + 7u].xyz, objects[b + 8u].xy) + dc) * vs;
            min_d = min(min_d, d);
        }
    }
    textureStore(gdf_out, vec3<i32>(id), vec4<f32>(min_d, 0.0, 0.0, 0.0));
}
"#;

#[derive(Clone, Copy, Debug)]
pub struct StaticGdfInstance { pub mesh: usize, pub local_to_world: Mat4 }

pub struct StaticGdf {
    pub clipmaps: Vec<wgpu::Texture>,
    pub views: [wgpu::TextureView; 4],
    pub sampler: wgpu::Sampler,
    pub view_constants: [[f32; 4]; 9],
    pub objects: usize,
    pub notes: Vec<String>,
}

pub fn max_sphere_query_radius(max_occlusion_distance: f32) -> f32 { max_occlusion_distance / (1. + cone_half_angle().tan()) }

pub fn clipmap_center(view_origin: Vec3, clip: usize) -> Vec3 {
    let extent = clipmap_extent(clip);
    let cell = extent * 2. / ue::GLOBAL_DF_RESOLUTION as f32;
    (view_origin / cell).floor() * cell
}

pub fn view_constants(view_origin: Vec3, clips: usize, max_occlusion_distance: f32) -> [[f32; 4]; 9] {
    let mut v = [[0f32; 4]; 9];
    for c in 0..clips.min(ue::NUM_CLIPMAPS) {
        let e = clipmap_extent(c);
        let center = clipmap_center(view_origin, c);
        v[c] = [center.x, center.y, center.z, e];
        let add = -center / (e * 2.) + Vec3::splat(0.5);
        v[4 + c] = [add.x, add.y, add.z, 1. / (e * 2.)];
    }
    let res = ue::GLOBAL_DF_RESOLUTION as f32;
    v[8] = [res, 1. / res, max_sphere_query_radius(max_occlusion_distance), 0.];
    v
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Gu { vmin: [f32; 4], misc: [f32; 4] }

impl StaticGdf {
    pub fn build(device: &wgpu::Device, queue: &wgpu::Queue, meshes: &[Arc<MeshDistanceField>], instances: &[StaticGdfInstance], view_origin: Vec3, max_occlusion_distance: f32, clips: usize) -> StaticGdf {
        let mut notes = vec![];
        let res = ue::GLOBAL_DF_RESOLUTION;
        let radius = max_sphere_query_radius(max_occlusion_distance);
        let sizes: Vec<Option<[u32; 3]>> = meshes.iter().map(|m| m.is_valid().then_some(m.size)).collect();
        let layout = pack_atlas(&sizes);
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("static gdf atlas"), size: wgpu::Extent3d { width: layout.size[0], height: layout.size[1], depth_or_array_layers: layout.size[2] }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D3, format: wgpu::TextureFormat::R8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
        });
        for (m, b) in meshes.iter().zip(&layout.blocks) {
            let Some(b) = b else { continue };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &atlas, mip_level: 0, origin: wgpu::Origin3d { x: b[0], y: b[1], z: b[2] }, aspect: wgpu::TextureAspect::All },
                &m.texels, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(m.size[0]), rows_per_image: Some(m.size[1]) },
                wgpu::Extent3d { width: m.size[0], height: m.size[1], depth_or_array_layers: m.size[2] },
            );
        }
        let mut objects = vec![];
        for inst in instances {
            let (Some(m), Some(Some(b))) = (meshes.get(inst.mesh), layout.blocks.get(inst.mesh)) else { continue };
            if !m.affects_distance_field_lighting { continue; }
            if let Some(o) = build_object(inst.mesh, m, *b, layout.size, inst.local_to_world) { objects.push(o); }
        }
        let gpu: Vec<[[f32; 4]; OBJECT_VEC4S]> = objects.iter().map(|o| o.gpu()).collect();
        let obj_bytes: Vec<u8> = bytemuck::cast_slice(&gpu).to_vec();
        let mk_buf = |label: &str, bytes: &[u8], usage: wgpu::BufferUsages| {
            let b = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: (bytes.len() as u64).max(16).div_ceil(16) * 16, usage: usage | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            if !bytes.is_empty() { queue.write_buffer(&b, 0, bytes); }
            b
        };
        let obj_buf = mk_buf("static gdf objects", &obj_bytes, wgpu::BufferUsages::STORAGE);
        let atlas_samp = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("static gdf atlas sampler"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("global distance field sampler"), address_mode_u: wgpu::AddressMode::Repeat, address_mode_v: wgpu::AddressMode::Repeat, address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default()
        });
        let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("static gdf"), source: wgpu::ShaderSource::Wgsl(WGSL.into()) });
        let pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("static gdf composite"), layout: None, module: &sm, entry_point: Some("cs_static_gdf"), compilation_options: Default::default(), cache: None });
        let bgl = pipe.get_bind_group_layout(0);
        let mk_vol = |label: &str, size: u32, usage: wgpu::TextureUsages| device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label), size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: size }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D3, format: wgpu::TextureFormat::Rgba16Float, usage, view_formats: &[],
        });
        let mut clipmaps = vec![];
        let cells = res.div_ceil(CELL);
        let atlas_view = atlas.create_view(&Default::default());
        for c in 0..clips.min(ue::NUM_CLIPMAPS) {
            let extent = clipmap_extent(c);
            let center = clipmap_center(view_origin, c);
            let vmin = center - Vec3::splat(extent);
            let texel = extent * 2. / res as f32;
            let mut heads = vec![0u32; (cells * cells * cells * 2) as usize];
            let mut lists: Vec<u32> = vec![];
            let base = heads.len() as u32;
            let mut culled = 0usize;
            for z in 0..cells { for y in 0..cells { for x in 0..cells {
                let ci = ((z * cells + y) * cells + x) as usize;
                let half = Vec3::splat(0.5 * CELL as f32 * texel);
                let tc = vmin + (Vec3::new(x as f32, y as f32, z as f32) + 0.5) * CELL as f32 * texel + 0.5 * texel;
                let first = base + lists.len() as u32;
                for (i, o) in objects.iter().enumerate() {
                    let a = ((o.center - tc).abs() - half).max(Vec3::ZERO);
                    let comb = o.radius + radius;
                    if a.dot(a) < comb * comb { lists.push(i as u32); }
                }
                heads[ci * 2] = first;
                heads[ci * 2 + 1] = base + lists.len() as u32 - first;
                culled += heads[ci * 2 + 1] as usize;
            }}}
            let mut all = heads;
            all.extend_from_slice(&lists);
            let list_buf = mk_buf("static gdf cell lists", bytemuck::cast_slice(&all), wgpu::BufferUsages::STORAGE);
            let gu = Gu { vmin: [vmin.x, vmin.y, vmin.z, extent], misc: [res as f32, radius, cells as f32, 0.] };
            let ubuf = mk_buf("static gdf params", bytemuck::bytes_of(&gu), wgpu::BufferUsages::UNIFORM);
            let tex = mk_vol(&format!("global distance field clipmap {c}"), res, wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING);
            let tv = tex.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("static gdf bg"), layout: &bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: obj_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: list_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&atlas_samp) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tv) },
            ] });
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("static gdf") });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("static gdf composite"), timestamp_writes: None });
                pass.set_pipeline(&pipe);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(res.div_ceil(4), res.div_ceil(4), res.div_ceil(4));
            }
            queue.submit([enc.finish()]);
            notes.push(format!("clipmap {c}: centre {:?} extent {extent} cm, texel {texel:.2} cm, {} object-cell pairs", center.to_array(), culled));
            clipmaps.push(tex);
        }
        let dummy = mk_vol("global distance field (none)", 1, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &dummy, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &[0u8; 8],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(1) }, wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        let views: [wgpu::TextureView; 4] = std::array::from_fn(|i| clipmaps.get(i).unwrap_or(&dummy).create_view(&Default::default()));
        notes.push(format!("{} distance-field objects of {} instances, {} meshes, atlas {:?}, MaxGlobalDistance {radius:.3} cm (MaxOcclusionDistance {max_occlusion_distance})", objects.len(), instances.len(), meshes.len(), layout.size));
        clipmaps.push(dummy);
        StaticGdf { clipmaps, views, sampler, view_constants: view_constants(view_origin, clips, max_occlusion_distance), objects: objects.len(), notes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_constants_match_update_parameter_data() {
        let eye = Vec3::new(-10., 125992.5, 90.45);
        let v = view_constants(eye, 2, 1000.);
        let cell0 = 5000. / 128.;
        assert_eq!(v[0][3], 2500.);
        assert!((v[0][1] - (eye.y / cell0).floor() * cell0).abs() < 1e-2);
        let uv = Vec3::new(v[0][0], v[0][1], v[0][2]) * v[4][3] + Vec3::new(v[4][0], v[4][1], v[4][2]);
        assert!((uv - Vec3::splat(0.5)).abs().max_element() < 1e-5, "the clipmap centre maps to uv 0.5: {uv:?}");
        assert_eq!(v[2], [0.; 4]);
        assert_eq!(v[6], [0.; 4]);
        let tan = (1f32 - 1. / 9.).acos().tan();
        assert!((v[8][2] - 1000. / (1. + tan)).abs() < 1e-3);
        assert_eq!(v[8][0], 128.);
    }
}
