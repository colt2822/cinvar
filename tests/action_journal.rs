//! The action journal records real tool calls (opt-in, redacted, local) and replays validate.

use cinvar::actions::{validate_calls, ActionRecord, ToolCall};
use cinvar::project::store::ProjectStore;

fn run(record: bool) -> (tempfile::TempDir, ProjectStore) {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project) = ProjectStore::create(dir.path(), "journal").unwrap();
    let registry = cinvar::tools::build_default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let mut config = cinvar::config::Config::default();
    config.actions.record = record;
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    registry
        .call(
            "cinvar.timeline.create",
            &mut ctx,
            serde_json::json!({"name": "Main", "width": 1280, "height": 720}),
        )
        .expect("create timeline");
    // a failing call is journaled too (ok=false), with the path reduced to a file name
    let _ = registry.call(
        "cinvar.media.import",
        &mut ctx,
        serde_json::json!({"path": "/nonexistent/secret-dir/a.mp4"}),
    );
    (dir, store)
}

#[test]
fn journal_is_off_by_default() {
    let (_d, store) = run(false);
    assert!(!store.actions_dir().exists());
}

#[test]
fn journal_records_successes_and_failures_with_redaction() {
    let (_d, store) = run(true);
    let text = std::fs::read_to_string(store.actions_dir().join("actions.jsonl")).unwrap();
    let recs: Vec<ActionRecord> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].tool, "cinvar.timeline.create");
    assert!(recs[0].ok && recs[0].revision_after > recs[0].revision_before);
    assert_eq!(recs[0].schema, "cinvar.action/1");
    assert_eq!(recs[1].tool, "cinvar.media.import");
    assert!(!recs[1].ok && recs[1].error_code.is_some());
    assert!(
        !text.contains("secret-dir"),
        "directory names must not be journaled"
    );
    assert!(text.contains("<path>/a.mp4"));
    // every journaled call is itself a valid action against the registry
    let calls: Vec<ToolCall> = recs
        .iter()
        .map(|r| ToolCall {
            tool: r.tool.clone(),
            args: r.args.clone(),
        })
        .collect();
    let reg = cinvar::tools::build_default();
    assert!(validate_calls(&reg, &calls).ok);
}
