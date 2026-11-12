use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::{Project, Timeline, TrackKind};
use crate::project::store::ProjectStore;
use crate::util::time::Time;

pub(crate) mod probe;

const SKIP_PREFIX: &str = "skipped: ";
const MIN_BYTES: u64 = 1024;
const BLACK_MIN_SECS: f64 = 0.10;
const BLACK_MAX_SPANS: usize = 3;
const BLACK_MAX_SPAN_SECS: f64 = 1.0;
const AUDIO_SILENT_MEAN_DB: f64 = -50.0;
const AUDIO_CLIP_MAX_DB: f64 = 0.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaReport {
    pub passed: bool,
    pub checks: Vec<QaCheck>,
    pub score: Option<f64>,
}

/// One check plus whether it counts toward the score. A skipped check cannot be
/// measured in this situation, is reported as passed, and is excluded from the
/// denominator so it never inflates or depresses the score.
struct Scored {
    check: QaCheck,
    scored: bool,
}

fn measured(name: &str, passed: bool, detail: String) -> Scored {
    Scored {
        check: QaCheck {
            name: name.to_string(),
            passed,
            detail,
        },
        scored: true,
    }
}

fn skipped(name: &str, detail: impl AsRef<str>) -> Scored {
    Scored {
        check: QaCheck {
            name: name.to_string(),
            passed: true,
            detail: format!("{SKIP_PREFIX}{}", detail.as_ref()),
        },
        scored: false,
    }
}

fn resolve(output: &Path, store: &ProjectStore) -> PathBuf {
    if output.is_absolute() {
        output.to_path_buf()
    } else {
        store.root.join(output)
    }
}

fn timeline_canvas(project: &Project, timeline: Option<&Timeline>) -> (u32, u32) {
    match timeline {
        Some(tl) if tl.width > 0 && tl.height > 0 => (tl.width, tl.height),
        _ => (project.render.width, project.render.height),
    }
}

/// True when the timeline should carry an audio stream in the render: either an
/// audio track holds an unmuted clip, or a clip's imported asset measures audio.
fn expects_audio(project: &Project, timeline: Option<&Timeline>) -> bool {
    let Some(timeline) = timeline else {
        return project
            .assets
            .iter()
            .any(|a| a.info.as_ref().map(|i| i.has_audio).unwrap_or(false));
    };
    for track in &timeline.tracks {
        for clip in &track.clips {
            if clip.audio.muted {
                continue;
            }
            if track.kind == TrackKind::Audio {
                return true;
            }
            if let Some(asset_id) = &clip.asset {
                if let Some(asset) = project.asset(asset_id) {
                    if asset.info.as_ref().map(|i| i.has_audio).unwrap_or(false) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn fail_unmeasurable(reason: &str) -> String {
    format!("could not measure: {reason}")
}

pub fn validate_render(project: &Project, store: &ProjectStore, output: &Path) -> Result<QaReport> {
    // Missing tooling is an environment error, not a verdict about the render.
    let _ = probe::ffprobe()?;
    let _ = probe::ffmpeg()?;

    let path = resolve(output, store);
    let timeline = project.timeline();
    let mut results: Vec<Scored> = Vec::new();

    let metadata = std::fs::metadata(&path).ok().filter(|m| m.is_file());
    let bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
    let exists = metadata.is_some() && bytes > MIN_BYTES;
    let exists_detail = match (&metadata, exists) {
        (None, _) => format!("no file at {}", path.display()),
        (Some(_), false) => format!("measured {bytes} bytes, need more than {MIN_BYTES}"),
        (Some(_), true) => format!("measured {bytes} bytes (> {MIN_BYTES} required)"),
    };
    results.push(measured("file_exists", exists, exists_detail));

    let probe_result = if metadata.is_some() {
        probe::probe(&path)
    } else {
        Err(Error::new(
            ErrorCode::NotFound,
            format!("{} does not exist", path.display()),
        ))
    };

    let info = match &probe_result {
        Ok(info) => Some(info),
        Err(err) => {
            // An unreadable container cannot satisfy any downstream check, and the
            // report still names each one so the caller sees the full row of checks.
            let reason = fail_unmeasurable(&crate::secrets::redact(&err.message));
            for name in [
                "video_stream",
                "resolution",
                "duration",
                "audio_present",
                "black_frames",
                "audio_levels",
            ] {
                results.push(measured(name, false, reason.clone()));
            }
            None
        }
    };

    if let Some(info) = info {
        let video_detail = match (&info.video_codec, info.fps) {
            (Some(codec), Some(fps)) if info.has_video => {
                format!(
                    "measured video stream {codec} {} @ {fps:.3} fps",
                    info.resolution()
                )
            }
            (_, _) => "measured no video stream in the container".to_string(),
        };
        results.push(measured("video_stream", info.has_video, video_detail));

        let (want_w, want_h) = timeline_canvas(project, timeline);
        if want_w == 0 || want_h == 0 {
            results.push(skipped(
                "resolution",
                "project has no canvas size, so there is nothing to compare against",
            ));
        } else {
            let ok = info.width == want_w && info.height == want_h;
            let detail = format!(
                "measured {} against timeline canvas {want_w}x{want_h}",
                info.resolution()
            );
            results.push(measured("resolution", ok, detail));
        }

        let expected = timeline.map(|t| t.duration()).unwrap_or(Time::ZERO);
        if expected.nanos <= 0 {
            results.push(skipped(
                "duration",
                "the timeline is empty (0.000 s), so there is no expected duration to compare",
            ));
        } else {
            let tolerance = (expected.as_secs_f64() * 0.02).max(0.5);
            let delta = (info.duration.as_secs_f64() - expected.as_secs_f64()).abs();
            let ok = delta <= tolerance;
            let detail = format!(
                "measured {:.3} s against timeline {:.3} s (delta {delta:.3} s, tolerance ±{tolerance:.3} s)",
                info.duration.as_secs_f64(),
                expected.as_secs_f64()
            );
            results.push(measured("duration", ok, detail));
        }

        let want_audio = expects_audio(project, timeline);
        let audio_detail = match (want_audio, info.has_audio) {
            (true, true) => {
                let codec = info.audio_codec.clone().unwrap_or_else(|| "unknown".into());
                format!(
                    "measured audio stream {codec} {} Hz {} ch; the timeline expects audio",
                    info.sample_rate, info.channels
                )
            }
            (true, false) => "measured no audio stream, but the timeline has audio".to_string(),
            (false, true) => format!(
                "measured audio stream ({} Hz) although the timeline expects none; extra audio is allowed",
                info.sample_rate
            ),
            (false, false) => "measured no audio stream, and the timeline expects none".to_string(),
        };
        results.push(measured(
            "audio_present",
            !want_audio || info.has_audio,
            audio_detail,
        ));

        if !info.has_video {
            results.push(measured(
                "black_frames",
                false,
                fail_unmeasurable("no video stream to inspect"),
            ));
        } else {
            match probe::black_spans(&path, BLACK_MIN_SECS, 0.10) {
                Ok(spans) => {
                    let longest =
                        spans
                            .iter()
                            .map(|s| s.duration)
                            .fold(0.0f64, |a, b| if b > a { b } else { a });
                    let too_many = spans.len() > BLACK_MAX_SPANS;
                    let too_long = longest > BLACK_MAX_SPAN_SECS;
                    let stamps: Vec<String> = spans
                        .iter()
                        .take(5)
                        .map(|s| format!("{:.2}-{:.2}s", s.start, s.end))
                        .collect();
                    let detail = if spans.is_empty() {
                        format!(
                            "measured 0 black spans ≥{BLACK_MIN_SECS:.2} s (limit: more than {BLACK_MAX_SPANS} spans or any span over {BLACK_MAX_SPAN_SECS:.2} s)"
                        )
                    } else {
                        format!(
                            "measured {} black span(s), longest {longest:.2} s at {} (limit: more than {BLACK_MAX_SPANS} spans or any span over {BLACK_MAX_SPAN_SECS:.2} s)",
                            spans.len(),
                            stamps.join(", ")
                        )
                    };
                    results.push(measured("black_frames", !too_many && !too_long, detail));
                }
                Err(err) => results.push(measured(
                    "black_frames",
                    false,
                    fail_unmeasurable(&crate::secrets::redact(&err.message)),
                )),
            }
        }

        // A single black frame between shots is invisible to sampled frame checks but obvious in
        // playback. Look for black spans shorter than BLACK_MIN_SECS that are not part of a fade
        // at the start or end of the file.
        if info.has_video {
            match probe::black_spans(&path, 0.02, 0.10) {
                Ok(spans) => {
                    let total = info.duration.as_secs_f64();
                    let flashes: Vec<String> = spans
                        .iter()
                        .filter(|s| {
                            s.duration < BLACK_MIN_SECS && s.start > 0.25 && s.end < total - 0.25
                        })
                        .map(|s| format!("{:.3}s", s.start))
                        .collect();
                    let detail = if flashes.is_empty() {
                        "measured 0 isolated black frames inside the programme".to_string()
                    } else {
                        format!("{} isolated black frame(s) at {} - usually a clip ending one frame early", flashes.len(), flashes.join(", "))
                    };
                    results.push(measured("black_flash", flashes.is_empty(), detail));
                }
                Err(err) => results.push(measured(
                    "black_flash",
                    false,
                    fail_unmeasurable(&crate::secrets::redact(&err.message)),
                )),
            }
        }

        if !info.has_audio {
            results.push(skipped(
                "audio_levels",
                "the file carries no audio stream to measure",
            ));
        } else {
            match probe::volume_stats(&path, None, None) {
                Ok(stats) => {
                    let mean = stats.mean_db;
                    let max = stats.max_db;
                    let too_quiet =
                        want_audio && mean.map(|m| m < AUDIO_SILENT_MEAN_DB).unwrap_or(false);
                    let clipping = max.map(|m| m >= AUDIO_CLIP_MAX_DB).unwrap_or(false);
                    let detail = format!(
                        "measured mean {} dB, max {} dB (fail below {AUDIO_SILENT_MEAN_DB:.0} dB mean when audio is expected, or at {AUDIO_CLIP_MAX_DB:.0} dB max)",
                        mean.map(|m| format!("{m:.1}")).unwrap_or_else(|| "n/a".into()),
                        max.map(|m| format!("{m:.1}")).unwrap_or_else(|| "n/a".into())
                    );
                    results.push(measured("audio_levels", !too_quiet && !clipping, detail));
                }
                Err(err) => results.push(measured(
                    "audio_levels",
                    false,
                    fail_unmeasurable(&crate::secrets::redact(&err.message)),
                )),
            }
        }
    }

    let scored: Vec<&QaCheck> = results
        .iter()
        .filter(|r| r.scored)
        .map(|r| &r.check)
        .collect();
    let passed_count = scored.iter().filter(|c| c.passed).count();
    let passed = scored.iter().all(|c| c.passed);
    let score = if scored.is_empty() {
        None
    } else {
        Some(((passed_count as f64 / scored.len() as f64) * 1000.0).round() / 1000.0)
    };
    Ok(QaReport {
        passed,
        checks: results.into_iter().map(|r| r.check).collect(),
        score,
    })
}

pub fn validate_project(project: &Project, store: &ProjectStore) -> QaReport {
    let _ = store;
    let issues = crate::project::ops::validate(project);
    let checks = vec![QaCheck {
        name: "project_integrity".into(),
        passed: issues.is_empty(),
        detail: if issues.is_empty() {
            "no structural problems".into()
        } else {
            issues.join("; ")
        },
    }];
    let passed = checks.iter().all(|c| c.passed);
    QaReport {
        passed,
        checks,
        score: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{Asset, AssetKind, Clip, MediaInfo, Track};

    fn project_with_audio(has_asset_audio: bool) -> (Project, Timeline) {
        let mut project = Project::new("QA");
        let mut timeline = Timeline {
            width: 320,
            height: 240,
            ..Timeline::default()
        };
        let mut video_track = Track {
            kind: TrackKind::Video,
            ..Track::default()
        };
        let clip = Clip {
            asset: Some("asset-1".into()),
            source_in: Time::ZERO,
            source_out: Time::from_secs_f64(2.0),
            ..Clip::default()
        };
        video_track.clips.push(clip);
        timeline.tracks.push(video_track);
        if has_asset_audio {
            let mut audio_track = Track {
                kind: TrackKind::Audio,
                ..Track::default()
            };
            let audio_clip = Clip {
                source_out: Time::from_secs_f64(2.0),
                ..Clip::default()
            };
            audio_track.clips.push(audio_clip);
            timeline.tracks.push(audio_track);
        }
        project.assets.push(Asset {
            id: "asset-1".into(),
            path: "assets/a.mp4".into(),
            kind: AssetKind::Video,
            bytes: 10,
            imported_at: String::new(),
            sha256: "x".into(),
            info: Some(MediaInfo {
                has_video: true,
                has_audio: false,
                ..MediaInfo::default()
            }),
            missing: false,
            provenance: None,
        });
        let id = timeline.id.clone();
        project.timelines.push(timeline.clone());
        project.active_timeline = Some(id);
        (project, timeline)
    }

    #[test]
    fn audio_expectation_follows_timeline() {
        let (project, timeline) = project_with_audio(false);
        assert!(!expects_audio(&project, Some(&timeline)));
        let (project, timeline) = project_with_audio(true);
        assert!(expects_audio(&project, Some(&timeline)));
    }

    #[test]
    fn canvas_falls_back_to_render_settings() {
        let mut project = Project::new("QA");
        project.render.width = 1280;
        project.render.height = 720;
        assert_eq!(timeline_canvas(&project, None), (1280, 720));
    }

    #[test]
    fn skipped_checks_are_marked_and_unscored() {
        let entry = skipped("duration", "empty timeline");
        assert!(entry.check.passed);
        assert!(!entry.scored);
        assert!(entry.check.detail.starts_with(SKIP_PREFIX));
    }

    #[test]
    fn missing_file_never_passes() {
        let dir = tempfile::tempdir().unwrap();
        let (store, project) = ProjectStore::create(dir.path(), "QA Missing").unwrap();
        let missing = dir.path().join("nope.mp4");
        match validate_render(&project, &store, &missing) {
            Ok(report) => {
                assert!(!report.passed);
                let first = report
                    .checks
                    .iter()
                    .find(|c| c.name == "file_exists")
                    .unwrap();
                assert!(!first.passed);
            }
            // No ffmpeg/ffprobe on this machine: the QA gate refuses to guess.
            Err(err) => assert_eq!(err.code, ErrorCode::DependencyMissing),
        }
    }
}
