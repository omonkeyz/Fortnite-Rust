use super::*;
use format::{Link, Poly};

fn quad_tile(tx: u16, ty: u16, links: Vec<Link>) -> Tile {
    let c = 60u16;
    let mut t = Tile { tx, ty, zbase: 0., ..Default::default() };
    t.verts = vec![[0, 0, 0], [c, 0, 0], [c, c, 0], [0, c, 0]];
    t.poly_verts = vec![0, 1, 2, 3];
    t.poly_neis = vec![NO_NEI; 4];
    let s = 17. * format::DXY_SCALE;
    t.dverts = vec![[0, 0], [(c as f32 * s) as i16, 0], [(c as f32 * s) as i16, (c as f32 * s) as i16], [0, (c as f32 * s) as i16]];
    t.dvz = vec![(format::DZ_BIAS * format::DZ_SCALE) as u16; 4];
    t.dtris = vec![[0, 1, 2], [0, 2, 3]];
    t.polys = vec![Poly { vbase: 0, nv: 4, area: 63, dvbase: 0, dvn: 4, dtbase: 0, dtn: 2, ..Default::default() }];
    t.links = links;
    t
}

fn link(edge: u8, slot: u32) -> Link { Link { poly: 0, edge, tmin: 0, tmax: 255, to: slot << POLY_BITS } }

fn synth(tiles_x: u32, tiles_y: u32, tiles: Vec<Tile>) -> NavMesh {
    let h = Header { cs: 17., ch: 16., tile_cells: 60, origin: [0., 0.], tiles_x, tiles_y, agent_radius: 34., agent_height: 144., agent_climb: 45., kill_z: -5000. };
    NavMesh::from_bytes(&format::encode(&h, &tiles)).expect("synthetic mesh decodes")
}

fn strip() -> NavMesh { synth(2, 1, vec![quad_tile(0, 0, vec![link(1, 1)]), quad_tile(1, 0, vec![link(3, 0)])]) }

fn ell() -> NavMesh {
    synth(2, 2, vec![quad_tile(0, 0, vec![link(1, 1), link(2, 2)]), quad_tile(1, 0, vec![link(3, 0)]), quad_tile(0, 1, vec![link(0, 0)])])
}

#[test]
fn path_crosses_tiles_in_a_straight_line() {
    let nm = strip();
    let p = nm.find_path(V3::new(100., 500., 0.), V3::new(1900., 500., 0.)).expect("path");
    assert_eq!(p.len(), 2, "{p:?}");
    assert!((p[0] - V3::new(100., 500., 0.)).length() < 0.1 && (p[1] - V3::new(1900., 500., 0.)).length() < 0.1, "{p:?}");
}

#[test]
fn funnel_turns_at_the_inner_corner() {
    let nm = ell();
    let p = nm.find_path(V3::new(1900., 900., 0.), V3::new(900., 1900., 0.)).expect("path");
    assert_eq!(p.len(), 3, "{p:?}");
    assert!((p[1].truncate() - Vec2::new(1020., 1020.)).length() < 0.5, "corner {:?}", p[1]);
}

#[test]
fn raycast_hits_the_mesh_border_only() {
    let nm = strip();
    assert_eq!(nm.raycast(V3::new(100., 500., 0.), V3::new(1900., 500., 0.)), None);
    let h = nm.raycast(V3::new(100., 500., 0.), V3::new(2500., 500., 0.)).expect("hit");
    assert!((h.x - 2040.).abs() < 0.5 && (h.y - 500.).abs() < 0.5, "{h:?}");
}

#[test]
fn project_snaps_to_the_surface_within_the_extent() {
    let nm = strip();
    let q = nm.project(V3::new(500., 500., 100.), V3::new(50., 50., 200.)).expect("on mesh");
    assert!((q - V3::new(500., 500., 0.)).length() < 0.1, "{q:?}");
    assert!(nm.project(V3::new(500., 500., 900.), V3::new(50., 50., 200.)).is_none());
    assert!(nm.project(V3::new(-500., 500., 0.), V3::new(50., 50., 200.)).is_none());
}

#[test]
fn obstacles_block_paths_and_rays_until_removed() {
    let mut nm = strip();
    let (a, b) = (V3::new(100., 500., 0.), V3::new(1900., 500., 0.));
    let id = nm.add_obstacle(V3::new(1000., -100., -10.), V3::new(1040., 1100., 300.));
    assert!(nm.find_path(a, b).is_none());
    let h = nm.raycast(a, b).expect("wall hit");
    assert!((h.x - (1000. - 34.)).abs() < 0.5, "{h:?}");
    assert!(nm.remove_obstacle(id));
    assert!(nm.find_path(a, b).is_some());
}

#[test]
fn random_points_stay_in_radius_and_repeat_per_seed() {
    let nm = strip();
    let c = V3::new(1000., 500., 0.);
    for seed in 0..64 {
        let p = nm.random_point_in_radius(c, 300., seed).expect("point");
        assert!((p - c).truncate().length() <= 300.01 && p.z.abs() < 0.1, "{p:?}");
        assert_eq!(Some(p), nm.random_point_in_radius(c, 300., seed));
    }
}

#[test]
fn offset_maps_caller_space_to_mesh_space() {
    let nm = strip().with_offset(V3::new(1000., 0., 50.));
    let q = nm.project(V3::new(-500., 500., -40.), V3::new(50., 50., 200.)).expect("on mesh");
    assert!((q - V3::new(-500., 500., -50.)).length() < 0.1, "{q:?}");
}

fn athena() -> Option<NavMesh> {
    let nm = NavMesh::load_athena().or_else(|| NavMesh::load_from("D:/Fortnite/FortniteRust/FortniteRuntime/assets/nav/athena.fnnav"));
    if nm.is_none() { eprintln!("athena.fnnav not found (FN_NAV_FILE / FN_ASSETS): skipped"); }
    nm
}

pub const SALTY: V3 = V3::new(6000., -4800., 1915.);
pub const GREASY: V3 = V3::new(1598., -95948., 800.);

fn ground(nm: &NavMesh, p: V3) -> V3 { nm.project(p, V3::new(100., 100., 150.)).expect("ground on the navmesh") }

#[test]
fn athena_salty_springs_to_greasy_grove() {
    let Some(nm) = athena() else { return };
    let (a, b) = (ground(&nm, SALTY), ground(&nm, GREASY));
    let p = nm.find_path(a, b).expect("a complete path from Salty Springs to Greasy Grove");
    let len: f32 = p.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    assert!((p[0] - a).length() < 1. && (*p.last().unwrap() - b).length() < 1.);
    assert!(len >= (b - a).truncate().length() && len < 2. * (b - a).length(), "length {len}");
    for w in p.windows(2) {
        let d = w[1] - w[0];
        let n = (d.truncate().length() / 50.).ceil().max(1.) as usize;
        for k in 0..=n {
            let q = w[0] + d * (k as f32 / n as f32);
            assert!(nm.project(q, V3::new(5., 5., 1500.)).is_some(), "path point {q:?} on {:?} -> {:?} is off the mesh", w[0], w[1]);
        }
        let s = w[0] + d.normalize_or_zero() * 2.;
        assert!(nm.raycast(s, w[1]).is_none(), "segment {:?} -> {:?} is blocked", w[0], w[1]);
    }
}

#[test]
fn athena_projects_onto_a_salty_roof() {
    let Some(nm) = athena() else { return };
    let q = nm.project(V3::new(4600., -13800., 3900.), V3::new(50., 50., 250.)).expect("roof");
    assert!((q.z - 3701.9).abs() <= 32., "roof at {q:?}, SM_Fort_Roofs_Generic03 collision top 3701.9");
    let g = nm.project(V3::new(4600., -13800., 3100.), V3::new(50., 50., 60.)).expect("floor below");
    assert!((g.z - 3080.).abs() <= 32., "upper floor at {g:?}, SM_Fort_Floors_Generic_TileBasic top 3080");
}

#[test]
fn athena_has_no_path_into_the_sea() {
    let Some(nm) = athena() else { return };
    let sea = V3::new(-95416., -23328., -5312.);
    assert!(nm.project(sea, V3::new(400., 400., 2000.)).is_none());
    assert!(nm.find_path(ground(&nm, SALTY), sea).is_none());
}

#[test]
fn game_mapping_takes_metres_y_up() {
    let m = fn_terrain::Mapping { centre_cm: [1000., 500., 0.], anchor: V3::new(3., 0., 4.), ref_z_cm: -100. };
    let nm = strip().with_game_mapping(m);
    let g = m.to_game(V3::new(1500., 600., 0.));
    let q = nm.project(g + V3::new(0., 0.5, 0.), V3::new(0.5, 2., 0.5)).expect("on mesh");
    assert!((q - g).length() < 1e-3, "{q:?} vs {g:?}");
    let p = nm.find_path(m.to_game(V3::new(100., 500., 0.)), m.to_game(V3::new(1900., 500., 0.))).expect("path");
    assert_eq!(p.len(), 2);
    let r = nm.random_point_in_radius(g, 3., 7).expect("point");
    assert!((r - g).length() <= 3.01 && (r.y - g.y).abs() < 1e-3, "{r:?}");
}

#[test]
fn disconnected_targets_fail_without_a_search() {
    let nm = synth(3, 1, vec![quad_tile(0, 0, vec![]), quad_tile(2, 0, vec![])]);
    assert!(nm.find_path(V3::new(100., 500., 0.), V3::new(2200., 500., 0.)).is_none());
    assert!(nm.find_path(V3::new(100., 500., 0.), V3::new(900., 500., 0.)).is_some());
}
