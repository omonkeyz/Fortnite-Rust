//! Shader slicer: finds the zlib streams UE 4.16 serialises inside cooked materials (`FMaterialShaderMap`) and in
//! `GlobalShaderCache-PCD3D_SM5.bin`, cuts the DXBC container out of each inflated stream and re-signs it (UE zeroes the hash).
//!
//! Layout recovered by the earlier tooling (tools/sky-shader-extract.cjs): every shader's bytecode is an independent zlib stream
//! (`78 9c`), the inflated buffer is `<UE shader header, 24..140 bytes>` followed by the DXBC container; the container's own size field
//! (offset 24) gives the slice length.  The stream's byte offset inside the file is the shader id used by the old reports (`shader_<id>`).

use crate::container::{resign, Container};
use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

fn decompress_to_vec_zlib_with_limit(mut input: &[u8], max_output_size: usize) -> Result<Vec<u8>, TINFLStatus> {
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let mut ret: Vec<u8> = vec![0; input.len().saturating_mul(2).min(max_output_size).min(32 << 10)];
    let mut decomp = Box::<DecompressorOxide>::default();
    let mut out_pos = 0;
    loop {
        let (status, in_consumed, out_consumed) = decompress(&mut decomp, input, &mut ret, out_pos, flags);
        out_pos += out_consumed;
        match status {
            TINFLStatus::Done => { ret.truncate(out_pos); return Ok(ret); }
            TINFLStatus::HasMoreOutput => {
                if in_consumed > input.len() { return Err(status); }
                input = &input[in_consumed..];
                if ret.len() >= max_output_size { return Err(status); }
                let new_len = ret.len().saturating_mul(2).min(max_output_size);
                ret.resize(new_len, 0);
            }
            _ => return Err(status),
        }
    }
}

#[derive(Clone)]
pub struct Blob {
    /// zlib stream offset inside the scanned file (the old tools' shader id)
    pub id: usize,
    /// bytes between the start of the inflated buffer and `DXBC`
    pub prefix: usize,
    /// Unmodified bytes surrounding DXBC in the inflated Unreal shader packet.
    /// Needed for resource-table recovery; length alone loses the bindings.
    pub header: Vec<u8>,
    pub trailer: Vec<u8>,
    /// re-signed DXBC container
    pub bytes: Vec<u8>,
    /// UE stores the container with a zeroed hash: `true` if it was all zero before re-signing
    pub hash_was_zero: bool,
}

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SerializedStream{
    pub start:usize,
    pub end:usize,
    pub frequency:u32,
    pub platform:u32,
    /// Preserve these preceding bytes without assigning their field meaning.
    pub preceding_tag:[u8;8],
    pub tail:SerializedStreamTail,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SerializedStreamTail{
    pub end:usize,
    /// Retained opaque bytes; hash semantics not yet independently verified.
    pub identifier:[u8;20],
    pub unverified_count_word:u32,
    pub sampler_count:u32,
    pub inflated_size:u32,
}
/// Validate the observed 1.8 inline stage/platform/byte-array record against
/// an already decoded shader. Exact compressed consumption is required.
pub fn serialized_stream(data:&[u8],blob:&Blob,shader:&crate::ir::Shader)->Result<SerializedStream,String>{
    use miniz_oxide::inflate::core::{decompress,DecompressorOxide,inflate_flags};
    use miniz_oxide::inflate::TINFLStatus;
    use crate::ir::ProgramType;
    let start=blob.id.checked_sub(20).ok_or("missing inline shader header")?;
    let header=data.get(start..blob.id).ok_or("inline shader header outside data")?;
    let frequency=u32::from_le_bytes(header[8..12].try_into().unwrap());
    let platform=u32::from_le_bytes(header[12..16].try_into().unwrap());
    let count=u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
    let end=blob.id.checked_add(count).ok_or("inline shader range overflow")?;
    let input=data.get(blob.id..end).ok_or("inline shader byte array outside data")?;
    let expected=match shader.program_type{ProgramType::Vertex=>0,ProgramType::Hull=>1,ProgramType::Domain=>2,ProgramType::Pixel=>3,ProgramType::Geometry=>4,ProgramType::Compute=>5,ProgramType::Other(_)=>return Err("unsupported inline shader stage".into())};
    if frequency!=expected{return Err("inline shader stage disagrees with DXBC".into());}
    let len=blob.header.len().checked_add(blob.bytes.len()).and_then(|n|n.checked_add(blob.trailer.len())).filter(|n|*n<=32<<20).ok_or("inline shader output size limit")?;
    let mut output=vec![0;len+1];let mut state=Box::<DecompressorOxide>::default();
    let flags=inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER|inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status,consumed,written)=decompress(&mut state,input,&mut output,0,flags);
    if status!=TINFLStatus::Done || consumed!=count || written!=len{return Err(format!("inline shader length/decompression mismatch: {status:?}, {consumed}/{count}, {written}/{len}"));}
    output.truncate(written);let dxbc_end=blob.header.len()+blob.bytes.len();
    if output[..blob.header.len()]!=blob.header || output[dxbc_end..]!=blob.trailer || resign(&output[blob.header.len()..dxbc_end])!=blob.bytes{return Err("inline shader packet differs from scanned shader".into());}
    let tail_end=end.checked_add(32).ok_or("inline shader tail range overflow")?;
    let raw_tail=data.get(end..tail_end).ok_or("truncated inline shader tail")?;
    let tail=SerializedStreamTail{end:tail_end,identifier:raw_tail[..20].try_into().unwrap(),unverified_count_word:u32::from_le_bytes(raw_tail[20..24].try_into().unwrap()),sampler_count:u32::from_le_bytes(raw_tail[24..28].try_into().unwrap()),inflated_size:u32::from_le_bytes(raw_tail[28..32].try_into().unwrap())};
    if tail.inflated_size as usize!=len{return Err("inline shader tail inflated-size mismatch".into());}
    if tail.sampler_count as usize!=shader.decls.samplers.len(){return Err("inline shader tail sampler-count mismatch".into());}
    Ok(SerializedStream{start,end,frequency,platform,preceding_tag:header[..8].try_into().unwrap(),tail})
}

fn find_dxbc(buf: &[u8]) -> Option<usize> {
    // first position where a plausible container starts: magic, then total size consistent with the buffer
    let mut from = 0;
    while let Some(p) = buf[from..].windows(4).position(|w| w == b"DXBC") {
        let o = from + p;
        if o + 32 <= buf.len() {
            let size = u32::from_le_bytes(buf[o + 24..o + 28].try_into().unwrap()) as usize;
            if size >= 32 && o + size <= buf.len() { return Some(o); }
        }
        from = o + 1;
    }
    None
}

/// All DXBC blobs found in `data` (zlib streams at every `78 9c` / `78 da` / `78 01` / `78 5e` candidate).
pub fn scan_blobs(data: &[u8]) -> Vec<Blob> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x78 && matches!(data[i + 1], 0x9c | 0xda | 0x01 | 0x5e) {
            if let Ok(inflated) = decompress_to_vec_zlib_with_limit(&data[i..], 32 << 20) {
                if let Some(o) = find_dxbc(&inflated) {
                    let size = u32::from_le_bytes(inflated[o + 24..o + 28].try_into().unwrap()) as usize;
                    out.push(Blob { id: i, prefix: o, header: inflated[..o].to_vec(), trailer: inflated[o+size..].to_vec(), hash_was_zero: inflated[o + 4..o + 20].iter().all(|&b| b == 0), bytes: resign(&inflated[o..o + size]) });
                }
            }
        }
        i += 1;
    }
    out
}

/// Plain (uncompressed) DXBC containers embedded in `data` (used for already-inflated dumps).
pub fn scan_raw_dxbc(data: &[u8]) -> Vec<Blob> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = data[from..].windows(4).position(|w| w == b"DXBC") {
        let o = from + p;
        if o + 32 <= data.len() {
            let size = u32::from_le_bytes(data[o + 24..o + 28].try_into().unwrap()) as usize;
            if size >= 32 && o + size <= data.len() && Container::parse(&data[o..o + size]).is_ok() {
                out.push(Blob { id: o, prefix: 0, header:vec![], trailer:vec![], hash_was_zero: data[o + 4..o + 20].iter().all(|&b| b == 0), bytes: resign(&data[o..o + size]) });
                from = o + size;
                continue;
            }
        }
        from = o + 1;
    }
    out
}

/// FNV-1a 64 of the shader program chunk: identical programs share it regardless of container hash/other chunks.
pub fn program_hash(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    if let Ok(c) = Container::parse(bytes) {
        if let Some(p) = c.program() { for &b in p.data { h = (h ^ b as u64).wrapping_mul(0x100000001b3); } }
    }
    h
}
