//! Verification diagram (core agent): places a small build through the real preview / query path (cooked catalog) and draws the
//! pieces' cooked collision convexes in a top view and a side view, over the 512 / 384 grid.
//! `cargo run -p fn-building --example core_diagram -- <install or paks dir> <out.png>`
use fn_building::grid::{self, Rot};
use fn_building::pieces::{Material, PieceCatalog, PieceKind};
use fn_building::placement::{self, BuildPreview, PlacementEnv, QueryResult, ViewInput};
use fn_building::world::BuildingWorld;
use glam::Vec3;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let set = fn_pak::PakSet::open_install(&a[1]).expect("paks");
    let cat = PieceCatalog::load(&set).expect("catalog");
    let mut world = BuildingWorld::new();
    let env = PlacementEnv { ground_z: Some(0.), grid_origin: Vec3::ZERO, ..Default::default() };
    let mut p = BuildPreview::default();
    // a player at the origin builds: wall ahead, floor under, ramp ahead (stone), then walks up and builds a floor + metal wall + cone
    let mut pawn = Vec3::new(100., 60., 90.);
    let mut log = vec![];
    let mut build = |world: &mut BuildingWorld, p: &mut BuildPreview, pawn: Vec3, kind, mat, pitch: f32, yaw: f32| {
        p.set_piece(kind); p.material = mat;
        let cam = pawn + grid::rotate_yaw(Vec3::new(-250., 60., 60.), yaw);
        let vi = ViewInput { camera: cam, view: Rot { pitch, yaw, roll: 0. }, pawn };
        let r = p.update(world, &cat, &env, &vi);
        let res = placement::place(world, &cat, &env, p, 999);
        log.push(format!("{kind:?} {mat:?} pitch {pitch} yaw {yaw}: preview {:?} at {:?} yaw {} -> {:?}", r, p.loc, p.rot.yaw, res.map(|x| x.0)));
    };
    build(&mut world, &mut p, pawn, PieceKind::Floor, Material::Wood, -80., 0.);
    build(&mut world, &mut p, pawn, PieceKind::Stairs, Material::Stone, -10., 0.);
    build(&mut world, &mut p, pawn, PieceKind::Wall, Material::Wood, -10., 90.);
    pawn = Vec3::new(700., 60., 384. + 90.); // on top of the ramp
    build(&mut world, &mut p, pawn, PieceKind::Floor, Material::Wood, -10., 0.);
    build(&mut world, &mut p, pawn, PieceKind::Wall, Material::Metal, -5., 0.);
    build(&mut world, &mut p, pawn, PieceKind::Roof, Material::Wood, 30., 0.);
    for l in &log { println!("{l}"); }
    for q in world.iter() { println!("#{} {:?} {:?} loc {:?} yaw {} health {}/{} key {:?}", q.id, q.kind, q.material, q.location, q.yaw_deg, q.health, q.max_health, q.key); }
    let _ = QueryResult::CanAdd;

    // drawing: left = top view (X right, Y up), right = side view (X right, Z up); 1 px = 4 cm
    let (w, h) = (1200u32, 600u32);
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([250, 250, 250]));
    let s = 0.25f32;
    let top = |v: Vec3| (140. + v.x * s, 300. - v.y * s);
    let side = |v: Vec3| (900. + (v.x - 512.) * s, 520. - v.z * s);
    let line = |img: &mut image::RgbImage, a: (f32, f32), b: (f32, f32), c: [u8; 3]| {
        let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) as i32).max(1);
        for i in 0..=n { let t = i as f32 / n as f32; let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t); if x >= 0. && y >= 0. && (x as u32) < w && (y as u32) < h { img.put_pixel(x as u32, y as u32, image::Rgb(c)); } }
    };
    // grid: cell edges at 256 + 512 k (top), levels at 384 k (side)
    for k in -2..=3 { let x = -256. + 512. * k as f32; line(&mut img, top(Vec3::new(x, -768., 0.)), top(Vec3::new(x, 768., 0.)), [200, 200, 200]); line(&mut img, top(Vec3::new(-512., x, 0.)), top(Vec3::new(1792., x, 0.)), [200, 200, 200]); line(&mut img, side(Vec3::new(x, 0., -100.)), side(Vec3::new(x, 0., 1300.)), [200, 200, 200]); }
    for k in 0..=3 { let z = 384. * k as f32; line(&mut img, side(Vec3::new(-1000., 0., z)), side(Vec3::new(2000., 0., z)), if k == 0 { [120, 160, 120] } else { [200, 200, 200] }); }
    line(&mut img, (600., 0.), (600., h as f32), [0, 0, 0]);
    for q in world.iter() {
        let c = match q.material { Material::Wood => [150, 90, 30], Material::Stone => [90, 90, 90], Material::Metal => [40, 90, 170] };
        for hull in placement::piece_hulls(&cat, q) {
            for e in hull.edges.iter() { let (a, b) = (hull.verts[e[0] as usize], hull.verts[e[1] as usize]); line(&mut img, top(a), top(b), c); line(&mut img, side(a), side(b), c); }
        }
    }
    img.save(&a[2]).expect("png");
    println!("wrote {}", a[2]);
}
