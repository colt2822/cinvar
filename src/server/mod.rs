use std::io::Cursor;
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

struct UiState {
    project_dir: Option<PathBuf>,
    jobs: Jobs,
    session: Option<crate::agent::AgentSession>,
    history: History,
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
            if let Err(msg) = guard_request(&request, bound) {
                let _ = request.respond(json_response(403, &serde_json::json!({"error": msg})));
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

/// Browser-origin defences for an unauthenticated localhost server: a web page the user visits
/// must not be able to drive it (CSRF) or read it through DNS rebinding.
///  * `Host` must be the loopback name + our port (a rebound hostname fails here);
///  * a present `Origin` must be our own origin;
///  * mutating requests must be `application/json` (cross-site JSON posts need a CORS preflight,
///    which this server never grants).
fn guard_request(request: &Request, port: u16) -> std::result::Result<(), String> {
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
        _ => return Err("Host header not allowed".into()),
    }
    if let Some(origin) = header("Origin") {
        let ok = allowed
            .iter()
            .any(|a| origin.eq_ignore_ascii_case(&format!("http://{a}")));
        if !ok {
            return Err("cross-origin request refused".into());
        }
    }
    if *request.method() != Method::Get && *request.method() != Method::Head {
        let ct = header("Content-Type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !ct.starts_with("application/json") {
            return Err("Content-Type must be application/json".into());
        }
    }
    Ok(())
}

fn json_response(status: u16, value: &serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(value).unwrap_or_default();
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn text_response(status: u16, content_type: &str, body: Vec<u8>) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).unwrap())
}

fn read_body(request: &mut Request) -> serde_json::Value {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null)
}

fn handle(mut request: Request, state: Shared) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();

    if method == Method::Get && (path == "/" || path == "/index.html") {
        let html = include_str!("../../web/index.html");
        return request
            .respond(text_response(
                200,
                "text/html; charset=utf-8",
                html.as_bytes().to_vec(),
            ))
            .map_err(Error::from);
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
                    "timelines": project.timelines.iter().map(|t| json!({"name": t.name, "duration": t.duration().as_secs_f64(), "clips": t.tracks.iter().map(|tr| tr.clips.len()).sum::<usize>(), "width": t.width, "height": t.height})).collect::<Vec<_>>(),
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
                &crate::agent::AgentOptions::default(),
            )?;
            store.save(&mut project)?;
            let _ = session.save_transcript(&store);
            Ok(json!({
                "text": outcome.text,
                "tool_calls": outcome.tool_calls.iter().map(|t| json!({"name": t.name, "ok": t.ok, "error": t.error})).collect::<Vec<_>>(),
                "iterations": outcome.iterations,
                "usage": outcome.usage,
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
        let output = store.renders_dir().join(format!(
            "{}-{}.mp4",
            crate::util::text::slugify(&project.name),
            if preview {
                format!("preview-{}", project.revision)
            } else {
                format!("export-{}", project.revision)
            }
        ));
        let mut req = crate::render::RenderRequest::for_output(output.clone());
        req.preview = preview;
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
        let file = paths::ensure_within(&store.assets_dir(), Path::new(name))?;
        if !file.exists() {
            return request
                .respond(json_response(404, &json!({"error": "not found"})))
                .map_err(Error::from);
        }
        let bytes = std::fs::read(&file)?;
        let ctype = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "mp4" | "m4v" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            _ => "application/octet-stream",
        };
        return request
            .respond(text_response(200, ctype, bytes))
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
        return request
            .respond(json_response(200, &json!({"active": config.active_model, "models": config.models.keys().cloned().collect::<Vec<_>>()})))
            .map_err(Error::from);
    }

    request
        .respond(json_response(404, &json!({"error": "unknown route"})))
        .map_err(Error::from)
}
