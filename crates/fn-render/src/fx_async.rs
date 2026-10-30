//! BROWSER PORT (wasm32 only; a child module of fx.rs): FX pipelines through `GPUDevice.createRenderPipelineAsync`.
//!
//! In the browser `createRenderPipeline` returns at once, but the GPU process compiles the pipeline (Dawn: WGSL -> HLSL -> DXC) synchronously on
//! its main thread, 0.5-1.1 s for one of the translated UE4 programs: every frame and every queue write of the page waits behind it
//! (verification/browser/launch8 trace: 155 pipelines, 17.5 s; the first-use freezes of new building pieces). `createRenderPipelineAsync`
//! compiles on the browser's worker threads instead, in parallel. So on wasm32 each FX pipeline (and its depth / velocity twins) is created
//! asynchronously: until all of a program's pipelines have resolved, its pipe holds a placeholder and its draws are skipped (the object appears a
//! few frames later instead of freezing the game); [`FxRenderer::pending_pipelines`] says how many are still compiling (the loading screen
//! waits for 0). Natively nothing changes: fx.rs creates its pipelines synchronously.
use super::*;

fn now_ms() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0., |d| d.as_secs_f64() * 1000.) }

/// Which of a program's pipelines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Slot { Main, Depth, Velocity }

#[derive(Default)]
pub(super) struct WebPipes {
    /// pipe index -> its pipelines still compiling (or resolved, waiting for the rest of the set)
    pending: HashMap<usize, Entry>,
    /// the stand-in [`GpuPipe::pipe`] / twin until the real pipeline is there (never drawn with)
    placeholder: Option<wgpu::RenderPipeline>,
    /// programs whose pipeline failed (their draws stay skipped)
    failed: std::collections::HashSet<usize>,
    /// programs installed so far, and when the oldest pending one started (for the log line when the queue drains)
    installed: usize,
    busy_since: Option<f64>,
}

struct Entry { label: String, slots: Vec<(Slot, wgpu::PendingRenderPipeline, Option<wgpu::RenderPipeline>)> }

impl WebPipes {
    /// Starts `desc` for pipe `idx` (the index the program's pipe gets when it is pushed) and returns the placeholder to store meanwhile.
    pub(super) fn start(&mut self, device: &wgpu::Device, depth_format: wgpu::TextureFormat, idx: usize, slot: Slot, desc: &wgpu::RenderPipelineDescriptor) -> wgpu::RenderPipeline {
        let pending = device.fnweb_create_render_pipeline_async(desc);
        if self.pending.is_empty() { self.busy_since = Some(now_ms()); }
        // a program's pipelines start with its main pipeline: a new Main for `idx` replaces what an earlier, failed creation left there
        if slot == Slot::Main { self.pending.insert(idx, Entry { label: desc.label.unwrap_or("fx").to_owned(), slots: vec![] }); }
        if let Some(e) = self.pending.get_mut(&idx) { e.slots.push((slot, pending, None)); }
        self.placeholder(device, depth_format)
    }
    fn placeholder(&mut self, device: &wgpu::Device, depth_format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        if let Some(p) = &self.placeholder { return p.clone(); }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("fx pending placeholder"), source: wgpu::ShaderSource::Wgsl("@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }".into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx pending placeholder"), bind_group_layouts: &[], immediate_size: 0 });
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx pending placeholder"), layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState { format: depth_format, depth_write_enabled: Some(false), depth_compare: Some(wgpu::CompareFunction::Always), stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
        });
        self.placeholder = Some(p.clone());
        p
    }
    /// The program's draws must be skipped (still compiling, or failed).
    pub(super) fn skip(&self, idx: usize) -> bool { self.pending.contains_key(&idx) || self.failed.contains(&idx) }
    pub(super) fn pending_count(&self) -> usize { self.pending.len() }
    pub(super) fn is_pending(&self, idx: usize) -> bool { self.pending.contains_key(&idx) }
}

impl FxRenderer {
    /// Installs every program whose pipelines have all resolved (called at the start of [`FxRenderer::prepare`], i.e. once a frame).
    pub(super) fn web_poll(&mut self) {
        if self.web.pending.is_empty() { return; }
        let mut done = vec![];
        for (idx, e) in self.web.pending.iter_mut() {
            let mut error = None;
            for (_, p, got) in e.slots.iter_mut() {
                if got.is_none() { match p.poll() { Some(Ok(pipe)) => *got = Some(pipe), Some(Err(m)) => error = Some(m), None => {} } }
            }
            if let Some(m) = error { done.push((*idx, Err(format!("{}: {m}", e.label)))); }
            else if e.slots.iter().all(|s| s.2.is_some()) { done.push((*idx, Ok(()))); }
        }
        let drained = !done.is_empty();
        for (idx, r) in done {
            let Some(e) = self.web.pending.remove(&idx) else { continue };
            self.web.installed += 1;
            if let Err(m) = r { eprintln!("[fx] pipeline failed (its draws stay off): {m}"); self.last_error = Some(m); self.web.failed.insert(idx); continue; }
            if idx >= self.pipes.len() { continue; }
            for (slot, _, got) in e.slots {
                let Some(pipe) = got else { continue };
                match slot {
                    Slot::Main => self.pipes[idx].pipe = pipe,
                    Slot::Depth => { self.depth_pipes.insert(idx, pipe); }
                    Slot::Velocity => { self.velocity_pipes.insert(idx, pipe); }
                }
            }
        }
        if drained && self.web.pending.is_empty() {
            let ms = self.web.busy_since.take().map_or(0., |t| now_ms() - t);
            eprintln!("[fx] the browser compiled the queued pipelines in {ms:.0} ms (async; {} programs so far, {} failed)", self.web.installed, self.web.failed.len());
        }
    }
}
