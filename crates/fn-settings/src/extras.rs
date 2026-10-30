use crate::Settings;

pub type ExtraKey = (&'static str, &'static str);

pub const CONTROLLER_CONFIGURATION: ExtraKey = ("Input", "ControllerConfiguration");
pub const CONTROLLER_LEFT_DEADZONE: ExtraKey = ("Input", "ControllerLeftStickDeadzone");
pub const CONTROLLER_RIGHT_DEADZONE: ExtraKey = ("Input", "ControllerRightStickDeadzone");

pub const OFF_ON: &[&str] = &["Off", "On"];
pub const CONFIGURATIONS: &[&str] = &["Standard", "Quick Builder", "Combat Pro"];
pub const DEFAULT_LEFT_DEADZONE: f32 = 0.24;
pub const DEFAULT_RIGHT_DEADZONE: f32 = 0.27;
pub const DEADZONE_MIN: f32 = 0.0;
pub const DEADZONE_MAX: f32 = 0.9;

pub const CONTROLLER_KEYS: &[ExtraKey] = &[CONTROLLER_CONFIGURATION, CONTROLLER_LEFT_DEADZONE, CONTROLLER_RIGHT_DEADZONE];

pub fn flag(s: &Settings, k: ExtraKey) -> bool {
    s.extra(k.0, k.1).map_or(false, |v| { let v = v.trim(); v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true") || v == "1" })
}
pub fn set_flag(s: &mut Settings, k: ExtraKey, on: bool) { s.set_extra(k.0, k.1, OFF_ON[on as usize]); }

pub fn configuration(s: &Settings) -> usize {
    s.extra(CONTROLLER_CONFIGURATION.0, CONTROLLER_CONFIGURATION.1).and_then(|v| CONFIGURATIONS.iter().position(|o| o.eq_ignore_ascii_case(v.trim()))).unwrap_or(0)
}
pub fn set_configuration(s: &mut Settings, i: usize) { s.set_extra(CONTROLLER_CONFIGURATION.0, CONTROLLER_CONFIGURATION.1, CONFIGURATIONS[i.min(CONFIGURATIONS.len() - 1)]); }

fn default_deadzone(k: ExtraKey) -> f32 { if k == CONTROLLER_LEFT_DEADZONE { DEFAULT_LEFT_DEADZONE } else { DEFAULT_RIGHT_DEADZONE } }
pub fn deadzone(s: &Settings, k: ExtraKey) -> f32 {
    s.extra(k.0, k.1).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite()).map_or(default_deadzone(k), |v| v.clamp(DEADZONE_MIN, DEADZONE_MAX))
}
pub fn set_deadzone(s: &mut Settings, k: ExtraKey, v: f32) { s.set_extra(k.0, k.1, &format!("{:.2}", v.clamp(DEADZONE_MIN, DEADZONE_MAX))); }
pub fn deadzones(s: &Settings) -> (f32, f32) { (deadzone(s, CONTROLLER_LEFT_DEADZONE), deadzone(s, CONTROLLER_RIGHT_DEADZONE)) }

pub fn reset_controller(s: &mut Settings) { for k in CONTROLLER_KEYS { s.clear_extra(k.0, k.1); } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn defaults_are_off_standard_and_the_1_8_cvars() {
        let s = Settings::default();
        assert_eq!(configuration(&s), 0);
        assert_eq!(deadzones(&s), (0.24, 0.27));
    }
    #[test] fn values_round_trip_through_the_settings_file() {
        let mut s = Settings::default();
        s.set_extra("Game", "TurboBuild", "On"); s.set_extra("Game", "ConfirmEditOnRelease", "On"); set_configuration(&mut s, 2); set_deadzone(&mut s, CONTROLLER_LEFT_DEADZONE, 0.1); set_deadzone(&mut s, CONTROLLER_RIGHT_DEADZONE, 5.);
        let dir = std::env::temp_dir().join(format!("fn-settings-extras-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("GameUserSettings.ini");
        s.save(&p).unwrap();
        let (t, _) = Settings::load(&p);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(configuration(&t), 2);
        assert_eq!(deadzones(&t), (0.1, DEADZONE_MAX));
        let mut t = t; reset_controller(&mut t); assert_eq!((configuration(&t), deadzones(&t)), (0, (0.24, 0.27)));
    }
}
