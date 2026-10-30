//! OUTLINE HOOK (fn-app): the build preview's highlight of the props it is blocked by (red) or would destroy (yellow), drawn the 1.8 way: the props' meshes into a
//! custom depth / stencil target and the original `SkinAndMeshOutlinePostProcess` program over the scene (`fn_matshader::outline`, `fn_render::outline_pass`).
//! Which props and which colours: `building_hook::highlight` (the preview update's `vfunc_672` call).
//!
//! * [`start`] (from the scene build): decodes the material on a worker thread.
//! * [`submit`] (every `Scene::draw`): installs the program once it is decoded, then hands the frame's custom depth instances + uniforms to the renderer.
use fn_matshader::outline::OutlineAssets;
use fn_matshader::view::ViewInput;
use fn_render::static_mesh::StaticMeshInstance;
use fn_render::{CameraParams, Renderer};
use std::sync::{mpsc, Mutex};

struct State { assets: Option<OutlineAssets>, loading: Option<mpsc::Receiver<Result<OutlineAssets, String>>>, installed: bool, failed: bool }
static STATE: Mutex<State> = Mutex::new(State { assets: None, loading: None, installed: false, failed: false });
static PIECE_MESHES: Mutex<Option<std::collections::HashMap<String, Option<fn_render::StaticMeshHandle>>>> = Mutex::new(None);

fn piece_mesh(renderer: &mut Renderer, path: &str) -> Option<fn_render::StaticMeshHandle> {
    let mut g = PIECE_MESHES.lock().unwrap_or_else(|e| e.into_inner());
    let cache = g.get_or_insert_with(Default::default);
    if let Some(h) = cache.get(path) { return *h; }
    let lod = crate::building_hook::with_set(|set| {
        let set = set?;
        let (a, x, b) = fn_fx::data::package_files(set, fn_fx::data::long_package(path)).ok()?;
        let p = fn_ueio::Package::parse(&a, &x, &b).ok()?;
        let m = fn_mesh::StaticMesh::parse_named(&p, path.split_once('.').map(|x| x.1)).ok()?;
        let lod = m.lods.into_iter().next()?;
        Some((lod.positions, lod.indices))
    });
    let h = lod.map(|(positions, indices)| {
        let vertices: Vec<fn_render::static_mesh::StaticVertex> = positions.iter().map(|p| fn_render::static_mesh::StaticVertex { pos: (fn_building::visual::ue_to_game(glam::Vec3::from(*p))).to_array(), normal: [0., 1., 0.], uv: [0., 0.], color: [255; 4] }).collect();
        let n = indices.len() as u32;
        let data = fn_render::static_mesh::StaticMeshData { name: path.to_owned(), lods: vec![fn_render::static_mesh::StaticLod { vertices, indices, sections: vec![fn_render::static_mesh::StaticSection { first_index: 0, index_count: n, material: 0 }], max_distance: f32::MAX }], ..Default::default() };
        let mat = renderer.create_static_material(&fn_render::static_mesh::StaticMaterialData { name: format!("{path} custom depth"), two_sided: true, ..Default::default() });
        renderer.create_static_mesh(&data, &[mat])
    });
    if h.is_none() { eprintln!("[outline] {path}: piece mesh not loaded, the occupant is not outlined"); }
    cache.insert(path.to_owned(), h);
    h
}

/// Starts decoding the outline material from the paks (once).
pub fn start(install_dir: &std::path::Path) {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if s.loading.is_some() || s.assets.is_some() || s.failed { return; }
    let (tx, rx) = mpsc::channel();
    let dir = install_dir.to_owned();
    let _ = std::thread::Builder::new().name("fn-outline-load".into()).spawn(move || {
        let r = fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| OutlineAssets::load(&set));
        let _ = tx.send(r);
    });
    s.loading = Some(rx);
}

/// This frame's highlight for the renderer (call before `Renderer::render`). `real` = the loaded prop meshes, `props` = the sim's props.
pub fn submit(renderer: &mut Renderer, real: Option<&crate::props::Real>, props: &[fn_combat::Harvestable], camera: &CameraParams, time: f32) {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rx) = s.loading.as_ref() {
        if let Ok(r) = rx.try_recv() {
            s.loading = None;
            match r {
                Ok(a) => { for n in &a.notes { eprintln!("[outline] {n}"); } s.assets = Some(a); }
                Err(e) => { s.failed = true; eprintln!("[outline] NOT available, the build preview highlights nothing: {e}"); }
            }
        }
    }
    let Some(a) = s.assets.as_ref() else { return };
    if !s.installed {
        let (desc, stencil) = (a.desc.clone(), a.selection_stencil);
        match renderer.outline().create(desc, fn_render::renderer::HDR_FORMAT) {
            Ok(()) => { s.installed = true; eprintln!("[outline] ON: SkinAndMeshOutlinePostProcess (SM5 {}, DXBC -> WGSL), custom stencil {stencil}", fn_matshader::outline::PROGRAM); }
            Err(e) => { s.failed = true; s.assets = None; eprintln!("[outline] program rejected: {e}"); return; }
        }
    }
    let Some(a) = s.assets.as_ref() else { return };
    // the highlighted props that have a real mesh
    let mut hl = crate::building_hook::highlight();
    // Scripted-run test aid (never set in play): FN_DEBUG_OUTLINE=<0|2> highlights the 3 live real props nearest to the camera with that HighlightColors mode
    if hl.is_none() { if let Some(mode) = fn_live::var("FN_DEBUG_OUTLINE").ok().and_then(|v| v.parse::<u8>().ok()) {
        let mut near: Vec<(f32, usize)> = props.iter().enumerate().filter(|(_, p)| p.alive() && p.hull.is_some()).map(|(i, p)| ((p.pos - camera.eye).length(), i)).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        hl = Some((mode, mode != 0, near.iter().take(3).map(|x| x.1).collect()));
    } }
    let mut instances = vec![];
    if hl.is_none() {
        if let Some((mesh, loc, yaw, mirrored)) = crate::building_hook::highlight_piece() {
            drop(s);
            let h = piece_mesh(renderer, &mesh);
            s = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(h) = h { instances.push(StaticMeshInstance::new(h, fn_building::visual::ue_to_game(loc)).with_yaw(-yaw.to_radians()).with_mirror_x(mirrored)); hl = Some((0, true, vec![])); }
        }
    }
    let Some(a) = s.assets.as_ref() else { return };
    if let (Some((_, _, list)), Some(real)) = (&hl, real) {
        for &i in list {
            let Some(p) = props.get(i).filter(|p| p.alive()) else { continue };
            let Some(mesh) = real.mesh_of(p) else { continue };
            instances.push(StaticMeshInstance::new(mesh, p.pos).with_yaw(p.yaw).with_scale(p.scale).with_shake(p.shake).with_mirror_x(p.mirror_x)); // as scene::split_props
        }
    }
    let colors = hl.filter(|_| !instances.is_empty()).map(|(mode, valid, _)| a.colors[(mode as usize).min(2)][if valid { 0 } else { 1 }]);
    let (tx, ty) = camera.tan_half();
    let view = ViewInput { eye: camera.eye, forward: camera.forward, up: camera.up, tan_half: (tx, ty), near_m: camera.near, far_m: camera.far, width: camera.width, height: camera.height, time };
    let frame = a.frame(&view, fn_render::time_of_day::current().weights, colors);
    drop(s);
    renderer.set_custom_depth_instances(&instances);
    renderer.set_outline_frame(frame);
}
