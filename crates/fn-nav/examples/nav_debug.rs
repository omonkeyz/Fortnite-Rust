use fn_nav::{NavMesh, V3};
use image::{Rgb, RgbImage};
use std::time::Instant;

struct View { xmax: f32, ymin: f32, s: f32, w: u32, h: u32 }
impl View {
    fn px(&self, p: V3) -> (f32, f32) { ((p.y - self.ymin) / self.s, (self.xmax - p.x) / self.s) }
}

fn ramp(t: f32) -> Rgb<u8> {
    let t = t.clamp(0., 1.);
    let c = [(40., 110., 60.), (120., 170., 70.), (210., 200., 90.), (220., 140., 70.), (235., 235., 235.)];
    let f = t * (c.len() - 1) as f32;
    let i = (f as usize).min(c.len() - 2);
    let u = f - i as f32;
    let (a, b): ((f32, f32, f32), (f32, f32, f32)) = (c[i], c[i + 1]);
    Rgb([(a.0 + (b.0 - a.0) * u) as u8, (a.1 + (b.1 - a.1) * u) as u8, (a.2 + (b.2 - a.2) * u) as u8])
}

fn line(img: &mut RgbImage, a: (f32, f32), b: (f32, f32), r: f32, c: Rgb<u8>) {
    let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) * 2.).ceil().max(1.) as usize;
    for k in 0..=n {
        let t = k as f32 / n as f32;
        let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let ri = r.ceil() as i32;
        for dy in -ri..=ri { for dx in -ri..=ri {
            if (dx * dx + dy * dy) as f32 > r * r { continue; }
            let (px, py) = (x as i32 + dx, y as i32 + dy);
            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() { img.put_pixel(px as u32, py as u32, c); }
        } }
    }
}

fn render(nm: &NavMesh, v: &View, zr: (f32, f32), path: &[V3], edges: bool, out: &str) {
    let mut img = RgbImage::from_pixel(v.w, v.h, Rgb([22, 34, 58]));
    let lo = V3::new(v.xmax - v.h as f32 * v.s, v.ymin, -1e6);
    let hi = V3::new(v.xmax, v.ymin + v.w as f32 * v.s, 1e6);
    let mut polys = nm.polys_in_box(lo, hi);
    polys.sort_by(|a, b| { let za = a.iter().map(|p| p.z).sum::<f32>() / a.len() as f32; let zb = b.iter().map(|p| p.z).sum::<f32>() / b.len() as f32; za.total_cmp(&zb) });
    for p in &polys {
        let q: Vec<(f32, f32)> = p.iter().map(|&x| v.px(x)).collect();
        let z = p.iter().map(|x| x.z).sum::<f32>() / p.len() as f32;
        let col = ramp((z - zr.0) / (zr.1 - zr.0));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in &q { x0 = x0.min(x); y0 = y0.min(y); x1 = x1.max(x); y1 = y1.max(y); }
        let n = q.len();
        let orient = (0..n).map(|k| { let (a, b) = (q[k], q[(k + 1) % n]); a.0 * b.1 - b.0 * a.1 }).sum::<f32>().signum();
        for py in (y0.floor().max(0.) as u32)..=(y1.ceil().min(v.h as f32 - 1.) as u32) {
            for px in (x0.floor().max(0.) as u32)..=(x1.ceil().min(v.w as f32 - 1.) as u32) {
                let c = (px as f32 + 0.5, py as f32 + 0.5);
                let inside = (0..n).all(|k| { let (a, b) = (q[k], q[(k + 1) % n]); ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)) * orient >= 0. });
                if inside { img.put_pixel(px, py, col); }
            }
        }
        if edges { for k in 0..n { line(&mut img, q[k], q[(k + 1) % n], 0.4, Rgb([30, 40, 30])); } }
    }
    for w in path.windows(2) { line(&mut img, v.px(w[0]), v.px(w[1]), 2.5, Rgb([230, 30, 40])); }
    if let (Some(a), Some(b)) = (path.first(), path.last()) {
        line(&mut img, v.px(*a), v.px(*a), 7., Rgb([40, 230, 60]));
        line(&mut img, v.px(*b), v.px(*b), 7., Rgb([40, 120, 255]));
    }
    img.save(out).expect("save");
    println!("wrote {out} ({} polys)", polys.len());
}

fn time<T>(n: usize, mut f: impl FnMut(usize) -> T) -> (f64, Vec<T>) {
    let t = Instant::now();
    let r: Vec<T> = (0..n).map(&mut f).collect();
    (t.elapsed().as_secs_f64() * 1e6 / n as f64, r)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "D:/fn-verify/nav".into());
    let t = Instant::now();
    let path = fn_nav::athena_candidates().into_iter().find(|p| p.exists()).unwrap_or_else(|| "D:/Fortnite/FortniteRust/FortniteRuntime/assets/nav/athena.fnnav".into());
    let bytes = std::fs::read(&path).expect("read");
    let read_ms = t.elapsed().as_secs_f64() * 1000.;
    let t = Instant::now();
    let mut nm = NavMesh::from_bytes(&bytes).expect("decode");
    println!("{}: {} bytes on disk, read {read_ms:.0} ms, decode {:.0} ms, {} tiles, {} polys, {} bytes in memory", path.display(), bytes.len(), t.elapsed().as_secs_f64() * 1000., nm.tile_count(), nm.poly_count(), nm.memory_bytes());
    let salty = nm.project(V3::new(6000., -4800., 1915.), V3::new(100., 100., 150.)).expect("salty");
    let house = nm.project(V3::new(1550., -7205., 2312.), V3::new(100., 100., 150.)).expect("house floor loot");
    let greasy = nm.project(V3::new(1598., -95948., 800.), V3::new(100., 100., 150.)).expect("greasy");
    let in_town = nm.find_path(salty, house).expect("salty street to the house");
    let long = nm.find_path_ex(salty, greasy).expect("salty to greasy");
    nm.max_search_nodes = fn_nav::UE_MAX_SEARCH_NODES;
    let short2048 = nm.find_path_ex(salty, greasy).map(|r| r.partial);
    nm.max_search_nodes = fn_nav::DEFAULT_MAX_SEARCH_NODES;
    println!("salty->house {} pts; salty->greasy {} pts {} polys partial {}; with the 1.8 node limit 2048 partial = {:?}", in_town.len(), long.points.len(), long.polys.len(), long.partial, short2048);
    let pts: Vec<V3> = (0..200).filter_map(|i| nm.random_point_in_radius(salty, 15000., i)).collect();
    let (proj_us, _) = time(2000, |i| nm.project(pts[i % pts.len()] + V3::new(0., 0., 80.), V3::new(68., 68., 144.)));
    let (path_us, _) = time(400, |i| nm.find_path(pts[i % pts.len()], pts[(i * 7 + 3) % pts.len()]));
    let (ray_us, _) = time(2000, |i| nm.raycast(pts[i % pts.len()], pts[(i * 13 + 5) % pts.len()]));
    let (rnd_us, _) = time(2000, |i| nm.random_point_in_radius(pts[i % pts.len()], 2000., i as u64));
    let (fail_us, _) = time(5, |_| nm.find_path(salty, V3::new(4600., -13800., 3702.)));
    let (long_us, _) = time(20, |_| nm.find_path(salty, greasy));
    println!("timings (us/query, native release): project {proj_us:.1}, find_path in Salty (<150 m) {path_us:.1}, raycast {ray_us:.1}, random_point r=20m {rnd_us:.1}, Salty->Greasy {long_us:.0}, unreachable roof target (component check) {fail_us:.0}");
    let mut both = in_town.clone();
    both.extend(long.points.iter().copied().take_while(|p| p.x > -1500. && p.y > -15000.));
    render(&nm, &View { xmax: 10500., ymin: -15000., s: 5., w: 2600, h: 2400 }, (1400., 4200.), &in_town, true, &format!("{out}/nav_salty.png"));
    render(&nm, &View { xmax: 10500., ymin: -15000., s: 5., w: 2600, h: 2400 }, (1400., 4200.), &long.points, true, &format!("{out}/nav_salty_to_greasy_start.png"));
    render(&nm, &View { xmax: 153000., ymin: -153000., s: 120., w: 2550, h: 2550 }, (-5000., 8000.), &long.points, false, &format!("{out}/nav_map_salty_to_greasy.png"));
}
