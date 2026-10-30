pub mod format;

pub use glam::Vec3 as V3;
use format::{Header, Tile, EXT_NEI, NO_NEI};
use glam::Vec2;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

pub const ATHENA_FILE: &str = "nav/athena.fnnav";
pub const UE_MAX_SEARCH_NODES: usize = 2048;
pub const DEFAULT_MAX_SEARCH_NODES: usize = 65536;
pub const HEURISTIC_SCALE: f32 = 0.999;

pub type PolyRef = u32;
pub const POLY_BITS: u32 = 12;
pub const POLY_MASK: u32 = (1 << POLY_BITS) - 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obstacle { pub id: u32, pub min: V3, pub max: V3 }

#[derive(Clone, Debug)]
pub struct PathResult { pub points: Vec<V3>, pub polys: Vec<PolyRef>, pub partial: bool }

pub struct NavMesh {
    pub offset: V3,
    pub game: Option<fn_terrain::Mapping>,
    pub max_search_nodes: usize,
    pub header: Header,
    slots: Vec<u32>,
    tiles: Vec<Tile>,
    obstacles: Vec<Obstacle>,
    next_obstacle: u32,
}

fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
fn frand(s: &mut u64) -> f32 { (splitmix(s) >> 40) as f32 / (1u64 << 24) as f32 }

fn cross2(a: Vec2, b: Vec2, c: Vec2) -> f32 { (b - a).perp_dot(c - a) }

fn closest_on_seg2(p: Vec2, a: Vec2, b: Vec2) -> (Vec2, f32) {
    let d = b - a;
    let l = d.length_squared();
    let t = if l > 1e-12 { ((p - a).dot(d) / l).clamp(0., 1.) } else { 0. };
    (a + d * t, t)
}

fn seg_box2_hit(a: Vec2, b: Vec2, lo: Vec2, hi: Vec2) -> Option<f32> {
    let d = b - a;
    let (mut t0, mut t1) = (0f32, 1f32);
    for k in 0..2 {
        if d[k].abs() < 1e-9 {
            if a[k] < lo[k] || a[k] > hi[k] { return None; }
        } else {
            let (mut ta, mut tb) = ((lo[k] - a[k]) / d[k], (hi[k] - a[k]) / d[k]);
            if ta > tb { std::mem::swap(&mut ta, &mut tb); }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t0 > t1 { return None; }
        }
    }
    Some(t0)
}

#[derive(Clone, Copy)]
struct Node { pos: V3, cost: f32, total: f32, parent: u32, poly: PolyRef, edge: u8, tmin: u8, tmax: u8, closed: bool }

#[derive(PartialEq)]
struct Open(f32, u32);
impl Eq for Open {}
impl PartialOrd for Open { fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(o)) } }
impl Ord for Open { fn cmp(&self, o: &Self) -> std::cmp::Ordering { o.0.total_cmp(&self.0).then(o.1.cmp(&self.1)) } }

impl NavMesh {
    pub fn load_athena() -> Option<NavMesh> {
        athena_candidates().into_iter().find_map(|p| NavMesh::load_from(&p))
    }

    pub fn load_from(path: impl AsRef<Path>) -> Option<NavMesh> {
        let b = std::fs::read(path).ok()?;
        NavMesh::from_bytes(&b)
    }

    pub fn from_bytes(b: &[u8]) -> Option<NavMesh> {
        let (header, dir) = format::decode_header(b)?;
        let mut slots = vec![u32::MAX; (header.tiles_x * header.tiles_y) as usize];
        let mut tiles = Vec::with_capacity(dir.len());
        for (at, len) in dir {
            let t = format::decode_tile(&b[at..at + len], header.ch)?;
            if t.tx as u32 >= header.tiles_x || t.ty as u32 >= header.tiles_y { return None; }
            let s = t.ty as usize * header.tiles_x as usize + t.tx as usize;
            if slots[s] != u32::MAX { return None; }
            slots[s] = tiles.len() as u32;
            tiles.push(t);
        }
        let mut nm = NavMesh { offset: V3::ZERO, game: None, max_search_nodes: DEFAULT_MAX_SEARCH_NODES, header, slots, tiles, obstacles: vec![], next_obstacle: 1 };
        for t in &nm.tiles {
            for l in &t.links {
                let (ts, tp) = (l.to >> POLY_BITS, l.to & POLY_MASK);
                let ok = nm.slots.get(ts as usize).and_then(|&s| nm.tiles.get(s as usize)).is_some_and(|tt| (tp as usize) < tt.polys.len());
                if !ok || l.edge as usize >= t.polys[l.poly as usize].nv as usize { return None; }
            }
            for (pi, p) in t.polys.iter().enumerate() {
                for k in 0..p.nv as usize {
                    let n = t.poly_neis[p.vbase as usize + k];
                    if n != NO_NEI && n & EXT_NEI == 0 && (n as usize >= t.polys.len() || n as usize == pi) { return None; }
                }
            }
        }
        nm.label_components();
        Some(nm)
    }

    fn label_components(&mut self) {
        let mut base = Vec::with_capacity(self.tiles.len());
        let mut n = 0u32;
        for t in &self.tiles { base.push(n); n += t.polys.len() as u32; }
        let mut parent: Vec<u32> = (0..n).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x { let g = p[p[x as usize] as usize]; p[x as usize] = g; x = g; }
            x
        }
        let global = |nm: &NavMesh, r: PolyRef| base[nm.slots[(r >> POLY_BITS) as usize] as usize] + (r & POLY_MASK);
        for (ti, t) in self.tiles.iter().enumerate() {
            let slot = t.ty as u32 * self.header.tiles_x + t.tx as u32;
            for pi in 0..t.polys.len() as u32 {
                let a = base[ti] + pi;
                self.for_each_link((slot << POLY_BITS) | pi, |to, _, _, _| {
                    let (ra, rb) = (find(&mut parent, a), find(&mut parent, global(self, to)));
                    if ra != rb { parent[ra.max(rb) as usize] = ra.min(rb); }
                });
            }
        }
        for ti in 0..self.tiles.len() {
            for pi in 0..self.tiles[ti].polys.len() {
                let c = find(&mut parent, base[ti] + pi as u32);
                self.tiles[ti].polys[pi].comp = c;
            }
        }
    }

    pub fn component(&self, r: PolyRef) -> u32 { self.tile_of(r).polys[(r & POLY_MASK) as usize].comp }

    pub fn load_athena_in(asset_root: impl AsRef<Path>) -> Option<NavMesh> { NavMesh::load_from(asset_root.as_ref().join(ATHENA_FILE)) }

    pub fn with_game_mapping(mut self, map: fn_terrain::Mapping) -> NavMesh { self.game = Some(map); self }

    fn inp(&self, p: V3) -> V3 { match &self.game { Some(m) => m.to_ue(p), None => p + self.offset } }
    fn outp(&self, p: V3) -> V3 { match &self.game { Some(m) => m.to_game(p), None => p - self.offset } }
    fn in_ext(&self, e: V3) -> V3 { match &self.game { Some(_) => V3::new(e.x, e.z, e.y).abs() * 100., None => e.abs() } }
    fn in_len(&self, r: f32) -> f32 { if self.game.is_some() { r * 100. } else { r } }

    pub fn with_offset(mut self, offset: V3) -> NavMesh { self.offset = offset; self }

    pub fn tile_count(&self) -> usize { self.tiles.len() }
    pub fn poly_count(&self) -> usize { self.tiles.iter().map(|t| t.polys.len()).sum() }
    pub fn memory_bytes(&self) -> usize { self.tiles.iter().map(|t| t.bytes_in_memory()).sum::<usize>() + self.slots.capacity() * 4 + std::mem::size_of::<NavMesh>() }

    pub fn add_obstacle(&mut self, min: V3, max: V3) -> u32 {
        let id = self.next_obstacle;
        self.next_obstacle = self.next_obstacle.wrapping_add(1).max(1);
        let (a, b) = (self.inp(min), self.inp(max));
        self.obstacles.push(Obstacle { id, min: a.min(b), max: a.max(b) });
        id
    }
    pub fn remove_obstacle(&mut self, id: u32) -> bool {
        let n = self.obstacles.len();
        self.obstacles.retain(|o| o.id != id);
        n != self.obstacles.len()
    }
    pub fn clear_obstacles(&mut self) { self.obstacles.clear(); }
    pub fn obstacles(&self) -> impl Iterator<Item = Obstacle> + '_ { self.obstacles.iter().map(move |x| { let (a, b) = (self.outp(x.min), self.outp(x.max)); Obstacle { id: x.id, min: a.min(b), max: a.max(b) } }) }

    pub fn find_path(&self, from: V3, to: V3) -> Option<Vec<V3>> {
        let ext = self.default_extent();
        let (sp, _) = self.nearest_poly(self.inp(from), ext)?;
        let (ep, _) = self.nearest_poly(self.inp(to), ext)?;
        if self.component(sp) != self.component(ep) { return None; }
        let r = self.find_path_ex(from, to)?;
        (!r.partial).then_some(r.points)
    }

    pub fn find_path_ex(&self, from: V3, to: V3) -> Option<PathResult> {
        let ext = self.default_extent();
        let (sp, s) = self.nearest_poly(self.inp(from), ext)?;
        let (ep, e) = self.nearest_poly(self.inp(to), ext)?;
        let (polys, edges, partial) = self.astar(sp, s, ep, e)?;
        let end = if partial { self.closest_point_on_poly(*polys.last()?, e) } else { e };
        let mut pts = self.string_pull(&polys, &edges, s, end);
        for p in &mut pts { *p = self.outp(*p); }
        Some(PathResult { points: pts, polys, partial })
    }

    pub fn project(&self, p: V3, extent: V3) -> Option<V3> {
        self.nearest_poly(self.inp(p), self.in_ext(extent)).map(|(_, q)| self.outp(q))
    }

    pub fn random_point_in_radius(&self, center: V3, radius: f32, seed: u64) -> Option<V3> {
        let c = self.inp(center);
        let (sp, sc) = self.nearest_poly(c, self.default_extent())?;
        let radius = self.in_len(radius);
        let r2 = radius.max(0.) * radius.max(0.);
        let mut rng = seed ^ 0x6A09_E667_F3BC_C908;
        let mut seen: HashMap<PolyRef, ()> = HashMap::new();
        let mut stack = vec![sp];
        seen.insert(sp, ());
        let mut cand: Vec<(PolyRef, f32)> = vec![];
        let mut total = 0f32;
        while let Some(p) = stack.pop() {
            let a = self.poly_area(p);
            if a > 0. { total += a; cand.push((p, total)); }
            if seen.len() >= 4096 { continue; }
            self.for_each_link(p, |to, edge, tmin, tmax| {
                if seen.contains_key(&to) { return; }
                let (l, r) = self.portal(p, edge, tmin, tmax);
                let (q, _) = closest_on_seg2(sc.truncate(), r.truncate(), l.truncate());
                if (q - sc.truncate()).length_squared() > r2 { return; }
                seen.insert(to, ());
                stack.push(to);
            });
        }
        if cand.is_empty() { return None; }
        for _ in 0..32 {
            let pick = frand(&mut rng) * total;
            let i = cand.partition_point(|x| x.1 < pick).min(cand.len() - 1);
            let q = self.random_in_poly(cand[i].0, &mut rng);
            if (q - sc).truncate().length_squared() <= r2 && !self.point_in_obstacle(q) { return Some(self.outp(q)); }
        }
        (!self.point_in_obstacle(sc)).then_some(self.outp(sc))
    }

    pub fn raycast(&self, from: V3, to: V3) -> Option<V3> {
        let (s, e) = (self.inp(from), self.inp(to));
        let Some((mut cur, sp)) = self.nearest_poly(s, self.default_extent()) else { return Some(from) };
        let (a, b) = (sp.truncate(), e.truncate());
        let d = b - a;
        let dlen = d.length();
        let mut prev_t = 0f32;
        let mut hit: Option<V3> = None;
        for _ in 0..65536 {
            let vs = self.poly_verts(cur);
            let n = vs.len();
            let mut tmax = f32::MAX;
            let mut exit_edge = usize::MAX;
            for k in 0..n {
                let (p0, p1) = (vs[k].truncate(), vs[(k + 1) % n].truncate());
                let ed = p1 - p0;
                let nrm = Vec2::new(ed.y, -ed.x);
                let den = nrm.dot(d);
                if den <= 1e-9 { continue; }
                let t = nrm.dot(p0 - a) / den;
                if t < tmax { tmax = t; exit_edge = k; }
            }
            if exit_edge == usize::MAX || (1. - tmax) * dlen <= 1. { break; }
            let tmax = tmax.max(prev_t);
            let x = a + d * tmax;
            let mut next = None;
            self.for_each_link(cur, |to, edge, tmin, tmx| {
                if next.is_some() || edge as usize != exit_edge { return; }
                let (l, r) = self.portal(cur, edge, tmin, tmx);
                let (q, _) = closest_on_seg2(x, r.truncate(), l.truncate());
                if (q - x).length_squared() < 1.0 { next = Some(to); }
            });
            match next {
                Some(nx) => { cur = nx; prev_t = tmax; }
                None => { let z = self.height_in_poly(cur, x); hit = Some(V3::new(x.x, x.y, z)); break; }
            }
        }
        let ob = self.obstacle_hit(sp, e);
        let best = match (hit, ob) {
            (Some(h), Some(o)) => Some(if (o - sp).truncate().length_squared() < (h - sp).truncate().length_squared() { o } else { h }),
            (h, o) => h.or(o),
        };
        best.map(|h| self.outp(h))
    }

    pub fn polys_in_box(&self, min: V3, max: V3) -> Vec<Vec<V3>> {
        let (a, b) = (self.inp(min), self.inp(max));
        let mut out = vec![];
        self.for_each_poly_in(a.min(b), a.max(b), |r| out.push(self.poly_verts(r).into_iter().map(|v| self.outp(V3::new(v.x, v.y, self.height_in_poly(r, v.truncate())))).collect()));
        out
    }

    pub fn poly_at(&self, p: V3, extent: V3) -> Option<PolyRef> { self.nearest_poly(self.inp(p), self.in_ext(extent)).map(|x| x.0) }

    fn default_extent(&self) -> V3 { V3::new(self.header.agent_radius * 2., self.header.agent_radius * 2., self.header.agent_height) }

    fn tile_of(&self, r: PolyRef) -> &Tile { &self.tiles[self.slots[(r >> POLY_BITS) as usize] as usize] }

    fn vert_world(&self, t: &Tile, v: u16) -> V3 {
        let h = &self.header;
        let q = t.verts[v as usize];
        let c = h.tile_cells as f32;
        V3::new(h.origin[0] + (t.tx as f32 * c + q[0] as f32) * h.cs, h.origin[1] + (t.ty as f32 * c + q[1] as f32) * h.cs, t.zbase + q[2] as f32 * h.ch)
    }

    fn poly_verts(&self, r: PolyRef) -> Vec<V3> {
        let t = self.tile_of(r);
        let p = &t.polys[(r & POLY_MASK) as usize];
        (0..p.nv as usize).map(|k| self.vert_world(t, t.poly_verts[p.vbase as usize + k])).collect()
    }

    fn tile_origin(&self, t: &Tile) -> Vec2 {
        let h = &self.header;
        let c = h.tile_cells as f32 * h.cs;
        Vec2::new(h.origin[0] + t.tx as f32 * c, h.origin[1] + t.ty as f32 * c)
    }

    fn detail_verts(&self, r: PolyRef) -> Vec<V3> {
        let t = self.tile_of(r);
        let p = &t.polys[(r & POLY_MASK) as usize];
        let o = self.tile_origin(t);
        (0..p.dvn as usize).map(|k| {
            let i = p.dvbase as usize + k;
            let d = t.dverts[i];
            V3::new(o.x + d[0] as f32 / format::DXY_SCALE, o.y + d[1] as f32 / format::DXY_SCALE, t.zbase + t.dvz[i] as f32 / format::DZ_SCALE - format::DZ_BIAS)
        }).collect()
    }

    fn height_in_poly(&self, r: PolyRef, x: Vec2) -> f32 {
        let t = self.tile_of(r);
        let p = &t.polys[(r & POLY_MASK) as usize];
        let dv = self.detail_verts(r);
        let mut best = (f32::MAX, 0f32);
        for k in 0..p.dtn as usize {
            let tri = t.dtris[p.dtbase as usize + k];
            let (a, b, c) = (dv[tri[0] as usize], dv[tri[1] as usize], dv[tri[2] as usize]);
            let (a2, b2, c2) = (a.truncate(), b.truncate(), c.truncate());
            let den = cross2(a2, b2, c2);
            if den.abs() < 1e-6 { continue; }
            let w1 = cross2(x, b2, c2) / den;
            let w2 = cross2(a2, x, c2) / den;
            let w3 = 1. - w1 - w2;
            let (u, v, w, out) = if w1 >= -1e-4 && w2 >= -1e-4 && w3 >= -1e-4 { (w1, w2, w3, 0.) } else {
                let cands = [closest_on_seg2(x, a2, b2), closest_on_seg2(x, b2, c2), closest_on_seg2(x, c2, a2)];
                let (i, d2) = cands.iter().enumerate().map(|(i, c)| (i, (c.0 - x).length_squared())).min_by(|m, n| m.1.total_cmp(&n.1)).unwrap_or((0, f32::MAX));
                let s = cands[i].1;
                let (u, v, w) = match i { 0 => (1. - s, s, 0.), 1 => (0., 1. - s, s), _ => (s, 0., 1. - s) };
                (u, v, w, d2)
            };
            if out < best.0 {
                best = (out, a.z * u + b.z * v + c.z * w);
                if out == 0. { break; }
            }
        }
        if best.0 < f32::MAX { return best.1; }
        let vs = self.poly_verts(r);
        vs.iter().map(|v| v.z).sum::<f32>() / vs.len().max(1) as f32
    }

    fn inside2(vs: &[V3], x: Vec2) -> bool {
        let n = vs.len();
        (0..n).all(|k| cross2(vs[k].truncate(), vs[(k + 1) % n].truncate(), x) >= -1e-3)
    }

    fn closest_point_on_poly(&self, r: PolyRef, p: V3) -> V3 {
        let vs = self.poly_verts(r);
        let x = p.truncate();
        let q = if Self::inside2(&vs, x) { x } else {
            let n = vs.len();
            (0..n).map(|k| closest_on_seg2(x, vs[k].truncate(), vs[(k + 1) % n].truncate()).0).min_by(|a, b| (*a - x).length_squared().total_cmp(&(*b - x).length_squared())).unwrap_or(x)
        };
        V3::new(q.x, q.y, self.height_in_poly(r, q))
    }

    fn for_each_poly_in(&self, lo: V3, hi: V3, mut f: impl FnMut(PolyRef)) {
        let h = &self.header;
        let tw = h.tile_cells as f32 * h.cs;
        let tx0 = (((lo.x - h.origin[0]) / tw).floor() as i64).max(0);
        let ty0 = (((lo.y - h.origin[1]) / tw).floor() as i64).max(0);
        let tx1 = (((hi.x - h.origin[0]) / tw).floor() as i64).min(h.tiles_x as i64 - 1);
        let ty1 = (((hi.y - h.origin[1]) / tw).floor() as i64).min(h.tiles_y as i64 - 1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let s = (ty * h.tiles_x as i64 + tx) as usize;
                let si = self.slots[s];
                if si == u32::MAX { continue; }
                let t = &self.tiles[si as usize];
                for (pi, p) in t.polys.iter().enumerate() {
                    if p.zmin > hi.z || p.zmax < lo.z { continue; }
                    let mut mn = Vec2::splat(f32::MAX);
                    let mut mx = Vec2::splat(f32::MIN);
                    for k in 0..p.nv as usize { let v = self.vert_world(t, t.poly_verts[p.vbase as usize + k]).truncate(); mn = mn.min(v); mx = mx.max(v); }
                    if mn.x > hi.x || mn.y > hi.y || mx.x < lo.x || mx.y < lo.y { continue; }
                    f(((s as u32) << POLY_BITS) | pi as u32);
                }
            }
        }
    }

    fn nearest_poly(&self, c: V3, ext: V3) -> Option<(PolyRef, V3)> {
        let mut best: Option<(f32, PolyRef, V3)> = None;
        let climb = self.header.agent_climb;
        self.for_each_poly_in(c - ext, c + ext, |r| {
            let q = self.closest_point_on_poly(r, c);
            let d = q - c;
            if d.x.abs() > ext.x || d.y.abs() > ext.y || d.z.abs() > ext.z { return; }
            let inside = d.truncate().length_squared() < 1e-4;
            let score = if inside { (d.z.abs() - climb).max(0.).powi(2) } else { d.length_squared() };
            if best.map_or(true, |b| score < b.0) { best = Some((score, r, q)); }
        });
        best.map(|b| (b.1, b.2))
    }

    fn for_each_link(&self, r: PolyRef, mut f: impl FnMut(PolyRef, u8, u8, u8)) {
        let t = self.tile_of(r);
        let p = &t.polys[(r & POLY_MASK) as usize];
        for k in 0..p.nv as usize {
            let n = t.poly_neis[p.vbase as usize + k];
            if n != NO_NEI && n & EXT_NEI == 0 { f((r & !POLY_MASK) | n as u32, k as u8, 0, 255); }
        }
        for l in &t.links[p.lbase as usize..(p.lbase + p.nl as u32) as usize] { f(l.to, l.edge, l.tmin, l.tmax); }
    }

    fn portal(&self, r: PolyRef, edge: u8, tmin: u8, tmax: u8) -> (V3, V3) {
        let vs = self.poly_verts(r);
        let n = vs.len();
        let (a, b) = (vs[edge as usize % n], vs[(edge as usize + 1) % n]);
        let (ra, rb) = (a.lerp(b, tmin as f32 / 255.), a.lerp(b, tmax as f32 / 255.));
        (rb, ra)
    }

    fn poly_area(&self, r: PolyRef) -> f32 {
        let vs = self.poly_verts(r);
        (1..vs.len().saturating_sub(1)).map(|k| cross2(vs[0].truncate(), vs[k].truncate(), vs[k + 1].truncate()).abs() * 0.5).sum()
    }

    fn random_in_poly(&self, r: PolyRef, rng: &mut u64) -> V3 {
        let vs = self.poly_verts(r);
        let areas: Vec<f32> = (1..vs.len() - 1).map(|k| cross2(vs[0].truncate(), vs[k].truncate(), vs[k + 1].truncate()).abs()).collect();
        let tot: f32 = areas.iter().sum();
        let mut pick = frand(rng) * tot;
        let mut k = 0;
        while k + 1 < areas.len() && pick > areas[k] { pick -= areas[k]; k += 1; }
        let (a, b, c) = (vs[0].truncate(), vs[k + 1].truncate(), vs[k + 2].truncate());
        let (mut u, mut v) = (frand(rng), frand(rng));
        if u + v > 1. { u = 1. - u; v = 1. - v; }
        let x = a + (b - a) * u + (c - a) * v;
        V3::new(x.x, x.y, self.height_in_poly(r, x))
    }

    fn obstacle_box(&self, o: &Obstacle) -> (V3, V3) {
        let r = self.header.agent_radius;
        (o.min - V3::new(r, r, self.header.agent_height), o.max + V3::new(r, r, 0.))
    }

    fn blocked_segment(&self, a: V3, b: V3) -> bool { self.obstacle_hit(a, b).is_some() }

    fn obstacle_hit(&self, a: V3, b: V3) -> Option<V3> {
        let mut best: Option<f32> = None;
        for o in &self.obstacles {
            let (lo, hi) = self.obstacle_box(o);
            if a.z.max(b.z) < lo.z || a.z.min(b.z) > hi.z { continue; }
            if let Some(t) = seg_box2_hit(a.truncate(), b.truncate(), lo.truncate(), hi.truncate()) {
                if best.map_or(true, |x| t < x) { best = Some(t); }
            }
        }
        best.map(|t| a.lerp(b, t))
    }

    fn point_in_obstacle(&self, p: V3) -> bool {
        self.obstacles.iter().any(|o| { let (lo, hi) = self.obstacle_box(o); p.cmpge(lo).all() && p.cmple(hi).all() })
    }

    fn astar(&self, sp: PolyRef, s: V3, ep: PolyRef, e: V3) -> Option<(Vec<PolyRef>, Vec<(u8, u8, u8)>, bool)> {
        if sp == ep { return Some((vec![sp], vec![], false)); }
        let mut nodes: Vec<Node> = Vec::with_capacity(256);
        let mut index: HashMap<PolyRef, u32> = HashMap::new();
        let mut open = BinaryHeap::new();
        let h0 = (s - e).length() * HEURISTIC_SCALE;
        nodes.push(Node { pos: s, cost: 0., total: h0, parent: u32::MAX, poly: sp, edge: 0, tmin: 0, tmax: 0, closed: false });
        index.insert(sp, 0);
        open.push(Open(h0, 0));
        let mut best = (h0, 0u32);
        let mut found = None;
        let limit = self.max_search_nodes.max(2);
        let mut adds: Vec<(PolyRef, u8, u8, u8, V3)> = vec![];
        while let Some(Open(f, ni)) = open.pop() {
            let n = nodes[ni as usize];
            if n.closed || f > n.total + 1e-3 { continue; }
            nodes[ni as usize].closed = true;
            if n.poly == ep { found = Some(ni); break; }
            adds.clear();
            let parent_poly = if n.parent != u32::MAX { nodes[n.parent as usize].poly } else { u32::MAX };
            self.for_each_link(n.poly, |to, edge, tmin, tmax| {
                if to == parent_poly { return; }
                let (l, r) = self.portal(n.poly, edge, tmin, tmax);
                adds.push((to, edge, tmin, tmax, (l + r) * 0.5));
            });
            for &(to, edge, tmin, tmax, mid) in &adds {
                let pos = if to == ep { e } else { mid };
                let step = if to == ep { (mid - n.pos).length() + (e - mid).length() } else { (mid - n.pos).length() };
                if !self.obstacles.is_empty() && (self.blocked_segment(n.pos, mid) || (to == ep && self.blocked_segment(mid, e))) { continue; }
                let cost = n.cost + step;
                let h = if to == ep { 0. } else { (pos - e).length() * HEURISTIC_SCALE };
                let total = cost + h;
                match index.get(&to) {
                    Some(&j) => {
                        let m = &mut nodes[j as usize];
                        if total >= m.total { continue; }
                        *m = Node { pos, cost, total, parent: ni, poly: to, edge, tmin, tmax, closed: false };
                        open.push(Open(total, j));
                        if h < best.0 { best = (h, j); }
                    }
                    None => {
                        if nodes.len() >= limit { continue; }
                        let j = nodes.len() as u32;
                        nodes.push(Node { pos, cost, total, parent: ni, poly: to, edge, tmin, tmax, closed: false });
                        index.insert(to, j);
                        open.push(Open(total, j));
                        if h < best.0 { best = (h, j); }
                    }
                }
            }
        }
        let (last, partial) = match found { Some(f) => (f, false), None => (best.1, true) };
        if partial && last == 0 { return None; }
        let mut polys = vec![];
        let mut edges = vec![];
        let mut k = last;
        while k != u32::MAX {
            let n = &nodes[k as usize];
            polys.push(n.poly);
            if n.parent != u32::MAX { edges.push((n.edge, n.tmin, n.tmax)); }
            k = n.parent;
        }
        polys.reverse();
        edges.reverse();
        Some((polys, edges, partial))
    }

    fn string_pull(&self, polys: &[PolyRef], edges: &[(u8, u8, u8)], s: V3, e: V3) -> Vec<V3> {
        let mut portals: Vec<(V3, V3, PolyRef)> = Vec::with_capacity(polys.len() + 1);
        portals.push((s, s, polys[0]));
        for (i, &(edge, tmin, tmax)) in edges.iter().enumerate() {
            let (l, r) = self.portal(polys[i], edge, tmin, tmax);
            portals.push((l, r, polys[i]));
        }
        portals.push((e, e, *polys.last().unwrap_or(&polys[0])));
        let lift = |pt: V3, k: usize| { let pr = portals[k].2; V3::new(pt.x, pt.y, self.height_in_poly(pr, pt.truncate())) };
        let push = |p: V3, pts: &mut Vec<V3>| { if pts.last().map_or(true, |q: &V3| (q.truncate() - p.truncate()).length_squared() > 1e-4) { pts.push(p); } };
        let mut pts = vec![s];
        let (mut apex, mut left, mut right) = (s.truncate(), s.truncate(), s.truncate());
        let (mut li, mut ri) = (0usize, 0usize);
        let mut i = 1;
        let mut guard = 0;
        while i < portals.len() && guard < 8 * portals.len() + 16 {
            guard += 1;
            let (l, r) = (portals[i].0.truncate(), portals[i].1.truncate());
            if cross2(apex, right, r) >= 0. {
                if apex == right || cross2(apex, left, r) < 0. {
                    right = r;
                    ri = i;
                } else {
                    let ai = li;
                    push(if ai == 0 { s } else { lift(portals[ai].0, ai) }, &mut pts);
                    apex = left;
                    right = apex;
                    ri = ai;
                    i = ai + 1;
                    continue;
                }
            }
            if cross2(apex, left, l) <= 0. {
                if apex == left || cross2(apex, right, l) > 0. {
                    left = l;
                    li = i;
                } else {
                    let ai = ri;
                    push(if ai == 0 { s } else { lift(portals[ai].1, ai) }, &mut pts);
                    apex = right;
                    left = apex;
                    li = ai;
                    i = ai + 1;
                    continue;
                }
            }
            i += 1;
        }
        push(e, &mut pts);
        if pts.len() == 1 { pts.push(e); }
        pts
    }
}

pub fn athena_candidates() -> Vec<PathBuf> {
    let mut c = vec![];
    if let Some(p) = fn_live::var_os("FN_NAV_FILE") { c.push(PathBuf::from(p)); }
    if let Some(p) = fn_live::var_os("FN_ASSETS") { c.push(PathBuf::from(p).join(ATHENA_FILE)); }
    if let Ok(exe) = std::env::current_exe() { if let Some(d) = exe.parent() { c.push(d.join("assets").join(ATHENA_FILE)); } }
    c.push(PathBuf::from("assets").join(ATHENA_FILE));
    c
}

#[cfg(test)]
mod tests;
