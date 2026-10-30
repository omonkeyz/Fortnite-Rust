//! Incremental ("streamed") decode of long looping waves (ambience beds, music).
//!
//! A [`StreamSource`] is the *encoded* wave (UE4OPUS or Ogg/Vorbis bytes + a packet index). A [`StreamedWave`] owns a small
//! pool of ring buffers ("slots") and ONE worker thread that decodes ahead of every playhead:
//!
//! * the worker decodes one packet at a time into a power-of-two ring of f32 frames ([`RING_FRAMES`]) and keeps it full;
//! * a voice takes a slot with [`StreamedWave::open`] (a lock-free CAS, no allocation) and reads frames sequentially
//!   through its [`StreamCursor`] (relaxed atomic loads of the ring + one acquire load of the write cursor per block);
//! * when the end of the wave is reached and the voice loops, the worker restarts the decoder from frame 0, so the ring
//!   continues with exactly the frames a whole-wave decode would give after the loop point (sample-exact, bit-identical:
//!   a fresh decoder on the same packets);
//! * if the worker is late the cursor reports an underrun and the voice stalls (outputs silence for that frame without
//!   advancing), it never reads stale data; underruns are counted in [`StreamStats`].
//!
//! Decoding any single pass from frame 0 is bit-identical to [`crate::codec::decode_asset`] (same decoder calls, same
//! f32 conversion, same truncation to the stream's true length). Starting at another offset ([`StreamedWave::open`] with
//! `start_frame != 0`, [`StreamDecoder::seek`]) is exact for Ogg (decode from 0 and discard) and uses a short pre-roll
//! for Opus (CELT/SILK state converges but is not bit-exact; `preroll = usize::MAX` decodes from 0 and is exact).

use crate::codec::guarded;
use crate::codec::ogg::ogg_total_frames;
use crate::codec::opus::{parse_header, split_code3, OpusHeader};
use crate::error::{AudioError, Result};
use crate::soundwave::SoundWaveAsset;
use lewton::inside_ogg::OggStreamReader;
use opus_decoder::OpusDecoder;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::*};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Frames per ring (power of two): 65 536 frames = 2.7 s at 24 kHz.
pub const RING_FRAMES: usize = 1 << 16;
const MASK: u64 = RING_FRAMES as u64 - 1;
/// Frames a slot must hold before it counts as primed (`StreamedWave::wait_primed`).
pub const PRIME_FRAMES: u64 = 8192;
/// Concurrent playheads per wave (the Day ambience plays `Grasslands_Wind04` twice at once).
pub const SLOTS: usize = 4;
/// Opus packets decoded before the seek target to warm the decoder up (60 ms each).
pub const DEFAULT_PREROLL: usize = 6;
/// Largest unit one decode step can produce (120 ms of Opus at 48 kHz = 5760; a Vorbis long block = 4096).
const MAX_UNIT: u64 = 8192;

fn bad(codec: &'static str, d: impl Into<String>) -> AudioError {
    AudioError::Decode { codec, detail: d.into() }
}

// ---------------------------------------------------------------------------------------------------------- source

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamFormat {
    Opus,
    Ogg,
}

#[derive(Clone, Copy, Debug)]
struct PacketRef {
    off: u32,
    len: u16,
    /// First frame (per channel) of the packet in the decoded wave.
    frame0: u64,
}

/// Encoded wave + the facts needed to stream it. Cheap to share (`Arc`).
pub struct StreamSource {
    data: Arc<[u8]>,
    pub format: StreamFormat,
    pub sample_rate: u32,
    pub channels: u16,
    /// Exact decoded length in frames (what `decode_asset` returns for a healthy stream).
    pub frames: u64,
    opus: Vec<PacketRef>,
}

impl std::fmt::Debug for StreamSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamSource({:?} {} Hz {} ch {} frames, {} bytes)", self.format, self.sample_rate, self.channels, self.frames, self.data.len())
    }
}

/// Samples per channel of one Opus packet from its TOC (RFC 6716 s3.1), `None` for an invalid packet.
fn opus_packet_samples(p: &[u8], rate: u32) -> Option<u64> {
    let toc = *p.first()?;
    let cfg = (toc >> 3) as usize;
    // frame duration in 0.1 ms units
    let dur: u64 = match cfg {
        0..=11 => [100, 200, 400, 600][cfg & 3],
        12 | 13 => 100,
        14 | 15 => 200,
        _ => [25, 50, 100, 200][cfg & 3],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => (*p.get(1)? & 0x3f) as u64,
    };
    if count == 0 {
        return None;
    }
    Some(count * dur * rate as u64 / 10000)
}

impl StreamSource {
    /// `UE4OPUS` elementary stream (`StreamedAudio::stream_bytes()` or an inline `OPUS` format payload).
    pub fn from_opus(bytes: Vec<u8>) -> Result<StreamSource> {
        let h: OpusHeader = parse_header(&bytes)?;
        let ch = h.channels as usize;
        if !(1..=2).contains(&ch) {
            return Err(bad("ue-opus", format!("unsupported channel count {ch}")));
        }
        let mut packets = Vec::with_capacity(h.frame_count as usize);
        let (mut p, mut frame0) = (17usize, 0u64);
        for _ in 0..h.frame_count {
            let Some(sz) = bytes.get(p..p + 2) else { break };
            let len = u16::from_le_bytes([sz[0], sz[1]]) as usize;
            let Some(packet) = bytes.get(p + 2..p + 2 + len) else { break };
            let n = opus_packet_samples(packet, h.sample_rate).ok_or_else(|| bad("ue-opus", format!("packet {} has an invalid TOC", packets.len())))?;
            packets.push(PacketRef { off: (p + 2) as u32, len: len as u16, frame0 });
            frame0 += n;
            p += 2 + len;
        }
        let frames = frame0.min(h.true_sample_count as u64);
        Ok(StreamSource { data: bytes.into(), format: StreamFormat::Opus, sample_rate: h.sample_rate, channels: h.channels as u16, frames, opus: packets })
    }

    /// Ogg/Vorbis stream. Needs the final granule position (UE's encoder always writes it); otherwise `Err` and the caller
    /// decodes the whole wave instead.
    pub fn from_ogg(bytes: Vec<u8>) -> Result<StreamSource> {
        let total = ogg_total_frames(&bytes).ok_or_else(|| bad("ogg/vorbis", "no granule position"))?;
        let r = OggStreamReader::new(Cursor::new(&bytes[..])).map_err(|e| bad("ogg/vorbis", format!("{e:?}")))?;
        let (channels, rate) = (r.ident_hdr.audio_channels as u16, r.ident_hdr.audio_sample_rate);
        if !(1..=2).contains(&channels) || rate == 0 {
            return Err(bad("ogg/vorbis", format!("unsupported stream ({channels} ch, {rate} Hz)")));
        }
        Ok(StreamSource { data: bytes.into(), format: StreamFormat::Ogg, sample_rate: rate, channels, frames: total, opus: Vec::new() })
    }

    /// A streamable source for `a`: streamed `OPUS` chunks, an inline `OPUS` or an inline `OGG` payload; `Ok(None)` for
    /// any other format (decode those whole).
    pub fn from_asset(a: &SoundWaveAsset) -> Result<Option<StreamSource>> {
        if let Some(s) = &a.streamed {
            return match s.format.as_str() {
                "OPUS" => StreamSource::from_opus(s.stream_bytes()).map(Some),
                _ => Ok(None),
            };
        }
        let Some(f) = a.primary() else { return Ok(None) };
        match f.name.as_str() {
            "OPUS" => StreamSource::from_opus(f.payload.clone()).map(Some),
            "OGG" => StreamSource::from_ogg(f.payload.clone()).map(Some),
            _ => Ok(None),
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }
}

// -------------------------------------------------------------------------------------------------------- decoder

enum Inner {
    Opus { dec: OpusDecoder, next: usize, pcm: Vec<f32> },
    Ogg { r: Box<OggStreamReader<Cursor<Arc<[u8]>>>> },
}

/// Sequential decoder of one [`StreamSource`]: `next` yields the wave packet by packet, exactly the frames of the
/// whole-wave decode. Not real-time safe (allocates); it runs on the worker thread or in tests.
pub struct StreamDecoder {
    src: Arc<StreamSource>,
    inner: Inner,
    /// Frames delivered so far in this pass (the position of the next frame `next` returns).
    pos: u64,
    /// Frames already decoded past `pos` by a seek (delivered first).
    pending: Vec<f32>,
}

impl StreamDecoder {
    pub fn new(src: Arc<StreamSource>) -> Result<StreamDecoder> {
        let inner = Self::fresh(&src, 0)?;
        Ok(StreamDecoder { src, inner, pos: 0, pending: Vec::new() })
    }

    fn fresh(src: &Arc<StreamSource>, first_packet: usize) -> Result<Inner> {
        match src.format {
            StreamFormat::Opus => {
                let dec = OpusDecoder::new(src.sample_rate, src.channels as usize).map_err(|e| bad("ue-opus", format!("decoder init: {e:?}")))?;
                let max_frames = (src.sample_rate as usize / 1000) * 120;
                Ok(Inner::Opus { dec, next: first_packet, pcm: vec![0f32; max_frames * src.channels as usize] })
            }
            StreamFormat::Ogg => {
                let r = OggStreamReader::new(Cursor::new(src.data.clone())).map_err(|e| bad("ogg/vorbis", format!("{e:?}")))?;
                Ok(Inner::Ogg { r: Box::new(r) })
            }
        }
    }

    pub fn source(&self) -> &Arc<StreamSource> {
        &self.src
    }

    /// Position (frames) of the next frame `next` will deliver.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Back to frame 0 with a brand-new decoder (the loop wrap; bit-exact).
    pub fn rewind(&mut self) -> Result<()> {
        self.inner = Self::fresh(&self.src, 0)?;
        self.pos = 0;
        self.pending.clear();
        Ok(())
    }

    /// Decode one raw unit (an Opus packet, a Vorbis packet) and append it to `out`; `Ok(false)` at the end of the stream.
    fn unit(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        let (src, inner) = (&self.src, &mut self.inner);
        guarded("stream", || match inner {
            Inner::Opus { dec, next, pcm } => {
                let ch = src.channels as usize;
                let max_frames = (src.sample_rate as usize / 1000) * 120;
                let Some(pk) = src.opus.get(*next).copied() else { return Ok(false) };
                let packet = &src.data[pk.off as usize..pk.off as usize + pk.len as usize];
                *next += 1;
                if packet.len() > 1500 {
                    let parts = split_code3(packet).ok_or_else(|| bad("ue-opus", "oversize packet that is not splittable"))?;
                    for part in &parts {
                        let n = dec.decode_float(part, pcm, false).map_err(|e| bad("ue-opus", format!("{e:?}")))?;
                        out.extend_from_slice(&pcm[..n.min(max_frames) * ch]);
                    }
                } else {
                    let n = dec.decode_float(packet, pcm, false).map_err(|e| bad("ue-opus", format!("{e:?}")))?;
                    out.extend_from_slice(&pcm[..n.min(max_frames) * ch]);
                }
                Ok(true)
            }
            Inner::Ogg { r } => match r.read_dec_packet_itl() {
                Ok(Some(p)) => {
                    out.extend(p.into_iter().map(|x| x as f32 / 32768.0));
                    Ok(true)
                }
                Ok(None) => Ok(false),
                Err(e) => Err(bad("ogg/vorbis", format!("{e:?}"))),
            },
        })
    }

    /// Append the next decoded unit (trimmed to the wave's true length) to `out`. `Ok(false)` at the end of the wave.
    /// A unit may be empty (the first Vorbis packet); an error ends the pass like a truncated stream does in the
    /// whole-wave decode.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        let ch = self.src.channels as usize;
        if self.pos >= self.src.frames {
            return Ok(false);
        }
        let room = (self.src.frames - self.pos) as usize;
        if !self.pending.is_empty() {
            let n = (self.pending.len() / ch).min(room);
            out.extend_from_slice(&self.pending[..n * ch]);
            self.pending.clear();
            self.pos += n as u64;
            return Ok(true);
        }
        let start = out.len();
        if !self.unit(out)? {
            return Ok(false);
        }
        let n = ((out.len() - start) / ch).min(room);
        out.truncate(start + n * ch);
        self.pos += n as u64;
        Ok(true)
    }

    /// Position so that the next frame delivered is `frame` (clamped to the length). Ogg: exact (decodes from 0 and
    /// discards). Opus: restarts `preroll` packets before the target packet; `usize::MAX` decodes from packet 0 (exact).
    pub fn seek(&mut self, frame: u64, preroll: usize) -> Result<()> {
        let target = frame.min(self.src.frames);
        let ch = self.src.channels as usize;
        let first = match self.src.format {
            StreamFormat::Opus => {
                let p = self.src.opus.partition_point(|k| k.frame0 <= target).saturating_sub(1);
                p.saturating_sub(preroll)
            }
            StreamFormat::Ogg => 0,
        };
        self.inner = Self::fresh(&self.src, first)?;
        self.pending.clear();
        let mut at = match self.src.format {
            StreamFormat::Opus => self.src.opus.get(first).map_or(0, |k| k.frame0),
            StreamFormat::Ogg => 0,
        };
        let mut tmp = Vec::new();
        while at < target {
            tmp.clear();
            if !self.unit(&mut tmp)? {
                break;
            }
            let n = (tmp.len() / ch) as u64;
            if at + n <= target {
                at += n;
            } else {
                let skip = (target - at) as usize;
                self.pending = tmp[skip * ch..].to_vec();
                at = target;
            }
        }
        self.pos = target;
        Ok(())
    }

    /// Decode the whole wave (tests / fallbacks): the pass from frame 0 to the end.
    pub fn decode_all(src: &Arc<StreamSource>) -> Result<Vec<f32>> {
        let mut d = StreamDecoder::new(src.clone())?;
        let mut out = Vec::with_capacity(src.frames as usize * src.channels as usize);
        loop {
            match d.next(&mut out) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    if out.is_empty() {
                        return Err(e);
                    }
                    break;
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------------- slots + worker

const FREE: u8 = 0; // released: the worker resets it
const PRIMING: u8 = 1; // reset to frame 0 and filling; claimable
const READY: u8 = 2; // holds >= PRIME_FRAMES from frame 0; claimable
const CLAIMED: u8 = 3; // owned by a StreamCursor

#[inline]
fn pack(gen: u64, frames: u64) -> u64 {
    (gen & 0xffff) << 48 | (frames & 0x0000_ffff_ffff_ffff)
}

struct Slot {
    ring: Box<[AtomicU32]>,
    /// `gen << 48 | frames written` in the current sequence (sequence position 0 = the frame the cursor started at).
    written: AtomicU64,
    /// Lowest sequence frame the cursor may still read.
    read: AtomicU64,
    owner: AtomicU8,
    looping: AtomicBool,
    /// 0 = none, else `frame + 1`: reposition the decoder (set by `open`, taken by the worker).
    seek_req: AtomicU64,
    /// Sequence length when a non-looping pass hit the end of the stream (`u64::MAX` = not yet).
    end_seq: AtomicU64,
}

/// Counters shared by a wave's slots, cursors and worker.
#[derive(Default)]
pub struct StreamStats {
    /// Frames a started cursor wanted but the worker had not decoded yet (the voice stalled for them).
    pub underrun_frames: AtomicU64,
    /// Blocks in which at least one underrun frame happened.
    pub underrun_blocks: AtomicU64,
    /// Frames spent waiting for the very first data of a cursor (start-up latency, not an underrun).
    pub startup_stall_frames: AtomicU64,
    pub decoded_frames: AtomicU64,
    pub loop_wraps: AtomicU64,
    pub opens: AtomicU64,
    /// `open` found every slot busy.
    pub starved: AtomicU64,
    pub decode_errors: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStatsSnapshot {
    pub underrun_frames: u64,
    pub underrun_blocks: u64,
    pub startup_stall_frames: u64,
    pub decoded_frames: u64,
    pub loop_wraps: u64,
    pub opens: u64,
    pub starved: u64,
    pub decode_errors: u64,
}

impl StreamStats {
    pub fn snapshot(&self) -> StreamStatsSnapshot {
        StreamStatsSnapshot {
            underrun_frames: self.underrun_frames.load(Relaxed),
            underrun_blocks: self.underrun_blocks.load(Relaxed),
            startup_stall_frames: self.startup_stall_frames.load(Relaxed),
            decoded_frames: self.decoded_frames.load(Relaxed),
            loop_wraps: self.loop_wraps.load(Relaxed),
            opens: self.opens.load(Relaxed),
            starved: self.starved.load(Relaxed),
            decode_errors: self.decode_errors.load(Relaxed),
        }
    }
}

struct Work {
    dec: Option<StreamDecoder>,
    gen: u64,
    /// Frames written into the ring for the current gen.
    seq: u64,
    /// The pass reached its end without looping (or produced nothing): stop topping up.
    ended: bool,
    /// Frames produced by the current pass (0 detects an empty wave so a loop cannot spin).
    pass_frames: u64,
}

struct Shared {
    src: Arc<StreamSource>,
    slots: Vec<Arc<Slot>>,
    stop: AtomicBool,
    stats: Arc<StreamStats>,
    preroll: usize,
}

/// A streamed, looping-capable wave: the worker thread + slot pool. Put it in [`crate::mixer::LoadedWave::stream`].
pub struct StreamedWave {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for StreamedWave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamedWave({:?})", self.shared.src)
    }
}

impl Drop for StreamedWave {
    fn drop(&mut self) {
        // never joins: this can run on the audio thread; the worker notices within one poll interval
        self.shared.stop.store(true, Release);
    }
}

impl StreamedWave {
    pub fn new(src: Arc<StreamSource>) -> Result<Arc<StreamedWave>> {
        Self::with_preroll(src, DEFAULT_PREROLL)
    }

    pub fn with_preroll(src: Arc<StreamSource>, preroll: usize) -> Result<Arc<StreamedWave>> {
        let ch = src.channels as usize;
        let slots = (0..SLOTS)
            .map(|_| {
                Arc::new(Slot {
                    ring: (0..RING_FRAMES * ch).map(|_| AtomicU32::new(0)).collect(),
                    written: AtomicU64::new(0),
                    read: AtomicU64::new(0),
                    // claimable at once (reset to frame 0 by construction); the worker builds the decoder on its first visit
                    owner: AtomicU8::new(PRIMING),
                    looping: AtomicBool::new(true),
                    seek_req: AtomicU64::new(0),
                    end_seq: AtomicU64::new(u64::MAX),
                })
            })
            .collect();
        let shared = Arc::new(Shared { src, slots, stop: AtomicBool::new(false), stats: Arc::new(StreamStats::default()), preroll });
        register(shared.clone())?;
        Ok(Arc::new(StreamedWave { shared }))
    }

    pub fn source(&self) -> &Arc<StreamSource> {
        &self.shared.src
    }
    pub fn sample_rate(&self) -> u32 {
        self.shared.src.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.shared.src.channels
    }
    pub fn frames(&self) -> u64 {
        self.shared.src.frames
    }
    pub fn stats(&self) -> StreamStatsSnapshot {
        self.shared.stats.snapshot()
    }

    /// Block (tests, loaders) until at least `n` slots hold [`PRIME_FRAMES`] from frame 0.
    pub fn wait_primed(&self, n: usize, timeout: Duration) -> bool {
        let t = Instant::now();
        loop {
            let ready = self.shared.slots.iter().filter(|s| s.owner.load(Acquire) == READY).count();
            if ready >= n.min(SLOTS) {
                return true;
            }
            if t.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Take a playhead starting at `start_frame` (0 = the primed beginning, available at once). Lock-free and
    /// allocation-free (safe on the audio thread); `None` when every slot is busy.
    pub fn open(&self, start_frame: u64, looping: bool) -> Option<StreamCursor> {
        let sh = &self.shared;
        let start = start_frame.min(sh.src.frames);
        // prefer a fully primed slot, then one that is still filling
        for want in [READY, PRIMING] {
            for s in &sh.slots {
                if s.owner.compare_exchange(want, CLAIMED, AcqRel, Acquire).is_ok() {
                    s.read.store(0, Release);
                    s.looping.store(looping, Release);
                    let mut gen = s.written.load(Acquire) >> 48;
                    if start != 0 {
                        gen = (gen + 1) & 0xffff;
                        s.seek_req.store(start + 1, Release);
                    }
                    sh.stats.opens.fetch_add(1, Relaxed);
                    return Some(StreamCursor { slot: s.clone(), stats: sh.stats.clone(), ch: sh.src.channels as usize, gen, limit: if looping { u64::MAX } else { sh.src.frames - start }, avail: 0, had_data: false, stalled_this_block: false, base: 0 });
                }
            }
        }
        sh.stats.starved.fetch_add(1, Relaxed);
        None
    }
}

/// One playhead of a [`StreamedWave`]. Sequence position 0 is the frame it was opened at; with looping the sequence
/// simply continues across the loop point. Dropping it frees the slot (an atomic store).
pub struct StreamCursor {
    slot: Arc<Slot>,
    stats: Arc<StreamStats>,
    ch: usize,
    gen: u64,
    /// Non-looping end of the sequence (frames left in the wave at `open`).
    limit: u64,
    avail: u64,
    had_data: bool,
    stalled_this_block: bool,
    /// Sequence frame of wave position 0 for the mixer (it adds one wave length per loop pass). Unused by `frame`.
    pub base: u64,
}

impl Drop for StreamCursor {
    fn drop(&mut self) {
        self.slot.owner.store(FREE, Release);
    }
}

impl StreamCursor {
    /// Frames of the sequence the worker has decoded so far (acquire; call once per block).
    #[inline]
    pub fn refresh(&mut self) -> u64 {
        let w = self.slot.written.load(Acquire);
        self.avail = if w >> 48 == self.gen { w & 0x0000_ffff_ffff_ffff } else { 0 };
        if self.avail > 0 {
            self.had_data = true;
        }
        self.stalled_this_block = false;
        self.avail
    }
    /// Frames available as of the last `refresh`.
    #[inline]
    pub fn available(&self) -> u64 {
        self.avail
    }
    /// End of the sequence for a non-looping voice (`u64::MAX` for a looping one still in range).
    #[inline]
    pub fn end(&self) -> u64 {
        self.limit.min(self.slot.end_seq.load(Relaxed))
    }
    /// Frame `i` of the sequence (valid when `i < available()`).
    #[inline]
    pub fn frame(&self, i: u64) -> (f32, f32) {
        let b = (i & MASK) as usize * self.ch;
        let l = f32::from_bits(self.slot.ring[b].load(Relaxed));
        if self.ch > 1 {
            (l, f32::from_bits(self.slot.ring[b + 1].load(Relaxed)))
        } else {
            (l, l)
        }
    }
    /// The voice needed `n` frames that were not there: it stalled for them.
    #[inline]
    pub fn note_stall(&mut self, n: u64) {
        if self.had_data {
            self.stats.underrun_frames.fetch_add(n, Relaxed);
            if !self.stalled_this_block {
                self.stalled_this_block = true;
                self.stats.underrun_blocks.fetch_add(1, Relaxed);
            }
        } else {
            self.stats.startup_stall_frames.fetch_add(n, Relaxed);
        }
    }
    /// Tell the worker everything below `pos` has been consumed (frees ring space).
    #[inline]
    pub fn consumed(&self, pos: u64) {
        self.slot.read.store(pos, Release);
    }
    /// Offline renderers: wait until frame `need` (exclusive) is available or the pass ended; `false` on timeout.
    pub fn wait_for(&mut self, need: u64, timeout: Duration) -> bool {
        let t = Instant::now();
        loop {
            let a = self.refresh();
            // at the end of a non-looping pass `need` can never be met: whatever is there is all there is
            if a >= need || a >= self.end() {
                return true;
            }
            if t.elapsed() > timeout {
                return false;
            }
            std::thread::yield_now();
        }
    }
}

static PENDING: std::sync::Mutex<Vec<Arc<Shared>>> = std::sync::Mutex::new(Vec::new());
static WORKER: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();

fn register(sh: Arc<Shared>) -> Result<()> {
    PENDING.lock().unwrap_or_else(|p| p.into_inner()).push(sh);
    WORKER
        .get_or_init(|| std::thread::Builder::new().name("fn-audio-stream".into()).spawn(worker).map(|_| ()).map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| AudioError::Device(format!("stream worker: {e}")))
}

fn worker() {
    let mut live: Vec<(Arc<Shared>, Vec<Work>)> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();
    loop {
        {
            let mut p = PENDING.lock().unwrap_or_else(|p| p.into_inner());
            for sh in p.drain(..) { live.push((sh, (0..SLOTS).map(|_| Work { dec: None, gen: 0, seq: 0, ended: false, pass_frames: 0 }).collect())); }
        }
        live.retain(|(sh, _)| !sh.stop.load(Acquire));
        let mut busy = false;
        for (sh, work) in live.iter_mut() { busy |= service(sh, work, &mut tmp); }
        if !busy {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn service(sh: &Shared, work: &mut [Work], tmp: &mut Vec<f32>) -> bool {
    let ch = sh.src.channels as usize;
    {
        let mut busy = false;
        for (slot, w) in sh.slots.iter().zip(work.iter_mut()) {
            match slot.owner.load(Acquire) {
                FREE => {
                    // reset to frame 0 with a fresh decoder, then publish as claimable
                    match StreamDecoder::new(sh.src.clone()) {
                        Ok(d) => w.dec = Some(d),
                        Err(_) => {
                            sh.stats.decode_errors.fetch_add(1, Relaxed);
                            continue;
                        }
                    }
                    w.gen = (w.gen + 1) & 0xffff;
                    w.seq = 0;
                    w.ended = false;
                    w.pass_frames = 0;
                    slot.read.store(0, Release);
                    slot.end_seq.store(u64::MAX, Release);
                    slot.seek_req.store(0, Release);
                    slot.written.store(pack(w.gen, 0), Release);
                    slot.owner.store(PRIMING, Release);
                    busy = true;
                }
                o @ (PRIMING | READY | CLAIMED) => {
                    if o == CLAIMED {
                        let req = slot.seek_req.swap(0, AcqRel);
                        if req != 0 {
                            let target = req - 1;
                            if let Some(d) = w.dec.as_mut() {
                                if d.seek(target, sh.preroll).is_err() {
                                    sh.stats.decode_errors.fetch_add(1, Relaxed);
                                }
                            }
                            w.gen = (w.gen + 1) & 0xffff;
                            w.seq = 0;
                            w.ended = false;
                            w.pass_frames = 0;
                            slot.end_seq.store(u64::MAX, Release);
                            slot.written.store(pack(w.gen, 0), Release);
                        }
                    }
                    if w.ended {
                        continue;
                    }
                    // one packet per visit so the slots progress together
                    let read = slot.read.load(Acquire);
                    if w.seq + MAX_UNIT - read > RING_FRAMES as u64 {
                        continue; // ring full
                    }
                    if w.dec.is_none() {
                        match StreamDecoder::new(sh.src.clone()) {
                            Ok(d) => w.dec = Some(d),
                            Err(_) => {
                                sh.stats.decode_errors.fetch_add(1, Relaxed);
                                w.ended = true;
                                continue;
                            }
                        }
                    }
                    let Some(d) = w.dec.as_mut() else { continue };
                    tmp.clear();
                    let more = match d.next(tmp) {
                        Ok(m) => m,
                        Err(_) => {
                            sh.stats.decode_errors.fetch_add(1, Relaxed);
                            false
                        }
                    };
                    if more {
                        let n = (tmp.len() / ch) as u64;
                        for (i, fr) in tmp.chunks_exact(ch).enumerate() {
                            let b = ((w.seq + i as u64) & MASK) as usize * ch;
                            for (c, s) in fr.iter().enumerate() {
                                slot.ring[b + c].store(s.to_bits(), Relaxed);
                            }
                        }
                        w.seq += n;
                        w.pass_frames += n;
                        sh.stats.decoded_frames.fetch_add(n, Relaxed);
                        slot.written.store(pack(w.gen, w.seq), Release);
                        if w.seq >= PRIME_FRAMES {
                            let _ = slot.owner.compare_exchange(PRIMING, READY, AcqRel, Acquire);
                        }
                        busy = true;
                    } else {
                        // end of the pass (or an error that ends it like a truncated whole-wave decode)
                        let looping = slot.looping.load(Acquire) || slot.owner.load(Acquire) != CLAIMED;
                        if looping && w.pass_frames > 0 {
                            if d.rewind().is_err() {
                                sh.stats.decode_errors.fetch_add(1, Relaxed);
                                w.ended = true;
                            }
                            w.pass_frames = 0;
                            sh.stats.loop_wraps.fetch_add(1, Relaxed);
                            busy = true;
                        } else {
                            slot.end_seq.store(w.seq, Release);
                            w.ended = true;
                        }
                    }
                }
                _ => {}
            }
        }
        busy
    }
}
