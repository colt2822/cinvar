//! Distributed animation rendering (Argon 5F) over ssh.
//!
//! One coordinator, N worker machines that only need ssh + tar + xz. Frames are partitioned into
//! deterministic, self-describing work units; each worker renders its unit with a portable Blender
//! cached under $HOME/.cache/cinvar/blender-dist/<version>/ and sends the PNGs back; the coordinator
//! validates every frame before it is placed, never overwrites a frame that is already verified, and
//! reassembles through the same encode path a local render uses.
//!
//! # What this module decided about Blender's own flags
//!
//! * use_overwrite = False. The generated scene already sets it, and it is exactly the guarantee
//!   this module needs: a second pass over a frame range that is already on disk renders nothing and
//!   therefore cannot damage a verified frame. Re-running a finished range is a no-op render.
//! * use_placeholder = False. A placeholder writes a stub file for a frame before it is rendered, so
//!   a killed worker leaves files that exist but are not images. That turns "missing" into "present
//!   but corrupt", which is the one distinction the completeness check relies on, and Blender will
//!   then skip the frame on a retry because the file is there. Units instead render into a private
//!   worker directory and only IEND-complete frames are transferred and placed, so a placeholder
//!   would buy nothing and cost correctness.
//!
//! # Transfer, why tar and not a shared filesystem
//!
//! There is no fast shared storage between these machines (only an sshfs NAS share, unsuitable for
//! frame traffic), so each unit travels as a tar stream on ssh stdin and frames come back as a
//! base64 tar on stdout. A unit is idempotent: its remote directory is derived from its content
//! hash, so re-sending the same unit overwrites the same place and a resumed run cannot mix halves
//! of two different attempts.
//!
//! # Cache identity
//!
//! Cache identity stays spec_hash, unchanged: the job directory, the frames and the final clip live
//! under <base>/<spec_hash>/ exactly as a local render lays them out, which is what makes a
//! distributed run resumable by a local one and vice versa.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::blender::{self, gpu, AnimationSpec, RenderControl};
use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

#[path = "render_media.rs"]
pub mod render_media;
#[path = "render_tar.rs"]
pub mod render_tar;
#[path = "render_transport.rs"]
pub mod render_transport;

use render_media as media;
use render_tar::TarFile;
use render_transport as transport;

/// The Python line the generated script renders with. Patching replaces exactly this call.
pub const RENDER_CALL: &str = "bpy.ops.render.render(animation=True)";

/// Cycles settings both the workers and the single-machine baseline use, so the comparison is fair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderProfile {
    pub samples: u32,
    pub denoise: bool,
    /// Cycles adaptive sampling. Off by default: with it on, the sample count per pixel depends on
    /// how tiles happened to be scheduled, so two machines rendering the same frame can disagree
    /// pixel by pixel. That is a scheduling difference, not a wrong frame, but a distributed render
    /// whose frames cannot be compared is much harder to trust, and a reproducible frame is worth
    /// more here than the few percent adaptive sampling saves.
    #[serde(default)]
    pub adaptive: bool,
}

impl Default for RenderProfile {
    fn default() -> Self {
        RenderProfile {
            samples: 32,
            denoise: false,
            adaptive: false,
        }
    }
}

impl RenderProfile {
    /// Stable key used in a unit's identity.
    pub fn key(&self) -> String {
        format!(
            "samples={};denoise={};adaptive={}",
            self.samples, self.denoise, self.adaptive
        )
    }
}

/// One worker, as written on the command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkerSpec {
    /// ssh destination: a host alias, or user@host. Never a URL.
    pub host: String,
}

impl WorkerSpec {
    pub fn label(&self) -> String {
        self.host.clone()
    }
}

/// Parses a worker list: comma or whitespace separated, ssh:// optional, duplicates removed in order.
pub fn parse_workers(input: &str) -> Result<Vec<WorkerSpec>> {
    let mut out: Vec<WorkerSpec> = Vec::new();
    for raw in input.split(|c: char| c == ',' || c.is_whitespace()) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        let host = item
            .strip_prefix("ssh://")
            .unwrap_or(item)
            .trim_end_matches('/');
        if host.is_empty() {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                format!("worker {item:?} has no host"),
            ));
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                format!(
                    "worker {host:?} contains characters that are unsafe in a remote command"
                ),
            )
            .with_hint("use a host alias from ~/.ssh/config; ports and paths do not belong here"));
        }
        if !out.iter().any(|w| w.host == host) {
            out.push(WorkerSpec {
                host: host.to_string(),
            });
        }
    }
    if out.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "no workers given",
        )
        .with_hint("pass --workers ssh://n1,ssh://n2"));
    }
    Ok(out)
}

/// A contiguous span of frames that one worker renders in one Blender invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnit {
    pub index: usize,
    pub start: u32,
    pub end: u32,
}

impl WorkUnit {
    pub fn frames(&self) -> u32 {
        self.end.saturating_sub(self.start) + 1
    }
}

/// Splits a frame list into contiguous units of at most max_frames frames each.
///
/// Deterministic: the same frames and the same cap always produce the same units in the same order,
/// which is what lets a resumed run recognise that a unit is already finished.
pub fn partition(frames: &[u32], max_frames: u32) -> Vec<WorkUnit> {
    let cap = max_frames.max(1);
    let mut out: Vec<WorkUnit> = Vec::new();
    let mut index = 0usize;
    let mut run_start: Option<u32> = None;
    let mut previous: u32 = 0;
    let flush = |start: u32, end: u32, index: &mut usize, out: &mut Vec<WorkUnit>| {
        let mut cursor = start;
        loop {
            let stop = (cursor + cap - 1).min(end);
            out.push(WorkUnit {
                index: *index,
                start: cursor,
                end: stop,
            });
            *index += 1;
            if stop >= end {
                break;
            }
            cursor = stop + 1;
        }
    };
    for frame in frames {
        match run_start {
            None => {
                run_start = Some(*frame);
                previous = *frame;
            }
            Some(start) => {
                if *frame == previous + 1 {
                    previous = *frame;
                } else {
                    flush(start, previous, &mut index, &mut out);
                    run_start = Some(*frame);
                    previous = *frame;
                }
            }
        }
    }
    if let Some(start) = run_start {
        flush(start, previous, &mut index, &mut out);
    }
    out
}

/// Target unit size: about four units per worker, so the tail of a run stays short without
/// flooding ssh with one invocation per frame.
pub fn default_unit_frames(missing: usize, workers: usize) -> u32 {
    let target = (workers.max(1) * 4).max(1);
    (((missing + target - 1) / target).max(1)) as u32
}

/// Greedy least-loaded assignment, weighted by worker core count.
///
/// Returns, for each worker, the unit indices it should render, in ascending order. Deterministic:
/// ties go to the earlier worker.
pub fn assign(units: &[WorkUnit], weights: &[u32]) -> Vec<Vec<usize>> {
    let mut loads: Vec<u64> = vec![0; weights.len()];
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); weights.len()];
    if weights.is_empty() {
        return out;
    }
    for unit in units {
        let mut best = 0usize;
        for candidate in 1..weights.len() {
            // Compare load/capacity without floating point: load_b * cap_a < load_a * cap_b.
            let lhs = loads[candidate] * weights[best].max(1) as u64;
            let rhs = loads[best] * weights[candidate].max(1) as u64;
            if lhs < rhs {
                best = candidate;
            }
        }
        loads[best] += unit.frames() as u64;
        out[best].push(unit.index);
    }
    out
}

/// Reads the frame range the generated script will render, straight from the script itself.
pub fn script_frame_range(script: &str) -> Result<(u32, u32)> {
    let read = |name: &str| -> Option<u32> {
        script
            .lines()
            .find(|line| line.trim_start().starts_with(name))
            .and_then(|line| line.split('=').nth(1))
            .and_then(|value| value.trim().parse::<u32>().ok())
    };
    match (read("FRAME_START"), read("FRAME_END")) {
        (Some(start), Some(end)) if end >= start => Ok((start, end)),
        _ => Err(Error::new(
            ErrorCode::Internal,
            "the generated Blender script carries no readable FRAME_START/FRAME_END pair",
        )),
    }
}

/// Python that turns the generated script into one unit: same scene, same animation curves, only the
/// frame window narrows. The device block is applied before rendering, so a worker renders on the
/// device its own probe chose.
pub fn unit_script(
    base_script: &str,
    unit: WorkUnit,
    profile: &RenderProfile,
    device: &gpu::DeviceChoice,
) -> Result<String> {
    if base_script.matches(RENDER_CALL).count() != 1 {
        return Err(Error::new(
            ErrorCode::Internal,
            "the generated Blender script no longer holds exactly one animation render call",
        )
        .with_hint("script_for() changed shape; update the distributed unit patch to match"));
    }
    let mut block = String::new();
    block.push_str("# CINVAR distributed unit: patched by cinvar::blender::distributed.\n");
    block.push_str(&device.apply_script());
    block.push_str(&format!(
        "_cinvar_scene.cycles.samples = {}\n",
        profile.samples.max(1)
    ));
    block.push_str(&format!(
        "_cinvar_scene.cycles.use_denoising = {}\n",
        if profile.denoise { "True" } else { "False" }
    ));
    block.push_str("_cinvar_scene.cycles.time_limit = 0\n");
    block.push_str(&format!(
        "_cinvar_scene.cycles.use_adaptive_sampling = {}\n",
        if profile.adaptive { "True" } else { "False" }
    ));
    // A fixed seed: the same frame on two machines must be the same image.
    block.push_str("_cinvar_scene.cycles.seed = 0\n");
    block.push_str("_cinvar_scene.cycles.use_animated_seed = False\n");
    // Blender must never write over a frame that is already there, and must never leave a stub.
    block.push_str("_cinvar_scene.render.use_overwrite = False\n");
    block.push_str("_cinvar_scene.render.use_placeholder = False\n");
    block.push_str("_cinvar_scene.render.use_persistent_data = False\n");
    block.push_str(&format!("_cinvar_scene.frame_start = {}\n", unit.start));
    block.push_str(&format!("_cinvar_scene.frame_end = {}\n", unit.end));
    block.push_str(&format!(
        "print(\"CINVAR_UNIT_DONE {}..{}\")\n",
        unit.start, unit.end
    ));

    // Insert before the render call, at the same indentation: the call sits inside main(), so a
    // block written at column zero would break the function it is patching.
    let mut out = String::with_capacity(base_script.len() + block.len() + 64);
    let mut injected = false;
    for line in base_script.lines() {
        if !injected && line.contains(RENDER_CALL) {
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            for block_line in block.lines() {
                if block_line.trim().is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&indent);
                    out.push_str(block_line);
                    out.push('\n');
                }
            }
            injected = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !injected {
        return Err(Error::new(
            ErrorCode::Internal,
            "the animation render call could not be located for patching",
        ));
    }
    Ok(out)
}

/// Identity of a unit: derived from what it renders, so sending it twice lands in the same place.
pub fn unit_id(spec_hash: &str, unit: &WorkUnit, profile: &RenderProfile) -> String {
    use sha2::{Digest, Sha256};
    let canonical = format!(
        "{spec_hash}:{}:{}-{}:{}",
        unit.index,
        unit.start,
        unit.end,
        profile.key()
    );
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// The manifest written next to a unit script, so a worker directory explains itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitManifest {
    pub schema: String,
    pub spec_hash: String,
    pub unit_id: String,
    pub index: usize,
    pub start: u32,
    pub end: u32,
    pub frames: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub profile: String,
    pub device: String,
}

/// Frame number encoded in a collected file name (frame_0007.png), if it is one.
pub fn frame_from_name(name: &str) -> Option<u32> {
    let base = name.rsplit('/').next()?;
    let stem = base.strip_prefix("frame_")?.strip_suffix(".png")?;
    if stem.is_empty() || !stem.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    stem.parse::<u32>().ok()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Placement {
    pub placed: usize,
    /// Frames that were already complete and were deliberately left alone.
    pub skipped_verified: usize,
    /// Files that claimed to be frames but were not complete PNGs.
    pub rejected: usize,
    pub frames: Vec<u32>,
}

/// Places collected frames into the job directory.
///
/// Three rules, in order: a file that is not a complete PNG is rejected; a frame that is already
/// verified is never touched; anything else is written atomically (temp file + rename) so an
/// interrupted placement cannot leave a half frame behind.
pub fn place_frames(frames_dir: &Path, files: &[TarFile]) -> Result<Placement> {
    paths::ensure_dir(frames_dir)?;
    let mut placement = Placement::default();
    for file in files {
        let name = file.name.trim_start_matches("./").to_string();
        if name.starts_with('/') || name.contains("..") {
            return Err(Error::new(
                ErrorCode::Protocol,
                format!("collected archive member {name:?} escapes the job directory"),
            ));
        }
        let Some(number) = frame_from_name(&name) else {
            continue; // manifests and logs are not frames
        };
        let target = media::frame_path(frames_dir, number);
        if media::png_complete(&target) {
            placement.skipped_verified += 1;
            placement.frames.push(number);
            continue;
        }
        if !media::png_complete_bytes(&file.data) {
            placement.rejected += 1;
            continue;
        }
        let temp = frames_dir.join(format!(".incoming-{number}.png"));
        std::fs::write(&temp, &file.data)
            .map_err(|e| Error::new(ErrorCode::Io, format!("writing {name}: {e}")))?;
        std::fs::rename(&temp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            Error::new(ErrorCode::Io, format!("placing {name}: {e}"))
        })?;
        placement.placed += 1;
        placement.frames.push(number);
    }
    placement.frames.sort_unstable();
    Ok(placement)
}

/// A frame window that still has to be rendered, plus how many attempts it has had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pending {
    pub index: usize,
    pub start: u32,
    pub end: u32,
    pub attempt: u32,
}

impl Pending {
    pub fn frames(&self) -> u32 {
        self.end.saturating_sub(self.start) + 1
    }
}

/// The units one run still owes, per worker, with orphans for units whose worker went away.
///
/// Pure bookkeeping: no ssh, no filesystem, so the whole assignment and rebalance policy is testable.
#[derive(Debug, Default, Clone)]
pub struct WorkPool {
    queues: Vec<VecDeque<Pending>>,
    orphans: VecDeque<Pending>,
    dead: Vec<bool>,
    next_index: usize,
}

impl WorkPool {
    pub fn new(workers: usize, units: &[WorkUnit], assignment: &[Vec<usize>]) -> WorkPool {
        let mut queues: Vec<VecDeque<Pending>> = vec![VecDeque::new(); workers];
        for (worker, indices) in assignment.iter().enumerate().take(workers) {
            for index in indices {
                if let Some(unit) = units.iter().find(|u| u.index == *index) {
                    queues[worker].push_back(Pending {
                        index: unit.index,
                        start: unit.start,
                        end: unit.end,
                        attempt: 0,
                    });
                }
            }
        }
        let next_index = units.iter().map(|u| u.index).max().map(|m| m + 1).unwrap_or(0);
        WorkPool {
            queues,
            orphans: VecDeque::new(),
            dead: vec![false; workers],
            next_index,
        }
    }

    /// Next unit for a worker: its own queue, then orphans, then the longest other live queue.
    pub fn take(&mut self, worker: usize) -> Option<Pending> {
        if self.dead.get(worker).copied().unwrap_or(true) {
            return None;
        }
        if let Some(pending) = self.queues.get_mut(worker).and_then(|q| q.pop_front()) {
            return Some(pending);
        }
        if let Some(pending) = self.orphans.pop_front() {
            return Some(pending);
        }
        let mut best: Option<usize> = None;
        for index in 0..self.queues.len() {
            if index == worker || self.dead.get(index).copied().unwrap_or(true) {
                continue;
            }
            let depth = self.queues[index].len();
            if depth == 0 {
                continue;
            }
            match best {
                Some(current) if self.queues[current].len() >= depth => {}
                _ => best = Some(index),
            }
        }
        best.and_then(|donor| self.queues[donor].pop_back())
    }

    /// Puts work back for whoever is still alive.
    pub fn requeue(&mut self, pending: Pending) {
        self.orphans.push_back(pending);
    }

    /// A worker is gone: its remaining units become orphans.
    pub fn kill(&mut self, worker: usize) {
        if let Some(slot) = self.dead.get_mut(worker) {
            *slot = true;
        }
        if let Some(queue) = self.queues.get_mut(worker) {
            while let Some(pending) = queue.pop_front() {
                self.orphans.push_back(pending);
            }
        }
    }

    pub fn is_dead(&self, worker: usize) -> bool {
        self.dead.get(worker).copied().unwrap_or(true)
    }

    /// Units still owed by anybody.
    pub fn remaining(&self) -> usize {
        self.orphans.len() + self.queues.iter().map(|q| q.len()).sum::<usize>()
    }

    pub fn next_index(&mut self) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        index
    }
}

#[derive(Debug, Clone)]
pub struct DistributedOptions {
    pub workers: Vec<WorkerSpec>,
    /// Cycles settings for every worker and for the single-machine baseline.
    pub profile: RenderProfile,
    /// Frames per unit; 0 picks a size from the amount of work and the number of workers.
    pub max_unit_frames: u32,
    /// Extra attempts allowed for a unit that produced nothing usable.
    pub retries: u32,
    pub ssh_timeout_secs: u64,
    /// Leave the worker scratch directories in place (debugging only).
    pub keep_remote: bool,
    /// Blender version to provision on workers; defaults to the coordinator's own.
    pub blender_version: Option<String>,
}

impl Default for DistributedOptions {
    fn default() -> Self {
        DistributedOptions {
            workers: Vec::new(),
            profile: RenderProfile::default(),
            max_unit_frames: 0,
            retries: 2,
            ssh_timeout_secs: 600,
            keep_remote: false,
            blender_version: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerTally {
    pub host: String,
    pub cores: u32,
    /// Frames this worker actually delivered and got verified.
    pub frames: u32,
    pub units: usize,
    pub attempts: usize,
    pub seconds: f64,
    pub device: String,
    pub device_reason: Option<String>,
    pub blender_cached: bool,
    pub libs_staged: Vec<String>,
    /// ok | unavailable | prepare-failed
    pub state: String,
}

impl WorkerTally {
    fn new(host: &str) -> WorkerTally {
        WorkerTally {
            host: host.to_string(),
            cores: 1,
            frames: 0,
            units: 0,
            attempts: 0,
            seconds: 0.0,
            device: "unknown".to_string(),
            device_reason: None,
            blender_cached: false,
            libs_staged: Vec::new(),
            state: "ok".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UnitFailure {
    pub index: usize,
    pub start: u32,
    pub end: u32,
    pub host: String,
    pub attempts: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DistributedRender {
    pub clip: PathBuf,
    pub frames: usize,
    pub units_planned: usize,
    pub units_completed: usize,
    pub rendered_frames: usize,
    /// Frames that were already verified before this run started.
    pub reused_frames: usize,
    /// Collected files that were not complete PNGs and were thrown away rather than placed.
    pub rejected_frames: usize,
    pub spec_hash: String,
    pub job_dir: PathBuf,
    pub per_worker: Vec<WorkerTally>,
    pub failures: Vec<UnitFailure>,
    /// Workers that declined, with the reason (the run continues on the rest).
    pub unavailable: Vec<(String, String)>,
    pub plan_secs: f64,
    pub provision_secs: f64,
    pub render_secs: f64,
    pub reassemble_secs: f64,
    pub wall_secs: f64,
    pub media: Option<media::MediaCheck>,
    pub profile: RenderProfile,
    pub cleanup_warnings: Vec<String>,
}

impl DistributedRender {
    /// Wall clock the workers spent, without provisioning or encoding.
    pub fn render_seconds(&self) -> f64 {
        self.render_secs
    }

    pub fn frames_by_host(&self) -> Vec<(String, u32)> {
        self.per_worker
            .iter()
            .filter(|w| w.frames > 0)
            .map(|w| (w.host.clone(), w.frames))
            .collect()
    }
}

/// Everything one worker needs to render a unit of this job.
struct WorkerRuntime {
    host: String,
    blender_dir: String,
    device: gpu::DeviceChoice,
}

struct RunContext {
    spec_hash: String,
    frames_dir: PathBuf,
    base_script: String,
    profile: RenderProfile,
    retries: u32,
    ssh_timeout: u64,
    fps: u32,
    width: u32,
    height: u32,
}

struct Attempt {
    run_error: Option<String>,
    log_tail: String,
    unreachable: bool,
    rejected: usize,
    skipped: usize,
    placed: usize,
}

impl Attempt {
    fn new() -> Attempt {
        Attempt {
            run_error: None,
            log_tail: String::new(),
            unreachable: false,
            rejected: 0,
            skipped: 0,
            placed: 0,
        }
    }
}

fn unit_frames_dir(hash: &str, unit_id: &str) -> String {
    format!("{}/u-{}/frames", transport::remote_job_dir(hash), unit_id)
}

/// Sends one unit to a worker, runs it, and brings back whatever frames it produced.
///
/// A run that dies still returns its partial result: a killed Blender leaves complete frames behind,
/// and those are exactly what must survive. Only incomplete frames are rejected.
fn attempt_unit(
    rt: &WorkerRuntime,
    ctx: &RunContext,
    pending: &Pending,
) -> Attempt {
    let mut attempt = Attempt::new();
    let unit = WorkUnit {
        index: pending.index,
        start: pending.start,
        end: pending.end,
    };
    let script = match unit_script(&ctx.base_script, unit, &ctx.profile, &rt.device) {
        Ok(script) => script,
        Err(err) => {
            attempt.run_error = Some(err.message.clone());
            return attempt;
        }
    };
    let id = unit_id(&ctx.spec_hash, &unit, &ctx.profile);
    let manifest = UnitManifest {
        schema: "cinvar.blender.unit/1".to_string(),
        spec_hash: ctx.spec_hash.clone(),
        unit_id: id.clone(),
        index: unit.index,
        start: unit.start,
        end: unit.end,
        frames: unit.frames(),
        fps: ctx.fps,
        width: ctx.width,
        height: ctx.height,
        profile: ctx.profile.key(),
        device: rt.device.label(),
    };
    let manifest_json = match serde_json::to_vec_pretty(&manifest) {
        Ok(bytes) => bytes,
        Err(e) => {
            attempt.run_error = Some(format!("manifest could not be encoded: {e}"));
            return attempt;
        }
    };
    let script_name = format!("unit-{id}.py");
    let archive = match render_tar::build(&[
        TarFile::new(script_name.clone(), script.into_bytes()),
        TarFile::new("manifest.json".to_string(), manifest_json),
    ]) {
        Ok(archive) => archive,
        Err(err) => {
            attempt.run_error = Some(err.message.clone());
            return attempt;
        }
    };
    let unit_dir = format!("{}/u-{id}", transport::remote_job_dir(&ctx.spec_hash));
    if let Err(err) = transport::push_tar(&rt.host, &archive, &unit_dir, ctx.ssh_timeout) {
        attempt.run_error = Some(err.message.clone());
        attempt.unreachable = transport::reachable(&rt.host).is_err();
        return attempt;
    }

    let run = format!(
        "cd {unit_dir} && LD_LIBRARY_PATH={libs} {blender} -b --factory-startup -P {script_name} > blender.log 2>&1",
        libs = format!("{}/libs", rt.blender_dir),
        blender = format!("{}/blender", rt.blender_dir),
    );
    let (ran, stdout) = match transport::run_allow_failure(&rt.host, &run, ctx.ssh_timeout) {
        Ok(result) => result,
        Err(err) => {
            attempt.run_error = Some(err.message.clone());
            attempt.unreachable = true;
            (false, String::new())
        }
    };
    if !ran {
        attempt.unreachable = transport::reachable(&rt.host).is_err();
        let log_probe = format!("tail -n 12 {unit_dir}/blender.log");
        attempt.log_tail = transport::run_allow_failure(&rt.host, &log_probe, 60)
            .map(|(_, text)| text)
            .unwrap_or_default();
        attempt.run_error = Some(if attempt.unreachable {
            format!("worker {} stopped responding during unit {}", rt.host, pending.index)
        } else {
            format!(
                "Blender exited non-zero on unit {} ({})",
                pending.index,
                attempt.log_tail.lines().last().unwrap_or("no log").trim()
            )
        });
    } else {
        let _ = stdout;
    }

    // Collect regardless of the exit status: complete frames are the point.
    match transport::collect_dir(&rt.host, &unit_frames_dir(&ctx.spec_hash, &id), ctx.ssh_timeout) {
        Ok(files) => match place_frames(&ctx.frames_dir, &files) {
            Ok(placement) => {
                attempt.placed = placement.placed;
                attempt.rejected = placement.rejected;
                attempt.skipped = placement.skipped_verified;
            }
            Err(err) => attempt.run_error = Some(err.message.clone()),
        },
        Err(err) => {
            if attempt.run_error.is_none() {
                attempt.run_error = Some(err.message.clone());
            }
        }
    }
    attempt
}

/// Outcomes shared by the worker threads.
#[derive(Default)]
struct RunResults {
    failures: Vec<UnitFailure>,
}

/// Frames in the range that are complete on disk right now.
fn completed_between(frames_dir: &Path, start: u32, end: u32) -> Vec<u32> {
    (start..=end)
        .filter(|frame| media::png_complete(&media::frame_path(frames_dir, *frame)))
        .collect()
}

/// Reads an integer constant written by script_for().
pub fn script_constant(script: &str, name: &str) -> Option<u32> {
    script
        .lines()
        .find(|line| line.trim_start().starts_with(name))
        .and_then(|line| line.split('=').nth(1))
        .and_then(|value| value.trim().parse::<u32>().ok())
}

/// A device choice that pins Cycles to the CPU.
pub fn cpu_choice() -> gpu::DeviceChoice {
    gpu::DeviceChoice {
        backend: gpu::Backend::Cpu,
        device: None,
        preference: gpu::PREFERENCE.to_vec(),
        rejected: Vec::new(),
        alternatives: Vec::new(),
        fallback_reason: Some("CPU forced for a like-for-like comparison".to_string()),
    }
}

fn probe_workers(workers: &[WorkerSpec]) -> Vec<Result<transport::WorkerFacts>> {
    let mut handles = Vec::new();
    for worker in workers {
        let host = worker.host.clone();
        handles.push(std::thread::spawn(move || {
            transport::reachable(&host).and_then(|_| transport::facts(&host))
        }));
    }
    handles
        .into_iter()
        .map(|handle| {
            handle.join().unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorCode::Internal,
                    "worker probe thread panicked",
                ))
            })
        })
        .collect()
}

struct Prepared {
    runtime: WorkerRuntime,
    blender: transport::RemoteBlender,
}

/// Provisions one worker and asks it which render device it will use.
fn prepare_one(
    worker: &WorkerSpec,
    version: &str,
    tarball: &Path,
    timeout: u64,
) -> Result<Prepared> {
    let blender = transport::provision(&worker.host, version, tarball)?;
    let probe_tar = render_tar::build(&[TarFile::new(
        "cinvar_gpu_probe.py".to_string(),
        gpu::probe_script().into_bytes(),
    )])?;
    transport::push_tar(&worker.host, &probe_tar, &blender.dir, 120)?;
    let run = format!(
        "LD_LIBRARY_PATH={dir}/libs {dir}/blender -b --factory-startup -P {dir}/cinvar_gpu_probe.py",
        dir = blender.dir
    );
    let text = transport::run_text(&worker.host, &run, timeout.max(120))?;
    let probe = gpu::parse_probe(&text)?;
    let device = gpu::choose(&probe, None)?;
    Ok(Prepared {
        runtime: WorkerRuntime {
            host: worker.host.clone(),
            blender_dir: blender.dir.clone(),
            device,
        },
        blender,
    })
}

fn prepare_all(
    live: &[(WorkerSpec, transport::WorkerFacts)],
    version: &str,
    tarball: &Path,
    timeout: u64,
) -> Vec<Result<Prepared>> {
    let mut handles = Vec::new();
    for (worker, _) in live {
        let worker = worker.clone();
        let version = version.to_string();
        let tarball = tarball.to_path_buf();
        handles.push(std::thread::spawn(move || {
            prepare_one(&worker, &version, &tarball, timeout)
        }));
    }
    handles
        .into_iter()
        .map(|handle| {
            handle.join().unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorCode::Internal,
                    "worker preparation thread panicked",
                ))
            })
        })
        .collect()
}

/// Distributed render, gated on the Pro feature.
pub fn render(
    spec: &AnimationSpec,
    base: &Path,
    opts: &DistributedOptions,
    ctl: &RenderControl,
) -> Result<DistributedRender> {
    crate::edition::resolve_from_disk()
        .check_feature(crate::edition::Feature::BlenderDistributed)?;
    execute(spec, base, opts, ctl)
}

/// The orchestration itself, without the edition gate, for callers that have already checked it.
pub fn execute(
    spec: &AnimationSpec,
    base: &Path,
    opts: &DistributedOptions,
    ctl: &RenderControl,
) -> Result<DistributedRender> {
    let started = Instant::now();
    let base_script = blender::script_for(spec);
    let (start_frame, end_frame) = script_frame_range(&base_script)?;
    let total = (end_frame - start_frame + 1) as usize;
    let hash = blender::spec_hash(spec);
    let job_dir = blender::job_dir(base, spec);
    let frames_dir = blender::frames_dir(&job_dir);
    paths::ensure_dir(&frames_dir)?;
    let width = script_constant(&base_script, "RES_X").unwrap_or(spec.width);
    let height = script_constant(&base_script, "RES_Y").unwrap_or(spec.height);

    // Plan from what is actually on disk: resume is the default, not a special mode.
    let missing = media::missing_frames(&frames_dir, start_frame, end_frame);
    let reused_frames = total.saturating_sub(missing.len());

    // Workers: reachability and facts first, in parallel. One that declines is reported, not fatal.
    let mut tallies: Vec<WorkerTally> = Vec::new();
    let mut unavailable: Vec<(String, String)> = Vec::new();
    let mut live: Vec<(WorkerSpec, transport::WorkerFacts)> = Vec::new();
    for (worker, result) in opts.workers.iter().zip(probe_workers(&opts.workers)) {
        match result {
            Ok(facts) => live.push((worker.clone(), facts)),
            Err(err) => {
                let mut tally = WorkerTally::new(&worker.host);
                tally.state = "unavailable".to_string();
                tally.device_reason = Some(err.message.clone());
                tallies.push(tally);
                unavailable.push((worker.host.clone(), err.message.clone()));
            }
        }
    }
    if live.is_empty() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "no worker is reachable: {}",
                unavailable
                    .iter()
                    .map(|(host, reason)| format!("{host}: {reason}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
        .with_hint("check ssh -o BatchMode=yes <host> true for each worker"));
    }

    let version = match &opts.blender_version {
        Some(version) => version.clone(),
        None => blender::detect()?.version,
    };
    let tarball = transport::ensure_tarball(&version)?;
    let plan_secs = started.elapsed().as_secs_f64();
    let provision_started = Instant::now();
    let prepared = prepare_all(&live, &version, &tarball, opts.ssh_timeout_secs);
    let provision_secs = provision_started.elapsed().as_secs_f64();

    let mut runtimes: Vec<WorkerRuntime> = Vec::new();
    let mut live_tallies: Vec<WorkerTally> = Vec::new();
    for ((worker, facts), result) in live.iter().zip(prepared) {
        match result {
            Ok(prepared) => {
                let mut tally = WorkerTally::new(&worker.host);
                tally.cores = facts.cores;
                tally.device = prepared.runtime.device.label();
                tally.device_reason = prepared.runtime.device.fallback_reason.clone();
                tally.blender_cached = prepared.blender.cached;
                tally.libs_staged = prepared.blender.libs.clone();
                live_tallies.push(tally);
                runtimes.push(prepared.runtime);
            }
            Err(err) => {
                let mut tally = WorkerTally::new(&worker.host);
                tally.cores = facts.cores;
                tally.state = "prepare-failed".to_string();
                tally.device_reason = Some(err.message.clone());
                live_tallies.push(tally);
                unavailable.push((worker.host.clone(), err.message.clone()));
            }
        }
    }
    if runtimes.is_empty() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "no worker could be prepared for rendering: {}",
                unavailable
                    .iter()
                    .map(|(host, reason)| format!("{host}: {reason}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }

    // Units: deterministic partition of exactly the frames that are missing.
    let cap = if opts.max_unit_frames > 0 {
        opts.max_unit_frames
    } else {
        default_unit_frames(missing.len(), runtimes.len())
    };
    let units = partition(&missing, cap);
    let weights: Vec<u32> = runtimes
        .iter()
        .map(|rt| {
            live_tallies
                .iter()
                .find(|t| t.host == rt.host)
                .map(|t| t.cores)
                .unwrap_or(1)
        })
        .collect();
    let assignment = assign(&units, &weights);

    let ctx = RunContext {
        spec_hash: hash.clone(),
        frames_dir: frames_dir.clone(),
        base_script,
        profile: opts.profile,
        retries: opts.retries,
        ssh_timeout: opts.ssh_timeout_secs,
        fps: spec.fps.max(1),
        width,
        height,
    };
    let pool = Mutex::new(WorkPool::new(runtimes.len(), &units, &assignment));
    let results = Mutex::new(RunResults::default());
    let guard_tallies: Vec<Mutex<WorkerTally>> = live_tallies
        .iter()
        .cloned()
        .map(Mutex::new)
        .collect();
    let dead_hosts: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    let render_started = Instant::now();
    // The render control's callbacks are borrowed, not `Sync`, so they stay on this thread: workers
    // only publish progress into atomics and read a cancellation flag.
    let done = AtomicUsize::new(total - missing.len());
    let cancel = AtomicBool::new(false);
    let active = AtomicUsize::new(runtimes.len());
    let rejected = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for (index, runtime) in runtimes.iter().enumerate() {
            let pool = &pool;
            let results = &results;
            let ctx = &ctx;
            let tallies = &guard_tallies;
            let dead_hosts = &dead_hosts;
            let frames_dir = &frames_dir;
            let done = &done;
            let cancel = &cancel;
            let active = &active;
            let rejected = &rejected;
            scope.spawn(move || {
                let worker_started = Instant::now();
                let mut worker_units = 0usize;
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let pending = match pool.lock().unwrap().take(index) {
                        Some(pending) => pending,
                        None => break,
                    };
                    let before = completed_between(frames_dir, pending.start, pending.end).len();
                    let attempt = attempt_unit(runtime, ctx, &pending);
                    rejected.fetch_add(attempt.rejected, Ordering::Relaxed);
                    let after = completed_between(frames_dir, pending.start, pending.end);
                    let gained = after.len().saturating_sub(before);
                    {
                        let mut tally = tallies[index].lock().unwrap();
                        tally.attempts += 1;
                        tally.frames += gained as u32;
                    }
                    done.store(media::count_valid_frames(frames_dir), Ordering::Relaxed);

                    if after.len() as u32 == pending.frames() {
                        worker_units += 1;
                        continue;
                    }
                    let remaining: Vec<u32> = (pending.start..=pending.end)
                        .filter(|frame| !after.contains(frame))
                        .collect();
                    let next_attempt = pending.attempt + 1;
                    let can_retry = next_attempt <= ctx.retries;
                    if can_retry {
                        for piece in partition(&remaining, cap) {
                            let mut guard = pool.lock().unwrap();
                            let index = guard.next_index();
                            guard.requeue(Pending {
                                index,
                                start: piece.start,
                                end: piece.end,
                                attempt: next_attempt,
                            });
                        }
                    } else {
                        results.lock().unwrap().failures.push(UnitFailure {
                            index: pending.index,
                            start: pending.start,
                            end: pending.end,
                            host: runtime.host.clone(),
                            attempts: next_attempt,
                            reason: attempt
                                .run_error
                                .clone()
                                .unwrap_or_else(|| "frames stayed incomplete".to_string()),
                        });
                    }
                    if attempt.unreachable {
                        pool.lock().unwrap().kill(index);
                        dead_hosts.lock().unwrap().push((
                            runtime.host.clone(),
                            attempt
                                .run_error
                                .clone()
                                .unwrap_or_else(|| "worker stopped responding".to_string()),
                        ));
                        break;
                    }
                }
                let mut tally = tallies[index].lock().unwrap();
                tally.units += worker_units;
                tally.seconds += worker_started.elapsed().as_secs_f64();
                drop(tally);
                active.fetch_sub(1, Ordering::Relaxed);
            });
        }
        // This thread owns the callbacks: report progress and relay cancellation to the workers.
        while active.load(Ordering::Relaxed) > 0 {
            if (ctl.cancelled)() {
                cancel.store(true, Ordering::Relaxed);
            }
            (ctl.progress)(done.load(Ordering::Relaxed), total);
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        (ctl.progress)(done.load(Ordering::Relaxed), total);
    });
    let render_secs = render_started.elapsed().as_secs_f64();

    for (host, reason) in dead_hosts.into_inner().unwrap_or_default() {
        unavailable.push((host, reason));
    }
    if (ctl.cancelled)() {
        return Err(Error::new(
            ErrorCode::Cancelled,
            "distributed render cancelled",
        )
        .with_hint("completed frames stay on disk; re-run the same request to resume"));
    }

    let still_missing = media::missing_frames(&frames_dir, start_frame, end_frame);
    if !still_missing.is_empty() {
        let failures = results.lock().unwrap().failures.clone();
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "{} of {total} frames are still missing after the run ({} failed units): first gap at frame {}",
                still_missing.len(),
                failures.len(),
                still_missing[0]
            ),
        )
        .with_hint(format!(
            "worker output: {}",
            failures
                .iter()
                .take(3)
                .map(|f| format!("{} says {}", f.host, f.reason))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }

    // Reassemble through the same path a local render uses, then measure the result.
    let reassemble_started = Instant::now();
    let clip = blender::render_animation_job(spec, base, ctl)?;
    let media_check = media::verify_media(
        &clip.clip,
        media::MediaExpect {
            frames: total as u64,
            width,
            height,
            fps: spec.fps.max(1),
        },
    )?;
    let reassemble_secs = reassemble_started.elapsed().as_secs_f64();
    if !media_check.ok() {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "the reassembled clip does not match the request: {}",
                media_check.findings.join("; ")
            ),
        ));
    }

    // Worker scratch space is not a cache: clear it.
    let mut cleanup_warnings = Vec::new();
    if !opts.keep_remote {
        for runtime in &runtimes {
            if let Err(err) = transport::cleanup(&runtime.host, &hash) {
                cleanup_warnings.push(err.message);
            }
        }
    }

    let mut per_worker: Vec<WorkerTally> = guard_tallies
        .into_iter()
        .map(|tally| tally.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect();
    // Workers that declined are reported alongside the ones that rendered.
    per_worker.extend(tallies.drain(..));
    let completed_units = units
        .iter()
        .filter(|unit| {
            (unit.start..=unit.end)
                .all(|frame| media::png_complete(&media::frame_path(&frames_dir, frame)))
        })
        .count();

    Ok(DistributedRender {
        clip: clip.clip.clone(),
        frames: clip.frames,
        units_planned: units.len(),
        units_completed: completed_units,
        rendered_frames: total.saturating_sub(reused_frames),
        reused_frames,
        rejected_frames: rejected.load(Ordering::Relaxed),
        spec_hash: hash,
        job_dir,
        per_worker,
        failures: results.into_inner().unwrap_or_default().failures,
        unavailable,
        plan_secs,
        provision_secs,
        render_secs,
        reassemble_secs,
        wall_secs: started.elapsed().as_secs_f64(),
        media: Some(media_check),
        profile: opts.profile,
        cleanup_warnings,
    })
}

/// What a single-machine render of the same range measured.
#[derive(Debug, Clone, Serialize)]
pub struct LocalRender {
    pub frames: u32,
    pub wall_secs: f64,
    pub dir: PathBuf,
    pub device: String,
}

/// Renders the whole range on this machine with the same scene settings as a worker unit.
///
/// This exists so a distributed run can be compared with a single machine honestly: same engine,
/// same samples, same frame window, only the parallelism differs. Pass None to force the CPU.
pub fn render_local(
    spec: &AnimationSpec,
    dir: &Path,
    profile: &RenderProfile,
    device: Option<&gpu::DeviceChoice>,
) -> Result<LocalRender> {
    let info = blender::detect()?;
    let base_script = blender::script_for(spec);
    let (start_frame, end_frame) = script_frame_range(&base_script)?;
    let frames_dir = dir.join("frames");
    paths::ensure_dir(&frames_dir)?;
    let unit = WorkUnit {
        index: 0,
        start: start_frame,
        end: end_frame,
    };
    let owned;
    let choice = match device {
        Some(choice) => choice,
        None => {
            owned = cpu_choice();
            &owned
        }
    };
    let script = unit_script(&base_script, unit, profile, choice)?;
    let script_path = dir.join("local_render.py");
    std::fs::write(&script_path, script.as_bytes())
        .map_err(|e| Error::new(ErrorCode::Io, format!("writing the local script: {e}")))?;
    let log_path = dir.join("blender.log");
    let log = std::fs::File::create(&log_path)
        .map_err(|e| Error::new(ErrorCode::Io, format!("creating the render log: {e}")))?;
    let started = Instant::now();
    let status = std::process::Command::new(&info.path)
        .args(["-b", "--factory-startup", "-P"])
        .arg(&script_path)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?)
        .stderr(log)
        .status()
        .map_err(|e| Error::new(ErrorCode::Io, format!("running Blender locally: {e}")))?;
    let wall_secs = started.elapsed().as_secs_f64();
    let total = (end_frame - start_frame + 1) as u32;
    let valid = media::count_valid_frames(&frames_dir) as u32;
    if !status.success() || valid < total {
        let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
        let lines: Vec<&str> = tail.lines().collect();
        let tail = lines[lines.len().saturating_sub(8)..].join("\n");
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "local baseline produced {valid} of {total} frames (status {:?}): {tail}",
                status.code()
            ),
        ));
    }
    Ok(LocalRender {
        frames: valid,
        wall_secs,
        dir: dir.to_path_buf(),
        device: choice.label(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::time::Time;

    fn spec() -> AnimationSpec {
        AnimationSpec {
            template: "logo_reveal".to_string(),
            text: "CINVAR".to_string(),
            duration: Time::from_secs_f64(0.8),
            width: 960,
            height: 540,
            fps: 30,
            color: "#4f8cff".to_string(),
            transparent: false,
        }
    }

    fn png_bytes(body: usize, complete: bool) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend(std::iter::repeat(0x5A).take(body));
        if complete {
            bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
        }
        bytes
    }

    #[test]
    fn worker_lists_parse_leniently_and_reject_injection() {
        let workers = parse_workers("ssh://n1, ssh://n4").unwrap();
        assert_eq!(
            workers.iter().map(|w| w.host.as_str()).collect::<Vec<_>>(),
            vec!["n1", "n4"]
        );
        // Bare hosts, whitespace separating, and duplicates collapsed in order.
        let workers = parse_workers("n2 n1 n2").unwrap();
        assert_eq!(
            workers.iter().map(|w| w.host.as_str()).collect::<Vec<_>>(),
            vec!["n2", "n1"]
        );
        assert_eq!(parse_workers("ssh://ops@n1").unwrap()[0].host, "ops@n1");
        assert!(parse_workers("").is_err());
        assert!(parse_workers("ssh://").is_err());
        assert!(parse_workers("n1; reboot").is_err());
        assert!(parse_workers("n1 && curl x").is_err());
        assert!(parse_workers("ssh://n1/render").is_err(), "paths are not hosts");
    }

    #[test]
    fn partition_is_deterministic_and_covers_every_frame_once() {
        let frames: Vec<u32> = (1..=24).collect();
        let units = partition(&frames, 3);
        assert_eq!(units.len(), 8);
        assert_eq!(units[0], WorkUnit { index: 0, start: 1, end: 3 });
        assert_eq!(units[7], WorkUnit { index: 7, start: 22, end: 24 });
        let mut seen: Vec<u32> = units
            .iter()
            .flat_map(|u| u.start..=u.end)
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, frames, "every frame appears exactly once");
        assert_eq!(partition(&frames, 3), units, "same input, same units");

        // A gap (a frame that was already verified) splits the units.
        let gapped = vec![1, 2, 3, 7, 8];
        let units = partition(&gapped, 2);
        assert_eq!(
            units
                .iter()
                .map(|u| (u.start, u.end))
                .collect::<Vec<_>>(),
            vec![(1, 2), (3, 3), (7, 8)]
        );
        assert!(partition(&[], 4).is_empty());
        // A cap of zero is still a cap: one frame per unit, never an infinite loop.
        assert_eq!(partition(&[5, 6], 0).len(), 2);
    }

    #[test]
    fn unit_size_scales_with_work_and_workers() {
        assert_eq!(default_unit_frames(24, 2), 3);
        assert_eq!(default_unit_frames(100, 4), 7);
        assert_eq!(default_unit_frames(1, 8), 1);
        assert_eq!(default_unit_frames(0, 2), 1);
    }

    #[test]
    fn assignment_respects_capacity_and_covers_every_unit() {
        let units = partition(&(1..=24).collect::<Vec<u32>>(), 2);
        let assignment = assign(&units, &[4, 12]);
        let mut covered: Vec<usize> = assignment.iter().flatten().copied().collect();
        covered.sort_unstable();
        assert_eq!(covered, (0..units.len()).collect::<Vec<_>>());
        let frames = |index: usize| -> u32 {
            assignment[index]
                .iter()
                .map(|i| units[*i].frames())
                .sum()
        };
        // Balance is by load per core, so a 4-core and a 12-core worker split the frames about 1:3.
        assert_eq!(frames(0) + frames(1), 24, "every frame is assigned");
        assert!(frames(1) > frames(0), "{} vs {}", frames(1), frames(0));
        assert!(
            frames(1) <= 3 * frames(0) + 2,
            "the big worker must not be overloaded: {} vs {}",
            frames(1),
            frames(0)
        );
        assert_eq!(assignment.len(), 2);

        // Deterministic, and always the earlier worker on a tie.
        assert_eq!(assign(&units, &[4, 12]), assignment);
        let tie = assign(&units, &[1, 1]);
        assert_eq!(tie[0].len(), tie[1].len());
        assert!(assign(&units, &[]).is_empty());
    }

    #[test]
    fn frame_range_comes_from_the_generated_script() {
        let script = blender::script_for(&spec());
        let (start, end) = script_frame_range(&script).unwrap();
        assert_eq!((start, end), (1, 24));
        assert_eq!(script_constant(&script, "RES_X"), Some(960));
        assert_eq!(script_constant(&script, "RES_Y"), Some(540));
        assert!(script_frame_range("FRAME_END = 3").is_err());
    }

    #[test]
    fn unit_script_patches_one_unit_of_the_real_script() {
        let base = blender::script_for(&spec());
        let unit = WorkUnit { index: 2, start: 7, end: 9 };
        let profile = RenderProfile {
            samples: 16,
            denoise: false,
            adaptive: false,
        };
        let device = gpu::DeviceChoice {
            backend: gpu::Backend::Cuda,
            device: Some(gpu::GpuDevice {
                backend: gpu::Backend::Cuda,
                name: "Tesla P40".to_string(),
                id: "CUDA_Tesla P40_0000:03:00".to_string(),
                enabled: true,
            }),
            preference: gpu::PREFERENCE.to_vec(),
            rejected: Vec::new(),
            alternatives: Vec::new(),
            fallback_reason: None,
        };
        let patched = unit_script(&base, unit, &profile, &device).unwrap();
        // The scene keeps its full range for the animation curves, and only the window narrows.
        assert_eq!(script_frame_range(&patched).unwrap(), (1, 24));
        assert!(patched.contains("_cinvar_scene.frame_start = 7"));
        assert!(patched.contains("_cinvar_scene.frame_end = 9"));
        assert!(patched.contains("_cinvar_scene.render.engine = 'CYCLES'"));
        assert!(patched.contains("_cinvar_scene.cycles.samples = 16"));
        assert!(patched.contains("_cinvar_scene.cycles.use_adaptive_sampling = False"));
        assert!(patched.contains("_cinvar_scene.cycles.seed = 0"));
        assert!(patched.contains("_cinvar_scene.cycles.device = 'GPU'"));
        assert!(patched.contains("Tesla P40"));
        // Never overwrite, never leave a placeholder: both are stated explicitly.
        assert!(patched.contains("_cinvar_scene.render.use_overwrite = False"));
        assert!(patched.contains("_cinvar_scene.render.use_placeholder = False"));
        assert_eq!(patched.matches(RENDER_CALL).count(), 1);
        assert!(patched.contains("CINVAR_UNIT_DONE 7..9"));

        // CPU choice renders on the CPU.
        let cpu = unit_script(&base, unit, &profile, &cpu_choice()).unwrap();
        assert!(cpu.contains("_cinvar_scene.cycles.device = 'CPU'"));

        // A script that lost its marker is refused rather than silently rendered whole.
        let broken = base.replace(RENDER_CALL, "pass");
        assert_eq!(
            unit_script(&broken, unit, &profile, &device).unwrap_err().code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn unit_identity_tracks_content() {
        let profile = RenderProfile::default();
        let unit = WorkUnit { index: 1, start: 4, end: 6 };
        let id = unit_id("abc123", &unit, &profile);
        assert_eq!(id, unit_id("abc123", &unit, &profile));
        assert_ne!(id, unit_id("abc123", &WorkUnit { index: 1, start: 4, end: 7 }, &profile));
        assert_ne!(id, unit_id("abc124", &unit, &profile));
        assert_ne!(
            id,
            unit_id(
                "abc123",
                &unit,
                &RenderProfile {
                    samples: 64,
                    denoise: false,
                    adaptive: false
                }
            )
        );
        assert_ne!(
            id,
            unit_id(
                "abc123",
                &unit,
                &RenderProfile {
                    samples: 32,
                    denoise: false,
                    adaptive: true
                }
            ),
            "turning adaptive sampling on changes what a unit renders"
        );
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn frames_are_recognised_by_name_only() {
        assert_eq!(frame_from_name("frame_0007.png"), Some(7));
        assert_eq!(frame_from_name("./frame_0012.png"), Some(12));
        assert_eq!(frame_from_name("manifest.json"), None);
        assert_eq!(frame_from_name("frame_.png"), None);
        assert_eq!(frame_from_name("frame_00x1.png"), None);
        assert_eq!(frame_from_name("notaframe_0001.png"), None);
    }

    #[test]
    fn placement_never_overwrites_a_verified_frame() {
        let dir = tempfile::tempdir().unwrap();
        let frames = dir.path().join("frames");
        paths::ensure_dir(&frames).unwrap();
        // Frame 1 is already verified with known content.
        let original = png_bytes(200, true);
        std::fs::write(media::frame_path(&frames, 1), &original).unwrap();

        let files = vec![
            TarFile::new("./frame_0001.png", png_bytes(999, true)),
            TarFile::new("./frame_0002.png", png_bytes(50, true)),
            TarFile::new("./frame_0003.png", png_bytes(50, false)),
            TarFile::new("./manifest.json", b"{}".to_vec()),
        ];
        let placement = place_frames(&frames, &files).unwrap();
        assert_eq!(placement.placed, 1, "only the new complete frame is written");
        assert_eq!(placement.skipped_verified, 1);
        assert_eq!(placement.rejected, 1);
        assert_eq!(placement.frames, vec![1, 2]);
        assert_eq!(
            std::fs::read(media::frame_path(&frames, 1)).unwrap(),
            original,
            "the verified frame keeps its bytes"
        );
        assert!(media::png_complete(&media::frame_path(&frames, 2)));
        assert!(!media::png_complete(&media::frame_path(&frames, 3)));

        // An archive member that tries to escape the job directory is refused outright.
        let hostile = vec![TarFile::new("../../../etc/passwd", png_bytes(10, true))];
        assert_eq!(
            place_frames(&frames, &hostile).unwrap_err().code,
            ErrorCode::Protocol
        );
    }

    #[test]
    fn work_pool_hands_out_own_work_then_steals_then_rebalances() {
        let units = partition(&(1..=8).collect::<Vec<u32>>(), 2);
        assert_eq!(units.len(), 4);
        let assignment = vec![vec![0, 1], vec![2, 3]];
        let mut pool = WorkPool::new(2, &units, &assignment);
        assert_eq!(pool.remaining(), 4);
        // Its own queue first, in order.
        assert_eq!(pool.take(0).unwrap().index, 0);
        assert_eq!(pool.take(0).unwrap().index, 1);
        // Then it steals from the only other worker instead of idling.
        assert_eq!(pool.take(0).unwrap().index, 3);
        assert_eq!(pool.take(1).unwrap().index, 2);
        assert!(pool.take(0).is_none());
        assert!(pool.take(1).is_none());

        // A worker that dies hands its queue to the survivors.
        let mut pool = WorkPool::new(3, &units, &vec![vec![0], vec![1], vec![2, 3]]);
        pool.kill(1);
        assert!(pool.is_dead(1));
        assert!(pool.take(1).is_none());
        let mut orphans = 0;
        while pool.take(0).is_some() || pool.take(2).is_some() {
            orphans += 1;
            if orphans > 10 {
                break;
            }
        }
        assert!(orphans >= 4, "survivors pick up the dead worker's unit");
        assert_eq!(pool.remaining(), 0);
    }

    #[test]
    fn requeued_work_is_not_lost() {
        let units = partition(&[1, 2, 3], 3);
        let mut pool = WorkPool::new(1, &units, &vec![vec![0]]);
        let pending = pool.take(0).unwrap();
        pool.requeue(Pending {
            attempt: 1,
            ..pending
        });
        assert_eq!(pool.remaining(), 1);
        let again = pool.take(0).unwrap();
        assert_eq!(again.attempt, 1);
        assert_eq!(pool.next_index(), units.len());
    }

    /// The real thing: renders across the cluster named in CINVAR_DIST_WORKERS (default n1,n4),
    /// compares against a single-machine render of the same range, re-runs to prove frame reuse, and
    /// measures the delivered clip. Opt-in because it needs ssh, disks and minutes.
    ///
    /// cargo test --release --lib real_cluster -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_cluster_render_rerun_and_verify() {
        let workers = std::env::var("CINVAR_DIST_WORKERS")
            .unwrap_or_else(|_| "ssh://n1,ssh://n4".to_string());
        let root = PathBuf::from(
            std::env::var("CINVAR_DIST_ROOT").unwrap_or_else(|_| "/tmp/cinvar-dist-accept".into()),
        );
        let _ = std::fs::remove_dir_all(&root);
        paths::ensure_dir(&root).unwrap();

        let spec = spec();
        let (start, end) = script_frame_range(&blender::script_for(&spec)).unwrap();
        let total = (end - start + 1) as usize;
        println!("SPEC {total} frames {start}..{end} at {}x{} {} fps", spec.width, spec.height, spec.fps);

        // 1. Single machine, same scene and samples, CPU only: the honest baseline.
        let baseline = render_local(&spec, &root.join("baseline"), &RenderProfile::default(), None)
            .unwrap();
        println!(
            "BASELINE device={} frames={} wall={:.2}s",
            baseline.device, baseline.frames, baseline.wall_secs
        );

        // 2. Distributed.
        let opts = DistributedOptions {
            workers: parse_workers(&workers).unwrap(),
            profile: RenderProfile::default(),
            ..Default::default()
        };
        let run = execute(&spec, &root.join("dist"), &opts, &RenderControl::none()).unwrap();
        println!(
            "DISTRIBUTED frames={} rendered={} reused={} units={}/{} wall={:.2}s render={:.2}s provision={:.2}s reassemble={:.2}s",
            run.frames,
            run.rendered_frames,
            run.reused_frames,
            run.units_completed,
            run.units_planned,
            run.wall_secs,
            run.render_secs,
            run.provision_secs,
            run.reassemble_secs
        );
        for worker in &run.per_worker {
            println!(
                "WORKER {} state={} cores={} device={} frames={} units={} attempts={} active={:.2}s cached={} staged={:?}",
                worker.host,
                worker.state,
                worker.cores,
                worker.device,
                worker.frames,
                worker.units,
                worker.attempts,
                worker.seconds,
                worker.blender_cached,
                worker.libs_staged
            );
        }
        assert!(run.frames == total);
        assert_eq!(run.reused_frames, 0);
        assert!(run.media.as_ref().map(|m| m.ok()).unwrap_or(false));
        let delivered: u32 = run.per_worker.iter().map(|w| w.frames).sum();
        assert_eq!(delivered as usize, total, "every frame is accounted for by a worker");

        // Two independent renders of the same scene, on different CPUs. Byte equality is not
        // reachable: Cycles' SIMD kernels round differently on a Xeon and a Core, so every frame
        // differs by a few 8-bit steps. What has to hold is that each frame is the same image, which
        // is measured rather than assumed.
        let base_frames = root.join("baseline").join("frames");
        let dist_frames = blender::frames_dir(&run.job_dir);
        let left = media::frame_digests(&base_frames, start, end).unwrap();
        let right = media::frame_digests(&dist_frames, start, end).unwrap();
        assert_eq!(left.len(), total);
        assert_eq!(right.len(), total);
        let identical = left.iter().zip(right.iter()).filter(|(a, b)| a.1 == b.1).count();
        let psnr = psnr_between(&base_frames, &dist_frames, spec.fps);
        println!("FRAMES identical_bytes={identical}/{total} psnr_db={psnr:.2}");
        assert!(
            identical == total || psnr > 80.0,
            "distributed frames must be the same images as the single-machine render (psnr {psnr:.2})"
        );

        for (host, reason) in &run.unavailable {
            println!("UNAVAILABLE {host}: {reason}");
        }
        let speedup = baseline.wall_secs / run.render_secs;
        println!(
            "SPEEDUP render-only {:.2}x (baseline {:.2}s vs distributed {:.2}s); end-to-end {:.2}x",
            speedup,
            baseline.wall_secs,
            run.render_secs,
            baseline.wall_secs / run.wall_secs
        );

        // 3. Re-running a finished range must reuse every frame and render nothing.
        let started = Instant::now();
        let again = execute(&spec, &root.join("dist"), &opts, &RenderControl::none()).unwrap();
        let rerun_secs = started.elapsed().as_secs_f64();
        println!(
            "RERUN wall={:.2}s reused={} planned_units={} rendered={}",
            rerun_secs, again.reused_frames, again.units_planned, again.rendered_frames
        );
        assert_eq!(again.units_planned, 0, "nothing to plan when every frame is verified");
        assert_eq!(again.reused_frames, total);
        assert_eq!(again.rendered_frames, 0);
        assert!(rerun_secs < run.wall_secs, "a re-run must be cheaper than the render");
        let media = again.media.as_ref().unwrap();
        println!(
            "MEDIA codec={} {}x{} frames={} duration={:.3}s findings={:?}",
            media.codec, media.width, media.height, media.frames, media.duration, media.findings
        );

        // 4. Damage the job (drop three frames, as an interrupted run would leave it) and resume: the
        // 21 verified frames must be kept and only the gap re-rendered.
        let dist_frames_dir = blender::frames_dir(&run.job_dir);
        for frame in [5u32, 6, 7] {
            std::fs::remove_file(media::frame_path(&dist_frames_dir, frame)).unwrap();
        }
        // A half-written frame must be rejected too, not treated as done.
        std::fs::write(
            media::frame_path(&dist_frames_dir, 8),
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
        )
        .unwrap();
        let resumed = execute(&spec, &root.join("dist"), &opts, &RenderControl::none()).unwrap();
        println!(
            "RESUME reused={} rendered={} units={} rejected={} wall={:.2}s",
            resumed.reused_frames,
            resumed.rendered_frames,
            resumed.units_planned,
            resumed.rejected_frames,
            resumed.wall_secs
        );
        assert_eq!(resumed.reused_frames, total - 4, "21 verified frames are kept");
        assert_eq!(resumed.rendered_frames, 4, "only the gap is rendered");
        assert!(
            resumed.units_planned >= 1,
            "the four missing frames are planned again"
        );
        assert_eq!(
            media::missing_frames(&dist_frames_dir, start, end).len(),
            0,
            "the gap is filled again"
        );
        assert!(resumed.media.as_ref().map(|m| m.ok()).unwrap_or(false));
    }

    /// Kills a worker mid-render: the frames it had finished must survive, its incomplete frames must
    /// be rejected, and the run must still finish on the surviving worker.
    ///
    /// cargo test --release --lib real_cluster_kill -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_cluster_kill_and_recover() {
        let workers = std::env::var("CINVAR_DIST_WORKERS")
            .unwrap_or_else(|_| "ssh://n1,ssh://n4".to_string());
        let root = PathBuf::from(
            std::env::var("CINVAR_DIST_ROOT_KILL")
                .unwrap_or_else(|_| "/tmp/cinvar-dist-kill".into()),
        );
        let _ = std::fs::remove_dir_all(&root);
        paths::ensure_dir(&root).unwrap();

        let spec = spec();
        let (start, end) = script_frame_range(&blender::script_for(&spec)).unwrap();
        let total = (end - start + 1) as usize;
        let victim = parse_workers(&workers).unwrap()[0].host.clone();
        let opts = DistributedOptions {
            workers: parse_workers(&workers).unwrap(),
            profile: RenderProfile {
                samples: 48,
                denoise: false,
                adaptive: false,
            },
            max_unit_frames: 4,
            retries: 2,
            ..Default::default()
        };
        let job_dir = blender::job_dir(&root.join("dist"), &spec);
        let frames_dir = blender::frames_dir(&job_dir);

        let victim_for_thread = victim.clone();
        let frames_for_thread = frames_dir.clone();
        let monitor = std::thread::spawn(move || {
            // Let the workers get going, then kill the Blender process on the victim mid-frame.
            std::thread::sleep(std::time::Duration::from_millis(6000));
            let before = media::missing_frames(&frames_for_thread, start, end);
            let present: Vec<u32> = (start..=end).filter(|f| !before.contains(f)).collect();
            println!("PRE-KILL complete={} frames={:?}", present.len(), present);
            let killed = std::process::Command::new("ssh")
                .args(["-o", "BatchMode=yes", &victim_for_thread, "pkill -9 -f 'unit-.*[.]py'"])
                .output();
            match killed {
                Ok(out) => println!(
                    "KILL on {} status={:?} stdout={} stderr={}",
                    victim_for_thread,
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout).trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => println!("KILL could not run: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(2500));
            let after = media::missing_frames(&frames_for_thread, start, end);
            let survivors: Vec<u32> = (start..=end).filter(|f| !after.contains(f)).collect();
            println!("POST-KILL complete={} frames={:?}", survivors.len(), survivors);
            // The frames the killed worker had finished, fingerprint taken now so they can be
            // checked again once the whole run is over.
            let mut digests = Vec::new();
            for frame in &survivors {
                if let Ok(Some(digest)) = sha_of(&media::frame_path(&frames_for_thread, *frame)) {
                    digests.push((*frame, digest));
                }
            }
            digests
        });

        let run = execute(&spec, &root.join("dist"), &opts, &RenderControl::none()).unwrap();
        let pre_kill = monitor.join().unwrap();

        println!(
            "DISTRIBUTED frames={} units={}/{} wall={:.2}s render={:.2}s failures={} rejected_incomplete={} retried={}",
            run.frames,
            run.units_completed,
            run.units_planned,
            run.wall_secs,
            run.render_secs,
            run.failures.len(),
            run.rejected_frames,
            run.per_worker.iter().map(|w| w.attempts.saturating_sub(w.units)).sum::<usize>()
        );
        for worker in &run.per_worker {
            println!(
                "WORKER {} state={} frames={} units={} attempts={}",
                worker.host, worker.state, worker.frames, worker.units, worker.attempts
            );
        }
        for failure in &run.failures {
            println!("FAILURE unit={} {}..{} host={} reason={}", failure.index, failure.start, failure.end, failure.host, failure.reason);
        }
        assert!(run.wall_secs > 6.0, "the kill landed while the run was still working");
        assert!(
            !pre_kill.is_empty(),
            "the killed worker's completed frames were collected, not discarded"
        );
        // Frames that were already on disk when the worker died are still there, byte for byte: the
        // retry rendered the rest of the range and never touched a verified frame.
        for (frame, digest) in &pre_kill {
            let now = sha_of(&media::frame_path(&frames_dir, *frame))
                .unwrap()
                .unwrap_or_else(|| panic!("frame {frame} disappeared after the kill"));
            assert_eq!(&now, digest, "frame {frame} survived the kill unchanged");
        }
        // Nothing incomplete was allowed to survive into the job directory.
        assert_eq!(
            media::missing_frames(&frames_dir, start, end).len(),
            0,
            "every frame in the range is a complete PNG"
        );
        let strays = std::fs::read_dir(&frames_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".incoming-"))
            .count();
        assert_eq!(strays, 0, "no half-written temp frames are left behind");
        assert_eq!(run.frames, total, "the run finished after the kill");
        assert!(run.failures.is_empty(), "recovery needed no give-up: {:?}", run.failures);
        assert!(run.media.as_ref().map(|m| m.ok()).unwrap_or(false));
    }

    /// Mean PSNR between two PNG sequences, from ffmpeg itself rather than from a hand-rolled
    /// comparison: infinity when the images are identical.
    fn psnr_between(a: &Path, b: &Path, fps: u32) -> f64 {
        let pattern = |dir: &Path| dir.join("frame_%04d.png").to_string_lossy().to_string();
        let out = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-framerate", &fps.to_string()])
            .args(["-start_number", "1", "-i", &pattern(a)])
            .args(["-framerate", &fps.to_string()])
            .args(["-start_number", "1", "-i", &pattern(b)])
            .args(["-lavfi", "[0:v][1:v]psnr", "-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        let text = String::from_utf8_lossy(&out.stderr).to_string();
        let marker = text.rfind("average:").expect("ffmpeg reports psnr");
        let value = &text[marker + "average:".len()..];
        let number: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == 'i' || *c == 'n' || *c == 'f')
            .collect();
        if number.starts_with("inf") {
            f64::INFINITY
        } else {
            number.parse().unwrap_or(0.0)
        }
    }

    fn sha_of(path: &Path) -> Result<Option<String>> {
        use sha2::{Digest, Sha256};
        let Ok(bytes) = std::fs::read(path) else {
            return Ok(None);
        };
        if !media::png_complete_bytes(&bytes) {
            return Ok(None);
        }
        Ok(Some(
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        ))
    }
}
