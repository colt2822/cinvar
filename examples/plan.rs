// Prints the ffmpeg filtergraph CINVAR would use for a project: `cargo run --example plan -- <project-dir>`
fn main() {
    let dir = std::env::args().nth(1).expect("project dir");
    let store = cinvar::project::store::ProjectStore::open(std::path::Path::new(&dir)).unwrap();
    let project = store.load().unwrap();
    let req = cinvar::render::RenderRequest::for_output("/tmp/plan-out.mp4");
    let plan = cinvar::render::plan_render(&project, &store, &req).unwrap();
    println!("{}", plan.filtergraph.replace(';', ";\n"));
    for w in plan.warnings {
        eprintln!("warning: {w}");
    }
}
