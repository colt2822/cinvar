use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::jobs::Jobs;
use crate::project::history::History;
use crate::project::store::ProjectStore;
use crate::util::paths;

mod studio;

struct UiState {
    project_dir: Option<PathBuf>,
    jobs: Jobs,
    session: Option<crate::agent::AgentSession>,
    history: History,
    studio: Arc<studio::Studio>,
}

type Shared = Arc<Mutex<UiState>>;

pub fn serve(config: &Config, port: u16, project: Option<PathBuf>) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| {
        Error::new(ErrorCode::Io, format!("cannot bind 127.0.0.1:{port}: {e}"))
            .with_hint("choose another port with --port")
    })?;
    let dir = match project {
        Some(dir) => Some(paths::absolutize(
            &dir,
            &std::env::current_dir().unwrap_or_default(),
        )),
        None => {
            let here = std::env::current_dir().unwrap_or_default();
            if here.join("project.json").exists() {
                Some(here)
            } else {
                None
            }
        }
    };
    let state: Shared = Arc::new(Mutex::new(UiState {
        project_dir: dir,
        jobs: Jobs::new(),
        session: None,
        history: History::new(50),
        studio: Arc::new(studio::Studio::new()),
    }));
    println!("CINVAR web UI on http://127.0.0.1:{port}");
    if let Some(dir) = state.lock().unwrap().project_dir.clone() {
        println!("project: {}", dir.display());
    } else {
        println!("no project open yet — create one from the UI or with cinvar project create");
    }
    let _ = config;
    let bound = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);
    for request in server.incoming_requests() {
        let shared = state.clone();
        std::thread::spawn(move || {
            if let Err((status, msg)) = guard_request(&request, bound) {
                let _ = request.respond(json_response(status, &serde_json::json!({"error": msg})));
                return;
            }
            if let Err(err) = handle(request, shared) {
                eprintln!(
                    "ui request failed: {}",
                    crate::secrets::redact(&err.to_string())
                );
            }
        });
    }
    Ok(())
}

// Largest JSON request body the UI accepts; larger ones are refused before they are read.
const MAX_JSON_BODY: usize = 1024 * 1024;

/// Browser-origin defences for an unauthenticated localhost server: a web page the user visits
/// must not be able to drive it (CSRF) or read it through DNS rebinding.
///  * `Host` must be the loopback name + our port (a rebound hostname fails here);
///  * a present `Origin` must be our own origin;
///  * mutating requests must be `application/json` (cross-site JSON posts need a CORS preflight,
///    which this server never grants).
fn guard_request(request: &Request, port: u16) -> std::result::Result<(), (u16, String)> {
    let header = |name: &'static str| -> Option<String> {
        request
            .headers()
            .iter()
            .find(|h| h.field.equiv(name))
            .map(|h| h.value.as_str().to_string())
    };
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    match header("Host") {
        Some(h) if allowed.contains(&h.to_ascii_lowercase()) => {}
        _ => return Err((403, "Host header not allowed".into())),
    }
    if let Some(origin) = header("Origin") {
        let ok = allowed
            .iter()
            .any(|a| origin.eq_ignore_ascii_case(&format!("http://{a}")));
        if !ok {
            return Err((403, "cross-origin request refused".into()));
        }
    }
    if *request.method() != Method::Get && *request.method() != Method::Head {
        let ct = header("Content-Type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let upload = request.url().split('?').next() == Some("/api/upload");
        if !(ct.starts_with("application/json")
            || (upload && ct.starts_with("application/octet-stream")))
        {
            return Err((403, "Content-Type must be application/json".into()));
        }
        // Refuse an oversized JSON body before a byte of it is read (the upload route streams to
        // disk and has its own cap).
        if !upload {
            let declared = header("Content-Length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if declared > MAX_JSON_BODY {
                return Err((413, format!("request body exceeds {MAX_JSON_BODY} bytes")));
            }
        }
    }
    Ok(())
}

/// (provider id, label, auth kind, suggested model ids). Kinds: key = API key, cli = signed-in local CLI, url = local server.
const CONNECTIONS: [(&str, &str, &str, &[&str]); 12] = [
    ("claude-cli", "Claude (your subscription)", "cli", &["default", "sonnet", "opus", "haiku"]),
    ("anthropic", "Anthropic API", "key", &["claude-fable-5-2", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]),
    ("deepseek", "DeepSeek", "key", &["deepseek-v4-pro", "deepseek-flash"]),
    ("openai", "OpenAI", "key", &["gpt-4.1", "gpt-4.1-mini"]),
    ("gemini", "Google Gemini", "key", &["gemini-2.5-pro", "gemini-2.5-flash"]),
    ("xai", "xAI", "key", &["grok-4"]),
    ("openrouter", "OpenRouter", "key", &["anthropic/claude-sonnet-4.5", "deepseek/deepseek-chat"]),
    ("ollama", "Ollama (local)", "url", &["llama3.1", "qwen2.5:7b"]),
    ("openai-compatible", "Custom OpenAI-compatible server", "url", &[]),
    ("runway", "Runway (video)", "key", &["gen4.5", "gen4_turbo", "seedance2"]),
    ("luma", "Luma (video)", "key", &["ray-2", "ray-flash-2"]),
    ("fal", "fal.ai (video, many models)", "key", &["fal-ai/wan/v2.2-a14b/text-to-video"]),
];

const MODEL_PROVIDER_CHOICES: [&str; 13] = [
    "anthropic", "deepseek", "openai", "openai-compatible", "gemini", "xai", "openrouter", "ollama", "llamacpp", "local", "google", "claude-cli", "mock",
];

fn request_error(status: u16, msg: &str) -> (u16, serde_json::Value) {
    (status, json!({"error": msg}))
}

fn respond_json(request: Request, (status, value): (u16, serde_json::Value)) -> Result<()> {
    request.respond(json_response(status, &value)).map_err(Error::from)
}

fn json_response(status: u16, value: &serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(value).unwrap_or_default();
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

/// Identity of the UI this binary serves. The page compares it with the server's current value and offers a reload.
fn ui_build() -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(include_str!("../../web/index.html").as_bytes()).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn text_response(status: u16, content_type: &str, body: Vec<u8>) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).unwrap())
}

fn read_body(request: &mut Request) -> serde_json::Value {
    // A chunked body never declares its length, so the cap is enforced here as well as in the guard.
    let mut body = String::new();
    let _ = request
        .as_reader()
        .take(MAX_JSON_BODY as u64)
        .read_to_string(&mut body);
    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null)
}

/// Tool families the web panel may call. Everything else stays behind the agent/MCP permission modes.
const UI_TOOL_PREFIXES: &[&str] = &[
    "cinvar.render.batch_",
    "cinvar.preset.",
    "cinvar.color.grade",
    "cinvar.project.status",
    // The Studio "Production suite" exposes these families directly. Each tool still enforces
    // config.security and the edition gate, so widening this list exposes capability, not
    // authority: a Pro-only tool still refuses on a Free license through tool_feature().
    "cinvar.captions.",
    "cinvar.blender.",
    "cinvar.shorts.",
    "cinvar.audio.",
    "cinvar.enhance.",
    "cinvar.reframe.",
    "cinvar.otio.",
    "cinvar.scenes.",
];

fn edition_status() -> serde_json::Value {
    let config = Config::load().unwrap_or_default();
    let ent = crate::edition::resolve(&config);
    let mut v = ent.summary();
    v["catalog"] = json!(ent.catalog_status());
    v["upgrade_url"] = json!(crate::edition::UPGRADE_URL);
    v["pro_module"] = json!(crate::edition::modules().contains(&"pro"));
    v
}

fn handle(mut request: Request, state: Shared) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();

    if method == Method::Get && (path == "/" || path == "/index.html") {
        let html = include_str!("../../web/index.html").replace("</head>", &format!("<meta name=\"ui-build\" content=\"{}\"></head>", ui_build()));
        return request
            .respond(
                text_response(200, "text/html; charset=utf-8", html.into_bytes())
                    // never cached: a stale tab must not keep running an old UI against a new server
                    .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap()),
            )
            .map_err(Error::from);
    }

    if method == Method::Get && path == "/api/edition" {
        return request
            .respond(json_response(200, &edition_status()))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/license" {
        let body = read_body(&mut request);
        let text = body.get("license").and_then(|v| v.as_str()).unwrap_or("");
        let result = crate::edition::license::install_text(
            text,
            &crate::edition::license::default_path(),
            crate::edition::license::production_keys(),
        );
        return match result {
            Ok(_) => request
                .respond(json_response(200, &edition_status()))
                .map_err(Error::from),
            Err(e) => request
                .respond(json_response(400, &json!({"error": e.message})))
                .map_err(Error::from),
        };
    }

    if method == Method::Post && path == "/api/license/activate" {
        let body = read_body(&mut request);
        let source = if let Some(text) = body.get("license").and_then(|v| v.as_str()) {
            Some(crate::edition::ActivationSource::OrderKey(text.to_string()))
        } else if let Some(key) = body.get("order_key").and_then(|v| v.as_str()) {
            Some(crate::edition::ActivationSource::OrderKey(key.to_string()))
        } else if let Some(path) = body.get("file").and_then(|v| v.as_str()) {
            Some(crate::edition::ActivationSource::File(PathBuf::from(path)))
        } else {
            None
        };
        let Some(source) = source else {
            return request
                .respond(json_response(
                    400,
                    &json!({"error": "send {\"order_key\": \"...\"}, {\"license\": \"...\"} or {\"file\": \"path\"}"}),
                ))
                .map_err(Error::from);
        };
        return match crate::edition::activate(source) {
            Ok(activated) => request
                .respond(json_response(
                    200,
                    &json!({"activated": activated, "edition": edition_status()}),
                ))
                .map_err(Error::from),
            Err(e) => request
                .respond(json_response(400, &json!({"error": e.message, "hint": e.hint})))
                .map_err(Error::from),
        };
    }

    if method == Method::Post && path == "/api/license/remove" {
        let _ = crate::edition::license::remove(&crate::edition::license::default_path());
        return request
            .respond(json_response(200, &edition_status()))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/tool" {
        let body = read_body(&mut request);
        let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let args = body.get("args").cloned().unwrap_or(json!({}));
        if !UI_TOOL_PREFIXES.iter().any(|p| name.starts_with(p)) {
            return request
                .respond(json_response(403, &json!({"error": format!("tool '{name}' is not available from this panel")})))
                .map_err(Error::from);
        }
        let result = (|| -> Result<serde_json::Value> {
            let dir = state.lock().unwrap().project_dir.clone().ok_or_else(|| Error::not_found("no project is open"))?;
            let store = ProjectStore::open(&dir)?;
            let mut project = store.load()?;
            let config = Config::load().unwrap_or_default();
            let registry = crate::tools::build_default();
            let jobs = state.lock().unwrap().jobs.clone();
            let mut guard = state.lock().unwrap();
            let mut ctx = crate::tools::ToolCtx {
                project: &mut project,
                store: &store,
                config: &config,
                jobs: &jobs,
                history: &mut guard.history,
                dry_run: false,
            };
            registry.call(&name, &mut ctx, args)
        })();
        return match result {
            Ok(v) => request.respond(json_response(200, &v)).map_err(Error::from),
            Err(e) => request
                .respond(json_response(if e.code == ErrorCode::PermissionDenied { 403 } else { 400 }, &json!({"error": e.message, "code": e.code, "hint": e.hint})))
                .map_err(Error::from),
        };
    }

    if method == Method::Get && path == "/api/doctor" {
        return request
            .respond(json_response(200, &crate::cli::doctor_report()))
            .map_err(Error::from);
    }

    if method == Method::Get && path == "/api/status" {
        let dir = state.lock().unwrap().project_dir.clone();
        let value = match dir {
            Some(dir) => match ProjectStore::open(&dir).and_then(|store| {
                let project = store.load()?;
                Ok((store, project))
            }) {
                Ok((store, project)) => json!({
                    "open": true,
                    "path": dir.to_string_lossy(),
                    "name": project.name,
                    "revision": project.revision,
                    "issues": crate::project::ops::validate(&project),
                    "assets": project.assets.iter().map(|a| json!({"id": a.id, "name": a.display_name(), "kind": format!("{:?}", a.kind).to_lowercase(), "seconds": a.info.as_ref().map(|i| i.duration.as_secs_f64()), "missing": a.missing})).collect::<Vec<_>>(),
                    "ui_build": ui_build(), "timelines": project.timelines.iter().map(|t| json!({"id": t.id, "name": t.name, "duration": t.duration().as_secs_f64(), "clips": t.tracks.iter().map(|tr| tr.clips.len()).sum::<usize>(), "width": t.width, "height": t.height, "fps": t.fps.as_f64(), "tracks": t.tracks.iter().map(|tr| json!({"id": tr.id, "name": tr.name, "kind": format!("{:?}", tr.kind).to_lowercase(), "muted": tr.muted, "hidden": tr.hidden, "locked": tr.locked, "clips": tr.clips.iter().map(|c| json!({"id": c.id, "name": if c.name.is_empty() { c.text.as_ref().map(|x| x.text.clone()).unwrap_or_default() } else { c.name.clone() }, "asset": c.asset, "start": c.timeline_start.as_secs_f64(), "duration": c.duration().as_secs_f64(), "speed": c.speed, "text": c.text.is_some(), "effects": c.effects.len()})).collect::<Vec<_>>()})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                    "renders": std::fs::read_dir(store.renders_dir()).map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect::<Vec<_>>()).unwrap_or_default(),
                }),
                Err(err) => json!({"open": false, "error": err.to_string()}),
            },
            None => json!({"open": false}),
        };
        return request
            .respond(json_response(200, &value))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/project" {
        let body = read_body(&mut request);
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Cinvar Project");
        let parent = body
            .get("dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let (store, project) = ProjectStore::create(&parent, name)?;
        state.lock().unwrap().project_dir = Some(store.root.clone());
        if let Ok(db) = crate::db::Db::open(&crate::db::Db::default_path()) {
            let _ = db.upsert_project(&project.id, &store.root.to_string_lossy(), &project.name);
        }
        return request
            .respond(json_response(
                200,
                &json!({"path": store.root.to_string_lossy(), "name": project.name}),
            ))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/open" {
        let body = read_body(&mut request);
        let dir = body.get("dir").and_then(|v| v.as_str()).unwrap_or_default();
        let store = ProjectStore::open(Path::new(dir))?;
        let project = store.load()?;
        state.lock().unwrap().project_dir = Some(store.root.clone());
        return request
            .respond(json_response(
                200,
                &json!({"path": store.root.to_string_lossy(), "name": project.name}),
            ))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/chat" {
        let body = read_body(&mut request);
        let message = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if message.trim().is_empty() {
            return request
                .respond(json_response(400, &json!({"error": "empty message"})))
                .map_err(Error::from);
        }
        let pinned: Vec<String> = body.get("skills").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).take(4).collect()).unwrap_or_default();
        let config = Config::load()?;
        let dir = state.lock().unwrap().project_dir.clone();
        let dir = dir.ok_or_else(|| Error::not_found("no project is open"))?;
        let store = ProjectStore::open(&dir)?;
        let mut project = store.load()?;
        let jobs = state.lock().unwrap().jobs.clone();
        let result = (|| -> Result<serde_json::Value> {
            let mut guard = state.lock().unwrap();
            if guard.session.is_none() {
                guard.session = Some(crate::agent::AgentSession::new(&config, None)?);
            }
            let UiState {
                session, history, ..
            } = &mut *guard;
            let session = session.as_mut().unwrap();
            let outcome = session.send(
                &mut project,
                &store,
                &jobs,
                history,
                &message,
                &crate::agent::AgentOptions { pinned_skills: pinned.clone(), ..Default::default() },
            )?;
            store.save(&mut project)?;
            let _ = session.save_transcript(&store);
            Ok(json!({
                "text": outcome.text,
                "tool_calls": outcome.tool_calls.iter().map(|t| json!({"name": t.name, "ok": t.ok, "error": t.error})).collect::<Vec<_>>(),
                "iterations": outcome.iterations,
                "usage": outcome.usage,
                "skills": outcome.skills,
            }))
        })();
        return match result {
            Ok(value) => request
                .respond(json_response(200, &value))
                .map_err(Error::from),
            Err(err) => request
                .respond(json_response(
                    500,
                    &json!({"error": crate::secrets::redact(&err.to_string())}),
                ))
                .map_err(Error::from),
        };
    }

    if method == Method::Post && path == "/api/render" {
        let body = read_body(&mut request);
        let preview = body
            .get("preview")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dir = state
            .lock()
            .unwrap()
            .project_dir
            .clone()
            .ok_or_else(|| Error::not_found("no project is open"))?;
        let store = ProjectStore::open(&dir)?;
        let project = store.load()?;
        let want_tl = body.get("timeline_id").and_then(|v| v.as_str()).unwrap_or("");
        let tl_slug = project.timelines.iter().find(|t| t.id == want_tl).map(|t| format!("-{}", crate::util::text::slugify(&t.name))).unwrap_or_default();
        let output = store.renders_dir().join(format!(
            "{}{}-{}.mp4",
            crate::util::text::slugify(&project.name),
            tl_slug,
            if preview {
                format!("preview-{}", project.revision)
            } else {
                format!("export-{}", project.revision)
            }
        ));
        let mut req = crate::render::RenderRequest::for_output(output.clone());
        req.preview = preview;
        req.timeline_id = body.get("timeline_id").and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(String::from);
        let jobs = state.lock().unwrap().jobs.clone();
        let store_for_job = store.clone();
        let project_for_job = project.clone();
        let output_str = output.to_string_lossy().to_string();
        let job_id = jobs.spawn("render", move |job| {
            let progress = |p: f32, m: &str| job.progress(p, m);
            let cancelled = || job.cancelled();
            let outcome = crate::render::render(
                &project_for_job,
                &store_for_job,
                &req,
                &progress,
                &cancelled,
            )?;
            Ok(json!(outcome))
        });
        return request
            .respond(json_response(
                200,
                &json!({"job_id": job_id, "output": output_str}),
            ))
            .map_err(Error::from);
    }

    if method == Method::Get && path.starts_with("/api/jobs") {
        let jobs = state.lock().unwrap().jobs.clone();
        let id = path.trim_start_matches("/api/jobs").trim_start_matches('/');
        let value = if id.is_empty() {
            json!({"jobs": jobs.list()})
        } else {
            json!(jobs.get(id))
        };
        return request
            .respond(json_response(200, &value))
            .map_err(Error::from);
    }

    if method == Method::Get && path.starts_with("/api/media/") {
        let name = path.trim_start_matches("/api/media/");
        let dir = state
            .lock()
            .unwrap()
            .project_dir
            .clone()
            .ok_or_else(|| Error::not_found("no project is open"))?;
        let store = ProjectStore::open(&dir)?;
        let decoded = percent_decode(name);
        // An escape attempt is an explicit refusal, not a dropped connection: answer 404 so a
        // traversal cannot be distinguished from a miss and never reaches the filesystem.
        let file = match paths::ensure_within(&store.assets_dir(), Path::new(&decoded)) {
            Ok(f) if f.exists() => f,
            _ => match paths::ensure_within(&store.renders_dir(), Path::new(&decoded)) {
                Ok(f) if f.exists() => f,
                _ => {
                    return request
                        .respond(json_response(404, &json!({"error": "not found"})))
                        .map_err(Error::from)
                }
            },
        };
        let ctype = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "mp4" | "m4v" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "m4a" => "audio/mp4",
            _ => "application/octet-stream",
        };
        // Byte ranges so the player can seek without loading the whole file.
        let total = std::fs::metadata(&file)?.len();
        let range = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .and_then(|h| {
                h.value
                    .as_str()
                    .strip_prefix("bytes=")
                    .map(|v| v.to_string())
            });
        let (start, end, partial) = match range.as_deref().and_then(|r| r.split_once('-')) {
            Some((a, b)) if total > 0 => {
                let start = a.parse::<u64>().unwrap_or(0).min(total - 1);
                let end = b
                    .parse::<u64>()
                    .ok()
                    .unwrap_or(total - 1)
                    .min(total - 1)
                    .max(start);
                (start, end.min(start + 32 * 1024 * 1024), true)
            }
            _ => (0, total.saturating_sub(1), false),
        };
        let mut f = std::fs::File::open(&file)?;
        {
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Start(start))?;
        }
        let mut buf = Vec::new();
        if total > 0 {
            use std::io::Read;
            f.take(end - start + 1).read_to_end(&mut buf)?;
        }
        let mut resp = text_response(if partial { 206 } else { 200 }, ctype, buf)
            .with_header(Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap());
        if partial {
            resp = resp.with_header(
                Header::from_bytes(
                    &b"Content-Range"[..],
                    format!("bytes {start}-{end}/{total}").as_bytes(),
                )
                .unwrap(),
            );
        }
        return request.respond(resp).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/upload" {
        // Human drag-and-drop import from the UI: the file is streamed to a temp file inside the
        // project, probed by content, copied into the project's assets, then the temp file is removed.
        let dir = state
            .lock()
            .unwrap()
            .project_dir
            .clone()
            .ok_or_else(|| Error::not_found("no project is open"))?;
        let store = ProjectStore::open(&dir)?;
        let query = url.split_once('?').map(|x| x.1).unwrap_or("");
        let raw = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("name="))
            .unwrap_or("upload");
        let decoded = percent_decode(raw);
        let safe: String = decoded
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("upload")
            .chars()
            .filter(|c| c.is_alphanumeric() || " ._-()".contains(*c))
            .collect();
        let safe = if safe.trim_matches('.').is_empty() {
            "upload".to_string()
        } else {
            safe
        };
        let tmp_dir = store.root.join(".uploads");
        std::fs::create_dir_all(&tmp_dir)?;
        let tmp = tmp_dir.join(&safe);
        paths::unlink_if_symlink(&tmp);
        {
            let mut file = std::fs::File::create(&tmp)?;
            let mut reader = request.as_reader().take(16 * 1024 * 1024 * 1024);
            std::io::copy(&mut reader, &mut file)?;
        }
        let result = (|| -> Result<serde_json::Value> {
            crate::media::probe_untrusted(&tmp).map_err(|e| {
                Error::new(
                    crate::error::ErrorCode::InvalidInput,
                    format!("not importable as media: {}", e.message),
                )
            })?;
            let mut project = store.load()?;
            let asset = store.import_asset(&mut project, &tmp, "ui:upload")?;
            store.save(&mut project)?;
            Ok(json!({"asset_id": asset.id, "name": asset.display_name()}))
        })();
        let _ = std::fs::remove_file(&tmp);
        return match result {
            Ok(v) => request.respond(json_response(200, &v)).map_err(Error::from),
            Err(err) => request
                .respond(json_response(
                    400,
                    &json!({"error": crate::secrets::redact(&err.to_string())}),
                ))
                .map_err(Error::from),
        };
    }

    if method == Method::Post && path == "/api/model" {
        let body = read_body(&mut request);
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let mut config = Config::load()?;
        if !config.models.contains_key(name) {
            return request
                .respond(json_response(400, &json!({"error": "unknown model"})))
                .map_err(Error::from);
        }
        config.active_model = Some(name.to_string());
        config.save()?;
        state.lock().unwrap().session = None;
        return request
            .respond(json_response(200, &json!({"active": name})))
            .map_err(Error::from);
    }

    if method == Method::Get && path == "/api/skills" {
        let skills = crate::skills::load_all().unwrap_or_default();
        let value = skills
            .iter()
            .map(|s| json!({"name": s.name, "title": s.title, "summary": s.summary}))
            .collect::<Vec<_>>();
        return request
            .respond(json_response(200, &json!(value)))
            .map_err(Error::from);
    }

    if method == Method::Get && path == "/api/models" {
        let config = Config::load()?;
        let store = crate::secrets::CredentialStore::load().ok();
        // Never returns a key: only whether one is available for each profile.
        let details: Vec<serde_json::Value> = config
            .models
            .iter()
            .map(|(name, p)| {
                let has_key = p.provider.eq_ignore_ascii_case("mock")
                    || p.provider.eq_ignore_ascii_case("claude-cli")
                    || p.provider.eq_ignore_ascii_case("claude-cli")
                    || matches!(p.provider.to_ascii_lowercase().as_str(), "ollama" | "llamacpp" | "local")
                    || crate::secrets::resolve(name, p).is_some()
                    || store.as_ref().and_then(|s| s.get_key(name)).is_some();
                json!({"name": name, "provider": p.provider, "model": p.model, "base_url": p.base_url, "has_key": has_key})
            })
            .collect();
        return request
            .respond(json_response(200, &json!({
                "active": config.active_model,
                "models": config.models.keys().cloned().collect::<Vec<_>>(),
                "details": details,
                "providers": MODEL_PROVIDER_CHOICES,
            })))
            .map_err(Error::from);
    }

    // Model management from the UI: add/update a profile, store its API key in the local credential store
    // (0600 file, never echoed back or logged), test, and remove.
    if method == Method::Post && path == "/api/models/add" {
        let body = read_body(&mut request);
        let text = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").to_string();
        let (name, provider, model, base_url, api_key) =
            (text("name"), text("provider").to_ascii_lowercase(), text("model"), text("base_url"), text("api_key"));
        let bad = |msg: &str| request_error(400, msg);
        if name.is_empty() || name.len() > 40 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return respond_json(request, bad("name must be 1-40 letters, digits, - or _"));
        }
        if !MODEL_PROVIDER_CHOICES.contains(&provider.as_str()) {
            return respond_json(request, bad("unknown provider"));
        }
        if model.is_empty() || model.len() > 120 {
            return respond_json(request, bad("model is required"));
        }
        if !base_url.is_empty() && !(base_url.starts_with("https://") || base_url.starts_with("http://localhost") || base_url.starts_with("http://127.0.0.1")) {
            return respond_json(request, bad("base_url must be https:// (or http://localhost for local servers)"));
        }
        let mut config = Config::load()?;
        let mut profile = config.models.get(&name).cloned().unwrap_or_default();
        profile.provider = provider.clone();
        profile.model = model.clone();
        profile.base_url = if base_url.is_empty() { None } else { Some(base_url) };
        config.upsert_profile(&name, profile);
        if config.active_model.is_none() || config.active_model.as_deref() == Some("mock") {
            config.active_model = Some(name.clone());
        }
        config.save()?;
        if !api_key.is_empty() {
            let mut store = crate::secrets::CredentialStore::load()?;
            store.set_key(&name, &api_key)?;
        }
        state.lock().unwrap().session = None;
        return request
            .respond(json_response(200, &json!({"ok": true, "name": name, "active": config.active_model, "key_stored": !api_key.is_empty()})))
            .map_err(Error::from);
    }

    if method == Method::Post && path == "/api/models/key" {
        let body = read_body(&mut request);
        let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let config = Config::load()?;
        if !config.models.contains_key(&name) {
            return respond_json(request, request_error(400, "unknown model"));
        }
        if key.is_empty() || key.len() > 4096 {
            return respond_json(request, request_error(400, "no key provided"));
        }
        let mut store = crate::secrets::CredentialStore::load()?;
        store.set_key(&name, &key)?;
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/models/test" {
        let body = read_body(&mut request);
        let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let config = Config::load()?;
        let Some(profile) = config.models.get(&name).cloned() else {
            return respond_json(request, request_error(400, "unknown model"));
        };
        if matches!(name.as_str(), "runway" | "luma" | "fal") {
            let has = crate::videogen::Backend::parse(&name).and_then(crate::videogen::api_key).is_some();
            return request.respond(json_response(200, &json!({"ok": has, "message": if has { "key saved. It is not verified: the only real check is a billable generation" } else { "no key saved" }}))).map_err(Error::from);
        }
        let result = crate::providers::build(&name, &profile, &config).and_then(|p| p.health().map(|_| ()));
        let payload = match result {
            Ok(()) => json!({"ok": true, "message": format!("{name} is reachable ({})", profile.model)}),
            // Errors are passed through the same redaction used for logs, so a key can never be echoed.
            Err(e) => json!({"ok": false, "message": crate::secrets::redact(&e.message)}),
        };
        return request.respond(json_response(200, &payload)).map_err(Error::from);
    }

    // ---- Harness settings: one CONNECTION per provider, and ROLES (assistant / script / video) that pick a model. ----
    if method == Method::Get && path == "/api/harness" {
        let config = Config::load()?;
        let store = crate::secrets::CredentialStore::load().ok();
        let connections: Vec<serde_json::Value> = CONNECTIONS.iter().map(|(provider, label, kind, models)| {
            let profile = config.models.get(*provider);
            let connected = match *kind {
                "cli" => crate::providers::build(provider, &profile.cloned().unwrap_or_else(|| crate::config::ModelProfile { provider: provider.to_string(), ..Default::default() }), &config).and_then(|p| p.health()).is_ok(),
                "url" => profile.is_some(),
                _ => profile.map(|p| crate::secrets::resolve(provider, p).is_some()).unwrap_or(false) || store.as_ref().and_then(|s| s.get_key(provider)).is_some(),
            };
            json!({"provider": provider, "label": label, "kind": kind, "video_only": matches!(*provider, "runway" | "luma" | "fal"), "connected": connected, "base_url": profile.and_then(|p| p.base_url.clone()), "models": models})
        }).collect();
        let workflows: Vec<serde_json::Value> = crate::comfy::load_registry(&crate::comfy::registry_dir()).iter().map(|d| json!({"name": d.name, "task": d.task})).collect();
        let reachable = crate::comfy::ComfyClient::from_env().and_then(|c| c.system_stats()).is_ok();
        let backends: Vec<serde_json::Value> = crate::videogen::BACKENDS.iter().map(|b| json!({"backend": b.id(), "label": b.label(), "connected": crate::videogen::api_key(*b).is_some(), "models": b.models(), "max_seconds": b.max_seconds()})).collect();
        let assistant = config.role_profile("assistant").ok().map(|(_, p)| json!({"provider": p.provider, "model": p.model}));
        let script = config.role_profile("script").ok().map(|(_, p)| json!({"provider": p.provider, "model": p.model}));
        return request.respond(json_response(200, &json!({
            "connections": connections,
            "roles": {"assistant": assistant, "script": script, "script_is_default": config.roles.script.is_none(),
                "blender": config.role_profile("blender").ok().map(|(_, p)| json!({"provider": p.provider, "model": p.model})), "blender_is_default": config.roles.blender.is_none(),
                "motion": config.role_profile("motion").ok().map(|(_, p)| json!({"provider": p.provider, "model": p.model})), "motion_is_default": config.roles.motion.is_none(),
                "auto_skills": config.ui.auto_skills, "video_workflow": config.roles.video_workflow, "video_backend": config.roles.video_backend},
            "video": {"workflows": workflows, "comfy_reachable": reachable, "backends": backends, "settings": {"enabled": config.videogen.enabled, "max_job_seconds": config.videogen.max_job_seconds, "max_day_seconds": config.videogen.max_day_seconds, "used_today_seconds": crate::videogen::seconds_used_today()}},
        }))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/connections" {
        let body = read_body(&mut request);
        let text = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").to_string();
        let (provider, base_url, api_key) = (text("provider"), text("base_url"), text("api_key"));
        let Some((_, _, kind, models)) = CONNECTIONS.iter().find(|c| c.0 == provider) else {
            return respond_json(request, request_error(400, "unknown provider"));
        };
        if !base_url.is_empty() && !(base_url.starts_with("https://") || base_url.starts_with("http://localhost") || base_url.starts_with("http://127.0.0.1")) {
            return respond_json(request, request_error(400, "base_url must be https:// (or http://localhost for local servers)"));
        }
        let mut config = Config::load()?;
        let mut profile = config.models.get(&provider).cloned().unwrap_or_default();
        profile.provider = provider.clone();
        if profile.model.is_empty() || profile.model == "mock-planner" {
            profile.model = models.first().map(|m| m.to_string()).unwrap_or_default();
        }
        if !base_url.is_empty() || *kind == "url" {
            profile.base_url = if base_url.is_empty() { None } else { Some(base_url) };
        }
        config.upsert_profile(&provider, profile);
        config.save()?;
        if !api_key.is_empty() {
            let mut store = crate::secrets::CredentialStore::load()?;
            store.set_key(&provider, &api_key)?;
        }
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true, "key_stored": !api_key.is_empty()}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/connections/remove" {
        let body = read_body(&mut request);
        let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let mut config = Config::load()?;
        config.models.remove(&provider);
        if CONNECTIONS.iter().any(|c| c.0 == provider) {
            crate::secrets::CredentialStore::load()?.remove_key(&provider)?;
        }
        if config.roles.video_backend.as_ref().map(|b| b.provider == provider).unwrap_or(false) {
            config.roles.video_backend = None;
        }
        for role in [&mut config.roles.assistant, &mut config.roles.script] {
            if role.as_ref().map(|b| b.provider == provider).unwrap_or(false) {
                *role = None;
            }
        }
        if config.active_model.as_deref() == Some(provider.as_str()) {
            config.active_model = config.models.keys().next().cloned();
        }
        config.save()?;
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/roles" {
        let body = read_body(&mut request);
        let text = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").to_string();
        let (role, provider, model) = (text("role"), text("provider"), text("model"));
        let mut config = Config::load()?;
        match role.as_str() {
            "video" => {
                // Either a ComfyUI workflow (local) or a hosted backend; choosing one clears the other.
                let wf = text("workflow");
                let backend = text("backend");
                if !backend.is_empty() {
                    let Some(b) = crate::videogen::Backend::parse(&backend) else { return respond_json(request, request_error(400, "unknown video backend")) };
                    let m = if model.is_empty() { b.models()[0].to_string() } else { model.clone() };
                    config.roles.video_backend = Some(crate::config::RoleBinding { provider: b.id().into(), model: m });
                    config.roles.video_workflow = None;
                } else {
                    config.roles.video_workflow = if wf.is_empty() { None } else { Some(wf) };
                    config.roles.video_backend = None;
                }
            }
            "assistant" | "script" | "blender" | "motion" => {
                if provider.is_empty() && role != "assistant" {
                    match role.as_str() {
                        "script" => config.roles.script = None, // follow the assistant
                        "blender" => config.roles.blender = None,
                        _ => config.roles.motion = None,
                    }
                } else {
                    if !CONNECTIONS.iter().any(|c| c.0 == provider) || model.is_empty() || model.len() > 120 {
                        return respond_json(request, request_error(400, "pick a connection and a model"));
                    }
                    let mut profile = config.models.get(&provider).cloned().unwrap_or_default();
                    profile.provider = provider.clone();
                    if role == "assistant" {
                        profile.model = model.clone();
                    } else if profile.model.is_empty() || profile.model == "mock-planner" {
                        profile.model = model.clone();
                    }
                    config.models.insert(provider.clone(), profile);
                    let binding = crate::config::RoleBinding { provider: provider.clone(), model };
                    if role == "assistant" {
                        config.active_model = Some(provider.clone());
                        config.roles.assistant = Some(binding);
                    } else {
                        match role.as_str() {
                            "script" => config.roles.script = Some(binding),
                            "blender" => config.roles.blender = Some(binding),
                            _ => config.roles.motion = Some(binding),
                        }
                    }
                }
            }
            _ => return respond_json(request, request_error(400, "unknown role")),
        }
        config.save()?;
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/auto_skills" {
        let body = read_body(&mut request);
        let mut config = Config::load()?;
        config.ui.auto_skills = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        config.save()?;
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/videogen/settings" {
        let body = read_body(&mut request);
        let mut config = Config::load()?;
        if let Some(v) = body.get("enabled").and_then(|v| v.as_bool()) {
            config.videogen.enabled = v;
        }
        if let Some(v) = body.get("max_job_seconds").and_then(|v| v.as_u64()) {
            config.videogen.max_job_seconds = v.clamp(1, 60) as u32;
        }
        if let Some(v) = body.get("max_day_seconds").and_then(|v| v.as_u64()) {
            config.videogen.max_day_seconds = v.clamp(1, 3600) as u32;
        }
        config.save()?;
        return request.respond(json_response(200, &json!({"ok": true}))).map_err(Error::from);
    }

    if method == Method::Post && path == "/api/models/remove" {
        let body = read_body(&mut request);
        let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let mut config = Config::load()?;
        if config.models.remove(&name).is_none() {
            return respond_json(request, request_error(400, "unknown model"));
        }
        if config.active_model.as_deref() == Some(name.as_str()) {
            config.active_model = config.models.keys().next().cloned();
        }
        config.save()?;
        state.lock().unwrap().session = None;
        return request.respond(json_response(200, &json!({"ok": true, "active": config.active_model}))).map_err(Error::from);
    }

    // Studio "production suite" routes. Their contract, request and response shapes live in
    // web/STUDIO_ENDPOINTS.md and are implemented in server::studio.
    let method_name = match method {
        Method::Get => "GET",
        Method::Post => "POST",
        _ => "",
    };
    if studio::is_route(method_name, &path) {
        let body = if method == Method::Post {
            read_body(&mut request)
        } else {
            serde_json::Value::Null
        };
        let (dir, jobs, studio) = {
            let guard = state.lock().unwrap();
            (
                guard.project_dir.clone(),
                guard.jobs.clone(),
                guard.studio.clone(),
            )
        };
        let config = Config::load().unwrap_or_default();
        let env = studio::Env {
            project_dir: dir.as_deref(),
            jobs: &jobs,
            config: &config,
        };
        let result = studio::handle(&studio, method_name, &path, &body, &env)
            .unwrap_or_else(|| Err(Error::not_found("unknown route")));
        let (status, value) = match result {
            Ok(value) => (200, value),
            Err(err) => (studio::status_for(&err), studio::error_body(&err)),
        };
        return request
            .respond(json_response(status, &value))
            .map_err(Error::from);
    }

    request
        .respond(json_response(404, &json!({"error": "unknown route"})))
        .map_err(Error::from)
}

fn percent_decode(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
