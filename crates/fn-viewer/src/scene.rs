//! The viewer scene: character body + head (skinned glTF), the pickaxe attached to `weapon_r`, driven by a rest pose or an md5anim clip.
use crate::assets::AssetIndex;
use crate::character::{self, Clip, LoadedModel, SkelModel};
use fn_render::{Gpu, MaterialKind, ModelId, Renderer, RendererAssets, SkyAssets};
use glam::{Mat4, Quat, Vec3};

/// Pawn spawn of the prototype (x, z) and the pawn's rotation about Y: model faces the camera-forward direction (-Z) for facing 0.
pub const SPAWN_FEET: Vec3 = Vec3::new(0., 0., 12.);

pub struct Part {
    pub id: ModelId,
    pub skel: SkelModel,
}
pub struct Scene {
    pub renderer: Renderer,
    pub body: Part,
    pub head: Part,
    pub pick: ModelId,
    pub clip: Option<Clip>,
    pub noise: Option<Clip>,
    pub feet: Vec3,
    pub facing_deg: f32,
    pub use_clip: bool,
    pub use_noise: bool,
    /// Placeholder trees / rocks (`fn_combat::World::sandbox`), uploaded to the renderer by `animate`.
    pub props: Option<fn_combat::World>,
    /// Extra placed props (`--prop-count n`), drawn in addition to the sandbox world.
    pub extra_props: Vec<fn_render::PropInstance>,
    /// Textured static props (`--props` / `--prop-grid`); when present the procedural sandbox props are not drawn.
    pub statics: Vec<fn_render::StaticMeshInstance>,
}
pub struct SceneConfig { pub use_clip: bool, pub use_noise: bool, pub props: bool, pub extra_props: usize, pub static_dir: Option<std::path::PathBuf>, pub prop_grid: usize, pub prop_seed: u32, pub prop_kind: Vec<String> }

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now();
    let r = f();
    eprintln!("[load] {label}: {:.2}s", t.elapsed().as_secs_f32());
    r
}

impl Scene {
    pub fn load(gpu: &Gpu, index: &AssetIndex, output_format: fn_render::wgpu::TextureFormat, cfg: &SceneConfig) -> Result<Scene, String> {
        let sky = timed("sky assets", || SkyAssets::load(&index.root.join("sky")))?;
        let ground_texture = timed("grass texture", || index.load_rgba("T_Grasslands_D.png"))?;
        let mut renderer = timed("renderer + pipelines", || Renderer::new(&gpu.device, &gpu.queue, output_format, &RendererAssets { sky, ground_texture, ground_mips: None }))?;
        let body_m: LoadedModel = timed("body gltf + textures", || character::load_gltf(index, "SK_M_Med_Soldier_04.gltf", MaterialKind::Character))?;
        let head_m: LoadedModel = timed("head gltf + textures", || character::load_gltf(index, "SK_M_MED_Head_01_ATH.gltf", MaterialKind::Character))?;
        let mut pick_m = timed("SK_Pick gltf + textures", || character::load_gltf(index, "SK_Pick.gltf", MaterialKind::Prop))?;
        character::make_rigid(&mut pick_m.primitives);
        let body_id = timed("upload body", || renderer.create_model(&body_m.primitives, body_m.skel.joints.len()));
        let head_id = timed("upload head", || renderer.create_model(&head_m.primitives, head_m.skel.joints.len()));
        let pick = renderer.create_model(&pick_m.primitives, 1);
        let clip = if cfg.use_clip { Some(Clip::load(index.find("M_Melee_TwoHanded.md5anim")?)?) } else { None };
        let noise = if cfg.use_clip && cfg.use_noise { index.find("Idle_Noise.md5anim").ok().and_then(|p| Clip::load(p).ok()) } else { None };
        let statics = if cfg.prop_grid > 0 || cfg.static_dir.is_some() {
            let kinds = timed("static props", || crate::static_props::load_kinds(&mut renderer, cfg.static_dir.as_deref(), &cfg.prop_kind))?;
            let g = crate::static_props::grid(&kinds, cfg.prop_grid, cfg.prop_seed);
            for i in g.iter().take(4) { eprintln!("[props] mesh {} at ({:.2}, {:.2}) yaw {:.2} scale {:.2}", i.mesh.0, i.pos.x, i.pos.z, i.yaw, i.scale); }
            g
        } else { vec![] };
        Ok(Scene {
            renderer, body: Part { id: body_id, skel: body_m.skel }, head: Part { id: head_id, skel: head_m.skel }, pick,
            clip, noise, feet: SPAWN_FEET, facing_deg: 0., use_clip: cfg.use_clip, use_noise: cfg.use_noise,
            props: if cfg.props && statics.is_empty() { Some(fn_combat::World::sandbox()) } else { None },
            extra_props: crate::props::scatter(cfg.extra_props, 0x5EED), statics,
        })
    }
    pub fn model_ids(&self) -> [ModelId; 3] { [self.body.id, self.head.id, self.pick] }
    /// Model-to-world: the old runtime rotates the model by (180 - facing) degrees about Y at the pawn's feet.
    pub fn world_matrix(&self) -> Mat4 { Mat4::from_rotation_translation(Quat::from_rotation_y((180. - self.facing_deg).to_radians()), self.feet) }
    /// Poses the skeletons at animation time `t` and uploads the skin matrices.
    pub fn animate(&mut self, t: f32) {
        let world = self.world_matrix();
        let mut instances = self.props.as_ref().map(fn_render::PropInstance::from_world).unwrap_or_default();
        instances.extend_from_slice(&self.extra_props);
        self.renderer.set_prop_instances(&instances);
        self.renderer.set_static_instances(&self.statics);
        let (clip, noise) = (if self.use_clip { self.clip.as_ref() } else { None }, if self.use_noise { self.noise.as_ref() } else { None });
        let body_globals = self.body.skel.globals(clip, noise, t);
        let head_globals = self.head.skel.globals(clip, noise, t);
        self.renderer.update_model(self.body.id, world, &self.body.skel.skin(&body_globals));
        self.renderer.update_model(self.head.id, world, &self.head.skel.skin(&head_globals));
        let attach = self.body.skel.bone("weapon_r").map(|i| body_globals[i] * Mat4::from_rotation_y(character::PICK_SOCKET_YAW)).unwrap_or(Mat4::IDENTITY);
        self.renderer.update_model(self.pick, world, &[attach]);
    }
    /// Chest height of the standing pawn (orbit target).
    pub fn focus(&self) -> Vec3 { self.feet + Vec3::Y * 1.0 }
}
