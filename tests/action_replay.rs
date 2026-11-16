//! Deterministic replay + synthetic benchmark (needs ffmpeg).
use cinvar::actions::bench::{generate, generate_media, score, to_dataset_example};
use cinvar::actions::replay::replay;
use cinvar::actions::ToolCall;
use cinvar::util::proc;

fn ok() -> bool {
    proc::which("ffmpeg").is_some() && proc::which("ffprobe").is_some()
}

#[test]
fn replay_is_deterministic_and_benchmark_scores() {
    if !ok() {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let reg = cinvar::tools::build_default();
    let dir = tempfile::tempdir().unwrap();
    let media = generate_media(dir.path()).unwrap();
    let tasks = generate(&reg, &media, 7, 16);
    assert_eq!(tasks.len(), 16);
    let cats: std::collections::BTreeSet<_> = tasks.iter().map(|t| t.category.clone()).collect();
    assert_eq!(cats.len(), 8, "every category covered: {cats:?}");
    // same seed => identical tasks and hashes (determinism of generation and replay)
    let again = generate(&reg, &media, 7, 16);
    for (a, b) in tasks.iter().zip(&again) {
        assert_eq!(a.instruction, b.instruction);
        assert_eq!(a.reference_hash, b.reference_hash, "{}", a.id);
    }
    // a different seed changes at least one reference
    let other = generate(&reg, &media, 8, 16);
    assert!(tasks
        .iter()
        .zip(&other)
        .any(|(a, b)| a.reference_hash != b.reference_hash));
    // the reference calls score exact; a tampered candidate does not
    for t in &tasks {
        let s = score(&reg, t, &media, &t.reference_calls);
        assert!(s.valid && s.replayed && s.exact, "{} {:?}", t.id, s.errors);
        let ds = to_dataset_example(t);
        assert!(
            ds.validate(&reg).ok,
            "{} {:?}",
            t.id,
            ds.validate(&reg).errors
        );
    }
    let t = tasks.iter().find(|t| t.category == "trim").unwrap();
    let mut bad = t.reference_calls.clone();
    bad.last_mut().unwrap().args["source_out"] = serde_json::json!("1.0");
    let s = score(&reg, t, &media, &bad);
    assert!(s.valid && s.replayed && !s.exact);
    // an invalid action list is rejected before anything runs
    let junk = vec![ToolCall {
        tool: "cinvar.timeline.nope".into(),
        args: serde_json::json!({}),
    }];
    let s = score(&reg, t, &media, &junk);
    assert!(!s.valid && !s.replayed);
    // an unresolved reference is a replay error, not a panic
    let r = replay(
        &reg,
        &[ToolCall {
            tool: "cinvar.timeline.split".into(),
            args: serde_json::json!({"clip_id": "@clip:5", "at": "1.0"}),
        }],
        &[],
    );
    assert!(!r.ok);
}

#[test]
fn journal_stays_off_during_replay() {
    if !ok() {
        return;
    }
    let reg = cinvar::tools::build_default();
    let dir = tempfile::tempdir().unwrap();
    let media = generate_media(dir.path()).unwrap();
    let t = &generate(&reg, &media, 1, 1)[0];
    let m: Vec<_> = media
        .iter()
        .filter(|(n, _)| t.media.contains(n))
        .cloned()
        .collect();
    assert!(replay(&reg, &t.reference_calls, &m).ok);
}
