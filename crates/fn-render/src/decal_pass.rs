#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecalBind {
    View,
    Pass,
    Material,
    Vertex,
    DecalTexture,
    DecalSampler,
    SceneDepth,
    PointSampler,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecalFilter { Nearest, Linear }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecalAddress { Wrap, Clamp, Mirror }

#[derive(Clone, Debug)]
pub struct DecalDesc {
    pub label: String,
    pub wgsl: String,
    pub vs_entry: String,
    pub fs_entry: String,
    pub entries: Vec<(u32, u32, DecalBind)>,
    pub view_bytes: usize,
    pub pass_bytes: usize,
    pub material_bytes: usize,
    pub vertex_bytes: usize,
    pub texture: Option<(u32, u32, bool, Vec<(u32, u32, Vec<u8>)>)>,
    pub sampler: (DecalFilter, DecalFilter, DecalFilter, DecalAddress, u16),
}

#[derive(Clone, Debug, Default)]
pub struct DecalDraw { pub pass: Vec<u8>, pub material: Vec<u8>, pub vertex: Vec<u8> }

#[derive(Clone, Debug, Default)]
pub struct DecalFrame { pub view: Vec<u8>, pub decals: Vec<DecalDraw> }

const ALIGN: u64 = 256;
const BLOCK: u64 = 3 * ALIGN;

pub struct DecalRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    desc: Option<DecalDesc>,
    layouts: Vec<wgpu::BindGroupLayout>,
    pipe: Option<wgpu::RenderPipeline>,
    view_buf: Option<wgpu::Buffer>,
    blocks: Option<wgpu::Buffer>,
    capacity: u64,
    texture: Option<wgpu::TextureView>,
    decal_sampler: Option<wgpu::Sampler>,
    point: wgpu::Sampler,
    groups: Vec<wgpu::BindGroup>,
    count: u32,
    frame: DecalFrame,
    pub last_error: Option<String>,
}

impl DecalRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let point = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("decal scene depth sampler"), address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, address_mode_w: wgpu::AddressMode::ClampToEdge, mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, ..Default::default() });
        DecalRenderer { device: device.clone(), queue: queue.clone(), desc: None, layouts: vec![], pipe: None, view_buf: None, blocks: None, capacity: 0, texture: None, decal_sampler: None, point, groups: vec![], count: 0, frame: DecalFrame::default(), last_error: None }
    }

    fn dynamic(b: DecalBind) -> bool { matches!(b, DecalBind::Pass | DecalBind::Material | DecalBind::Vertex) }

    pub fn create(&mut self, d: DecalDesc, format: wgpu::TextureFormat) -> Result<(), String> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(&d.label), source: wgpu::ShaderSource::Wgsl(d.wgsl.clone().into()) });
        let groups = d.entries.iter().map(|e| e.0).max().map_or(0, |g| g + 1);
        let sizes = |b: DecalBind| match b { DecalBind::View => d.view_bytes, DecalBind::Pass => d.pass_bytes, DecalBind::Material => d.material_bytes, _ => d.vertex_bytes };
        let layouts: Vec<wgpu::BindGroupLayout> = (0..groups).map(|g| {
            let entries: Vec<wgpu::BindGroupLayoutEntry> = d.entries.iter().filter(|e| e.0 == g).map(|&(_, binding, what)| {
                let ty = match what {
                    DecalBind::View | DecalBind::Pass | DecalBind::Material | DecalBind::Vertex => wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: Self::dynamic(what), min_binding_size: wgpu::BufferSize::new(sizes(what) as u64) },
                    DecalBind::DecalTexture => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    DecalBind::SceneDepth => wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    DecalBind::DecalSampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    DecalBind::PointSampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                };
                wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty, count: None }
            }).collect();
            self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(&d.label), entries: &entries })
        }).collect();
        let refs: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(Some).collect();
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(&d.label), bind_group_layouts: &refs, immediate_size: 0 });
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::SrcAlpha, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
        };
        let pipe = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&d.label), layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(&d.vs_entry), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(&d.fs_entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::COLOR })] }),
            multiview_mask: None, cache: None,
        });
        let view_buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("decal view"), size: (d.view_bytes.max(16).div_ceil(16) * 16) as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let (texture, sampler) = self.upload_texture(&d);
        if let Some(e) = crate::gpu::pop_error_scope(scope) { let m = format!("{}: {e}", d.label); self.last_error = Some(m.clone()); return Err(m); }
        self.desc = Some(d); self.layouts = layouts; self.pipe = Some(pipe); self.view_buf = Some(view_buf); self.texture = Some(texture); self.decal_sampler = Some(sampler);
        self.blocks = None; self.capacity = 0; self.groups.clear();
        self.ensure_capacity(64);
        Ok(())
    }

    fn upload_texture(&self, d: &DecalDesc) -> (wgpu::TextureView, wgpu::Sampler) {
        let (w0, h0, srgb, levels) = d.texture.clone().unwrap_or((1, 1, false, vec![(1, 1, vec![0, 0, 0, 255])]));
        let format = if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor { label: Some("decal texture"), size: wgpu::Extent3d { width: w0, height: h0, depth_or_array_layers: 1 }, mip_level_count: levels.len() as u32, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        for (i, (w, h, data)) in levels.iter().enumerate() {
            self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(*h) }, wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 });
        }
        let f = |x: DecalFilter| if x == DecalFilter::Linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
        let m = |x: DecalFilter| if x == DecalFilter::Linear { wgpu::MipmapFilterMode::Linear } else { wgpu::MipmapFilterMode::Nearest };
        let (mag, min, mip, addr, aniso) = d.sampler;
        let a = match addr { DecalAddress::Wrap => wgpu::AddressMode::Repeat, DecalAddress::Clamp => wgpu::AddressMode::ClampToEdge, DecalAddress::Mirror => wgpu::AddressMode::MirrorRepeat };
        let all_linear = mag == DecalFilter::Linear && min == DecalFilter::Linear && mip == DecalFilter::Linear;
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor { label: Some("decal texture sampler"), address_mode_u: a, address_mode_v: a, address_mode_w: a, mag_filter: f(mag), min_filter: f(min), mipmap_filter: m(mip), anisotropy_clamp: if all_linear { aniso.max(1) } else { 1 }, ..Default::default() });
        (tex.create_view(&Default::default()), sampler)
    }

    fn ensure_capacity(&mut self, n: u64) {
        if self.blocks.is_some() && n <= self.capacity { return; }
        let cap = n.max(64).next_power_of_two();
        self.blocks = Some(self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("decal blocks"), size: cap * BLOCK, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
        self.capacity = cap;
        self.groups.clear();
    }

    pub fn installed(&self) -> bool { self.pipe.is_some() }
    pub fn set_frame(&mut self, f: DecalFrame) { self.frame = f; }
    pub fn active(&self) -> bool { self.pipe.is_some() && self.count > 0 }
    pub fn live(&self) -> u32 { self.count }

    pub fn prepare(&mut self, scene_depth: &wgpu::TextureView) {
        let frame = std::mem::take(&mut self.frame);
        self.count = 0;
        self.groups.clear();
        if self.pipe.is_none() || frame.decals.is_empty() { return; }
        self.ensure_capacity(frame.decals.len() as u64);
        if let Some(vb) = self.view_buf.as_ref() { let n = (frame.view.len() as u64).min(vb.size()) as usize; self.queue.write_buffer(vb, 0, &frame.view[..n]); }
        let mut bytes = vec![0u8; frame.decals.len() * BLOCK as usize];
        for (i, d) in frame.decals.iter().enumerate() {
            let o = i * BLOCK as usize;
            for (k, part) in [&d.pass, &d.material, &d.vertex].into_iter().enumerate() {
                let n = part.len().min(ALIGN as usize);
                bytes[o + k * ALIGN as usize..o + k * ALIGN as usize + n].copy_from_slice(&part[..n]);
            }
        }
        if let Some(b) = self.blocks.as_ref() { self.queue.write_buffer(b, 0, &bytes); }
        self.count = frame.decals.len() as u32;
        {
            let (Some(d), Some(blocks), Some(vb), Some(tex), Some(ds)) = (self.desc.as_ref(), self.blocks.as_ref(), self.view_buf.as_ref(), self.texture.as_ref(), self.decal_sampler.as_ref()) else { self.count = 0; return };
            let sizes = |b: DecalBind| match b { DecalBind::Pass => d.pass_bytes, DecalBind::Material => d.material_bytes, _ => d.vertex_bytes } as u64;
            self.groups = self.layouts.iter().enumerate().map(|(g, layout)| {
                let entries: Vec<wgpu::BindGroupEntry> = d.entries.iter().filter(|e| e.0 == g as u32).map(|&(_, binding, what)| wgpu::BindGroupEntry { binding, resource: match what {
                    DecalBind::View => vb.as_entire_binding(),
                    DecalBind::Pass => wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: blocks, offset: 0, size: wgpu::BufferSize::new(sizes(what)) }),
                    DecalBind::Material => wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: blocks, offset: ALIGN, size: wgpu::BufferSize::new(sizes(what)) }),
                    DecalBind::Vertex => wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: blocks, offset: 2 * ALIGN, size: wgpu::BufferSize::new(sizes(what)) }),
                    DecalBind::DecalTexture => wgpu::BindingResource::TextureView(tex),
                    DecalBind::SceneDepth => wgpu::BindingResource::TextureView(scene_depth),
                    DecalBind::DecalSampler => wgpu::BindingResource::Sampler(ds),
                    DecalBind::PointSampler => wgpu::BindingResource::Sampler(&self.point),
                } }).collect();
                self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("decal"), layout, entries: &entries })
            }).collect();
        }
    }

    pub fn encode(&self, enc: &mut wgpu::CommandEncoder, hdr_view: &wgpu::TextureView, timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>) {
        let (Some(pipe), Some(d)) = (self.pipe.as_ref(), self.desc.as_ref()) else { return };
        if self.count == 0 || self.groups.is_empty() { return; }
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("decals"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: hdr_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: timestamps, occlusion_query_set: None, multiview_mask: None,
        });
        pass.set_pipeline(pipe);
        let counts: Vec<usize> = (0..self.groups.len()).map(|g| d.entries.iter().filter(|e| e.0 == g as u32 && Self::dynamic(e.2)).count()).collect();
        let mut offsets = [0u32; 4];
        for i in 0..self.count as u64 {
            offsets.fill((i * BLOCK) as u32);
            for (g, bg) in self.groups.iter().enumerate() { pass.set_bind_group(g as u32, bg, &offsets[..counts[g].min(4)]); }
            pass.draw(0..36, 0..1);
        }
    }
}
