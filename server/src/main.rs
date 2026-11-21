//! Trimly web server.
//!
//! The same engine as the desktop app, behind an HTTP API, so the Trimly web
//! interface can trim, crop, queue, pause and resume clips in a browser. The
//! server does the downloading and cutting; the browser then downloads the
//! finished file from it.
//!
//! Settings come from environment variables (all optional):
//!   PORT                  port to listen on (default 8787)
//!   TRIMLY_ACCESS_KEY     if set, every request must carry this key
//!   TRIMLY_ALLOW_ORIGIN   comma-separated origins allowed by CORS (default: any)
//!   TRIMLY_DATA_DIR       where finished clips are kept (default: ./data)
//!   TRIMLY_STATIC_DIR     a folder of web files to serve (the Trimly web app)
//!   TRIMLY_MAX_JOBS       clips running at once (default 4)
//!   TRIMLY_MAX_PARTS      parts one clip may download at once (default 4)
//!   TRIMLY_MAX_CLIP_SECS  longest clip allowed (default 1800)
//!   TRIMLY_FILE_TTL_MINS  how long finished clips stay available (default 120)
//!   TRIMLY_COOKIES_FILE   a cookies file for yt-dlp (some sites block servers)
//!   TRIMLY_ALLOW_PRIVATE  set to 1 to allow links to private network addresses

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::{ServeDir, ServeFile},
};
use trimly_engine::{self as engine, ClipRequest, ClipState, CropSpec, Host, Progress};

// ------------------------------------------------------------------ config

#[derive(Clone)]
struct Config {
    access_key: Option<String>,
    data_dir: PathBuf,
    max_jobs: usize,
    max_parts: u32,
    max_clip_secs: f64,
    file_ttl: Duration,
    allow_private: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

// ------------------------------------------------------------------ jobs

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

struct JobRecord {
    req: ClipRequest,
    status: Status,
    progress: Option<Progress>,
    file: Option<PathBuf>,
    error: Option<String>,
    updated: Instant,
}

type Jobs = Arc<Mutex<HashMap<String, JobRecord>>>;

/// Gives the engine somewhere to report progress and keep its job table.
#[derive(Clone)]
struct ServerHost {
    state: Arc<ClipState>,
    jobs: Jobs,
}

impl Host for ServerHost {
    fn state(&self) -> &ClipState {
        &self.state
    }
    fn emit(&self, p: Progress) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(&p.id) {
            job.progress = Some(p);
        }
    }
}

struct AppState {
    cfg: Config,
    host: ServerHost,
    /// Preview id -> the video address the server will read frames from. The
    /// browser never gets to name an address, only an id.
    previews: Mutex<HashMap<String, (String, Instant)>>,
}

type Shared = Arc<AppState>;

// ------------------------------------------------------------------ errors

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ------------------------------------------------------------------ safety

/// Only links to the public internet are allowed, so the server can't be used
/// to reach machines on its own network (or cloud metadata addresses).
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
                || o[0] == 0)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80) // link local
        }
    }
}

fn check_link(cfg: &Config, link: &str) -> Result<(), String> {
    let url = url::Url::parse(link).map_err(|_| "That doesn't look like a link.".to_string())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Please enter a full http(s) link.".into());
    }
    let host = url.host_str().ok_or("That link has no website in it.")?;
    if cfg.allow_private {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|_| "Couldn't find that website.".to_string())?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| !is_public(a.ip())) {
        return Err("Links to private or local addresses aren't allowed.".into());
    }
    Ok(())
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ------------------------------------------------------------------ auth

async fn auth(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if let Some(key) = &app.cfg.access_key {
        if path.starts_with("/api/") && path != "/api/health" && req.method() != Method::OPTIONS {
            let from_header = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string);
            // Downloads are plain links, so they may carry the key in the address.
            let from_query = req
                .uri()
                .query()
                .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("key=")))
                .map(str::to_string);
            let given = from_header.or(from_query).unwrap_or_default();
            if !constant_time_eq(given.as_bytes(), key.as_bytes()) {
                return ApiError(StatusCode::UNAUTHORIZED, "A valid access key is needed.".into()).into_response();
            }
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ------------------------------------------------------------------ handlers

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    ok: bool,
    auth_required: bool,
    version: &'static str,
}

async fn health(State(app): State<Shared>) -> Json<Health> {
    Json(Health {
        ok: true,
        auth_required: app.cfg.access_key.is_some(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn tools() -> ApiResult<engine::ToolStatus> {
    Ok(Json(blocking(engine::check_tools).await?))
}

#[derive(Deserialize)]
struct UrlBody {
    url: String,
}

async fn info(State(app): State<Shared>, Json(body): Json<UrlBody>) -> ApiResult<engine::VideoInfo> {
    let cfg = app.cfg.clone();
    blocking(move || -> Result<engine::VideoInfo, String> {
        check_link(&cfg, &body.url)?;
        engine::fetch_info(&body.url)
    })
    .await?
    .map(Json)
    .map_err(bad)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewId {
    preview_id: Option<String>,
}

async fn preview(State(app): State<Shared>, Json(body): Json<UrlBody>) -> ApiResult<PreviewId> {
    let cfg = app.cfg.clone();
    let source = blocking(move || -> Result<Option<String>, String> {
        check_link(&cfg, &body.url)?;
        engine::preview_url(&body.url, None)
    })
    .await?
    .map_err(bad)?;
    let preview_id = source.map(|src| {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut map = app.previews.lock().unwrap();
        // Forget old ones so this can't grow without bound.
        map.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3 * 3600));
        map.insert(id.clone(), (src, Instant::now()));
        id
    });
    Ok(Json(PreviewId { preview_id }))
}

#[derive(Deserialize)]
struct FrameQuery {
    t: f64,
}

#[derive(Serialize)]
struct Frame {
    image: String,
}

async fn frame(State(app): State<Shared>, Path(id): Path<String>, Query(q): Query<FrameQuery>) -> ApiResult<Frame> {
    let source = app
        .previews
        .lock()
        .unwrap()
        .get(&id)
        .map(|(s, _)| s.clone())
        .ok_or(ApiError(StatusCode::NOT_FOUND, "That preview has expired.".into()))?;
    let t = q.t;
    let image = blocking(move || engine::preview_frame(&id, &source, t))
        .await?
        .map_err(|e| {
            if e == engine::SUPERSEDED {
                // A newer request for this preview replaced it; the browser ignores this one.
                ApiError(StatusCode::CONFLICT, e)
            } else {
                eprintln!("[trimly] preview frame at {t:.1}s failed: {e}");
                ApiError(StatusCode::BAD_GATEWAY, e)
            }
        })?;
    Ok(Json(Frame { image }))
}

/// What the browser sends to start a clip. The server decides where files go
/// and how the clip is named.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewClip {
    url: String,
    start: f64,
    end: f64,
    kind: String,
    max_height: Option<u32>,
    #[serde(default)]
    is_live: bool,
    audio_format: Option<String>,
    #[serde(default = "yes")]
    exact: bool,
    crop: Option<CropSpec>,
    #[serde(default = "four")]
    parts: u32,
}

fn yes() -> bool {
    true
}
fn four() -> u32 {
    4
}

#[derive(Serialize)]
struct Created {
    id: String,
}

async fn create_clip(State(app): State<Shared>, Json(c): Json<NewClip>) -> ApiResult<Created> {
    let cfg = app.cfg.clone();
    if !(c.start >= 0.0 && c.end > c.start && c.end.is_finite()) {
        return Err(bad("End time must be after start time."));
    }
    if c.end - c.start > cfg.max_clip_secs {
        return Err(bad(format!(
            "This server allows clips up to {} minutes long.",
            (cfg.max_clip_secs / 60.0).floor()
        )));
    }
    if c.kind != "video" && c.kind != "audio" {
        return Err(bad("Choose video or audio."));
    }
    {
        let running = app.host.jobs.lock().unwrap().values().filter(|j| j.status == Status::Running).count();
        if running >= cfg.max_jobs {
            return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "The server is busy. Try again in a minute.".into()));
        }
    }
    let url = c.url.clone();
    blocking(move || check_link(&cfg, &url)).await?.map_err(bad)?;

    let id = uuid::Uuid::new_v4().simple().to_string();
    let out_dir = app.cfg.data_dir.join("out").join(&id);
    std::fs::create_dir_all(&out_dir).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let req = ClipRequest {
        id: id.clone(),
        url: c.url,
        start: c.start,
        end: c.end,
        out_dir: out_dir.to_string_lossy().into_owned(),
        kind: c.kind,
        max_height: c.max_height,
        is_live: c.is_live,
        audio_format: c.audio_format,
        exact: c.exact,
        cookies_browser: None, // browser sign-ins only exist on a person's own computer
        template: None,
        crop: c.crop,
        parts: c.parts.clamp(1, app.cfg.max_parts),
    };
    app.host.jobs.lock().unwrap().insert(
        id.clone(),
        JobRecord { req, status: Status::Running, progress: None, file: None, error: None, updated: Instant::now() },
    );
    start_job(&app, &id);
    Ok(Json(Created { id }))
}

/// Run (or carry on) a clip in the background.
fn start_job(app: &Shared, id: &str) {
    let host = app.host.clone();
    let id = id.to_string();
    let req = match host.jobs.lock().unwrap().get_mut(&id) {
        Some(j) => {
            j.status = Status::Running;
            j.error = None;
            j.updated = Instant::now();
            j.req.clone()
        }
        None => return,
    };
    tokio::task::spawn_blocking(move || {
        let result = engine::run_clip(&host, &req);
        if let Some(job) = host.jobs.lock().unwrap().get_mut(&id) {
            job.updated = Instant::now();
            match result {
                Ok(path) => {
                    job.status = Status::Done;
                    job.file = Some(PathBuf::from(path));
                }
                Err(e) if e == "Paused." => job.status = Status::Paused,
                Err(e) if e == "Cancelled." => job.status = Status::Cancelled,
                Err(e) => {
                    job.status = Status::Failed;
                    job.error = Some(e);
                }
            }
        }
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobView {
    status: Status,
    stage: String,
    percent: Option<f64>,
    detail: String,
    bytes: Option<u64>,
    resumable: bool,
    file_name: Option<String>,
    size_bytes: Option<u64>,
    error: Option<String>,
}

fn view_of(job: &JobRecord) -> JobView {
    let p = job.progress.as_ref();
    JobView {
        status: job.status,
        stage: p.map(|p| p.stage.clone()).unwrap_or_else(|| "starting".into()),
        percent: p.and_then(|p| p.percent),
        detail: p.map(|p| p.detail.clone()).unwrap_or_default(),
        bytes: p.and_then(|p| p.bytes),
        resumable: p.map(|p| p.resumable).unwrap_or(false),
        file_name: job.file.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()),
        size_bytes: job.file.as_ref().and_then(|f| std::fs::metadata(f).ok()).map(|m| m.len()),
        error: job.error.clone(),
    }
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "That clip wasn't found. It may have expired.".into())
}

async fn clip_status(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<JobView> {
    let jobs = app.host.jobs.lock().unwrap();
    jobs.get(&id).map(|j| Json(view_of(j))).ok_or_else(not_found)
}

#[derive(Serialize)]
struct Ok1 {
    ok: bool,
}

async fn pause_clip(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Ok1> {
    if !safe_id(&id) {
        return Err(not_found());
    }
    Ok(Json(Ok1 { ok: engine::pause_job(&app.host, &id) }))
}

async fn resume_clip(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Ok1> {
    let paused = app.host.jobs.lock().unwrap().get(&id).map(|j| j.status == Status::Paused);
    match paused {
        Some(true) => {
            start_job(&app, &id);
            Ok(Json(Ok1 { ok: true }))
        }
        Some(false) => Ok(Json(Ok1 { ok: false })),
        None => Err(not_found()),
    }
}

async fn cancel_clip(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Ok1> {
    if !safe_id(&id) {
        return Err(not_found());
    }
    let (existed, running) = {
        let jobs = app.host.jobs.lock().unwrap();
        (jobs.contains_key(&id), jobs.get(&id).map(|j| j.status == Status::Running).unwrap_or(false))
    };
    if !existed {
        return Err(not_found());
    }
    if running {
        engine::cancel_clip(&app.host, &id);
    } else {
        // Nothing is running: just clean up what was saved.
        engine::discard_clip(&id);
        remove_job(&app, &id);
    }
    Ok(Json(Ok1 { ok: true }))
}

fn remove_job(app: &Shared, id: &str) {
    if let Some(job) = app.host.jobs.lock().unwrap().remove(id) {
        let _ = std::fs::remove_dir_all(&job.req.out_dir);
    }
    engine::discard_clip(id);
}

async fn download(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let file = {
        let jobs = app.host.jobs.lock().unwrap();
        let job = jobs.get(&id).ok_or_else(not_found)?;
        match (&job.status, &job.file) {
            (Status::Done, Some(f)) => f.clone(),
            _ => return Err(ApiError(StatusCode::CONFLICT, "That clip isn't finished yet.".into())),
        }
    };
    let f = tokio::fs::File::open(&file).await.map_err(|_| not_found())?;
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "clip.mp4".into());
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || " ._-[]()".contains(c) { c } else { '_' }).collect();
    let disposition = format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", percent_encode(&name));
    let ctype = match file.extension().and_then(|e| e.to_str()) {
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("opus") => "audio/ogg",
        _ => "application/octet-stream",
    };
    Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(f)))
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

// ------------------------------------------------------------------ housekeeping

/// Delete finished clips after a while, and paused ones that were forgotten.
fn spawn_cleanup(app: Shared) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(300)).await;
            let stale: Vec<String> = app
                .host
                .jobs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, j)| match j.status {
                    Status::Running => false,
                    Status::Paused => j.updated.elapsed() > Duration::from_secs(6 * 3600),
                    _ => j.updated.elapsed() > app.cfg.file_ttl,
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                remove_job(&app, &id);
            }
        }
    });
}

// ------------------------------------------------------------------ main

/// The addresses allowed by TRIMLY_ALLOW_ORIGIN. A browser's origin never ends
/// in a slash, so a trailing one (an easy mistake to type) is ignored.
fn parse_origins(list: &str) -> Vec<HeaderValue> {
    list.split(',')
        .map(|o| o.trim().trim_end_matches('/'))
        .filter(|o| !o.is_empty())
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect()
}

fn router(app: Shared) -> Router {
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/tools", get(tools))
        .route("/api/info", post(info))
        .route("/api/preview", post(preview))
        .route("/api/preview/:id/frame", get(frame))
        .route("/api/clips", post(create_clip))
        .route("/api/clips/:id", get(clip_status))
        .route("/api/clips/:id/pause", post(pause_clip))
        .route("/api/clips/:id/resume", post(resume_clip))
        .route("/api/clips/:id/cancel", post(cancel_clip))
        .route("/api/clips/:id/file", get(download))
        .layer(middleware::from_fn_with_state(app.clone(), auth));

    let origins = env("TRIMLY_ALLOW_ORIGIN");
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .allow_origin(match origins.as_deref() {
            None | Some("*") => AllowOrigin::any(),
            Some(list) => AllowOrigin::list(parse_origins(list)),
        });

    let mut router = api.with_state(app).layer(cors);
    if let Some(dir) = env("TRIMLY_STATIC_DIR") {
        let index = PathBuf::from(&dir).join("index.html");
        router = router.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)));
    }
    router
}

#[tokio::main]
async fn main() {
    let cfg = Config {
        access_key: env("TRIMLY_ACCESS_KEY"),
        data_dir: PathBuf::from(env("TRIMLY_DATA_DIR").unwrap_or_else(|| "data".into())),
        max_jobs: env_num("TRIMLY_MAX_JOBS", 4),
        max_parts: env_num("TRIMLY_MAX_PARTS", 4),
        max_clip_secs: env_num("TRIMLY_MAX_CLIP_SECS", 1800.0),
        file_ttl: Duration::from_secs(60 * env_num("TRIMLY_FILE_TTL_MINS", 120u64)),
        allow_private: env("TRIMLY_ALLOW_PRIVATE").as_deref() == Some("1"),
    };
    std::fs::create_dir_all(cfg.data_dir.join("out")).expect("can't create the data folder");
    engine::set_cookies_file(env("TRIMLY_COOKIES_FILE"));

    let app = Arc::new(AppState {
        cfg: cfg.clone(),
        host: ServerHost { state: Arc::new(ClipState::default()), jobs: Arc::new(Mutex::new(HashMap::new())) },
        previews: Mutex::new(HashMap::new()),
    });
    spawn_cleanup(app.clone());

    let port: u16 = env_num("PORT", 8787);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.expect("can't listen on that port");
    let tools = engine::check_tools();
    println!("Trimly server on http://0.0.0.0:{port}");
    println!("  yt-dlp: {}", tools.ytdlp.as_deref().unwrap_or("NOT FOUND"));
    println!("  ffmpeg: {}", tools.ffmpeg.as_deref().unwrap_or("NOT FOUND"));
    println!("  access key: {}", if cfg.access_key.is_some() { "required" } else { "none (anyone can use this server)" });
    axum::serve(listener, router(app)).await.expect("server stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_local_addresses() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.5", "172.16.0.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fe80::1", "fd00::1", "::ffff:127.0.0.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["8.8.8.8", "142.250.80.46", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[test]
    fn rejects_bad_links() {
        let cfg = Config {
            access_key: None,
            data_dir: PathBuf::from("."),
            max_jobs: 1,
            max_parts: 4,
            max_clip_secs: 60.0,
            file_ttl: Duration::from_secs(1),
            allow_private: false,
        };
        assert!(check_link(&cfg, "not a link").is_err());
        assert!(check_link(&cfg, "file:///etc/passwd").is_err());
        assert!(check_link(&cfg, "http://127.0.0.1:8080/x").is_err());
        assert!(check_link(&cfg, "http://169.254.169.254/latest/meta-data").is_err());
    }

    #[test]
    fn allowed_origins_ignore_trailing_slashes_and_spaces() {
        let got: Vec<String> = parse_origins(" https://a.example/ , https://b.example,, ")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(got, vec!["https://a.example", "https://b.example"]);
    }

    #[test]
    fn compares_keys_in_constant_time() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secrets"));
    }

    #[test]
    fn only_simple_ids_are_accepted() {
        assert!(safe_id("a1b2-c3"));
        assert!(!safe_id("../etc"));
        assert!(!safe_id(""));
    }

    #[test]
    fn encodes_file_names_for_downloads() {
        assert_eq!(percent_encode("a b é.mp4"), "a%20b%20%C3%A9.mp4");
    }
}
