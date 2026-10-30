//! `--script <file|builtin:NAME>`: an input timeline replayed through the real game loop (fixed time step, no real devices, no mouse capture).
//!
//! ```text
//! # comment (also ';')
//! size 1280x720          client size of the window for this run
//! fps 60                 fixed frame rate (dt = 1/fps)
//! props sandbox|none|tree|real|realtree|realrock    sandbox (default) = the three procedural harvestables of the old runtime, none = empty baseplate, tree = one procedural tree 1.8 m ahead,
//!                        real = the REAL 1.8 props around the anchor (props.rs), realtree / realrock = one real Athena_Tree_Large_1 / Athena_Prop_Rocks_StoneLarge01 1.4 m ahead
//!                        (the real ones need the exported props; without them the sandbox stays)
//! end 900                last frame + 1 (default: last entry + 30)
//! 20-100 forward         held for frames 20..100   (`A-B`: A inclusive, B exclusive; a single number is one frame)
//! 65 jump                one frame
//! 300-375 forward sprint
//! 410-480 forward strafe=0.41421356        axis values: fwd=<-1..1> strafe=<-1..1> (absolute, added to the key axes)
//! 0-60 yaw=90 pitch=-10  absolute control yaw / pitch every frame      turn=<deg>  look=<deg>   per-frame yaw / pitch delta
//! 10-130 pad=1:0         right stick (x right, y up, -1..1) held for the range, through the 1.8 gamepad look chain (logged as `[pad]`)
//! 5 equip                start the Melee_Equip montage          5 reset      back to the spawn          9999 quit
//! 100 tp=54272:73216:3   ALL POIS: teleport to UE world (X, Y) cm, UP m above the highest collision surface there       100 ghost / fly / walk   free flight modes
//! 100 resize 1920x1080   resize the window client area
//! 10 shot idle           capture <out>/idle.png after frame 10 has been rendered
//! 30 key=q               build / edit keys (key-binding rows, held for the range: a single frame = press then release next frame):
//! 40-80 key=fire         switch|q f1..f5 fire|lmb material|rmb edit|g confirm reset rotate|r improve -> BuildInput::update, as the real keys;
//!                        key=1..6 = the weapon slot keys (HotbarKey1..6) through the live quickbar_press path (selects the slot, leaves build mode)
//! 90-120 aimtile=2       aim the view at edit tile 2 (aimarrow=K: stair arrow K) of the running edit session every frame of the range
//! ```
//! Actions: `forward back left right jump crouch sprint attack` map to the same `FrameInput` fields the keyboard path fills (`jump`,
//! `crouch_key`, `sprint_key`, `attack`), so hold/toggle latches and the jump/crouch interplay are exercised exactly like real play.
use fn_game::{FrameInput, GameSim};

#[derive(Clone, Debug, PartialEq)]
pub enum Action { Slot(usize), Reload, Health(f32), Shield(f32), Give(usize, u32), Forward, Back, Left, Right, Jump, Crouch, Sprint, Attack, Equip, Reset, Quit, Fwd(f32), Strafe(f32), Yaw(f32), Pitch(f32), Turn(f32), Look(f32), Shot(String), Resize(u32, u32),
    /// MOUSE LOOK: raw mouse counts (dx, dy; dy > 0 = mouse moved down) per frame, run through the live 1.8 mouse chain (`mouse=DX:DY`).
    Mouse(i32, i32),
    /// PICKUP CARD: the console `spawn <item> <count>` (a pickup dropped in front of the player): `spawn=QUERY:COUNT`.
    Spawn(String, u32),
    /// GAMEPAD LOOK: right stick deflection (x right, y up; -1..1) held this frame, run through the 1.8 stick chain (`pad=RX:RY`, `pad.rs` PadLook).
    Pad(f32, f32),
    /// SCRIPT BUILD KEYS: a key-binding row (`fn_settings::A_*`) held this frame (`key=NAME`, see [`build_key`]); the row's bound keys answer
    /// `BuildInput::update`'s key poll, so the build / edit input layer sees the same press / release edges as from the keyboard and mouse.
    Key(usize),
    /// SCRIPT BUILD KEYS: aim the view at a tile / stair arrow of the running edit session this frame (`aimtile=K` / `aimarrow=K`).
    Aim(fn_building::edit::EditTarget),
    /// INVENTORY HOOK: an inventory-screen step (one frame), see [`InvCmd`].
    Inv(InvCmd),
    /// ALL POIS: teleport the feet to UE world (X, Y) cm (the coordinates of `props/poi/index.json`, through the landscape's mapping), standing `UP` metres
    /// above the highest collision surface there (`GameSim::floor_below`: a POI roof / floor / terrace, else the landscape): `tp=X:Y[:UP]`.
    Tp(f32, f32, f32),
    /// CONTAINERS: `tpz=X:Y:Z`: feet to UE (X, Y) on the first collision surface below UE height Z (cm): indoor spots (a chest inside a house) that `tp`
    /// (highest surface = the roof) cannot reach.
    TpZ(f32, f32, f32),
    /// ALL POIS: the console's `walk` / `fly` / `ghost` (`fn_game::sim::FlyMode`) for overview shots: `walk`, `fly`, `ghost`.
    Fly(fn_game::sim::FlyMode),
    KillFeed,
    Die(fn_ui::screens::kill_feed::DeathCause) }

/// INVENTORY HOOK script steps (`inv=open|close|equip|drop`, `invclick=T`, `invdbl=T`, `invdrag=T>T` (`invsplit=` = with Shift), `invq=confirm|cancel|max|V`, `mats=W:S:M` (sets the counters directly: `World::set_mats`, not the harvest path)), targets `slotN` (equip slot 1..5),
/// `tileK` (K-th backpack tile, 0-based), `drop` (the drop slot). They run through the same panel input path as the mouse (press / move / release at the target centres).
#[derive(Clone, Debug, PartialEq)]
pub enum InvCmd { Open, Close, MapOpen, MapClose, EquipKey, DropKey, Click(InvTarget), DoubleClick(InvTarget), Drag { from: InvTarget, to: InvTarget, shift: bool }, Mats([u32; 3]),
    /// INVENTORY FIX2: `invq=confirm|cancel|max|<0..1>`: a click on the QuantitySelector_C button / a press on its slider at that value.
    Qsel(QselCmd),
    /// INVENTORY FIX2: `invwheel=N`: N mouse-wheel notches over the tile view (negative = down).
    Wheel(f32) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QselCmd { Confirm, Cancel, Max, Slider(f32) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvTarget { Slot(u8), Tile(usize), Drop }
fn inv_target(s: &str, n: usize) -> Result<InvTarget, String> {
    if s == "drop" { return Ok(InvTarget::Drop); }
    if let Some(k) = s.strip_prefix("slot").and_then(|v| v.parse::<u8>().ok()).filter(|k| (1..=5).contains(k)) { return Ok(InvTarget::Slot(k)); }
    if let Some(k) = s.strip_prefix("tile").and_then(|v| v.parse::<usize>().ok()) { return Ok(InvTarget::Tile(k)); }
    Err(format!("line {n}: inventory target {s:?} (slot1..slot5, tileK, drop)"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Props { Sandbox, None, Tree, Real, RealTree, RealRock }

#[derive(Clone, Debug)]
struct Entry { from: u32, to: u32, action: Action }

#[derive(Clone, Debug)]
pub struct Script { pub name: String, pub size: Option<(u32, u32)>, pub fps: f32, pub props: Props, end: Option<u32>, entries: Vec<Entry>, pub assume: bool, pub standin_gun: bool, /// INVENTORY HOOK `loadout br`: the 1.8 BR spawn inventory instead of the dev loadout scripts get by default.
    pub br_loadout: bool }

/// SCRIPT BUILD KEYS: `key=NAME` -> the key-binding row the build / edit input layer reads (build_input.rs; defaults = ConfigD_Athena: Q, F1..F5, LMB, RMB, G, R, F).
/// `edit` and `confirm` are the same row (CompleteBuildingEditInteraction shares the "Building Edit" key), `material` and `reset` share RMB in the defaults.
pub fn build_key(name: &str) -> Option<usize> {
    use fn_settings::*;
    Some(match name {
        "switch" | "q" => A_SWITCH_QUICKBAR, "fire" | "lmb" => A_FIRE, "material" | "rmb" => A_CHANGE_MATERIAL, "edit" | "g" | "confirm" => A_PERFORM_BUILDING_EDIT_INTERACTION,
        "reset" => A_BUILDING_EDIT_RESET, "rotate" | "r" => A_ROTATE, "improve" => A_PERFORM_BUILDING_IMPROVEMENT_INTERACTION,
        "use" | "e" => A_USE, // INTERACT: the Use row (interact::set_script_use)
        "target" | "ads" => A_TARGET,
        // HUD FIX: `key=1..6` = HotbarKey1..6 (the weapon slot rows, ConfigD_Athena keys 1..6), polled by the live `Input::quickbar_press` path in app.rs
        d if d.len() == 1 && ("1"..="6").contains(&d) => A_PRIMARY_QUICKBAR_SLOT1 + d.parse::<usize>().ok()? - 1,
        f => A_SECONDARY_QUICKBAR_SLOT_F1 + f.strip_prefix('f')?.parse::<usize>().ok().filter(|k| (1..=crate::build_input::BUILD_SLOTS).contains(k))? - 1,
    })
}

fn num(s: &str, what: &str, line: usize) -> Result<f32, String> { s.parse::<f32>().map_err(|_| format!("line {line}: bad number in {what:?}")).and_then(|v| if v.is_finite() { Ok(v) } else { Err(format!("line {line}: {what:?} is not finite")) }) }

impl Script {
    pub fn parse(name: &str, text: &str) -> Result<Script, String> {
        let mut s = Script { name: name.to_owned(), size: None, fps: 60., props: Props::Sandbox, end: None, entries: vec![], assume: false, standin_gun: false, br_loadout: false };
        for (n, raw) in text.lines().enumerate() {
            let n = n + 1;
            let line = raw.split(['#', ';']).next().unwrap_or("").trim();
            if line.is_empty() { continue; }
            let toks: Vec<&str> = line.split_whitespace().collect();
            match toks[0] {
                "size" => { let (w, h) = crate::cli::parse_size(toks.get(1).ok_or(format!("line {n}: size needs WxH"))?).map_err(|e| format!("line {n}: {e}"))?; s.size = Some(crate::platform::check_client_size(w, h, "size").map_err(|e| format!("line {n}: {e}"))?); }
                "fps" => { let f = num(toks.get(1).ok_or(format!("line {n}: fps needs a value"))?, "fps", n)?; if !(1. ..=1000.).contains(&f) { return Err(format!("line {n}: fps {f} out of range")); } s.fps = f; }
                "props" => s.props = match toks.get(1).copied() { Some("sandbox") => Props::Sandbox, Some("none") => Props::None, Some("tree") => Props::Tree, Some("real") => Props::Real, Some("realtree") => Props::RealTree, Some("realrock") => Props::RealRock, o => return Err(format!("line {n}: unknown props {o:?}")) },
                "assume" => s.assume = true, "standin-gun" => s.standin_gun = true,
                "loadout" => s.br_loadout = match toks.get(1).copied() { Some("br") => true, Some("dev") => false, o => return Err(format!("line {n}: loadout br|dev, got {o:?}")) }, // INVENTORY HOOK
                "end" => s.end = Some(toks.get(1).and_then(|v| v.parse().ok()).ok_or(format!("line {n}: end needs a frame number"))?),
                first => {
                    let (from, to) = match first.split_once('-') {
                        Some((a, b)) => (a.parse::<u32>().map_err(|_| format!("line {n}: bad frame {a:?}"))?, b.parse::<u32>().map_err(|_| format!("line {n}: bad frame {b:?}"))?),
                        None => { let a = first.parse::<u32>().map_err(|_| format!("line {n}: expected a frame or range, got {first:?}"))?; (a, a.checked_add(1).ok_or_else(|| format!("line {n}: frame {a} is too large"))?) }
                    };
                    if to <= from { return Err(format!("line {n}: empty range {first}")); }
                    let mut i = 1;
                    if toks.len() < 2 { return Err(format!("line {n}: no action after {first}")); }
                    while i < toks.len() {
                        let t = toks[i];
                        let action = match t {
                            "forward" => Action::Forward, "back" => Action::Back, "left" => Action::Left, "right" => Action::Right, "jump" => Action::Jump, "crouch" => Action::Crouch,
                            "walk" => Action::Fly(fn_game::sim::FlyMode::Walk), "fly" => Action::Fly(fn_game::sim::FlyMode::Fly), "ghost" => Action::Fly(fn_game::sim::FlyMode::Ghost), // ALL POIS
                            "killfeed" => Action::KillFeed, "sprint" => Action::Sprint, "attack" | "fire" => Action::Attack, "reload" => Action::Reload, "equip" => Action::Equip, "reset" => Action::Reset, "quit" => Action::Quit,
                            "shot" => { i += 1; Action::Shot(toks.get(i).ok_or(format!("line {n}: shot needs a name"))?.to_string()) }
                            "resize" => { i += 1; let (w, h) = crate::cli::parse_size(toks.get(i).ok_or(format!("line {n}: resize needs WxH"))?).map_err(|e| format!("line {n}: {e}"))?; let (w, h) = crate::platform::check_client_size(w, h, "resize").map_err(|e| format!("line {n}: {e}"))?; Action::Resize(w, h) }
                            _ => match t.split_once('=') {
                                Some(("inv", v)) => Action::Inv(match v { "open" => InvCmd::Open, "close" => InvCmd::Close, "equip" => InvCmd::EquipKey, "drop" => InvCmd::DropKey, o => return Err(format!("line {n}: inv=open|close|equip|drop, got {o:?}")) }),
                                Some(("map", v)) => Action::Inv(match v { "open" => InvCmd::MapOpen, "close" => InvCmd::MapClose, o => return Err(format!("line {n}: map=open|close, got {o:?}")) }),
                                Some(("invclick", v)) => Action::Inv(InvCmd::Click(inv_target(v, n)?)), Some(("invdbl", v)) => Action::Inv(InvCmd::DoubleClick(inv_target(v, n)?)),
                                Some((k @ ("invdrag" | "invsplit"), v)) => { let (a, b) = v.split_once('>').ok_or(format!("line {n}: {k} needs FROM>TO"))?; Action::Inv(InvCmd::Drag { from: inv_target(a, n)?, to: inv_target(b, n)?, shift: k == "invsplit" }) }
                                Some(("invwheel", v)) => Action::Inv(InvCmd::Wheel(num(v, "invwheel", n)?)),
                                Some(("invq", v)) => Action::Inv(InvCmd::Qsel(match v { "confirm" => QselCmd::Confirm, "cancel" => QselCmd::Cancel, "max" => QselCmd::Max, o => QselCmd::Slider(num(o, "invq", n)?) })),
                                Some(("mats", v)) => { let p: Vec<u32> = v.split(':').map(|x| x.parse::<u32>()).collect::<Result<_, _>>().map_err(|_| format!("line {n}: mats=W:S:M"))?; if p.len() != 3 { return Err(format!("line {n}: mats=W:S:M")); } Action::Inv(InvCmd::Mats([p[0], p[1], p[2]])) }
                                Some(("fwd", v)) => Action::Fwd(num(v, t, n)?), Some(("strafe", v)) => Action::Strafe(num(v, t, n)?), Some(("yaw", v)) => Action::Yaw(num(v, t, n)?),
                                Some(("pitch", v)) => Action::Pitch(num(v, t, n)?), Some(("slot", v)) => Action::Slot(v.parse::<usize>().ok().filter(|k| (1..=6).contains(k)).ok_or(format!("line {n}: slot needs 1..6"))? - 1), Some(("health", v)) => Action::Health(num(v, t, n)?), Some(("shield", v)) => Action::Shield(num(v, t, n)?),
                                Some(("spawn", v)) => { let (q, c) = v.rsplit_once(':').ok_or(format!("line {n}: spawn=QUERY:COUNT"))?; Action::Spawn(q.replace('_', " "), c.parse().map_err(|_| format!("line {n}: bad count"))?) } Some(("give", v)) => { let (name, qty) = v.split_once(':').ok_or(format!("line {n}: give needs <bandage|medkit|shields|purple>:<count>"))?; let idx = ["bandage", "medkit", "shields", "purple", "ammo_ar", "ammo_shotgun", "ammo_sniper" /* AMMO HOOK: 4.. = an ammo pickup for firearm idx-4 */].iter().position(|k| *k == name).ok_or(format!("line {n}: unknown healable / ammo {name:?}"))?; Action::Give(idx, qty.parse().map_err(|_| format!("line {n}: bad count"))?) } Some(("mouse", v)) => { let (a, b) = v.split_once(':').ok_or(format!("line {n}: mouse=DX:DY"))?; Action::Mouse(a.parse().map_err(|_| format!("line {n}: mouse=DX:DY"))?, b.parse().map_err(|_| format!("line {n}: mouse=DX:DY"))?) } Some(("turn", v)) => Action::Turn(num(v, t, n)?), Some(("look", v)) => Action::Look(num(v, t, n)?),
                                Some(("die", v)) => Action::Die(fn_ui::screens::kill_feed::DeathCause::parse(v).ok_or(format!("line {n}: die=storm|fall|self|banhammer"))?),
                                Some(("tp", v)) => { let p: Vec<f32> = v.split(':').map(|x| x.parse::<f32>()).collect::<Result<_, _>>().map_err(|_| format!("line {n}: tp=X:Y[:UP] (UE cm, metres)"))?; if !(2..=3).contains(&p.len()) { return Err(format!("line {n}: tp=X:Y[:UP]")); } Action::Tp(p[0], p[1], p.get(2).copied().unwrap_or(0.)) } // ALL POIS
                                Some(("tpz", v)) => { let p: Vec<f32> = v.split(':').map(|x| x.parse::<f32>()).collect::<Result<_, _>>().map_err(|_| format!("line {n}: tpz=X:Y:Z (UE cm)"))?; if p.len() != 3 { return Err(format!("line {n}: tpz=X:Y:Z")); } Action::TpZ(p[0], p[1], p[2]) } // CONTAINERS
                                Some(("pad", v)) => { let (a, b) = v.split_once(':').ok_or(format!("line {n}: pad=RX:RY"))?; let (x, y) = (num(a, "pad", n)?, num(b, "pad", n)?); if x.abs() > 1. || y.abs() > 1. { return Err(format!("line {n}: pad values are -1..1")); } Action::Pad(x, y) } // GAMEPAD LOOK
                                Some(("key", v)) => Action::Key(build_key(v).ok_or(format!("line {n}: key={v}: expected switch|q, f1..f5, 1..6, fire|lmb, material|rmb, edit|g, confirm, reset, rotate|r, improve, use|e"))?), // SCRIPT BUILD KEYS
                                Some((k @ ("aimtile" | "aimarrow"), v)) => { let i = v.parse::<usize>().map_err(|_| format!("line {n}: {k} needs an index"))?; Action::Aim(if k == "aimtile" { fn_building::edit::EditTarget::Tile(i) } else { fn_building::edit::EditTarget::Aux(i) }) } // SCRIPT BUILD KEYS
                                _ => return Err(format!("line {n}: unknown action {t:?}")),
                            },
                        };
                        s.entries.push(Entry { from, to, action });
                        i += 1;
                    }
                }
            }
        }
        Ok(s)
    }
    pub fn end_frame(&self) -> u32 { self.end.unwrap_or_else(|| self.entries.iter().map(|e| e.to).max().unwrap_or(0).saturating_add(30)) }
    fn active(&self, frame: u32) -> impl Iterator<Item = &Action> + '_ { self.entries.iter().filter(move |e| (e.from..e.to).contains(&frame)).map(|e| &e.action) }
    /// The input of one frame (the keyboard-path fields of `FrameInput`).
    pub fn input_at(&self, frame: u32) -> FrameInput {
        let mut f = FrameInput::default();
        for a in self.active(frame) {
            match a {
                Action::Forward => f.forward += 1., Action::Back => f.forward -= 1., Action::Right => f.side += 1., Action::Left => f.side -= 1.,
                Action::Fwd(v) => f.forward += v, Action::Strafe(v) => f.side += v,
                Action::Jump => f.jump = true, Action::Crouch => f.crouch_key = true, Action::Sprint => f.sprint_key = true, Action::Attack => f.attack = true,
                Action::Equip => f.equip = true, Action::Reset => f.reset = true,
                Action::Yaw(v) => f.set_yaw = Some(*v), Action::Pitch(v) => f.set_pitch = Some(*v), Action::Turn(v) => f.yaw_delta += v, Action::Look(v) => f.pitch_delta += v,
                Action::Spawn(..) | Action::Quit | Action::Shot(_) | Action::Resize(..) | Action::Slot(_) | Action::Reload | Action::Health(_) | Action::Shield(_) | Action::Give(..) | Action::Inv(_) | Action::Mouse(..) | Action::Pad(..) => {}
                Action::Key(k) => if *k == fn_settings::A_FIRE { f.attack = true }, // SCRIPT BUILD KEYS: Fire also fills `attack` like the mouse path (build mode clears it again)
                Action::Aim(_) | Action::Tp(..) | Action::TpZ(..) | Action::Fly(_) | Action::KillFeed | Action::Die(_) => {} // ALL POIS: applied in `prepare`
            }
        }
        f.forward = f.forward.clamp(-1., 1.); f.side = f.side.clamp(-1., 1.);
        f
    }
    /// The arms-side input of one frame (slot=N selects, reload, health=N sets the player's health for the heal test); fire is `attack` / `fire`.
    pub fn arms_at(&self, frame: u32) -> crate::arms::ArmsInput {
        let mut a = crate::arms::ArmsInput::default();
        for act in self.active(frame) { match act { Action::Slot(i) => a.select = Some(*i), Action::Reload => a.reload_press = true, Action::Health(h) => a.set_health = Some(*h), Action::Shield(v) => a.set_shield = Some(*v), Action::Give(i, q) if *i >= 4 => a.give_ammo = Some((*i - 4, *q)), Action::Give(i, q) => a.give = Some((*i, *q)), _ => {} } }
        a
    }
    pub fn shots_at(&self, frame: u32) -> Vec<String> { self.active(frame).filter_map(|a| if let Action::Shot(n) = a { Some(n.clone()) } else { None }).collect() }
    pub fn resize_at(&self, frame: u32) -> Option<(u32, u32)> { self.active(frame).find_map(|a| if let Action::Resize(w, h) = a { Some((*w, *h)) } else { None }) }
    /// MOUSE LOOK: the raw mouse counts of `frame`.
    pub fn mouse_at(&self, frame: u32) -> (i32, i32) { self.active(frame).fold((0, 0), |m, a| if let Action::Mouse(x, y) = a { (m.0 + x, m.1 + y) } else { m }) }
    /// GAMEPAD LOOK: the right stick held at `frame` (the last `pad=` listed wins).
    pub fn pad_at(&self, frame: u32) -> Option<(f32, f32)> { self.active(frame).filter_map(|a| if let Action::Pad(x, y) = a { Some((*x, *y)) } else { None }).last() }
    /// SCRIPT BUILD KEYS: the key-binding rows held at `frame`.
    pub fn build_keys_at(&self, frame: u32) -> Vec<usize> { self.active(frame).filter_map(|a| if let Action::Key(k) = a { Some(*k) } else { None }).collect() }
    /// SCRIPT BUILD KEYS: the edit tile / stair arrow to aim at during `frame` (the last one listed wins).
    pub fn aim_at(&self, frame: u32) -> Option<fn_building::edit::EditTarget> { self.active(frame).filter_map(|a| if let Action::Aim(t) = a { Some(*t) } else { None }).last() }
    pub fn quit_at(&self, frame: u32) -> bool { self.active(frame).any(|a| matches!(a, Action::Quit)) }
    /// INVENTORY HOOK: the inventory steps of `frame`, in script order.
    pub fn inventory_at(&self, frame: u32) -> Vec<InvCmd> { self.active(frame).filter_map(|a| if let Action::Inv(c) = a { Some(c.clone()) } else { None }).collect() }
}

/// What a scripted run needs from a script, whether it is a text timeline or a built-in scenario with world scripting (the pickaxe series).
pub trait Driver {
    fn name(&self) -> String;
    fn fps(&self) -> f32 { 60. }
    fn size(&self) -> Option<(u32, u32)> { None }
    fn end_frame(&self) -> u32;
    /// World set-up before `frame` is stepped (props planted / cleared).
    fn prepare(&mut self, _frame: u32, _g: &mut GameSim) {}
    /// How the real 1.8 props fill the world when the run starts (`props.rs`); `Off` keeps the sandbox / the script's own props (the parity scripts).
    fn real_props(&self) -> crate::props::Mode { crate::props::Mode::Off }
    fn input(&mut self, frame: u32) -> FrameInput;
    /// MOUSE LOOK: raw mouse counts of `frame` (converted by the app with the live settings and the view FOV).
    fn mouse(&mut self, _frame: u32) -> (i32, i32) { (0, 0) }
    /// GAMEPAD LOOK: right stick (x, y in -1..1) of `frame`, None = no scripted pad.
    fn pad(&mut self, _frame: u32) -> Option<(f32, f32)> { None }
    /// Arms input of `frame` (quickbar selection, reload, health set) for scripted weapon tests.
    fn arms(&mut self, _frame: u32) -> crate::arms::ArmsInput { Default::default() }
    fn assume_unrecovered(&self) -> bool { false }
    fn standin_gun(&self) -> bool { false }
    /// Screenshot names to capture after `frame` has been stepped and rendered (`g.frames` is already incremented).
    fn shots(&mut self, frame: u32, g: &GameSim) -> Vec<String>;
    fn resize(&self, _frame: u32) -> Option<(u32, u32)> { None }
    fn quit(&self, _frame: u32) -> bool { false }
    /// INVENTORY HOOK: inventory-screen steps of `frame`.
    fn inventory(&mut self, _frame: u32) -> Vec<InvCmd> { vec![] }
    /// INVENTORY HOOK: start with the 1.8 BR spawn inventory (`loadout br`) instead of the dev loadout.
    fn br_loadout(&self) -> bool { false }
    /// SCRIPT BUILD KEYS: key-binding rows held at `frame` (they drive `BuildInput::update` through its key poll).
    fn build_keys(&mut self, _frame: u32) -> Vec<usize> { vec![] }
    /// SCRIPT BUILD KEYS: the edit tile / stair arrow the view is aimed at during `frame`.
    fn edit_aim(&mut self, _frame: u32) -> Option<fn_building::edit::EditTarget> { None }
}

/// ALL POIS: `tp=X:Y[:UP]`: the feet to UE world (X, Y) cm through the installed landscape's mapping (without one: the props' mapping about the layout
/// centre, `fn_game::spawn` for the height), `up` metres above the highest collision surface there (`GameSim::floor_below` from `SPAWN_PROBE_M` above the
/// landscape), velocity zeroed (the tick driver snaps a jump this long instead of interpolating it).
pub fn teleport_ue(g: &mut GameSim, x: f32, y: f32, up: f32) {
    g.sync_terrain();
    let (gx, gz) = match fn_terrain::world() { Some(w) => { let p = w.map.to_game(glam::Vec3::new(x, y, w.map.ref_z_cm)); (p.x, p.z) } None => (0., fn_game::spawn().2) };
    let top = g.floor_below(glam::Vec3::new(gx, g.ground_y(gx, gz) + fn_game::SPAWN_PROBE_M, gz));
    let h = top + up.max(0.);
    g.controller.position = glam::Vec2::new(gx, gz - fn_game::Z_OFFSET);
    g.controller.velocity = glam::Vec2::ZERO; g.controller.vertical_speed = 0.;
    g.controller.height = h;
    g.x = gx; g.z = gz; g.y = fn_game::EYE + h;
    eprintln!("[script] tp UE ({x:.0}, {y:.0}) cm -> game ({gx:.2}, {h:.2}, {gz:.2}) m (floor {top:.2} m + {up:.1} m; landscape {:.2} m)", g.ground_y(gx, gz));
}

/// CONTAINERS: `tpz=X:Y:Z` (see [`Action::TpZ`]).
pub fn teleport_ue_below(g: &mut GameSim, x: f32, y: f32, z_cm: f32) {
    g.sync_terrain();
    let Some(w) = fn_terrain::world() else { return teleport_ue(g, x, y, 0.) };
    let p = w.map.to_game(glam::Vec3::new(x, y, z_cm));
    let h = g.floor_below(p);
    g.controller.position = glam::Vec2::new(p.x, p.z - fn_game::Z_OFFSET);
    g.controller.velocity = glam::Vec2::ZERO; g.controller.vertical_speed = 0.;
    g.controller.height = h;
    g.x = p.x; g.z = p.z; g.y = fn_game::EYE + h;
    eprintln!("[script] tpz UE ({x:.0}, {y:.0}, {z_cm:.0}) cm -> game ({:.2}, {h:.2}, {:.2}) m", p.x, p.z);
}

pub fn apply_props(props: Props, g: &mut GameSim) {
    match props {
        Props::Sandbox | Props::Real | Props::RealTree | Props::RealRock => {} // the real ones are placed when the load finishes (`props::Library::populate`)
        Props::None => g.world.props.clear(),
        Props::Tree => { g.world.props.clear(); g.world.props.push(fn_combat::Harvestable::new(fn_combat::Kind::Tree, glam::Vec3::new(0., 0., 12. - 1.8))); }
    }
}

impl Driver for Script {
    fn name(&self) -> String { self.name.clone() }
    fn fps(&self) -> f32 { self.fps }
    fn size(&self) -> Option<(u32, u32)> { self.size }
    fn end_frame(&self) -> u32 { Script::end_frame(self) }
    fn prepare(&mut self, frame: u32, g: &mut GameSim) {
        if frame == 0 { apply_props(self.props, g); crate::poi::set_sync_build(true); /* ALL POIS: a scripted run builds every POI group as it comes into range (no pop-in in the shots) */ }
        // ALL POIS: teleports and fly modes of this frame
        let moves: Vec<Action> = self.active(frame).filter(|a| matches!(a, Action::Tp(..) | Action::TpZ(..) | Action::Fly(_))).cloned().collect();
        for a in moves { match a { Action::Tp(x, y, up) => teleport_ue(g, x, y, up), Action::TpZ(x, y, z) => teleport_ue_below(g, x, y, z), Action::Fly(m) => g.fly = m, _ => {} } }
        let reqs: Vec<fn_console::Request> = self.active(frame).filter_map(|a| if let Action::Spawn(q, c) = a { Some(fn_console::Request::SpawnPickup { query: q.clone(), count: *c }) } else { None }).collect();
        if !reqs.is_empty() { crate::pickups_hook::requests(&reqs); }
        for a in self.active(frame) { match a { Action::KillFeed => crate::kill_feed_hook::add_cheat_messages(), Action::Die(c) => { crate::kill_feed_hook::force_local_death(*c); } _ => {} } }
    }
    fn real_props(&self) -> crate::props::Mode { match self.props { Props::Real => crate::props::Mode::Layout, Props::RealTree => crate::props::Mode::One(crate::props::ONE_TREE_ID), Props::RealRock => crate::props::Mode::One(crate::props::ONE_ROCK_ID), _ => crate::props::Mode::Off } }
    fn input(&mut self, frame: u32) -> FrameInput { self.input_at(frame) }
    fn mouse(&mut self, frame: u32) -> (i32, i32) { self.mouse_at(frame) }
    fn pad(&mut self, frame: u32) -> Option<(f32, f32)> { self.pad_at(frame) }
    fn arms(&mut self, frame: u32) -> crate::arms::ArmsInput { self.arms_at(frame) }
    fn assume_unrecovered(&self) -> bool { self.assume }
    fn standin_gun(&self) -> bool { self.standin_gun }
    fn shots(&mut self, frame: u32, _g: &GameSim) -> Vec<String> {
        let shots = self.shots_at(frame);
        if cfg!(target_arch = "wasm32") && !shots.is_empty() {
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) { eprintln!("[script] shot {} at frame {frame} skipped: the browser build cannot read the GPU back synchronously on the main thread (mapAsync resolves only after the frame returns), so script shots are off there; take page screenshots from the driver instead", shots[0]); }
            return vec![];
        }
        shots
    }
    fn resize(&self, frame: u32) -> Option<(u32, u32)> { self.resize_at(frame) }
    fn quit(&self, frame: u32) -> bool { self.quit_at(frame) }
    fn inventory(&mut self, frame: u32) -> Vec<InvCmd> { self.inventory_at(frame) }
    fn br_loadout(&self) -> bool { self.br_loadout }
    fn build_keys(&mut self, frame: u32) -> Vec<usize> { self.build_keys_at(frame) }
    fn edit_aim(&mut self, frame: u32) -> Option<fn_building::edit::EditTarget> { self.aim_at(frame) }
}

/// The fn-sim `--pickaxe-test` series (idle, jog, sprint, crouch, jump, swing / hit / bounce / harvest, equip, ... then a resize sweep) driven
/// through the app: the inputs are `pick_script::step`, the world scripting is `pick_run`'s, so its frame log equals fn-sim's line for line.
#[cfg(feature = "sim-compare")]
pub struct PickDriver;
#[cfg(feature = "sim-compare")]
impl PickDriver {
    pub fn new() -> PickDriver { use fn_sim::pick_script as p; p::set_active(true); p::set_anim_series(false); p::set_dir_series(false); PickDriver }
}
#[cfg(feature = "sim-compare")]
impl Driver for PickDriver {
    fn name(&self) -> String { "pickaxe".into() }
    fn size(&self) -> Option<(u32, u32)> { Some((1280, 720)) }
    fn end_frame(&self) -> u32 { fn_sim::pick_script::end_frame() }
    fn prepare(&mut self, f: u32, g: &mut GameSim) {
        use fn_sim::pick_script as pick;
        if f == pick::clear_frame() { g.world.props.clear(); }
        if f == pick::target_frame() {
            g.controller = fn_game::movement::Controller::default(); g.to_spawn();
            g.world.props.push(fn_combat::Harvestable::new(fn_combat::Kind::Tree, glam::Vec3::new(0., 0., 12. - 1.8)));
        }
    }
    fn input(&mut self, f: u32) -> FrameInput {
        use fn_sim::pick_script as pick;
        let ps = pick::step(f);
        FrameInput { forward: ps.forward, side: ps.side, script_jump: ps.jump, force_crouch: ps.crouch, force_run: ps.run, attack: ps.attack,
            set_yaw: ps.yaw.map(|y| (fn_game::spawn().3 + y).rem_euclid(360.)), set_pitch: ps.pitch, equip: pick::equip(f), ..Default::default() }
    }
    fn shots(&mut self, _f: u32, g: &GameSim) -> Vec<String> {
        use fn_sim::pick_script as pick;
        if let Some(n) = pick::capture(g.frames) { return vec![n.to_owned()]; }
        if pick::size_capture(g.frames) { return vec!["pick-size".to_owned()]; } // the resolution sweep: the name gets the size appended
        vec![]
    }
    fn resize(&self, f: u32) -> Option<(u32, u32)> { fn_sim::pick_script::resize(f).map(|(w, h)| (w as u32, h as u32)) }
}

pub const SMOKE: &str = "# the old --smoke-test timeline at fixed 60 Hz: idle, jog, jump, landing, crouch, sprint, direction blend, swings, moving swing, crouched swing
size 1280x720
fps 60
props sandbox
0 equip
20-100 forward
65 jump
195-280 crouch
225-270 forward
300-375 forward sprint
410-480 forward strafe=0.41421356
490-545 attack
610-715 forward
635-690 attack
750-850 crouch
770-805 attack
10 shot idle
60 shot jog
85 shot jump
145 shot landing
210 shot crouch-idle
250 shot crouch-walk
350 shot sprint
465 shot direction-blend
507 shot swing
540 shot swing-two
570 shot swing-recovery
652 shot moving-swing
787 shot crouched-swing
870 shot scene
end 880
";
pub const WALK: &str = "# walk, turn, jump, crouch, sprint, swing next to a tree (short)
size 1280x720
fps 60
props tree
0 equip
10 shot start
30-150 forward
60 jump
90-150 sprint
150 shot walk-end
160-220 crouch
200 shot crouch
230-250 attack
250 shot swing
260-320 turn=1.5
320 shot turned
end 330
";

pub const WEAPONS_BODY: &str = "size 1280x720\nfps 60\nprops none\n5 slot=2\n40-100 fire\n120 reload\n280 slot=3\n320 fire\n420 fire\n470 reload\n660 slot=4\n700 fire\n760 reload\n960 slot=5\n965 health=40\n970 fire\n1300 slot=2\n";
/// Default policy: every behaviour the 1.8 data does not decide stays missing and is reported (`[arms] ... MISSING`). Ends with one capture of the held state.
pub const HEALING: &str = "# every healing consumable of the data through the real ability logic (use ring, montage, trigger, cost, effect, cooldown)
size 1280x720
fps 60
props none
5 slot=5
10 health=30 shield=20
15 fire
150 shot ring
400 give=medkit:1 health=30
410 fire
1100 fire
1300 give=shields:2 shield=10
1310 fire
1700 fire
1900 give=purple:1 health=10 shield=0
1910 fire
2500 fire
end 3000
";
pub fn weapons_text(assumed: bool) -> String { let head = if assumed { "assume\nstandin-gun\n" } else { "" }; format!("{head}{WEAPONS_BODY}{}end 1420\n", if assumed { "1400 shot held\n" } else { "" }) }

pub fn builtin(name: &str) -> Option<Result<Box<dyn Driver>, String>> {
    match name {
        "smoke" => Some(Script::parse("smoke", SMOKE).map(|s| Box::new(s) as Box<dyn Driver>)),
        "walk" => Some(Script::parse("walk", WALK).map(|s| Box::new(s) as Box<dyn Driver>)),
        "healing" => Some(Script::parse("healing", HEALING).map(|s| Box::new(s) as Box<dyn Driver>)),
        "weapons" => Some(Script::parse("weapons", &weapons_text(false)).map(|s| Box::new(s) as Box<dyn Driver>)),
        "weapons-assumed" => Some(Script::parse("weapons-assumed", &weapons_text(true)).map(|s| Box::new(s) as Box<dyn Driver>)),
        #[cfg(feature = "sim-compare")]
        "pickaxe" => Some(Ok(Box::new(PickDriver::new()))),
        _ => None,
    }
}

/// `file` or `builtin:NAME`.
pub fn load(spec: &str) -> Result<Box<dyn Driver>, String> {
    if let Some(name) = spec.strip_prefix("builtin:") { return builtin(name).unwrap_or_else(|| Err(format!("unknown built-in script {name:?} (smoke, walk, pickaxe, weapons, weapons-assumed, healing)"))); }
    let text = std::fs::read_to_string(spec).map_err(|e| format!("cannot read script {spec}: {e}"))?;
    Ok(Box::new(Script::parse(spec, &text)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_single_frame_at_u32_max_is_an_error_not_a_wrapping_range() {
        let e = Script::parse("t", "4294967295 jump\n").unwrap_err(); assert!(e.contains("too large") && e.contains("line 1"), "{e}");
        let s = Script::parse("t", "4294967294 jump\n").unwrap(); assert_eq!(s.end_frame(), u32::MAX, "end_frame saturates instead of wrapping");
        assert!(Script::parse("t", "5-4294967295 forward\n").is_ok());
    }
    #[test] fn oversized_or_tiny_script_windows_are_rejected_with_the_limit_in_the_message() {
        for text in ["10 resize 9000x500", "10 resize 1280x9000", "10 resize 99999x1", "size 9000x720", "size 100x100", "5 resize 639x360"] {
            let e = Script::parse("t", text).unwrap_err(); assert!(e.contains("line 1") && (e.contains("8192") || e.contains("minimum")), "{text}: {e}");
        }
        assert!(Script::parse("t", "10 resize 8192x8192\nsize 640x360").is_ok());
    }
    #[test] fn parses_ranges_axes_and_options() {
        let s = Script::parse("t", "size 800x600\nfps 30\nprops none\n# c\n10-20 forward sprint ; trailing\n15 jump shot a\n5-8 strafe=0.5 yaw=90\n40 resize 640x360\nend 100").unwrap();
        assert_eq!((s.size, s.fps, s.props, s.end_frame()), (Some((800, 600)), 30., Props::None, 100));
        let f = s.input_at(15); assert_eq!(f.forward, 1.); assert!(f.sprint_key && f.jump && !f.attack); assert_eq!(s.shots_at(15), vec!["a".to_owned()]);
        assert!(s.input_at(9).forward == 0. && s.input_at(20).forward == 0., "20 is exclusive");
        let g = s.input_at(6); assert_eq!((g.side, g.set_yaw), (0.5, Some(90.))); assert_eq!(s.resize_at(40), Some((640, 360))); assert_eq!(s.resize_at(41), None);
    }
    #[test] fn forward_back_cancel_and_axes_clamp() { let s = Script::parse("t", "0-5 forward back\n0-5 fwd=1 forward").unwrap(); assert_eq!(s.input_at(1).forward, 1.); }
    #[test] fn end_defaults_to_the_last_entry_plus_30() { assert_eq!(Script::parse("t", "5-10 forward").unwrap().end_frame(), 40); assert_eq!(Script::parse("t", "").unwrap().end_frame(), 30); }
    #[test] fn bad_lines_are_reported_with_their_number() {
        for (text, needle) in [("foo bar", "line 1"), ("10-5 jump", "empty range"), ("5", "no action"), ("0 sprintt", "unknown action"), ("0 yaw=x", "bad number"), ("size 5", "expected WxH"), ("fps 0", "out of range"), ("props lots", "unknown props"), ("0 shot", "needs a name"), ("0 resize", "needs WxH")] {
            let e = Script::parse("t", text).unwrap_err(); assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }
    #[test] fn builtin_scripts_parse_and_have_captures() {
        for n in ["smoke", "walk"] { let mut d = builtin(n).unwrap().unwrap(); let g = GameSim::new(); let shots: usize = (0..d.end_frame()).map(|f| d.shots(f, &g).len()).sum(); assert!(shots >= 5, "{n}"); assert!(d.end_frame() > 100); }
        assert!(builtin("nope").is_none());
    }
    #[test] fn smoke_script_walks_jumps_crouches_sprints_and_swings_in_the_simulation() {
        let mut d = builtin("smoke").unwrap().unwrap(); let mut g = GameSim::new();
        let (mut max_h, mut crouched, mut ran, mut swung, mut min_z) = (0f32, false, false, false, g.z);
        for f in 0..d.end_frame() { d.prepare(f, &mut g); g.step(&d.input(f), 1. / 60.); max_h = max_h.max(g.controller.air_height()); crouched |= g.controller.crouched; ran |= g.controller.running; swung |= g.swing.stage > 0; min_z = min_z.min(g.z); }
        assert!(max_h > 0.5 && crouched && ran && swung && min_z < g.feet().z + 1., "jump {max_h} crouch {crouched} run {ran} swing {swung}");
        assert!(g.world.hits + g.world.misses > 0, "swings landed somewhere");
    }
    #[test] fn weapon_script_actions_map_to_arms_input() {
        let s = Script::parse("t", "assume\nstandin-gun\n5 slot=2\n7 reload health=40\n9 fire\n").unwrap();
        assert!(s.assume && s.standin_gun);
        assert_eq!(s.arms_at(5).select, Some(1)); let a = s.arms_at(7); assert!(a.reload_press && a.set_health == Some(40.)); assert!(s.input_at(9).attack);
        assert!(Script::parse("t", "5 slot=7").is_err());
        for assumed in [false, true] { let t = weapons_text(assumed); let s = Script::parse("w", &t).unwrap(); assert_eq!(s.assume, assumed); assert_eq!(s.end_frame(), 1420); }
    }    #[test] fn tree_props_plant_one_tree_ahead() { let mut g = GameSim::new(); apply_props(Props::Tree, &mut g); assert_eq!(g.world.props.len(), 1); apply_props(Props::None, &mut g); assert!(g.world.props.is_empty()); }
    /// SCRIPT BUILD KEYS: `key=` rows go through the real `BuildInput::update` (bound keys polled), with the same edges as the keyboard tests in build_input.rs.
    #[test] fn build_keys_drive_the_real_build_input_layer() {
        use crate::build_input::{BuildInput, BuildIntent as I};
        use fn_building::pieces::{Material, PieceKind};
        let s = Script::parse("t", "1 key=q\n3 key=f3\n5 key=rmb\n7-60 key=fire\n61 key=g\n63-70 key=lmb aimtile=4\n64-70 aimarrow=1\n71 key=reset\n73 key=confirm\n").unwrap();
        assert!(s.input_at(10).attack, "Fire fills attack like the mouse path"); assert_eq!(s.aim_at(65), Some(fn_building::edit::EditTarget::Aux(1))); assert_eq!(s.aim_at(63), Some(fn_building::edit::EditTarget::Tile(4)));
        let b = fn_settings::Settings::default().input.bindings.clone();
        let mut bi = BuildInput::new(); let mut log = vec![];
        for f in 0..80 {
            if f == 62 { bi.set_editing(true); } // the edit hook's answer to EditBegin
            let codes: Vec<u16> = s.build_keys_at(f).iter().flat_map(|&a| b.get(a)).filter_map(|k| k.poll_code()).collect();
            bi.update(&b, |c| codes.contains(&c), false, 1. / 60.); log.extend(bi.drain_intents());
        }
        assert_eq!(log, vec![I::EnterBuildMode(PieceKind::Wall), I::SelectPiece(PieceKind::Stairs), I::MaterialChanged(Material::Stone), I::Place, I::EditBegin,
            I::EditSelectPressed, I::EditSelectReleased, I::EditReset, I::EditConfirm], "held fire places once (no turbo build)");
        for bad in ["0 key=f6", "0 key=jump", "0 aimtile=x"] { assert!(Script::parse("t", bad).is_err(), "{bad}"); }
    }
}
