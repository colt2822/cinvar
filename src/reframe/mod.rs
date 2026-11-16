//! Subject-aware reframing: track the main person in a shot with a real
//! detector (torchvision FastERRCNN via a helper venv), then crop a moving
//! window around the subject path to the target aspect ratio with ffmpeg.
//!
//! Pattern matches the other integrations: per-tool venv selected by
//! `CINVAR_TRACK_PYTHON`, device by `CINVAR_TRACK_DEVICE` (default prefers the
//! second GPU so the desktop GPU stays untouched), weights never downloaded
//! by CINVAR itself (they are cached on first manual use; the helper refuses
//! to download).
pub mod track_helper;

use crate::error::{Error, Result};
use crate::util::proc::{self, ProcOpts};
use serde::Deserialize;
use std::path::Path;

const TIMEOUT_SECS: u64 = 1800;
pub const ENV_PYTHON: &str = "CINVAR_TRACK_PYTHON";

#[derive(Debug, Deserialize)]
pub struct TrackReport {
    pub width: u32,
    pub height: u32,
    pub sample_fps: f64,
    pub frames_sampled: usize,
    pub frames_with_person: usize,
    pub device: String,
    pub keyframes: Vec<Keyframe>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Keyframe {
    pub t: f64,
    pub cx: f64,
    pub cy: f64,
    pub score: f32,
}

/// Median-smooth the path over a window so single-frame detector jitter does
/// not make the crop dance.
fn smooth(values: &[f64], window: usize) -> Vec<f64> {
    let w = window.max(1);
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let lo = i.saturating_sub(w / 2);
            let hi = (i + w / 2 + 1).min(values.len());
            let mut s: Vec<f64> = values[lo..hi].to_vec();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            s[s.len() / 2]
        })
        .collect()
}

/// Piecewise-linear ffmpeg expression for one coordinate over the keyframes.
/// Commas inside the quoted filter value are safe; nesting depth is capped by
/// rdp_simplify below (deep nested if() chains blow ffmpeg's eval limits).
fn ffmpeg_lerp_expr(times: &[f64], coords: &[f64]) -> String {
    if times.is_empty() {
        return "0".to_string();
    }
    if times.len() == 1 {
        return format!("{:.1}", coords[0]);
    }
    let mut expr = format!("{:.1}", coords[coords.len() - 1]);
    for i in (0..times.len() - 1).rev() {
        let (t0, c0) = (times[i], coords[i]);
        let (t1, c1) = (times[i + 1], coords[i + 1]);
        let slope = if t1 > t0 { (c1 - c0) / (t1 - t0) } else { 0.0 };
        expr = format!(
            "if(lt(t,{t0:.3}),{c0:.1}+({slope:.4})*(t-{t0:.3}),{expr})"
        );
    }
    expr
}

/// Split keyframes into per-shot segments: a detector jump larger than
/// `jump_frac` of the frame width between consecutive samples means a scene
/// cut (or a subject switch), not motion — smoothing must not span it.
fn cut_segments(times: &[f64], xs: &[f64], width: u32, sample_fps: f64) -> Vec<(usize, usize)> {
    let jump = width as f64 * 0.35;
    let max_gap = 1.5 / sample_fps.max(0.1);
    let mut segs = Vec::new();
    let mut start = 0usize;
    for i in 1..times.len() {
        if (xs[i] - xs[i - 1]).abs() > jump || (times[i] - times[i - 1]) > max_gap {
            segs.push((start, i));
            start = i;
        }
    }
    segs.push((start, times.len()));
    segs
}

/// Ramer-Douglas-Peucker style decimation of a tracked path: keep points whose
/// deviation from the simplified path exceeds `eps`, always keeping the first
/// and last. ffmpeg's expression evaluator chokes on deeply nested if() chains
/// (~100+), so the path must be simplified before it becomes an expression.
fn rdp_simplify(times: &[f64], coords: &[f64], eps: f64) -> Vec<usize> {
    if times.len() <= 2 {
        return (0..times.len()).collect();
    }
    let mut keep = vec![0usize];
    for i in 1..times.len() - 1 {
        let j = *keep.last().unwrap();
        let k = i + 1;
        let span = times[k] - times[j];
        let predicted = if span.abs() > 1e-9 {
            coords[j] + (coords[k] - coords[j]) * (times[i] - times[j]) / span
        } else {
            coords[j]
        };
        if (coords[i] - predicted).abs() > eps {
            keep.push(i);
        }
    }
    keep.push(times.len() - 1);
    keep.dedup();
    keep
}

/// Simplify to at most `max_points` points by raising eps until it fits.
fn simplify_to(times: &[f64], coords: &[f64], max_points: usize) -> Vec<usize> {
    let mut eps = 10.0;
    loop {
        let idx = rdp_simplify(times, coords, eps);
        if idx.len() <= max_points || eps > 10_000.0 {
            return idx;
        }
        eps *= 1.6;
    }
}

/// Greedy deviation decimation with mandatory points: raise eps until the
/// path fits `max_points`, but indices in `must_keep` survive at every eps
/// (cut-boundary points — dropping them would re-introduce cross-cut slide).
fn simplify_capped(times: &[f64], coords: &[f64], max_points: usize, must_keep: &[usize]) -> Vec<usize> {
    let n = times.len();
    let mut eps = 10.0;
    loop {
        let mut keep: Vec<usize> = vec![0];
        for i in 1..n - 1 {
            let j = *keep.last().unwrap();
            let k = i + 1;
            let span = times[k] - times[j];
            let predicted = if span.abs() > 1e-9 {
                coords[j] + (coords[k] - coords[j]) * (times[i] - times[j]) / span
            } else {
                coords[j]
            };
            if (coords[i] - predicted).abs() > eps || must_keep.contains(&i) {
                keep.push(i);
            }
        }
        keep.push(n - 1);
        keep.dedup();
        if keep.len() <= max_points || eps > 10_000.0 {
            return keep;
        }
        eps *= 1.6;
    }
}

/// Assemble the per-shot simplified crop path (with hold-and-takeover points
/// at every hard cut) and enforce the global 60-point cap. The cap is on the
/// TOTAL path: a cut-heavy source gets one segment per cut, and ffmpeg's eval
/// parser dies on ~100+ nested if() levels. Cut-boundary points are mandatory
/// so every hard-cut jump survives decimation.
fn assemble_path(
    ts: &[f64],
    xs: &[f64],
    ys: &[f64],
    segs: &[(usize, usize)],
) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut ts_x: Vec<f64> = Vec::new();
    let mut xs_s: Vec<f64> = Vec::new();
    let mut ts_y: Vec<f64> = Vec::new();
    let mut ys_s: Vec<f64> = Vec::new();
    let mut x_keep: Vec<usize> = Vec::new();
    let mut y_keep: Vec<usize> = Vec::new();
    for (si, &(a, b)) in segs.iter().enumerate() {
        let seg_ts = ts[a..b].to_vec();
        for i in simplify_to(&seg_ts, &xs[a..b], 60) {
            if si + 1 < segs.len() && i == b - a - 1 {
                x_keep.push(ts_x.len()); // segment's last point: holds until the cut
            }
            ts_x.push(seg_ts[i]);
            xs_s.push(xs[a + i]);
        }
        for i in simplify_to(&seg_ts, &ys[a..b], 60) {
            if si + 1 < segs.len() && i == b - a - 1 {
                y_keep.push(ts_y.len());
            }
            ts_y.push(seg_ts[i]);
            ys_s.push(ys[a + i]);
        }
        // Hard cut: the crop must JUMP to the next shot's window, not slide
        // across the cut. At the cut instant hold this segment's last edge,
        // then 1 ms later take over the next segment's first edge.
        if si + 1 < segs.len() {
            let cut_t = ts[b - 1];
            let (next_x, next_y) = (xs[b], ys[b]);
            ts_x.push(cut_t);
            xs_s.push(xs[b - 1]);
            x_keep.push(ts_x.len()); // takeover point: next segment's first edge
            ts_x.push(cut_t + 0.001);
            xs_s.push(next_x);
            ts_y.push(cut_t);
            ys_s.push(ys[b - 1]);
            y_keep.push(ts_y.len());
            ts_y.push(cut_t + 0.001);
            ys_s.push(next_y);
        }
    }
    if ts_x.len() > 60 {
        let keep = simplify_capped(&ts_x, &xs_s, 60, &x_keep);
        ts_x = keep.iter().map(|&i| ts_x[i]).collect();
        xs_s = keep.iter().map(|&i| xs_s[i]).collect();
    }
    if ts_y.len() > 60 {
        let keep = simplify_capped(&ts_y, &ys_s, 60, &y_keep);
        ts_y = keep.iter().map(|&i| ts_y[i]).collect();
        ys_s = keep.iter().map(|&i| ys_s[i]).collect();
    }
    (ts_x, xs_s, ts_y, ys_s)
}

fn run_helper(input: &Path, work_dir: &Path, sample_fps: f64) -> Result<TrackReport> {
    let python = std::env::var(ENV_PYTHON)
        .map_err(|_| {
            Error::invalid("no tracking helper configured").with_hint(format!(
                "set {ENV_PYTHON} to a Python with torch+torchvision and the FastERRCNN COCO weights cached"
            ))
        })?;
    std::fs::create_dir_all(work_dir)?;
    let script = work_dir.join("track_helper.py");
    std::fs::write(&script, track_helper::HELPER)?;
    // proc::run env-clears children, so documented tuning envs must be
    // forwarded explicitly or the helper silently ignores them.
    let mut o = ProcOpts::default().with_timeout(TIMEOUT_SECS);
    for var in ["CINVAR_TRACK_DEVICE", "CINVAR_TRACK_CONF"] {
        if let Ok(v) = std::env::var(var) {
            o.env.push((var.to_string(), v));
        }
    }
    let out = proc::run(
        Path::new(&python),
        &[
            "-I".to_string(),
            script.to_string_lossy().to_string(),
            input.to_string_lossy().to_string(),
            format!("{sample_fps}"),
        ],
        &o,
    )?;
    if !out.success {
        return Err(Error::internal(format!(
            "track helper failed: {}",
            crate::secrets::redact(&out.tail_stderr(3))
        )));
    }
    let line = out
        .stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| Error::internal("track helper printed no JSON result"))?;
    serde_json::from_str(line).map_err(|e| Error::internal(format!("invalid track JSON: {e}")))
}

#[derive(Debug)]
pub struct ReframeReport {
    pub track: TrackReport,
    pub output: String,
    pub crop_w: u32,
    pub crop_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub seconds: f64,
}

/// Reframe `input` to `target` ("vertical" 9:16, "square" 1:1) keeping the
/// tracked subject centered. Returns the output path.
pub fn reframe(
    input: &Path,
    output: &Path,
    target: &str,
    work_dir: &Path,
    sample_fps: f64,
) -> Result<ReframeReport> {
    if !input.is_file() {
        return Err(Error::invalid(format!("input not found: {}", input.display())));
    }
    let track = run_helper(input, work_dir, sample_fps)?;
    let (out_w, out_h) = match target {
        "vertical" => (1080, 1920),
        "square" => (1080, 1080),
        "landscape" => (1920, 1080),
        other => {
            return Err(Error::invalid(format!(
                "unknown target `{other}`; use vertical, square or landscape"
            )))
        }
    };
    if track.keyframes.is_empty() {
        return Err(Error::invalid(
            "no subject detected in the footage; reframe needs at least one person in frame",
        )
        .with_hint("plain scaling (no subject awareness) is available by cropping without --track"));
    }

    // Crop window inside the source: fill the output AR.
    let src_ar = track.width as f64 / track.height as f64;
    let out_ar = out_w as f64 / out_h as f64;
    let (crop_w, crop_h) = if src_ar > out_ar {
        // source wider: crop horizontally
        let h = track.height;
        let w = ((h as f64 * out_ar) as u32 / 2) * 2;
        (w.max(16), h)
    } else {
        let w = track.width;
        let h = ((w as f64 / out_ar) as u32 / 2) * 2;
        (w, h.max(16))
    };

    // Convert subject centers to crop-window LEFT edges (ffmpeg crop x/y are
    // the top-left corner) clamped so the window stays inside the frame. No
    // median smoothing: benchmarked on a continuous panning shot it only adds
    // lag during pans; RDP simplification already rejects single-frame jitter.
    let ts: Vec<f64> = track.keyframes.iter().map(|k| k.t).collect();
    let raw_xs: Vec<f64> = track.keyframes.iter().map(|k| k.cx).collect();
    let raw_ys: Vec<f64> = track.keyframes.iter().map(|k| k.cy).collect();
    let to_edge = |v: &[f64], crop: u32, full: u32| -> Vec<f64> {
        let half = crop as f64 / 2.0;
        v.iter()
            .map(|c| c - half) // subject center -> window left edge
            .map(|e| e.max(0.0).min(full as f64 - crop as f64))
            .collect()
    };
    let xs = to_edge(&raw_xs, crop_w, track.width);
    let ys = to_edge(&raw_ys, crop_h, track.height);
    // Per-shot simplification: smoothing and RDP must not span scene cuts.
    let segs = cut_segments(&ts, &raw_xs, track.width, sample_fps);
    let (ts_x, xs_s, ts_y, ys_s) = assemble_path(&ts, &xs, &ys, &segs);
    let x_expr = ffmpeg_lerp_expr(&ts_x, &xs_s);
    let y_expr = ffmpeg_lerp_expr(&ts_y, &ys_s);

    let ffmpeg = proc::which("ffmpeg")
        .ok_or_else(|| Error::invalid("ffmpeg not found on PATH"))?;
    let vf = format!(
        "crop={crop_w}:{crop_h}:x='{x_expr}':y='{y_expr}',scale={out_w}:{out_h}"
    );
    let args = vec![
        "-y".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-vf".to_string(),
        vf,
        "-c:v".to_string(),
        "libx264".to_string(),
        "-crf".to_string(),
        "20".to_string(),
        "-preset".to_string(),
        "medium".to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        output.to_string_lossy().to_string(),
    ];
    let started = std::time::Instant::now();
    let o = ProcOpts::default().with_timeout(TIMEOUT_SECS);
    let out = proc::run(&ffmpeg, &args, &o)?;
    if !out.success {
        return Err(Error::invalid(format!(
            "reframe render failed: {}",
            crate::secrets::redact(&out.tail_stderr(3))
        )));
    }
    Ok(ReframeReport {
        track,
        output: output.to_string_lossy().to_string(),
        crop_w,
        crop_h,
        out_w,
        out_h,
        seconds: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_expr_interpolates_between_keyframes() {
        let expr = ffmpeg_lerp_expr(&[0.0, 2.0], &[100.0, 200.0]);
        // before t=0 -> 100; at t=1 -> 150; after t=2 -> 200
        assert!(expr.contains("if(lt(t,0.000)"), "{expr}");
        assert!(expr.contains("200.0"), "{expr}");
    }

    #[test]
    fn single_keyframe_is_constant() {
        assert_eq!(ffmpeg_lerp_expr(&[5.0], &[42.0]), "42.0");
        assert_eq!(ffmpeg_lerp_expr(&[], &[42.0]), "0");
    }

    #[test]
    fn smoothing_removes_single_frame_jitter() {
        let v = vec![100.0, 100.0, 500.0, 100.0, 100.0];
        let s = smooth(&v, 3);
        assert_eq!(s[2], 100.0); // median kills the 500 spike
    }

    #[test]
    fn simplify_bounds_expression_depth() {
        // 180 sampled keyframes must reduce to <= 60 points
        let ts: Vec<f64> = (0..180).map(|i| i as f64 * 0.5).collect();
        let cs: Vec<f64> = (0..180).map(|i| (i as f64 * 13.7).sin() * 500.0 + 960.0).collect();
        let idx = simplify_to(&ts, &cs, 60);
        assert!(idx.len() <= 60, "kept {}", idx.len());
        assert_eq!(idx[0], 0);
        assert_eq!(*idx.last().unwrap(), 179);
        // the generated expression must parse within ffmpeg
        let pts_t: Vec<f64> = idx.iter().map(|&i| ts[i]).collect();
        let pts_c: Vec<f64> = idx.iter().map(|&i| cs[i]).collect();
        let expr = ffmpeg_lerp_expr(&pts_t, &pts_c);
        assert!(expr.len() < 20_000);
    }

    #[test]
    fn cuts_split_smoothing_segments() {
        // positions jump 700px between samples 3 and 4: that is a cut, not motion
        let ts: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
        let cs: Vec<f64> = vec![100.0, 110.0, 120.0, 130.0, 830.0, 840.0, 850.0, 860.0];
        let segs = cut_segments(&ts, &cs, 1920, 2.0);
        assert_eq!(segs, vec![(0, 4), (4, 8)]);
    }

    #[test]
    fn assembled_path_jumps_at_cuts_never_slides() {
        let ts: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
        let xs: Vec<f64> = vec![100.0, 110.0, 120.0, 130.0, 830.0, 840.0, 850.0, 860.0];
        let ys: Vec<f64> = vec![50.0; 8];
        let segs = cut_segments(&ts, &xs, 1920, 2.0);
        let (ts_x, xs_s, _, _) = assemble_path(&ts, &xs, &ys, &segs);
        // at the cut instant (1.5s) the OLD window edge still applies...
        let (x_old, _) = eval_at(&ts_x, &xs_s, 1.499);
        assert!((x_old - xs[3]).abs() < 1.0, "just before cut: {x_old}");
        // ...and 1ms later the NEW segment's first edge takes over (no slide)
        let (x_new, _) = eval_at(&ts_x, &xs_s, 1.501);
        assert!((x_new - xs[4]).abs() < 1.0, "just after cut: {x_new}");
    }

    #[test]
    fn assembled_path_never_exceeds_60_points_on_cut_heavy_source() {
        // 20 shots x 12 samples = 240 keyframes: the old per-segment cap alone
        // produced >60 nested if() levels and ffmpeg died (84s trailer bug)
        let ts: Vec<f64> = (0..240).map(|i| i as f64 * 0.25).collect();
        let mut xs = Vec::new();
        for shot in 0..20 {
            for j in 0..12 {
                xs.push((shot % 2) as f64 * 800.0 + 300.0 + j as f64 * 5.0);
            }
        }
        let ys = vec![100.0; 240];
        let segs = cut_segments(&ts, &xs, 1920, 4.0);
        assert!(segs.len() >= 20, "expected many segments, got {}", segs.len());
        let (ts_x, xs_s, ts_y, ys_s) = assemble_path(&ts, &xs, &ys, &segs);
        assert!(ts_x.len() <= 60, "x path kept {}", ts_x.len());
        assert!(ts_y.len() <= 60, "y path kept {}", ts_y.len());
        assert_eq!(ts_x.len(), xs_s.len());
        // every hard-cut jump survives the global decimation
        let jumps = xs_s.windows(2).filter(|w| (w[1] - w[0]).abs() > 400.0).count();
        assert!(jumps >= 15, "only {jumps} jumps survived");
    }

    /// Piecewise-linear evaluation of the assembled path (mirrors ffmpeg's
    /// if(lt(t,...)) chain semantics closely enough for assertions).
    fn eval_at(ts: &[f64], coords: &[f64], t: f64) -> (f64, usize) {
        if ts.is_empty() {
            return (0.0, 0);
        }
        let mut i = 0;
        while i + 1 < ts.len() && ts[i + 1] <= t {
            i += 1;
        }
        if t <= ts[0] {
            return (coords[0], 0);
        }
        if i + 1 >= ts.len() {
            return (*coords.last().unwrap(), ts.len() - 1);
        }
        let (t0, t1) = (ts[i], ts[i + 1]);
        let (c0, c1) = (coords[i], coords[i + 1]);
        if t1 > t0 {
            (c0 + (c1 - c0) * (t - t0) / (t1 - t0), i)
        } else {
            (c0, i) // duplicate/near-duplicate time (cut takeover): jump, no lerp
        }
    }

    #[test]
    fn gaps_also_split_segments() {
        let ts: Vec<f64> = vec![0.0, 0.5, 1.0, 5.0, 5.5];
        let cs: Vec<f64> = vec![100.0, 105.0, 110.0, 115.0, 120.0];
        let segs = cut_segments(&ts, &cs, 1920, 2.0);
        assert_eq!(segs, vec![(0, 3), (3, 5)]);
    }

    #[test]
    fn edges_never_go_negative_or_overrun() {
        // subject near the left edge: crop left edge clamps to 0, not negative
        let to_edge = |c: f64, crop: u32, full: u32| -> f64 {
            (c - crop as f64 / 2.0).max(0.0).min(full as f64 - crop as f64)
        };
        assert_eq!(to_edge(50.0, 606, 1920), 0.0);
        assert_eq!(to_edge(1900.0, 606, 1920), 1314.0);
        assert_eq!(to_edge(960.0, 606, 1920), 657.0);
    }
}
