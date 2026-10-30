use fn_nav::format::{self, Header, Link, Poly, Tile, NO_NEI};
use glam::{Quat, Vec2, Vec3};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const CELL_SIZE: f32 = 17.;
const CELL_HEIGHT: f32 = 16.;
const TILE_SIZE_UU: f32 = 1024.;
const AGENT_RADIUS: f32 = 34.;
const AGENT_HEIGHT: f32 = 144.;
const AGENT_MAX_STEP: f32 = 45.;
const AGENT_MAX_SLOPE: f32 = 44.51;
const MIN_REGION_AREA_UU: f32 = 0.;
const MERGE_REGION_SIZE_UU: f32 = 340.;
const MAX_SIMPLIFICATION_ERROR: f32 = 0.99;
const MAX_VERTS_PER_POLY: u16 = 6;
const DETAIL_SAMPLE_DIST: f32 = 600.;
const DETAIL_SAMPLE_MAX_ERROR: f32 = 1.;

struct Cfg {
    tile_cells: u16,
    walkable_height: u16,
    walkable_climb: u16,
    walkable_radius: u16,
    border: u16,
    max_edge_len: u16,
    min_region_area: u16,
    merge_region_area: u16,
    slope_cos: f32,
    kill_z: f32,
}

impl Cfg {
    fn ue416() -> Cfg {
        let walkable_radius = (AGENT_RADIUS / CELL_SIZE).ceil() as u16;
        Cfg {
            tile_cells: (TILE_SIZE_UU / CELL_SIZE).trunc() as u16,
            walkable_height: (AGENT_HEIGHT / CELL_HEIGHT).ceil() as u16,
            walkable_climb: (AGENT_MAX_STEP / CELL_HEIGHT).ceil() as u16,
            walkable_radius,
            border: walkable_radius + 3,
            max_edge_len: (1200. / CELL_SIZE) as u16,
            min_region_area: (MIN_REGION_AREA_UU / CELL_SIZE).powi(2) as u16,
            merge_region_area: (MERGE_REGION_SIZE_UU / CELL_SIZE).powi(2) as u16,
            slope_cos: AGENT_MAX_SLOPE.to_radians().cos(),
            kill_z: -5000.,
        }
    }
}

static DROPPED: AtomicUsize = AtomicUsize::new(0);
static CLAMPED: AtomicUsize = AtomicUsize::new(0);
static UNSET: AtomicUsize = AtomicUsize::new(0);

struct MeshCol { tris: Vec<[Vec3; 3]>, two_sided: Vec<bool>, min: Vec3, max: Vec3 }
struct Inst { mesh: Arc<MeshCol>, q: Quat, s: Vec3, t: Vec3, min: Vec3, max: Vec3 }

impl Inst {
    fn new(mesh: Arc<MeshCol>, q: Quat, s: Vec3, t: Vec3) -> Inst {
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for i in 0..8 {
            let c = Vec3::new(if i & 1 == 0 { mesh.min.x } else { mesh.max.x }, if i & 2 == 0 { mesh.min.y } else { mesh.max.y }, if i & 4 == 0 { mesh.min.z } else { mesh.max.z });
            let w = t + q * (s * c);
            min = min.min(w);
            max = max.max(w);
        }
        Inst { mesh, q, s, t, min, max }
    }
    fn world(&self, p: Vec3) -> Vec3 { self.t + self.q * (self.s * p) }
}

fn f(v: &Value) -> Option<f32> { v.as_f64().map(|x| x as f32) }
fn v3(v: &Value) -> Option<Vec3> { let a = v.as_array()?; Some(Vec3::new(f(a.first()?)?, f(a.get(1)?)?, f(a.get(2)?)?)) }
fn quat(v: &Value) -> Option<Quat> { let a = v.as_array()?; Some(Quat::from_xyzw(f(a.first()?)?, f(a.get(1)?)?, f(a.get(2)?)?, f(a.get(3)?)?).normalize()) }

fn rotator_quat(pitch: f32, yaw: f32, roll: f32) -> Quat {
    let (sp, cp) = (pitch.to_radians() * 0.5).sin_cos();
    let (sy, cy) = (yaw.to_radians() * 0.5).sin_cos();
    let (sr, cr) = (roll.to_radians() * 0.5).sin_cos();
    Quat::from_xyzw(cr * sp * sy - sr * cp * cy, -cr * sp * cy - sr * cp * sy, cr * cp * sy - sr * sp * cy, cr * cp * cy + sr * sp * sy).normalize()
}

fn hull_tris(pts: &[Vec3], tris: &mut Vec<[Vec3; 3]>, two: &mut Vec<bool>) {
    if let Some(h) = fn_hull::Hull::from_points(pts) {
        for t in h.tris.iter() {
            tris.push([h.verts[t[0] as usize], h.verts[t[1] as usize], h.verts[t[2] as usize]]);
            two.push(false);
        }
    }
}

fn mesh_from_json(m: &Value) -> Option<MeshCol> {
    if m.get("collision_enabled").and_then(|v| v.as_str()) == Some("ECollisionEnabled::NoCollision") { return None; }
    let mut tris = vec![];
    let mut two = vec![];
    if let Some(c) = m.get("complex").filter(|c| c.get("positions").is_some()) {
        let pos: Vec<Vec3> = c["positions"].as_array()?.iter().filter_map(v3).collect();
        let idx: Vec<usize> = c["indices"].as_array()?.iter().filter_map(|v| v.as_u64().map(|x| x as usize)).collect();
        for t in idx.chunks_exact(3) {
            if t.iter().all(|&i| i < pos.len()) { tris.push([pos[t[0]], pos[t[1]], pos[t[2]]]); two.push(true); }
        }
    } else {
        for s in m.get("shapes").and_then(|v| v.as_array()).into_iter().flatten() {
            match s.get("type").and_then(|v| v.as_str()) {
                Some("convex") => {
                    let tr = &s["transform"];
                    let (r, sc, t) = (quat(&tr["rotation"]).unwrap_or(Quat::IDENTITY), v3(&tr["scale"]).unwrap_or(Vec3::ONE), v3(&tr["translation"]).unwrap_or(Vec3::ZERO));
                    let pts: Vec<Vec3> = s["vertices"].as_array().into_iter().flatten().filter_map(v3).map(|p| t + r * (sc * p)).collect();
                    hull_tris(&pts, &mut tris, &mut two);
                }
                Some("box") => {
                    let (c, he, r) = (v3(&s["center"])?, v3(&s["half_extent"])?, quat(&s["rotation"]).unwrap_or(Quat::IDENTITY));
                    let pts: Vec<Vec3> = (0..8).map(|i| c + r * Vec3::new(if i & 1 == 0 { -he.x } else { he.x }, if i & 2 == 0 { -he.y } else { he.y }, if i & 4 == 0 { -he.z } else { he.z })).collect();
                    hull_tris(&pts, &mut tris, &mut two);
                }
                _ => {}
            }
        }
    }
    if tris.is_empty() { return None; }
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for t in &tris { for p in t { min = min.min(*p); max = max.max(*p); } }
    Some(MeshCol { tris, two_sided: two, min, max })
}

fn read_json(p: &Path) -> Option<Value> { serde_json::from_slice(&std::fs::read(p).ok()?).ok() }

fn load_instances(props: &Path) -> (Vec<Inst>, HashMap<String, usize>) {
    let mut cache: HashMap<String, Option<Arc<MeshCol>>> = HashMap::new();
    let mut out = vec![];
    let mut stats: HashMap<String, usize> = HashMap::new();
    let index = read_json(&props.join("poi/index.json")).expect("poi/index.json");
    for l in index["levels"].as_array().into_iter().flatten() {
        let Some(file) = l["file"].as_str() else { continue };
        let Some(d) = read_json(&props.join("poi").join(file)) else { eprintln!("skip {file}"); continue };
        let meshes = &d["meshes"];
        for p in d["placements"].as_array().into_iter().flatten() {
            if p["collide"].as_bool() == Some(false) || p["profile"].as_str() == Some("NoCollision") { *stats.entry("poi_no_collision".into()).or_default() += 1; continue; }
            let Some(key) = p["mesh"].as_str() else { continue };
            let mesh = cache.entry(key.to_string()).or_insert_with(|| meshes.get(key).and_then(mesh_from_json).map(Arc::new)).clone();
            let Some(mesh) = mesh else { *stats.entry("poi_no_shape".into()).or_default() += 1; continue };
            let (Some(q), Some(s), Some(t)) = (quat(&p["q"]), v3(&p["s"]), v3(&p["t"])) else { continue };
            out.push(Inst::new(mesh, q, s, t));
            *stats.entry("poi".into()).or_default() += 1;
        }
    }
    if let Some(all) = read_json(&props.join("layout/all.json")) {
        let classes: Vec<String> = all["classes"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect();
        for row in all["instances"].as_array().into_iter().flatten() {
            let Some(r) = row.as_array() else { continue };
            let Some(cls) = r.first().and_then(|v| v.as_u64()).and_then(|i| classes.get(i as usize)) else { continue };
            let key = format!("layout:{cls}");
            let mesh = cache.entry(key).or_insert_with(|| read_json(&props.join(cls).join("collision.json")).and_then(|m| mesh_from_json(&m)).map(Arc::new)).clone();
            let Some(mesh) = mesh else { *stats.entry("layout_no_collision".into()).or_default() += 1; continue };
            let g = |i: usize| r.get(i).and_then(f).unwrap_or(0.);
            out.push(Inst::new(mesh, rotator_quat(g(4), g(5), g(6)), Vec3::new(g(7), g(8), g(9)), Vec3::new(g(1), g(2), g(3))));
            *stats.entry("layout".into()).or_default() += 1;
        }
    }
    stats.insert("meshes".into(), cache.values().filter(|m| m.is_some()).count());
    (out, stats)
}

struct World { hf: fn_terrain::Heightfield, insts: Vec<Inst>, buckets: Vec<Vec<u32>>, origin: Vec2, tiles_x: usize, tile_w: f32, pad: f32 }

fn tri_area(a: Vec3, b: Vec3, c: Vec3, two_sided: bool, flip: bool, cfg: &Cfg) -> rerecast::AreaType {
    let mut n = (b - a).cross(c - a);
    if flip { n = -n; }
    let l = n.length();
    if l < 1e-6 { return rerecast::AreaType::NOT_WALKABLE; }
    let nz = n.z / l;
    let up = if two_sided { nz.abs() } else { nz };
    let cz = (a.z + b.z + c.z) / 3.;
    if up >= cfg.slope_cos && cz >= cfg.kill_z { rerecast::AreaType::DEFAULT_WALKABLE } else { rerecast::AreaType::NOT_WALKABLE }
}

fn build_tile(w: &World, cfg: &Cfg, tx: usize, ty: usize) -> Result<Option<Tile>, String> {
    use glam33::{UVec3, Vec3A};
    let o = w.origin + Vec2::new(tx as f32, ty as f32) * w.tile_w;
    let lo = o - Vec2::splat(w.pad);
    let hi = o + Vec2::splat(w.tile_w + w.pad);
    let mut verts: Vec<Vec3A> = vec![];
    let mut idx: Vec<UVec3> = vec![];
    let mut areas = vec![];
    let mut any_walk = false;
    let (mut zmin, mut zmax) = (f32::MAX, f32::MIN);
    let mut push = |t: [Vec3; 3], a: rerecast::AreaType, verts: &mut Vec<Vec3A>, idx: &mut Vec<UVec3>, areas: &mut Vec<rerecast::AreaType>| {
        let tmin = t[0].min(t[1]).min(t[2]);
        let tmax = t[0].max(t[1]).max(t[2]);
        if tmax.x < lo.x || tmax.y < lo.y || tmin.x > hi.x || tmin.y > hi.y { return; }
        let b = verts.len() as u32;
        for p in t { verts.push(Vec3A::new(p.x, p.z, p.y)); }
        idx.push(UVec3::new(b, b + 1, b + 2));
        if a.is_walkable() { any_walk = true; }
        areas.push(a);
        zmin = zmin.min(tmin.z);
        zmax = zmax.max(tmax.z);
    };
    w.hf.triangles_in(lo, hi, |t| { let a = tri_area(t[0], t[1], t[2], true, false, cfg); push(t, a, &mut verts, &mut idx, &mut areas); });
    for &ii in &w.buckets[ty * w.tiles_x + tx] {
        let inst = &w.insts[ii as usize];
        let flip = inst.s.x * inst.s.y * inst.s.z < 0.;
        for (k, t) in inst.mesh.tris.iter().enumerate() {
            let wt = [inst.world(t[0]), inst.world(t[1]), inst.world(t[2])];
            let a = tri_area(wt[0], wt[1], wt[2], inst.mesh.two_sided[k], flip, cfg);
            push(wt, a, &mut verts, &mut idx, &mut areas);
        }
    }
    if !any_walk { return Ok(None); }
    let tr = std::env::var_os("NAV_TRACE").is_some();
    let t0 = std::time::Instant::now();
    let ph = |n: &str| if tr { eprintln!("  {n} {:.3}s tris {}", t0.elapsed().as_secs_f32(), 0) };
    let zmin = zmin.max(cfg.kill_z - 2000.) - CELL_HEIGHT;
    let zmax = zmax.min(zmin + 60000.) + CELL_HEIGHT;
    let n = cfg.tile_cells as f32 + cfg.border as f32 * 2.;
    let aabb = rerecast::Aabb3d { min: glam33::Vec3::new(lo.x, zmin, lo.y), max: glam33::Vec3::new(lo.x + n * CELL_SIZE, zmax, lo.y + n * CELL_SIZE) };
    let trimesh = rerecast::TriMesh { vertices: verts, indices: idx, area_types: areas };
    ph("gather");
    let mut hf = rerecast::HeightfieldBuilder { aabb, cell_size: CELL_SIZE, cell_height: CELL_HEIGHT }.build().map_err(|e| e.to_string())?;
    hf.rasterize_triangles(&trimesh, cfg.walkable_climb).map_err(|e| e.to_string())?;
    ph("raster");
    hf.filter_low_hanging_walkable_obstacles(cfg.walkable_climb);
    hf.filter_ledge_spans(cfg.walkable_height, cfg.walkable_climb);
    hf.filter_walkable_low_height_spans(cfg.walkable_height);
    ph("filter");
    let mut chf = hf.into_compact(cfg.walkable_height, cfg.walkable_climb).map_err(|e| e.to_string())?;
    chf.erode_walkable_area(cfg.walkable_radius);
    ph("compact+erode");
    chf.build_distance_field();
    chf.build_regions(cfg.border, cfg.min_region_area, cfg.merge_region_area).map_err(|e| e.to_string())?;
    ph("regions");
    let cset = chf.build_contours(MAX_SIMPLIFICATION_ERROR, cfg.max_edge_len, rerecast::BuildContoursFlags::default());
    ph("contours");
    let pm = match cset.clone().into_polygon_mesh(MAX_VERTS_PER_POLY) {
        Ok(pm) => pm,
        Err(e) if e.to_string().contains("Invalid contour") => {
            let mut good = cset.clone();
            good.contours.retain(|c| { let mut one = cset.clone(); one.contours = vec![c.clone()]; one.into_polygon_mesh(MAX_VERTS_PER_POLY).is_ok() });
            DROPPED.fetch_add(cset.contours.len() - good.contours.len(), Ordering::Relaxed);
            good.into_polygon_mesh(MAX_VERTS_PER_POLY).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(e.to_string()),
    };
    ph("polymesh");
    if pm.polygon_count() == 0 { return Ok(None); }
    if pm.polygon_count() > fn_nav::POLY_MASK as usize { return Err(format!("{} polys in one tile", pm.polygon_count())); }
    let dm = rerecast::DetailNavmesh::new(&pm, &chf, DETAIL_SAMPLE_DIST, DETAIL_SAMPLE_MAX_ERROR).map_err(|e| e.to_string())?;
    ph("detail");
    let offx = ((pm.aabb.min.x - o.x) / CELL_SIZE).round() as i32;
    let offy = ((pm.aabb.min.z - o.y) / CELL_SIZE).round() as i32;
    let zbase = pm.aabb.min.y;
    let mut t = Tile { tx: tx as u16, ty: ty as u16, zbase, ..Default::default() };
    for v in &pm.vertices {
        let (x, y) = (v.x as i32 + offx, v.z as i32 + offy);
        if x < 0 || y < 0 || x > cfg.tile_cells as i32 || y > cfg.tile_cells as i32 { return Err(format!("vertex off tile ({x}, {y})")); }
        t.verts.push([x as u16, y as u16, v.y]);
    }
    let nvp = pm.max_vertices_per_polygon as usize;
    let npolys = pm.polygon_count();
    let mut polys_v: Vec<Vec<u16>> = vec![];
    let mut polys_n: Vec<Vec<u16>> = vec![];
    for i in 0..npolys {
        let vs: Vec<u16> = pm.polygons[i * nvp..(i + 1) * nvp].iter().copied().take_while(|&v| v != rerecast::PolygonNavmesh::NO_INDEX).collect();
        let ns: Vec<u16> = (0..vs.len()).map(|j| { let nb = pm.polygon_neighbors[i * nvp + j]; if (nb as usize) < npolys { nb } else { NO_NEI } }).collect();
        polys_v.push(vs);
        polys_n.push(ns);
    }
    let signed = |vs: &[u16]| -> f32 {
        let p: Vec<Vec2> = vs.iter().map(|&v| Vec2::new(t.verts[v as usize][0] as f32, t.verts[v as usize][1] as f32)).collect();
        (0..p.len()).map(|k| p[k].perp_dot(p[(k + 1) % p.len()])).sum()
    };
    let total: f32 = polys_v.iter().map(|vs| signed(vs)).sum();
    if total < 0. {
        for (vs, ns) in polys_v.iter_mut().zip(polys_n.iter_mut()) {
            let n = vs.len();
            let nv: Vec<u16> = (0..n).map(|k| vs[(n - k) % n]).collect();
            let nn: Vec<u16> = (0..n).map(|k| ns[(n - 1 - k) % n]).collect();
            *vs = nv;
            *ns = nn;
        }
    }
    for i in 0..npolys {
        let sm = &dm.meshes[i];
        let p = Poly { vbase: t.poly_verts.len() as u32, nv: polys_v[i].len() as u8, area: 63, dvbase: t.dverts.len() as u32, dvn: sm.vertex_count as u16, dtbase: t.dtris.len() as u32, dtn: sm.triangle_count as u16, ..Default::default() };
        if sm.vertex_count > 255 || sm.triangle_count > 255 { return Err("detail submesh too large".into()); }
        t.poly_verts.extend_from_slice(&polys_v[i]);
        t.poly_neis.extend_from_slice(&polys_n[i]);
        for k in 0..sm.vertex_count as usize {
            let v = dm.vertices[sm.base_vertex_index as usize + k];
            let (x, y, mut z) = (v.x - o.x, v.z - o.y, v.y);
            if !(z > zbase - format::DZ_BIAS && z < zmax + 1000.) {
                UNSET.fetch_add(1, Ordering::Relaxed);
                z = polys_v[i].iter().map(|&pv| { let q = t.verts[pv as usize]; (Vec2::new(q[0] as f32 * CELL_SIZE - x, q[1] as f32 * CELL_SIZE - y).length_squared(), zbase + q[2] as f32 * CELL_HEIGHT) }).min_by(|a, b| a.0.total_cmp(&b.0)).map_or(zbase, |m| m.1);
            }
            let (qx, qy, qz) = ((x * format::DXY_SCALE).round(), (y * format::DXY_SCALE).round(), ((z - zbase + format::DZ_BIAS) * format::DZ_SCALE).round());
            if !(-32768. ..=32767.).contains(&qx) || !(-32768. ..=32767.).contains(&qy) || !(0. ..=65535.).contains(&qz) { CLAMPED.fetch_add(1, Ordering::Relaxed); eprintln!("clamp tile {tx},{ty}: local ({x:.0}, {y:.0}) z {z:.0} zbase {zbase:.0}"); }
            t.dverts.push([qx.clamp(-32768., 32767.) as i16, qy.clamp(-32768., 32767.) as i16]);
            t.dvz.push(qz.clamp(0., 65535.) as u16);
        }
        for k in 0..sm.triangle_count as usize {
            let d = dm.triangles[sm.base_triangle_index as usize + k];
            t.dtris.push([d[0], d[1], d[2]]);
        }
        t.polys.push(p);
    }
    Ok(Some(t))
}

fn stitch(tiles: &mut [Tile], slots: &[u32], tiles_x: usize, tiles_y: usize, tc: u16) -> usize {
    let climb = AGENT_MAX_STEP + CELL_HEIGHT;
    let mut all_links: Vec<Vec<Link>> = vec![vec![]; tiles.len()];
    for (ti, t) in tiles.iter().enumerate() {
        for (pi, p) in t.polys.iter().enumerate() {
            for j in 0..p.nv as usize {
                if t.poly_neis[p.vbase as usize + j] != NO_NEI { continue; }
                let va = t.verts[t.poly_verts[p.vbase as usize + j] as usize];
                let vb = t.verts[t.poly_verts[p.vbase as usize + (j + 1) % p.nv as usize] as usize];
                let (side, ntx, nty): (u8, i64, i64) = if va[0] == 0 && vb[0] == 0 { (0, t.tx as i64 - 1, t.ty as i64) }
                    else if va[0] == tc && vb[0] == tc { (1, t.tx as i64 + 1, t.ty as i64) }
                    else if va[1] == 0 && vb[1] == 0 { (2, t.tx as i64, t.ty as i64 - 1) }
                    else if va[1] == tc && vb[1] == tc { (3, t.tx as i64, t.ty as i64 + 1) }
                    else { continue };
                if ntx < 0 || nty < 0 || ntx >= tiles_x as i64 || nty >= tiles_y as i64 { continue; }
                let nslot = nty as usize * tiles_x + ntx as usize;
                let ni = slots[nslot];
                if ni == u32::MAX { continue; }
                let nt = &tiles[ni as usize];
                let along = |v: [u16; 3]| if side < 2 { v[1] as f32 } else { v[0] as f32 };
                let (opp_val, opp_axis) = match side { 0 => (tc, 0), 1 => (0, 0), 2 => (tc, 1), _ => (0, 1) };
                let (ua, ub) = (along(va), along(vb));
                if (ub - ua).abs() < 1e-6 { continue; }
                let za = t.zbase + va[2] as f32 * CELL_HEIGHT;
                let zb = t.zbase + vb[2] as f32 * CELL_HEIGHT;
                for (npi, np) in nt.polys.iter().enumerate() {
                    for k in 0..np.nv as usize {
                        if nt.poly_neis[np.vbase as usize + k] != NO_NEI { continue; }
                        let wa = nt.verts[nt.poly_verts[np.vbase as usize + k] as usize];
                        let wb = nt.verts[nt.poly_verts[np.vbase as usize + (k + 1) % np.nv as usize] as usize];
                        if wa[opp_axis] != opp_val || wb[opp_axis] != opp_val { continue; }
                        let (wua, wub) = (along(wa), along(wb));
                        let lo = ua.min(ub).max(wua.min(wub));
                        let hi = ua.max(ub).min(wua.max(wub));
                        if hi - lo < 0.01 { continue; }
                        let wza = nt.zbase + wa[2] as f32 * CELL_HEIGHT;
                        let wzb = nt.zbase + wb[2] as f32 * CELL_HEIGHT;
                        let z_this = |u: f32| za + (zb - za) * (u - ua) / (ub - ua);
                        let z_that = |u: f32| if (wub - wua).abs() < 1e-6 { wza } else { wza + (wzb - wza) * (u - wua) / (wub - wua) };
                        if (z_this(lo) - z_that(lo)).abs() > climb || (z_this(hi) - z_that(hi)).abs() > climb { continue; }
                        let (t0, t1) = ((lo - ua) / (ub - ua), (hi - ua) / (ub - ua));
                        let (t0, t1) = (t0.min(t1).clamp(0., 1.), t0.max(t1).clamp(0., 1.));
                        all_links[ti].push(Link { poly: pi as u16, edge: j as u8, tmin: (t0 * 255.).floor() as u8, tmax: (t1 * 255.).ceil() as u8, to: ((nslot as u32) << fn_nav::POLY_BITS) | npi as u32 });
                    }
                }
            }
        }
    }
    let mut n = 0;
    for (t, l) in tiles.iter_mut().zip(all_links) { n += l.len(); t.links = l; t.finish(CELL_HEIGHT); }
    n
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let assets = PathBuf::from(arg("--assets").or_else(|| std::env::var("FN_ASSETS").ok()).unwrap_or_else(|| "D:/Fortnite/FortniteRust/FortniteRuntime/assets".into()));
    let out = PathBuf::from(arg("--out").unwrap_or_else(|| assets.join(fn_nav::ATHENA_FILE).to_string_lossy().into_owned()));
    let threads: usize = arg("--threads").and_then(|v| v.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(1)));
    let only: Option<[f32; 4]> = arg("--only").map(|s| { let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect(); [v[0], v[1], v[2], v[3]] });
    let props = assets.join("props");
    let mut cfg = Cfg::ue416();
    if let Some(b) = read_json(&props.join("backdrop/backdrop.json")) { if let Some(k) = f(&b["world_settings"]["KillZ"]) { cfg.kill_z = k; } }
    let t0 = std::time::Instant::now();
    let tb = std::fs::read(props.join("backdrop/terrain.bin")).expect("terrain.bin");
    let hf = fn_terrain::Heightfield::new(fn_terrain::parse_terrain(&tb).expect("terrain")).expect("heightfield");
    let (insts, stats) = load_instances(&props);
    eprintln!("loaded terrain {:?}..{:?}, {} instances {:?} in {:.1}s", hf.min, hf.max, insts.len(), stats, t0.elapsed().as_secs_f32());
    let tile_w = cfg.tile_cells as f32 * CELL_SIZE;
    let origin = hf.min;
    let tiles_x = ((hf.max.x - hf.min.x) / tile_w).ceil() as usize;
    let tiles_y = ((hf.max.y - hf.min.y) / tile_w).ceil() as usize;
    assert!(tiles_x * tiles_y <= 1 << 20, "{tiles_x}x{tiles_y} tiles");
    let pad = cfg.border as f32 * CELL_SIZE;
    let mut buckets: Vec<Vec<u32>> = vec![vec![]; tiles_x * tiles_y];
    for (i, inst) in insts.iter().enumerate() {
        let x0 = (((inst.min.x - pad - origin.x) / tile_w).floor() as i64).max(0);
        let y0 = (((inst.min.y - pad - origin.y) / tile_w).floor() as i64).max(0);
        let x1 = (((inst.max.x + pad - origin.x) / tile_w).floor() as i64).min(tiles_x as i64 - 1);
        let y1 = (((inst.max.y + pad - origin.y) / tile_w).floor() as i64).min(tiles_y as i64 - 1);
        for y in y0..=y1 { for x in x0..=x1 { buckets[y as usize * tiles_x + x as usize].push(i as u32); } }
    }
    let world = World { hf, insts, buckets, origin, tiles_x, tile_w, pad };
    let mut todo: Vec<(usize, usize)> = vec![];
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            if let Some(r) = only {
                let o = origin + Vec2::new(tx as f32, ty as f32) * tile_w;
                if o.x + tile_w < r[0] || o.y + tile_w < r[1] || o.x > r[2] || o.y > r[3] { continue; }
            }
            todo.push((tx, ty));
        }
    }
    eprintln!("cfg: tile {} cells ({} cm), {}x{} tiles, {} to build, walkable h {} climb {} radius {} border {} edge {} merge {} slope cos {:.4} killz {}, {threads} threads", cfg.tile_cells, tile_w, tiles_x, tiles_y, todo.len(), cfg.walkable_height, cfg.walkable_climb, cfg.walkable_radius, cfg.border, cfg.max_edge_len, cfg.merge_region_area, cfg.slope_cos, cfg.kill_z);
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Tile>> = Mutex::new(vec![]);
    let errors = AtomicUsize::new(0);
    let t1 = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= todo.len() { break; }
                if i % 2000 == 0 { eprintln!("  tile {i}/{} ({:.0}s)", todo.len(), t1.elapsed().as_secs_f32()); }
                let (tx, ty) = todo[i];
                let tt = std::time::Instant::now();
                if std::env::var_os("NAV_TRACE").is_some() { eprintln!("start {tx},{ty}"); }
                let r = build_tile(&world, &cfg, tx, ty);
                if tt.elapsed().as_secs_f32() > 2. { eprintln!("slow tile {tx},{ty}: {:.1}s", tt.elapsed().as_secs_f32()); }
                match r {
                    Ok(Some(t)) => results.lock().unwrap().push(t),
                    Ok(None) => {}
                    Err(e) => { errors.fetch_add(1, Ordering::Relaxed); eprintln!("tile {tx},{ty}: {e}"); }
                }
            });
        }
    });
    let mut tiles = results.into_inner().unwrap();
    tiles.sort_by_key(|t| (t.ty, t.tx));
    let mut slots = vec![u32::MAX; tiles_x * tiles_y];
    for (i, t) in tiles.iter().enumerate() { slots[t.ty as usize * tiles_x + t.tx as usize] = i as u32; }
    let nlinks = stitch(&mut tiles, &slots, tiles_x, tiles_y, cfg.tile_cells);
    let header = Header { cs: CELL_SIZE, ch: CELL_HEIGHT, tile_cells: cfg.tile_cells as u32, origin: [origin.x, origin.y], tiles_x: tiles_x as u32, tiles_y: tiles_y as u32, agent_radius: AGENT_RADIUS, agent_height: AGENT_HEIGHT, agent_climb: AGENT_MAX_STEP, kill_z: cfg.kill_z };
    let bytes = format::encode(&header, &tiles);
    if let Some(d) = out.parent() { std::fs::create_dir_all(d).expect("out dir"); }
    std::fs::write(&out, &bytes).expect("write");
    let polys: usize = tiles.iter().map(|t| t.polys.len()).sum();
    let dtris: usize = tiles.iter().map(|t| t.dtris.len()).sum();
    let nm = fn_nav::NavMesh::from_bytes(&bytes).expect("reload");
    eprintln!("built {} tiles ({} errors), {polys} polys, {dtris} detail tris, {nlinks} tile links, {} bad contours dropped, {} detail vertices clamped, {} unset detail heights replaced in {:.1}s; wrote {} ({} bytes); runtime memory {} bytes", tiles.len(), errors.load(Ordering::Relaxed), DROPPED.load(Ordering::Relaxed), CLAMPED.load(Ordering::Relaxed), UNSET.load(Ordering::Relaxed), t1.elapsed().as_secs_f32(), out.display(), bytes.len(), nm.memory_bytes());
}
