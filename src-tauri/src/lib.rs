//! The desktop app: a thin Tauri layer over the Trimly engine.

use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};
use trimly_engine::{self as engine, ClipRequest, ClipState, Host, Progress, ToolStatus, VideoInfo};

/// Lets the engine report progress to the window and keep its job table in
/// the app's managed state.
#[derive(Clone)]
struct TauriHost(AppHandle);

impl Host for TauriHost {
    fn state(&self) -> &ClipState {
        self.0.state::<ClipState>().inner()
    }
    fn emit(&self, progress: Progress) {
        let _ = self.0.emit("clip-progress", progress);
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn set_tool_paths(ytdlp: Option<String>, ffmpeg: Option<String>) {
    engine::set_tool_paths(ytdlp, ffmpeg);
}

#[tauri::command]
async fn check_tools() -> ToolStatus {
    blocking(engine::check_tools).await.unwrap_or(ToolStatus { ytdlp: None, ffmpeg: None })
}

#[tauri::command]
async fn fetch_info(url: String) -> Result<VideoInfo, String> {
    blocking(move || engine::fetch_info(&url)).await?
}

#[tauri::command]
async fn preview_url(url: String, cookies_browser: Option<String>) -> Result<Option<String>, String> {
    blocking(move || engine::preview_url(&url, cookies_browser.as_deref())).await?
}

#[tauri::command]
async fn preview_frame(source: String, time: f64) -> Result<String, String> {
    blocking(move || engine::preview_frame("local", &source, time)).await?
}

#[tauri::command]
async fn clip(app: AppHandle, req: ClipRequest) -> Result<String, String> {
    blocking(move || engine::run_clip(&TauriHost(app), &req)).await?
}

#[tauri::command]
fn cancel_clip(app: AppHandle, id: String) {
    engine::cancel_clip(&TauriHost(app), &id);
}

#[tauri::command]
fn pause_clip(app: AppHandle, id: String) -> bool {
    engine::pause_job(&TauriHost(app), &id)
}

#[tauri::command]
fn discard_clip(id: String) {
    engine::discard_clip(&id);
}

#[tauri::command]
fn default_dir(app: AppHandle) -> Option<String> {
    app.path().download_dir().ok().map(|p| p.to_string_lossy().into_owned())
}

/// Open a web link in the person's browser. Only plain https links are
/// accepted, and the address is passed as a single argument (never through a shell).
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.len() > 2000 {
        return Err("That isn't a link Trimly can open.".into());
    }
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32").arg("url.dll,FileProtocolHandler").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(&url).spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal(path: String) {
    let p = PathBuf::from(&path);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", p.display()))
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(std::path::Path::new("/")))
        .spawn();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ClipState::default())
        .invoke_handler(tauri::generate_handler![
            check_tools,
            fetch_info,
            preview_url,
            preview_frame,
            clip,
            cancel_clip,
            pause_clip,
            discard_clip,
            default_dir,
            reveal,
            open_external,
            set_tool_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Trimly");
}
