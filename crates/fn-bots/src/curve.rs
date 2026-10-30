use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Interp { Linear, Constant, Cubic }

#[derive(Clone, Debug)]
pub struct Curve { pub interp: Interp, pub keys: Vec<(f32, f32)>, pub cycle: bool }

impl Curve {
    pub fn from_json(v: &Value) -> Curve {
        let interp = match v["m"].as_str() { Some("constant") => Interp::Constant, Some("cubic") => Interp::Cubic, _ => Interp::Linear };
        let keys = v["k"].as_array().map(|a| a.iter().map(|k| (k[0].as_f64().unwrap_or(0.) as f32, k[1].as_f64().unwrap_or(0.) as f32)).collect()).unwrap_or_default();
        let cycle = v["post"].as_str().map_or(false, |p| p.contains("Cycle"));
        Curve { interp, keys, cycle }
    }
    pub fn constant(v: f32) -> Curve { Curve { interp: Interp::Constant, keys: vec![(0., v)], cycle: false } }
    pub fn eval(&self, t: f32) -> f32 {
        let k = &self.keys;
        if k.is_empty() { return 0.; }
        let mut t = t;
        if self.cycle && k.len() > 1 {
            let (a, b) = (k[0].0, k[k.len() - 1].0);
            if b > a { t = a + (t - a).rem_euclid(b - a); }
        }
        if t <= k[0].0 { return k[0].1; }
        if t >= k[k.len() - 1].0 { return k[k.len() - 1].1; }
        let i = k.partition_point(|p| p.0 <= t);
        let (a, b) = (k[i - 1], k[i]);
        match self.interp {
            Interp::Constant => a.1,
            _ => { let f = (t - a.0) / (b.0 - a.0).max(1e-9); a.1 + (b.1 - a.1) * f }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linear_and_clamp() {
        let c = Curve { interp: Interp::Linear, keys: vec![(0., 0.65), (800., 0.55), (1000., 0.45)], cycle: false };
        assert!((c.eval(400.) - 0.6).abs() < 1e-5);
        assert_eq!(c.eval(-5.), 0.65);
        assert_eq!(c.eval(5000.), 0.45);
        let k = Curve { interp: Interp::Constant, keys: vec![(0., 1.), (10., 2.)], cycle: false };
        assert_eq!(k.eval(9.), 1.);
    }
}
