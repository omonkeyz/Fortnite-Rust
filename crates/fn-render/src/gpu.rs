//! Device / adapter setup and an offscreen render target with CPU read-back.
//! Backends: Metal on macOS, Vulkan and DX12 on Windows (no GL, no system C dependencies beyond the GPU driver).
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

/// The platform's native backends: Metal on macOS, Vulkan + DX12 elsewhere.
pub fn default_backends() -> wgpu::Backends {
    if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else {
        wgpu::Backends::VULKAN | wgpu::Backends::DX12
    }
}

/// Backend selection: env `FN_BACKEND` (or wgpu's own `WGPU_BACKEND`) = a single backend restricts it, default is the platform's own ([`default_backends`]).
pub fn backends_from_env() -> wgpu::Backends {
    let v = std::env::var("FN_BACKEND").or_else(|_| std::env::var("WGPU_BACKEND"));
    match v.map(|s| s.to_ascii_lowercase()).as_deref() {
        Ok("vulkan") | Ok("vk") => wgpu::Backends::VULKAN,
        Ok("dx12") | Ok("d3d12") => wgpu::Backends::DX12,
        Ok("metal") | Ok("mtl") => wgpu::Backends::METAL,
        Ok("gles") | Ok("gl") => wgpu::Backends::GL,
        _ => default_backends(),
    }
}

/// Instance flags: `FN_VALIDATION=1` forces `InstanceFlags::debugging() | VALIDATION (FN_VALIDATION=2: advanced, also GPU-based validation)` (API validation layers: Vulkan validation layer /
/// D3D12 debug layer when installed, plus wgpu's own validation, and installs a stderr logger so wgpu warnings become visible),
/// `FN_VALIDATION=0` forces them off, otherwise the build-config default applies (on in debug builds).
pub fn instance_flags_from_env() -> wgpu::InstanceFlags {
    match std::env::var("FN_VALIDATION").as_deref() {
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
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        })).map_err(|e| format!("no suitable GPU adapter: {e}"))?;
        let mut limits = wgpu::Limits::default();
        limits.max_texture_dimension_2d = limits.max_texture_dimension_2d.max(8192).min(adapter.limits().max_texture_dimension_2d);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fn-render device"),
            // GPU pass timing (profiler.rs): requested only when the adapter has it, the renderer degrades gracefully without
            required_features: adapter.features() & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            required_limits: limits,
            ..Default::default()
        })).map_err(|e| format!("request_device failed: {e}"))?;
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| panic!("wgpu error: {e}")));
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
