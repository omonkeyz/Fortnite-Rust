pub const MAGIC: &[u8; 4] = b"FNNV";
pub const VERSION: u32 = 2;
pub const NO_NEI: u16 = 0xffff;
pub const EXT_NEI: u16 = 0x8000;
pub const DXY_SCALE: f32 = 16.;
pub const DZ_SCALE: f32 = 1.;
pub const DZ_BIAS: f32 = 1024.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub cs: f32,
    pub ch: f32,
    pub tile_cells: u32,
    pub origin: [f32; 2],
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub agent_radius: f32,
    pub agent_height: f32,
    pub agent_climb: f32,
    pub kill_z: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Link {
    pub poly: u16,
    pub edge: u8,
    pub tmin: u8,
    pub tmax: u8,
    pub to: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Poly {
    pub vbase: u32,
    pub nv: u8,
    pub area: u8,
    pub lbase: u32,
    pub nl: u16,
    pub dvbase: u32,
    pub dvn: u16,
    pub dtbase: u32,
    pub dtn: u16,
    pub zmin: f32,
    pub zmax: f32,
    pub comp: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Tile {
    pub tx: u16,
    pub ty: u16,
    pub zbase: f32,
    pub verts: Vec<[u16; 3]>,
    pub polys: Vec<Poly>,
    pub poly_verts: Vec<u16>,
    pub poly_neis: Vec<u16>,
    pub links: Vec<Link>,
    pub dverts: Vec<[i16; 2]>,
    pub dvz: Vec<u16>,
    pub dtris: Vec<[u8; 3]>,
}

impl Tile {
    pub fn bytes_in_memory(&self) -> usize {
        std::mem::size_of::<Tile>() + self.verts.capacity() * 6 + self.polys.capacity() * std::mem::size_of::<Poly>() + self.poly_verts.capacity() * 2 + self.poly_neis.capacity() * 2
            + self.links.capacity() * std::mem::size_of::<Link>() + self.dverts.capacity() * 4 + self.dvz.capacity() * 2 + self.dtris.capacity() * 3
    }
    pub fn finish(&mut self, ch: f32) {
        let mut lb = 0u32;
        let mut order: Vec<usize> = (0..self.links.len()).collect();
        order.sort_by_key(|&i| (self.links[i].poly, self.links[i].edge));
        self.links = order.iter().map(|&i| self.links[i]).collect();
        for (pi, p) in self.polys.iter_mut().enumerate() {
            p.lbase = lb;
            let mut n = 0;
            while (lb + n) < self.links.len() as u32 && self.links[(lb + n) as usize].poly as usize == pi { n += 1; }
            p.nl = n as u16;
            lb += n;
            let mut zmin = f32::MAX;
            let mut zmax = f32::MIN;
            for k in 0..p.nv as usize {
                let z = self.zbase + self.verts[self.poly_verts[p.vbase as usize + k] as usize][2] as f32 * ch;
                zmin = zmin.min(z);
                zmax = zmax.max(z);
            }
            for k in 0..p.dvn as usize {
                let z = self.zbase + self.dvz[p.dvbase as usize + k] as f32 / DZ_SCALE - DZ_BIAS;
                zmin = zmin.min(z);
                zmax = zmax.max(z);
            }
            p.zmin = zmin;
            p.zmax = zmax;
        }
    }
}

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) { self.0.push(v); }
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()); }
    fn f32(&mut self, v: f32) { self.0.extend_from_slice(&v.to_le_bytes()); }
}

struct R<'a> { b: &'a [u8], p: usize }
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> { let s = self.b.get(self.p..self.p + n)?; self.p += n; Some(s) }
    fn u8(&mut self) -> Option<u8> { Some(self.take(1)?[0]) }
    fn u16(&mut self) -> Option<u16> { let s = self.take(2)?; Some(u16::from_le_bytes([s[0], s[1]])) }
    fn i16(&mut self) -> Option<i16> { Some(self.u16()? as i16) }
    fn u32(&mut self) -> Option<u32> { let s = self.take(4)?; Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]])) }
    fn f32(&mut self) -> Option<f32> { Some(f32::from_bits(self.u32()?)) }
}

pub fn encode_tile(t: &Tile) -> Vec<u8> {
    let mut w = W(Vec::new());
    w.u16(t.tx);
    w.u16(t.ty);
    w.f32(t.zbase);
    w.u16(t.verts.len() as u16);
    for v in &t.verts { w.u16(v[0]); w.u16(v[1]); w.u16(v[2]); }
    w.u16(t.polys.len() as u16);
    for p in &t.polys {
        w.u8(p.nv);
        w.u8(p.area);
        w.u8(p.dvn as u8);
        w.u8(p.dtn as u8);
        for k in 0..p.nv as usize { w.u16(t.poly_verts[p.vbase as usize + k]); }
        for k in 0..p.nv as usize { w.u16(t.poly_neis[p.vbase as usize + k]); }
        for k in 0..p.dvn as usize { let d = t.dverts[p.dvbase as usize + k]; w.u16(d[0] as u16); w.u16(d[1] as u16); w.u16(t.dvz[p.dvbase as usize + k]); }
        for k in 0..p.dtn as usize { let d = t.dtris[p.dtbase as usize + k]; w.u8(d[0]); w.u8(d[1]); w.u8(d[2]); }
    }
    w.u32(t.links.len() as u32);
    for l in &t.links { w.u16(l.poly); w.u8(l.edge); w.u8(l.tmin); w.u8(l.tmax); w.u32(l.to); }
    w.0
}

pub fn decode_tile(b: &[u8], ch: f32) -> Option<Tile> {
    let mut r = R { b, p: 0 };
    let mut t = Tile { tx: r.u16()?, ty: r.u16()?, zbase: r.f32()?, ..Default::default() };
    let nv = r.u16()? as usize;
    t.verts.reserve_exact(nv);
    for _ in 0..nv { t.verts.push([r.u16()?, r.u16()?, r.u16()?]); }
    let np = r.u16()? as usize;
    t.polys.reserve_exact(np);
    for _ in 0..np {
        let (pnv, area, dvn, dtn) = (r.u8()?, r.u8()?, r.u8()?, r.u8()?);
        let p = Poly { vbase: t.poly_verts.len() as u32, nv: pnv, area, dvbase: t.dverts.len() as u32, dvn: dvn as u16, dtbase: t.dtris.len() as u32, dtn: dtn as u16, ..Default::default() };
        for _ in 0..pnv { let v = r.u16()?; if v as usize >= nv { return None; } t.poly_verts.push(v); }
        for _ in 0..pnv { t.poly_neis.push(r.u16()?); }
        for _ in 0..dvn { t.dverts.push([r.i16()?, r.i16()?]); t.dvz.push(r.u16()?); }
        for _ in 0..dtn { let d = [r.u8()?, r.u8()?, r.u8()?]; if d.iter().any(|&i| i >= dvn) { return None; } t.dtris.push(d); }
        t.polys.push(p);
    }
    let nl = r.u32()? as usize;
    t.links.reserve_exact(nl);
    for _ in 0..nl { t.links.push(Link { poly: r.u16()?, edge: r.u8()?, tmin: r.u8()?, tmax: r.u8()?, to: r.u32()? }); }
    if t.links.iter().any(|l| l.poly as usize >= np) { return None; }
    t.finish(ch);
    t.verts.shrink_to_fit();
    t.polys.shrink_to_fit();
    t.poly_verts.shrink_to_fit();
    t.poly_neis.shrink_to_fit();
    t.dverts.shrink_to_fit();
    t.dvz.shrink_to_fit();
    t.dtris.shrink_to_fit();
    Some(t)
}

pub fn encode(h: &Header, tiles: &[Tile]) -> Vec<u8> {
    let mut w = W(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.f32(h.cs);
    w.f32(h.ch);
    w.u32(h.tile_cells);
    w.f32(h.origin[0]);
    w.f32(h.origin[1]);
    w.u32(h.tiles_x);
    w.u32(h.tiles_y);
    w.f32(h.agent_radius);
    w.f32(h.agent_height);
    w.f32(h.agent_climb);
    w.f32(h.kill_z);
    w.u32(tiles.len() as u32);
    let blobs: Vec<Vec<u8>> = tiles.iter().map(encode_tile).collect();
    for b in &blobs { w.u32(b.len() as u32); }
    for b in blobs { w.0.extend_from_slice(&b); }
    w.0
}

pub fn decode_header(b: &[u8]) -> Option<(Header, Vec<(usize, usize)>)> {
    if b.get(..4)? != MAGIC { return None; }
    let mut r = R { b, p: 4 };
    if r.u32()? != VERSION { return None; }
    let h = Header { cs: r.f32()?, ch: r.f32()?, tile_cells: r.u32()?, origin: [r.f32()?, r.f32()?], tiles_x: r.u32()?, tiles_y: r.u32()?, agent_radius: r.f32()?, agent_height: r.f32()?, agent_climb: r.f32()?, kill_z: r.f32()? };
    if h.tiles_x as u64 * h.tiles_y as u64 > (1 << 20) || h.cs <= 0. || h.ch <= 0. { return None; }
    let n = r.u32()? as usize;
    let mut sizes = Vec::with_capacity(n.min(65536));
    for _ in 0..n { sizes.push(r.u32()? as usize); }
    let mut at = r.p;
    let mut dir = Vec::with_capacity(sizes.len());
    for s in sizes { if at + s > b.len() { return None; } dir.push((at, s)); at += s; }
    Some((h, dir))
}
