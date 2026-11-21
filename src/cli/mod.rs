use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::config::{Config, ModelProfile};
use crate::error::{Error, ErrorCode, Result};
use crate::jobs::Jobs;
use crate::project::history::History;
use crate::project::model::Project;
use crate::project::store::ProjectStore;
use crate::util::{paths, proc as shproc, text};

#[derive(Parser, Debug)]
#[command(
    name = "cinvar",
    version,
    about = "CINVAR — the open-source AI video creation harness",
    long_about = "CINVAR gives an AI model professional video editing capabilities: import media, build a timeline, render real videos, inspect the result and revise it.\n\nDOWNLOAD. CONNECT. CREATE."
)]
struct Cli {
    #[arg(long, global = true, help = "Machine readable JSON output")]
    json: bool,
    #[arg(
        long,
        global = true,
        help = "Run a Pro-only operation for real, but only the first 10 seconds and with the CINVAR PRO PREVIEW notice burned in (Free edition only; ignored when licensed)"
    )]
    pro_preview: bool,
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help = "Project directory (defaults to the current directory)"
    )]
    project: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help = "Also let AI agents read media from this folder (repeatable). By default agents see only the project folder"
    )]
    media_root: Vec<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum CloudCmd {
    #[command(
        about = "Validate a RunPod API key (read from stdin, never from an argument) and store it privately"
    )]
    Connect,
    #[command(
        about = "Show whether a key is configured and whether RunPod accepts it (read-only call)"
    )]
    Status,
    #[command(about = "Forget the stored RunPod key")]
    Disconnect,
    #[command(
        about = "Review and approve a cloud-render quote an agent requested (interactive terminal only). Without an id, lists pending quotes"
    )]
    Approve {
        #[arg(help = "Quote id shown by the agent's cloud-render request")]
        quote_id: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Check and install dependencies and write a starter configuration")]
    Setup {
        #[arg(long, help = "Accept defaults without prompting")]
        yes: bool,
        #[arg(
            long,
            help = "Install and configure the Pro AI stack (WhisperX, faster-whisper, Real-ESRGAN, Demucs) into managed virtualenvs"
        )]
        pro_ai: bool,
    },
    #[command(
        about = "Connect an AI assistant (Claude Code) to a CINVAR project: creates the project, imports footage, writes the MCP config"
    )]
    Connect {
        #[arg(help = "Assistant to connect: claude")]
        client: String,
        #[arg(long, help = "Project folder (created if missing; default ./my-video)")]
        project: Option<PathBuf>,
        #[arg(long, help = "Folder of footage to import into the project")]
        footage: Option<PathBuf>,
        #[arg(
            long,
            help = "Also run `claude mcp add` for this project (changes your Claude Code settings)"
        )]
        apply: bool,
    },
    #[command(
        about = "Report the environment: ffmpeg, hardware encoders, fonts, Blender, model profiles"
    )]
    Doctor,
    #[command(
        about = "Run the render worker (used inside RunPod pods or on your own render machine)"
    )]
    Worker {
        #[arg(long, default_value = "0.0.0.0:8000", help = "Address to listen on")]
        bind: String,
        #[arg(long, help = "Working directory for uploads and renders")]
        root: PathBuf,
        #[arg(
            long,
            help = "Stop after this many seconds (default: CINVAR_WORKER_MAX_RUNTIME_SECS or unlimited)"
        )]
        max_runtime_secs: Option<u64>,
    },
    #[command(
        about = "Connect and inspect an optional RunPod account for cloud rendering (never bills by itself)"
    )]
    Cloud {
        #[command(subcommand)]
        cmd: CloudCmd,
    },
    #[command(about = "Manage model profiles")]
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    #[command(about = "Create, inspect and open projects")]
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    #[command(about = "Run one natural-language editing instruction to completion")]
    Run {
        #[arg(help = "What you want made, in plain language")]
        request: String,
        #[arg(long, help = "Model profile to use (default: active profile)")]
        model: Option<String>,
        #[arg(
            long,
            help = "Show the planned operations without changing the project"
        )]
        dry_run: bool,
        #[arg(long, help = "Maximum model turns for this request")]
        max_iterations: Option<u32>,
    },
    #[command(about = "Conversational editing session")]
    Chat {
        #[arg(long)]
        model: Option<String>,
    },
    #[command(about = "Render the timeline")]
    Render {
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        #[arg(long, help = "Fast low-resolution preview")]
        preview: bool,
        #[arg(long)]
        width: Option<u32>,
        #[arg(long)]
        height: Option<u32>,
        #[arg(long)]
        crf: Option<u8>,
        #[arg(long)]
        codec: Option<String>,
        #[arg(long, help = "Render only this range, e.g. 00:00:05-00:00:15")]
        range: Option<String>,
        #[arg(long, help = "Block until the render finishes")]
        wait: bool,
    },
    #[command(about = "Build a semantic scene index for footage in a folder (local vision model when available)")]
    Analyze {
        #[arg(long, help = "Folder of raw footage")]
        media_root: PathBuf,
        #[arg(long, help = "Project folder whose cache stores the scene index")]
        project: Option<PathBuf>,
        #[arg(long, default_value = "gemma3:4b", help = "Ollama vision model name")]
        model: String,
        #[arg(long, default_value = "http://localhost:11434", help = "Ollama base URL")]
        ollama_url: String,
    },
    #[command(about = "Search the project's scene index with free text")]
    Scenes {
        #[arg(help = "What to look for, e.g. \"someone celebrating\"")]
        query: String,
    },
    #[command(about = "Detect scenes in one video with PySceneDetect (needs CINVAR_SCENEDETECT_PYTHON venv)")]
    ScenesDetect {
        #[arg(value_name = "VIDEO")]
        video: PathBuf,
        #[arg(long, default_value = "content", help = "content or adaptive detector")]
        detector: String,
        #[arg(long, help = "Detector threshold (default: detector default)")]
        threshold: Option<f64>,
    },
    #[command(
        about = "Autonomous highlight reel: analyse footage in a folder, pick the best moments, build a timeline, render, inspect and correct"
    )]
    AutoEdit {
        #[arg(long, help = "Folder of raw footage")]
        media_root: PathBuf,
        #[arg(long, default_value_t = 60.0, help = "Target runtime in seconds")]
        target_secs: f64,
        #[arg(long, help = "Vertical 1080x1920 canvas for Shorts/Reels")]
        vertical: bool,
        #[arg(long, help = "Opening title overlay text")]
        title: Option<String>,
        #[arg(long, default_value_t = 2.0, help = "Shortest clip to select (seconds)")]
        min_clip: f64,
        #[arg(long, default_value_t = 6.0, help = "Longest clip to select (seconds)")]
        max_clip: f64,
        #[arg(long, value_name = "FILE", help = "Output file (default: project renders dir)")]
        output: Option<PathBuf>,
        #[arg(long, help = "Creative instruction, e.g. \"the loudest celebrations\" (needs a scene index from cinvar analyze)")]
        instruction: Option<String>,
        #[arg(long, help = "Transcribe speech and burn captions")]
        captions: bool,
        #[arg(long, help = "Remove dead air inside selected clips via Auto-Editor (needs CINVAR_AUTOEDITOR_PYTHON or auto-editor on PATH)")]
        silence_removal: bool,
        #[arg(long, default_value = "social", help = "Caption preset: subtitles, social, documentary, cinematic")]
        caption_preset: String,
        #[arg(long, value_name = "ENGINE", help = "Caption engine: auto (default) or whisperx (needs CINVAR_WHISPERX_PYTHON)")]
        engine: Option<String>,
        #[arg(long, default_value = "srt", value_name = "srt|ass", help = "Caption burn-in: srt (default, drawtext) or ass (libass karaoke track)")]
        style: String,
        #[arg(long, help = "Highlight each word as it is spoken (needs --style ass and --engine whisperx)")]
        word_highlight: bool,
        #[arg(long, default_value_t = 2, help = "Max inspect/correct/render revisions")]
        max_revisions: u32,
        #[arg(long, help = "Subject-aware reframe of the final render (FastERRCNN tracking; needs CINVAR_TRACK_PYTHON)")]
        reframe: bool,
    },
    #[command(about = "Audio source separation and cleanup (Demucs; needs CINVAR_DEMUCS_PYTHON venv)")]
    Audio {
        #[command(subcommand)]
        cmd: AudioCmd,
    },
    #[command(about = "Show project status, jobs and validation issues")]
    Status,
    #[command(about = "Validate a rendered file against the project (ffprobe checks, no model)")]
    Qa {
        #[arg(help = "Rendered file to validate")]
        file: PathBuf,
    },
    #[command(about = "Inspect render jobs")]
    Jobs {
        #[command(subcommand)]
        cmd: JobsCmd,
    },
    #[command(about = "Media analysis helpers")]
    Media {
        #[command(subcommand)]
        cmd: MediaCmd,
    },
    #[command(about = "Upscale a video with Real-ESRGAN (needs CINVAR_ESRGAN_PYTHON venv)")]
    Enhance {
        #[arg(value_name = "VIDEO")]
        input: PathBuf,
        #[arg(long, default_value_t = 4, help = "Upscale factor (2 or 4)")]
        upscale: u32,
        #[arg(value_name = "FILE", help = "Output file (default: <input>.upscaled.mp4)")]
        output: Option<PathBuf>,
        #[arg(long, help = "ncnn tile size (e.g. 256) for VRAM safety; omit for auto")]
        tile: Option<u32>,
    },
    #[command(about = "Subject-aware reframe to vertical/square (needs CINVAR_TRACK_PYTHON venv with FastERRCNN weights cached)")]
    Reframe {
        #[arg(value_name = "VIDEO")]
        input: PathBuf,
        #[arg(value_name = "FILE", help = "Output file (default: <input>.reframed.mp4)")]
        output: Option<PathBuf>,
        #[arg(long, default_value = "vertical", help = "Target aspect: vertical (9:16), square (1:1), landscape (16:9)")]
        target: String,
        #[arg(long, default_value_t = 4.0, help = "Frame sampling rate for tracking (fps); 4 fps recommended — higher sampling beats smoothing")]
        sample_fps: f64,
    },
    #[command(about = "List and read creative skills")]
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    #[command(about = "Show the active edition (Free/Pro), its limits and license state")]
    Edition {
        #[command(subcommand)]
        cmd: EditionCmd,
    },
    #[command(about = "Call any CINVAR tool directly (the same validated tools agents use)")]
    Tool {
        #[command(subcommand)]
        cmd: ToolCmd,
    },
    #[command(about = "List installed plugins")]
    Plugins {
        #[command(subcommand)]
        cmd: PluginsCmd,
    },
    #[command(about = "Serve the MCP interface for external agents")]
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    #[command(about = "Serve the local web UI")]
    Ui {
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    #[command(about = "Timeline interchange")]
    Otio {
        #[command(subcommand)]
        cmd: OtioCmd,
    },
    #[command(about = "Blender: render animations locally or across worker machines, and report render devices")]
    Blender {
        #[command(subcommand)]
        cmd: BlenderCmd,
    },
    #[command(about = "Measure startup and render timings on this machine")]
    Bench,
    #[command(
        about = "Turn one long video into finished vertical Shorts (highlight selection, subject-tracked 9:16 reframe, word-highlighted captions, QA)"
    )]
    Shorts {
        #[arg(value_name = "INPUT", help = "Long-form source video")]
        input: PathBuf,
        #[arg(long, default_value_t = 5, help = "How many Shorts to produce")]
        count: u32,
        #[arg(
            long,
            default_value = "30-60",
            help = "Per-Short duration bounds in seconds: 30-60, or a single number"
        )]
        duration: String,
        #[arg(long, value_name = "DIR", help = "Output directory (default: <project>/shorts)")]
        output: Option<PathBuf>,
        #[arg(
            long,
            value_name = "WORDS",
            help = "Comma-separated custom vocabulary (brand names, technical terms) for transcription"
        )]
        vocab: Option<String>,
        #[arg(long, value_name = "FILE", help = "Also write the JSON report here")]
        report: Option<PathBuf>,
    },
    #[command(about = "Activate CINVAR Pro from an order key or a license file")]
    Activate {
        #[arg(
            value_name = "ORDER-KEY-OR-FILE",
            help = "Order key from the checkout email, or the path to a license .json file"
        )]
        key: Option<String>,
        #[arg(long, value_name = "FILE", help = "Import a license file explicitly")]
        file: Option<PathBuf>,
        #[arg(
            long,
            value_name = "URL",
            help = "License redemption service for an order key (overrides [edition] redeem_url and CINVAR_LICENSE_REDEEM_URL)"
        )]
        redeem_url: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ModelsCmd {
    List,
    Add {
        name: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key_env: Option<String>,
        #[arg(long)]
        context_window: Option<u32>,
    },
    Use {
        name: String,
    },
    Test {
        name: Option<String>,
    },
    #[command(
        about = "Store the API key for a profile in the local credential store (read from stdin)"
    )]
    Key {
        name: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum ProjectCmd {
    Create {
        #[arg(long, default_value = "Cinvar Project")]
        name: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    List,
    Show,
    Open,
    #[command(about = "Relink assets that moved on disk")]
    Relink,
    #[command(about = "List saved revisions of this project")]
    History,
    #[command(about = "Restore a previously saved revision")]
    Rollback {
        #[arg(long, help = "Revision number, as listed by project history")]
        revision: u64,
    },
}

#[derive(Subcommand, Debug)]
enum JobsCmd {
    List,
    Status { job_id: String },
    Cancel { job_id: String },
}

#[derive(Subcommand, Debug)]
enum MediaCmd {
    Probe {
        path: PathBuf,
    },
    Import {
        path: PathBuf,
    },
    Frames {
        path: PathBuf,
        #[arg(long, default_value = "0")]
        at: String,
        #[arg(long, default_value_t = 480)]
        width: u32,
    },
    Scenes {
        path: PathBuf,
        #[arg(long, default_value_t = 0.35)]
        threshold: f32,
    },
}

#[derive(Subcommand, Debug)]
enum SkillsCmd {
    List,
    Show { name: String },
}

#[derive(Subcommand, Debug)]
enum ToolCmd {
    #[command(about = "List available tool names (edition-specific tools appear only when present)")]
    List,
    #[command(about = "Call a tool with JSON arguments; --wait blocks until background jobs finish")]
    Call {
        name: String,
        #[arg(long, default_value = "{}", help = "JSON object of arguments")]
        args: String,
        #[arg(long, help = "Wait for any background jobs the call started")]
        wait: bool,
    },
}

#[derive(Subcommand, Debug)]
enum EditionCmd {
    #[command(about = "Print edition, enforcement mode, export limit and license state")]
    Status,
    #[command(about = "Install a CINVAR Pro license file (verified offline; no account or network)")]
    Activate {
        #[arg(long, help = "Path to the license.json you received")]
        file: PathBuf,
    },
    #[command(about = "Remove the installed license and return to Free")]
    Deactivate,
}

#[derive(Subcommand, Debug)]
enum PluginsCmd {
    List,
}

#[derive(Subcommand, Debug)]
enum McpCmd {
    Serve,
    /// Read-only Project Inspector: exactly project.status, qa.project_check, render.preflight.
    /// Ignores CINVAR_MCP_PERMISSIONS. Packaged as the Claude Desktop extension.
    Inspector,
}

#[derive(Subcommand, Debug)]
enum OtioCmd {
    Export {
        #[arg(long, default_value = "timeline.otio")]
        output: PathBuf,
    },
}

fn out(json_mode: bool, value: serde_json::Value, human: impl FnOnce() -> String) -> Result<()> {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{}", human());
    }
    Ok(())
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn resolve_project(explicit: Option<&Path>) -> Result<ProjectStore> {
    if let Some(dir) = explicit {
        return ProjectStore::open(&paths::absolutize(dir, &cwd()));
    }
    let here = cwd();
    if here.join("project.json").exists() {
        return ProjectStore::open(&here);
    }
    Err(Error::not_found("no CINVAR project in this directory")
        .with_hint("run: cinvar project create --name MyProject, or pass --project <dir>"))
}

fn resolve_or_create_project(
    explicit: Option<&Path>,
    name_hint: &str,
) -> Result<(ProjectStore, Project)> {
    match resolve_project(explicit) {
        Ok(store) => {
            let project = store.load()?;
            Ok((store, project))
        }
        Err(_) => {
            let (store, project) = ProjectStore::create(&cwd(), name_hint)?;
            Ok((store, project))
        }
    }
}

pub fn doctor_report() -> serde_json::Value {
    let ffmpeg = shproc::which("ffmpeg");
    let ffprobe = shproc::which("ffprobe");
    let mut encoders = Vec::new();
    let mut ffmpeg_version = String::new();
    if let Some(ffmpeg_path) = &ffmpeg {
        if let Ok(out) = shproc::run(
            ffmpeg_path,
            &["-hide_banner".into(), "-encoders".into()],
            &shproc::ProcOpts::default().with_timeout(20),
        ) {
            for line in out.stdout.lines() {
                for name in [
                    "libx264",
                    "libx265",
                    "libvpx-vp9",
                    "h264_nvenc",
                    "hevc_nvenc",
                    "h264_vaapi",
                    "aac",
                    "libopus",
                ] {
                    if line.contains(name) {
                        encoders.push(name.to_string());
                    }
                }
            }
        }
        if let Ok(out) = shproc::run(
            ffmpeg_path,
            &["-hide_banner".into(), "-version".into()],
            &shproc::ProcOpts::default().with_timeout(20),
        ) {
            ffmpeg_version = out.stdout.lines().next().unwrap_or("").to_string();
        }
    }
    let drawtext = ffmpeg.as_ref().map(|f| {
        shproc::run(
            f,
            &["-hide_banner".into(), "-filters".into()],
            &shproc::ProcOpts::default().with_timeout(20),
        )
        .map(|o| o.stdout.contains(" drawtext "))
        .unwrap_or(false)
    });
    let font_count = shproc::which("fc-list")
        .and_then(|fc| shproc::run(&fc, &[], &shproc::ProcOpts::default().with_timeout(15)).ok())
        .map(|out| out.stdout.lines().count())
        .unwrap_or(0);
    let config = Config::load().unwrap_or_default();
    json!({
        "cinvar_version": crate::VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "ffmpeg": ffmpeg.map(|p| p.to_string_lossy().to_string()),
        "ffprobe": ffprobe.map(|p| p.to_string_lossy().to_string()),
        "ffmpeg_version": ffmpeg_version,
        "encoders": encoders,
        "hardware_encoding": !encoders.iter().all(|e| !e.contains("nvenc") && !e.contains("vaapi")),
        "fonts": font_count,
        "drawtext_filter": drawtext,
        "blender": crate::blender::available().map(|p| p.to_string_lossy().to_string()),
        "python": shproc::which("python3").map(|p| p.to_string_lossy().to_string()),
        "config_file": Config::path().to_string_lossy().to_string(),
        "project": resolve_project(None).ok().map(|s| s.root.to_string_lossy().to_string()),
        "models": config.models.keys().cloned().collect::<Vec<_>>(),
        "active_model": config.active_model,
        "tool_count": crate::tools::build_default().len(),
    })
}

pub fn run() -> i32 {
    let cli = Cli::parse();
    if !cli.media_root.is_empty() {
        // Process-local and set by the human launching this command; agents cannot change it.
        let mut roots: Vec<PathBuf> = std::env::var_os("CINVAR_MEDIA_ROOTS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        roots.extend(cli.media_root.iter().map(|p| paths::absolutize(p, &cwd())));
        if let Ok(joined) = std::env::join_paths(roots) {
            std::env::set_var("CINVAR_MEDIA_ROOTS", joined);
        }
    }
    match dispatch(&cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {}", crate::secrets::redact(&err.to_string()));
            match err.code {
                ErrorCode::PermissionDenied => 3,
                ErrorCode::Budget => 4,
                ErrorCode::Cancelled => 5,
                _ => 1,
            }
        }
    }
}

fn dispatch(cli: &Cli) -> Result<()> {
    let json_mode = cli.json;
    // Propagated like --json: every gate that has a preview surface reads it.
    let pro_preview = cli.pro_preview;
    match &cli.command {
        Commands::Blender { cmd } => cmd_blender(cmd, json_mode),
        Commands::Setup { yes, pro_ai } => cmd_setup(*yes, *pro_ai, json_mode),
        Commands::Shorts {
            input,
            count,
            duration,
            output,
            vocab,
            report,
        } => cmd_shorts(
            input,
            *count,
            duration,
            output.as_deref(),
            vocab.as_deref(),
            report.as_deref(),
            json_mode,
        ),
        Commands::Activate {
            key,
            file,
            redeem_url,
        } => cmd_activate(
            key.as_deref(),
            file.as_deref(),
            redeem_url.as_deref(),
            json_mode,
        ),
        Commands::Connect {
            client,
            project,
            footage,
            apply,
        } => cmd_connect(
            client,
            project.as_deref(),
            footage.as_deref(),
            *apply,
            json_mode,
        ),
        Commands::Worker {
            bind,
            root,
            max_runtime_secs,
        } => {
            let token = std::env::var("CINVAR_WORKER_TOKEN")
                .map_err(|_| Error::invalid("CINVAR_WORKER_TOKEN must be set (16+ characters)"))?;
            let max = max_runtime_secs.or_else(|| {
                std::env::var("CINVAR_WORKER_MAX_RUNTIME_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            });
            let handle = crate::cloud::worker::start(crate::cloud::worker::WorkerConfig {
                bind: bind.clone(),
                root: root.clone(),
                token,
                max_runtime: max.map(std::time::Duration::from_secs),
            })?;
            eprintln!("cinvar worker listening on {}", handle.url());
            // Serve until the runtime limit stops the loop (the process then exits, which
            // stops the container), or until the process is killed.
            handle.wait();
            Ok(())
        }
        Commands::Cloud { cmd } => {
            let base = std::env::var("CINVAR_RUNPOD_BASE")
                .unwrap_or_else(|_| crate::cloud::runpod::DEFAULT_BASE.into());
            match cmd {
                CloudCmd::Connect => {
                    let mut line = String::new();
                    eprintln!("Paste your RunPod API key, then press Enter (it is not echoed back or logged):");
                    std::io::stdin().read_line(&mut line)?;
                    let key = line.trim().to_string();
                    if key.is_empty() {
                        return Err(Error::invalid("no key provided"));
                    }
                    crate::cloud::runpod::RunpodClient::new(&base, &key)
                        .validate_credentials()
                        .map_err(Error::from)?;
                    let mut store = crate::secrets::CredentialStore::load()?;
                    store.set_key("runpod", &key)?;
                    out(
                        json_mode,
                        serde_json::json!({"connected": true, "stored_in": crate::secrets::CredentialStore::path().to_string_lossy(), "note": "Nothing was provisioned or billed. Enable security.allow_cloud_render to render in the cloud."}),
                        || {
                            "RunPod key accepted and stored privately. Nothing was provisioned or billed.".to_string()
                        },
                    )?;
                    Ok(())
                }
                CloudCmd::Status => {
                    let value = match crate::secrets::runpod_key() {
                        None => {
                            serde_json::json!({"configured": false, "hint": "run: cinvar cloud connect"})
                        }
                        Some(key) => match crate::cloud::runpod::RunpodClient::new(&base, &key)
                            .validate_credentials()
                        {
                            Ok(()) => {
                                serde_json::json!({"configured": true, "accepted_by_runpod": true})
                            }
                            Err(e) => {
                                serde_json::json!({"configured": true, "accepted_by_runpod": false, "reason": e.message})
                            }
                        },
                    };
                    out(json_mode, value.clone(), || value.to_string())?;
                    Ok(())
                }
                CloudCmd::Approve { quote_id } => {
                    use crate::cloud::approval;
                    let Some(id) = quote_id else {
                        let pending = approval::list_pending();
                        let value = serde_json::json!({"pending": pending.iter().map(|p| serde_json::json!({
                            "quote_id": p.quote.id, "project": p.project, "image": p.quote.image_name,
                            "worst_case_usd": p.quote.worst_case_usd})).collect::<Vec<_>>()});
                        return out(json_mode, value, || {
                            if pending.is_empty() {
                                "No pending cloud quotes.".to_string()
                            } else {
                                pending
                                    .iter()
                                    .map(|p| {
                                        format!(
                                            "{}  worst case ${:.2}  image {}  project {}",
                                            p.quote.id,
                                            p.quote.worst_case_usd,
                                            p.quote.image_name,
                                            p.project
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            }
                        });
                    };
                    let pending = approval::load_pending(id)?;
                    let q = &pending.quote;
                    // A human decision needs a human at a terminal: refuse pipes and scripts.
                    if !(std::io::IsTerminal::is_terminal(&std::io::stdin())
                        && std::io::IsTerminal::is_terminal(&std::io::stderr()))
                    {
                        return Err(Error::new(
                            crate::error::ErrorCode::PermissionDenied,
                            "approving a billable render needs an interactive terminal",
                        ));
                    }
                    eprintln!("CLOUD RENDER QUOTE {}", q.id);
                    eprintln!("  project:            {}", pending.project);
                    eprintln!("  container image:    {}   <- the agent chose this; your project files are sent to it", q.image_name);
                    eprintln!("  GPU candidates:     {}", q.gpu_candidates.join(", "));
                    eprintln!("  price ceiling:      ${:.2}/hour", q.max_price_per_hr);
                    eprintln!("  runtime limit:      {} minutes", q.max_runtime_secs / 60);
                    eprintln!("  WORST CASE:         ${:.2} (not a prediction; CINVAR cannot cap RunPod charges)", q.worst_case_usd);
                    eprintln!("  upload:             {} bytes", q.upload_bytes_total);
                    for line in &q.disclosure {
                        eprintln!("  - {line}");
                    }
                    eprint!("Type APPROVE to allow this render (anything else cancels): ");
                    let mut answer = String::new();
                    std::io::stdin().read_line(&mut answer)?;
                    if answer.trim() != "APPROVE" {
                        return Err(Error::new(
                            crate::error::ErrorCode::Cancelled,
                            "not approved; nothing was started",
                        ));
                    }
                    approval::approve(id)?;
                    out(
                        json_mode,
                        serde_json::json!({"approved": id, "valid_minutes": approval::TTL_SECS / 60}),
                        || {
                            format!(
                                "Approved for {} minutes. The agent can now confirm the quote.",
                                approval::TTL_SECS / 60
                            )
                        },
                    )?;
                    Ok(())
                }
                CloudCmd::Disconnect => {
                    let mut store = crate::secrets::CredentialStore::load()?;
                    store.keys.remove("runpod");
                    store.save()?;
                    out(json_mode, serde_json::json!({"disconnected": true}), || {
                        "RunPod key removed.".to_string()
                    })?;
                    Ok(())
                }
            }
        }
        Commands::Doctor => {
            let report = doctor_report();
            out(json_mode, report.clone(), || {
                let mut text = String::new();
                text.push_str(&format!("CINVAR {}\n", crate::VERSION));
                text.push_str(&format!(
                    "  os            {} {}\n",
                    report["os"].as_str().unwrap_or("?"),
                    report["arch"].as_str().unwrap_or("?")
                ));
                text.push_str(&format!("  cpus          {}\n", report["cpus"]));
                text.push_str(&format!(
                    "  ffmpeg        {}\n",
                    report["ffmpeg"].as_str().unwrap_or("NOT FOUND")
                ));
                text.push_str(&format!(
                    "  ffprobe       {}\n",
                    report["ffprobe"].as_str().unwrap_or("NOT FOUND")
                ));
                text.push_str(&format!(
                    "  encoders      {}\n",
                    report["encoders"].as_array().map(|a| a.len()).unwrap_or(0)
                ));
                text.push_str(&format!("  fonts         {}\n", report["fonts"]));
                if report["drawtext_filter"] == false {
                    text.push_str("  drawtext      MISSING - titles/captions cannot render; install a full FFmpeg build (libfreetype)\n");
                }
                text.push_str(&format!(
                    "  blender       {}\n",
                    report["blender"]
                        .as_str()
                        .unwrap_or("not installed (optional)")
                ));
                text.push_str(&format!(
                    "  models        {}\n",
                    report["models"].as_array().map(|a| a.len()).unwrap_or(0)
                ));
                text.push_str(&format!("  tools         {}\n", report["tool_count"]));
                text
            })
        }
        Commands::Models { cmd } => cmd_models(cmd, json_mode),
        Commands::Project { cmd } => cmd_project(cmd, cli.project.as_deref(), json_mode),
        Commands::Run {
            request,
            model,
            dry_run,
            max_iterations,
        } => cmd_run(
            request,
            model.as_deref(),
            *dry_run,
            *max_iterations,
            cli.project.as_deref(),
            json_mode,
        ),
        Commands::Chat { model } => cmd_chat(model.as_deref(), cli.project.as_deref()),
        Commands::Render {
            output,
            preview,
            width,
            height,
            crf,
            codec,
            range,
            wait,
        } => cmd_render(
            cli.project.as_deref(),
            output.clone(),
            *preview,
            *width,
            *height,
            *crf,
            codec.clone(),
            range.clone(),
            *wait,
            json_mode,
            pro_preview,
        ),
        Commands::Analyze {
            media_root,
            project,
            model,
            ollama_url,
        } => {
            // Enforced before the index is built: a Free project never gets a partial index.
            let clips = crate::tools::count_indexable_clips(media_root);
            crate::edition::resolve_from_disk().check_semantic_clips(clips)?;
            let dir = match project {
                Some(dir) => {
                    let abs = paths::absolutize(&dir, &cwd());
                    if abs.join("project.json").exists() {
                        ProjectStore::open(&abs)?
                    } else {
                        // Customer pointed at their own directory (possibly
                        // created by `setup --project`): initialize IN it, do
                        // not silently invent a sibling `name-2`.
                        let name = abs
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Semantic Index".to_string());
                        ProjectStore::create_at(&abs, &name)?.0
                    }
                }
                None => resolve_project(cli.project.as_deref())?,
            };
            cmd_analyze(&media_root, &dir, &model, &ollama_url, json_mode)
        }
        Commands::Scenes { query } => cmd_scenes_search(&query, cli.project.as_deref(), json_mode),
        Commands::ScenesDetect { video, detector, threshold } => {
            cmd_scenes_detect(&video, &detector, *threshold, json_mode)
        }
        Commands::AutoEdit {
            media_root,
            target_secs,
            vertical,
            title,
            min_clip,
            max_clip,
            output,
            instruction,
            captions,
            silence_removal,
            caption_preset,
            engine,
            style,
            word_highlight,
            max_revisions,
            reframe,
        } => cmd_autoedit(
            &media_root,
            *target_secs,
            *vertical,
            title.as_deref(),
            *min_clip,
            *max_clip,
            output.clone(),
            instruction.as_deref(),
            *captions,
            *silence_removal,
            &caption_preset,
            engine.as_deref(),
            style,
            *word_highlight,
            *max_revisions,
            *reframe,
            cli.project.as_deref(),
            json_mode,
        ),
        Commands::Status => cmd_status(cli.project.as_deref(), json_mode),
        Commands::Qa { file } => {
            let store = resolve_project(cli.project.as_deref())?;
            let project = store.load()?;
            let path = paths::absolutize(file, &cwd());
            let report = crate::qa::validate_render(&project, &store, &path)?;
            let verdict = if report.passed { "passed" } else { "failed" };
            out(json_mode, json!(report), || {
                let mut text = format!("{}: {}\n", path.display(), verdict);
                for check in &report.checks {
                    text.push_str(&format!(
                        "  {:<14} {:<5} {}\n",
                        check.name,
                        if check.passed { "pass" } else { "FAIL" },
                        check.detail
                    ));
                }
                if let Some(score) = report.score {
                    text.push_str(&format!("  score          {score:.3}\n"));
                }
                text
            })
        }
        Commands::Jobs { cmd } => cmd_jobs(cmd, json_mode),
        Commands::Media { cmd } => cmd_media(cmd, json_mode),
        Commands::Enhance { input, upscale, output, tile } => {
            cmd_enhance(&input, *upscale, output.as_deref(), *tile, json_mode, pro_preview)
        }
        Commands::Reframe { input, output, target, sample_fps } => {
            // Subject-tracking reframing is a Pro capability (the Free path is a static crop).
            crate::edition::resolve_from_disk()
                .check_feature(crate::edition::Feature::ReframeTracking)?;
            cmd_reframe(&input, output.as_deref(), target, *sample_fps, json_mode)
        }
        Commands::Audio { cmd } => {
            // Demucs stem separation and cleaned mixes are a Pro capability.
            crate::edition::resolve_from_disk().check_feature(crate::edition::Feature::AudioStems)?;
            cmd_audio(cmd, json_mode)
        }
        Commands::Skills { cmd } => cmd_skills(cmd, json_mode),
        Commands::Edition { cmd } => match cmd {
            EditionCmd::Activate { file } => {
                let lic = crate::edition::license::install(
                    &file,
                    &crate::edition::license::default_path(),
                    crate::edition::license::production_keys(),
                )?;
                out(
                    json_mode,
                    serde_json::json!({"activated": true, "license_id": lic.id, "grandfathered": lic.grandfathered}),
                    || format!("CINVAR Pro license {} installed", lic.id),
                )
            }
            EditionCmd::Deactivate => {
                let removed = crate::edition::license::remove(&crate::edition::license::default_path())?;
                out(json_mode, serde_json::json!({"removed": removed}), || {
                    if removed { "license removed; running as CINVAR Free".to_string() } else { "no license installed".to_string() }
                })
            }
            EditionCmd::Status => {
                let config = crate::config::Config::load().unwrap_or_default();
                let ent = crate::edition::resolve(&config);
                let summary = ent.summary();
                out(json_mode, summary.clone(), || {
                    let limit = ent.export_limit();
                    // Report what the gates will actually do, not whether the config file happens
                    // to set [edition] enforce: a release build enforces without that flag.
                    let enforcement = match ent.enforcement {
                        crate::edition::Enforcement::Enforce => "enforce",
                        crate::edition::Enforcement::Observe => "observe (no gate refuses)",
                    };
                    let mut text = format!(
                        "edition: {}\nenforcement: {enforcement}\nexport limit: {}\nlicense: {}",
                        ent.edition.as_str(),
                        limit,
                        serde_json::to_string(&ent.source).unwrap_or_default()
                    );
                    if config.edition.enforce {
                        text.push_str("\nnote: [edition] enforce = true in the config tightens the default");
                    }
                    if ent.edition == crate::edition::Edition::Free {
                        text.push_str(&format!(
                            "\nupgrade: {} (or try any Pro capability once with --pro-preview)",
                            crate::edition::UPGRADE_URL
                        ));
                    }
                    text
                })
            }
        },
        Commands::Tool { cmd } => match cmd {
            ToolCmd::List => {
                let registry = crate::tools::build_with_plugins(&crate::config::Config::load().unwrap_or_default());
                let names: Vec<String> = registry.specs().into_iter().map(|s| s.name).collect();
                out(json_mode, json!({"tools": names}), || names.join("\n"))
            }
            ToolCmd::Call { name, args, wait } => {
                let config = crate::config::Config::load().unwrap_or_default();
                let registry = crate::tools::build_with_plugins(&config);
                let store = resolve_project(cli.project.as_deref())?;
                let mut project = store.load()?;
                let jobs = crate::jobs::Jobs::new();
                let mut history = crate::project::history::History::default();
                let parsed: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| Error::invalid(format!("--args is not valid JSON: {e}")))?;
                let mut ctx = crate::tools::ToolCtx {
                    project: &mut project,
                    store: &store,
                    config: &config,
                    jobs: &jobs,
                    history: &mut history,
                    dry_run: false,
                };
                let value = registry.call(name, &mut ctx, parsed)?;
                let mut failed = false;
                if *wait {
                    for job in jobs.list() {
                        let done = jobs.wait(&job.id, std::time::Duration::from_secs(24 * 3600));
                        failed |= done.map(|j| j.status != crate::jobs::JobStatus::Succeeded).unwrap_or(true);
                    }
                }
                out(json_mode, value.clone(), || serde_json::to_string_pretty(&value).unwrap_or_default())?;
                if failed {
                    return Err(Error::new(ErrorCode::Render, "a background job did not succeed"));
                }
                Ok(())
            }
        },
        Commands::Plugins { cmd } => cmd_plugins(cmd, json_mode),
        Commands::Mcp { cmd } => match cmd {
            McpCmd::Serve => {
                let mut server = crate::mcp::McpServer::new(
                    crate::tools::build_with_plugins(
                        &crate::config::Config::load().unwrap_or_default(),
                    ),
                    cli.project.clone(),
                );
                server.serve_stdio()
            }
            McpCmd::Inspector => {
                let mut server = crate::mcp::McpServer::inspector(
                    crate::tools::build_default(),
                    cli.project.clone(),
                );
                server.serve_stdio()
            }
        },
        Commands::Ui { port } => {
            let config = Config::load()?;
            let port = if *port == 0 { config.ui.port } else { *port };
            crate::server::serve(&config, port, cli.project.clone())
        }
        Commands::Otio { cmd } => match cmd {
            OtioCmd::Export { output } => {
                let store = resolve_project(cli.project.as_deref())?;
                let project = store.load()?;
                let out_path = paths::absolutize(output, &cwd());
                let grant = crate::edition::resolve_from_disk().gate_feature(
                    crate::edition::Feature::OtioExport,
                    pro_preview,
                )?;
                let report = crate::otio::export_otio(&project, None, &out_path)?;
                // A timeline file is an asset, not a video: the preview marker is a sidecar.
                let marker = if grant.is_preview() {
                    Some(crate::render::preview::write_marker(&out_path)?)
                } else {
                    None
                };
                let marker_path = marker.as_ref().map(|m| m.to_string_lossy().to_string());
                // The report keeps its shape: the preview only adds fields to it.
                let mut payload = serde_json::to_value(&report)?;
                payload["pro_preview"] = json!(grant.is_preview());
                payload["marker"] = json!(marker_path);
                out(
                    json_mode,
                    payload,
                    || match &marker {
                        Some(m) => format!(
                            "wrote {} (PRO PREVIEW - see {})",
                            out_path.display(),
                            m.display()
                        ),
                        None => format!("wrote {}", out_path.display()),
                    },
                )
            }
        },
        Commands::Bench => cmd_bench(),
    }
}

fn cmd_connect(
    client: &str,
    project: Option<&Path>,
    footage: Option<&Path>,
    apply: bool,
    json_mode: bool,
) -> Result<()> {
    if !client.eq_ignore_ascii_case("claude") {
        return Err(Error::invalid(format!(
            "'{client}' is not a verified integration; only 'claude' is"
        ))
        .with_hint("other MCP-capable assistants can use the printed server command directly"));
    }
    let report = doctor_report();
    if report["ffmpeg"].is_null() || report["ffprobe"].is_null() {
        return Err(Error::new(ErrorCode::DependencyMissing, "FFmpeg and ffprobe are required before connecting an assistant")
            .with_hint("install FFmpeg (apt install ffmpeg / brew install ffmpeg / winget install Gyan.FFmpeg), then run: cinvar doctor"));
    }
    let dir = paths::absolutize(project.unwrap_or_else(|| Path::new("my-video")), &cwd());
    let store = if dir.join("project.json").exists() {
        ProjectStore::open(&dir)?
    } else {
        let parent = dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(cwd);
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-video")
            .to_string();
        let (store, _) = ProjectStore::create(&parent, &name)?;
        store
    };
    let mut imported = 0usize;
    let mut skipped = 0usize;
    if let Some(folder) = footage {
        let folder = paths::absolutize(folder, &cwd());
        if !folder.is_dir() {
            return Err(Error::not_found(format!(
                "{} is not a folder",
                folder.display()
            )));
        }
        let _lock = store.lock(std::time::Duration::from_secs(20))?;
        let mut project = store.load()?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&folder)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for path in entries {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(
                ext.as_str(),
                "mp4"
                    | "mov"
                    | "mkv"
                    | "webm"
                    | "avi"
                    | "m4v"
                    | "wav"
                    | "mp3"
                    | "m4a"
                    | "flac"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
            ) {
                continue;
            }
            match store.import_asset(&mut project, &path, "connect") {
                Ok(_) => imported += 1,
                Err(_) => skipped += 1,
            }
        }
        store.save(&mut project)?;
    }
    let binary = std::env::current_exe()
        .map_err(|e| Error::io(format!("cannot locate the cinvar binary: {e}")))?;
    let server = json!({"command": binary.to_string_lossy(), "args": ["mcp", "serve", "--project", store.root.to_string_lossy()]});
    // Project-scoped MCP config: Claude Code started in this folder finds it; nothing global is touched.
    let mcp_json = json!({"mcpServers": {"cinvar": server}});
    paths::write_atomic(
        &store.root.join(".mcp.json"),
        serde_json::to_string_pretty(&mcp_json)?.as_bytes(),
    )?;
    let guide = store.root.join("CLAUDE.md");
    if !guide.exists() {
        paths::write_atomic(&guide, b"# CINVAR video project\n\nThis folder is a CINVAR project served to you through the `cinvar` MCP server.\nEdit video only through those tools. Start with cinvar.project.status, load a creative skill\n(cinvar.skills.list / load) for the style requested, look at footage (contact sheets, frames) before\ncutting, preview, inspect rendered frames, then export. Revise the existing timeline; use\ncinvar.project.history / rollback instead of starting over.\n")?;
    }
    let mut applied = serde_json::Value::Null;
    if apply {
        let result = std::process::Command::new("claude")
            .args(["mcp", "add", "cinvar", "--"])
            .arg(&binary)
            .args(["mcp", "serve", "--project"])
            .arg(&store.root)
            .current_dir(&store.root)
            .output();
        applied = match result {
            Ok(o) if o.status.success() => {
                json!("registered with Claude Code for this project folder")
            }
            Ok(o) => json!(format!(
                "claude mcp add failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => json!(format!("could not run claude: {e}")),
        };
    }
    let root = store.root.to_string_lossy().to_string();
    out(
        json_mode,
        json!({"project": root, "imported": imported, "skipped": skipped, "mcp_config": store.root.join(".mcp.json").to_string_lossy(), "applied": applied}),
        || {
            format!(
                "CINVAR is ready for Claude Code.\n\n  project   {root}\n  footage   {imported} file(s) imported{}\n  config    {root}/.mcp.json (project-scoped; your global Claude settings were not changed{})\n\nNext:\n  cd {root}\n  claude\n  (approve the 'cinvar' MCP server when Claude Code asks)\n\nThen ask, for example:\n  \"Make a 30 second cinematic trailer from my footage.\"",
                if skipped > 0 { format!(", {skipped} skipped") } else { String::new() },
                if apply { "; --apply also registered it with `claude mcp add`" } else { "" }
            )
        },
    )
}

fn cmd_setup(yes: bool, pro_ai: bool, json_mode: bool) -> Result<()> {
    if pro_ai {
        let report = crate::setup::pro_ai(json_mode)?;
        return out(
            json_mode,
            serde_json::to_value(&report).unwrap_or(serde_json::Value::Null),
            || {
                let mut text = String::from("CINVAR Pro AI setup\n\n");
                for c in &report.components {
                    text.push_str(&format!(
                        "  {:<14} {:<8} {}\n",
                        c.component,
                        if c.installed { "ready" } else { "FAILED" },
                        if c.problem.is_empty() { c.venv.clone() } else { c.problem.clone() }
                    ));
                }
                text.push_str(&format!("\n  config  {}\n", report.config_path));
                text.push_str(if report.complete {
                    "\nAll Pro AI components are configured. No environment variables needed.\n"
                } else {
                    "\nSome components failed; fix the problems above and re-run setup --pro-ai.\n"
                });
                text
            },
        );
    }
    let report = doctor_report();
    let mut config = Config::load().unwrap_or_default();
    if config.models.is_empty() {
        config.upsert_profile(
            "mock",
            ModelProfile {
                provider: "mock".into(),
                model: "mock-planner".into(),
                label: Some("Offline demo planner".into()),
                ..Default::default()
            },
        );
        config.active_model = Some("mock".into());
        config.save()?;
    }
    paths::ensure_dir(&paths::data_dir())?;
    let missing: Vec<&str> = ["ffmpeg", "ffprobe"]
        .iter()
        .copied()
        .filter(|name| report[*name].is_null())
        .collect();
    let guidance = if missing.is_empty() {
        "All required media tools are installed.".to_string()
    } else {
        format!(
            "Missing: {}. Install FFmpeg:\n  Debian/Ubuntu: sudo apt install ffmpeg\n  Fedora: sudo dnf install ffmpeg\n  macOS: brew install ffmpeg\n  Windows: winget install Gyan.FFmpeg",
            missing.join(", ")
        )
    };
    // Never block a script or CI job waiting for Enter: prompt only on an interactive terminal.
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if !yes && interactive {
        println!("CINVAR setup\n");
        println!("{guidance}");
        println!("\nOptional: Blender for 3D animation -> https://www.blender.org/download/");
        println!("Optional: Ollama for fully local models -> https://ollama.com/download");
        print!("\nPress Enter to continue (Ctrl-C to abort)... ");
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
    }
    out(
        json_mode,
        json!({"doctor": report, "guidance": guidance, "config": Config::path().to_string_lossy()}),
        || {
            format!("\n{guidance}\n\nConfiguration written to {}\nNext: cinvar models add local --provider ollama --model qwen2.5:7b (or keep the offline mock planner)\nThen: cinvar project create --name MyVideo && cinvar run \"make a 30 second teaser\"", Config::path().display())
        },
    )
}

fn cmd_models(cmd: &ModelsCmd, json_mode: bool) -> Result<()> {
    let mut config = Config::load()?;
    match cmd {
        ModelsCmd::List => out(
            json_mode,
            json!({"active": config.active_model, "models": config.models}),
            || {
                if config.models.is_empty() {
                    return "no model profiles yet. Add one:\n  cinvar models add local --provider ollama --model qwen2.5:7b".to_string();
                }
                let mut text = String::new();
                for (name, profile) in &config.models {
                    let marker = if config.active_model.as_deref() == Some(name.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    text.push_str(&format!(
                        "{marker} {name:<14} {:<20} {}\n",
                        profile.provider, profile.model
                    ));
                }
                text
            },
        ),
        ModelsCmd::Add {
            name,
            provider,
            model,
            base_url,
            api_key_env,
            context_window,
        } => {
            let profile = ModelProfile {
                provider: provider.clone(),
                model: model.clone(),
                base_url: base_url.clone(),
                api_key_env: api_key_env.clone(),
                context_window: context_window.unwrap_or(128_000),
                ..Default::default()
            };
            config.upsert_profile(name, profile);
            config.save()?;
            out(json_mode, json!({"ok": true, "profile": name}), || {
                format!("added model profile '{name}' ({provider}/{model})")
            })
        }
        ModelsCmd::Use { name } => {
            config.profile(name)?;
            config.active_model = Some(name.clone());
            config.save()?;
            out(json_mode, json!({"active": name}), || {
                format!("active model profile: {name}")
            })
        }
        ModelsCmd::Test { name } => {
            let profile_name = name
                .clone()
                .or_else(|| config.active_model.clone())
                .ok_or_else(|| Error::invalid("no model profile selected"))?;
            let profile = config.profile(&profile_name)?.clone();
            let provider = crate::providers::build(&profile_name, &profile, &config)?;
            provider.health()?;
            out(
                json_mode,
                json!({"ok": true, "profile": profile_name, "capabilities": provider.capabilities()}),
                || format!("{} is reachable ({})", profile_name, profile.model),
            )
        }
        ModelsCmd::Key { name } => {
            config.profile(name)?;
            eprint!("paste the API key for '{name}' then press Enter: ");
            std::io::stderr().flush().ok();
            let mut key = String::new();
            std::io::stdin().read_line(&mut key)?;
            let key = key.trim().to_string();
            if key.is_empty() {
                return Err(Error::invalid("no key provided"));
            }
            let mut store = crate::secrets::CredentialStore::load()?;
            store.set_key(name, &key)?;
            out(json_mode, json!({"ok": true}), || {
                format!(
                    "stored credential for '{name}' in {}",
                    crate::secrets::CredentialStore::path().display()
                )
            })
        }
        ModelsCmd::Remove { name } => {
            config.models.remove(name);
            if config.active_model.as_deref() == Some(name.as_str()) {
                config.active_model = config.models.keys().next().cloned();
            }
            config.save()?;
            out(json_mode, json!({"ok": true}), || {
                format!("removed model profile '{name}'")
            })
        }
    }
}

fn cmd_project(cmd: &ProjectCmd, explicit: Option<&Path>, json_mode: bool) -> Result<()> {
    match cmd {
        ProjectCmd::Create { name, dir } => {
            let parent = paths::absolutize(dir, &cwd());
            let (store, project) = ProjectStore::create(&parent, name)?;
            if let Ok(db) = crate::db::Db::open(&crate::db::Db::default_path()) {
                let _ =
                    db.upsert_project(&project.id, &store.root.to_string_lossy(), &project.name);
            }
            out(
                json_mode,
                json!({"path": store.root.to_string_lossy(), "id": project.id, "name": project.name}),
                || {
                    format!(
                        "created project '{}' at {}\nnext: cd {} && cinvar run \"...\"",
                        project.name,
                        store.root.display(),
                        store.root.display()
                    )
                },
            )
        }
        ProjectCmd::List => {
            let db = crate::db::Db::open(&crate::db::Db::default_path())?;
            let rows = db.projects()?;
            out(json_mode, json!({"projects": rows}), || {
                if rows.is_empty() {
                    "no projects registered yet".to_string()
                } else {
                    rows.iter()
                        .map(|(_, path, name, updated)| format!("{name:<24} {path}  ({updated})"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            })
        }
        ProjectCmd::Show => {
            let store = resolve_project(explicit)?;
            let project = store.load()?;
            let summary = json!({
                "path": store.root.to_string_lossy(),
                "id": project.id,
                "name": project.name,
                "revision": project.revision,
                "assets": project.assets.iter().map(|a| json!({"id": a.id, "name": a.display_name(), "missing": a.missing})).collect::<Vec<_>>(),
                "timelines": project.timelines.iter().map(|t| json!({"id": t.id, "name": t.name, "duration": t.duration().as_secs_f64(), "tracks": t.tracks.len()})).collect::<Vec<_>>(),
                "issues": crate::project::ops::validate(&project),
            });
            out(json_mode, summary.clone(), || {
                format!("{} ({})\n  path        {}\n  revision    {}\n  assets      {}\n  timelines   {}\n  issues      {}", project.name, project.id, store.root.display(), project.revision, summary["assets"].as_array().map(|a| a.len()).unwrap_or(0), summary["timelines"].as_array().map(|a| a.len()).unwrap_or(0), summary["issues"].as_array().map(|a| a.len()).unwrap_or(0))
            })
        }
        ProjectCmd::Open => {
            let store = resolve_project(explicit)?;
            let project = store.load()?;
            out(
                json_mode,
                json!({"path": store.root.to_string_lossy(), "id": project.id}),
                || format!("project '{}' at {}", project.name, store.root.display()),
            )
        }
        ProjectCmd::History => {
            let store = resolve_project(explicit)?;
            let revisions = store.revisions()?;
            out(json_mode, json!({"revisions": revisions}), || {
                if revisions.is_empty() {
                    "no revisions recorded yet".to_string()
                } else {
                    revisions
                        .iter()
                        .map(|r| {
                            format!(
                                "{:>5}  {}  {:>8} bytes  {}",
                                r.revision, r.at, r.bytes, r.label
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(
                            "
",
                        )
                }
            })
        }
        ProjectCmd::Rollback { revision } => {
            let store = resolve_project(explicit)?;
            let mut project = store.load()?;
            store.rollback(&mut project, *revision)?;
            out(
                json_mode,
                json!({"ok": true, "revision": project.revision}),
                || {
                    format!(
                        "restored revision {revision}; project is now at revision {}",
                        project.revision
                    )
                },
            )
        }
        ProjectCmd::Relink => {
            let store = resolve_project(explicit)?;
            let mut project = store.load()?;
            let changed = store.refresh_missing(&mut project);
            store.save(&mut project)?;
            out(json_mode, json!({"updated": changed}), || {
                format!("{changed} asset reference(s) updated")
            })
        }
    }
}

fn start_session(store: &ProjectStore, project: &mut Project, config: &Config, jobs: &Jobs) {
    if let Ok(db) = crate::db::Db::open(&crate::db::Db::default_path()) {
        let _ = db.upsert_project(&project.id, &store.root.to_string_lossy(), &project.name);
    }
    let _ = config;
    let _ = jobs;
}

/// A render started by a tool keeps running on its own thread, but the process must not
/// exit while it is still encoding: wait for outstanding jobs and report how they ended.
fn wait_for_outstanding_jobs(jobs: &Jobs) {
    let pending: Vec<String> = jobs
        .list()
        .into_iter()
        .filter(|job| !job.status.is_terminal())
        .map(|job| job.id)
        .collect();
    for id in pending {
        eprintln!("waiting for background job {id} (Ctrl-C aborts the job by exiting)");
        if let Some(snapshot) = jobs.wait(&id, std::time::Duration::from_secs(7_200)) {
            match snapshot.status {
                crate::jobs::JobStatus::Succeeded => {
                    let detail = snapshot
                        .output
                        .as_ref()
                        .and_then(|value| {
                            value
                                .get("output")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    eprintln!("job {id} finished: {detail}");
                }
                crate::jobs::JobStatus::Failed => {
                    let message = snapshot
                        .error
                        .map(|err| err.to_string())
                        .unwrap_or_else(|| "unknown failure".into());
                    eprintln!("job {id} failed: {}", crate::secrets::redact(&message));
                }
                other => eprintln!("job {id} ended as {}", other.as_str()),
            }
        }
    }
}

use crate::util::time::Time;
use crate::project::ops as ops_mod;

fn render_now(
    store: &ProjectStore,
    project: &Project,
    output_path: PathBuf,
) -> Result<serde_json::Value> {
    let mut request = crate::render::RenderRequest::for_output(output_path.clone());
    let jobs = Jobs::new();
    let store_for_job = store.clone();
    let project_for_job = project.clone();
    let job_id = jobs.spawn("render", move |job| {
        let progress = |p: f32, message: &str| job.progress(p, message);
        let cancelled = || job.cancelled();
        crate::render::render(&project_for_job, &store_for_job, &request, &progress, &cancelled)
            .map(|outcome| json!(outcome))
    });
    let snapshot = jobs
        .wait(&job_id, std::time::Duration::from_secs(7_200))
        .ok_or_else(|| Error::internal("render job disappeared"))?;
    match snapshot.status {
        crate::jobs::JobStatus::Succeeded => Ok(snapshot.output.unwrap_or_else(|| json!({}))),
        _ => {
            let message = snapshot
                .error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "render failed".into());
            Err(Error::new(ErrorCode::Render, crate::secrets::redact(&message)))
        }
    }
}

fn cmd_analyze(
    media_root: &Path,
    store: &ProjectStore,
    model: &str,
    ollama_url: &str,
    json_mode: bool,
) -> Result<()> {
    let opts = crate::analysis::semantic::IndexOptions {
        ollama_url: ollama_url.to_string(),
        model: model.to_string(),
    };
    let index = crate::analysis::semantic::index_media(media_root, &opts)?;
    let path = store.cache_dir().join("semantic-index.json");
    index.save(&path)?;
    let vision_note = if index.vision_available {
        format!("described by local vision model {}", index.model)
    } else {
        "no vision model reachable; deterministic descriptors only (NOT model understanding)".to_string()
    };
    out(json_mode, json!({
        "index": path.display().to_string(),
        "segments": index.segments.len(),
        "vision_available": index.vision_available,
        "model": index.model,
        "sources": index.segments.iter().map(|s| s.source.clone()).collect::<std::collections::BTreeSet<_>>(),
    }), || {
        let mut text = format!(
            "indexed {} segment(s) from {} - {}\nindex: {}\n",
            index.segments.len(),
            media_root.display(),
            vision_note,
            path.display()
        );
        for seg in index.segments.iter().take(8) {
            text.push_str(&format!(
                "  {:.1}-{:.1}s {} [{}] {}\n",
                seg.start, seg.end, seg.file_name, seg.source, seg.description
            ));
        }
        if index.segments.len() > 8 {
            text.push_str(&format!("  ... {} more\n", index.segments.len() - 8));
        }
        text
    });
    Ok(())
}

fn cmd_scenes_detect(
    video: &Path,
    detector: &str,
    threshold: Option<f64>,
    json_mode: bool,
) -> Result<()> {
    let detector = crate::analysis::pyscenedetect::Detector::parse(detector)?;
    let work_dir = std::env::temp_dir().join("cinvar-scenedetect");
    let report = crate::analysis::pyscenedetect::detect(video, detector, threshold, &work_dir)?;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "scenes: {} ({}, fps {:.2}, {}x{})",
            report.scenes.len(),
            report.detector,
            report.fps,
            report.width,
            report.height
        );
        for s in &report.scenes {
            println!("{:>8.2}s - {:>8.2}s  (frames {}-{})", s.start_sec, s.end_sec, s.start_frame, s.end_frame);
        }
    }
    Ok(())
}

fn cmd_scenes_search(
    query: &str,
    explicit: Option<&Path>,
    json_mode: bool,
) -> Result<()> {
    let store = resolve_project(explicit)?;
    let index = crate::analysis::semantic::SceneIndex::load(&store.cache_dir().join("semantic-index.json"))?;
    let hits = crate::analysis::semantic::search(&index, query);
    if hits.is_empty() {
        return Err(Error::invalid(format!(
            "no indexed footage matches \"{query}\""
        ))
        .with_hint("index more footage with cinvar analyze, or rephrase the query"));
    }
    let payload: Vec<serde_json::Value> = hits
        .iter()
        .take(20)
        .map(|(i, score)| {
            let seg = &index.segments[*i];
            json!({
                "segment_id": seg.id,
                "file": seg.file,
                "start": seg.start,
                "end": seg.end,
                "score": score,
                "description": seg.description,
                "subjects": seg.subjects,
                "source": seg.source,
            })
        })
        .collect();
    out(json_mode, json!(payload), || {
        let mut text = String::new();
        for (i, score) in hits.iter().take(10) {
            let seg = &index.segments[*i];
            text.push_str(&format!(
                "{score:.2}  {} {:.1}-{:.1}s [{}] {}\n",
                seg.file_name, seg.start, seg.end, seg.source, seg.description
            ));
        }
        text
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_autoedit(
    media_root: &Path,
    target_secs: f64,
    vertical: bool,
    title: Option<&str>,
    min_clip: f64,
    max_clip: f64,
    output: Option<PathBuf>,
    instruction: Option<&str>,
    captions: bool,
    silence_removal: bool,
    caption_preset: &str,
    engine: Option<&str>,
    style: &str,
    word_highlight: bool,
    max_revisions: u32,
    reframe: bool,
    explicit: Option<&Path>,
    json_mode: bool,
) -> Result<()> {
    if !matches!(style, "srt" | "ass") {
        return Err(crate::error::Error::invalid("--style must be srt or ass").into());
    }
    if style == "ass" && !captions {
        return Err(crate::error::Error::invalid("--style ass needs --captions").into());
    }
    // Pro caption/reframe capabilities, refused before any analysis work starts.
    let entitlements = crate::edition::resolve_from_disk();
    if matches!(engine, Some("whisperx")) {
        entitlements.check_feature(crate::edition::Feature::CaptionsWordLevel)?;
    }
    if style == "ass" || word_highlight {
        entitlements.check_feature(crate::edition::Feature::CaptionsKaraoke)?;
    }
    if reframe {
        entitlements.check_feature(crate::edition::Feature::ReframeTracking)?;
    }
    let opts = crate::analysis::autoedit::AutoEditOptions {
        target_secs: target_secs.max(1.0),
        min_clip: min_clip.max(0.5),
        max_clip: max_clip.max(min_clip),
        vertical,
        title: title.map(|t| t.to_string()),
        instruction: instruction.map(|t| t.to_string()),
        captions,
        silence_removal,
    };
    let name_hint = media_root
        .file_name()
        .map(|n| format!("AutoEdit {}", n.to_string_lossy()))
        .unwrap_or_else(|| "AutoEdit Reel".to_string());
    let (store, mut project) = match explicit {
        Some(dir) => {
            let abs = paths::absolutize(dir, &cwd());
            if abs.join("project.json").exists() {
                let store = ProjectStore::open(&abs)?;
                let project = store.load()?;
                (store, project)
            } else {
                let name = abs
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| name_hint.clone());
                if abs.exists() {
                    // Customer pointed at their own directory (possibly created
                    // by `setup --project`): initialize IN it, do not silently
                    // invent a sibling `name-2`.
                    ProjectStore::create_at(&abs, &name)?
                } else {
                    ProjectStore::create(&abs.parent().unwrap_or(&abs), &name)?
                }
            }
        }
        None => resolve_or_create_project(None, &name_hint)?,
    };

    let (plan, clip_ids) = crate::analysis::autoedit::auto_edit(&store, &mut project, media_root, &opts)?;
    let mut captions_outcome: Option<crate::analysis::autoedit::CaptionOutcome> = None;
    if captions {
        captions_outcome =
            Some(crate::analysis::autoedit::generate_captions_with(
                &store,
                &mut project,
                &clip_ids,
                None,
                caption_preset,
                vertical,
                engine.unwrap_or("auto"),
            )?);
        store.save(&mut project)?;
    }
    let revisions_root = store.renders_dir();

    let output_path = match output {
        Some(path) => paths::absolutize(&path, &cwd()),
        None => revisions_root.join(format!(
            "{}-autoedit-r{}.mp4",
            crate::util::text::slugify(&project.name),
            project.revision
        )),
    };

    let mut corrections: Vec<String> = Vec::new();
    let mut render_passes = 0u32;
    let mut report;
    let mut current_output = output_path.clone();
    // --style ass: drawtext captions are suppressed and the libass track is burned after render.
    let ass_mode = style == "ass";
    let ass_style = crate::captions::ass::AssStyle {
        word_highlight,
        ..Default::default()
    };
    let ass_lines = captions_outcome.as_ref().map(|c| c.lines.clone()).unwrap_or_default();
    loop {
        render_passes += 1;
        eprintln!("render pass {render_passes}: {}", current_output.display());
        if ass_mode {
            let saved = std::mem::take(&mut project.captions);
            let rendered = render_now(&store, &project, current_output.clone());
            project.captions = saved;
            rendered?;
            crate::captions::ass::burn_track(
                &ass_lines,
                plan.width as u32,
                plan.height as u32,
                &ass_style,
                &current_output,
            )?;
        } else {
            render_now(&store, &project, current_output.clone())?;
        }
        report = crate::qa::validate_render(&project, &store, &current_output)?;
        if report.passed || render_passes > max_revisions {
            break;
        }
        // Plan one correction round from the failing checks, then re-render.
        let mut changed = false;
        for check in &report.checks {
            if check.passed {
                continue;
            }
            match check.name.as_str() {
                "black_flash" => {
                    // A clip ends a frame early: shave a hair off its source out
                    // point so the boundary never exposes the black frame.
                    let flash = check
                        .detail
                        .split("at ")
                        .nth(1)
                        .and_then(|rest| rest.split(&['s', ',', ' '][..]).next())
                        .and_then(|tok| tok.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    if let Some(timeline) = project.timeline() {
                        let victim = timeline
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| c.asset.is_some())
                            .find(|c| {
                                let start = c.timeline_start.as_secs_f64();
                                let end = start + c.duration().as_secs_f64();
                                flash >= start - 0.05 && flash <= end
                            })
                            .map(|c| c.id.clone());
                        if let Some(clip_id) = victim {
                            let (new_out, new_dur) = {
                                let clip = project.clip(&clip_id).unwrap();
                                (
                                    clip.source_out - Time::from_secs_f64(0.05),
                                    clip.duration().as_secs_f64(),
                                )
                            };
                            if new_dur > 0.5 {
                                ops_mod::trim_clip(&mut project, &clip_id, None, Some(new_out))?;
                                corrections.push(format!(
                                    "black_flash at {flash:.2}s: trimmed {clip_id} out point by 0.05 s"
                                ));
                                changed = true;
                            }
                        }
                    }
                }
                "audio_levels" => {
                    if check.detail.contains("clipping") || check.detail.contains("max 0") {
                        for clip_id in clip_ids.iter() {
                            let _ = ops_mod::set_clip_audio(
                                &mut project,
                                clip_id,
                                Some(-6.0),
                                None,
                                None,
                                None,
                                None,
                                None,
                            );
                        }
                        corrections.push("audio clipping: applied -6 dB to every reel clip".to_string());
                    } else {
                        for clip_id in clip_ids.iter() {
                            let _ = ops_mod::set_clip_audio(
                                &mut project,
                                clip_id,
                                Some(6.0),
                                None,
                                None,
                                None,
                                None,
                                None,
                            );
                        }
                        corrections.push("audio too quiet: applied +6 dB to every reel clip".to_string());
                    }
                    changed = true;
                }
                "frozen_frames" => {
                    // Frozen footage inside a reel clip: skip the frozen span.
                    // "0.57-6.00s" style detail gives the span; if it starts at
                    // the clip head advance the in point (ripple), if it ends
                    // the clip trim the out point, otherwise split trim both.
                    let span = check
                        .detail
                        .split("at ")
                        .nth(1)
                        .map(|rest| rest.split(' ').next().map(|s| s.to_string()))
                        .flatten();
                    let (frozen_start, frozen_end) = span
                        .map(|s| {
                            let mut it = s.trim_end_matches('s').split('-');
                            (
                                it.next().and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0),
                                it.next().and_then(|t| t.parse::<f64>().ok()),
                            )
                        })
                        .unwrap_or((0.0, None));
                    if let Some(timeline) = project.timeline() {
                        let victims: Vec<(String, f64, f64, Time)> = timeline
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| c.asset.is_some())
                            .filter(|c| {
                                let start = c.timeline_start.as_secs_f64();
                                let end = start + c.duration().as_secs_f64();
                                frozen_start < end
                                    && frozen_end.map(|fe| fe > start).unwrap_or(true)
                            })
                            .map(|c| {
                                (
                                    c.id.clone(),
                                    c.timeline_start.as_secs_f64(),
                                    c.duration().as_secs_f64(),
                                    c.source_in,
                                )
                            })
                            .collect();
                        for (clip_id, clip_start, dur, source_in) in victims {
                            let clip_freeze_start =
                                (frozen_start - clip_start).max(0.0);
                            let clip_freeze_end = frozen_end
                                .map(|fe| (fe - clip_start).min(dur))
                                .unwrap_or(dur);
                            let new_dur = dur - (clip_freeze_end - clip_freeze_start);
                            if new_dur < 0.5 {
                                continue;
                            }
                            if clip_freeze_start <= 0.05 {
                                // Frozen head: advance the in point, ripple left.
                                let new_in = source_in + Time::from_secs_f64(clip_freeze_end);
                                let _ = ops_mod::trim_clip_ripple(
                                    &mut project,
                                    &clip_id,
                                    Some(new_in),
                                    None,
                                )?;
                                corrections.push(format!(
                                    "frozen_frames at {frozen_start:.2}s: skipped {:.2}s of frozen head on {clip_id} (ripple)",
                                    clip_freeze_end
                                ));
                                changed = true;
                            } else if clip_freeze_end >= dur - 0.05 {
                                // Frozen tail: cut the out point and ripple the
                                // later clips left so no black gap opens.
                                let new_out = source_in + Time::from_secs_f64(clip_freeze_start);
                                let _ = ops_mod::trim_clip_ripple(
                                    &mut project,
                                    &clip_id,
                                    None,
                                    Some(new_out),
                                )?;
                                corrections.push(format!(
                                    "frozen_frames at {frozen_start:.2}s: trimmed {clip_id} to end before the freeze (ripple)"
                                ));
                                changed = true;
                            } else {
                                // Frozen middle: keep the head, drop the rest,
                                // ripple later clips left into the gap.
                                let new_out = source_in + Time::from_secs_f64(clip_freeze_start);
                                let _ = ops_mod::trim_clip_ripple(
                                    &mut project,
                                    &clip_id,
                                    None,
                                    Some(new_out),
                                )?;
                                corrections.push(format!(
                                    "frozen_frames at {frozen_start:.2}s: trimmed {clip_id} to end before the freeze (ripple)"
                                ));
                                changed = true;
                            }
                        }
                    }
                }
                "dead_air" => {
                    // Dead air inside the reel usually means a silent source window;
                    // fade it out audibly so the dead stretch reads as an intentional pause.
                    for clip_id in clip_ids.iter() {
                        let _ = ops_mod::set_clip_audio(
                            &mut project,
                            clip_id,
                            None,
                            None,
                            Some(Time::from_secs_f64(0.4)),
                            None,
                            None,
                            None,
                        );
                    }
                    corrections.push("dead air: added 0.4 s fade-out tails to reel clips".to_string());
                    changed = true;
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
        store.save(&mut project)?;
        current_output = revisions_root.join(format!(
            "{}-autoedit-r{}.mp4",
            crate::util::text::slugify(&project.name),
            project.revision
        ));
    }

    // Subject-aware reframe of the finished reel (item: reframe integrated into auto-edit).
    // Runs after QA passes so corrections operate on the un-reframed timeline.
    let mut reframe_outcome: Option<crate::reframe::ReframeReport> = None;
    if reframe {
        let reframed = revisions_root.join(format!(
            "{}-autoedit-r{}-reframed.mp4",
            crate::util::text::slugify(&project.name),
            project.revision
        ));
        let work_dir = std::env::temp_dir().join("cinvar-reframe");
        eprintln!("reframe pass: {} -> {}", current_output.display(), reframed.display());
        let rr = crate::reframe::reframe(&current_output, &reframed, "vertical", &work_dir, 4.0)?;
        current_output = reframed;
        reframe_outcome = Some(rr);
    }

    let payload = json!({
        "project": store.root.display().to_string(),
        "plan": plan.to_json(),
        "clips": clip_ids,
        "output": current_output.display().to_string(),
        "render_passes": render_passes,
        "corrections": corrections,
        "captions": captions_outcome.as_ref().map(|c| json!({"cues": c.cues, "engine": c.engine, "files_transcribed": c.files_transcribed})),
        "reframe": reframe_outcome.as_ref().map(|r| json!({
            "output": r.output, "crop": [r.crop_w, r.crop_h], "out": [r.out_w, r.out_h],
            "seconds": r.seconds, "frames_sampled": r.track.frames_sampled,
            "frames_with_person": r.track.frames_with_person, "device": r.track.device,
        })),
        "qa": report,
    });
    out(json_mode, payload.clone(), || {
        let verdict = if report.passed { "passed" } else { "FAILED" };
        let mut text = format!(
            "autoedit: {} selected clip(s), {:.1}s reel\nrendered: {} (QA {})\n",
            plan.selections.len(),
            plan.timeline_secs,
            current_output.display(),
            verdict
        );
        for sel in &plan.selections {
            text.push_str(&format!(
                "  {:.1}-{:.1}s score {:.2} from {} ({})\n",
                sel.start, sel.end, sel.score, sel.file_name, sel.reason
            ));
        }
        if !corrections.is_empty() {
            text.push_str("corrections:\n");
            for c in &corrections {
                text.push_str(&format!("  - {c}\n"));
            }
        }
        text
    });
    if !report.passed {
        return Err(Error::new(
            ErrorCode::Render,
            format!("autoedit finished but QA did not pass after {max_revisions} revision(s); see the qa section in the report"),
        ));
    }
    Ok(())
}

fn cmd_run(
    request: &str,
    model: Option<&str>,
    dry_run: bool,
    max_iterations: Option<u32>,
    explicit: Option<&Path>,
    json_mode: bool,
) -> Result<()> {
    let config = Config::load()?;
    let name_hint = text::truncate(request, 40);
    let (store, mut project) = resolve_or_create_project(explicit, &name_hint)?;
    let jobs = Jobs::new();
    start_session(&store, &mut project, &config, &jobs);
    let mut history = History::new(50);
    let mut session = crate::agent::AgentSession::new(&config, model)?;
    let mut opts = crate::agent::AgentOptions {
        dry_run,
        ..crate::agent::AgentOptions::default()
    };
    if let Some(limit) = max_iterations {
        opts.max_iterations = limit;
    }
    eprintln!("model: {} ({})", session.model, session.profile_name);
    let outcome = session.send(&mut project, &store, &jobs, &mut history, request, &opts)?;
    wait_for_outstanding_jobs(&jobs);
    let transcript = session.save_transcript(&store)?;
    if !dry_run {
        store.save(&mut project)?;
    }
    let payload = json!({
        "response": outcome.text,
        "iterations": outcome.iterations,
        "tool_calls": outcome.tool_calls,
        "usage": outcome.usage,
        "stop_reason": outcome.stop_reason,
        "project": store.root.to_string_lossy(),
        "transcript": transcript.to_string_lossy(),
        "issues": crate::project::ops::validate(&project),
    });
    out(json_mode, payload.clone(), || {
        let mut text = String::new();
        text.push_str(&outcome.text);
        text.push_str(&format!(
            "\n\n--- {} tool call(s), {} iterations, {} tokens",
            outcome.tool_calls.len(),
            outcome.iterations,
            outcome.usage.total()
        ));
        text.push_str(&format!(
            "\nproject: {}  transcript: {}",
            store.root.display(),
            transcript.display()
        ));
        text
    })
}

fn cmd_chat(model: Option<&str>, explicit: Option<&Path>) -> Result<()> {
    let config = Config::load()?;
    let (store, mut project) = resolve_or_create_project(explicit, "Chat Project")?;
    let jobs = Jobs::new();
    let mut history = History::new(50);
    let mut session = crate::agent::AgentSession::new(&config, model)?;
    println!(
        "CINVAR chat with {} ({}). Type your instructions; Ctrl-D to exit.",
        session.model, session.profile_name
    );
    let stdin = std::io::stdin();
    loop {
        print!("\n> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let request = line.trim();
        if request.is_empty() {
            continue;
        }
        if request == "/quit" || request == "/exit" {
            break;
        }
        if request == "/status" {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"name": project.name, "revision": project.revision, "issues": crate::project::ops::validate(&project)})
                )?
            );
            continue;
        }
        match session.send(
            &mut project,
            &store,
            &jobs,
            &mut history,
            request,
            &crate::agent::AgentOptions::default(),
        ) {
            Ok(outcome) => {
                println!("\n{}\n", outcome.text);
                store.save(&mut project)?;
            }
            Err(err) => eprintln!("error: {}", crate::secrets::redact(&err.to_string())),
        }
    }
    wait_for_outstanding_jobs(&jobs);
    session.save_transcript(&store)?;
    store.save(&mut project)?;
    println!("\nsaved project at {}", store.root.display());
    Ok(())
}

fn parse_range(raw: &str) -> Result<(crate::util::time::Time, crate::util::time::Time)> {
    let (a, b) = raw
        .split_once('-')
        .ok_or_else(|| Error::invalid("range must look like 00:00:05-00:00:15"))?;
    Ok((a.trim().parse()?, b.trim().parse()?))
}

#[allow(clippy::too_many_arguments)]
fn cmd_render(
    explicit: Option<&Path>,
    output: Option<PathBuf>,
    preview: bool,
    width: Option<u32>,
    height: Option<u32>,
    crf: Option<u8>,
    codec: Option<String>,
    range: Option<String>,
    wait: bool,
    json_mode: bool,
    pro_preview: bool,
) -> Result<()> {
    let store = resolve_project(explicit)?;
    let project = store.load()?;
    if project.timeline().is_none() {
        return Err(Error::invalid("project has no timeline to render")
            .with_hint("ask the agent to build a timeline first"));
    }
    let output_path = match output {
        Some(path) => paths::absolutize(&path, &cwd()),
        None => {
            let suffix = if preview { "preview" } else { "export" };
            store.renders_dir().join(format!(
                "{}-{}-{}.mp4",
                text::slugify(&project.name),
                suffix,
                project.revision
            ))
        }
    };
    // Fail fast: an explicit size or codec this edition cannot reach is refused here, with
    // PermissionDenied (exit code 3), instead of surfacing from the background job as a render
    // failure. render::plan_render re-checks, so this is an accelerator, not the gate.
    if !preview {
        let entitlements = crate::edition::resolve_from_disk();
        if let (Some(w), Some(h)) = (width, height) {
            entitlements.gate_export(w, h, pro_preview)?;
        }
        if codec
            .as_deref()
            .and_then(crate::render::pro_codec_name)
            .is_some()
        {
            entitlements.gate_feature(crate::edition::Feature::ProCodecs, pro_preview)?;
        }
    }
    let mut request = crate::render::RenderRequest::for_output(output_path.clone());
    request.preview = preview;
    request.width = width;
    request.height = height;
    request.crf = crf;
    request.codec = codec;
    request.pro_preview = pro_preview;
    if let Some(raw) = range {
        request.range = Some(parse_range(&raw)?);
    }
    let jobs = Jobs::new();
    let store_for_job = store.clone();
    let project_for_job = project.clone();
    let job_id = jobs.spawn("render", move |job| {
        let progress = |p: f32, message: &str| {
            job.progress(p, message);
            eprint!("\rrender {:>5.1}% {}", p * 100.0, message);
            std::io::stderr().flush().ok();
        };
        let cancelled = || job.cancelled();
        let outcome = crate::render::render(
            &project_for_job,
            &store_for_job,
            &request,
            &progress,
            &cancelled,
        )?;
        Ok(json!(outcome))
    });
    if wait || json_mode {
        let snapshot = jobs
            .wait(&job_id, std::time::Duration::from_secs(7_200))
            .ok_or_else(|| Error::internal("job disappeared"))?;
        eprintln!();
        if let Ok(db) = crate::db::Db::open(&crate::db::Db::default_path()) {
            let _ = db.record_job(
                &job_id,
                "render",
                Some(&project.id),
                snapshot.status.as_str(),
                Some(&output_path.to_string_lossy()),
            );
        }
        let value = json!(snapshot);
        match snapshot.status {
            crate::jobs::JobStatus::Succeeded => out(json_mode, value, || {
                format!("rendered {}", output_path.display())
            }),
            crate::jobs::JobStatus::Failed => {
                let message = snapshot
                    .error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "render failed".into());
                Err(Error::new(
                    ErrorCode::Render,
                    crate::secrets::redact(&message),
                ))
            }
            other => Err(Error::cancelled(format!(
                "render finished as {}",
                other.as_str()
            ))),
        }
    } else {
        out(
            json_mode,
            json!({"job_id": job_id, "output": output_path.to_string_lossy()}),
            || {
                format!("render started as job {job_id} -> {}\ncheck progress with: cinvar jobs status {job_id}", output_path.display())
            },
        )
    }
}

fn cmd_status(explicit: Option<&Path>, json_mode: bool) -> Result<()> {
    let store = resolve_project(explicit)?;
    let project = store.load()?;
    let issues = crate::project::ops::validate(&project);
    let payload = json!({
        "path": store.root.to_string_lossy(),
        "name": project.name,
        "revision": project.revision,
        "assets": project.assets.len(),
        "missing_assets": project.assets.iter().filter(|a| a.missing).count(),
        "timelines": project.timelines.iter().map(|t| json!({
            "name": t.name,
            "duration": t.duration().as_secs_f64(),
            "clips": t.tracks.iter().map(|tr| tr.clips.len()).sum::<usize>(),
        })).collect::<Vec<_>>(),
        "issues": issues,
        "renders": std::fs::read_dir(store.renders_dir()).map(|dir| {
            let mut files: Vec<String> = dir.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
            files.sort();
            files
        }).unwrap_or_default(),
    });
    out(json_mode, payload.clone(), || {
        format!(
            "{} (revision {})\n  path      {}\n  assets    {} ({} missing)\n  timelines {}\n  issues    {}",
            project.name,
            project.revision,
            store.root.display(),
            project.assets.len(),
            project.assets.iter().filter(|a| a.missing).count(),
            payload["timelines"].as_array().map(|a| a.len()).unwrap_or(0),
            if issues.is_empty() { "none".to_string() } else { issues.join("; ") }
        )
    })
}

fn cmd_jobs(cmd: &JobsCmd, json_mode: bool) -> Result<()> {
    let db = crate::db::Db::open(&crate::db::Db::default_path())?;
    match cmd {
        JobsCmd::List => {
            let rows = db.recent_jobs(20)?;
            let payload = json!({"jobs": rows.iter().map(|(id, kind, status, started, detail)| json!({
                "id": id, "kind": kind, "status": status, "started_at": started, "detail": detail
            })).collect::<Vec<_>>()});
            out(json_mode, payload, || {
                if rows.is_empty() {
                    "no jobs recorded yet".to_string()
                } else {
                    rows.iter()
                        .map(|(id, kind, status, started, detail)| {
                            format!("{id}  {kind:<14} {status:<10} {started}  {}", detail.clone().unwrap_or_default())
                        })
                        .collect::<Vec<_>>()
                        .join("
")
                }
            })
        }
        JobsCmd::Status { job_id } => {
            let rows = db.recent_jobs(200)?;
            let found = rows.into_iter().find(|(id, ..)| id == job_id);
            match found {
                Some((id, kind, status, started, detail)) => {
                    let payload = json!({"id": id, "kind": kind, "status": status, "started_at": started, "detail": detail});
                    out(json_mode, payload.clone(), || serde_json::to_string_pretty(&payload).unwrap_or_default())
                }
                None => Err(Error::not_found(format!("no recorded job {job_id}"))
                    .with_hint("in-process job handles are polled with the MCP cinvar.render.status tool")),
            }
        }
        JobsCmd::Cancel { job_id } => Err(Error::not_found(format!("job {job_id} is not running in this process"))
            .with_hint("cancel a render through the MCP cinvar.render.cancel tool, or Ctrl-C a blocking render")),
    }
}

fn cmd_reframe(
    input: &Path,
    output: Option<&Path>,
    target: &str,
    sample_fps: f64,
    json_mode: bool,
) -> Result<()> {
    let output = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| input.with_extension("reframed.mp4"));
    let work_dir = std::env::temp_dir().join("cinvar-reframe");
    let report = crate::reframe::reframe(input, &output, target, &work_dir, sample_fps)?;
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "input": input,
                "output": report.output,
                "target": target,
                "input_size": [report.track.width, report.track.height],
                "output_size": [report.out_w, report.out_h],
                "crop_window": [report.crop_w, report.crop_h],
                "frames_sampled": report.track.frames_sampled,
                "frames_with_person": report.track.frames_with_person,
                "keyframes": report.track.keyframes.len(),
                "device": report.track.device,
                "wall_seconds": report.seconds,
            })
        );
    } else {
        println!(
            "reframed {} -> {} ({}x{} window -> {}x{}; subject in {}/{} sampled frames on {})",
            input.display(),
            report.output,
            report.crop_w,
            report.crop_h,
            report.out_w,
            report.out_h,
            report.track.frames_with_person,
            report.track.frames_sampled,
            report.track.device,
        );
    }
    Ok(())
}

fn cmd_enhance(
    input: &Path,
    upscale: u32,
    output: Option<&Path>,
    tile: Option<u32>,
    json_mode: bool,
    pro_preview: bool,
) -> Result<()> {
    if !input.is_file() {
        return Err(Error::invalid(format!("input not found: {}", input.display())));
    }
    let output = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| input.with_extension(format!("upscaled{upscale}x.mp4")));
    let work_dir = std::env::temp_dir().join("cinvar-enhance");
    let started = std::time::Instant::now();
    let report = crate::enhance::enhance_video_with(
        input,
        &output,
        upscale,
        tile.unwrap_or(0),
        &work_dir,
        pro_preview,
    )?;
    let (duration, _fps, width, height, _audio) = crate::enhance::probe_video(input)?;
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "input": input,
                "output": output,
                "scale": upscale,
                "input_size": [width, height],
                "output_size": [width * upscale, height * upscale],
                "frames": report.count,
                "gpu_seconds": report.seconds,
                "wall_seconds": started.elapsed().as_secs_f64(),
                "input_duration": duration,
            })
        );
    } else {
        println!(
            "upscaled {}x: {} -> {}\n  {}x{} -> {}x{}, {} frames, {} s on GPU {} (wall {:.1} s)",
            upscale,
            input.display(),
            output.display(),
            width,
            height,
            width * upscale,
            height * upscale,
            report.count,
            report.seconds,
            report.gpuid,
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

#[derive(clap::Subcommand, Debug)]
pub enum AudioCmd {
    #[command(about = "Separate stems and render a cleaned mix (vocals + ducked background)")]
    Clean {
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        #[arg(value_name = "FILE", help = "Output file (default: <input>.clean.wav)")]
        output: Option<PathBuf>,
        #[arg(long, default_value = "htdemucs")]
        model: String,
    },
    #[command(about = "Separate into 4 stems without mixing")]
    Stems {
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        #[arg(long, default_value = "htdemucs")]
        model: String,
    },
}

fn cmd_audio(cmd: &AudioCmd, json_mode: bool) -> Result<()> {
    match cmd {
        AudioCmd::Clean { input, output, model } => {
            if !input.is_file() {
                return Err(Error::invalid(format!("input not found: {}", input.display())));
            }
            let out = output
                .clone()
                .unwrap_or_else(|| input.with_extension("clean.wav"));
            let work_dir = std::env::temp_dir().join("cinvar-demucs");
            let started = std::time::Instant::now();
            let report = crate::audio::demucs::clean_mix(input, &out, &work_dir)?;
            if json_mode {
                println!("{}", serde_json::json!({
                    "input": input, "output": out, "stems": report.stems,
                    "gpu_seconds": report.seconds,
                    "wall_seconds": started.elapsed().as_secs_f64(),
                }));
            } else {
                println!(
                    "cleaned mix: {} ({} stems, {} s separation, wall {:.1} s)",
                    out.display(),
                    report.stems.len(),
                    report.seconds,
                    started.elapsed().as_secs_f64()
                );
            }
            Ok(())
        }
        AudioCmd::Stems { input, model } => {
            if !input.is_file() {
                return Err(Error::invalid(format!("input not found: {}", input.display())));
            }
            let work_dir = std::env::temp_dir().join("cinvar-demucs");
            let report = crate::audio::demucs::separate(input, &work_dir, model)?;
            if json_mode {
                println!("{}", serde_json::json!({ "input": input, "stems": report.stems }));
            } else {
                for s in &report.stems {
                    println!("{}", s);
                }
            }
            Ok(())
        }
    }
}

fn cmd_media(cmd: &MediaCmd, json_mode: bool) -> Result<()> {
    match cmd {
        MediaCmd::Probe { path } => {
            let info = crate::media::probe(&paths::absolutize(path, &cwd()))?;
            out(json_mode, json!({"info": info}), || {
                format!(
                    "{}x{}  {:.2}s  {:.3} fps  video={} audio={}",
                    info.width,
                    info.height,
                    info.duration.as_secs_f64(),
                    info.fps.map(|f| f.as_f64()).unwrap_or(0.0),
                    info.has_video,
                    info.has_audio
                )
            })
        }
        MediaCmd::Import { path } => {
            let store = resolve_project(None)?;
            let mut project = store.load()?;
            let asset =
                store.import_asset(&mut project, &paths::absolutize(path, &cwd()), "cli:import")?;
            store.save(&mut project)?;
            out(
                json_mode,
                json!({"asset_id": asset.id, "path": asset.path}),
                || format!("imported {} as {}", asset.display_name(), asset.id),
            )
        }
        MediaCmd::Frames { path, at, width } => {
            let at: crate::util::time::Time = at.parse()?;
            let out_path = cwd().join(format!(
                "frame-{}.png",
                at.to_frames(crate::util::time::parse_fps("1000")?)
            ));
            let written = crate::media::extract_frame(
                &paths::absolutize(path, &cwd()),
                at,
                &out_path,
                Some(*width),
            )?;
            out(
                json_mode,
                json!({"file": written.to_string_lossy()}),
                || format!("wrote {}", written.display()),
            )
        }
        MediaCmd::Scenes { path, threshold } => {
            let cuts = crate::media::detect_scenes(&paths::absolutize(path, &cwd()), *threshold)?;
            out(json_mode, json!({"scenes": cuts}), || {
                cuts.iter()
                    .map(|c| format!("{:.3}s  {:.2}", c.at.as_secs_f64(), c.score))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
    }
}

fn cmd_skills(cmd: &SkillsCmd, json_mode: bool) -> Result<()> {
    match cmd {
        SkillsCmd::List => {
            let skills = crate::skills::load_all()?;
            out(
                json_mode,
                json!({"skills": skills.iter().map(|s| json!({"name": s.name, "title": s.title, "summary": s.summary, "tags": s.tags})).collect::<Vec<_>>()}),
                || {
                    skills
                        .iter()
                        .map(|s| format!("{:<22} {}", s.name, s.title))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            )
        }
        SkillsCmd::Show { name } => {
            let skills = crate::skills::load_all()?;
            let skill = skills
                .iter()
                .find(|s| &s.name == name)
                .ok_or_else(|| Error::not_found(format!("skill '{name}' is not installed")))?;
            out(
                json_mode,
                json!({"name": skill.name, "title": skill.title, "body": skill.body}),
                || skill.body.clone(),
            )
        }
    }
}

fn cmd_plugins(cmd: &PluginsCmd, json_mode: bool) -> Result<()> {
    match cmd {
        PluginsCmd::List => {
            let dir = paths::config_dir().join("plugins");
            let manifests = crate::plugins::discover(&dir)?;
            out(json_mode, json!({"plugins": manifests}), || {
                if manifests.is_empty() {
                    format!("no plugins installed in {}", dir.display())
                } else {
                    manifests
                        .iter()
                        .map(|m| format!("{:<20} {}  {}", m.name, m.version, m.description))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            })
        }
    }
}

fn cmd_bench() -> Result<()> {
    let started = std::time::Instant::now();
    let config = Config::load().unwrap_or_default();
    let config_ms = started.elapsed().as_millis();
    let dir = tempfile::tempdir()?;
    let (store, mut project) = ProjectStore::create(dir.path(), "bench")?;
    let created = std::time::Instant::now();
    store.save(&mut project)?;
    let save_ms = created.elapsed().as_millis();
    let loaded = std::time::Instant::now();
    let _ = store.load()?;
    let load_ms = loaded.elapsed().as_millis();
    let sample = dir.path().join("sample.mp4");
    let ffmpeg = shproc::require("ffmpeg")?;
    let gen_start = std::time::Instant::now();
    let args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=30:duration=5",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=5",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-shortest",
        sample.to_str().unwrap_or("sample.mp4"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    shproc::run(
        &ffmpeg,
        &args,
        &shproc::ProcOpts::default().with_timeout(120),
    )?;
    let gen_ms = gen_start.elapsed().as_millis();
    let probe_start = std::time::Instant::now();
    let info = crate::media::probe(&sample)?;
    let probe_ms = probe_start.elapsed().as_millis();
    let payload = json!({
        "config_load_ms": config_ms,
        "project_save_ms": save_ms,
        "project_load_ms": load_ms,
        "synthetic_clip_generation_ms": gen_ms,
        "ffprobe_ms": probe_ms,
        "probe_duration": info.duration.as_secs_f64(),
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "model_profiles": config.models.len(),
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn cmd_shorts(
    input: &Path,
    count: u32,
    duration: &str,
    output: Option<&Path>,
    vocab: Option<&str>,
    report: Option<&Path>,
    json_mode: bool,
) -> Result<()> {
    let (min_duration, max_duration) = crate::shorts::parse_duration_range(duration)?;
    if count == 0 {
        return Err(Error::invalid("--count must be at least 1"));
    }
    if !crate::shorts::source_usable(input) {
        return Err(Error::invalid(format!("input not found or empty: {}", input.display())));
    }
    crate::edition::resolve_from_disk().check_feature(crate::edition::Feature::ShortsMulti)?;
    let req = crate::shorts::ShortsRequest {
        input: input.to_path_buf(),
        count,
        min_duration,
        max_duration,
        output_dir: output.map(|p| p.to_path_buf()),
        vocab: vocab
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    };
    let result = crate::shorts::generate(&req)?;
    if let Some(path) = report {
        let body = serde_json::to_vec_pretty(&result)?;
        std::fs::write(path, body)?;
    }
    out(json_mode, serde_json::to_value(&result).unwrap_or_default(), || {
        let mut text = format!(
            "shorts: {} from {} ({} ms)\n\n",
            result.shorts.len(),
            result.source.display(),
            result.elapsed_ms
        );
        for s in &result.shorts {
            let name = s
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            text.push_str(&format!(
                "  {}  {:>6.2}s-{:<6.2}s  {:>5.1} score  {}x{}  QA {}\n",
                name,
                s.start,
                s.end,
                s.score,
                s.width,
                s.height,
                if s.qa_passed { "passed" } else { "FAILED" }
            ));
        }
        text
    })
}

fn cmd_activate(
    key: Option<&str>,
    file: Option<&Path>,
    redeem_url: Option<&str>,
    json_mode: bool,
) -> Result<()> {
    let source = match (key, file) {
        (_, Some(path)) => crate::edition::ActivationSource::File(path.to_path_buf()),
        (Some(raw), None) => {
            let candidate = Path::new(raw);
            if candidate.is_file() {
                crate::edition::ActivationSource::File(candidate.to_path_buf())
            } else {
                crate::edition::ActivationSource::OrderKey(raw.to_string())
            }
        }
        (None, None) => return Err(Error::invalid("pass an order key or --file <license.json>")),
    };
    let license = crate::edition::activate_with(source, redeem_url)?;
    out(json_mode, serde_json::to_value(&license).unwrap_or_default(), || {
        format!(
            "CINVAR Pro activated (license {}, edition {}).\nPro capabilities are unlocked now; no restart needed.",
            license.id, license.edition
        )
    })
}
#[derive(Subcommand, Debug)]
enum BlenderCmd {
    #[command(about = "Render a Blender animation: single machine, or across ssh workers with --workers")]
    Render {
        #[arg(long, value_name = "ssh://n1,ssh://n4", help = "Worker machines; omit to render on this machine")]
        workers: Option<String>,
        #[arg(long, default_value = "logo_reveal")]
        template: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long, default_value_t = 5.0)]
        duration: f64,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long, help = "Keep the alpha channel (ProRes 4444)")]
        transparent: bool,
        #[arg(long, value_name = "DIR", help = "Render job directory (default: the project folder)")]
        out_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 32, help = "Cycles samples per pixel")]
        samples: u32,
        #[arg(long, help = "Extra attempts for a unit that produced nothing usable")]
        retries: Option<u32>,
    },
    #[command(about = "Report the render devices Blender offers on this machine and which one is chosen")]
    Devices,
}

fn cmd_blender(cmd: &BlenderCmd, json_mode: bool) -> Result<()> {
    match cmd {
        BlenderCmd::Devices => {
            let (info, probe, choice) = crate::blender::gpu::detect()?;
            out(
                json_mode,
                serde_json::to_value(&serde_json::json!({
                    "blender": info.version,
                    "probe": probe,
                    "choice": choice,
                }))
                .unwrap_or_default(),
                || {
                    let mut text = format!("Blender {}\n\n", info.version);
                    for d in &probe.devices {
                        text.push_str(&format!("  {:<8} {}\n", d.backend.as_str(), d.name));
                    }
                    text.push_str(&format!("\n{}\n", choice.diagnosis()));
                    text
                },
            )
        }
        BlenderCmd::Render {
            workers,
            template,
            text,
            duration,
            width,
            height,
            fps,
            transparent,
            out_dir,
            samples,
            retries,
        } => {
            let mut anim = crate::blender::AnimationSpec::default();
            anim.template = template.clone();
            if let Some(value) = text {
                anim.text = value.clone();
            }
            anim.duration = crate::util::time::Time::from_secs_f64(*duration);
            anim.width = *width;
            anim.height = *height;
            anim.fps = *fps;
            anim.transparent = *transparent;
            let ent = crate::edition::resolve_from_disk();
            ent.check_export(anim.width, anim.height)?;
            let base = match out_dir {
                Some(dir) => dir.clone(),
                None => std::env::current_dir().map_err(|e| {
                    crate::error::Error::new(
                        crate::error::ErrorCode::Io,
                        format!("current directory: {e}"),
                    )
                })?,
            };
            let started = std::time::Instant::now();
            match workers {
                Some(list) => {
                    let mut opts = crate::blender::distributed::DistributedOptions::default();
                    opts.workers = crate::blender::distributed::parse_workers(list)?;
                    opts.profile.samples = *samples;
                    if let Some(r) = retries {
                        opts.retries = *r;
                    }
                    let report = crate::blender::distributed::render(
                        &anim,
                        &base,
                        &opts,
                        &crate::blender::RenderControl::none(),
                    )?;
                    let elapsed = started.elapsed().as_secs_f64();
                    out(
                        json_mode,
                        serde_json::to_value(&report).unwrap_or_default(),
                        || {
                            format!(
                                "rendered {} frame(s) across {} worker(s) in {:.1}s -> {}\nreused {} frame(s), {} of {} unit(s) completed",
                                report.rendered_frames,
                                report.per_worker.len(),
                                elapsed,
                                report.clip.display(),
                                report.reused_frames,
                                report.units_completed,
                                report.units_planned
                            )
                        },
                    )
                }
                None => {
                    let render = crate::blender::render_animation_job(
                        &anim,
                        &base,
                        &crate::blender::RenderControl::none(),
                    )?;
                    let elapsed = started.elapsed().as_secs_f64();
                    out(
                        json_mode,
                        serde_json::to_value(&render).unwrap_or_default(),
                        || {
                            format!(
                                "rendered {} frame(s) in {:.1}s -> {}",
                                render.frames,
                                elapsed,
                                render.clip.display()
                            )
                        },
                    )
                }
            }
        }
    }
}