//! The cooked shader permutations of one material package, with the ownership the earlier tooling left open ("pass / vertex-factory ownership unrecovered"):
//! every serialised `FShader` entry of a UE 4.16 `FMaterialShaderMap` starts with the 12-byte `FName shader type + u32 end offset` header
//! (`fn_dxbc::unreal_entry`) and contains the `FName` of its **vertex factory type** (`FMeshParticleVertexFactory`, `FLocalVertexFactory`, ...) before the zlib stream
//! of the bytecode. Both are read from the original bytes here, no permutation is chosen from a shader name alone: [`Cooked::find`] needs the (vertex factory, shader
//! type) pair.
use fn_dxbc::extract::{scan_blobs, serialized_stream, Blob};
use fn_dxbc::unreal_packet::{named_bindings, optional_fields, uniform_buffer_names, NamedBinding, ResourceTable};
use fn_dxbc::{Container, Shader};

/// One cooked program with everything the package says about it. (Clone: the browser build decodes a program in full on first use, see [`Cooked::find`].)
#[derive(Clone)]
pub struct Program {
    /// Name of the serialised shader type, e.g. `TBasePassVSFNoLightMapPolicy`.
    pub shader_type: String,
    /// Vertex factory type FName found inside the entry (empty for global / material-only shaders).
    pub vertex_factory: String,
    /// zlib stream offset inside the `.uexp` (the id of the old `shader_<id>` reports).
    pub stream: usize,
    pub blob: Blob,
    pub shader: Shader,
    pub resource_table: ResourceTable,
    pub uniform_buffer_names: Option<Vec<String>>,
    pub bindings: Vec<NamedBinding>,
    /// The loose parameters the entry serialized for itself (per-frame expression offsets, base-pass VS velocity globals): read from the shader map, see
    /// [`crate::shader_params`] (`None` when the entry is not a material shader or did not parse).
    pub params: Option<crate::shader_params::MaterialShaderParams>,
    pub discards: bool,
    pub packed: std::sync::Arc<Vec<u8>>,
}

impl Program {
    pub fn full(&self) -> std::borrow::Cow<'_, Program> {
        if !self.shader.instructions.is_empty() || self.packed.is_empty() { return std::borrow::Cow::Borrowed(self); }
        let mut p = self.clone();
        p.blob.bytes = miniz_oxide::inflate::decompress_to_vec(&self.packed).unwrap_or_default();
        if let Ok(s) = Shader::parse(&p.blob.bytes) { p.shader = s; }
        p.packed = std::sync::Arc::new(Vec::new());
        std::borrow::Cow::Owned(p)
    }
}

#[derive(Clone)]
pub struct Entry {
    pub shader_type: String,
    pub vertex_factory: String,
    pub stream: usize,
    pub major: u8,
    lz_blob: Blob,
    lz_uniform_buffer_names: Option<Vec<String>>,
    lz_params: Option<crate::shader_params::MaterialShaderParams>,
    lz_discards: bool,
    lz_packed: std::sync::Arc<Vec<u8>>,
    lz_cell: std::sync::OnceLock<Box<Program>>,
}

impl Entry {
    fn decode(&self) -> Program {
        let bytes = if self.lz_packed.is_empty() { self.lz_blob.bytes.clone() } else { miniz_oxide::inflate::decompress_to_vec(&self.lz_packed).unwrap_or_default() };
        let mut shader = Shader::parse(&bytes).expect("shader parsed at load");
        let resource_table = ResourceTable::parse(&self.lz_blob.header).expect("resource table parsed at load");
        let bindings = named_bindings(&resource_table, self.lz_uniform_buffer_names.as_deref(), &shader.decls).unwrap_or_default();
        let mut blob = self.lz_blob.clone();
        if self.lz_packed.is_empty() { blob.bytes = bytes; } else { shader.instructions = Vec::new(); }
        Program { shader_type: self.shader_type.clone(), vertex_factory: self.vertex_factory.clone(), stream: self.stream, blob, shader, resource_table, uniform_buffer_names: self.lz_uniform_buffer_names.clone(), bindings, params: self.lz_params.clone(), discards: self.lz_discards, packed: self.lz_packed.clone() }
    }
    pub fn is_decoded(&self) -> bool { self.lz_cell.get().is_some() }
}

impl std::ops::Deref for Entry {
    type Target = Program;
    fn deref(&self) -> &Program { self.lz_cell.get_or_init(|| Box::new(self.decode())) }
}

/// Every program of a material package.
pub struct Cooked {
    pub programs: Vec<Entry>,
    pub names: Vec<String>,
}

impl Cooked {
    /// `data` = the `.uexp` bytes, `names` the package name table, `header` the total header size of the `.uasset` (the end offsets inside the entries are absolute: `header + position in uexp`).
    pub fn parse(data: &[u8], names: &[String], header: usize) -> Result<Cooked, String> { Self::parse_range(data, names, header, 0, data.len()) }

    /// LANDSCAPE MATERIAL (2026-10-08): [`Self::parse`] of the programs whose zlib stream starts in `[lo, hi)` of the `.uexp` (one export's serialized shader maps:
    /// a landscape sublevel umap holds several instances' maps; inflating only the owning export's streams instead of the whole map cuts the parse and the
    /// memory of the cooked landscape path). Stream ids stay absolute `.uexp` offsets.
    pub fn parse_range(data: &[u8], names: &[String], header: usize, lo: usize, hi: usize) -> Result<Cooked, String> {
        let t0 = std::time::Instant::now(); // LOAD TIME probe (FN_FX_TIMING=1)
        let (lo, hi) = (lo.min(data.len()), hi.min(data.len()));
        let blobs: Vec<_> = scan_blobs(&data[lo..hi]).into_iter().map(|mut b| { b.id += lo; b }).collect();
        let t_scan = t0.elapsed();
        let mut t_shader = std::time::Duration::ZERO;
        let mut programs = vec![];
        let mut search_from = lo;
        for blob in blobs {
            let ts = std::time::Instant::now();
            let shader = Shader::parse(&blob.bytes).map_err(|e| format!("shader stream {}: {e}", blob.id))?;
            t_shader += ts.elapsed();
            let st = serialized_stream(data, &blob, &shader).map_err(|e| format!("shader stream {} inline record: {e}", blob.id))?;
            let entry = fn_dxbc::unreal_entry::named_entry(data, names, search_from, header, &st).map_err(|e| format!("shader stream {} named entry: {e}", blob.id))?;
            // vertex factory FName: the entry carries it twice, between its header and the inline stream header (20 bytes before the zlib stream); some entries
            // start with one more, unrelated `FLocalVertexFactory` name, so the LAST such name is the owner
            let mut vf = String::new();
            let lo = entry.offset + 12;
            let hi = st.start;
            let mut i = lo;
            while i + 8 <= hi {
                let idx = i32::from_le_bytes(data[i..i + 4].try_into().unwrap());
                let num = i32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap());
                if idx >= 0 && num == 0 {
                    if let Some(n) = names.get(idx as usize) { if n.ends_with("VertexFactory") || n.starts_with("TGPUSkinVertexFactory") { vf = n.clone(); } }
                }
                i += 1;
            }            search_from = st.tail.end;
            let resource_table = ResourceTable::parse(&blob.header).map_err(|e| format!("shader stream {} resource table: {e}", blob.id))?;
            let optional = optional_fields(&blob.trailer).map_err(|e| format!("shader stream {} optional fields: {e}", blob.id))?;
            let uniform_buffer_names = uniform_buffer_names(&optional)?;
            let bindings = named_bindings(&resource_table, uniform_buffer_names.as_deref(), &shader.decls).unwrap_or_default();
            // serialized FMaterialShader parameters of this entry (shader_params.rs: offsets read from the cooked data instead of inferred)
            let params = crate::shader_params::parse(data, names, entry.offset, (entry.name_index, entry.number), st.start, header);
            let discards = shader.body().any(|i| i.op == fn_dxbc::Opcode::Discard);
            #[cfg(target_arch = "wasm32")]
            {
                if shader.major < 5 { continue; }
                let _ = (&resource_table, &bindings);
                let major = shader.major;
                drop(shader);
                let mut blob = blob;
                let packed = std::sync::Arc::new({ let mut v = miniz_oxide::deflate::compress_to_vec(&std::mem::take(&mut blob.bytes), 1); v.shrink_to_fit(); v });
                programs.push(Entry { shader_type: entry.name.clone(), vertex_factory: vf, stream: blob.id, major, lz_blob: blob, lz_uniform_buffer_names: uniform_buffer_names, lz_params: params, lz_discards: discards, lz_packed: packed, lz_cell: std::sync::OnceLock::new() });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let major = shader.major;
                let p = Program { shader_type: entry.name.clone(), vertex_factory: vf.clone(), stream: blob.id, blob: blob.clone(), shader, resource_table, uniform_buffer_names: uniform_buffer_names.clone(), bindings, params: params.clone(), discards, packed: std::sync::Arc::new(Vec::new()) };
                let mut lz = blob;
                lz.bytes = Vec::new(); lz.header = Vec::new(); lz.trailer = Vec::new();
                let cell = std::sync::OnceLock::new();
                let _ = cell.set(Box::new(p));
                programs.push(Entry { shader_type: entry.name.clone(), vertex_factory: vf, stream: lz.id, major, lz_blob: lz, lz_uniform_buffer_names: uniform_buffer_names, lz_params: params, lz_discards: discards, lz_packed: std::sync::Arc::new(Vec::new()), lz_cell: cell });
            }
        }
        if fn_live::var_os("FN_FX_TIMING").is_some() { eprintln!("[fx-timing] Cooked::parse: {} programs from {} KB: scan_blobs {:.1} ms, Shader::parse {:.1} ms, total {:.1} ms", programs.len(), data.len() / 1024, t_scan.as_secs_f32() * 1000., t_shader.as_secs_f32() * 1000., t0.elapsed().as_secs_f32() * 1000.); }
        Ok(Cooked { programs, names: names.to_vec() })
    }

    /// The SM5 program of a (vertex factory, shader type) pair; vertex / pixel stage is part of the type name. SM4 duplicates (`vs_4_0`) are skipped.
    pub fn find(&self, vertex_factory: &str, shader_type: &str) -> Option<&Program> {
        self.programs.iter().find(|p| p.vertex_factory == vertex_factory && p.shader_type == shader_type && p.major >= 5).map(|e| &**e)
    }
    /// LANDSCAPE MATERIAL (2026-10-07): the SM5 programs ([`Self::find`] never returns another) whose zlib stream lies in `[lo, hi)` of the `.uexp` (one export's serialized shader maps: a map package holds
    /// the `StaticPermutationMaterialResources` of several `LandscapeMaterialInstanceConstant`s, each export owning its own).
    pub fn subset(&self, lo: usize, hi: usize) -> Cooked {
        let keep: Vec<usize> = (0..self.programs.len()).filter(|&i| self.programs[i].stream >= lo && self.programs[i].stream < hi && self.programs[i].major >= 5).collect();
        Cooked {
            programs: keep.iter().map(|&i| self.programs[i].clone()).collect(),
            names: self.names.clone(),
        }
    }
    pub fn list(&self) -> Vec<String> {
        self.programs.iter().map(|p| format!("{:>7} {} {:<62} {}", p.stream, p.shader.version_string(), p.shader_type, p.vertex_factory)).collect()
    }
}

/// The names the translated WGSL must expose for [`Program`] (see `fn_dxbc::codegen`): `name_main` is the stock entry point, which the FX path replaces by its own wrapper.
pub fn translated_function_wgsl(shader: &Shader, name: &str) -> Result<String, String> {
    let w = fn_dxbc::codegen::translate_wgsl(shader, name).map_err(|e| e.0)?;
    // keep everything up to the stock `struct <Name>Out { .. }` / entry point
    match w.find("\nstruct ") { Some(p) => Ok(w[..p + 1].to_string()), None => Ok(w) }
}

pub fn container_input_registers(bytes: &[u8]) -> Vec<(String, u32, u32, u8)> {
    // (semantic name, semantic index, register, mask) of the input signature
    Container::parse(bytes).ok().and_then(|c| c.input_signature()).map(|s| s.elements.iter().map(|e| (e.name.clone(), e.semantic_index, e.register, e.mask)).collect()).unwrap_or_default()
}
pub fn container_output_registers(bytes: &[u8]) -> Vec<(String, u32, u32, u8)> {
    Container::parse(bytes).ok().and_then(|c| c.output_signature()).map(|s| s.elements.iter().map(|e| (e.name.clone(), e.semantic_index, e.register, e.mask)).collect()).unwrap_or_default()
}


