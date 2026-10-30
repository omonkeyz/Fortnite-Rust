use fn_ui::menu::{self, pages, pause::SettingsScreen, Event, MenuSystem, NullBackend};
use fn_ui::settings::{extras, NullPlatform, Settings};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(args.first().cloned().unwrap_or_else(|| "ctrl_capture".into()));
    std::fs::create_dir_all(&out).unwrap();
    let (w, h) = (1920u32, 1080u32);
    let install = fn_ui::pak_assets::default_install();
    let mut ctx = fn_ui::scene::UiContext::from_install(&install, Some(&fn_ui::assets::DirAssets::default_root())).expect("ui context");
    let mut s = Settings::default();
    s.drain_changes();
    let mut platform = NullPlatform::static_16_9();
    let mut be = NullBackend::default();
    let mut m = MenuSystem::new(ctx.fonts.clone());
    pages::register_all_into(&mut m);
    m.fade_time = 0.;
    m.open();
    m.push(Box::new(SettingsScreen::new()));
    let ids: Vec<String> = m.pages().iter().map(|p| p.id().to_owned()).collect();
    for (name, scroll) in [("game", 0), ("game", 6), ("controller", 0)] {
        let i = ids.iter().position(|x| x == name).unwrap();
        m.set_settings_tab(i);
        for _ in 0..4 { let mut cx = menu::UiContext::new(&mut s, &mut platform, &mut be); m.frame_with(&mut cx, w as f32, h as f32, 1. / 60.); }
        for _ in 0..scroll { m.event(Event::MouseMove(960., 500.)); m.event(Event::Wheel(-3.)); let mut cx = menu::UiContext::new(&mut s, &mut platform, &mut be); m.frame_with(&mut cx, w as f32, h as f32, 1. / 60.); }
        let mut cx = menu::UiContext::new(&mut s, &mut platform, &mut be);
        let draw = m.frame_with(&mut cx, w as f32, h as f32, 1. / 60.);
        let mut list = fn_ui::draw::RenderList::new();
        menu::lower::lower(&draw, &mut ctx, w as f32, h as f32, &mut list);
        let px = fn_ui::raster::render(&list, &mut ctx.assets, w, h, Some([0.05, 0.08, 0.2, 1.]));
        let p = out.join(format!("{name}-scroll{scroll}.png"));
        image::save_buffer(&p, &px, w, h, image::ColorType::Rgba8).unwrap();
        println!("{}", p.display());
    }
}
