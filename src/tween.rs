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

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub spans: Vec<Span>,
    /// Maximum accepted error for [x, y, kx, ky, sx, sy, frame, alpha].
    pub max_errors: [f64; 8],
    /// Source transitions that cannot be represented safely by a standard
    /// shortest-path Adobe classic tween (for example a one-frame scale flip).
    /// Discrete artwork/frame switches are intentional cuts, and excluded.
    pub unfitted_intervals: Vec<usize>,
}
impl Plan {
    pub fn tween_count(&self) -> usize {
        self.spans.iter().filter(|s| s.tween).count()
    }
    pub fn compressed_tween_count(&self) -> usize {
        self.spans
            .iter()
            .filter(|s| s.tween && s.duration > 1)
            .count()
    }
}

/// The input must contain expanded/fill-forward transforms.
pub fn spans(transforms: &[Transform]) -> Vec<Span> {
    plan(transforms, Tolerance::default()).spans
}

/// Fit common linear/quadratic timing, splitting at errors and discrete changes.
/// A multi-revolution movement is split into spans under 169 degrees per axis,
/// so matrix decomposition cannot reverse its intended rotation direction.
pub fn plan(transforms: &[Transform], tolerance: Tolerance) -> Plan {
    let mut plan = Plan {
        spans: Vec::new(),
        max_errors: [0.; 8],
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
        for &i in &FIELDS {
            plan.max_errors[i] = plan.max_errors[i].max(fit.errors[i]);
        }
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
    errors: [f64; 8],
    split: usize,
}
fn fit(samples: &[Transform], start: usize, end: usize, limits: [f64; 8]) -> Fit {
    let invalid = || Fit {
        easing: 0,
        ratio: f64::INFINITY,
        errors: [0.; 8],
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
        errors: [0.; 8],
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
                fit.errors[i] = fit.errors[i].max(error);
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

/// Evaluate the emitted timing curve in raw reanim parameter space.
/// This is also useful to report reconstruction error to callers.
pub fn sample_span(samples: &[Transform], span: Span, frame: f64) -> [f64; 8] {
    let a = samples[span.start].values.map(f64::from);
    if !span.tween {
        return a;
    }
    let b = samples[span.start + span.duration].values.map(f64::from);
    let t = ((frame - span.start as f64) / span.duration as f64).clamp(0., 1.);
    let q = t + f64::from(span.easing) / 100. * t * (1. - t);
    let mut value = a;
    for i in FIELDS {
        value[i] = a[i] + (b[i] - a[i]) * q;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    fn transform(values: [f32; 8]) -> Transform {
        Transform {
            values,
            image: "IMAGE_REANIM_TEST".into(),
            ..Transform::default()
        }
    }
    fn assert_continuous_bound(samples: &[Transform], plan: &Plan, tolerance: Tolerance) {
        let limits = tolerance.fields();
        assert!(
            plan.unfitted_intervals.is_empty(),
            "{:?}",
            plan.unfitted_intervals
        );
        let mut covered = 0;
        for &span in &plan.spans {
            assert_eq!(span.start, covered);
            covered += span.duration;
            for offset in 0..span.duration {
                let before = span.start + offset;
                if before + 1 >= samples.len()
                    || !same_discrete(&samples[before], &samples[before + 1])
                {
                    continue;
                }
                for fraction in [0., 0.125, 0.25, 0.5, 0.75, 0.875, 0.999] {
                    let reconstructed = sample_span(samples, span, before as f64 + fraction);
                    for i in FIELDS {
                        let expected = f64::from(samples[before].values[i])
                            + (f64::from(samples[before + 1].values[i])
                                - f64::from(samples[before].values[i]))
                                * fraction;
                        assert!(
                            (expected - reconstructed[i]).abs() <= limits[i] + 1e-7,
                            "frame {before}+{fraction}, field {i}, error {}",
                            (expected - reconstructed[i]).abs()
                        );
                    }
                }
            }
        }
        assert_eq!(covered, samples.len());
    }
    #[test]
    fn linear_motion_becomes_two_keys_for_every_parameter() {
        let samples: Vec<_> = (0..101)
            .map(|i| {
                let t = i as f32 / 100.;
                transform([
                    100. * t,
                    -20. * t,
                    45. * t,
                    -30. * t,
                    1. + t,
                    2. - t,
                    0.,
                    1. - t,
                ])
            })
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert_eq!(plan.spans.len(), 2);
        assert_eq!(plan.spans[0].duration, 100);
        assert!(plan.spans[0].tween);
        assert_eq!(plan.spans[0].easing, 0);
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn eased_motion_is_recovered_and_checked_between_frames() {
        let samples: Vec<_> = (0..41)
            .map(|i| {
                let t = i as f32 / 40.;
                let q = t + 0.6 * t * (1. - t);
                transform([
                    10. * q,
                    -5. * q,
                    30. * q,
                    20. * q,
                    1. + 0.1 * q,
                    1. - 0.2 * q,
                    0.,
                    1. - 0.2 * q,
                ])
            })
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert_eq!(plan.spans.len(), 2);
        assert_eq!(plan.spans[0].easing, 60);
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn curved_baked_motion_is_split_and_still_has_real_multiframe_tweens() {
        let samples: Vec<_> = (0..121)
            .map(|i| {
                let t = i as f32 / 120.;
                transform([
                    30. * (t * 3.).sin(),
                    10. * (t * 4.).cos(),
                    20. * t,
                    15. * t,
                    1. + 0.1 * (t * 2.).sin(),
                    1.,
                    0.,
                    0.8,
                ])
            })
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert!(
            plan.spans.len() < samples.len() / 2,
            "{} keys",
            plan.spans.len()
        );
        assert!(plan.compressed_tween_count() > 0);
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn artwork_visibility_and_frame_changes_cut_spans() {
        let mut samples: Vec<_> = (0..20)
            .map(|i| transform([i as f32, 0., 0., 0., 1., 1., 0., 1.]))
            .collect();
        for t in &mut samples[5..10] {
            t.image = "OTHER".into();
        }
        for t in &mut samples[10..15] {
            t.values[6] = -1.;
        }
        for t in &mut samples[15..] {
            t.values[6] = 2.;
        }
        let plan = plan(&samples, Tolerance::default());
        for s in &plan.spans {
            if s.tween {
                assert!(same_discrete(
                    &samples[s.start],
                    &samples[s.start + s.duration]
                ));
            }
        }
        for start in [5, 10, 15] {
            assert!(plan.spans.iter().any(|s| s.start == start));
        }
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn accumulated_rotation_keeps_direction_without_fullturn_aliasing() {
        let samples: Vec<_> = (0..49)
            .map(|i| transform([0., 0., i as f32 * 15., i as f32 * 15., 1., 1., 0., 1.]))
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert!(plan.spans.len() < samples.len() / 2);
        for s in &plan.spans {
            if s.tween {
                assert!(
                    samples[s.start + s.duration].values[2] - samples[s.start].values[2] <= 169.
                );
            }
        }
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn opposite_axis_rotation_crosses_signed_angle_boundaries() {
        let samples: Vec<_> = (0..31)
            .map(|i| {
                transform([
                    0.,
                    0.,
                    170. + i as f32 * 4.,
                    -170. - i as f32 * 4.,
                    1.,
                    2.,
                    0.,
                    1.,
                ])
            })
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert_eq!(plan.spans.len(), 2);
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn xfl_endpoint_matrices_and_easing_recompile_with_original_direction() {
        let samples: Vec<_> = (0..41)
            .map(|i| {
                let t = i as f32 / 40.;
                let q = t + 0.6 * t * (1. - t);
                transform([
                    10. * q,
                    -5. * q,
                    170. + 30. * q,
                    -170. - 20. * q,
                    1. + 0.1 * q,
                    1. - 0.2 * q,
                    0.,
                    1. - 0.2 * q,
                ])
            })
            .collect();
        let plan = plan(&samples, Tolerance::default());
        assert_eq!(plan.spans.len(), 2);
        assert_eq!(plan.spans[0].easing, 60);
        let temp = tempfile::tempdir().unwrap();
        let mut xml = String::from(
            "<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name=\"locator_track\"><frames>",
        );
        for span in plan.spans {
            let t = &samples[span.start].values;
            let kx = f64::from(t[2]).to_radians();
            let ky = f64::from(t[3]).to_radians();
            let timing = if span.tween {
                format!(" tweenType=\"motion\" acceleration=\"{}\"", -span.easing)
            } else {
                String::new()
            };
            xml.push_str(&format!("<DOMFrame index=\"{}\" duration=\"{}\"{}><elements><DOMSymbolInstance libraryItemName=\"locator\"><matrix><Matrix a=\"{}\" b=\"{}\" c=\"{}\" d=\"{}\" tx=\"{}\" ty=\"{}\"/></matrix><transformationPoint><Point x=\"0\" y=\"0\"/></transformationPoint><color><Color alphaMultiplier=\"{}\"/></color></DOMSymbolInstance></elements></DOMFrame>",span.start,span.duration,timing,f64::from(t[4])*kx.cos(),f64::from(t[4])*kx.sin(),-f64::from(t[5])*ky.sin(),f64::from(t[5])*ky.cos(),t[0],t[1],t[7]));
        }
        xml.push_str("</frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>");
        std::fs::write(temp.path().join("DOMDocument.xml"), xml).unwrap();
        let compiled = crate::forward::convert(temp.path()).unwrap();
        let actual = &compiled
            .tracks
            .iter()
            .find(|t| t.name == "locator_track")
            .unwrap()
            .transforms;
        for (source, target) in samples.iter().zip(actual) {
            for i in FIELDS {
                let delta = if i == 2 || i == 3 {
                    (f64::from(source.values[i]) - f64::from(target.values[i]) + 180.)
                        .rem_euclid(360.)
                        - 180.
                } else {
                    f64::from(source.values[i]) - f64::from(target.values[i])
                };
                let tolerance = if i < 4 { 0.051 } else { 0.00051 };
                assert!(
                    delta.abs() <= tolerance,
                    "field {i} {source:?} -> {target:?}"
                );
            }
        }
    }
    #[test]
    fn subframe_extremum_rejects_a_false_integer_only_fit() {
        let samples = vec![
            transform([0., 0., 0., 0., 1., 1., 0., 1.]),
            transform([7.5, 0., 0., 0., 1., 1., 0., 1.]),
            transform([10., 0., 0., 0., 1., 1., 0., 1.]),
        ];
        let plan = plan(&samples, Tolerance::default());
        // A 100% quadratic ease hits all three whole frames exactly but has a
        // 0.625px error halfway through each interval against the engine.
        assert_ne!(plan.spans[0].duration, 2);
        assert_continuous_bound(&samples, &plan, Tolerance::default());
    }
    #[test]
    fn constant_holds_merge_and_scale_sign_flips_are_reported() {
        let samples = vec![Transform::default(); 50];
        let p = plan(&samples, Tolerance::default());
        assert_eq!(p.spans.len(), 1);
        assert!(!p.spans[0].tween);
        assert_eq!(p.spans[0].duration, 50);
        let samples = vec![
            transform([0., 0., 0., 0., -1., 1., 0., 1.]),
            transform([0., 0., 0., 0., 1., 1., 0., 1.]),
        ];
        let p = plan(&samples, Tolerance::default());
        assert_eq!(p.unfitted_intervals, vec![0]);
    }
}
