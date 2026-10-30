//! The 3D world as the renderer sees it: character body + head (GPU skinned, `MaterialKind::Character`), the pickaxe on the `RightHand` socket
//! (skinned character shading with constant spec / mask, `MaterialKind::Prop`, the same as `fn-viewer`, which is calibrated against the prototype), and
//! the harvestables (trees / rocks) through the renderer's own static-prop path `Renderer::set_prop_instances` (`fs_prop`: vertex-colour albedo, sun with
//! cascaded shadows, sky ambient, fog; a port of the prototype's untextured path). The props used to be pushed through the textured character material,
//! which sampled a 1x1 colour texture with a flat normal and rendered trunks / canopies much darker than the prototype.
//! Everything is driven every frame from [`fn_game::GameSim`]: pose tree (`GameSim::pose_tree`) -> `fn_pose::CharacterRig::evaluate_into`
//! (per-part skin matrices + sockets) -> `Renderer::update_model`.
use fn_animgraph::{ClipLibrary, sample_blended_curve};
use fn_game::{GameSim, EYE};
use fn_animgraph::Node;
use fn_pose::{CharacterPose, CharacterRig, ClipStore, Part as PosePart};
use fn_render::{CameraParams, FrameParams, FrameStats, MaterialImages, MaterialKind, ModelId, PropInstance, RenderOptions, Renderer, SkinnedPrimitive, SkinnedVertex, StaticMeshInstance};
use glam::{Mat4, Quat, Vec3};
use wgpu::TextureView;

// ---------------------------------------------------------------------------------------------------------------------------------
// meshes -> renderer primitives
// ---------------------------------------------------------------------------------------------------------------------------------
/// Texture names of one exported `.mat` file (`Diffuse=`, `Normal=`, `SpecPower=` = the SpecularMasks texture, `Other[n]=` ending `_M` = the mask).
#[derive(Default, Debug, PartialEq, Eq)]
pub struct MatSlots { pub diffuse: Option<String>, pub normal: Option<String>, pub spec: Option<String>, pub mask: Option<String> }
pub fn parse_mat(text: &str) -> MatSlots {
    let mut s = MatSlots::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().to_owned();
        match key.trim() {
            "Diffuse" => s.diffuse = Some(value),
            "Normal" => s.normal = Some(value),
            "SpecPower" => s.spec = Some(value),
            k if k.starts_with("Other[") && (value.ends_with("_M") || value.ends_with("_m")) => s.mask = Some(value),
            _ => {}
        }
    }
    s
}

/// Renderer primitives of a pose part: its skin primitives plus the textures its materials name (`<material>.mat` -> PNGs, all found by name).
/// `two_sided[i]`: the resolved `TwoSided` of primitive (= section) `i`'s material (`loader::mesh_two_sided`, read from the packages); the exported `.mat`
/// text carries no such flag, so a primitive without an entry (no paks) is one-sided. Drives the shadow-pass culling (UE4.16 ShadowDepthRendering.cpp:1059).
pub fn part_primitives(part: &PosePart, roots: &crate::assets::AssetRoots, kind: MaterialKind, two_sided: &[bool]) -> Vec<SkinnedPrimitive> {
    part.prims.iter().enumerate().map(|(i, p)| {
        let slots = roots.find(&format!("{}.mat", p.material)).and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_mat(&t)).unwrap_or_default();
        let load = |n: &Option<String>| n.as_ref().and_then(|n| roots.load_rgba(&format!("{n}.png")).ok());
        let material = MaterialImages { diffuse: load(&slots.diffuse), normal: load(&slots.normal), spec: load(&slots.spec), mask: load(&slots.mask), ue: None, two_sided: two_sided.get(i).copied().unwrap_or(false), cooked: Default::default() };
        let vertices = (0..p.positions.len()).map(|i| SkinnedVertex { pos: p.positions[i], normal: p.normals[i], uv: p.uvs[i], joints: p.joints[i], weights: p.weights[i] }).collect();
        SkinnedPrimitive { vertices, indices: p.indices.clone(), material, kind }
    }).collect()
}
/// SK_Pick is attached rigidly (its skin is ignored, like the prototype's `draw_attached`): joints -> 0, weight 1.
pub fn make_rigid(prims: &mut [SkinnedPrimitive]) { for p in prims { for v in &mut p.vertices { v.joints = [0; 4]; v.weights = [1., 0., 0., 0.]; } } }

// ---------------------------------------------------------------------------------------------------------------------------------
// the character rig + the scene
// ---------------------------------------------------------------------------------------------------------------------------------
/// Skeleton parts + animation clips + the reusable pose result.
pub struct Character {
    pub rig: CharacterRig,
    pub clips: Box<dyn ClipLibrary + Send>,
    /// The displayed pawn pose, including the recovered CopyBone and hand-IK
    /// post process from the Athena AnimBlueprint.  Every attached component
    /// uses this same final pose.
    pub pose: CharacterPose,
    /** PART ANIM HOOK: the BR parts' own AnimBPs (face rig, AnimDynamics); `None` = master pose only. */
    pub part_anim: Option<fn_pose::part_anim::PartAnimRig>,
}
impl Character {
    pub fn new(rig: CharacterRig, clips: Box<dyn ClipLibrary + Send>) -> Character {
        Character { rig, clips, pose: CharacterPose::default(), part_anim: None }
    }
    pub fn evaluate(&mut self, sim: &mut GameSim) {
        // The post-process nodes do not introduce clips, so the provisional
        // tree exposes the exact current sequence/blend weights used by UE's
        // float curves.  Feed those values back before constructing the final
        // CopyBone/IK chain for this frame.
        sim.anim.set_female(self.part_anim.as_ref().is_some_and(|p| p.female));
        let curve_tree = sim.pose_tree();
        sim.anim.set_hand_ik_curves(
            sample_blended_curve(&curve_tree, &*self.clips, "SnapWeapon_LHand"),
            crate::interact::montage_curve("BothHandFK", crate::build_anim::montage_curve("BothHandFK", sample_blended_curve(&curve_tree, &*self.clips, "BothHandFK"))), // BUILD ANIM HOOK: Blueprint_Equip's montage curve
            crate::interact::montage_curve("LeftHandFK", sample_blended_curve(&curve_tree, &*self.clips, "LeftHandFK")),
        );
        sim.anim.set_leg_fk_curve(sample_blended_curve(&curve_tree, &*self.clips, "BothLegFK"));
        sim.anim.set_turn_curves(
            sample_blended_curve(&curve_tree, &*self.clips, "BlendIn"),
            sample_blended_curve(&curve_tree, &*self.clips, "BlendOut"),
            sample_blended_curve(&curve_tree, &*self.clips, "TurnRotationAmount"),
        );
        let tree = sim.pose_tree();
        crate::enhanced_anim_load::capture_dst(&self.rig, &tree, &*self.clips);
        // PART ANIM HOOK
        if let Some(pa) = self.part_anim.as_mut() {
            pa.evaluate_into(&self.rig, &tree, &*self.clips, sim.frame_dt, &crate::part_anim_glue::inputs(sim.anim.vars.speed2d), &mut self.pose);
        } else {
            self.rig.evaluate_into(&tree, &*self.clips, &mut self.pose);
        }
        crate::enhanced_anim_load::observe(&self.rig, &self.pose, sim.frame_dt);
    }
    /// The body's local pose (what fn-sim hashes in its frame log).
    pub fn body_local(&self) -> &[(Vec3, Quat)] { self.pose.parts.first().map(|p| p.bone_local.as_slice()).unwrap_or(&[]) }
}

pub struct Scene {
    pub renderer: Renderer,
    pub character: Character,
    body: ModelId,
    head: ModelId,
    pick: ModelId,
    local_dissolving: std::cell::Cell<bool>,
    /// The prop list last uploaded with `set_prop_instances` (re-uploaded only when a prop appears, disappears, moves or shakes).
    props: Vec<PropInstance>,
    /// The REAL 1.8 props (textured static meshes, `fn_render::static_loader`): the mesh of every prop kind, set by the loader. `None` = the assets were missing: every
    /// harvestable is drawn as the procedural placeholder (`props` above).
    pub real: Option<crate::props::Real>,
    /// The static-mesh list last uploaded with `set_static_instances` (re-uploaded only when a prop appears, disappears or shakes; the wind is shader time).
    statics: Vec<StaticMeshInstance>,
    /// FOLIAGE HOOK: the ambient ground cover (set by the loader; its near blades are appended to the static-mesh list).
    pub grass: Option<crate::foliage::Grass>,
    released_static: u8,
    pub models: Vec<ModelId>,
    pub last_stats: Option<FrameStats>,
    /// Frames drawn so far (for the `[slow]` log lines).
    draws: u64,
    pub show_pickaxe: bool,
    /// Original starter meshes and montage/sequence closure loaded off the UI thread.
    pub loadout: Option<fn_pose::loadout::Resources>,
    /// The three original firearm meshes (index = loadout item), uploaded with their original Diffuse / Normals textures.
    guns: Vec<HeldGun>,
    /// Set each frame by the app: the held firearm and the weapon-skeleton clip (sequence package, time) its fire / reload montage plays.
    pub held: Option<(usize, Option<(String, f32)>)>,
    /// World position (metres) of the point the held weapon is attached at: the player's `RightHand` socket (the weapon mesh component is snapped to it, and the weapon's
    /// `Normal` / `LowAmmo` / `Degraded` AudioComponents are attached to that mesh), refreshed by every [`Scene::update`]. Where weapon sounds are emitted.
    pub weapon_origin: Option<Vec3>,
    /// Clip names the pose tree asked for and the library lacked (reported once each).
    reported_missing: std::collections::HashSet<String>,
    /// GUN MATERIAL HOOK: the held firearms drawn with their ORIGINAL material (fn-matshader), see gun_orig.rs. `None` = not installed (gun hidden unless the stand-in is requested).
    pub orig_guns: Option<crate::gun_orig::OrigGuns>,
    /// HELD PICKAXE LIGHTING: the held harvesting tool's mesh package (`SK_Pick`, or the cosmetic's `WeaponMeshOverride`) and, when it is drawn this frame, its
    /// mesh-to-world matrix (`world * RightHand`). With the original gun material on, `pickups_hook::frame` draws it with its cooked material
    /// (`MI_FORT_Melee_Pick` -> `WeaponMat_Base`) through `pickup_orig`, like the held and dropped guns (docs/spec/smallfix_followups.md section 3).
    pub pick_mesh: String,
    pub pick_world: Option<Mat4>,
}

/// One held firearm: renderer model, its own skeleton rig and the clips of exactly its skeleton (never the player's).
struct HeldGun { model: ModelId, rig: CharacterRig, clips: ClipStore, pose: CharacterPose }

/// Gun material, labelled: the original WeaponMat_Base shader permutation is NOT executed (594 programs, pass / vertex-factory ownership unrecovered);
/// the closest honest material is the renderer's Prop shading (constant specular) with the material instance's original Diffuse and Normals`n/// textures. SpecularMasks, CustomizationMask and the emissive map are not applied.
pub fn gun_primitives(res: &fn_pose::loadout::Resources, item: usize) -> Result<Vec<SkinnedPrimitive>, String> { gun_primitives_for(res, res.items.get(item).ok_or("item out of range")?) }
/// [`gun_primitives`] of any firearm item (starter or installed pack: `Resources::gun_item`).
pub fn gun_primitives_for(res: &fn_pose::loadout::Resources, it: &fn_pose::loadout::Item) -> Result<Vec<SkinnedPrimitive>, String> {
    let part = it.mesh.as_ref().ok_or("item has no mesh")?;
    let image = |mat: &str, param: &str| -> Option<image::RgbaImage> {
        let tex = res.materials.get(mat)?.texture_parameters.get(param)?.as_ref()?;
        let t = res.material_textures.get(tex)?;
        image::RgbaImage::from_raw(t.image.width as u32, t.image.height as u32, t.image.to_rgba8())
    };
    let cooked = |mat: &str, param: &str| -> Option<std::sync::Arc<fn_render::mesh::CookedTexture>> {
        let tex = res.materials.get(mat)?.texture_parameters.get(param)?.as_ref()?;
        crate::br_character::cooked_texture(&res.material_textures.get(tex)?.source)
    };
    part.prims.iter().enumerate().map(|(i, p)| {
        let mat = it.mesh_materials.get(i).cloned().flatten().ok_or_else(|| format!("section {i} has no original material"))?;
        let material = MaterialImages { diffuse: image(&mat, "Diffuse"), normal: image(&mat, "Normals"), spec: None, mask: None, ue: None, two_sided: loadout_two_sided(res, &mat), cooked: [cooked(&mat, "Diffuse"), cooked(&mat, "Normals"), None, None] };
        let vertices = (0..p.positions.len()).map(|v| SkinnedVertex { pos: p.positions[v], normal: p.normals[v], uv: p.uvs[v], joints: p.joints[v], weights: p.weights[v] }).collect();
        Ok(SkinnedPrimitive { vertices, indices: p.indices.clone(), material, kind: MaterialKind::Prop })
    }).collect()
}

/// The effective `TwoSided` flag of a loadout material through the loaded chain (root `TwoSided`, an instance's `BasePropertyOverrides.TwoSided` only with
/// `bOverride_TwoSided`, as `fn_pose::br_character::material_two_sided`); a chain the loadout did not load counts as one-sided.
fn loadout_two_sided(res: &fn_pose::loadout::Resources, mat: &str) -> bool {
    let mut over: Option<bool> = None; // the nearest instance override wins
    let mut cur = Some(mat.to_owned());
    for _ in 0..16 {
        let Some(m) = cur.take().and_then(|c| res.materials.get(&c)) else { return false };
        if m.class != "MaterialInstanceConstant" { return over.unwrap_or(m.properties.bool("TwoSided").unwrap_or(false)); }
        if over.is_none() { over = m.properties.fields("BasePropertyOverrides").filter(|o| o.bool("bOverride_TwoSided") == Some(true)).map(|o| o.bool("TwoSided").unwrap_or(false)); }
        cur = m.parent.clone();
    }
    false
}
/// World matrix of the pawn: the old runtime rotates the model by (180 - facing) degrees about Y at the pawn's feet.
const LOOT_SOCKET_BONE: &str = "spine_04";
const LOOT_SOCKET_OFFSET_CM: [f32; 3] = [13.926_156, -12.918_007, 0.0];
pub fn pawn_matrix(sim: &GameSim) -> Mat4 { Mat4::from_rotation_translation(Quat::from_rotation_y((180. - sim.controller.facing).to_radians()), Vec3::new(sim.x, sim.y - EYE, sim.z)) }
pub fn camera_for(sim: &GameSim, w: u32, h: u32) -> CameraParams {
    let v = &sim.cam_view;
    let mut c = CameraParams::from_yaw_pitch(v.eye, v.yaw_deg, v.pitch_deg, w.max(1), h.max(1));
    c.far = fn_camera::far_plane();
    if sim.ads.fov > 0. { c.fov_deg = sim.ads.fov; } // ADS HOOK: the blended hold / targeting mode FOV (AR / shotgun 80, scoped sniper 15)
    crate::hurt_hook::apply_shake(&mut c);
    crate::rocket_hook::apply_shake(&mut c); // HURT HOOK: PlayerTakeDamage_CameraShake
    c
}
/// FNV-1a over the exact bits of a local pose (the same hash fn-sim logs).
pub fn pose_hash(pose: &[(Vec3, Quat)]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut put = |v: f32| { for b in v.to_bits().to_le_bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); } };
    for (p, q) in pose { put(p.x); put(p.y); put(p.z); put(q.x); put(q.y); put(q.z); put(q.w); }
    h
}

/// Splits the live harvestables into the real static-mesh instances (a prop whose kind has an uploaded mesh) and the procedural fallback (everything else that
/// is alive). Destroyed props are in neither list: there is no stump mesh in the 1.8 export, a destroyed tree or rock simply disappears.
pub fn split_props(props: &[fn_combat::Harvestable], mesh_of: impl Fn(&fn_combat::Harvestable) -> Option<fn_render::StaticMeshHandle>) -> (Vec<StaticMeshInstance>, Vec<PropInstance>) {
    let (mut statics, mut procedural) = (vec![], vec![]);
    for p in props.iter().filter(|p| p.alive() && !fn_combat::is_poi_proto(p.proto)) { // POI actors: drawn by statics_render's POI path
        match mesh_of(p) {
            Some(m) => statics.push(StaticMeshInstance::new(m, p.pos).with_yaw(p.yaw).with_scale(p.scale).with_shake(p.shake).with_mirror_x(p.mirror_x)), // MIRRORFIX
            None => procedural.push(PropInstance::from_harvestable(p)),
        }
    }
    (statics, procedural)
}

impl Scene {
    /// `renderer` already holds nothing; the parts' primitives are uploaded here (body, head, pickaxe).
    pub fn new(renderer: Renderer, character: Character, body: &[SkinnedPrimitive], head: &[SkinnedPrimitive], pick: &[SkinnedPrimitive]) -> Scene {
        let palette = |i: usize| character.rig.parts.get(i).map(|p| p.palette_len()).unwrap_or(1);
        let mb = renderer.model_builder();
        let (b, h, p) = (mb.build(body, palette(0), "model body"), mb.build(head, palette(1), "model head"), mb.build(pick, 1, "model pickaxe"));
        Scene::with_models(renderer, character, b, h, p)
    }
    /// LOAD TIME (2026-10-06): [`Scene::new`] with the body / head / pickaxe models already uploaded. The loader builds them with
    /// `Renderer::model_builder` on their own threads while the props upload, instead of last on the critical path (~2.8 s of texture
    /// uploads with CPU mip chains).
    pub fn with_models(mut renderer: Renderer, character: Character, body: fn_render::BuiltModel, head: fn_render::BuiltModel, pick: fn_render::BuiltModel) -> Scene {
        let (b, h, p) = (renderer.create_model(&[], 1), renderer.create_model(&[], 1), renderer.create_model(&[], 1));
        renderer.replace_model(b, body); renderer.replace_model(h, head); renderer.replace_model(p, pick);
        Scene { renderer, character, body: b, head: h, pick: p, props: vec![], real: None, statics: vec![], grass: None, released_static: 0, models: vec![], last_stats: None, draws: 0, show_pickaxe: true, loadout: None, guns: vec![], held: None, weapon_origin: None, reported_missing: Default::default(), orig_guns: None, pick_mesh: fn_pose::loader::PICK_PKG.to_owned(), pick_world: None, local_dissolving: std::cell::Cell::new(false) }
    }
    /// HELD PICKAXE LIGHTING: the pickaxe is drawn by the gun pass this frame; drop its renderer model from the list `update` built.
    pub fn hide_pick_model(&mut self) { let p = self.pick; self.models.retain(|m| *m != p); }
    pub fn pawn_models(&self) -> (ModelId, ModelId, ModelId) { (self.body, self.head, self.pick) }

    /// COSMETIC SWAP: a `Send` GPU upload handle (`loader::cosmetic` uploads the new skin / pickaxe models on a worker thread with it).
    pub fn model_builder(&self) -> fn_render::ModelBuilder { self.renderer.model_builder() }
    /// COSMETIC SWAP: the new skin, atomically (call before `update`, same frame): rig parts (skeletons), the parts' own AnimBP runtime and the body / head GPU models replace the old ones in
    /// their slots (the old GPU resources are dropped). The pose, sockets, held weapon, position and camera are untouched.
    pub fn install_character(&mut self, rig_parts: Vec<PosePart>, part_anim: Option<fn_pose::part_anim::PartAnimRig>, body: fn_render::BuiltModel, head: fn_render::BuiltModel) {
        self.character.rig.parts = rig_parts;
        self.character.part_anim = part_anim;
        self.renderer.replace_model(self.body, body);
        self.renderer.replace_model(self.head, head);
    }
    /// COSMETIC SWAP: the held harvesting tool's mesh.
    pub fn install_pickaxe(&mut self, pick: fn_render::BuiltModel) { self.renderer.replace_model(self.pick, pick); }

    /// GUN MATERIAL HOOK: builds the three firearms with their original material (shader permutation, parameters, textures); on error nothing is installed.
    pub fn install_original_guns(&mut self, install: &std::path::Path) -> Result<(), String> {
        let Some(res) = self.loadout.as_ref() else { return Ok(()) };
        if cfg!(target_arch = "wasm32") { crate::gun_orig::start_starter_prepare(res, install); }
        self.orig_guns = Some(if cfg!(target_arch = "wasm32") { crate::gun_orig::install_lazy(&mut self.renderer, install)? } else { crate::gun_orig::install(&mut self.renderer, res, install)? });
        Ok(())
    }
    pub fn step_original_guns(&mut self) -> bool {
        let (Some(o), Some(res)) = (self.orig_guns.as_mut(), self.loadout.as_ref()) else { return true };
        if !o.starters_missing() { return true; }
        static WAIT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        if !crate::gun_orig::starter_prepare_done() && WAIT.get_or_init(std::time::Instant::now).elapsed().as_secs() < 30 { return false; }
        let t0 = std::time::Instant::now();
        if let Err(e) = o.build_next_starter(&mut self.renderer, res) { eprintln!("[arms] original gun material: {e}"); self.orig_guns = None; let _ = self.install_guns(); return true; }
        eprintln!("[load] original gun built in {:.0} ms", t0.elapsed().as_secs_f32() * 1000.);
        !self.orig_guns.as_ref().is_some_and(|o| o.starters_missing())
    }

    /// Evaluates the animation graph for this tick, uploads the character / prop matrices and rebuilds the model list (draw + shadow casters).
    /// Uploads the three original firearms (loadout items 0..3) and prepares their skeleton evaluators. Runs once, on the UI thread, when the world is built.
    pub fn install_guns(&mut self) -> Result<(), String> {
        let Some(res) = self.loadout.as_ref() else { return Ok(()) };
        let mut guns = vec![];
        for i in 0..fn_pose::loadout_gun::STARTER_GUNS {
            let prims = gun_primitives(res, i)?;
            let part = res.items[i].mesh.clone().ok_or("gun has no mesh")?;
            let clips = res.weapon_clips(i)?;
            let model = self.renderer.create_model(&prims, part.palette_len());
            guns.push(HeldGun { model, rig: CharacterRig::new(vec![part]), clips, pose: CharacterPose::default() });
        }
        self.guns = guns;
        Ok(())
    }

    /// Uploads the firearm `g` that `Resources::install_gun` just added (gun index = the previous gun count) into whichever gun path is active (original material or the stand-in).
    /// A path that has no firearms installed (hidden gun mode) installs nothing. UI thread: GPU uploads.
    /// `res` is the resource set the pack was installed into (the app takes it out of `loadout` for the duration of the install); `new_sequences` / `new_cores` are what `Resources::install_gun` added: the
    /// player-skeleton clips of the weapon's animation set join the character's clip library (`LayeredClips`).
    pub fn install_extra_gun(&mut self, res: &fn_pose::loadout::Resources, g: usize, new_sequences: &[String], new_cores: &[String]) -> Result<(), String> {
        let (extra, conflicts) = res.new_player_clips(new_sequences, new_cores);
        for c in conflicts { eprintln!("[weapon] {c}"); }
        if !extra.clips.is_empty() {
            let base = std::mem::replace(&mut self.character.clips, Box::new(ClipStore::new()));
            eprintln!("[weapon] {} player clips added to the character's library for firearm {g}", extra.clips.len());
            self.character.clips = Box::new(crate::weapon_runtime::LayeredClips { base, extra });
        }
        if let Some(o) = self.orig_guns.as_mut() { return o.add(&mut self.renderer, res, g); }
        if self.guns.is_empty() { return Ok(()); }
        if g != self.guns.len() { return Err(format!("gun index {g} is out of order (have {})", self.guns.len())); }
        let item = res.gun_item(g).ok_or("gun index out of range")?;
        let prims = gun_primitives_for(res, item)?;
        let part = item.mesh.clone().ok_or("gun has no mesh")?;
        let clips = res.gun_clips(g)?;
        let model = self.renderer.create_model(&prims, part.palette_len());
        self.guns.push(HeldGun { model, rig: CharacterRig::new(vec![part]), clips, pose: CharacterPose::default() });
        Ok(())
    }

    pub fn update(&mut self, sim: &mut GameSim) -> &[ModelId] {
        let world = pawn_matrix(sim);
        self.models.clear();
        self.character.evaluate(sim);
        for m in self.character.pose.missing_clips.iter() { if self.reported_missing.insert(m.clone()) { eprintln!("[anim] missing clip requested by the pose tree: {m}"); } }
        let pose = &self.character.pose;
        if let Some(p) = pose.parts.first() { self.renderer.update_model(self.body, world, &p.skin_matrices); }
        if let Some(p) = pose.parts.get(1) { self.renderer.update_model(self.head, world, &p.skin_matrices); }
        let local_death = crate::death_hook::local_time();
        if local_death.is_some() || self.local_dissolving.get() {
            let (h, r) = crate::mp_hook::socket_bones(self, pose, world);
            self.local_dissolving.set(crate::elim_fx::remote_dissolve(&self.renderer, &[self.body, self.head], h, r, local_death));
            if let Some(t) = local_death { crate::elim_fx::shell_frame(crate::elim_fx::LOCAL_SHELL_SLOT, t, world, pose.parts.iter().take(2).map(|p| p.skin_matrices.clone()).collect(), h, r); }
        }
        // HEAL FX: the displayed `head` bone (socket b_MF_Head) for P_Consume_Item (heal_fx.rs); UE yaw of the mesh = facing - 90 (foot_fx.rs)
        if let (Some(p), Some(i)) = (pose.parts.first(), self.character.rig.parts.first().and_then(|r| r.tree.bone_index("head"))) {
            if let Some(m) = p.bone_mesh.get(i) {
                let h = (world * *m).w_axis.truncate();
                let e = sim.cam_view.eye;
                crate::heal_fx::on_head_pose(fn_particles::sim::game_to_ue([h.x, h.y, h.z]), sim.controller.facing - 90., fn_particles::sim::game_to_ue([e.x, e.y, e.z]));
            }
        }
        if let (Some(p), Some(i)) = (pose.parts.first(), self.character.rig.parts.first().and_then(|r| r.tree.bone_index(LOOT_SOCKET_BONE))) {
            if let Some(m) = p.bone_mesh.get(i) { crate::pickups_hook::set_loot_point((world * *m).transform_point3(Vec3::from(fn_mesh::gltf::ue_to_gltf_position(LOOT_SOCKET_OFFSET_CM)))); }
        }
        let socket = pose.socket("RightHand").unwrap_or(Mat4::IDENTITY);
        // In the original pawn the weapon mesh component snaps to RightHand
        // after the complete post-process graph: HandIKRetargeting, CopyBone,
        // then the hand IK nodes.  Guns, the pickaxe and weapon-origin use
        // that one final socket basis.
        let weapon_socket = socket;
        // `OrigGuns` renders the recovered mesh/material path itself.  The
        // older `guns` list below is strictly a fallback; drawing both places
        // two copies of a firearm on the same socket and makes the hands look
        // detached from the visible model.
        let using_original_gun = self.orig_guns.as_mut().map(|o| {
            o.update(if sim.cam_view.hide_pawn { None } else { self.held.clone() }, world * weapon_socket);
            if let (Some((g, _)), false) = (&self.held, sim.cam_view.hide_pawn) { crate::muzzle_hook::on_gun_pose(*g, world * weapon_socket, &|b| o.bone_matrix(*g, b)); } // MUZZLE HOOK
            o.is_drawing()
        }).unwrap_or(false);
        self.renderer.update_model(self.pick, world, &[socket]);
        self.pick_world = (!sim.cam_view.hide_pawn && self.show_pickaxe).then(|| world * socket); // HELD PICKAXE LIGHTING (the rigid pick's one joint = RightHand)
        self.weapon_origin = pose.socket("RightHand").map(|s| (world * s).w_axis.truncate());
        self.sync_props(sim);
        if !sim.cam_view.hide_pawn {
            self.models.extend([self.body, self.head]);
            if self.show_pickaxe { self.models.push(self.pick); }
            if !using_original_gun { if let Some((g, clip)) = self.held.clone() {
                if let Some(h) = self.guns.get_mut(g) {
                    // The weapon's own montage drives its skeleton; without one it
                    // rests in its bind pose. The component itself is snapped
                    // to the final player RightHand socket.
                    let tree = match &clip { Some((c, t)) => Node::clip(c, *t, false), None => Node::Ref };
                    h.rig.evaluate_into(&tree, &h.clips, &mut h.pose);
                    if let Some(p) = h.pose.parts.first() { self.renderer.update_model(h.model, world * weapon_socket, &p.skin_matrices); }
                    { let (rig, pose) = (&h.rig, &h.pose); crate::muzzle_hook::on_gun_pose(g, world * weapon_socket, &|b| rig.parts.first()?.tree.bone_index(b).and_then(|i| pose.parts.first()?.bone_mesh.get(i).copied())); } // MUZZLE HOOK
                    self.models.push(h.model);
                }
            }}
        }
        &self.models
    }

    /// Uploads the harvestables when (and only when) the live prop list differs from what the renderer already holds.
    fn sync_props(&mut self, sim: &GameSim) {
        if let Some(real) = self.real.as_mut() { crate::props::upgrade_depth_only_kinds(&mut self.renderer, real, &sim.world.props); } // VRAM: a kind the real-shader path fails gets its full textures
        let Some(real) = self.real.as_ref() else {
            // no real assets: every harvestable is the procedural placeholder
            let now = PropInstance::from_world(&sim.world);
            if crate::building_hook::placeholder_dirty() || now != self.props { crate::building_hook::upload_props(&mut self.renderer, &now); self.props = now; } // BUILDING HOOK: PLACEHOLDER piece triangles ride along in the prop pass
            return;
        };
        let (statics, procedural) = split_props(&sim.world.props, |h| real.mesh_of(h));
        // FOLIAGE HOOK: the grass near the camera rides along in the same static-mesh list (rebuilt when the camera moved a few metres)
        // GRASS: once the real-shader path draws the ground cover (grass_render.rs) the blades leave this list
        if let Some(g) = self.grass.as_mut() { g.conform(); } // TERRAIN: the blades onto the landscape once it is installed
        let grass_fx = crate::statics_render::grass_active();
        let grass_changed = !grass_fx && self.grass.as_mut().map_or(false, |g| g.update(sim.cam_view.eye));
        let real_path_changed = crate::statics_render::take_changed(); // STATIC MATERIALS: the kinds drawn with their real cooked shaders (statics_render.rs) leave this list
        if real_path_changed || grass_changed || statics != self.statics {
            let mut all: Vec<StaticMeshInstance> = statics.iter().filter(|s| !crate::statics_render::handles(s.mesh)).cloned().collect(); // STATIC MATERIALS
            if let Some(g) = &self.grass { if !grass_fx { all.extend_from_slice(g.instances()); } }
            self.renderer.set_static_instances(&all);
            self.statics = statics;
        }
        if self.released_static & 1 == 0 && crate::statics_render::props_built() {
            self.released_static |= 1;
            let hs = crate::statics_render::handled_meshes();
            let (n, b) = self.renderer.release_static_meshes(&hs);
            eprintln!("[mem] fallback static path released for {} real-shader kinds: {n} materials, {:.0} MB GPU", hs.len(), b as f64 / 1048576.);
        }
        if self.released_static & 2 == 0 && grass_fx {
            self.released_static |= 2;
            if let Some(g) = &self.grass {
                let mut hs: Vec<fn_render::StaticMeshHandle> = g.instances().iter().map(|i| i.mesh).collect();
                hs.sort_by_key(|h| h.0);
                hs.dedup();
                let (n, b) = self.renderer.release_static_meshes(&hs);
                eprintln!("[mem] fallback grass released: {} meshes, {n} materials, {:.0} MB GPU", hs.len(), b as f64 / 1048576.);
            }
        }
        if crate::building_hook::placeholder_dirty() || procedural != self.props { crate::building_hook::upload_props(&mut self.renderer, &procedural); self.props = procedural; } // BUILDING HOOK: PLACEHOLDER piece triangles ride along in the prop pass
    }

        /// Draws the scene prepared by [`Scene::update`] into `target`.
    pub fn draw(&mut self, camera: &CameraParams, time: f32, dt: f32, options: &RenderOptions, target: &TextureView) {
        let t = std::time::Instant::now();
        crate::platform::timed("particles submit", || crate::particles::submit(&mut self.renderer, camera, dt)); // FIRST USE: [slow] when a first spawn builds. PARTICLE HOOK: tick the live particle systems, hand their draw data to the renderer
        if let Some(o) = self.orig_guns.as_mut() { crate::platform::timed("gun submit", || { o.submit(&mut self.renderer, camera, time); crate::pickup_orig::submit(&mut self.renderer, camera, time); }); } // GUN MATERIAL HOOK: the original-material gun draws of this frame (+ PICKUP ITEM LIGHTING: the dropped weapons)
        { let (vw, vh) = fn_render::quality::scaled_view_size(camera.width, camera.height, options.screen_percentage); crate::scope_hook::submit(&mut self.renderer, &CameraParams { width: vw, height: vh, ..*camera }, time); } // SCOPE HOOK: the scoped weapon's post-process material + mesh of this frame
        let stats = self.renderer.render(&FrameParams { camera, time, dt, models: &self.models, options }, target);
        self.last_stats = Some(stats);
        let ms = t.elapsed().as_secs_f32() * 1000.;
        // the first frame at a new size re-creates the HDR / depth / mip targets, the first with FXAA creates the LDR target, a new shadow size the cascades
        if ms > crate::gfx::SLOW_MS * 2. {
            // PERF: the renderer's three most expensive CPU steps of that frame (FrameStats::passes: graph encodes + `prep *` steps)
            let mut top: Vec<(&str, f32)> = self.last_stats.as_ref().map(|s| s.passes.iter().map(|p| (p.name.as_str(), p.encode_ms)).collect()).unwrap_or_default();
            top.sort_by(|a, b| b.1.total_cmp(&a.1));
            let top: Vec<String> = top.iter().take(3).map(|(n, v)| format!("{n} {v:.1}")).collect();
            eprintln!("[slow] scene render {}x{} took {ms:.1} ms (frame {}; {})", camera.width, camera.height, self.draws, top.join(", "));
        }
        self.draws += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_render::MaterialKind;
    #[test] fn mat_slots() {
        let s = parse_mat("Diffuse=A_C\nNormal=A_N\nSpecPower=A_S\nOther[0]=A_M\nOther[1]=T_NoisePack_01\n");
        assert_eq!(s, MatSlots { diffuse: Some("A_C".into()), normal: Some("A_N".into()), spec: Some("A_S".into()), mask: Some("A_M".into()) });
        assert_eq!(parse_mat("Diffuse=X\nOther[0]=T_NoisePack_01\n").mask, None);
    }
    #[test] fn rigid_binds_everything_to_joint_zero() {
        let mut p = vec![SkinnedPrimitive { vertices: vec![SkinnedVertex { joints: [3, 4, 5, 6], weights: [0.4; 4], ..Default::default() }], indices: vec![0, 0, 0], material: MaterialImages::default(), kind: MaterialKind::Prop }];
        make_rigid(&mut p); assert_eq!(p[0].vertices[0].joints, [0; 4]); assert_eq!(p[0].vertices[0].weights, [1., 0., 0., 0.]);
    }
    #[test] fn props_go_through_the_static_prop_path_with_the_live_shake() {
        use fn_combat::{Harvestable, Kind};
        let mut g = GameSim::new(); g.step(&Default::default(), 1. / 60.);
        let inst = PropInstance::from_world(&g.world);
        assert_eq!(inst.len(), g.world.props.iter().filter(|p| p.alive()).count());
        assert!(inst.iter().all(|i| i.scale == 1. && i.yaw == 0.), "the sandbox props are placed exactly like the old runtime (no yaw / scale)");
        let mut p = Harvestable::new(Kind::Tree, Vec3::new(1., 0., 2.)); p.shake = 0.1;
        let i = PropInstance::from_harvestable(&p); assert_eq!((i.pos, i.shake), (p.pos, 0.1));
        // the same expansion the renderer does equals fn_combat's per-vertex sway (what the old runtime drew)
        let v = fn_render::PropTemplates::default().build(&[i]);
        let first = fn_combat::prop_mesh(Kind::Tree)[0].0[0];
        assert!((Vec3::from(v[0].pos) - fn_combat::prop_vertex(&p, first)).length() < 1e-4);
    }    #[test] fn pawn_matrix_places_the_feet() {
        let mut g = GameSim::new(); g.step(&Default::default(), 1. / 60.);
        assert!((pawn_matrix(&g).transform_point3(Vec3::ZERO) - Vec3::new(g.x, g.y - EYE, g.z)).length() < 1e-5);
    }
    #[test] fn camera_params_follow_the_view() {
        let mut g = GameSim::new(); g.step(&Default::default(), 1. / 60.);
        let c = camera_for(&g, 1280, 720);
        assert_eq!(c.eye, g.cam_view.eye); assert!((c.forward - g.cam_view.forward).length() < 1e-3, "{:?} {:?}", c.forward, g.cam_view.forward); assert_eq!((c.width, c.height), (1280, 720));
    }
    #[cfg(feature = "sim-compare")]
    #[test] fn pose_hash_matches_the_fn_sim_hash() {
        let pose = vec![(Vec3::new(1., 2., 3.), Quat::IDENTITY), (Vec3::ZERO, Quat::from_rotation_y(0.5))];
        assert_eq!(pose_hash(&pose), fn_sim::rig::pose_hash(&pose));
    }
}


