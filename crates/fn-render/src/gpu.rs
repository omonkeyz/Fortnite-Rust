//! Device / adapter setup and an offscreen render target with CPU read-back.
//! Backends: Vulkan and DX12 only (no GL, no system C dependencies beyond the GPU driver).
use std::sync::Arc;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Errors reported by wgpu's uncaptured-error callback (validation, out of memory, internal) when [`Gpu::install_error_sink`] is in use.
#[derive(Default)]
pub struct GpuErrorSink { errors: std::sync::Mutex<Vec<String>>, total: std::sync::atomic::AtomicU64 }
impl GpuErrorSink {
    pub fn push(&self, e: String) { self.total.fetch_add(1, std::sync::atomic::Ordering::Relaxed); if let Ok(mut v) = self.errors.lock() { if v.len() < 16 { v.push(e); } } }
    /// Errors recorded since the last call (at most 16 are kept; `total` still counts every one).
    pub fn take(&self) -> Vec<String> { self.errors.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }
    pub fn total(&self) -> u64 { self.total.load(std::sync::atomic::Ordering::Relaxed) }
}

/// Backend selection: env `FN_BACKEND` (or wgpu's own `WGPU_BACKEND`) = `vulkan` | `dx12` restricts it, default is both (wgpu picks).
pub fn backends_from_env() -> wgpu::Backends {
    // BROWSER PORT: the browser build has exactly one backend (WebGPU)
    if cfg!(target_arch = "wasm32") { return wgpu::Backends::BROWSER_WEBGPU; }
    let v = fn_live::var("FN_BACKEND").or_else(|_| std::env::var("WGPU_BACKEND"));
    match v.map(|s| s.to_ascii_lowercase()).as_deref() {
        Ok("vulkan") | Ok("vk") => wgpu::Backends::VULKAN,
        Ok("dx12") | Ok("d3d12") => wgpu::Backends::DX12,
        _ => wgpu::Backends::VULKAN | wgpu::Backends::DX12,
    }
}

/// Instance flags: `FN_VALIDATION=1` forces `InstanceFlags::debugging() | VALIDATION (FN_VALIDATION=2: advanced, also GPU-based validation)` (API validation layers: Vulkan validation layer /
/// D3D12 debug layer when installed, plus wgpu's own validation, and installs a stderr logger so wgpu warnings become visible),
/// `FN_VALIDATION=0` forces them off, otherwise the build-config default applies (on in debug builds).
pub fn instance_flags_from_env() -> wgpu::InstanceFlags {
    match fn_live::var("FN_VALIDATION").as_deref() {
        Ok("2") => { init_logging(); wgpu::InstanceFlags::advanced_debugging() | wgpu::InstanceFlags::VALIDATION }
        Ok("1") | Ok("true") => { init_logging(); wgpu::InstanceFlags::debugging() | wgpu::InstanceFlags::VALIDATION }
        Ok("0") | Ok("false") => wgpu::InstanceFlags::empty(),
        _ => wgpu::InstanceFlags::from_build_config(),
    }
}

struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, m: &log::Metadata) -> bool { m.level() <= log::Level::Warn }
    fn log(&self, r: &log::Record) { if self.enabled(r.metadata()) { eprintln!("[{} {}] {}", r.level(), r.target(), r.args()); } }
    fn flush(&self) {}
}
/// Installs a minimal stderr logger (warnings and errors only) so wgpu / naga / validation-layer messages are not dropped. No-op if
/// the application already installed a logger.
pub fn init_logging() {
    static L: StderrLogger = StderrLogger;
    if log::set_logger(&L).is_ok() { log::set_max_level(log::LevelFilter::Warn); }
}

impl Gpu {
    /// Creates the instance (call before creating a surface so both share it). See [`instance_flags_from_env`].
    pub fn create_instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor { backends: backends_from_env(), flags: instance_flags_from_env(), ..wgpu::InstanceDescriptor::new_without_display_handle() })
    }
    /// Picks a high-performance adapter (compatible with `surface` when given) and creates the device.
    pub fn new(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, String> {
        pollster::block_on(Self::new_async(instance, surface))
    }
    /// [`Gpu::new`] as a future (BROWSER PORT: the browser's main thread cannot block on the adapter / device promises, it awaits them).
    pub async fn new_async(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, String> {
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        }).await.map_err(|e| format!("no suitable GPU adapter: {e}"))?;
        let mut limits = wgpu::Limits::default();
        limits.max_texture_dimension_2d = limits.max_texture_dimension_2d.max(8192).min(adapter.limits().max_texture_dimension_2d);
        // FX TEXTURES (water agent, 2026-10-07): a scene-lit FX pipeline binds the material's own textures (up to D3D11's 128 SRVs per stage,
        // D3D11_COMMONSHADER_INPUT_RESOURCE_SLOT_COUNT), the masked clip program's and the scene prelude's 5 (lit_common.wgsl): LandWaterBorder_Mud (11 + 2 + 5)
        // and Fortress_BaseMaterial / Stream_Flowmaps_01 needed 18 > the WebGPU default 16. Up to what the adapter has (a browser adapter may stay at 16).
        limits.max_sampled_textures_per_shader_stage = limits.max_sampled_textures_per_shader_stage.max(128).min(adapter.limits().max_sampled_textures_per_shader_stage);
        limits.max_samplers_per_shader_stage = limits.max_samplers_per_shader_stage.max(32).min(adapter.limits().max_samplers_per_shader_stage);
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fn-render device"),
            // GPU pass timing (profiler.rs): requested only when the adapter has it, the renderer degrades gracefully without
            // SCENE COLOR FORMAT (2026-10-05): R11G11B10 render targets (UE PF_FloatRGB on D3D11) when the adapter has them (light_shafts.rs filter buffers)
            // PERF (2026-10-06): FLOAT32_FILTERABLE lets the DFAO cone trace sample the R32Float global distance field with the hardware filter (distance_field_ao.rs)
            // TEXTURE MEMORY (2026-10-06): TEXTURE_COMPRESSION_BC when the adapter has it (desktop GPUs natively; WebGPU "texture-compression-bc"), so cooked
            // BC1/3/4/5/7 mips can be uploaded as they are; check `device.features()` before using a BC format
            required_features: adapter.features() & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS | wgpu::Features::RG11B10UFLOAT_RENDERABLE | wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::TEXTURE_COMPRESSION_BC),
            required_limits: limits,
            ..Default::default()
        }).await.map_err(|e| format!("request_device failed: {e}"))?;
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| panic!("wgpu error: {e}")));
        // DEVICE LOST (2026-10-06): wgpu drops device-lost errors from error scopes and the uncaptured handler (wgpu backend/wgpu_core.rs:304, "surfaced via
        // callback"), so a lost device only shows as `CurrentSurfaceTexture::Validation` at the next acquire. Log the reason here. Note: on Windows / Vulkan
        // wgpu-hal waits on the acquire fence with wgpu-core's 1 s FRAME_TIMEOUT_MS (present.rs:32, swapchain/native.rs:501-508); a VK_TIMEOUT there maps to
        // DeviceError::Unexpected, which wgpu-core turns into a device loss (device/resource.rs:701-707), so a >1 s presentation stall also ends here.
        device.set_device_lost_callback(|reason, msg| if !matches!(reason, wgpu::DeviceLostReason::Destroyed) { eprintln!("[gpu] DEVICE LOST ({reason:?}): {msg}"); });
        // TEXTURE MEMORY (2026-10-06): the texture decoders that run before the renderer exists (fn-fx predecode workers of the loading screen) learn here
        // whether this device takes BC blocks (fx.rs `bc_native_hint`)
        crate::fx::set_bc_native_hint(device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC));
        Ok(Gpu { instance, adapter, device, queue })
    }
    /// Largest texture / surface edge this device accepts (`max_texture_dimension_2d`): clamp window sizes to it before configuring a surface.
    pub fn max_texture_dimension_2d(&self) -> u32 { self.device.limits().max_texture_dimension_2d }
    /// Replaces the default "panic on any wgpu error" handler (kept for tools and tests, where a panic is the right signal) by one that logs the
    /// error to stderr and records it in the returned sink, so an application can show an error page / box instead of aborting with a panic.
    pub fn install_error_sink(&self) -> Arc<GpuErrorSink> {
        let sink = Arc::new(GpuErrorSink::default());
        let s = sink.clone();
        self.device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| { eprintln!("[wgpu error] {e}"); s.push(e.to_string()); }));
        sink
    }
    /// Headless device (no surface).
    pub fn headless() -> Result<Gpu, String> { Gpu::new(Gpu::create_instance(), None) }
    pub fn describe(&self) -> String {
        let i = self.adapter.get_info();
        format!("{} ({:?}, {:?})", i.name, i.backend, i.device_type)
    }
}

/// DEVICE LOST (2026-10-06): a blocking GPU wait of the running game (screenshots, idle waits, timing read-backs) gives up after 10 s instead of hanging
/// for ever (a lost or hung device; wgpu returns `PollError::Timeout`).
pub fn bounded_wait() -> wgpu::PollType { wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) } }

/// Pops a validation error scope and returns what it caught. Native: blocks on the result (as before). BROWSER PORT: the result is a JS promise that
/// only resolves once the main thread returns to the event loop, so blocking would hang the page; the error (if any) is logged when it arrives
/// and `None` is returned now (a broken shader then shows up as later validation errors instead of taking the native fallback path).
pub fn pop_error_scope(scope: wgpu::ErrorScopeGuard) -> Option<wgpu::Error> {
    #[cfg(not(target_arch = "wasm32"))]
    { pollster::block_on(scope.pop()) }
    #[cfg(target_arch = "wasm32")]
    {
        let f = scope.pop();
        wasm_bindgen_futures::spawn_local(async move { if let Some(e) = f.await { eprintln!("[wgpu error] (validation scope, reported asynchronously) {e}"); } });
        None
    }
}

/// An offscreen colour target that can be read back as tightly packed RGBA8.
pub struct OffscreenTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}
impl OffscreenTarget {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        OffscreenTarget { texture, view, width, height, format }
    }
    /// Copies the target to the CPU: RGBA8 (BGRA targets are swizzled), row-major, top row first.
    pub fn read_rgba8(&self, gpu: &Gpu) -> Vec<u8> {
        if cfg!(target_arch = "wasm32") { let _ = gpu; eprintln!("[readback] read_rgba8 skipped on wasm (blocking map would deadlock)"); return vec![0; (self.width * self.height * 4) as usize]; }
        let bpr_unpadded = self.width * 4;
        let bpr = bpr_unpadded.div_ceil(256) * 256;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size: (bpr * self.height) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(self.height) } },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map callback").expect("map failed");
        let data = slice.get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((bpr_unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * bpr) as usize;
            out.extend_from_slice(&data[start..start + bpr_unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        if matches!(self.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
            for px in out.chunks_exact_mut(4) { px.swap(0, 2); }
        }
        out
    }
}
