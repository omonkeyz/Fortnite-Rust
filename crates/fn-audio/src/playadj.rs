//! AMBIENCE HOOK: live adjustments of a running play (UE `UAudioComponent`): gain ramp from / to a level (`FadeIn` / `FadeOut` / `AdjustVolume`),
//! world position (`K2_SetWorldLocation`), a per-play attenuation override (`bOverrideAttenuation` + `AttenuationOverrides`), an explicit distance for
//! the attenuation curve (the listener-to-shape distance of a box shaped attenuation) and a low-pass cutoff (`SetLowPassFilterFrequency`).
//!
//! The state lives in `Mixer::adj` (one entry per adjusted play); the mixer reads it in `voice_targets` and advances the ramps once per block.
//! Separate from the music agent's `Command::Control` ramp (which starts from 1.0 and advances after the block): this one can start a play at a given level,
//! so a fade-in starts silent without a one-block click.

use crate::attenuation::Attenuation;
use crate::mixer::{PlayId, Vec3};
use std::sync::Arc;

/// What a [`Adjust`] changes (`None` = leave as is; `Some(None)` clears an override).
#[derive(Clone, Debug, Default)]
pub struct Adjust {
    /// `(from, to, seconds, stop_at_zero)`: ramp the play's gain linearly. `from == None` continues from the current gain (1.0 for a play never adjusted);
    /// `seconds == 0` jumps. With `stop_at_zero` the play ends when the gain reaches 0 (`AudioComponent::FadeOut`).
    pub gain: Option<(Option<f32>, f32, f32, bool)>,
    pub position: Option<Option<Vec3>>,
    pub atten: Option<Option<Arc<Attenuation>>>,
    /// Distance (cm) fed to the attenuation curve instead of the listener-to-position distance.
    pub dist: Option<Option<f32>>,
    /// Extra low-pass cutoff (Hz), combined (min) with the attenuation's own.
    pub lpf_hz: Option<Option<f32>>,
    pub floats: Vec<(String, f32)>,
}

#[derive(Clone, Debug)]
pub struct PlayAdj {
    pub id: PlayId,
    pub cur: f32,
    target: f32,
    /// gain units per second (`f32::INFINITY` = immediate)
    rate: f32,
    stop: bool,
    pub position: Option<Vec3>,
    pub atten: Option<Arc<Attenuation>>,
    pub dist: Option<f32>,
    pub lpf_hz: Option<f32>,
    pub floats: Vec<(String, f32)>,
}

/// Apply `a` to the entry of `id` (created on first use).
pub fn apply(adj: &mut Vec<PlayAdj>, id: PlayId, a: Adjust) {
    let i = match adj.iter().position(|c| c.id == id) {
        Some(i) => i,
        None => {
            adj.push(PlayAdj { id, cur: 1.0, target: 1.0, rate: 0.0, stop: false, position: None, atten: None, dist: None, lpf_hz: None, floats: Vec::new() });
            adj.len() - 1
        }
    };
    let c = &mut adj[i];
    if let Some((from, to, secs, stop)) = a.gain {
        if let Some(f) = from {
            c.cur = f.max(0.0);
        }
        c.target = to.max(0.0);
        c.stop = stop;
        if secs > 0.0 {
            c.rate = (c.target - c.cur).abs() / secs;
        } else {
            c.cur = c.target;
            c.rate = 0.0;
        }
    }
    if let Some(p) = a.position {
        c.position = p;
    }
    if let Some(t) = a.atten {
        c.atten = t;
    }
    if let Some(d) = a.dist {
        c.dist = d;
    }
    if let Some(l) = a.lpf_hz {
        c.lpf_hz = l;
    }
    for (n, v) in a.floats {
        match c.floats.iter_mut().find(|f| f.0 == n) {
            Some(f) => f.1 = v,
            None => c.floats.push((n, v)),
        }
    }
}

/// Advance every ramp by `dt` seconds; returns the plays that reached silence with the stop flag set (the mixer stops them).
pub fn advance(adj: &mut [PlayAdj], dt: f32) -> Vec<PlayId> {
    let mut stops = Vec::new();
    for c in adj.iter_mut() {
        if c.rate > 0.0 {
            let step = c.rate * dt;
            c.cur = if (c.target - c.cur).abs() <= step { c.target } else { c.cur + step.copysign(c.target - c.cur) };
            if c.cur == c.target {
                c.rate = 0.0;
            }
        }
        if c.stop && c.cur <= 0.0 {
            stops.push(c.id);
        }
    }
    stops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_starts_at_from_and_reaches_target_in_the_given_time() {
        let mut v = Vec::new();
        apply(&mut v, 7, Adjust { gain: Some((Some(0.0), 1.0, 5.0, false)), ..Default::default() });
        assert_eq!(v[0].cur, 0.0);
        for _ in 0..250 {
            advance(&mut v, 0.01);
        }
        assert!((v[0].cur - 0.5).abs() < 1e-3, "{}", v[0].cur);
        for _ in 0..260 {
            advance(&mut v, 0.01);
        }
        assert_eq!(v[0].cur, 1.0);
    }

    #[test]
    fn fade_out_to_zero_stops_the_play() {
        let mut v = Vec::new();
        apply(&mut v, 1, Adjust { gain: Some((None, 0.0, 1.0, true)), ..Default::default() });
        let mut stopped = vec![];
        for _ in 0..120 {
            stopped.extend(advance(&mut v, 0.01));
        }
        assert!(stopped.contains(&1));
    }

    #[test]
    fn zero_seconds_jumps_and_overrides_can_be_cleared() {
        let mut v = Vec::new();
        apply(&mut v, 1, Adjust { gain: Some((None, 0.25, 0.0, false)), dist: Some(Some(900.0)), lpf_hz: Some(Some(5000.0)), ..Default::default() });
        assert_eq!((v[0].cur, v[0].dist, v[0].lpf_hz), (0.25, Some(900.0), Some(5000.0)));
        apply(&mut v, 1, Adjust { dist: Some(None), ..Default::default() });
        assert_eq!((v[0].cur, v[0].dist, v[0].lpf_hz), (0.25, None, Some(5000.0)));
    }
}
