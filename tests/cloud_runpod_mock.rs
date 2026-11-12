//! RunPod end-to-end against a LOCAL MOCK of the RunPod REST API. The pod's worker is the real
//! CINVAR worker (real FFmpeg render, real checksums, real chunked transfer); only the
//! provider API and the network hop are simulated. This is a MOCK test, not a live cloud test.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cinvar::cloud::manifest;
use cinvar::cloud::orchestrator::{self, CloudOptions, CloudRun};
use cinvar::cloud::runpod::{Pod, RunpodClient};
use cinvar::cloud::worker::{self, WorkerConfig, WorkerHandle};
use cinvar::jobstore::{JobState, JobStore};
use cinvar::project::model::{Project, TrackKind};
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::render::RenderRequest;
use cinvar::util::time::{Rational, Time};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server, StatusCode};

const KEY: &str = "rpa_TESTKEY_0123456789abcdef";

#[derive(Default)]
struct MockState {
    pods: HashMap<String, (String, f64, bool)>, // id -> (name, cost, terminated)
    workers: HashMap<String, WorkerHandle>,
    worker_ports: HashMap<String, String>,
    creates: usize,
    deletes: Vec<String>,
    worker_dir: std::path::PathBuf,
}

struct Mock {
    url: String,
    state: Arc<Mutex<MockState>>,
    cost: Arc<Mutex<f64>>,
    create_500_after_creating: Arc<AtomicBool>,
    fail_delete: Arc<AtomicBool>,
    create_mode: Arc<std::sync::atomic::AtomicU8>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mock {
    fn start(worker_dir: std::path::PathBuf) -> Mock {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let state = Arc::new(Mutex::new(MockState {
            worker_dir,
            ..Default::default()
        }));
        let cost = Arc::new(Mutex::new(0.69));
        let flaky = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let fail_delete = Arc::new(AtomicBool::new(false));
        let create_mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let (st, c, fl, sp) = (state.clone(), cost.clone(), flaky.clone(), stop.clone());
        let (fd, cm) = (fail_delete.clone(), create_mode.clone());
        let thread = std::thread::spawn(move || {
            while !sp.load(Ordering::SeqCst) {
                let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let auth_ok = req.headers().iter().any(|h| {
                    h.field.equiv("Authorization") && h.value.as_str() == format!("Bearer {KEY}")
                });
                let reply = |req: tiny_http::Request, code: u16, v: Value| {
                    let mut r =
                        Response::from_string(v.to_string()).with_status_code(StatusCode(code));
                    r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    let _ = req.respond(r);
                };
                if !auth_ok {
                    reply(req, 401, json!({"error": "unauthorized"}));
                    continue;
                }
                let url = req.url().to_string();
                let parts: Vec<&str> = url.trim_matches('/').split('/').collect();
                let pod_json = |s: &MockState, id: &str| -> Value {
                    let (name, cost, term) = s.pods.get(id).cloned().unwrap();
                    json!({"id": id, "name": name, "desiredStatus": if term {"TERMINATED"} else {"RUNNING"}, "costPerHr": cost,
                           "publicIp": "127.0.0.1", "portMappings": {"8000": s.worker_ports.get(id)}})
                };
                match (req.method().clone(), parts.as_slice()) {
                    (Method::Get, ["pods"]) => {
                        let s = st.lock().unwrap();
                        let list: Vec<Value> = s.pods.keys().map(|id| pod_json(&s, id)).collect();
                        reply(req, 200, json!(list));
                    }
                    (Method::Post, ["pods"]) if cm.load(Ordering::SeqCst) == 1 => {
                        reply(
                            req,
                            400,
                            json!({"error": "There are no instances currently available"}),
                        );
                    }
                    (Method::Post, ["pods"]) if cm.load(Ordering::SeqCst) == 2 => {
                        // Never answers within the client's timeout; no pod is created.
                        std::thread::sleep(Duration::from_millis(2500));
                        reply(req, 504, json!({"error": "gateway timeout"}));
                    }
                    (Method::Delete, ["pods", _]) if fd.load(Ordering::SeqCst) => {
                        reply(req, 500, json!({"error": "terminate failed"}));
                    }
                    (Method::Post, ["pods"]) => {
                        let mut body = String::new();
                        req.as_reader().read_to_string(&mut body).unwrap();
                        let spec: Value = serde_json::from_str(&body).unwrap();
                        let mut s = st.lock().unwrap();
                        s.creates += 1;
                        let id = format!("pod{:04}", s.creates);
                        let token = spec["env"]["CINVAR_WORKER_TOKEN"]
                            .as_str()
                            .unwrap()
                            .to_string();
                        let max_rt = spec["env"]["CINVAR_WORKER_MAX_RUNTIME_SECS"]
                            .as_u64()
                            .unwrap();
                        let dir = s.worker_dir.join(&id);
                        let w = worker::start(WorkerConfig {
                            bind: "127.0.0.1:0".into(),
                            root: dir,
                            token,
                            max_runtime: Some(Duration::from_secs(max_rt)),
                        })
                        .unwrap();
                        s.worker_ports
                            .insert(id.clone(), w.addr.rsplit(':').next().unwrap().to_string());
                        s.workers.insert(id.clone(), w);
                        s.pods.insert(
                            id.clone(),
                            (
                                spec["name"].as_str().unwrap().to_string(),
                                *c.lock().unwrap(),
                                false,
                            ),
                        );
                        assert_eq!(spec["ports"][0], "8000/http");
                        assert!(!spec["gpuTypeIds"].as_array().unwrap().is_empty());
                        if fl.load(Ordering::SeqCst) {
                            reply(req, 500, json!({"error": "gateway hiccup after create"}));
                        } else {
                            let v = pod_json(&s, &id);
                            reply(req, 201, v);
                        }
                    }
                    (Method::Get, ["pods", id]) => {
                        let s = st.lock().unwrap();
                        if s.pods.contains_key(*id) {
                            let v = pod_json(&s, id);
                            reply(req, 200, v)
                        } else {
                            reply(req, 404, json!({"error": "no pod"}))
                        }
                    }
                    (Method::Delete, ["pods", id]) => {
                        let mut s = st.lock().unwrap();
                        if let Some(p) = s.pods.get_mut(*id) {
                            p.2 = true;
                            s.deletes.push(id.to_string());
                            if let Some(w) = s.workers.remove(*id) {
                                w.stop();
                            }
                            reply(req, 200, Value::Null);
                        } else {
                            reply(req, 404, json!({"error": "no pod"}));
                        }
                    }
                    _ => reply(req, 404, json!({"error": "nope"})),
                }
            }
        });
        Mock {
            url,
            state,
            cost,
            create_500_after_creating: flaky,
            fail_delete,
            create_mode,
            stop,
            thread: Some(thread),
        }
    }
    fn client(&self) -> RunpodClient {
        RunpodClient::new(&self.url, KEY)
    }
    fn opts(&self, ceiling: f64) -> CloudOptions {
        let mut o = CloudOptions::production(ceiling, 300, "ghcr.io/example/cinvar-worker:test");
        o.poll_interval = Duration::from_millis(100);
        o.startup_timeout = Duration::from_secs(20);
        o.acknowledge_billing_risk = true;
        o.worker_url = Arc::new(|p: &Pod| {
            let port = p.port_mappings.as_ref()?.get("8000")?.as_str()?.to_string();
            Some(format!("http://{}:{}", p.public_ip.clone()?, port))
        });
        o
    }
    fn creates(&self) -> usize {
        self.state.lock().unwrap().creates
    }
    fn deleted(&self) -> Vec<String> {
        self.state.lock().unwrap().deletes.clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let mut s = self.state.lock().unwrap();
        for (_, w) in s.workers.drain() {
            w.stop();
        }
    }
}

struct Fx {
    dir: tempfile::TempDir,
    store: ProjectStore,
    project: Project,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.mp4");
    assert!(std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=300:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest"
        ])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (store, mut project) = ProjectStore::create(dir.path(), "cloud").unwrap();
    let asset = store.import_asset(&mut project, &src, "t").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", 320, 180, Rational::new(24, 1)).unwrap();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let t = project.timeline_by_id(&tl).unwrap().tracks_of(kind)[0]
            .id
            .clone();
        ops::insert_clip(
            &mut project,
            Some(&tl),
            Some(&t),
            ClipRequest {
                asset_id: &asset.id,
                source_in: None,
                source_out: Some(Time::from_secs_f64(2.0)),
                at: Some(Time::ZERO),
                name: None,
            },
        )
        .unwrap();
    }
    store.save(&mut project).unwrap();
    Fx {
        dir,
        store,
        project,
    }
}

fn built(
    fx: &Fx,
) -> (
    manifest::JobManifest,
    Vec<(String, std::path::PathBuf)>,
    std::path::PathBuf,
) {
    let out = fx.store.renders_dir().join("cloud.mp4");
    let (m, files) =
        manifest::build(&fx.project, &fx.store, &RenderRequest::for_output(&out)).unwrap();
    (m, files, out)
}

fn run_with(
    mock: &Mock,
    fx: &Fx,
    ceiling: f64,
    confirm: Option<&str>,
    cancel: &dyn Fn() -> bool,
) -> cinvar::error::Result<orchestrator::CloudOutcome> {
    let (m, files, out) = built(fx);
    let opts = mock.opts(ceiling);
    let q = orchestrator::quote(&m, &opts)?;
    let jobs = JobStore::open(&fx.store.jobs_dir())?;
    let client = mock.client();
    let confirm = confirm.unwrap_or(&q.id).to_string();
    orchestrator::run(&CloudRun {
        client: &client,
        jobs: &jobs,
        manifest: &m,
        files: &files,
        opts: &opts,
        output: &out,
        confirm_quote_id: &confirm,
        cancelled: cancel,
        progress: &|_, _| {},
    })
}

#[test]
fn mock_end_to_end_local_project_to_verified_output_and_pod_released() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let outcome = run_with(&mock, &fx, 1.0, None, &|| false).expect("cloud render");
    assert!(!outcome.duplicate);
    assert_eq!(outcome.uploaded_assets, 1);
    assert!(outcome.pod_released);
    assert_eq!(mock.deleted().len(), 1, "the pod must be terminated");
    let out = outcome.output.unwrap();
    let report = cinvar::render::verify::deep_verify(&out, Some((320, 180)), Some(48)).unwrap();
    assert!(report.passed, "{:?}", report.issues);
    // The job record is durable and shows success.
    let jobs = JobStore::open(&fx.store.jobs_dir())
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(jobs[0].state, JobState::Succeeded);
    assert!(jobs[0].checkpoints.iter().any(|c| c == "pod released"));
    assert!(
        !serde_json::to_string(&jobs[0]).unwrap().contains(KEY),
        "API key must never be persisted"
    );
}

#[test]
fn second_identical_confirmed_run_starts_no_second_pod() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    run_with(&mock, &fx, 1.0, None, &|| false).unwrap();
    let again = run_with(&mock, &fx, 1.0, None, &|| false).unwrap();
    assert!(again.duplicate);
    assert_eq!(mock.creates(), 1);
}

#[test]
fn unconfirmed_or_stale_quote_creates_nothing() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let e = run_with(&mock, &fx, 1.0, Some("q_wrong"), &|| false).unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Budget);
    let e = run_with(&mock, &fx, 0.0, None, &|| false).unwrap_err();
    assert_eq!(
        e.code,
        cinvar::error::ErrorCode::Budget,
        "zero ceiling is refused"
    );
    assert_eq!(mock.creates(), 0);
}

#[test]
fn bad_credentials_stop_before_any_pod() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let (m, files, out) = built(&fx);
    let opts = mock.opts(1.0);
    let q = orchestrator::quote(&m, &opts).unwrap();
    let jobs = JobStore::open(&fx.store.jobs_dir()).unwrap();
    let bad = RunpodClient::new(&mock.url, "rpa_WRONG_KEY_0000000000");
    let e = orchestrator::run(&CloudRun {
        client: &bad,
        jobs: &jobs,
        manifest: &m,
        files: &files,
        opts: &opts,
        output: &out,
        confirm_quote_id: &q.id,
        cancelled: &|| false,
        progress: &|_, _| {},
    })
    .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::PermissionDenied);
    assert!(
        !e.message.contains("WRONG_KEY"),
        "key must not leak into errors: {}",
        e.message
    );
    assert_eq!(mock.creates(), 0);
}

#[test]
fn pod_priced_above_the_ceiling_is_terminated_immediately() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    *mock.cost.lock().unwrap() = 2.50;
    let e = run_with(&mock, &fx, 1.0, None, &|| false).unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Budget, "{e}");
    assert_eq!(mock.creates(), 1);
    assert_eq!(
        mock.deleted().len(),
        1,
        "over-ceiling pod must be terminated"
    );
    let job = &JobStore::open(&fx.store.jobs_dir())
        .unwrap()
        .list()
        .unwrap()[0];
    assert_eq!(job.state, JobState::Failed);
}

#[test]
fn cancellation_mid_run_terminates_the_pod() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let polls = AtomicUsize::new(0);
    // Cancel once the upload/submit phase is past: after the 3rd check.
    let cancel = || polls.fetch_add(1, Ordering::SeqCst) >= 3;
    let e = run_with(&mock, &fx, 1.0, None, &cancel).unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Cancelled, "{e}");
    assert_eq!(mock.deleted().len(), 1);
    let job = &JobStore::open(&fx.store.jobs_dir())
        .unwrap()
        .list()
        .unwrap()[0];
    assert_eq!(job.state, JobState::Cancelled);
}

#[test]
fn create_that_errors_after_succeeding_is_reconciled_by_name_not_duplicated() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    mock.create_500_after_creating.store(true, Ordering::SeqCst);
    let outcome = run_with(&mock, &fx, 1.0, None, &|| false).expect("reconciled run");
    assert_eq!(
        mock.creates(),
        1,
        "a 500 after a successful create must not create a second pod"
    );
    assert!(outcome.pod_released);
}

#[test]
fn orphan_sweep_only_touches_this_projects_dead_jobs() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let client = mock.client();
    // A foreign cinvar pod (no record in this project) must be left alone.
    let spec = |name: &str| cinvar::cloud::runpod::CreatePod {
        name: name.into(),
        image_name: "x".into(),
        gpu_type_ids: vec!["NVIDIA L4".into()],
        gpu_count: 1,
        container_disk_in_gb: 10,
        volume_in_gb: 0,
        ports: vec!["8000/http".into()],
        env: serde_json::from_value(
            json!({"CINVAR_WORKER_TOKEN": "t".repeat(20), "CINVAR_WORKER_MAX_RUNTIME_SECS": 60}),
        )
        .unwrap(),
        cloud_type: "SECURE".into(),
        interruptible: false,
    };
    let foreign = client.create_pod(&spec("cinvar-job_foreign")).unwrap();
    // This project's job whose owner process died while its pod was running.
    let jobs = JobStore::open(&fx.store.jobs_dir()).unwrap();
    let rec = jobs
        .submit("cloud_render", json!({"k": 1}))
        .unwrap()
        .record()
        .clone();
    jobs.start(&rec.id).unwrap();
    jobs.update(&rec.id, |r| r.owner_pid = Some(4_000_000_000))
        .unwrap();
    let mine = client
        .create_pod(&spec(&format!("cinvar-{}", rec.id)))
        .unwrap();
    jobs.recover().unwrap();
    let released =
        orchestrator::cleanup_orphans(&client, &jobs, Duration::from_millis(10)).unwrap();
    assert_eq!(released, vec![mine.id.clone()]);
    assert!(!mock.deleted().contains(&foreign.id));
}

#[test]
fn worker_rejects_wrong_token_and_hostile_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let w = worker::start(WorkerConfig {
        bind: "127.0.0.1:0".into(),
        root: dir.path().join("w"),
        token: "t".repeat(24),
        max_runtime: None,
    })
    .unwrap();
    let url = w.url();
    let r = ureq::get(&format!("{url}/jobs/abc")).call();
    assert!(matches!(r, Err(ureq::Error::Status(401, _))));
    let r = ureq::get(&format!("{url}/jobs/abc"))
        .set("Authorization", "Bearer wrong-token-wrong-token!!")
        .call();
    assert!(matches!(r, Err(ureq::Error::Status(401, _))));
    let hostile = json!({"schema": 1, "project": {}, "assets": [{"sha256": "a".repeat(64), "path": "../../etc/passwd", "bytes": 1}],
                         "render": {"size_preset": null, "width": null, "height": null, "codec": null, "crf": null, "bit_depth": null, "format": null, "burn_captions": true, "timeline_id": null},
                         "output_name": "render.mp4"});
    let r = ureq::post(&format!("{url}/jobs"))
        .set("Authorization", &format!("Bearer {}", "t".repeat(24)))
        .send_json(hostile);
    assert!(matches!(r, Err(ureq::Error::Status(400, _))), "{r:?}");
    // Path-traversal in a job id is rejected, not resolved.
    let r = ureq::get(&format!("{url}/jobs/..%2F..%2Fetc/output"))
        .set("Authorization", &format!("Bearer {}", "t".repeat(24)))
        .call();
    assert!(matches!(
        r,
        Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(400, _))
    ));
    w.stop();
}

#[test]
fn interrupted_upload_resumes_from_the_workers_offset_and_bad_bytes_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let token = "t".repeat(24);
    let w = worker::start(WorkerConfig {
        bind: "127.0.0.1:0".into(),
        root: dir.path().join("w"),
        token: token.clone(),
        max_runtime: None,
    })
    .unwrap();
    let url = w.url();
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let sha = worker::sha256_hex(&data);
    let auth = format!("Bearer {token}");
    // First half lands, then the "connection drops".
    let r: Value = ureq::put(&format!("{url}/assets/{sha}?offset=0&total=100000"))
        .set("Authorization", &auth)
        .send_bytes(&data[..40_000])
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(r["size"], 40_000);
    // Status reports where to resume; a wrong offset is refused with the true size.
    let st: Value = ureq::get(&format!("{url}/assets/{sha}"))
        .set("Authorization", &auth)
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(st["size"], 40_000);
    let bad = ureq::put(&format!("{url}/assets/{sha}?offset=0&total=100000"))
        .set("Authorization", &auth)
        .send_bytes(&data[..10]);
    assert!(matches!(bad, Err(ureq::Error::Status(409, _))));
    let done: Value = ureq::put(&format!("{url}/assets/{sha}?offset=40000&total=100000"))
        .set("Authorization", &auth)
        .send_bytes(&data[40_000..])
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(done["have"], true);
    // Corrupt bytes for a different declared hash are discarded.
    let other = worker::sha256_hex(b"something else");
    let corrupt = ureq::put(&format!("{url}/assets/{other}?offset=0&total=5"))
        .set("Authorization", &auth)
        .send_bytes(b"12345");
    assert!(matches!(corrupt, Err(ureq::Error::Status(422, _))));
    w.stop();
}

#[test]
fn tool_is_off_by_default_and_step_one_is_a_free_quote() {
    let fx = fixture();
    let registry = cinvar::tools::build_default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let mut project = fx.project.clone();
    let mut config = cinvar::config::Config::default();
    assert!(
        !config.security.allow_cloud_render,
        "cloud rendering must default to off"
    );
    let args =
        json!({"max_price_per_hr": 1.0, "max_runtime_minutes": 30, "image": "ghcr.io/example/w:1"});
    {
        let mut ctx = cinvar::tools::ToolCtx {
            project: &mut project,
            store: &fx.store,
            config: &config,
            jobs: &jobs,
            history: &mut history,
            dry_run: false,
        };
        let e = registry
            .call("cinvar.render.cloud", &mut ctx, args.clone())
            .unwrap_err();
        assert_eq!(e.code, cinvar::error::ErrorCode::PermissionDenied);
    }
    config.security.allow_cloud_render = true;
    std::env::set_var("CINVAR_RUNPOD_API_KEY", KEY);
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &fx.store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    let out = registry
        .call("cinvar.render.cloud", &mut ctx, args)
        .unwrap();
    assert_eq!(out["billed"], false);
    assert!(out["quote"]["id"].as_str().unwrap().starts_with("q_"));
    assert!(
        out["quote"]["worst_case_usd"].as_f64().unwrap() > 0.49
            && out["quote"]["worst_case_usd"].as_f64().unwrap() < 0.51
    );
    assert!(!out.to_string().contains(KEY));
    // Missing ceiling is refused even with permission.
    let e = registry
        .call(
            "cinvar.render.cloud",
            &mut ctx,
            json!({"max_runtime_minutes": 30, "image": "x"}),
        )
        .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Budget);
}

#[test]
fn real_worker_process_serves_health() {
    let dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cinvar");
    let mut child = std::process::Command::new(bin)
        .args(["worker", "--bind", "127.0.0.1:18931", "--root"])
        .arg(dir.path().join("w"))
        .env("CINVAR_WORKER_TOKEN", "0123456789abcdef0123")
        .env("CINVAR_WORKER_MAX_RUNTIME_SECS", "30")
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut ok = false;
    for _ in 0..50 {
        if ureq::get("http://127.0.0.1:18931/health").call().is_ok() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(ok, "worker process never became healthy");
}

#[test]
fn unavailable_gpu_fails_clearly_with_nothing_provisioned() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    mock.create_mode.store(1, Ordering::SeqCst);
    let e = run_with(&mock, &fx, 1.0, None, &|| false).unwrap_err();
    assert!(e.message.contains("no matching GPU is available"), "{e}");
    assert_eq!(mock.creates(), 0);
    assert!(mock.deleted().is_empty());
}

#[test]
fn api_timeout_on_create_is_marked_uncertain_and_blocks_a_blind_retry() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    mock.create_mode.store(2, Ordering::SeqCst);
    let (m, files, out) = built(&fx);
    let opts = mock.opts(1.0);
    let q = orchestrator::quote(&m, &opts).unwrap();
    let jobs = JobStore::open(&fx.store.jobs_dir()).unwrap();
    let client = mock.client().with_timeout(1);
    let e = orchestrator::run(&CloudRun {
        client: &client,
        jobs: &jobs,
        manifest: &m,
        files: &files,
        opts: &opts,
        output: &out,
        confirm_quote_id: &q.id,
        cancelled: &|| false,
        progress: &|_, _| {},
    })
    .unwrap_err();
    assert!(e.message.contains("outcome unknown"), "{e}");
    let job = &jobs.list().unwrap()[0];
    assert_eq!(job.state, JobState::Uncertain);
}

#[test]
fn failed_termination_is_reported_loudly_and_a_later_sweep_releases_the_pod() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    mock.fail_delete.store(true, Ordering::SeqCst);
    let out = run_with(&mock, &fx, 1.0, None, &|| false).unwrap();
    assert!(
        !out.pod_released,
        "termination failed, must not claim release"
    );
    assert!(out.note.contains("may still be billing"), "{}", out.note);
    // provider recovers; the orphan sweep finishes the job
    mock.fail_delete.store(false, Ordering::SeqCst);
    let jobs = JobStore::open(&fx.store.jobs_dir()).unwrap();
    let released =
        orchestrator::cleanup_orphans(&mock.client(), &jobs, Duration::from_millis(20)).unwrap();
    assert_eq!(released.len(), 1, "{released:?}");
}

#[test]
fn spend_limit_refuses_quotes_whose_worst_case_exceeds_it() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let (m, _f, _o) = built(&fx);
    let mut opts = mock.opts(1.0); // $1/h for 300 s = $0.0833 worst case
    opts.max_spend_usd = Some(0.05);
    let e = orchestrator::quote(&m, &opts).unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Budget);
    opts.max_spend_usd = Some(0.10);
    assert!(orchestrator::quote(&m, &opts).is_ok());
    assert_eq!(mock.creates(), 0);
}

#[test]
fn without_billing_risk_acknowledgement_nothing_is_provisioned() {
    let fx = fixture();
    let mock = Mock::start(fx.dir.path().join("workers"));
    let (m, files, out) = built(&fx);
    let mut opts = mock.opts(1.0);
    opts.acknowledge_billing_risk = false;
    let q = orchestrator::quote(&m, &opts).unwrap();
    let jobs = JobStore::open(&fx.store.jobs_dir()).unwrap();
    let client = mock.client();
    let e = orchestrator::run(&CloudRun {
        client: &client,
        jobs: &jobs,
        manifest: &m,
        files: &files,
        opts: &opts,
        output: &out,
        confirm_quote_id: &q.id,
        cancelled: &|| false,
        progress: &|_, _| {},
    })
    .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Budget);
    assert!(e.message.contains("acknowledge"), "{e}");
    assert_eq!(mock.creates(), 0);
}
