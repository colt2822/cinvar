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
    #[command(about = "List and read creative skills")]
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
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
    #[command(about = "Measure startup and render timings on this machine")]
    Bench,
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
enum PluginsCmd {
    List,
}

#[derive(Subcommand, Debug)]
enum McpCmd {
    Serve,
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
    match &cli.command {
        Commands::Setup { yes } => cmd_setup(*yes, json_mode),
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
        Commands::Skills { cmd } => cmd_skills(cmd, json_mode),
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
                let report = crate::otio::export_otio(&project, None, &out_path)?;
                out(json_mode, json!(report), || {
                    format!("wrote {}", out_path.display())
                })
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

fn cmd_setup(yes: bool, json_mode: bool) -> Result<()> {
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
    let mut request = crate::render::RenderRequest::for_output(output_path.clone());
    request.preview = preview;
    request.width = width;
    request.height = height;
    request.crf = crf;
    request.codec = codec;
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
