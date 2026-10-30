use crate::decode::{decode_mip, DecodeOptions};
use crate::format::PixelFormat;
use crate::decoded::DecodedImage;
use crate::{Error, Result};
use fn_ueio::props::{read_props, Ctx, PropList};
use fn_ueio::{BulkData, Package, Reader, BULK_ZLIB};

/// One entry of `FTexturePlatformData::Mips`.
#[derive(Debug, Clone)]
pub struct Mip {
    /// Width of this mip as `max(1, Mips[0].SizeX >> index)` (this is what UEViewer uses; the stored value can differ
    /// for the smallest mips).
    pub width: usize,
    /// Height of this mip, see [`Mip::width`].
    pub height: usize,
    /// `SizeX` as stored in the `FTexture2DMipMap`.
    pub stored_width: i32,
    /// `SizeY` as stored in the `FTexture2DMipMap`.
    pub stored_height: i32,
    /// `FByteBulkData::BulkDataFlags`.
    pub bulk_flags: u32,
    /// `FByteBulkData::BulkDataSizeOnDisk`.
    pub size_on_disk: i32,
    /// `FByteBulkData::ElementCount`.
    pub element_count: i32,
    /// `FByteBulkData::BulkDataOffsetInFile` (raw).
    pub offset_in_file: i64,
    /// Raw payload (all slices concatenated) if it could be resolved.
    data: Option<Vec<u8>>,
    /// Why there is no payload (`None` when `data` is present).
    missing: Option<String>,
}

impl Mip {
    /// `true` if the payload is available for decoding.
    pub fn is_available(&self) -> bool {
        self.data.is_some()
    }
    /// Raw payload (all slices concatenated).
    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }
    pub fn set_data(&mut self, d: Vec<u8>) { self.data = Some(d); self.missing = None; }
    /// Reason the payload is not available.
    pub fn missing_reason(&self) -> Option<&str> {
        self.missing.as_deref()
    }
    /// `true` if the payload lives in the `.ubulk` file (streamed mip).
    pub fn in_ubulk(&self) -> bool {
        self.bulk_flags & (fn_ueio::BULK_SEPARATE | 0x800) != 0
    }
}

/// A parsed `Texture2D` or `TextureCube` export (cooked UE 4.16 layout, see the crate docs).
#[derive(Debug, Clone)]
pub struct Texture2D {
    /// Export object name.
    pub name: String,
    /// Export class (`Texture2D` or `TextureCube`).
    pub class: String,
    /// `FTexturePlatformData::SizeX` (top mip width).
    pub size_x: usize,
    /// `FTexturePlatformData::SizeY`.
    pub size_y: usize,
    /// 1 for a 2D texture, 6 for a cube map (faces are concatenated per mip, +X -X +Y -Y +Z -Z).
    pub num_slices: usize,
    /// Pixel format.
    pub format: PixelFormat,
    /// `FTexturePlatformData::FirstMip`.
    pub first_mip: i32,
    /// All mips in the file, index 0 = largest.
    pub mips: Vec<Mip>,
    /// `SRGB` property (defaults to `true` when absent).
    pub srgb: bool,
    /// `CompressionSettings` property without the enum prefix, e.g. `TC_Normalmap` (default `TC_Default`).
    pub compression_settings: String,
    /// `AddressX` (default `TA_Wrap`).
    pub address_x: String,
    /// `AddressY` (default `TA_Wrap`).
    pub address_y: String,
    /// `LODGroup` (default `TEXTUREGROUP_World`).
    pub lod_group: String,
    /// `NeverStream` property.
    pub never_stream: bool,
    /// `CompressionNoAlpha` property.
    pub compression_no_alpha: bool,
    /// Every tagged property of the export, for anything not covered above.
    pub props: PropList,
}

fn strip_enum(s: &str) -> String {
    s.rsplit("::").next().unwrap_or(s).to_string()
}

fn read_fname(r: &mut Reader, names: &[String]) -> Result<String> {
    let idx = r.i32()?;
    let num = r.i32()?;
    let base = names.get(idx as usize).cloned().ok_or_else(|| Error::Invalid(format!("bad FName index {idx}")))?;
    Ok(if num > 0 { format!("{base}_{}", num - 1) } else { base })
}

impl Texture2D {
    /// Parse the first `Texture2D` / `TextureCube` export of a package.
    pub fn parse(pkg: &Package) -> Result<Texture2D> {
        let mut other = None;
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class(i);
            if class == "Texture2D" || class == "TextureCube" || class == "TextureLightProfile" {
                return Self::parse_export(pkg, i);
            }
            if class.contains("Texture") && other.is_none() {
                other = Some(class);
            }
        }
        Err(Error::NotATexture(match other {
            Some(c) => format!("class {c} is not supported (only Texture2D / TextureCube)"),
            None => "package has no Texture2D export".to_string(),
        }))
    }

    /// Parse from raw `.uasset`, `.uexp`, `.ubulk` file images (`uexp` / `ubulk` may be empty).
    pub fn from_files(uasset: &[u8], uexp: &[u8], ubulk: &[u8]) -> Result<Texture2D> {
        let pkg = Package::parse(uasset, uexp, ubulk)?;
        Self::parse(&pkg)
    }

    /// Parse export number `idx` (0 based) which must be a `Texture2D` or `TextureCube`.
    pub fn parse_export(pkg: &Package, idx: usize) -> Result<Texture2D> {
        let class = pkg.export_class(idx);
        if class != "Texture2D" && class != "TextureCube" && class != "TextureLightProfile" {
            return Err(Error::NotATexture(format!("export {idx} has class {class}")));
        }
        let (start, end) = pkg.export_range(idx);
        let data = pkg.data.get(..end).ok_or_else(|| Error::Invalid("export out of range".into()))?;
        let mut r = Reader::at(data, start);
        let ctx = Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
        let props = read_props(&mut r, &ctx)?;

        // UObject::Serialize tail: bool bSerializeGuid (+ FGuid), then UTexture / UTexture2D FStripDataFlags.
        if r.i32()? != 0 {
            r.skip(16)?;
        }
        r.skip(2)?; // UTexture FStripDataFlags
        r.skip(2)?; // UTexture2D / UTextureCube FStripDataFlags
        let cooked = r.i32()?;
        if cooked == 0 {
            return Err(Error::Invalid("uncooked texture (source art only) is not supported".into()));
        }

        // Loop over per-pixel-format platform data; like UEViewer only the first block is used.
        let mut result: Option<(usize, usize, usize, PixelFormat, i32, Vec<Mip>)> = None;
        loop {
            let fmt_name = read_fname(&mut r, &pkg.names)?;
            if fmt_name == "None" {
                break;
            }
            let skip_offset = r.i32()?;
            if result.is_none() {
                let size_x = r.i32()?;
                let size_y = r.i32()?;
                let num_slices = r.i32()? & 0x3fff_ffff;
                let fmt_str = r.fstring()?;
                let first_mip = r.i32()?;
                let count = r.i32()?;
                if size_x <= 0 || size_y <= 0 || !(0..=32).contains(&count) {
                    return Err(Error::Invalid(format!("bad platform data header {size_x}x{size_y} mips={count}")));
                }
                let format = PixelFormat::from_name(&fmt_str);
                let mut mips = Vec::with_capacity(count as usize);
                // UEViewer derives every mip size from `Mips[0].SizeX/SizeY` (not from the platform data size): when
                // the cooker dropped the top mips (`FirstMip > 0`) the array simply starts at a smaller level.
                let (mut base_x, mut base_y) = (size_x as usize, size_y as usize);
                for level in 0..count as usize {
                    let _cooked = r.i32()?;
                    let bulk = BulkData::read(&mut r)?;
                    let (stored_width, stored_height);
                    // inline payload (no AT_END / SEPARATE flag) follows the header immediately
                    let inline = bulk.flags & (fn_ueio::BULK_AT_END | fn_ueio::BULK_SEPARATE | 0x800) == 0 && !bulk.is_unused();
                    if inline {
                        r.skip(bulk.size_on_disk.max(0) as usize)?;
                    }
                    stored_width = r.i32()?;
                    stored_height = r.i32()?;
                    if level == 0 && stored_width > 0 && stored_height > 0 {
                        base_x = stored_width as usize;
                        base_y = stored_height as usize;
                    }
                    let (payload, missing) = if bulk.is_unused() {
                        (None, Some("bulk data flagged unused (mip stripped)".to_string()))
                    } else if bulk.flags & BULK_ZLIB != 0 {
                        (None, Some("zlib compressed bulk data is not supported".to_string()))
                    } else {
                        match resolve_bulk(pkg, &bulk) {
                            Ok(Some(p)) => (Some(p.to_vec()), None),
                            Ok(None) => (None, Some("empty bulk data".to_string())),
                            Err(e) => (None, Some(e)),
                        }
                    };
                    mips.push(Mip {
                        width: (base_x >> level).max(1),
                        height: (base_y >> level).max(1),
                        stored_width,
                        stored_height,
                        bulk_flags: bulk.flags,
                        size_on_disk: bulk.size_on_disk,
                        element_count: bulk.element_count,
                        offset_in_file: bulk.offset_in_file,
                        data: payload,
                        missing,
                    });
                }
                result = Some((size_x as usize, size_y as usize, num_slices.max(1) as usize, format, first_mip, mips));
            }
            // SkipOffset is an absolute position in `uasset ++ uexp`: always resync on it.
            if skip_offset < 0 || skip_offset as usize > data.len() {
                return Err(Error::Invalid(format!("bad SkipOffset {skip_offset}")));
            }
            r.seek(skip_offset as usize)?;
        }
        let (size_x, size_y, num_slices, format, first_mip, mips) =
            result.ok_or_else(|| Error::Invalid("texture has no platform data".into()))?;

        Ok(Texture2D {
            name: pkg.exports[idx].object_name.clone(),
            class,
            size_x,
            size_y,
            num_slices,
            format,
            first_mip,
            mips,
            srgb: props.bool("SRGB").unwrap_or(true),
            compression_settings: props.text("CompressionSettings").map(strip_enum).unwrap_or_else(|| "TC_Default".into()),
            address_x: props.text("AddressX").map(strip_enum).unwrap_or_else(|| "TA_Wrap".into()),
            address_y: props.text("AddressY").map(strip_enum).unwrap_or_else(|| "TA_Wrap".into()),
            lod_group: props.text("LODGroup").map(strip_enum).unwrap_or_else(|| "TEXTUREGROUP_World".into()),
            never_stream: props.bool("NeverStream").unwrap_or(false),
            compression_no_alpha: props.bool("CompressionNoAlpha").unwrap_or(false),
            props,
        })
    }

    /// `true` for `TextureCube`.
    pub fn is_cube(&self) -> bool {
        self.num_slices == 6
    }

    /// Index of the first (largest) mip with an available payload; this is the mip UEViewer exports.
    pub fn best_mip(&self) -> Option<usize> {
        self.mips.iter().position(|m| m.is_available())
    }

    /// Largest available mip whose width and height are both `<= max_dim`; falls back to the smallest available mip
    /// when every mip is larger.
    pub fn mip_for_max_dimension(&self, max_dim: usize) -> Option<usize> {
        let avail = || self.mips.iter().enumerate().filter(|(_, m)| m.is_available());
        avail().find(|(_, m)| m.width <= max_dim && m.height <= max_dim).map(|(i, _)| i).or_else(|| avail().last().map(|(i, _)| i))
    }

    /// Indices of all mips that have a payload.
    pub fn available_mips(&self) -> Vec<usize> {
        self.mips.iter().enumerate().filter(|(_, m)| m.is_available()).map(|(i, _)| i).collect()
    }

    /// Decode mip `mip_index` (slice 0 for cube maps) to an image.
    pub fn decode(&self, mip_index: usize, opts: DecodeOptions) -> Result<DecodedImage> {
        self.decode_slice(mip_index, 0, opts)
    }

    /// Decode one slice (cube face) of a mip.
    pub fn decode_slice(&self, mip_index: usize, slice: usize, opts: DecodeOptions) -> Result<DecodedImage> {
        let mip = self.mips.get(mip_index).ok_or_else(|| Error::MipUnavailable(format!("mip {mip_index} out of range ({} mips)", self.mips.len())))?;
        let data = mip.data().ok_or_else(|| Error::MipUnavailable(format!("mip {mip_index}: {}", mip.missing_reason().unwrap_or("no data"))))?;
        if slice >= self.num_slices {
            return Err(Error::MipUnavailable(format!("slice {slice} out of range ({} slices)", self.num_slices)));
        }
        let one = self
            .format
            .mip_bytes(mip.width, mip.height)
            .ok_or_else(|| Error::UnsupportedFormat(self.format.name().to_string()))?;
        let start = one * slice;
        let src = data.get(start..start + one).ok_or_else(|| {
            Error::Invalid(format!("mip {mip_index} payload is {} bytes, need {} for slice {slice} of {}x{} {}", data.len(), start + one, mip.width, mip.height, self.format.name()))
        })?;
        decode_mip(&self.format, src, mip.width, mip.height, &opts)
    }
}

fn resolve_bulk<'a>(pkg: &'a Package, b: &BulkData) -> std::result::Result<Option<&'a [u8]>, String> {
    pkg.bulk_payload(b).map_err(|e| e.to_string())
}

