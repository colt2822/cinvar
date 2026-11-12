//! Keyframe animation: compile a `KeyframeTrack` into an ffmpeg expression in clip-local time `t`.
//!
//! `t` is seconds since the clip's first visible frame, after speed changes, which is the same
//! clock the keyframe `at` values are stored in. Before the first keyframe the value holds at
//! the first keyframe's value; after the last it holds at the last value.

use crate::error::{Error, Result};
use crate::project::model::KeyframeTrack;

/// Properties the renderer can animate.
pub const ANIMATABLE: &[&str] = &["x", "y", "scale", "opacity", "rotation"];

/// Easing curves understood by the renderer.
pub const EASINGS: &[&str] = &["linear", "ease_in", "ease_out", "ease_in_out", "hold"];

pub fn check_property(property: &str) -> Result<()> {
    if ANIMATABLE.contains(&property) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "cannot animate '{property}'; animatable properties are: {}",
            ANIMATABLE.join(", ")
        ))
        .with_hint("use x, y, scale, opacity or rotation"))
    }
}

pub fn check_easing(easing: &str) -> Result<()> {
    if EASINGS.contains(&easing) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "unknown easing '{easing}'; supported: {}",
            EASINGS.join(", ")
        )))
    }
}

/// Normalised easing of progress `p` in 0..1, as an ffmpeg expression fragment.
fn ease_expr(easing: &str, p: &str) -> String {
    match easing {
        "ease_in" => format!("({p})*({p})"),
        "ease_out" => format!("(1-(1-({p}))*(1-({p})))"),
        // Smoothstep: zero velocity at both ends.
        "ease_in_out" => format!("(({p})*({p})*(3-2*({p})))"),
        _ => format!("({p})"),
    }
}

fn num(value: f64) -> String {
    let rounded = format!("{value:.6}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// An expression in `t` for the track, or the constant when there is one keyframe.
/// Segment easing is taken from the keyframe that *starts* the segment.
#[allow(dead_code)]
pub fn compile(track: &KeyframeTrack) -> Result<String> {
    compile_shifted(track, 0.0)
}

/// Like `compile`, for a render whose clock starts `shift` seconds into the clip (range renders).
pub fn compile_shifted(track: &KeyframeTrack, shift: f64) -> Result<String> {
    check_property(&track.property)?;
    let mut frames: Vec<(f64, f64, &str)> = track
        .keyframes
        .iter()
        .map(|k| (k.at.as_secs_f64() - shift, k.value, k.easing.as_str()))
        .collect();
    if frames.is_empty() {
        return Err(Error::invalid(format!(
            "keyframe track '{}' has no keyframes",
            track.property
        )));
    }
    for (_, value, easing) in &frames {
        if !value.is_finite() {
            return Err(Error::invalid(format!(
                "keyframe for '{}' has a non-finite value",
                track.property
            )));
        }
        check_easing(if easing.is_empty() { "linear" } else { easing })?;
    }
    frames.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if frames.len() == 1 {
        return Ok(num(frames[0].1));
    }
    // Build from the tail so the result nests as if(lt(t,t1), seg0, if(lt(t,t2), seg1, ... last)).
    let last = frames[frames.len() - 1];
    let mut expr = num(last.1);
    for window in frames.windows(2).rev() {
        let (t0, v0, easing) = window[0];
        let (t1, v1, _) = window[1];
        let span = (t1 - t0).max(1e-6);
        let easing = if easing.is_empty() { "linear" } else { easing };
        let segment = if easing == "hold" {
            num(v0)
        } else {
            let p = format!("(t-{})/{}", num(t0), num(span));
            format!("{}+({})*{}", num(v0), num(v1 - v0), ease_expr(easing, &p))
        };
        expr = format!("if(lt(t,{}),{},{})", num(t1), segment, expr);
    }
    // Hold the first value before the first keyframe.
    Ok(format!(
        "if(lt(t,{}),{},{})",
        num(frames[0].0),
        num(frames[0].1),
        expr
    ))
}

/// The same expression with the time variable renamed for filters that call it `T` (geq).
pub fn with_time_var(expr: &str, var: &str) -> String {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::with_capacity(expr.len());
    for (i, c) in chars.iter().enumerate() {
        let prev_alpha = i > 0 && (chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
        let next_alpha = chars
            .get(i + 1)
            .map(|n| n.is_ascii_alphanumeric() || *n == '_')
            .unwrap_or(false);
        if *c == 't' && !prev_alpha && !next_alpha {
            out.push_str(var);
        } else {
            out.push(*c);
        }
    }
    out
}

/// True when the track changes over time.
pub fn is_animated(track: &KeyframeTrack) -> bool {
    track.keyframes.len() >= 2
}

/// Reference evaluator used by tests and by tools that need the value at a time.
#[allow(dead_code)]
pub fn value_at(track: &KeyframeTrack, t: f64) -> f64 {
    let mut frames: Vec<(f64, f64, &str)> = track
        .keyframes
        .iter()
        .map(|k| (k.at.as_secs_f64(), k.value, k.easing.as_str()))
        .collect();
    frames.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let Some(first) = frames.first() else {
        return 0.0;
    };
    if t <= first.0 {
        return first.1;
    }
    for window in frames.windows(2) {
        let (t0, v0, easing) = window[0];
        let (t1, v1, _) = window[1];
        if t < t1 {
            let p = ((t - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
            let eased = match easing {
                "hold" => return v0,
                "ease_in" => p * p,
                "ease_out" => 1.0 - (1.0 - p) * (1.0 - p),
                "ease_in_out" => p * p * (3.0 - 2.0 * p),
                _ => p,
            };
            return v0 + (v1 - v0) * eased;
        }
    }
    frames[frames.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::Keyframe;
    use crate::util::time::Time;

    fn track(property: &str, frames: &[(f64, f64, &str)]) -> KeyframeTrack {
        KeyframeTrack {
            property: property.into(),
            keyframes: frames
                .iter()
                .map(|(at, value, easing)| Keyframe {
                    at: Time::from_secs_f64(*at),
                    value: *value,
                    easing: (*easing).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn renames_only_the_time_variable() {
        assert_eq!(
            with_time_var("if(lt(t,2),(t-1)/3,1)", "T"),
            "if(lt(T,2),(T-1)/3,1)"
        );
    }

    #[test]
    fn single_keyframe_is_a_constant() {
        assert_eq!(
            compile(&track("scale", &[(1.0, 1.25, "linear")])).unwrap(),
            "1.25"
        );
    }

    #[test]
    fn linear_two_point_expression() {
        let e = compile(&track(
            "scale",
            &[(0.0, 1.0, "linear"), (4.0, 2.0, "linear")],
        ))
        .unwrap();
        assert!(e.starts_with("if(lt(t,0),1,"), "{e}");
        assert!(e.contains("1+(1)*((t-0)/4)"), "{e}");
    }

    #[test]
    fn reference_values_hold_and_interpolate() {
        let tr = track(
            "opacity",
            &[
                (1.0, 0.0, "linear"),
                (3.0, 1.0, "ease_in_out"),
                (5.0, 0.2, "linear"),
            ],
        );
        assert_eq!(value_at(&tr, 0.0), 0.0);
        // first segment (1 s -> 3 s) uses the linear easing of the keyframe that starts it
        assert!((value_at(&tr, 2.0) - 0.5).abs() < 1e-9);
        assert_eq!(value_at(&tr, 3.0), 1.0);
        assert!((value_at(&tr, 4.0) - 0.6).abs() < 1e-9);
        assert_eq!(value_at(&tr, 9.0), 0.2);
    }

    #[test]
    fn ease_in_out_is_smoothstep() {
        let tr = track("x", &[(0.0, 0.0, "ease_in_out"), (2.0, 100.0, "linear")]);
        assert!((value_at(&tr, 1.0) - 50.0).abs() < 1e-9);
        assert!((value_at(&tr, 0.5) - 15.625).abs() < 1e-9);
    }

    #[test]
    fn hold_steps_at_the_next_keyframe() {
        let tr = track("x", &[(0.0, 10.0, "hold"), (2.0, 90.0, "linear")]);
        assert_eq!(value_at(&tr, 1.9), 10.0);
        assert_eq!(value_at(&tr, 2.0), 90.0);
    }

    #[test]
    fn rejects_unknown_property_and_easing() {
        assert!(compile(&track(
            "blur",
            &[(0.0, 1.0, "linear"), (1.0, 2.0, "linear")]
        ))
        .is_err());
        assert!(compile(&track("x", &[(0.0, 1.0, "bounce"), (1.0, 2.0, "linear")])).is_err());
        assert!(compile(&track("x", &[(0.0, f64::NAN, "linear")])).is_err());
    }

    #[test]
    fn expression_has_balanced_parentheses_and_no_quotes() {
        let tr = track(
            "x",
            &[
                (0.0, 0.0, "ease_in"),
                (1.0, 5.0, "ease_out"),
                (2.0, 9.0, "linear"),
                (3.5, 1.0, "linear"),
            ],
        );
        let e = compile(&tr).unwrap();
        assert_eq!(e.matches('(').count(), e.matches(')').count());
        assert!(!e.contains('\''));
    }
}
