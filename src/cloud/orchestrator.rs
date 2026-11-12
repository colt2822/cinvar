//! Cloud render orchestration with cost protection and guaranteed cleanup.
//!
//! What is and is NOT enforced (see CLOUD_COST_PROTECTION in the final report):
//! * ENFORCED locally: explicit quote confirmation, a user price ceiling checked against the
//!   created pod's reported `costPerHr` (pod terminated at once if above), a hard runtime
//!   deadline, termination on success/failure/cancel, duplicate-job prevention, orphan sweep.
//! * ENFORCED remotely (defence in depth): the worker stops serving after `max_runtime`, so a
//!   vanished controller cannot leave a GPU busy forever.
//! * NOT enforced: a provider-side hard spend cap. RunPod bills while the pod exists; a crash
//!   between "create" and "terminate" can bill until the worker deadline / next orphan sweep.

// `ureq::Error` is large (272 bytes) and is the error type of the HTTP calls wrapped below; boxing
// it everywhere would only add noise. Scoped to this module, this lint only.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::manifest::JobManifest;
use super::runpod::{recommend_gpus, CloudErrorKind, CreatePod, Pod, RunpodClient};
use super::{CHUNK_BYTES, WORKER_PORT};
use crate::error::{Error, ErrorCode, Result};
use crate::jobstore::{JobState, JobStore, Submission};
use crate::project::store::sha256_file;
use crate::util::paths;

pub const HARD_MAX_RUNTIME_SECS: u64 = 24 * 3600;
pub const POD_NAME_PREFIX: &str = "cinvar-";

/// Maps a created pod to the worker's base URL.
pub type WorkerUrlFn = Arc<dyn Fn(&Pod) -> Option<String> + Send + Sync>;

#[derive(Clone)]
pub struct CloudOptions {
    pub gpu_type_ids: Vec<String>,
    pub min_vram_gb: u32,
    /// USD per hour. Required: there is no default spend.
    pub max_price_per_hr: f64,
    pub max_runtime_secs: u64,
    pub image_name: String,
    pub container_disk_gb: u32,
    pub startup_timeout: Duration,
    /// Optional personal spending limit (USD). A quote whose worst case exceeds it is refused.
    pub max_spend_usd: Option<f64>,
    /// The user has read that RunPod billing cannot be capped by CINVAR and accepts that risk.
    /// Without this, `run` refuses to provision anything.
    pub acknowledge_billing_risk: bool,
    pub poll_interval: Duration,
    /// Maps a created pod to the worker's base URL. Production: the RunPod proxy URL.
    pub worker_url: WorkerUrlFn,
}

impl CloudOptions {
    pub fn production(
        max_price_per_hr: f64,
        max_runtime_secs: u64,
        image_name: &str,
    ) -> CloudOptions {
        CloudOptions {
            gpu_type_ids: vec![],
            min_vram_gb: 24,
            max_price_per_hr,
            max_runtime_secs,
            image_name: image_name.to_string(),
            container_disk_gb: 80,
            startup_timeout: Duration::from_secs(600),
            max_spend_usd: None,
            acknowledge_billing_risk: false,
            poll_interval: Duration::from_secs(5),
            worker_url: Arc::new(|pod: &Pod| Some(super::runpod::proxy_url(&pod.id, WORKER_PORT))),
        }
    }

    fn validate(&self) -> Result<()> {
        if !(self.max_price_per_hr.is_finite() && self.max_price_per_hr > 0.0) {
            return Err(Error::new(
                ErrorCode::Budget,
                "a positive max price per hour is required; CINVAR never spends without a ceiling",
            ));
        }
        if self.max_runtime_secs == 0 || self.max_runtime_secs > HARD_MAX_RUNTIME_SECS {
            return Err(Error::new(
                ErrorCode::Budget,
                format!("max runtime must be 1..={HARD_MAX_RUNTIME_SECS} seconds"),
            ));
        }
        if self.image_name.trim().is_empty() {
            return Err(Error::invalid("a worker image is required"));
        }
        if let Some(limit) = self.max_spend_usd {
            let worst = self.max_price_per_hr * self.max_runtime_secs as f64 / 3600.0;
            if !(limit.is_finite() && limit > 0.0) {
                return Err(Error::new(
                    ErrorCode::Budget,
                    "max_spend_usd must be a positive number",
                ));
            }
            if worst > limit + 1e-9 {
                return Err(Error::new(
                    ErrorCode::Budget,
                    format!("worst case ${worst:.2} (price ceiling x runtime limit) exceeds your spend limit of ${limit:.2}"),
                )
                .with_hint("lower the runtime limit or the hourly price ceiling, or raise the spend limit"));
            }
        }
        Ok(())
    }

    fn gpus(&self) -> Vec<String> {
        if self.gpu_type_ids.is_empty() {
            recommend_gpus(self.min_vram_gb)
        } else {
            self.gpu_type_ids.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub id: String,
    pub manifest_hash: String,
    pub gpu_candidates: Vec<String>,
    pub max_price_per_hr: f64,
    pub max_runtime_secs: u64,
    /// Upper bound if the pod lives the whole runtime at the price ceiling. NOT a prediction.
    pub worst_case_usd: f64,
    pub upload_bytes_total: u64,
    pub image_name: String,
    pub disclosure: Vec<String>,
}

pub fn quote(manifest: &JobManifest, opts: &CloudOptions) -> Result<Quote> {
    opts.validate()?;
    let gpus = opts.gpus();
    if gpus.is_empty() {
        return Err(Error::unsupported(format!(
            "no known GPU offers {} GB of VRAM; choose gpu_type_ids explicitly",
            opts.min_vram_gb
        )));
    }
    let worst = opts.max_price_per_hr * opts.max_runtime_secs as f64 / 3600.0;
    let mut h = Sha256::new();
    h.update(manifest.hash());
    h.update(format!(
        "{gpus:?}|{:.4}|{}|{}",
        opts.max_price_per_hr, opts.max_runtime_secs, opts.image_name
    ));
    let id: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Quote {
        id: format!("q_{id}"),
        manifest_hash: manifest.hash(),
        gpu_candidates: gpus,
        max_price_per_hr: opts.max_price_per_hr,
        max_runtime_secs: opts.max_runtime_secs,
        worst_case_usd: (worst * 100.0).ceil() / 100.0,
        upload_bytes_total: manifest.total_asset_bytes(),
        image_name: opts.image_name.clone(),
        disclosure: vec![
            "This sends your project file and every media asset it uses to a RunPod GPU pod you pay for.".into(),
            "Cost: the pod is billed by RunPod while it exists. The worst-case figure assumes the price ceiling for the whole runtime limit; the real price is only known when the pod is created and is checked against your ceiling immediately.".into(),
            "No completion time is promised: there is no benchmark for this workload yet.".into(),
            "The pod is terminated when the render finishes, fails or is cancelled; the worker also stops itself at the runtime limit.".into(),
            "CINVAR cannot set a provider-side spending cap; protection is local checks plus the runtime limit.".into(),
        ],
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudOutcome {
    pub job_id: String,
    pub duplicate: bool,
    pub output: Option<PathBuf>,
    pub bytes: u64,
    pub sha256: String,
    pub pod_id: Option<String>,
    pub reported_cost_per_hr: Option<f64>,
    pub uploaded_assets: usize,
    pub skipped_assets: usize,
    pub pod_released: bool,
    pub elapsed_secs: f64,
    pub note: String,
}

pub struct CloudRun<'a> {
    pub client: &'a RunpodClient,
    pub jobs: &'a JobStore,
    pub manifest: &'a JobManifest,
    pub files: &'a [(String, PathBuf)],
    pub opts: &'a CloudOptions,
    pub output: &'a Path,
    pub confirm_quote_id: &'a str,
    pub cancelled: &'a dyn Fn() -> bool,
    pub progress: &'a dyn Fn(f32, &str),
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Remote {
    pod_id: Option<String>,
    token: String,
    worker_job_id: Option<String>,
}

fn remote_of(job: &crate::jobstore::JobRecord) -> Remote {
    job.remote_ref
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_remote(jobs: &JobStore, id: &str, r: &Remote) -> Result<()> {
    jobs.update(id, |j| {
        j.remote_ref = Some(serde_json::to_string(r).unwrap_or_default())
    })?;
    Ok(())
}

fn new_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..4)
        .map(|_| format!("{:08x}", rng.gen::<u32>()))
        .collect()
}

/// Terminate with retries, then verify. Returns whether the pod is confirmed gone.
pub fn release_pod(client: &RunpodClient, pod_id: &str, pause: Duration) -> bool {
    for attempt in 0..4 {
        if client.terminate_pod(pod_id).is_ok() {
            match client.get_pod(pod_id) {
                Err(e) if e.kind == CloudErrorKind::NotFound => return true,
                Ok(p) if p.desired_status == "TERMINATED" => return true,
                _ => {}
            }
        }
        std::thread::sleep(pause * (attempt + 1));
    }
    false
}

/// Terminates pods left behind by THIS project's interrupted/failed/uncertain/cancelled jobs.
/// A pod is only touched if its name matches a job record in `jobs` that is not actively running
/// in a live process; unknown `cinvar-*` pods (other projects, other machines) are never touched.
pub fn cleanup_orphans(
    client: &RunpodClient,
    jobs: &JobStore,
    pause: Duration,
) -> Result<Vec<String>> {
    let mut released = Vec::new();
    let records = jobs.list()?;
    let owned: Vec<String> = records
        .iter()
        .filter(|r| r.kind == "cloud_render")
        .filter(|r| {
            !(r.state == JobState::Running
                && r.owner_pid
                    .map(crate::jobstore::process_alive)
                    .unwrap_or(false))
        })
        .map(|r| format!("{POD_NAME_PREFIX}{}", r.id))
        .collect();
    if owned.is_empty() {
        return Ok(released);
    }
    for pod in client.list_pods().map_err(Error::from)? {
        if owned.contains(&pod.name)
            && pod.desired_status != "TERMINATED"
            && release_pod(client, &pod.id, pause)
        {
            released.push(pod.id);
        }
    }
    Ok(released)
}

fn http(agent: &ureq::Agent, method: &str, url: &str, token: &str) -> ureq::Request {
    agent
        .request(method, url)
        .set("Authorization", &format!("Bearer {token}"))
}

fn transient(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Transport(_) => true,
        ureq::Error::Status(c, _) => *c == 429 || *c >= 500,
    }
}

/// Idempotent GET/PUT with a few retries for transient failures.
fn with_retry<T>(
    mut f: impl FnMut() -> std::result::Result<T, ureq::Error>,
) -> std::result::Result<T, ureq::Error> {
    let mut last = None;
    for i in 0..4 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if transient(&e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(200 * (i + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap())
}

fn json_of(r: ureq::Response) -> Value {
    r.into_json().unwrap_or(Value::Null)
}

fn upload_asset(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    sha: &str,
    path: &Path,
) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let total = std::fs::metadata(path)?.len();
    let status =
        with_retry(|| http(agent, "GET", &format!("{base}/assets/{sha}"), token).call())
            .map_err(|e| Error::new(ErrorCode::Provider, format!("asset status failed: {e}")))?;
    let st = json_of(status);
    if st["have"] == true {
        return Ok(false);
    }
    let mut offset = st["size"].as_u64().unwrap_or(0).min(total);
    let mut file = std::fs::File::open(path)?;
    loop {
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; CHUNK_BYTES.min(total - offset) as usize];
        file.read_exact(&mut buf)?;
        let url = format!("{base}/assets/{sha}?offset={offset}&total={total}");
        let resp = with_retry(|| http(agent, "PUT", &url, token).send_bytes(&buf));
        match resp {
            Ok(r) => {
                let v = json_of(r);
                if v["have"] == true {
                    return Ok(true);
                }
                offset = v["size"].as_u64().unwrap_or(offset + buf.len() as u64);
            }
            Err(ureq::Error::Status(409, r)) => {
                // Server is at a different offset (e.g. a retried chunk already landed): resync.
                offset = json_of(r)["size"].as_u64().unwrap_or(0);
            }
            Err(ureq::Error::Status(422, _)) => {
                return Err(Error::new(
                    ErrorCode::Provider,
                    format!("remote checksum mismatch for asset {sha}"),
                ));
            }
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::Provider,
                    format!("asset upload failed: {e}"),
                ))
            }
        }
        if offset >= total {
            // Final chunk acknowledged without "have": confirm.
            let again = json_of(
                with_retry(|| http(agent, "GET", &format!("{base}/assets/{sha}"), token).call())
                    .map_err(|e| Error::new(ErrorCode::Provider, e.to_string()))?,
            );
            return if again["have"] == true {
                Ok(true)
            } else {
                Err(Error::new(
                    ErrorCode::Provider,
                    "upload finished but the worker does not report the asset",
                ))
            };
        }
    }
}

fn download_output(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    wjob: &str,
    bytes: u64,
    partial: &Path,
) -> Result<()> {
    use std::io::{Read, Write};
    crate::util::paths::unlink_if_symlink(partial);
    let mut have = partial.metadata().map(|m| m.len()).unwrap_or(0);
    if have > bytes {
        std::fs::remove_file(partial)?;
        have = 0;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(partial)?;
    while have < bytes {
        let url = format!("{base}/jobs/{wjob}/output?offset={have}&len={CHUNK_BYTES}");
        let r = with_retry(|| http(agent, "GET", &url, token).call())
            .map_err(|e| Error::new(ErrorCode::Provider, format!("download failed: {e}")))?;
        let mut buf = Vec::new();
        r.into_reader()
            .take(CHUNK_BYTES + 1)
            .read_to_end(&mut buf)?;
        if buf.is_empty() {
            return Err(Error::new(
                ErrorCode::Provider,
                "worker returned an empty chunk before the output was complete",
            ));
        }
        f.write_all(&buf)?;
        have += buf.len() as u64;
    }
    f.sync_all()?;
    Ok(())
}

pub fn run(r: &CloudRun) -> Result<CloudOutcome> {
    let started = Instant::now();
    r.opts.validate()?;
    let q = quote(r.manifest, r.opts)?;
    if !r.opts.acknowledge_billing_risk {
        return Err(Error::new(
            ErrorCode::Budget,
            "CINVAR cannot cap RunPod spending at the provider; you must acknowledge that before anything is provisioned",
        )
        .with_hint("pass acknowledge_billing_risk=true after reading the quote disclosure"));
    }
    if r.confirm_quote_id != q.id {
        return Err(Error::new(
            ErrorCode::Budget,
            "billable provisioning needs explicit confirmation of the current quote id",
        )
        .with_hint(format!("review the quote and pass confirm_quote={}", q.id)));
    }
    (r.progress)(0.0, "validating RunPod credentials");
    r.client.validate_credentials().map_err(Error::from)?;

    // Anything a previous interrupted run left billing is released before new spend starts.
    r.jobs.recover()?;
    cleanup_orphans(r.client, r.jobs, Duration::from_millis(50))?;

    let manifest_json = json!({"quote": q.id, "output": r.output.to_string_lossy()});
    let submission = r.jobs.submit("cloud_render", manifest_json)?;
    let job = match submission {
        Submission::Existing(rec) if rec.state != JobState::Queued => {
            return Ok(CloudOutcome {
                job_id: rec.id.clone(),
                duplicate: true,
                output: None,
                bytes: 0,
                sha256: String::new(),
                pod_id: remote_of(&rec).pod_id,
                reported_cost_per_hr: None,
                uploaded_assets: 0,
                skipped_assets: 0,
                pod_released: false,
                elapsed_secs: 0.0,
                note: format!("an identical confirmed cloud render already exists ({:?}); no second pod was started", rec.state),
            })
        }
        Submission::Existing(rec) => rec,
        Submission::Created(rec) => rec,
    };
    r.jobs.start(&job.id)?;
    let mut remote = Remote {
        token: new_token(),
        ..Default::default()
    };
    save_remote(r.jobs, &job.id, &remote)?;

    let mut pod_id: Option<String> = None;
    let mut cost: Option<f64> = None;
    let result = execute(r, &q, &job.id, &mut remote, &mut pod_id, &mut cost);

    // Cleanup runs on every path: success, failure, cancel.
    let mut released = true;
    if let Some(id) = &pod_id {
        released = release_pod(r.client, id, Duration::from_millis(100));
        let _ = r.jobs.checkpoint(
            &job.id,
            if released {
                "pod released"
            } else {
                "POD RELEASE FAILED"
            },
        );
    }
    match result {
        Ok(mut outcome) => {
            outcome.pod_released = released;
            outcome.reported_cost_per_hr = cost;
            outcome.elapsed_secs = started.elapsed().as_secs_f64();
            if !released {
                outcome.note = format!(
                    "{} WARNING: pod {} may still be billing; run cloud cleanup.",
                    outcome.note,
                    pod_id.unwrap_or_default()
                );
            }
            r.jobs.succeed(&job.id, json!(outcome))?;
            Ok(outcome)
        }
        Err(e) => {
            let msg = if released {
                e.message.clone()
            } else {
                format!(
                    "{} -- POD {} MAY STILL BE BILLING; run cloud cleanup",
                    e.message,
                    pod_id.clone().unwrap_or_default()
                )
            };
            if e.code == ErrorCode::Cancelled {
                let _ = r.jobs.update(&job.id, |j| j.cancel_requested = true);
            }
            if e.code == ErrorCode::Provider && e.message.contains("outcome unknown") {
                let _ = r.jobs.mark_uncertain(&job.id, &e.message);
            } else {
                let _ = r.jobs.fail(&job.id, &msg);
            }
            Err(Error::new(e.code, msg))
        }
    }
}

fn execute(
    r: &CloudRun,
    q: &Quote,
    job_id: &str,
    remote: &mut Remote,
    pod_slot: &mut Option<String>,
    cost_slot: &mut Option<f64>,
) -> Result<CloudOutcome> {
    let deadline = Instant::now() + Duration::from_secs(r.opts.max_runtime_secs);
    let check = |what: &str| -> Result<()> {
        if (r.cancelled)() {
            return Err(Error::cancelled(format!("cancelled while {what}")));
        }
        if Instant::now() > deadline {
            return Err(Error::new(
                ErrorCode::Budget,
                format!(
                    "runtime limit of {}s reached while {what}",
                    r.opts.max_runtime_secs
                ),
            ));
        }
        Ok(())
    };

    (r.progress)(0.03, "creating pod");
    let name = format!("{POD_NAME_PREFIX}{job_id}");
    let mut env = serde_json::Map::new();
    env.insert("CINVAR_WORKER_TOKEN".into(), json!(remote.token));
    env.insert(
        "CINVAR_WORKER_MAX_RUNTIME_SECS".into(),
        json!(r.opts.max_runtime_secs),
    );
    let spec = CreatePod {
        name: name.clone(),
        image_name: r.opts.image_name.clone(),
        gpu_type_ids: q.gpu_candidates.clone(),
        gpu_count: 1,
        container_disk_in_gb: r.opts.container_disk_gb,
        volume_in_gb: 0,
        ports: vec![format!("{WORKER_PORT}/http")],
        env,
        cloud_type: "SECURE".into(),
        interruptible: false,
    };
    let created = match r.client.create_pod(&spec) {
        Ok(p) => p,
        Err(e) if e.kind == CloudErrorKind::Timeout || e.kind == CloudErrorKind::Transient => {
            // Outcome unknown: look for the pod by its deterministic name before deciding.
            let listed = r.client.list_pods().map_err(|le| {
                Error::new(
                    ErrorCode::Provider,
                    format!(
                        "pod create outcome unknown (create failed: {}; lookup failed too: {}); a pod may exist and bill until the sweep or runtime limit",
                        e.message, le.message
                    ),
                )
            })?;
            let found = listed.into_iter().find(|p| p.name == name);
            match found {
                Some(p) => p,
                None if e.kind == CloudErrorKind::Timeout => {
                    return Err(Error::new(
                        ErrorCode::Provider,
                        format!(
                            "pod create outcome unknown (timed out, no pod found yet): {}",
                            e.message
                        ),
                    ));
                }
                None => return Err(Error::from(e)),
            }
        }
        Err(e) => {
            let low = e.message.to_ascii_lowercase();
            if e.kind == CloudErrorKind::Rejected
                && [
                    "no instances",
                    "unavailable",
                    "capacity",
                    "not available",
                    "no gpu",
                    "out of stock",
                ]
                .iter()
                .any(|k| low.contains(k))
            {
                return Err(Error::new(
                    ErrorCode::Provider,
                    "no matching GPU is available on RunPod right now; nothing was provisioned or billed. Try again later or allow other gpu_type_ids",
                ));
            }
            return Err(Error::from(e));
        }
    };
    *pod_slot = Some(created.id.clone());
    remote.pod_id = Some(created.id.clone());
    save_remote(r.jobs, job_id, remote)?;
    r.jobs
        .checkpoint(job_id, &format!("pod created {}", created.id))?;

    // The real price is only known now. Enforce the user's ceiling immediately.
    *cost_slot = created.cost_per_hr;
    match created.cost_per_hr {
        Some(c) if c > r.opts.max_price_per_hr => {
            return Err(Error::new(
                ErrorCode::Budget,
                format!("pod costs ${c:.3}/h, above your ceiling of ${:.3}/h; terminating it immediately", r.opts.max_price_per_hr),
            ));
        }
        None => {
            return Err(Error::new(ErrorCode::Budget, "the provider did not report a price for the pod, so the ceiling cannot be verified; terminating"));
        }
        _ => {}
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build();

    (r.progress)(0.06, "waiting for the worker to start");
    let startup_deadline = Instant::now() + r.opts.startup_timeout;
    let base = loop {
        check("waiting for the worker")?;
        if Instant::now() > startup_deadline {
            return Err(Error::new(
                ErrorCode::Provider,
                "the worker did not become reachable in time",
            ));
        }
        let pod = r.client.get_pod(&created.id).map_err(Error::from)?;
        if pod.desired_status == "TERMINATED" || pod.desired_status == "EXITED" {
            return Err(Error::new(
                ErrorCode::Provider,
                format!("pod stopped unexpectedly ({})", pod.desired_status),
            ));
        }
        if let Some(url) = (r.opts.worker_url)(&pod) {
            if agent.get(&format!("{url}/health")).call().is_ok() {
                break url;
            }
        }
        std::thread::sleep(r.opts.poll_interval);
    };

    let total_assets = r.files.len().max(1);
    let (mut uploaded, mut skipped) = (0usize, 0usize);
    for (i, (sha, path)) in r.files.iter().enumerate() {
        check("uploading assets")?;
        (r.progress)(
            0.15 + 0.2 * (i as f32 / total_assets as f32),
            &format!("uploading asset {}/{}", i + 1, r.files.len()),
        );
        if upload_asset(&agent, &base, &remote.token, sha, path)? {
            uploaded += 1;
        } else {
            skipped += 1;
        }
    }

    check("submitting the job")?;
    let resp = with_retry(|| {
        http(&agent, "POST", &format!("{base}/jobs"), &remote.token)
            .send_json(serde_json::to_value(r.manifest).unwrap())
    })
    .map_err(|e| Error::new(ErrorCode::Provider, format!("job submit failed: {e}")))?;
    let wjob = json_of(resp)["job_id"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::Provider, "worker returned no job id"))?
        .to_string();
    remote.worker_job_id = Some(wjob.clone());
    save_remote(r.jobs, job_id, remote)?;

    let (bytes, sha) = loop {
        if let Err(e) = check("rendering") {
            let _ = http(
                &agent,
                "POST",
                &format!("{base}/jobs/{wjob}/cancel"),
                &remote.token,
            )
            .call();
            return Err(e);
        }
        let st = json_of(
            with_retry(|| {
                http(&agent, "GET", &format!("{base}/jobs/{wjob}"), &remote.token).call()
            })
            .map_err(|e| Error::new(ErrorCode::Provider, format!("status failed: {e}")))?,
        );
        let p = st["progress"].as_f64().unwrap_or(0.0) as f32;
        (r.progress)(
            0.35 + 0.55 * p,
            st["message"].as_str().unwrap_or("rendering"),
        );
        let _ = r.jobs.progress(
            job_id,
            0.35 + 0.55 * p,
            st["message"].as_str().unwrap_or("rendering"),
        );
        match st["state"].as_str().unwrap_or("") {
            "succeeded" => {
                break (
                    st["output_bytes"].as_u64().unwrap_or(0),
                    st["output_sha256"].as_str().unwrap_or("").to_string(),
                )
            }
            "failed" => {
                return Err(Error::new(
                    ErrorCode::Render,
                    format!(
                        "remote render failed: {}",
                        st["error"].as_str().unwrap_or("unknown")
                    ),
                ))
            }
            "cancelled" => return Err(Error::cancelled("remote render was cancelled")),
            _ => {}
        }
        std::thread::sleep(r.opts.poll_interval);
    };

    (r.progress)(0.92, "downloading result");
    if let Some(parent) = r.output.parent() {
        paths::ensure_dir(parent)?;
    }
    let partial = r.output.with_file_name(format!(
        ".{}.cloud.partial",
        r.output
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("out")
    ));
    download_output(&agent, &base, &remote.token, &wjob, bytes, &partial)?;
    let actual = sha256_file(&partial)?;
    if !actual.eq_ignore_ascii_case(&sha) {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::new(
            ErrorCode::Provider,
            "downloaded output failed checksum verification and was discarded",
        ));
    }
    let validation = crate::render::validate_output(&partial)?;
    if !validation.valid {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "downloaded output is not a valid video: {}",
                validation.issues.join("; ")
            ),
        ));
    }
    std::fs::rename(&partial, r.output)?;
    Ok(CloudOutcome {
        job_id: job_id.to_string(),
        duplicate: false,
        output: Some(r.output.to_path_buf()),
        bytes,
        sha256: actual,
        pod_id: Some(created.id),
        reported_cost_per_hr: *cost_slot,
        uploaded_assets: uploaded,
        skipped_assets: skipped,
        pod_released: false,
        elapsed_secs: 0.0,
        note: "output verified by checksum and decode check".into(),
    })
}
