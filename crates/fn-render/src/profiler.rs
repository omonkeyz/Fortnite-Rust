//! Per-pass GPU timing with wgpu timestamp queries.
//!
//! * Needs `Features::TIMESTAMP_QUERY` on the device (`Gpu::new` requests it when the adapter has it); without it every call is a no-op and
//!   [`PassTimings::supported`] is false. The overlay (UI) timing additionally needs `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
//! * Each frame writes begin/end timestamps around every pass into one query set, resolves them to a buffer and copies that into one of
//!   [`RING`] staging buffers. Results are collected without stalling a few frames later (results lag the frame by 1-3 frames); if all
//!   staging buffers are still in flight the frame simply is not measured.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Number of in-flight frames of timestamps.
pub const RING: usize = 8;
/// Maximum timed passes per frame (2 timestamps each).
pub const MAX_PASSES: u32 = 128;

/// GPU time of one pass.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuPassTime {
    pub name: String,
    /// Coarse group: `shadow`, `sky`, `scene`, `post` (HDR mip chain + exposure + post + FXAA) or `ui`.
    pub group: &'static str,
    pub ms: f32,
}

/// Timings of one completed GPU frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassTimings {
    /// False when the device has no timestamp queries (the vectors are then empty).
    pub supported: bool,
    /// Renderer frame counter of the measured frame.
    pub frame: u64,
    pub passes: Vec<GpuPassTime>,
    /// First pass begin to last pass end (includes idle gaps between passes).
    pub span_ms: f32,
}
impl PassTimings {
    pub fn sum_ms(&self) -> f32 { self.passes.iter().map(|p| p.ms).sum() }
    /// Summed time of one group.
    pub fn group_ms(&self, group: &str) -> f32 { self.passes.iter().filter(|p| p.group == group).map(|p| p.ms).sum() }
    /// `(group, ms)` in the order the groups first appear.
    pub fn groups(&self) -> Vec<(&'static str, f32)> {
        let mut out: Vec<(&'static str, f32)> = vec![];
        for p in &self.passes {
            match out.iter_mut().find(|g| g.0 == p.group) { Some(g) => g.1 += p.ms, None => out.push((p.group, p.ms)) }
        }
        out
    }
}

struct Staging {
    buf: wgpu::Buffer,
    ready: Arc<AtomicBool>,
    pending: bool,
    frame: u64,
    /// (name, group) per slot of the pending frame.
    names: Vec<(String, &'static str)>,
}

pub struct GpuProfiler {
    query_set: Option<wgpu::QuerySet>,
    resolve: Option<wgpu::Buffer>,
    ring: Vec<Staging>,
    period_ns: f32,
    inside_encoders: bool,
    /// Slots allocated this frame (None = this frame is not measured).
    frame_names: Mutex<Option<Vec<(String, &'static str)>>>,
    cursor: usize,
    frame: u64,
    last: PassTimings,
    completed: Vec<PassTimings>,
    map_pending: Option<(usize, u64)>,
}

impl GpuProfiler {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuProfiler {
        let supported = device.features().contains(wgpu::Features::TIMESTAMP_QUERY) && queue.get_timestamp_period() > 0.0;
        let inside_encoders = supported && device.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        if !supported {
            return GpuProfiler { query_set: None, resolve: None, ring: vec![], period_ns: 0., inside_encoders: false, frame_names: Mutex::new(None), cursor: 0, frame: 0, last: PassTimings::default(), completed: vec![], map_pending: None };
        }
        let count = MAX_PASSES * 2;
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("pass timestamps"), ty: wgpu::QueryType::Timestamp, count });
        let size = (count as u64) * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor { label: Some("timestamp resolve"), size, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let ring = (0..RING).map(|_| Staging {
            buf: device.create_buffer(&wgpu::BufferDescriptor { label: Some("timestamp staging"), size, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false }),
            ready: Arc::new(AtomicBool::new(false)), pending: false, frame: 0, names: vec![],
        }).collect();
        GpuProfiler { query_set: Some(query_set), resolve: Some(resolve), ring, period_ns: queue.get_timestamp_period(), inside_encoders, frame_names: Mutex::new(None), cursor: 0, frame: 0, last: PassTimings { supported: true, ..Default::default() }, completed: vec![], map_pending: None }
    }
    pub fn supported(&self) -> bool { self.query_set.is_some() }
    /// True when overlay (UI) timing is available (needs TIMESTAMP_QUERY_INSIDE_ENCODERS).
    pub fn inside_encoders(&self) -> bool { self.inside_encoders }
    pub fn last(&self) -> &PassTimings { &self.last }
    /// Every frame completed since the previous call, oldest first (the internal queue holds at most 4096 frames; unread ones are dropped).
    pub fn take_completed(&mut self) -> Vec<PassTimings> { std::mem::take(&mut self.completed) }

    /// Starts measuring a frame (no-op when unsupported or every staging buffer is still in flight).
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        let free = self.supported() && !self.ring[self.cursor].pending;
        *self.frame_names.lock().unwrap() = if free { Some(vec![]) } else { None };
    }
    /// Allocates the next begin/end timestamp pair; `None` when the frame is not measured. Returns the first query index.
    pub fn slot(&self, name: &str, group: &'static str) -> Option<u32> {
        let mut g = self.frame_names.lock().unwrap();
        let names = g.as_mut()?;
        if names.len() as u32 >= MAX_PASSES { return None; }
        names.push((name.to_string(), group));
        Some((names.len() as u32 - 1) * 2)
    }
    pub fn render_writes(&self, slot: Option<u32>) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let (qs, s) = (self.query_set.as_ref()?, slot?);
        Some(wgpu::RenderPassTimestampWrites { query_set: qs, beginning_of_pass_write_index: Some(s), end_of_pass_write_index: Some(s + 1) })
    }
    pub fn compute_writes(&self, slot: Option<u32>) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let (qs, s) = (self.query_set.as_ref()?, slot?);
        Some(wgpu::ComputePassTimestampWrites { query_set: qs, beginning_of_pass_write_index: Some(s), end_of_pass_write_index: Some(s + 1) })
    }
    /// Timestamp write outside a pass (queue order); used to time the overlay's own command buffers.
    pub fn write_marker(&self, device: &wgpu::Device, queue: &wgpu::Queue, index: u32) {
        if let (true, Some(qs)) = (self.inside_encoders, self.query_set.as_ref()) {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timestamp marker") });
            enc.write_timestamp(qs, index);
            queue.submit([enc.finish()]);
        }
    }
    /// PERF: timestamp write inside a command encoder (needs TIMESTAMP_QUERY_INSIDE_ENCODERS; no-op otherwise).
    pub fn encoder_marker(&self, enc: &mut wgpu::CommandEncoder, index: u32) {
        if let (true, Some(qs)) = (self.inside_encoders, self.query_set.as_ref()) { enc.write_timestamp(qs, index); }
    }
    /// Resolves the frame's timestamps and starts the read-back (call after everything of the frame was submitted).
    pub fn end_frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timestamp resolve") });
        if self.encode_end_frame(&mut enc) { queue.submit([enc.finish()]); self.start_map(); }
    }
    pub fn encode_end_frame(&mut self, enc: &mut wgpu::CommandEncoder) -> bool {
        let names = match self.frame_names.lock().unwrap().take() { Some(n) if !n.is_empty() => n, _ => return false };
        let (qs, resolve) = (self.query_set.as_ref().unwrap(), self.resolve.as_ref().unwrap());
        let st = &mut self.ring[self.cursor];
        let bytes = names.len() as u64 * 16;
        enc.resolve_query_set(qs, 0..names.len() as u32 * 2, resolve, 0);
        enc.copy_buffer_to_buffer(resolve, 0, &st.buf, 0, bytes);
        st.ready.store(false, Ordering::SeqCst);
        st.pending = true;
        st.frame = self.frame;
        st.names = names;
        self.map_pending = Some((self.cursor, bytes));
        self.cursor = (self.cursor + 1) % RING;
        true
    }
    pub fn start_map(&mut self) {
        let Some((i, bytes)) = self.map_pending.take() else { return };
        let st = &self.ring[i];
        let ready = st.ready.clone();
        st.buf.slice(..bytes).map_async(wgpu::MapMode::Read, move |r| { if r.is_ok() { ready.store(true, Ordering::SeqCst); } });
    }
    /// Collects every finished read-back (non-blocking after a `Poll`; `wait` blocks until all pending frames finished).
    pub fn collect(&mut self, device: &wgpu::Device, wait: bool) {
        if !self.supported() { return; }
        let _ = device.poll(if wait { crate::gpu::bounded_wait() } else { wgpu::PollType::Poll }); // DEVICE LOST: bounded
        let mut done: Vec<usize> = (0..self.ring.len()).filter(|&i| self.ring[i].pending && self.ring[i].ready.load(Ordering::SeqCst)).collect();
        done.sort_by_key(|&i| self.ring[i].frame);
        for i in done {
            let st = &mut self.ring[i];
            let n = st.names.len();
            let ticks: Vec<u64> = {
                let data = st.buf.slice(..(n * 16) as u64).get_mapped_range().expect("timestamp range");
                bytemuck::cast_slice::<u8, u64>(&data).to_vec()
            };
            st.buf.unmap();
            st.pending = false;
            st.ready.store(false, Ordering::SeqCst);
            let ms = |a: u64, b: u64| b.saturating_sub(a) as f32 * self.period_ns * 1e-6;
            let passes: Vec<GpuPassTime> = st.names.iter().enumerate().map(|(k, (name, group))| GpuPassTime { name: name.clone(), group, ms: ms(ticks[k * 2], ticks[k * 2 + 1]) }).collect();
            let (lo, hi) = (ticks.iter().copied().min().unwrap_or(0), ticks.iter().copied().max().unwrap_or(0));
            let t = PassTimings { supported: true, frame: st.frame, passes, span_ms: ms(lo, hi) };
            if self.completed.len() < 4096 { self.completed.push(t.clone()); }
            if st.frame >= self.last.frame { self.last = t; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_sum_in_first_seen_order() {
        let t = PassTimings { supported: true, frame: 1, span_ms: 3.0, passes: vec![
            GpuPassTime { name: "a".into(), group: "shadow", ms: 0.5 }, GpuPassTime { name: "b".into(), group: "scene", ms: 1.0 }, GpuPassTime { name: "c".into(), group: "shadow", ms: 0.25 },
        ] };
        assert_eq!(t.groups(), vec![("shadow", 0.75), ("scene", 1.0)]);
        assert!((t.sum_ms() - 1.75).abs() < 1e-6 && t.group_ms("scene") == 1.0);
    }
}
