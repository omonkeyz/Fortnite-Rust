//! Cascade distributions (`FRawDistributionFloat` / `FRawDistributionVector`) as cooked into the 1.8 packages.
//!
//! A cooked raw distribution is either
//!  * a baked lookup table (`FDistributionLookupTable`: `Op`, `EntryCount`, `EntryStride`, `SubEntryStride`, `TimeScale`, `TimeBias`, `LockFlag`, `Values`), used when the
//!    `Distribution` object reference is null, or
//!  * a reference to a distribution object (`DistributionFloatConstant`, `DistributionFloatConstantCurve`, `DistributionVectorConstant`, `DistributionVectorConstantCurve`).
//!
//! Table lookup (`FRawDistribution::GetEntry`): `x = (time + TimeBias) * TimeScale`, entries `floor(x)` / `floor(x) + 1` (clamped) are interpolated linearly; a table with one
//! entry is a constant. `Op` (what the data shows: 1 on every constant / curve table, 2 on every min/max table, 3 on min/max tables of modules that pick extremes):
//!  * 1: the interpolated entry is the value;
//!  * 2 (`RDO_Random`): the entry holds `min` then `max` (`SubEntryStride` floats each), the value is `lerp(min, max, random)` per component;
//!  * 3 (`RDO_Extreme`): the value is `min` or `max` per component, chosen by the random number (>= 0.5 -> max).
//! `LockFlag` (`EDistributionVectorLockFlags`: 1 XY, 2 XZ, 3 YZ, 4 XYZ) makes the locked components share one random number.
//! Curve objects use the `FInterpCurve` evaluation: Constant / Linear / cubic Hermite with the stored tangents scaled by the interval (the `CIM_Curve*` modes store their
//! computed tangents in the points).
use crate::rng::Rng;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub op: u8,
    pub lock: u8,
    pub entry_count: u32,
    pub entry_stride: u32,
    pub sub_entry_stride: u32,
    pub time_scale: f32,
    pub time_bias: f32,
    pub values: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interp { Linear, Constant, Curve }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint { pub t: f32, pub v: [f32; 3], pub arrive: [f32; 3], pub leave: [f32; 3], pub mode: Interp }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Dist {
    Const([f32; 3]),
    Table(Table),
    Curve(Vec<CurvePoint>),
    /// `DistributionFloatParticleParameter` / `DistributionVectorParticleParameter`: the value of an instance parameter of the component (see [`crate::Params`]).
    Param(ParamDist),
}

/// `DistributionParamMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamMode { Normal, Abs, Direct }

/// A `UDistribution{Float,Vector}ParameterBase` (UE4.16 Distributions.cpp:4244 / :4279). `GetValue`: read the parameter (`GetFloatParameter` for the float class, `GetAnyVectorParameter`
/// for the vector class); when the component has none, the parameter is `Constant`; then per component `Abs` takes `|x|` (`DPM_Abs`), the value is clamped to `Min/MaxInput` and mapped
/// linearly to `Min/MaxOutput` (gradient 0 when `MaxInput <= MinInput`), `DPM_Direct` returns the raw (unclamped) parameter instead. The float class has one `ParamMode`, the vector class one per component.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamDist {
    pub name: String,
    /// Vector class (`DistributionVectorParticleParameter`): `GetAnyVectorParameter` lookup, three components. Float class: `GetFloatParameter` lookup, component 0 replicated.
    pub vector: bool,
    pub constant: [f32; 3],
    pub min_input: [f32; 3],
    pub max_input: [f32; 3],
    pub min_output: [f32; 3],
    pub max_output: [f32; 3],
    pub modes: [ParamMode; 3],
    /// The parameter value currently set on the instance (written by [`crate::ParticleSystem::apply_params`]); `None` = the component has no such parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<[f32; 3]>,
}

impl ParamDist {
    /// `GetValue` after the parameter lookup: `param` is the raw parameter (or `constant` when it was not found).
    pub fn map(&self, param: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0; 3];
        for c in 0..3 {
            let mut x = param[c];
            if self.modes[c] == ParamMode::Abs { x = x.abs(); }
            let gradient = if self.max_input[c] <= self.min_input[c] { 0.0 } else { (self.max_output[c] - self.min_output[c]) / (self.max_input[c] - self.min_input[c]) };
            let clamped = if x < self.min_input[c] { self.min_input[c] } else if x < self.max_input[c] { x } else { self.max_input[c] };   // FMath::Clamp
            out[c] = if self.modes[c] == ParamMode::Direct { x } else { self.min_output[c] + (clamped - self.min_input[c]) * gradient };
        }
        if !self.vector { out = [out[0]; 3]; }
        out
    }
    /// Value when the instance has no such parameter (`Constant` through the map).
    pub fn unset_value(&self) -> [f32; 3] { self.map(self.constant) }
    /// Current value: the set parameter through the map, else [`unset_value`](Self::unset_value).
    pub fn value(&self) -> [f32; 3] { self.map(self.set.unwrap_or(self.constant)) }
    /// Looks the parameter up like the component does (float class: scalars only; vector class: vectors, colours, then scalars).
    pub fn lookup(&self, p: &crate::Params) -> Option<[f32; 3]> {
        if self.vector { p.get_any_vector(&self.name) } else { p.get_float(&self.name).map(|v| [v; 3]) }
    }
}

impl Default for Dist { fn default() -> Dist { Dist::Const([0.0; 3]) } }

fn hermite(p0: f32, t0: f32, p1: f32, t1: f32, a: f32) -> f32 {
    let (a2, a3) = (a * a, a * a * a);
    (2.0 * a3 - 3.0 * a2 + 1.0) * p0 + (a3 - 2.0 * a2 + a) * t0 + (a3 - a2) * t1 + (-2.0 * a3 + 3.0 * a2) * p1
}

fn eval_curve(points: &[CurvePoint], t: f32) -> [f32; 3] {
    let Some(first) = points.first() else { return [0.0; 3] };
    if points.len() == 1 || t <= first.t { return first.v; }
    let last = &points[points.len() - 1];
    if t >= last.t { return last.v; }
    let i = points.iter().rposition(|p| p.t <= t).unwrap_or(0).min(points.len() - 2);
    let (p0, p1) = (&points[i], &points[i + 1]);
    let diff = (p1.t - p0.t).max(1e-8);
    let a = (t - p0.t) / diff;
    let mut out = [0.0; 3];
    for c in 0..3 {
        out[c] = match p0.mode {
            Interp::Constant => p0.v[c],
            Interp::Linear => p0.v[c] + (p1.v[c] - p0.v[c]) * a,
            Interp::Curve => hermite(p0.v[c], p0.leave[c] * diff, p1.v[c], p1.arrive[c] * diff, a),
        };
    }
    out
}

impl Dist {
    pub fn constant(v: f32) -> Dist { Dist::Const([v; 3]) }
    pub fn constant3(v: [f32; 3]) -> Dist { Dist::Const(v) }
    /// True when the value never changes with time or randomness.
    pub fn is_constant(&self) -> bool {
        match self {
            Dist::Const(_) => true,
            Dist::Table(t) => t.entry_count <= 1 && t.op != 2 && t.op != 3,
            Dist::Curve(p) => p.len() <= 1,
            Dist::Param(_) => true,
        }
    }

    /// Value at `t` for `dim` components (1 or 3), drawing from `rng` for the random tables.
    pub fn eval(&self, t: f32, dim: usize, rng: &mut Rng) -> [f32; 3] {
        match self {
            Dist::Const(v) => *v,
            Dist::Curve(p) => eval_curve(p, t),
            Dist::Param(p) => p.value(),
            Dist::Table(tb) => eval_table(tb, t, dim, rng),
        }
    }
    pub fn eval1(&self, t: f32, rng: &mut Rng) -> f32 { self.eval(t, 1, rng)[0] }
    pub fn eval3(&self, t: f32, rng: &mut Rng) -> [f32; 3] { self.eval(t, 3, rng) }
    /// Value without randomness (random tables: the mean of min and max).
    pub fn mean(&self, t: f32, dim: usize) -> [f32; 3] {
        match self {
            Dist::Table(tb) if tb.op == 2 || tb.op == 3 => {
                let mut lo = eval_table_raw(tb, t, dim, 0); let hi = eval_table_raw(tb, t, dim, tb.sub_entry_stride as usize);
                for c in 0..3 { lo[c] = 0.5 * (lo[c] + hi[c]); } lo
            }
            _ => self.eval(t, dim, &mut Rng::new(1)),
        }
    }
    /// (min, max) the value can take at `t` (random tables), equal for the others.
    pub fn range_at(&self, t: f32, dim: usize) -> ([f32; 3], [f32; 3]) {
        match self {
            Dist::Table(tb) if tb.op == 2 || tb.op == 3 => (eval_table_raw(tb, t, dim, 0), eval_table_raw(tb, t, dim, tb.sub_entry_stride as usize)),
            _ => { let v = self.eval(t, dim, &mut Rng::new(1)); (v, v) }
        }
    }
}

fn entry_pair(tb: &Table, t: f32) -> (usize, usize, f32) {
    if tb.entry_count <= 1 { return (0, 0, 0.0); }
    let x = (t - tb.time_bias) * tb.time_scale;
    let last = (tb.entry_count - 1) as f32;
    let x = if x.is_finite() { x.clamp(0.0, last) } else { 0.0 };
    let i0 = (x.floor() as usize).min(tb.entry_count as usize - 1);
    let i1 = (i0 + 1).min(tb.entry_count as usize - 1);
    (i0, i1, x - i0 as f32)
}

/// Interpolated components starting `offset` floats into each entry.
fn eval_table_raw(tb: &Table, t: f32, dim: usize, offset: usize) -> [f32; 3] {
    let (i0, i1, a) = entry_pair(tb, t);
    let stride = tb.entry_stride.max(1) as usize;
    let mut out = [0.0; 3];
    for c in 0..dim.min(3) {
        let at = |i: usize| tb.values.get(i * stride + offset + c).copied().or_else(|| tb.values.get(i * stride + offset).copied()).unwrap_or(0.0);
        let (v0, v1) = (at(i0), at(i1));
        out[c] = v0 + (v1 - v0) * a;
    }
    // a one-component table asked for three components (a float distribution used as a vector): replicate
    if dim == 3 && (tb.entry_stride == 1 || tb.sub_entry_stride == 1) { out[1] = out[0]; out[2] = out[0]; }
    out
}

fn eval_table(tb: &Table, t: f32, dim: usize, rng: &mut Rng) -> [f32; 3] {
    match tb.op {
        2 | 3 => {
            let sub = tb.sub_entry_stride.max(1) as usize;
            let lo = eval_table_raw(tb, t, dim, 0); let hi = eval_table_raw(tb, t, dim, sub);
            let mut r = [rng.frac(), 0.0, 0.0];
            if dim > 1 { r[1] = rng.frac(); r[2] = rng.frac(); }
            match tb.lock { 1 => r[1] = r[0], 2 => r[2] = r[0], 3 => r[2] = r[1], 4 => { r[1] = r[0]; r[2] = r[0]; } _ => {} }
            let mut out = [0.0; 3];
            for c in 0..dim.min(3) { out[c] = if tb.op == 2 { lo[c] + (hi[c] - lo[c]) * r[c] } else if r[c] >= 0.5 { hi[c] } else { lo[c] }; }
            if dim == 3 && sub == 1 { out[1] = out[0]; out[2] = out[0]; }
            out
        }
        _ => eval_table_raw(tb, t, dim, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table(op: u8, count: u32, stride: u32, sub: u32, ts: f32, tb: f32, v: &[f32]) -> Dist { Dist::Table(Table { op, lock: 0, entry_count: count, entry_stride: stride, sub_entry_stride: sub, time_scale: ts, time_bias: tb, values: v.to_vec() }) }
    #[test] fn constant_table_and_two_entry_ramp() {
        let mut r = Rng::new(1);
        assert_eq!(table(1, 1, 1, 0, 0.0, 0.0, &[0.15]).eval1(5.0, &mut r), 0.15);
        let ramp = table(1, 2, 1, 0, 1.0, 0.0, &[1.0, 0.0]); // AlphaOverLife
        assert_eq!(ramp.eval1(0.0, &mut r), 1.0); assert!((ramp.eval1(0.25, &mut r) - 0.75).abs() < 1e-6); assert_eq!(ramp.eval1(2.0, &mut r), 0.0);
    }
    #[test] fn time_bias_matches_the_pine_erode_table() {
        // Erode of P_Pine_Tree_Med_Destruction smoke: bias 0.1859, scale 8.5985, 8 entries
        let v = [1.0189, 0.9071, 0.6186, 0.2243, -0.2054, -0.5997, -0.8881, -1.0];
        let d = table(1, 8, 1, 0, 8.598_514, 0.185_905_84, &v); let mut r = Rng::new(1);
        assert_eq!(d.eval1(0.0, &mut r), 1.0189);
        let e = d.eval1(0.5, &mut r); let x: f32 = (0.5 - 0.185_905_84) * 8.598_514; let i = x.floor() as usize; let w = v[i] + (v[i + 1] - v[i]) * (x - i as f32); assert!((e - w).abs() < 1e-4, "{e} {w}");
        assert_eq!(d.eval1(1.0, &mut r), -1.0);
    }
    #[test] fn random_table_stays_between_min_and_max_and_is_reproducible() {
        let d = table(2, 1, 6, 3, 0.0, 0.0, &[-50.0, -50.0, 150.0, 50.0, 50.0, 400.0]);
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..200 { let (x, y) = (d.eval3(0.0, &mut a), d.eval3(0.0, &mut b)); assert_eq!(x, y); assert!(x[0] >= -50.0 && x[0] <= 50.0 && x[2] >= 150.0 && x[2] <= 400.0, "{x:?}"); }
    }
    #[test] fn locked_xyz_shares_one_random_number() {
        let d = Dist::Table(Table { op: 2, lock: 4, entry_count: 1, entry_stride: 6, sub_entry_stride: 3, time_scale: 0.0, time_bias: 0.0, values: vec![4.0, 4.0, 4.0, 8.0, 8.0, 8.0] });
        let mut r = Rng::new(3); for _ in 0..50 { let v = d.eval3(0.0, &mut r); assert!(v[0] == v[1] && v[1] == v[2] && v[0] >= 4.0 && v[0] <= 8.0); }
    }
    #[test] fn extreme_picks_min_or_max() {
        let d = table(3, 1, 6, 3, 0.0, 0.0, &[1.0, 1.0, 1.0, 5.0, 5.0, 5.0]); let mut r = Rng::new(9);
        let (mut lo, mut hi) = (0, 0); for _ in 0..200 { let v = d.eval3(0.0, &mut r); if v[0] == 1.0 { lo += 1 } else if v[0] == 5.0 { hi += 1 } else { panic!("{v:?}") } }
        assert!(lo > 40 && hi > 40);
    }
    #[test] fn curve_linear_and_hermite() {
        let p = |t: f32, v: f32, mode| CurvePoint { t, v: [v; 3], arrive: [0.0; 3], leave: [0.0; 3], mode };
        let c = Dist::Curve(vec![p(0.0, 0.0, Interp::Linear), p(0.2, 64.0, Interp::Linear)]); let mut r = Rng::new(1);
        assert!((c.eval1(0.1, &mut r) - 32.0).abs() < 1e-4); assert_eq!(c.eval1(1.0, &mut r), 64.0);
        let h = Dist::Curve(vec![p(0.0, 0.0, Interp::Curve), p(1.0, 1.0, Interp::Curve)]);
        assert!((h.eval1(0.5, &mut r) - 0.5).abs() < 1e-5, "zero tangents give smoothstep through 0.5");
        assert!(h.eval1(0.25, &mut r) < 0.25);
    }
}

