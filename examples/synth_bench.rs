//! Writes the synthetic editing benchmark: tasks.jsonl (instruction, reference calls, reference
//! hash) and dataset.jsonl (cinvar.dataset/1 examples, provenance=synthetic). Needs ffmpeg.
//!   cargo run --example synth_bench -- OUT_DIR [N=64] [SEED=1]
use std::io::Write;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = std::path::PathBuf::from(a.get(1).expect("usage: synth_bench OUT_DIR [N] [SEED]"));
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(64);
    let seed: u64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    std::fs::create_dir_all(&out).unwrap();
    let media_dir = out.join("media");
    std::fs::create_dir_all(&media_dir).unwrap();
    let media = cinvar::actions::bench::generate_media(&media_dir).expect("ffmpeg");
    let reg = cinvar::tools::build_default();
    let tasks = cinvar::actions::bench::generate(&reg, &media, seed, n);
    let mut t = std::fs::File::create(out.join("tasks.jsonl")).unwrap();
    let mut d = std::fs::File::create(out.join("dataset.jsonl")).unwrap();
    for task in &tasks {
        writeln!(t, "{}", serde_json::to_string(task).unwrap()).unwrap();
        let ex = cinvar::actions::bench::to_dataset_example(task);
        assert!(ex.validate(&reg).ok);
        writeln!(d, "{}", serde_json::to_string(&ex).unwrap()).unwrap();
    }
    println!("{} tasks -> {}", tasks.len(), out.display());
}
