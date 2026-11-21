//! The Trimly engine: reads links, downloads just the chosen part of a video
//! with yt-dlp and ffmpeg, crops, joins parts, and supports pause and cancel.
//!
//! It knows nothing about windows or web servers. A `Host` reports progress
//! and holds the running-job table; the desktop app and the web server each
//! provide one.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub mod chunked;
pub mod proxy;

/// What the engine needs from whatever is running it.
pub trait Host: Clone + Send + Sync + 'static {
    /// The table of running clips.
    fn state(&self) -> &ClipState;
    /// Tell the user how a clip is doing.
    fn emit(&self, progress: Progress);
}

/// One running clip: the child process (yt-dlp, then ffmpeg for cropping) and
/// whether the user asked to stop it.
#[derive(Default)]
struct Job {
    /// The single tool process of a standard download.
    child: Option<Child>,
    /// The several ffmpeg processes of a download in parts.
    children: Vec<Child>,
    cancelled: bool,
    /// Stopped on purpose, keeping the finished parts so it can resume.
    paused: bool,
    /// True once the clip is downloading in parts, which can be paused.
    resumable: bool,
}

/// Stop every process this job started, and anything they started.
fn kill_job(job: &mut Job) {
    if let Some(c) = job.child.as_mut() {
        kill_tree(c);
    }
    for c in job.children.iter_mut() {
        kill_tree(c);
    }
}

fn kill_children<H: Host>(app: &H, id: &str) {
    if let Some(job) = app.state().jobs.lock().unwrap().get_mut(id) {
        kill_job(job);
    }
}

fn set_resumable<H: Host>(app: &H, id: &str, yes: bool) {
    app.state()
        .jobs
        .lock()
        .unwrap()
        .entry(id.to_string())
        .or_default()
        .resumable = yes;
}

/// All running clips by id, so several can run at once and be cancelled
/// individually from the queue.
#[derive(Default)]
pub struct ClipState {
    jobs: Mutex<HashMap<String, Job>>,
}

// ---------------------------------------------------------------- binaries

/// GUI apps on macOS/Linux start with a minimal PATH, so also look in the
/// usual install locations (Homebrew, ~/.local/bin, snap, ...).
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    dirs
}

/// User-chosen locations from Settings: (yt-dlp, ffmpeg).
static TOOL_PATHS: Mutex<(Option<String>, Option<String>)> = Mutex::new((None, None));

/// Use these locations for yt-dlp and ffmpeg (empty means look for them).
pub fn set_tool_paths(ytdlp: Option<String>, ffmpeg: Option<String>) {
    let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    *TOOL_PATHS.lock().unwrap() = (clean(ytdlp), clean(ffmpeg));
}

/// A cookies file (Netscape format) yt-dlp should use, e.g. for sites that
/// ask servers to sign in. Used by the web server.
static COOKIES_FILE: Mutex<Option<String>> = Mutex::new(None);

pub fn set_cookies_file(path: Option<String>) {
    *COOKIES_FILE.lock().unwrap() = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
}

fn cookies_file() -> Option<String> {
    COOKIES_FILE.lock().unwrap().clone()
}

pub fn find_bin(name: &str) -> Option<PathBuf> {
    {
        let paths = TOOL_PATHS.lock().unwrap();
        let custom = if name == "yt-dlp" { &paths.0 } else { &paths.1 };
        if let Some(p) = custom.as_ref().map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(p);
        }
    }
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    search_dirs()
        .into_iter()
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

fn command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    if let Ok(path) = std::env::join_paths(search_dirs()) {
        cmd.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        // Own process group, so cancelling can stop helper processes too
        // (yt-dlp may start ffmpeg or a JavaScript runtime).
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Stop a process and everything it started.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .output();
    }
    let _ = child.kill();
}

pub fn first_line_of(bin: &Path, arg: &str) -> Option<String> {
    let out = command(bin).arg(arg).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
pub struct ToolStatus {
    pub ytdlp: Option<String>,
    pub ffmpeg: Option<String>,
}

/// Which tools are installed, with their versions.
pub fn check_tools() -> ToolStatus {
    ToolStatus {
        ytdlp: find_bin("yt-dlp").and_then(|b| first_line_of(&b, "--version")),
        ffmpeg: find_bin("ffmpeg").and_then(|b| first_line_of(&b, "-version")),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub title: String,
    /// Source picture size, used to preview the crop.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub uploader: Option<String>,
    pub is_live: bool,
}

/// Look up a link: title, length, picture size and so on.
pub fn fetch_info(url: &str) -> Result<VideoInfo, String> {
    ensure_http(url)?;
    let ytdlp = find_bin("yt-dlp").ok_or("yt-dlp is not installed.")?;
    let mut cmd = command(&ytdlp);
    cmd.args(["-J", "--no-playlist", "--no-warnings"]);
    if let Some(f) = cookies_file() {
        cmd.args(["--cookies", &f]);
    }
    let out = cmd.arg("--").arg(url).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(error_from(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("Bad yt-dlp output: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    Ok(VideoInfo {
        title: s("title").unwrap_or_else(|| "Untitled".into()),
        width: v.get("width").and_then(|x| x.as_u64()).map(|n| n as u32),
        height: v.get("height").and_then(|x| x.as_u64()).map(|n| n as u32),
        duration: v.get("duration").and_then(|x| x.as_f64()),
        thumbnail: s("thumbnail"),
        uploader: s("uploader").or_else(|| s("channel")),
        is_live: v.get("is_live").and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

pub fn ensure_http(url: &str) -> Result<(), String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err("Please enter a full http(s) link.".into())
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClipRequest {
    /// Queue id; progress events and cancel use it.
    pub id: String,
    pub url: String,
    pub start: f64,
    pub end: f64,
    pub out_dir: String,
    /// "video" | "audio"
    pub kind: String,
    /// Max height in pixels; None = best available.
    pub max_height: Option<u32>,
    pub is_live: bool,
    /// "mp3" | "m4a" | "opus" (audio kind only)
    pub audio_format: Option<String>,
    /// Re-encode at the cut points for frame-accurate clips.
    pub exact: bool,
    /// "chrome" | "firefox" | "safari" | ... or None
    pub cookies_browser: Option<String>,
    /// yt-dlp output template without extension, e.g. "%(title)s [%(id)s] clip"
    pub template: Option<String>,
    /// Video only: crop the picture to an aspect ratio (e.g. 9:16 for reels).
    pub crop: Option<CropSpec>,
    /// How many parts download at once. 1 turns the feature off.
    pub parts: u32,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CropSpec {
    pub aspect_w: u32,
    pub aspect_h: u32,
    /// Window position across the free width, 0 = left, 1 = right.
    pub x: f64,
    /// Window position down the free height, 0 = top, 1 = bottom.
    pub y: f64,
}

#[derive(Serialize, Clone)]
pub struct Progress {
    pub id: String,
    /// "starting" | "reading" | "preparing" | "downloading" | "finishing" | "cropping"
    pub stage: String,
    /// None while the amount of work done isn't known yet.
    pub percent: Option<f64>,
    pub detail: String,
    /// Bytes saved to disk so far, when known.
    pub bytes: Option<u64>,
    /// True when this clip can be paused and resumed.
    pub resumable: bool,
}

/// Stop a clip for good. A cancel that arrives before the clip has started
/// is remembered, and the clip stops as soon as it begins.
pub fn cancel_clip<H: Host>(app: &H, id: &str) {
    let mut jobs = app.state().jobs.lock().unwrap();
    let job = jobs.entry(id.to_string()).or_default();
    job.cancelled = true;
    kill_job(job);
}

/// Pause a clip that is downloading in parts. Finished parts are kept, so
/// running it again with the same id carries on. Returns false if this clip
/// can't be paused (it isn't downloading in parts).
pub fn pause_job<H: Host>(app: &H, id: &str) -> bool {
    let state = app.state();
    let mut jobs = state.jobs.lock().unwrap();
    match jobs.get_mut(id) {
        Some(job) if job.resumable => {
            job.paused = true;
            kill_job(job);
            true
        }
        _ => false,
    }
}

/// Delete the saved parts of a paused clip that won't be resumed.
pub fn discard_clip(id: &str) {
    let _ = std::fs::remove_dir_all(chunked::work_dir(id));
}

/// A direct address of a small version of the video, which ffmpeg reads to
/// pull single frames for the preview. None when the site offers none.
pub fn preview_url(url: &str, cookies_browser: Option<&str>) -> Result<Option<String>, String> {
    ensure_http(url)?;
    let ytdlp = find_bin("yt-dlp").ok_or("yt-dlp is not installed.")?;
    let mut cmd = command(&ytdlp);
    cmd.args(["-g", "--no-playlist", "--no-warnings", "-f", "bv[height<=360]/b[height<=360]/bv*/b"]);
    if let Some(b) = cookies_browser {
        if COOKIE_BROWSERS.contains(&b) {
            cmd.args(["--cookies-from-browser", b]);
        }
    }
    if let Some(f) = cookies_file() {
        cmd.args(["--cookies", &f]);
    }
    let out = cmd.arg("--").arg(url).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("http"))
        .map(String::from))
}

/// Browsers whose sign-in yt-dlp may borrow.
const COOKIE_BROWSERS: [&str; 7] = ["chrome", "firefox", "safari", "edge", "brave", "opera", "vivaldi"];

/// The preview frames being fetched right now, one per caller (`slot`), tagged
/// with a request number. A newer request from the same caller stops its older
/// one. The numbers are handed out when a request arrives, so if requests
/// start out of order the older one never cancels the newer.
static PREVIEW: Mutex<Option<HashMap<String, (u64, Child)>>> = Mutex::new(None);
static PREVIEW_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Returned when a newer request for the same preview replaced this one.
pub const SUPERSEDED: &str = "Superseded.";

const PREVIEW_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

/// One picture from the video at `time` seconds, as a data address the page can show.
/// A failure is tried once more, since the first read of a long stream can fail
/// for passing reasons.
pub fn preview_frame(slot: &str, source: &str, time: f64) -> Result<String, String> {
    ensure_http(source)?;
    if !time.is_finite() || time < 0.0 {
        return Err("Bad time.".into());
    }
    let seq = PREVIEW_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    match preview_frame_once(slot, seq, source, time) {
        Err(e) if e != SUPERSEDED => {
            if is_newest_preview(slot, seq) {
                preview_frame_once(slot, seq, source, time)
            } else {
                Err(SUPERSEDED.into())
            }
        }
        other => other,
    }
}

/// False once a newer request for this slot has arrived.
fn is_newest_preview(slot: &str, seq: u64) -> bool {
    let guard = PREVIEW.lock().unwrap();
    guard
        .as_ref()
        .and_then(|m| m.get(slot))
        .map(|(current, _)| *current <= seq)
        .unwrap_or(true)
}

fn preview_frame_once(slot: &str, seq: u64, source: &str, time: f64) -> Result<String, String> {
    use base64::Engine;
    let ffmpeg = find_bin("ffmpeg").ok_or("ffmpeg is not installed.")?;
    let mut child = command(&ffmpeg)
        .args(["-loglevel", "error", "-nostdin", "-rw_timeout", "20000000", "-user_agent", PREVIEW_UA])
        .args(["-noaccurate_seek", "-ss", &format!("{time:.2}"), "-i"])
        .arg(source)
        .args(["-frames:v", "1", "-vf", "scale=640:-2", "-q:v", "4", "-f", "mjpeg", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start ffmpeg: {e}"))?;
    let mut out = child.stdout.take().ok_or("No output.")?;
    let mut err = child.stderr.take().ok_or("No output.")?;
    let pid = child.id();
    {
        let mut guard = PREVIEW.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        match map.get(slot) {
            // A newer request is already running: this one is out of date.
            Some((current, _)) if *current > seq => {
                kill_tree(&mut child);
                let _ = child.wait();
                return Err(SUPERSEDED.into());
            }
            Some(_) => {
                if let Some((_, mut old)) = map.remove(slot) {
                    kill_tree(&mut old);
                    let _ = old.wait();
                }
            }
            None => {}
        }
        map.insert(slot.to_string(), (seq, child));
    }
    // ffmpeg's messages are short; read them on the side so a full pipe can't block it.
    let messages = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let mut buf = Vec::new();
    let _ = out.read_to_end(&mut buf);
    {
        let mut guard = PREVIEW.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            if map.get(slot).map(|(s, c)| (*s, c.id())) == Some((seq, pid)) {
                if let Some((_, mut c)) = map.remove(slot) {
                    let _ = c.wait();
                }
            }
        }
    }
    let said = messages.join().unwrap_or_default();
    if buf.is_empty() {
        // Replaced by a newer request while running?
        if !is_newest_preview(slot, seq) {
            return Err(SUPERSEDED.into());
        }
        let reason = error_from(&said);
        return Err(if said.trim().is_empty() { "No picture at that time.".into() } else { reason });
    }
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf)))
}

// ---------------------------------------------------------------- clipping

fn emit<H: Host>(
    app: &H,
    id: &str,
    stage: &str,
    percent: Option<f64>,
    detail: impl Into<String>,
    bytes: Option<u64>,
) {
    emit_full(app, id, stage, percent, detail, bytes, false);
}

fn emit_full<H: Host>(
    app: &H,
    id: &str,
    stage: &str,
    percent: Option<f64>,
    detail: impl Into<String>,
    bytes: Option<u64>,
    resumable: bool,
) {
    app.emit(Progress {
        id: id.to_string(),
        stage: stage.to_string(),
        percent,
        detail: detail.into(),
        bytes,
        resumable,
    });}

/// How much of the file has been written so far (it grows as a `.part` file).
fn written_bytes(dest: &Path) -> Option<u64> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    [part, dest.to_path_buf()]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .max()
}

/// Cancelled or paused: either way the work should stop now.
fn is_stopped<H: Host>(app: &H, id: &str) -> bool {
    app.state()
        .jobs
        .lock()
        .unwrap()
        .get(id)
        .map(|j| j.cancelled || j.paused)
        .unwrap_or(false)
}

fn is_cancelled<H: Host>(app: &H, id: &str) -> bool {
    app.state()
        .jobs
        .lock()
        .unwrap()
        .get(id)
        .map(|j| j.cancelled)
        .unwrap_or(false)
}

/// Register a spawned child so `cancel_clip` can kill it. If the user already
/// cancelled while we were starting up, kill it straight away.
fn set_child<H: Host>(app: &H, id: &str, mut child: Child) {
    let state = app.state();
    let mut jobs = state.jobs.lock().unwrap();
    let job = jobs.entry(id.to_string()).or_default();
    if job.cancelled {
        kill_tree(&mut child);
    }
    job.child = Some(child);
}

enum Next {
    Line(String),
    /// A second passed with no output.
    Tick,
    End,
}

/// Next output line from a tool. Wakes up about once a second so progress can
/// keep updating while the tool is quiet. Ends when the tool does, or when the
/// job is cancelled: a killed tool can leave helper processes holding its
/// pipes open, and we must not wait for them.
fn next_event<H: Host>(rx: &mpsc::Receiver<String>, app: &H, id: &str) -> Next {
    match rx.recv_timeout(Duration::from_millis(1000)) {
        Ok(line) => {
            if is_stopped(app, id) {
                Next::End
            } else {
                Next::Line(line)
            }
        }
        Err(RecvTimeoutError::Timeout) => {
            if is_stopped(app, id) {
                Next::End
            } else {
                Next::Tick
            }
        }
        Err(RecvTimeoutError::Disconnected) => Next::End,
    }
}

fn finish_child<H: Host>(app: &H, id: &str) -> Option<bool> {
    let child = app
        .state()
        .jobs
        .lock()
        .unwrap()
        .get_mut(id)
        .and_then(|j| j.child.take());
    child.and_then(|mut c| c.wait().ok()).map(|s| s.success())
}

/// Run a clip to the end. Blocks until it finishes, fails, is paused (`Err("Paused.")`)
/// or is cancelled (`Err("Cancelled.")`).
pub fn run_clip<H: Host>(app: &H, req: &ClipRequest) -> Result<String, String> {
    let id = req.id.clone();
    app.state()
        .jobs
        .lock()
        .unwrap()
        .entry(id.clone())
        .or_default();
    let result = if is_cancelled(app, &id) {
        Err("Cancelled.".to_string())
    } else {
        run_clip_inner(app, req)
    };
    app.state().jobs.lock().unwrap().remove(&id);
    result
}

fn run_clip_inner<H: Host>(app: &H, req: &ClipRequest) -> Result<String, String> {
    ensure_http(&req.url)?;
    if !(req.start >= 0.0 && req.end > req.start) {
        return Err("End time must be after start time.".into());
    }
    let crop = match (&req.crop, req.kind.as_str()) {
        (Some(c), "video") => Some(crop_filter(c)?),
        _ => None,
    };
    let ytdlp = find_bin("yt-dlp").ok_or("yt-dlp is not installed.")?;
    let ffmpeg = find_bin("ffmpeg").ok_or("ffmpeg is not installed.")?;
    let ffmpeg_dir = ffmpeg.parent().unwrap_or(Path::new("."));
    if !Path::new(&req.out_dir).is_dir() {
        return Err("The output folder does not exist.".into());
    }

    let clip_len = req.end - req.start;
    // With a crop step to follow, downloading is the first 70% of the work.
    let scale = if crop.is_some() { 0.7 } else { 1.0 };
    let name = req
        .template
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("%(title).150B [%(id)s] clip");
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("The file name can't contain slashes.".into());
    }
    let template = Path::new(&req.out_dir).join(format!("{name}.%(ext)s"));

    // Long video clips download in parts: faster, and they can be paused.
    if req.kind == "video" && !req.is_live && req.exact && req.parts > 1 && clip_len >= chunked::MIN_CLIP {
        match chunked::run(app, req, &ytdlp, &ffmpeg, &template, crop.as_deref()) {
            Ok(path) => return Ok(path),
            Err(e) if e == "Cancelled." || e == "Paused." => return Err(e),
            Err(e) => {
                // Not every site works this way; carry on with the standard download.
                eprintln!("[trimly] download in parts failed, using the standard method: {e}");
                set_resumable(app, &req.id, false);
                emit(app, &req.id, "starting", None, "Switching to the standard download…", None);
            }
        }
    }

    let mut args: Vec<OsString> = Vec::new();
    let mut push = |a: &str| args.push(a.into());
    push("--no-playlist");
    push("--newline");
    push("--progress");
    push("--windows-filenames");
    push("--download-sections");
    push(&format!("*{}-{}", req.start, req.end));
    // Re-encode around the cut points so the clip starts/ends exactly where
    // requested instead of snapping to the nearest keyframe.
    if req.exact {
        push("--force-keyframes-at-cuts");
    }
    if let Some(b) = req.cookies_browser.as_deref() {
        if COOKIE_BROWSERS.contains(&b) {
            push("--cookies-from-browser");
            push(b);
        }
    }
    if let Some(f) = cookies_file() {
        push("--cookies");
        push(&f);
    }
    push("--progress-template");
    push("download:TRIMLY_PROGRESS %(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s");
    // The finished file's path goes to a small file. (`--print` would also work
    // but makes yt-dlp quiet, which silences ffmpeg's progress for sections.)
    let safe_id: String = req.id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let list_file = std::env::temp_dir().join(format!("trimly-{safe_id}.txt"));
    let _ = std::fs::remove_file(&list_file);
    push("--print-to-file");
    push("after_move:%(filepath)s");
    push(&list_file.to_string_lossy());
    if req.is_live {
        push("--live-from-start");
    }
    if req.kind == "audio" {
        push("-x");
        push("--audio-format");
        match req.audio_format.as_deref() {
            Some("m4a") => push("m4a"),
            Some("opus") => push("opus"),
            _ => push("mp3"),
        }
    } else {
        // Prefer H.264/AAC so the resulting MP4 plays everywhere.
        push("-S");
        match req.max_height {
            Some(h) => push(&format!("res:{h},vcodec:h264,acodec:m4a")),
            None => push("vcodec:h264,acodec:m4a"),
        }
        push("--merge-output-format");
        push("mp4");
    }
    args.push("--ffmpeg-location".into());
    args.push(ffmpeg_dir.into());
    args.push("-o".into());
    args.push(template.into());
    args.push("--".into());
    args.push(req.url.clone().into());

    let mut child = command(&ytdlp)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start yt-dlp: {e}"))?;

    let (tx, rx) = mpsc::channel::<String>();
    pump(child.stdout.take().unwrap(), tx.clone());
    pump(child.stderr.take().unwrap(), tx);
    set_child(app, &req.id, child);

    let mut tail: Vec<String> = Vec::new();
    let mut stage = "starting";
    let mut pct: Option<f64> = None;
    let mut detail = String::from("Starting up…");
    let mut dest: Option<PathBuf> = None;
    let mut bytes: Option<u64> = None;
    let mut last_emit = Instant::now();
    emit(app, &req.id, stage, pct, detail.clone(), bytes);
    loop {
        let mut changed = false;
        match next_event(&rx, app, &req.id) {
            Next::End => break,
            Next::Tick => {}
            Next::Line(line) => {
                if let Some(rest) = line.strip_prefix("TRIMLY_PROGRESS ") {
                    let mut it = rest.split('|').map(str::trim);
                    pct = it
                        .next()
                        .and_then(|p| p.trim_end_matches('%').parse::<f64>().ok())
                        .map(|p| p * scale);
                    let speed = it.next().unwrap_or("");
                    let eta = it.next().unwrap_or("");
                    stage = "downloading";
                    detail = format!("{speed}  ETA {eta}");
                    changed = true;
                } else {
                    if let Some(path) = line.strip_prefix("[download] Destination:") {
                        dest = Some(PathBuf::from(path.trim()));
                        stage = "downloading";
                        detail = "Connecting to the video server…".into();
                        changed = true;
                    } else if let Some(secs) = parse_ffmpeg_time(&line) {
                        // ffmpeg (used for sectioned downloads) reports `time=HH:MM:SS.xx`.
                        stage = "downloading";
                        pct = Some((secs / clip_len * 100.0).clamp(0.0, 99.0) * scale);
                        detail = "Downloading the clip…".into();
                        changed = true;
                    } else if line.contains("[Merger]")
                        || line.contains("[ExtractAudio]")
                        || line.contains("[VideoRemuxer]")
                        || line.contains("[VideoConvertor]")
                    {
                        stage = "finishing";
                        pct = Some(99.0 * scale);
                        detail = "Finishing the file…".into();
                        changed = true;
                    } else if stage == "starting" || stage == "reading" || stage == "preparing" {
                        if line.starts_with("[info]") && line.contains("Downloading") {
                            stage = "preparing";
                            detail = "Preparing the download…".into();
                            changed = true;
                        } else if line.starts_with('[') && !line.starts_with("[download]") && stage == "starting" {
                            stage = "reading";
                            detail = "Reading the link…".into();
                            changed = true;
                        }
                    }
                    tail.push(line);
                    if tail.len() > 12 {
                        tail.remove(0);
                    }
                }
            }
        }
        // Even when the tool is quiet, report how much has been saved so far.
        if last_emit.elapsed() >= Duration::from_secs(1) || changed {
            if let Some(d) = &dest {
                bytes = written_bytes(d).or(bytes);
            }
            emit(app, &req.id, stage, pct, detail.clone(), bytes);
            last_emit = Instant::now();
        }
    }

    let ok = finish_child(app, &req.id);
    if is_cancelled(app, &req.id) {
        return Err("Cancelled.".into());
    }
    let file = std::fs::read_to_string(&list_file)
        .ok()
        .and_then(|t| t.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(String::from));
    let _ = std::fs::remove_file(&list_file);
    let downloaded = match (ok, file) {
        (Some(true), Some(f)) => f,
        (Some(true), None) => {
            return Err("Finished, but the output file could not be located.".into())
        }
        _ => return Err(error_from(&tail.join("\n"))),
    };

    match (crop, &req.crop) {
        (Some(filter), Some(spec)) => {
            run_crop(app, &req.id, &ffmpeg, &downloaded, &filter, spec, clip_len)
        }
        _ => Ok(downloaded),
    }
}

// ---------------------------------------------------------------- cropping

/// Build an ffmpeg `crop` filter for the requested aspect ratio. Sizes are
/// derived from the source (`iw`/`ih`) so the app doesn't need to know them.
/// `x`/`y` pick where the window sits, from 0 (left/top) to 1 (right/bottom).
fn crop_filter(c: &CropSpec) -> Result<String, String> {
    if c.aspect_w == 0 || c.aspect_h == 0 || c.aspect_w > 100 || c.aspect_h > 100 {
        return Err("That crop shape isn't supported.".into());
    }
    let (a, b) = (c.aspect_w, c.aspect_h);
    let x = c.x.clamp(0.0, 1.0);
    let y = c.y.clamp(0.0, 1.0);
    Ok(format!(
        "crop=w='trunc(min(iw\\,ih*{a}/{b})/2)*2':h='trunc(min(ih\\,iw*{b}/{a})/2)*2':x='(iw-ow)*{x:.4}':y='(ih-oh)*{y:.4}'"
    ))
}

fn run_crop<H: Host>(
    app: &H,
    id: &str,
    ffmpeg: &Path,
    input: &str,
    filter: &str,
    spec: &CropSpec,
    clip_len: f64,
) -> Result<String, String> {
    let input_path = PathBuf::from(input);
    let stem = input_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let output = input_path.with_file_name(format!("{stem} {}x{}.mp4", spec.aspect_w, spec.aspect_h));

    emit(app, id, "cropping", Some(70.0), "Cropping the picture…", None);
    let mut child = command(ffmpeg)
        .args(["-y", "-nostdin", "-loglevel", "error", "-i"])
        .arg(&input_path)
        .args(["-vf", filter])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "copy", "-movflags", "+faststart"])
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(&output)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start ffmpeg: {e}"))?;

    let (tx, rx) = mpsc::channel::<String>();
    pump(child.stdout.take().unwrap(), tx.clone());
    pump(child.stderr.take().unwrap(), tx);
    set_child(app, id, child);

    let mut tail: Vec<String> = Vec::new();
    loop {
        match next_event(&rx, app, id) {
            Next::End => break,
            Next::Tick => {}
            Next::Line(line) => {
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                    let pct = 70.0 + (us / 1_000_000.0 / clip_len * 29.0).clamp(0.0, 29.0);
                    emit(app, id, "cropping", Some(pct), "Cropping the picture…", None);
                } else if !line.contains('=') {
                    tail.push(line);
                    if tail.len() > 8 {
                        tail.remove(0);
                    }
                }
            }
        }
    }

    let ok = finish_child(app, id);
    if is_cancelled(app, id) {
        let _ = std::fs::remove_file(&output);
        return Err("Cancelled.".into());
    }
    if ok != Some(true) {
        let _ = std::fs::remove_file(&output);
        return Err(error_from(&tail.join("\n")));
    }
    let _ = std::fs::remove_file(&input_path);
    Ok(output.to_string_lossy().into_owned())
}

/// Read a pipe and forward each line (split on \n *and* \r, since ffmpeg
/// rewrites its status line with carriage returns) to the channel.
fn pump<R: Read + Send + 'static>(mut reader: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut acc: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if b == b'\n' || b == b'\r' {
                            if !acc.is_empty() {
                                let _ = tx.send(String::from_utf8_lossy(&acc).into_owned());
                                acc.clear();
                            }
                        } else {
                            acc.push(b);
                        }
                    }
                }
            }
        }
        if !acc.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&acc).into_owned());
        }
    });
}

fn parse_ffmpeg_time(line: &str) -> Option<f64> {
    let idx = line.find("time=")?;
    let t = line[idx + 5..].split_whitespace().next()?;
    let mut secs = 0.0;
    for part in t.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs)
}

/// Prefer yt-dlp's own `ERROR:` line over raw noise.
fn error_from(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp failed.")
        .trim()
        .to_string()
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Arc;

    /// A host for tests: keeps the job table and remembers progress reports.
    #[derive(Clone, Default)]
    pub struct TestHost {
        pub state: Arc<ClipState>,
        pub reports: Arc<Mutex<Vec<Progress>>>,
    }

    impl Host for TestHost {
        fn state(&self) -> &ClipState {
            &self.state
        }
        fn emit(&self, p: Progress) {
            self.reports.lock().unwrap().push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_time() {
        let l = "frame= 120 fps=30 size=1024kB time=00:01:05.50 bitrate=128kbits/s";
        assert_eq!(parse_ffmpeg_time(l), Some(65.5));
        assert_eq!(parse_ffmpeg_time("no time here"), None);
    }

    #[test]
    fn picks_error_line() {
        assert_eq!(error_from("warn\nERROR: Video unavailable\n"), "ERROR: Video unavailable");
    }

    #[test]
    fn builds_vertical_crop_filter() {
        let f = crop_filter(&CropSpec { aspect_w: 9, aspect_h: 16, x: 0.5, y: 0.5 }).unwrap();
        assert!(f.starts_with("crop=w='trunc(min(iw\\,ih*9/16)/2)*2'"));
        assert!(f.contains("x='(iw-ow)*0.5000'"));
    }

    #[test]
    fn rejects_bad_crop() {
        let bad = CropSpec { aspect_w: 0, aspect_h: 16, x: 0.0, y: 0.0 };
        assert!(crop_filter(&bad).is_err());
    }

    /// A tool that starts a helper keeps the output pipe open after the tool
    /// itself is killed. Cancelling must stop the helper too.
    #[cfg(unix)]
    #[test]
    fn kill_tree_stops_helpers_holding_the_pipe() {
        let mut child = command(Path::new("/bin/sh"))
            .args(["-c", "sleep 30 & wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = child.stdout.take().unwrap();
        let t = Instant::now();
        kill_tree(&mut child);
        let mut buf = Vec::new();
        out.read_to_end(&mut buf).unwrap(); // would block ~30s if the helper survived
        let _ = child.wait();
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn cancel_before_start_is_remembered() {
        let host = testing::TestHost::default();
        cancel_clip(&host, "early");
        let req = ClipRequest {
            id: "early".into(),
            url: "https://example.com/v".into(),
            start: 0.0,
            end: 10.0,
            out_dir: ".".into(),
            kind: "video".into(),
            max_height: None,
            is_live: false,
            audio_format: None,
            exact: true,
            cookies_browser: None,
            template: None,
            crop: None,
            parts: 1,
        };
        assert_eq!(run_clip(&host, &req), Err("Cancelled.".to_string()));
    }
}
