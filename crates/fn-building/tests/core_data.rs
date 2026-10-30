//! Core building against the cooked 1.8 data (`FN_TEST_ROOT` = the FortniteGame directory; skipped when unset).
use fn_building::grid::{self, Rot};
use fn_building::pieces::{Material, PieceCatalog, PieceKind};
use fn_building::placement::{self, BuildPreview, PlacementEnv, QueryResult, ViewInput};
use fn_building::world::BuildingWorld;
use glam::Vec3;

fn catalog() -> Option<PieceCatalog> {
    let root = std::env::var("FN_TEST_ROOT").ok()?;
    let set = fn_pak::PakSet::open_install(&root).expect("paks");
    Some(PieceCatalog::load(&set).expect("catalog"))
}

#[test]
fn catalog_classes_and_numbers() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let d = |k, m| cat.def(k, m).unwrap();
    assert_eq!(d(PieceKind::Wall, Material::Wood).class.class_path, "/Game/Building/ActorBlueprints/Player/Wood/L1/PBWA_W1_Solid.PBWA_W1_Solid_C");
    assert_eq!(d(PieceKind::Floor, Material::Stone).class.class_path, "/Game/Building/ActorBlueprints/Player/Stone/L1/PBWA_S1_Floor.PBWA_S1_Floor_C");
    assert_eq!(d(PieceKind::Stairs, Material::Metal).class.class_path, "/Game/Building/ActorBlueprints/Player/Metal/L1/PBWA_M1_StairW.PBWA_M1_StairW_C");
    assert_eq!(d(PieceKind::Roof, Material::Wood).class.class_path, "/Game/Building/ActorBlueprints/Player/Wood/L1/PBWA_W1_RoofC.PBWA_W1_RoofC_C");
    // AttributesBuildingSection @ level 0: Size9 walls 200 / 300 / 400, Size7 others 190 / 280 / 370; BuildTime 4 / 12 / 20
    for (m, wall, other, t, pct) in [(Material::Wood, 200., 190., 4., 0.5), (Material::Stone, 300., 280., 12., 0.3), (Material::Metal, 400., 370., 20., 0.2)] {
        assert_eq!(d(PieceKind::Wall, m).max_health, wall);
        for k in [PieceKind::Floor, PieceKind::Stairs, PieceKind::Roof] { assert_eq!(d(k, m).max_health, other); }
        assert_eq!(d(PieceKind::Wall, m).build_time, t);
        assert!((d(PieceKind::Wall, m).initial_health_pct - pct).abs() < 1e-6);
        assert_eq!(d(PieceKind::Wall, m).cost, 10);
    }
    // connectivity cubes as cooked (also the constants of the placement unit tests)
    assert_eq!(d(PieceKind::Wall, Material::Wood).class.cube, [0x000F_C63F, 0x0008_4210, 0, 0x0000_8421, 0x01F0_0000, 0x01F0_0000]);
    assert_eq!(d(PieceKind::Floor, Material::Wood).class.cube, [0x000F_8000, 0x000F_8000, 0x000F_8000, 0x000F_8000, 0, 0x01F8_D63F]);
    assert_eq!(d(PieceKind::Stairs, Material::Wood).class.cube, [0x000F_8000, 0x0008_0001, 0x0000_001F, 0x0000_8010, 0x0000_001F, 0x01F0_0000]);
    for def in cat.defs() { assert!(!def.class.convexes.is_empty(), "{:?} {:?}", def.kind, def.material); }
    // the wall's cooked collision box: X -256..256, Y -8..8, Z 0..384 about the pivot
    let w = &d(PieceKind::Wall, Material::Wood).class.convexes[0];
    let (lo, hi) = w.iter().fold((Vec3::splat(1e9), Vec3::splat(-1e9)), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!((lo - Vec3::new(-256., -8., 0.)).abs().max_element() < 0.01 && (hi - Vec3::new(256., 8., 384.)).abs().max_element() < 0.01, "{lo} {hi}");
}

/// Ramp, wall and floor placed next to each other on the ground, checked against the values derived from the data:
/// cells centred on multiples of 512 (SnapGridSize), levels multiples of 384 (VertSnapGridSize), pivots at
/// grid point + R(yaw) * BaseLocToPivotOffset (0,-256,0), yaw = snapped view yaw + 90.
#[test]
fn ramp_wall_floor_side_by_side_transforms() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let mut world = BuildingWorld::new();
    let env = PlacementEnv { ground_z: Some(0.), grid_origin: Vec3::ZERO, ..Default::default() };
    let view = Rot::yaw(0.); // looking along +X
    let put = |world: &mut BuildingWorld, kind: PieceKind, target: Vec3| {
        let def = cat.def(kind, Material::Wood).unwrap();
        let (loc, rot) = grid::snap_transform(kind, target, view, 0);
        assert_eq!(placement::query(world, &cat, def, loc, rot.yaw, false, &env), QueryResult::CanAdd, "{kind:?} at {loc}");
        placement::spawn_piece(world, def, loc, rot.yaw, false, Vec3::ZERO)
    };
    // floor in cell (0,0), ramp in cell (1,0), wall on the +X edge of cell (1,0) (the ramp's high end)
    let f = put(&mut world, PieceKind::Floor, Vec3::new(40., -30., 10.));
    let s = put(&mut world, PieceKind::Stairs, Vec3::new(560., 20., 150.));
    let w = put(&mut world, PieceKind::Wall, Vec3::new(600., 10., 150.));
    let near = |a: Vec3, b: Vec3| (a - b).abs().max_element() < 1e-3;
    let (pf, ps, pw) = (world.get(f).unwrap(), world.get(s).unwrap(), world.get(w).unwrap());
    // yaw = 0 + 90 for all three; R(90) * (0,-256,0) = (256, 0, 0)
    assert!(near(pf.location, Vec3::new(256., 0., 0.)) && pf.yaw_deg == 90., "floor {:?} {}", pf.location, pf.yaw_deg);
    assert!(near(ps.location, Vec3::new(512. + 256., 0., 0.)) && ps.yaw_deg == 90., "ramp {:?} {}", ps.location, ps.yaw_deg);
    assert!(near(pw.location, Vec3::new(512. + 256., 0., 0.)) && pw.yaw_deg == 90., "wall {:?} {}", pw.location, pw.yaw_deg);
    // centroids: floor at the cell centre (0,0,0), ramp at (512,0,192), wall at the edge midpoint half a level up
    assert!(near(grid::centroid(PieceKind::Floor, pf.location, pf.yaw_deg), Vec3::new(0., 0., 0.)));
    assert!(near(grid::centroid(PieceKind::Stairs, ps.location, ps.yaw_deg), Vec3::new(512., 0., 192.)));
    assert!(near(grid::centroid(PieceKind::Wall, pw.location, pw.yaw_deg), Vec3::new(768., 0., 192.)));
    // the floor's far edge (x = 256) is the ramp's near edge: the ramp's collision is low (z ~ 0) at x = 256 and high (z ~ 384) at x = 768
    let hulls = placement::piece_hulls(&cat, ps);
    let top = |x: f32| hulls.iter().filter_map(|h| h.ray(Vec3::new(x, 0., 1000.), Vec3::NEG_Z, 2000.)).map(|(t, _)| 1000. - t).fold(f32::MIN, f32::max);
    assert!((top(300.) - 50.).abs() < 20., "ramp low end top {}", top(300.));
    assert!((top(740.) - 375.).abs() < 25., "ramp high end top {}", top(740.));
    // slots: a second floor in cell (0,0) is ExistingActor, a cone in the ramp's cell is ExistingActor, a cone over the floor is fine
    let def_f = cat.def(PieceKind::Floor, Material::Wood).unwrap();
    let (l, r) = grid::snap_transform(PieceKind::Floor, Vec3::new(-100., 100., 0.), view, 0);
    assert_eq!(placement::query(&world, &cat, def_f, l, r.yaw, false, &env), QueryResult::ExistingActor(f));
    let def_r = cat.def(PieceKind::Roof, Material::Wood).unwrap();
    let (l, r) = grid::snap_transform(PieceKind::Roof, Vec3::new(512., 0., 0.), view, 0);
    assert_eq!(placement::query(&world, &cat, def_r, l, r.yaw, false, &env), QueryResult::ExistingActor(s));
    let (l, r) = grid::snap_transform(PieceKind::Roof, Vec3::new(0., 0., 0.), view, 0);
    assert_eq!(placement::query(&world, &cat, def_r, l, r.yaw, false, &env), QueryResult::CanAdd);
    // a floor one level up over the ramp's high end is supported by the ramp (lattice edge shared), one more level up is not
    let (l, r) = grid::snap_transform(PieceKind::Floor, Vec3::new(1024., 0., 384.), view, 0);
    assert_eq!(placement::query(&world, &cat, def_f, l, r.yaw, false, &env), QueryResult::CanAdd);
    let (l, r) = grid::snap_transform(PieceKind::Floor, Vec3::new(1024., 0., 768.), view, 0);
    assert_eq!(placement::query(&world, &cat, def_f, l, r.yaw, false, &env), QueryResult::NoStructuralSupport);
}

/// The preview path end to end: a pawn on the ground looking forward and slightly down gets a wall right in front of it, and a
/// floor under its feet when looking down.
#[test]
fn preview_from_the_camera() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let world = BuildingWorld::new();
    let env = PlacementEnv { ground_z: Some(0.), grid_origin: Vec3::ZERO, ..Default::default() };
    let pawn = Vec3::new(100., 100., 90.);
    let cam = pawn + Vec3::new(-250., 60., 60.);
    let mut p = BuildPreview::default();
    p.set_piece(PieceKind::Wall);
    let r = p.update(&world, &cat, &env, &ViewInput { camera: cam, view: Rot { pitch: -10., yaw: 0., roll: 0. }, pawn });
    assert_eq!(r, QueryResult::CanAdd);
    // wall: target clamped to the 128 cm box around the pawn-distance point, cell (0,0) -> the +X edge of the pawn's cell
    assert!((p.loc - Vec3::new(256., 0., 0.)).abs().max_element() < 1e-3, "{:?}", p.loc);
    p.set_piece(PieceKind::Floor);
    let r = p.update(&world, &cat, &env, &ViewInput { camera: cam, view: Rot { pitch: -80., yaw: 0., roll: 0. }, pawn });
    assert_eq!(r, QueryResult::CanAdd);
    assert!((grid::centroid(PieceKind::Floor, p.loc, p.rot.yaw) - Vec3::new(0., 0., 0.)).abs().max_element() < 1e-3, "{:?}", p.loc);
}

#[test]
fn preview_grid_snap_flag_and_existing_actor_after_placing() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let mut world = BuildingWorld::new();
    let env = PlacementEnv { ground_z: Some(0.), grid_origin: Vec3::ZERO, ..Default::default() };
    let pawn = Vec3::new(100., 100., 90.);
    let view = ViewInput { camera: pawn + Vec3::new(-250., 60., 60.), view: Rot { pitch: -10., yaw: 0., roll: 0. }, pawn };
    let mut p = BuildPreview::default();
    p.set_piece(PieceKind::Wall);
    assert_eq!(p.update(&world, &cat, &env, &view), QueryResult::CanAdd);
    assert!(p.snapped);
    p.update(&world, &cat, &env, &view);
    assert!(!p.snapped);
    let (id, cost) = placement::place(&mut world, &cat, &env, &p, 999).unwrap();
    assert_eq!(cost, 10);
    assert_eq!(p.update(&world, &cat, &env, &view), QueryResult::ExistingActor(id));
    assert!(!p.snapped);
    assert_eq!(placement::place(&mut world, &cat, &env, &p, 999), Err(QueryResult::ExistingActor(id)));
    p.update(&world, &cat, &env, &ViewInput { view: Rot { pitch: -10., yaw: 90., roll: 0. }, ..view });
    assert!(p.snapped);
}

/// The lattice is the level's: with the level origin at `o` in the world space, every snapped transform moves by exactly `o`
/// (no player-relative term), and the preview of a camera moved by `o` lands on the moved lattice.
#[test]
fn grid_follows_the_level_origin() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let o = Vec3::new(137., -411., -95.);
    let world = BuildingWorld::new();
    let pawn = Vec3::new(100., 100., 90.);
    let cam = pawn + Vec3::new(-250., 60., 60.);
    let run = |origin: Vec3, shift: Vec3| {
        let env = PlacementEnv { ground_z: Some(shift.z), grid_origin: origin, ..Default::default() };
        let mut p = BuildPreview::default();
        p.set_piece(PieceKind::Wall);
        p.update(&world, &cat, &env, &ViewInput { camera: cam + shift, view: Rot { pitch: -10., yaw: 0., roll: 0. }, pawn: pawn + shift });
        p.loc
    };
    let a = run(Vec3::ZERO, Vec3::ZERO);
    let b = run(o, o);
    assert!((b - a - o).abs().max_element() < 1e-2, "{a} {b}");
}

fn box_hull(lo: Vec3, hi: Vec3) -> fn_hull::Hull {
    let mut v = vec![];
    for x in [lo.x, hi.x] { for y in [lo.y, hi.y] { for z in [lo.z, hi.z] { v.push(Vec3::new(x, y, z)); } } }
    fn_hull::Hull::from_points(&v).unwrap()
}

/// G2 (docs/spec/building_core_gaps.md): props (BuildingProp / BuildingContainer: bDoNotBlockBuildings 0, no centroid test) obstruct any
/// piece whose "WorldOverlapCheck" box touches them, never count as ground; a static mesh actor obstructs above the base (gl.z + 13 floors /
/// + 10 others) and is ground at / below it; neighbouring player pieces never obstruct (centroid rule).
#[test]
fn props_and_static_meshes_in_the_world_overlap_check() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let world = BuildingWorld::new();
    let view = Rot::yaw(0.);
    let wall = cat.def(PieceKind::Wall, Material::Wood).unwrap();
    let floor = cat.def(PieceKind::Floor, Material::Wood).unwrap();
    let (wl, wr) = grid::snap_transform(PieceKind::Wall, Vec3::new(600., 10., 150.), view, 0);
    let (fl, fr) = grid::snap_transform(PieceKind::Floor, Vec3::new(40., -30., 10.), view, 0);
    let wall_centre = grid::centroid(PieceKind::Wall, wl, wr.yaw);
    let with_props = |hulls: Vec<fn_hull::Hull>| PlacementEnv { ground_z: Some(0.), props: std::sync::Arc::new(hulls.into_iter().enumerate().map(|(i, h)| placement::PropCollider::blocking(i as u64, vec![h])).collect()), ..Default::default() };
    let q = |def, loc, yaw, env: &PlacementEnv| placement::query(&world, &cat, def, loc, yaw, false, env);
    // a 40 cm trunk standing through the wall's plane
    let trunk = box_hull(Vec3::new(wall_centre.x - 20., wall_centre.y - 20., 0.), Vec3::new(wall_centre.x + 20., wall_centre.y + 20., 600.));
    assert_eq!(q(wall, wl, wr.yaw, &with_props(vec![trunk.clone()])), QueryResult::Obstructed);
    // the same trunk 1 m beside the wall plane: clear (the wall box is only 6 cm thick)
    let thin = grid::rotate_yaw(Vec3::Y, wr.yaw); // the wall's cooked box is 16 cm thick along its local Y
    let off = trunk.mapped(|v| v + thin * 100., false);
    assert_eq!(q(wall, wl, wr.yaw, &with_props(vec![off])), QueryResult::CanAdd);
    // a rock under a floor: crossing the thin slab (+15 / -18) obstructs, one below it does not; a prop is never ground
    let fc = grid::centroid(PieceKind::Floor, fl, fr.yaw);
    let rock = |top: f32| box_hull(Vec3::new(fc.x - 50., fc.y - 50., top - 100.), Vec3::new(fc.x + 50., fc.y + 50., top));
    assert_eq!(q(floor, fl, fr.yaw, &with_props(vec![rock(60.)])), QueryResult::Obstructed);
    let up = Vec3::new(0., 0., 384.);
    let floating = with_props(vec![rock(384. - 30.)]);
    assert_eq!(q(floor, fl + up, fr.yaw, &floating), QueryResult::NoStructuralSupport, "a prop below the slab is neither an obstruction nor ground");
    // a static mesh actor below the floor's base is ground, above it obstructs
    let mesh = |top: f32| PlacementEnv { ground_z: Some(0.), static_meshes: std::sync::Arc::new(vec![rock(top)]), ..Default::default() };
    assert_eq!(q(floor, fl + up, fr.yaw, &mesh(384. + 5.)), QueryResult::CanAdd);
    assert_eq!(q(floor, fl + up, fr.yaw, &mesh(384. + 14.)), QueryResult::Obstructed);
    // a neighbouring player piece never obstructs: wall on the floor's edge with the floor placed
    let mut w2 = BuildingWorld::new();
    placement::spawn_piece(&mut w2, floor, fl, fr.yaw, false, Vec3::ZERO);
    let (el, er) = grid::snap_transform(PieceKind::Wall, Vec3::new(250., 10., 150.), view, 0);
    assert_eq!(placement::query(&w2, &cat, wall, el, er.yaw, false, &with_props(vec![])), QueryResult::CanAdd);
    // structural grid bounds: below -3840 (FUN_1404f2160) is InvalidActor
    let bounded = PlacementEnv { ground_z: Some(0.), grid_bounds: Some((Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, placement::STRUCTURAL_GRID_MIN_Z), Vec3::splat(f32::INFINITY))), ..Default::default() };
    assert_eq!(q(floor, fl - Vec3::new(0., 0., 384. * 11.), fr.yaw, &bounded), QueryResult::InvalidActor);
    assert_eq!(q(floor, fl, fr.yaw, &bounded), QueryResult::CanAdd);
    // bDestroyOnPlayerBuildingPlacement (every bush / hedge / small rock): the same trunk no longer obstructs, the placement destroys it;
    // bDoNotBlockBuildings: ignored and kept; a blocking prop next to a destroyable one still obstructs
    let flagged = |f: placement::BlockFlags, id: u64, h: fn_hull::Hull| placement::PropCollider { id, hulls: vec![h.clone()], flags: f, centroid: h.center };
    let destroy = placement::BlockFlags { destroy_on_player_building_placement: true, ..Default::default() };
    let ignore = placement::BlockFlags { do_not_block_buildings: true, ..Default::default() };
    let env_of = |v: Vec<placement::PropCollider>| PlacementEnv { ground_z: Some(0.), props: std::sync::Arc::new(v), ..Default::default() };
    let bush = env_of(vec![flagged(destroy, 41, trunk.clone())]);
    assert_eq!(q(wall, wl, wr.yaw, &bush), QueryResult::CanAdd);
    assert_eq!(placement::placement_destroys(&bush, PieceKind::Wall, wl, wr.yaw), vec![41]);
    let lamp = env_of(vec![flagged(ignore, 42, trunk.clone())]);
    assert_eq!(q(wall, wl, wr.yaw, &lamp), QueryResult::CanAdd);
    assert!(placement::placement_destroys(&lamp, PieceKind::Wall, wl, wr.yaw).is_empty());
    let both = env_of(vec![flagged(destroy, 41, trunk.clone()), flagged(placement::BlockFlags::default(), 43, trunk.clone())]);
    assert_eq!(q(wall, wl, wr.yaw, &both), QueryResult::Obstructed);
}


#[test]
fn player_piece_health_and_build_time_are_the_cooked_level_0_rows() {
    let Some(root) = std::env::var_os("FN_TEST_ROOT") else { return };
    let set = fn_pak::PakSet::open_install(std::path::Path::new(&root)).unwrap();
    let t = fn_building::pieces::CurveTable::load(&set, "/Game/Balance/DataTables/AttributesBuildingSection").unwrap();
    assert_eq!(t.eval("PlayerBuildingWood1.Size9.FortHealthSet.MaxHealth", 0.).unwrap(), 200.);
    assert_eq!(t.eval("PlayerBuildingStone1.Size9.FortHealthSet.MaxHealth", 0.).unwrap(), 300.);
    assert_eq!(t.eval("PlayerBuildingMetal1.Size9.FortHealthSet.MaxHealth", 0.).unwrap(), 400.);
    let cat = fn_building::pieces::PieceCatalog::load(&set).unwrap();
    for d in cat.defs() {
        let row = format!("{}.{}.FortHealthSet.MaxHealth", d.class.attribute_category, d.class.attribute_subcategory);
        assert_eq!(d.max_health, t.eval(&row, fn_building::pieces::ATHENA_BUILDING_LEVEL).unwrap(), "{row}");
        assert_eq!(d.build_time, t.eval(&format!("{}.FortBuildingActorSet.BuildTime", d.class.attribute_category), fn_building::pieces::ATHENA_BUILDING_LEVEL).unwrap());
    }
}

#[test]
fn poi_building_pieces_block_only_through_their_centroid() {
    let Some(cat) = catalog() else { eprintln!("FN_TEST_ROOT not set: skipped"); return };
    let world = BuildingWorld::new();
    let view = Rot::yaw(0.);
    let wall = cat.def(PieceKind::Wall, Material::Wood).unwrap();
    let floor = cat.def(PieceKind::Floor, Material::Wood).unwrap();
    let (wl, wr) = grid::snap_transform(PieceKind::Wall, Vec3::new(600., 10., 150.), view, 0);
    let c = grid::centroid(PieceKind::Wall, wl, wr.yaw);
    let along = grid::rotate_yaw(Vec3::X, wr.yaw).abs();
    let thick = grid::rotate_yaw(Vec3::Y, wr.yaw).abs();
    let wall_hull = |centre: Vec3| box_hull(centre - along * 256. - thick * 10. - Vec3::Z * 192., centre + along * 256. + thick * 10. + Vec3::Z * 192.);
    let poi = placement::BlockFlags { use_centroid: true, ..Default::default() };
    let env = |centre: Vec3| PlacementEnv { ground_z: Some(0.), props: std::sync::Arc::new(vec![placement::PropCollider { id: 7, hulls: vec![wall_hull(centre)], flags: poi, centroid: centre }]), ..Default::default() };
    let q = |def, loc, yaw, env: &PlacementEnv| placement::query(&world, &cat, def, loc, yaw, false, env);
    assert_eq!(q(wall, wl, wr.yaw, &env(c)), QueryResult::Obstructed);
    assert_eq!(q(wall, wl, wr.yaw, &env(c + along * 512.)), QueryResult::CanAdd);
    let (fl, fr) = grid::snap_transform(PieceKind::Floor, Vec3::new(40., -30., 10.), view, 0);
    let fc = grid::centroid(PieceKind::Floor, fl, fr.yaw);
    let slab = |centre: Vec3| box_hull(centre - Vec3::new(300., 300., 10.), centre + Vec3::new(300., 300., 10.));
    let floor_env = |centre: Vec3| PlacementEnv { ground_z: Some(0.), props: std::sync::Arc::new(vec![placement::PropCollider { id: 8, hulls: vec![slab(centre)], flags: poi, centroid: centre }]), ..Default::default() };
    assert_eq!(q(floor, fl, fr.yaw, &floor_env(fc + Vec3::X * 512.)), QueryResult::CanAdd);
    let old = PlacementEnv { ground_z: Some(0.), props: std::sync::Arc::new(vec![placement::PropCollider::blocking(9, vec![slab(fc + Vec3::X * 512.)])]), ..Default::default() };
    assert_eq!(q(floor, fl, fr.yaw, &old), QueryResult::Obstructed);
}
