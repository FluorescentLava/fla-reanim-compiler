//! Reconstruct editable classic motion tweens from sampled reanim poses.
//!
//! Reanim interpolates x/y, both angles, both scales and alpha independently
//! between adjacent samples (TodLib/Reanimator.cpp::GetTransformAtTime).
//! Adobe's JSFL classic ease E uses q(t) = t + E*t*(1-t), E in [-1, 1].
//! XFL stores acceleration = -100*E (the opposite sign of JSFL tweenEasing).
//! A span is accepted only after bounding its residual on *every whole unit
//! interval*, including the quadratic residual's interior stationary point.
//! This checks all subframes, rather than just comparing integer keyframes.
use crate::Transform;
use serde::Serialize;

const FIELDS: [usize; 7] = [0, 1, 2, 3, 4, 5, 7];
const MAX_ANGLE_SPAN: f64 = 169.;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Tolerance {
    /// Translation error in document pixels.
    pub position: f64,
    /// Error in each skew/rotation angle, in degrees.
    pub angle: f64,
    /// Absolute scale error.
    pub scale: f64,
    /// Absolute opacity error (1 is fully opaque).
    pub alpha: f64,
}
impl Default for Tolerance {
    fn default() -> Self {
        Self {
            position: 0.05,
            angle: 0.02,
            scale: 0.0005,
            alpha: 0.001,
        }
    }
}
impl Tolerance {
    /// Compact authoring preset. Parameter limits are deliberately distinct
    /// from a rendered pixel bound, which also depends on bitmap dimensions.
    pub fn editable() -> Self {
        Self {
            position: 0.25,
            angle: 0.1,
            scale: 0.002,
            alpha: 0.004,
        }
    }
    fn fields(self) -> [f64; 8] {
        [
            self.position,
            self.position,
            self.angle,
            self.angle,
            self.scale,
            self.scale,
            1.,
            self.alpha,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: usize,
    /// For a tween, the next endpoint key is at start + duration.
    /// For a hold, the duration is the number of frames held by this key.
    pub duration: usize,
    pub tween: bool,
    /// JSFL tweenEasing percentage. Positive eases out, negative eases in.
    /// Write its negative to the actual XFL acceleration attribute.
    pub easing: i32,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub spans: Vec<Span>,
    /// Source transitions that cannot be represented safely by a standard
    /// shortest-path Adobe classic tween (for example a one-frame scale flip).
    /// Discrete artwork/frame switches are intentional cuts, and excluded.
    pub unfitted_intervals: Vec<usize>,
}
/// Fit common linear/quadratic timing, splitting at errors and discrete changes.
/// A multi-revolution movement is split into spans under 169 degrees per axis,
/// so matrix decomposition cannot reverse its intended rotation direction.
pub fn plan(transforms: &[Transform], tolerance: Tolerance) -> Plan {
    let mut plan = Plan {
        spans: Vec::new(),
        unfitted_intervals: Vec::new(),
    };
    if transforms.is_empty() {
        return plan;
    }
    let limits = tolerance.fields();
    assert!(
        FIELDS
            .iter()
            .all(|&i| limits[i].is_finite() && limits[i] > 0.),
        "Tween tolerances must be positive and finite"
    );
    let mut begin = 0;
    while begin < transforms.len() {
        let mut end = begin + 1;
        while end < transforms.len() && same_discrete(&transforms[begin], &transforms[end]) {
            end += 1;
        }
        if end > begin + 1 {
            split(transforms, begin, end - 1, limits, &mut plan);
        }
        plan.spans.push(Span {
            start: end - 1,
            duration: 1,
            tween: false,
            easing: 0,
        });
        begin = end;
    }
    // A final endpoint of a constant span does not need a separate keyframe.
    // Never merge a tween endpoint: it carries the geometry Adobe interpolates to.
    let mut merged: Vec<Span> = Vec::with_capacity(plan.spans.len());
    for span in plan.spans {
        if let Some(previous) = merged.last_mut()
            && !previous.tween
            && !span.tween
            && previous.start + previous.duration == span.start
            && transforms[previous.start] == transforms[span.start]
        {
            previous.duration += span.duration;
        } else {
            merged.push(span);
        }
    }
    plan.spans = merged;
    plan
}

fn same_discrete(a: &Transform, b: &Transform) -> bool {
    a.image == b.image && a.font == b.font && a.text == b.text && a.values[6] == b.values[6]
}

fn split(samples: &[Transform], start: usize, end: usize, limits: [f64; 8], plan: &mut Plan) {
    let fit = fit(samples, start, end, limits);
    if fit.ratio <= 1. + 1e-8 {
        let moving = FIELDS
            .iter()
            .any(|&i| samples[start].values[i] != samples[end].values[i]);
        plan.spans.push(Span {
            start,
            duration: end - start,
            tween: moving,
            easing: fit.easing,
        });
    } else if end == start + 1 {
        plan.spans.push(Span {
            start,
            duration: 1,
            tween: false,
            easing: 0,
        });
        plan.unfitted_intervals.push(start);
    } else {
        let at = fit.split.clamp(start + 1, end - 1);
        split(samples, start, at, limits, plan);
        split(samples, at, end, limits, plan);
    }
}

struct Fit {
    easing: i32,
    ratio: f64,
    split: usize,
}
fn fit(samples: &[Transform], start: usize, end: usize, limits: [f64; 8]) -> Fit {
    let invalid = || Fit {
        easing: 0,
        ratio: f64::INFINITY,
        split: (start + end) / 2,
    };
    let a = &samples[start].values;
    let b = &samples[end].values;
    if FIELDS
        .iter()
        .any(|&i| !a[i].is_finite() || !b[i].is_finite())
    {
        return invalid();
    }
    // A matrix stores angles modulo 360 degrees. Keeping each span shorter
    // than a half-turn makes the two independently-skewed axes unambiguous.
    if [2, 3]
        .iter()
        .any(|&i| (f64::from(b[i]) - f64::from(a[i])).abs() > MAX_ANGLE_SPAN)
    {
        return invalid();
    }
    for index in [4, 5] {
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for sample in &samples[start..=end] {
            let value = f64::from(sample.values[index]);
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
        // Signed scale through zero cannot be inferred safely from endpoint
        // matrices: Adobe may turn 180 degrees instead of flipping the scale.
        if minimum < 0. && maximum > 0. {
            return invalid();
        }
        let angle = index - 2;
        for endpoint in [a, b] {
            if endpoint[index].abs() <= 1e-6 && endpoint[angle].rem_euclid(360.).abs() > 1e-4 {
                return invalid();
            }
        }
    }
    let length = (end - start) as f64;
    let mut numerator = 0.;
    let mut denominator = 0.;
    for (offset, sample) in samples[start + 1..end].iter().enumerate() {
        let t = (offset + 1) as f64 / length;
        let q = t * (1. - t);
        for &i in &FIELDS {
            if !sample.values[i].is_finite() {
                return invalid();
            }
            let delta = f64::from(b[i]) - f64::from(a[i]);
            let coefficient = delta * q / limits[i];
            let residual = (f64::from(sample.values[i]) - f64::from(a[i]) - delta * t) / limits[i];
            numerator += coefficient * residual;
            denominator += coefficient * coefficient;
        }
    }
    let estimate = if denominator > 1e-20 {
        (numerator / denominator).clamp(-1., 1.) * 100.
    } else {
        0.
    };
    let rounded = estimate.round() as i32;
    let mut candidates = vec![0, rounded];
    candidates.extend((rounded - 2..=rounded + 2).map(|e| e.clamp(-100, 100)));
    candidates.sort_unstable();
    candidates.dedup();
    let mut best = invalid();
    for easing in candidates {
        let candidate = evaluate(samples, start, end, limits, easing);
        // Prefer no ease when both meet the requested bounds: it preserves
        // the engine's linear interpolation on every subframe exactly.
        if easing == 0 && candidate.ratio <= 1. + 1e-8 {
            return candidate;
        }
        if candidate.ratio < best.ratio {
            best = candidate;
        }
    }
    best
}

fn evaluate(samples: &[Transform], start: usize, end: usize, limits: [f64; 8], easing: i32) -> Fit {
    let mut fit = Fit {
        easing,
        ratio: 0.,
        split: (start + end) / 2,
    };
    let count = (end - start) as f64;
    let step = 1. / count;
    let ease = f64::from(easing) / 100.;
    let a = &samples[start].values;
    let b = &samples[end].values;
    for frame in start..end {
        let t = (frame - start) as f64 / count;
        for &i in &FIELDS {
            let delta = f64::from(b[i]) - f64::from(a[i]);
            // At local u in this engine-frame interval, prediction-target is
            // constant + linear*u + quadratic*u^2. These are its exact extrema.
            let constant = f64::from(a[i]) + delta * (t + ease * t * (1. - t))
                - f64::from(samples[frame].values[i]);
            let linear = delta * step * (1. + ease - 2. * ease * t)
                - (f64::from(samples[frame + 1].values[i]) - f64::from(samples[frame].values[i]));
            let quadratic = -delta * ease * step * step;
            let mut positions = [0., 1., 0.];
            if quadratic.abs() > 1e-30 {
                positions[2] = (-linear / (2. * quadratic)).clamp(0., 1.);
            }
            for u in positions {
                let error = (constant + u * (linear + u * quadratic)).abs();
                let ratio = error / limits[i];
                if ratio > fit.ratio {
                    fit.ratio = ratio;
                    // Choose the nearest interior integer sample. Degenerate
                    // endpoints are clamped by split() to guarantee progress.
                    fit.split = if u < 0.5 { frame } else { frame + 1 };
                }
            }
        }
    }
    fit
}
