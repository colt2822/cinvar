//! Sidechain ducking for the demucs clean mix (src/audio/demucs.rs).
//!
//! Every number here is measured from real ffmpeg output: fixtures are synthesised with lavfi,
//! mixed through the crate, and measured with volumedetect over a named window. Nothing is
//! asserted from the filter text alone except in the configuration test.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::audio::demucs::{
    duck_filter, duck_mix, DuckOptions, DUCK_ATTACK_MS, DUCK_RELEASE_MS,
};

fn ffmpeg(args: &[String]) {
    let out = Command::new("ffmpeg")
        .args(args)
        .output()
        .expect("ffmpeg must be installed");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ffprobe(args: &[String]) -> String {
    let out = Command::new("ffprobe")
        .args(args)
        .output()
        .expect("ffprobe must be installed");
    assert!(out.status.success(), "ffprobe failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Synthesise a mono 48 kHz WAV from a lavfi expression, e.g. "0.7*sin(2*PI*220*t)".
fn fixture(path: &Path, expr: &str, seconds: f64) {
    ffmpeg(&[
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("aevalsrc='{expr}':s=48000:d={seconds}"),
        "-c:a".into(),
        "pcm_s16le".into(),
        path.to_string_lossy().to_string(),
    ]);
}

fn duration(path: &Path) -> f64 {
    ffprobe(&[
        "-v".into(),
        "error".into(),
        "-show_entries".into(),
        "format=duration".into(),
        "-of".into(),
        "csv=p=0".into(),
        path.to_string_lossy().to_string(),
    ])
    .parse()
    .expect("a measurable duration")
}

/// (mean, max) level in dBFS over [start, start+len) of a file, from ffmpeg volumedetect.
fn levels(path: &Path, start: f64, len: f64) -> (f64, f64) {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-ss",
            &format!("{start:.3}"),
            "-i",
        ])
        .arg(path)
        .args(["-t", &format!("{len:.3}"), "-vn", "-af", "volumedetect", "-f", "null", "-"])
        .output()
        .expect("ffmpeg must be installed");
    assert!(out.status.success(), "volumedetect failed");
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let field = |marker: &str| -> f64 {
        text.lines()
            .find(|line| line.contains(marker))
            .and_then(|line| {
                let rest = &line[line.find(marker)? + marker.len()..];
                rest.split_whitespace()
                    .next()
                    .and_then(|token| token.parse::<f64>().ok())
            })
            .unwrap_or_else(|| panic!("volumedetect printed no {marker}: {text}"))
    };
    (field("mean_volume:"), field("max_volume:"))
}

/// A music bed at a constant 0.7 and a speech stem that only sounds between 1.5 s and 3.5 s.
fn bed_and_speech(dir: &Path) -> (PathBuf, PathBuf) {
    let music = dir.join("music.wav");
    let speech = dir.join("speech.wav");
    fixture(&music, "0.7*sin(2*PI*220*t)", 8.0);
    fixture(&speech, "0.6*sin(2*PI*700*t)*between(t,1.5,3.5)", 8.0);
    (music, speech)
}

#[test]
fn the_documented_duck_timing_is_what_the_filter_uses() {
    assert_eq!(DUCK_ATTACK_MS, 20);
    assert_eq!(DUCK_RELEASE_MS, 300);
    let filter = duck_filter(&DuckOptions::default(), Some(8.0));
    assert!(filter.contains("apad=whole_dur=8.500"), "both stems are padded past the mix: {filter}");
    assert!(filter.contains("atrim=end=8.000"), "and the sum is trimmed back: {filter}");
    assert!(filter.contains("sidechaincompress"), "{filter}");
    assert!(filter.contains("attack=20"), "{filter}");
    assert!(filter.contains("release=300"), "{filter}");
    assert!(filter.contains("amix=inputs=2:normalize=0"), "the speech is not halved: {filter}");
    assert!(filter.contains("alimiter"), "the sum is capped below full scale: {filter}");
    let untimed = duck_filter(&DuckOptions::default(), None);
    assert!(!untimed.contains("apad=") && !untimed.contains("atrim="), "{untimed}");
    let bypass = duck_filter(&DuckOptions::default().without_ducking(), None);
    assert!(bypass.contains("ratio=1.000"), "bypass is transparent: {bypass}");
}

#[test]
fn speech_ducks_the_music_bed_and_it_recovers_in_the_pause() {
    let dir = tempfile::tempdir().unwrap();
    let (music, speech) = bed_and_speech(dir.path());
    let ducked = dir.path().join("ducked.wav");
    let control = dir.path().join("control.wav");
    let opts = DuckOptions::default();
    duck_mix(&speech, &music, &ducked, &opts).unwrap();
    duck_mix(&speech, &music, &control, &opts.without_ducking()).unwrap();

    // Before the speech the bed is untouched.
    let (before_ducked, _) = levels(&ducked, 0.5, 0.8);
    let (before_control, _) = levels(&control, 0.5, 0.8);
    assert!(
        (before_ducked - before_control).abs() < 0.3,
        "the bed must not be touched before any speech: ducked {before_ducked:.1} dB, control {before_control:.1} dB"
    );

    // While speech plays the bed is pushed down: measured just after the speech ends (where
    // the vocals no longer contribute and the 300 ms release is still holding the bed down).
    let (tail_ducked, _) = levels(&ducked, 3.55, 0.15);
    let (tail_control, _) = levels(&control, 3.55, 0.15);
    let depth = tail_control - tail_ducked;
    assert!(
        depth >= 4.0,
        "the bed must be ducked under speech: measured {depth:.1} dB (ducked {tail_ducked:.1} dB, control {tail_control:.1} dB)"
    );

    // The release lets the bed come back inside the pause after the speech.
    let (recovered, _) = levels(&ducked, 4.6, 1.4);
    let recovery = recovered - tail_ducked;
    assert!(
        recovery >= 4.0,
        "the bed must recover in the pause: measured {recovery:.1} dB from {tail_ducked:.1} dB to {recovered:.1} dB"
    );
    assert!(
        (recovered - before_ducked).abs() < 0.6,
        "recovered level {recovered:.1} dB must match the pre-speech level {before_ducked:.1} dB"
    );
}

#[test]
fn the_speech_itself_is_not_attenuated_by_the_ducker() {
    let dir = tempfile::tempdir().unwrap();
    let (music, speech) = bed_and_speech(dir.path());
    // Reference: the same speech stem through the same mono-to-stereo 48 kHz upmix the mix
    // applies (that upmix keeps the energy but normalises each channel by ~3 dB, so it must
    // be present on both sides of the comparison).
    let speech_only = dir.path().join("speech_only.wav");
    ffmpeg(&[
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        speech.to_string_lossy().to_string(),
        "-af".into(),
        "aformat=sample_rates=48000:channel_layouts=stereo".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        speech_only.to_string_lossy().to_string(),
    ]);
    let mix = dir.path().join("mix.wav");
    duck_mix(&speech, &music, &mix, &DuckOptions::default()).unwrap();

    let (speech_level, _) = levels(&speech_only, 2.0, 1.0);
    let (mix_level, _) = levels(&mix, 2.0, 1.0);
    assert!(
        mix_level >= speech_level - 1.0,
        "the words must come through at their own level: source {speech_level:.1} dB, mix {mix_level:.1} dB"
    );
}

#[test]
fn the_mix_does_not_clip_where_a_plain_sum_would() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music.wav");
    let speech = dir.path().join("speech.wav");
    fixture(&music, "0.9*sin(2*PI*220*t)", 4.0);
    fixture(&speech, "0.9*sin(2*PI*700*t)*between(t,0.5,2.5)", 4.0);

    let ducked = dir.path().join("ducked.wav");
    duck_mix(&speech, &music, &ducked, &DuckOptions::default()).unwrap();
    let (_, ducked_max) = levels(&ducked, 0.0, 4.0);
    assert!(
        ducked_max < -0.1,
        "the ducked mix must stay below full scale: max {ducked_max:.2} dBFS"
    );

    // The fixture really would clip without the ducking chain, so the assertion above
    // measures the chain and not the fixtures.
    let naive = dir.path().join("naive.wav");
    ffmpeg(&[
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        speech.to_string_lossy().to_string(),
        "-i".into(),
        music.to_string_lossy().to_string(),
        "-filter_complex".into(),
        "[0:a][1:a]amix=inputs=2:normalize=0:duration=longest[mix]".into(),
        "-map".into(),
        "[mix]".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        naive.to_string_lossy().to_string(),
    ]);
    let (_, naive_max) = levels(&naive, 0.0, 4.0);
    assert!(
        naive_max >= -0.05,
        "the fixture must reach full scale when summed plainly, measured {naive_max:.2} dBFS"
    );

    // A deliberately hot bed is still capped by the limiter.
    let hot = dir.path().join("hot.wav");
    let opts = DuckOptions {
        music_gain: 2.0,
        ..DuckOptions::default()
    };
    duck_mix(&speech, &music, &hot, &opts).unwrap();
    let (_, hot_max) = levels(&hot, 0.0, 4.0);
    assert!(
        hot_max < -0.1,
        "a hot bed must be limited below full scale: max {hot_max:.2} dBFS"
    );
}

#[test]
fn the_mix_runs_for_the_longer_input_and_keeps_the_speech_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music.wav");
    let speech = dir.path().join("speech.wav");
    fixture(&music, "0.7*sin(2*PI*220*t)", 8.0);
    fixture(&speech, "0.6*sin(2*PI*700*t)*between(t,1.5,3.5)", 6.0);
    let mix = dir.path().join("mix.wav");
    duck_mix(&speech, &music, &mix, &DuckOptions::default()).unwrap();

    let len = duration(&mix);
    assert!(
        (len - 8.0).abs() < 0.15,
        "the mix must run for the longer input (8.0 s), measured {len:.3} s"
    );

    // Synchronisation: the speech onset at 1.5 s must show up at 1.5 s, not earlier and not
    // later. Measured on the un-ducked mix, where the bed is a constant reference: ducking
    // lowers the bed during speech, which would mask the onset in the summed level.
    let control = dir.path().join("control.wav");
    duck_mix(&speech, &music, &control, &DuckOptions::default().without_ducking()).unwrap();
    let (just_before, _) = levels(&control, 1.40, 0.05);
    let (just_after, _) = levels(&control, 1.55, 0.05);
    assert!(
        just_after - just_before >= 1.5,
        "the speech onset must land inside 1.40-1.60 s: {just_before:.1} dB before, {just_after:.1} dB after"
    );
    let (well_before, _) = levels(&control, 1.20, 0.20);
    assert!(
        (well_before - just_before).abs() < 0.5,
        "nothing may happen before the onset: {well_before:.1} dB at 1.2 s vs {just_before:.1} dB at 1.4 s"
    );
    let (later, _) = levels(&control, 2.60, 0.40);
    assert!(
        (later - just_after).abs() < 2.0,
        "the speech level must stay steady 1 s after the onset: {just_after:.1} dB vs {later:.1} dB"
    );
}
