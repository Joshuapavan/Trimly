//! Parallel "download in parts" for video clips.
//!
//! The clip's time range is cut into chunks. Several ffmpeg processes fetch
//! and encode chunks at the same time (the video server limits each
//! connection, so more connections means a faster download). The audio is
//! fetched once as a single continuous piece so it can never drift out of
//! sync at the joins. At the end the chunks are joined with the audio.
//!
//! Finished chunks are kept in a work folder, so a paused clip can pick up
//! where it left off instead of starting over.

use super::*;
use std::collections::VecDeque;
use std::sync::Arc;

/// Aim for chunks this long (seconds), within these limits.
const MIN_CHUNK: f64 = 45.0;
const MAX_CHUNK: f64 = 150.0;
/// Clips shorter than this finish fast enough that parts aren't worth it.
pub const MIN_CLIP: f64 = 90.0;
/// Audio is tiny compared to video; it counts for this share of the progress.
const AUDIO_WEIGHT: f64 = 0.04;

struct Source {
    url: String,
    user_agent: String,
    /// Extra request headers as one ffmpeg `-headers` value ("Key: Value\r\n...").
    headers: String,
    /// All request headers (including the user agent) for our own requests.
    pairs: Vec<(String, String)>,
}

struct Plan {
    video: Source,
    audio: Source,
    /// Final file path without its extension.
    base: PathBuf,
}

pub fn work_dir(id: &str) -> PathBuf {
    let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    std::env::temp_dir().join(format!("trimly-{safe}"))
}

/// Ask yt-dlp for the direct stream addresses (and the file name it would use).
fn resolve(ytdlp: &Path, req: &ClipRequest, template: &Path) -> Result<Plan, String> {
    let mut cmd = command(ytdlp);
    cmd.args(["-j", "--no-playlist", "--no-warnings", "--windows-filenames", "-S"]);
    match req.max_height {
        Some(h) => cmd.arg(format!("res:{h},vcodec:h264,acodec:m4a")),
        None => cmd.arg("vcodec:h264,acodec:m4a"),
    };
    cmd.args(["-f", "bv*+ba/b"]);
    if let Some(b) = req.cookies_browser.as_deref() {
        if COOKIE_BROWSERS.contains(&b) {
            cmd.args(["--cookies-from-browser", b]);
        }
    }
    if let Some(f) = cookies_file() {
        cmd.args(["--cookies", &f]);
    }
    let out = cmd
        .arg("-o")
        .arg(template)
        .arg("--")
        .arg(&req.url)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(error_from(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;

    let formats: Vec<&serde_json::Value> = match v.get("requested_formats").and_then(|x| x.as_array()) {
        Some(list) if !list.is_empty() => list.iter().collect(),
        _ => vec![&v],
    };
    let has = |f: &serde_json::Value, key: &str| {
        f.get(key).and_then(|x| x.as_str()).map(|c| c != "none").unwrap_or(false)
    };
    let source = |f: &serde_json::Value| -> Option<Source> {
        let url = f.get("url")?.as_str()?.to_string();
        if !url.starts_with("http") {
            return None;
        }
        let mut user_agent = String::new();
        let mut headers = String::new();
        let mut pairs: Vec<(String, String)> = Vec::new();
        if let Some(map) = f.get("http_headers").and_then(|h| h.as_object()) {
            for (k, val) in map {
                let val = val.as_str().unwrap_or("");
                pairs.push((k.clone(), val.to_string()));
                if k.eq_ignore_ascii_case("user-agent") {
                    user_agent = val.to_string();
                } else {
                    headers.push_str(&format!("{k}: {val}\r\n"));
                }
            }
        }
        Some(Source { url, user_agent, headers, pairs })
    };
    // Streams that aren't plain files (HLS, DASH manifests) are left to the standard path.
    let plain = |f: &serde_json::Value| {
        matches!(f.get("protocol").and_then(|p| p.as_str()), Some("https") | Some("http"))
    };
    let video_fmt = formats.iter().find(|f| has(f, "vcodec")).ok_or("No video stream.")?;
    let audio_fmt = formats
        .iter()
        .find(|f| has(f, "acodec") && !has(f, "vcodec"))
        .or_else(|| formats.iter().find(|f| has(f, "acodec")))
        .ok_or("No audio stream.")?;
    if !plain(video_fmt) || !plain(audio_fmt) {
        return Err("This site doesn't offer plain files.".into());
    }
    let filename = v
        .get("_filename")
        .and_then(|x| x.as_str())
        .ok_or("yt-dlp gave no file name.")?;
    Ok(Plan {
        video: source(video_fmt).ok_or("No video address.")?,
        audio: source(audio_fmt).ok_or("No audio address.")?,
        base: PathBuf::from(filename).with_extension(""),
    })
}

/// Split [start, end) into chunks. Returns (chunk start, chunk length) pairs.
pub fn plan_chunks(start: f64, end: f64, parts: usize) -> Vec<(f64, f64)> {
    let total = end - start;
    let target = (total / (parts.max(1) as f64 * 3.0)).clamp(MIN_CHUNK, MAX_CHUNK);
    let n = (total / target).ceil().max(1.0) as usize;
    let len = total / n as f64;
    (0..n)
        .map(|i| (start + len * i as f64, if i + 1 == n { end - (start + len * i as f64) } else { len }))
        .collect()
}

/// State the workers share.
struct Shared {
    /// Seconds finished per chunk (index = chunk), plus audio last.
    done: Mutex<Vec<f64>>,
    queue: Mutex<VecDeque<usize>>,
    error: Mutex<Option<String>>,
}

/// Run one ffmpeg process to completion, reporting how far it got in `slot`.
fn run_ffmpeg<H: Host>(app: &H, id: &str, ffmpeg: &Path, args: &[OsString], shared: &Shared, slot: usize) -> Result<(), String> {
    let mut child = command(ffmpeg)
        .args(["-y", "-nostdin", "-loglevel", "error", "-progress", "pipe:1", "-nostats"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start ffmpeg: {e}"))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel::<String>();
    pump(child.stdout.take().unwrap(), tx.clone());
    pump(child.stderr.take().unwrap(), tx);
    {
        let state = app.state();
        let mut jobs = state.jobs.lock().unwrap();
        let job = jobs.entry(id.to_string()).or_default();
        if job.cancelled || job.paused {
            kill_tree(&mut child);
        }
        job.children.push(child);
    }

    let mut tail: Vec<String> = Vec::new();
    loop {
        match next_event(&rx, app, id) {
            Next::End => break,
            Next::Tick => {}
            Next::Line(line) => {
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                    if let Some(d) = shared.done.lock().unwrap().get_mut(slot) {
                        *d = us / 1_000_000.0;
                    }
                } else if !line.contains('=') {
                    tail.push(line);
                    if tail.len() > 8 {
                        tail.remove(0);
                    }
                }
            }
        }
    }

    // Take our child back out of the job and wait for it.
    let child = {
        let state = app.state();
        let mut jobs = state.jobs.lock().unwrap();
        jobs.get_mut(id).and_then(|j| {
            j.children
                .iter()
                .position(|c| c.id() == pid)
                .map(|i| j.children.remove(i))
        })
    };
    let ok = child.and_then(|mut c| c.wait().ok()).map(|s| s.success());
    if is_stopped(app, id) {
        return Err("Stopped.".into());
    }
    if ok == Some(true) {
        Ok(())
    } else {
        Err(error_from(&tail.join("\n")))
    }
}

fn input_args(src: &Source, start: f64, len: f64) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![
        "-rw_timeout".into(),
        "30000000".into(),
        "-reconnect".into(),
        "1".into(),
        "-reconnect_streamed".into(),
        "1".into(),
        "-reconnect_delay_max".into(),
        "5".into(),
    ];
    if !src.user_agent.is_empty() {
        a.extend(["-user_agent".into(), src.user_agent.clone().into()]);
    }
    if !src.headers.is_empty() {
        a.extend(["-headers".into(), src.headers.clone().into()]);
    }
    a.extend([
        "-ss".into(),
        format!("{start:.3}").into(),
        "-t".into(),
        format!("{len:.3}").into(),
        "-i".into(),
        src.url.clone().into(),
    ]);
    a
}

fn unique_path(base: &Path, suffix: &str) -> PathBuf {
    let name = base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = base.parent().unwrap_or(Path::new("."));
    let mut p = dir.join(format!("{name}{suffix}.mp4"));
    let mut n = 1;
    while p.exists() {
        p = dir.join(format!("{name}{suffix} ({n}).mp4"));
        n += 1;
    }
    p
}

fn list_line(p: &Path) -> String {
    format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''"))
}

fn total_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Run the whole chunked download. `Ok(path)` on success. `Err("Paused.")` and
/// `Err("Cancelled.")` mean the user stopped it; any other error means the
/// caller should fall back to the standard download.
pub fn run<H: Host>(
    app: &H,
    req: &ClipRequest,
    ytdlp: &Path,
    ffmpeg: &Path,
    template: &Path,
    crop: Option<&str>,
) -> Result<String, String> {
    let id = req.id.as_str();
    let dir = work_dir(id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    set_resumable(app, id, true);
    emit_full(app, id, "reading", None, "Finding the video…", None, true);
    let plan = resolve(ytdlp, req, template)?;
    if is_stopped(app, id) {
        return finish_stop(app, id, &dir);
    }

    // ffmpeg reads through local proxies that fetch the data in parallel pieces.
    // (The proxies stop when this function returns.)
    let fetchers = (req.parts as usize * 2).clamp(4, 12);
    let video_proxy = proxy::Proxy::start(&plan.video.url, &plan.video.pairs, fetchers)?;
    let audio_proxy = proxy::Proxy::start(&plan.audio.url, &plan.audio.pairs, 4)?;
    let local = |p: &proxy::Proxy| Source {
        url: p.url(),
        user_agent: String::new(),
        headers: String::new(),
        pairs: Vec::new(),
    };
    let (video_src, audio_src) = (Arc::new(local(&video_proxy)), Arc::new(local(&audio_proxy)));

    // Same chunks as before if we are resuming; otherwise start fresh.
    let chunks = plan_chunks(req.start, req.end, req.parts as usize);
    let signature = format!("{:.3} {:.3} {} {:?}", req.start, req.end, chunks.len(), crop);
    let sig_file = dir.join("plan.txt");
    if std::fs::read_to_string(&sig_file).ok().as_deref() != Some(signature.as_str()) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        std::fs::write(&sig_file, &signature).map_err(|e| e.to_string())?;
    }

    let n = chunks.len();
    let clip_len = req.end - req.start;
    let chunk_path = |i: usize| dir.join(format!("c{i:04}.mp4"));
    let audio_path = dir.join("audio.m4a");

    // Slot n is the audio. Finished pieces already count as done.
    let mut done = vec![0.0; n + 1];
    let mut queue: VecDeque<usize> = VecDeque::new();
    if audio_path.exists() {
        done[n] = clip_len;
    } else {
        queue.push_back(n);
    }
    for (i, (_, len)) in chunks.iter().enumerate() {
        if chunk_path(i).exists() {
            done[i] = *len;
        } else {
            queue.push_back(i);
        }
    }
    let shared = Arc::new(Shared {
        done: Mutex::new(done),
        queue: Mutex::new(queue),
        error: Mutex::new(None),
    });

    let workers = (req.parts as usize).clamp(1, 8);
    let mut handles = Vec::new();
    let base_path = plan.base.clone();
    let chunks = Arc::new(chunks);
    let crop_owned = crop.map(String::from);
    for _ in 0..workers {
        let (app, shared, chunks) = (app.clone(), shared.clone(), chunks.clone());
        let (video_src, audio_src) = (video_src.clone(), audio_src.clone());
        let (ffmpeg, dir, id, crop) = (ffmpeg.to_path_buf(), dir.clone(), id.to_string(), crop_owned.clone());
        handles.push(thread::spawn(move || loop {
            if is_stopped(&app, &id) || shared.error.lock().unwrap().is_some() {
                return;
            }
            let Some(i) = shared.queue.lock().unwrap().pop_front() else { return };
            let result = if i == chunks.len() {
                // The audio, in one continuous piece (always AAC so the MP4 plays everywhere).
                let part = dir.join("audio.part.m4a");
                let mut args = input_args(&audio_src, chunks[0].0, chunks.iter().map(|c| c.1).sum());
                args.extend(["-vn", "-c:a", "aac", "-b:a", "160k"].map(OsString::from));
                args.push(part.clone().into());
                run_ffmpeg(&app, &id, &ffmpeg, &args, &shared, i).and_then(|_| std::fs::rename(&part, dir.join("audio.m4a")).map_err(|e| e.to_string()))
            } else {
                let (s, len) = chunks[i];
                let part = dir.join(format!("c{i:04}.part.mp4"));
                let mut args = input_args(&video_src, s, len);
                args.extend(["-an".into()]);
                if let Some(f) = &crop {
                    args.extend(["-vf".into(), f.clone().into()]);
                }
                args.extend(
                    ["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p", "-f", "mp4"].map(OsString::from),
                );
                args.push(part.clone().into());
                run_ffmpeg(&app, &id, &ffmpeg, &args, &shared, i).and_then(|_| std::fs::rename(&part, dir.join(format!("c{i:04}.mp4"))).map_err(|e| e.to_string()))
            };
            match result {
                Ok(()) => {
                    // Count the whole piece as done (the last ffmpeg report can be a hair short).
                    let full = if i == chunks.len() { chunks.iter().map(|c| c.1).sum() } else { chunks[i].1 };
                    shared.done.lock().unwrap()[i] = full;
                }
                Err(e) => {
                    if !is_stopped(&app, &id) {
                        let mut slot = shared.error.lock().unwrap();
                        slot.get_or_insert(e);
                        drop(slot);
                        kill_children(&app, &id);
                    }
                    return;
                }
            }
        }));
    }

    // Report progress until the workers are done.
    let weights = |done: &[f64]| -> f64 {
        let video: f64 = done[..n].iter().sum::<f64>() / clip_len;
        let audio = (done[n] / clip_len).min(1.0);
        ((video * (1.0 - AUDIO_WEIGHT) + audio * AUDIO_WEIGHT) * 98.0).clamp(0.0, 98.0)
    };
    while handles.iter().any(|h| !h.is_finished()) {
        thread::sleep(Duration::from_millis(500));
        let pct = weights(&shared.done.lock().unwrap());
        let finished = shared.done.lock().unwrap()[..n].iter().zip(chunks.iter()).filter(|(d, c)| **d >= c.1 - 0.01).count();
        emit_full(
            app,
            id,
            "downloading",
            Some(pct),
            format!("Part {} of {} done", finished, n),
            Some(total_bytes(&dir)),
            true,
        );
    }
    for h in handles {
        let _ = h.join();
    }

    if is_stopped(app, id) {
        return finish_stop(app, id, &dir);
    }
    if let Some(e) = shared.error.lock().unwrap().take() {
        // Keep nothing half-baked; the standard path will start clean.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }

    // Join the chunks and add the audio.
    emit_full(app, id, "finishing", Some(99.0), "Joining the parts…", Some(total_bytes(&dir)), true);
    let mut list = String::new();
    for i in 0..n {
        list.push_str(&list_line(&chunk_path(i)));
    }
    let list_file = dir.join("video.txt");
    std::fs::write(&list_file, list).map_err(|e| e.to_string())?;
    let suffix = req.crop.as_ref().map(|c| format!(" {}x{}", c.aspect_w, c.aspect_h)).unwrap_or_default();
    let final_path = unique_path(&base_path, &suffix);
    let tmp = dir.join("final.part.mp4");
    let out = command(ffmpeg)
        .args(["-y", "-nostdin", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_file)
        .arg("-i")
        .arg(&audio_path)
        .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-movflags", "+faststart", "-f", "mp4"])
        .arg(&tmp)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(error_from(&String::from_utf8_lossy(&out.stderr)));
    }
    // Move (or copy across drives) into place.
    if std::fs::rename(&tmp, &final_path).is_err() {
        std::fs::copy(&tmp, &final_path).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(final_path.to_string_lossy().into_owned())
}

/// The user paused or cancelled: keep the parts for a pause, delete them for a cancel.
fn finish_stop<H: Host>(app: &H, id: &str, dir: &Path) -> Result<String, String> {
    if is_cancelled(app, id) {
        let _ = std::fs::remove_dir_all(dir);
        Err("Cancelled.".into())
    } else {
        Err("Paused.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_range_exactly() {
        let c = plan_chunks(100.0, 1445.0, 4);
        let total: f64 = c.iter().map(|x| x.1).sum();
        assert!((total - 1345.0).abs() < 1e-6);
        assert!((c[0].0 - 100.0).abs() < 1e-6);
        for w in c.windows(2) {
            assert!((w[0].0 + w[0].1 - w[1].0).abs() < 1e-6, "chunks must touch");
        }
        let last = c.last().unwrap();
        assert!((last.0 + last.1 - 1445.0).abs() < 1e-6);
    }

    #[test]
    fn chunk_length_stays_in_limits() {
        for (len, parts) in [(95.0, 4usize), (400.0, 4), (1345.0, 4), (7200.0, 8), (600.0, 1)] {
            let c = plan_chunks(0.0, len, parts);
            for (i, (_, l)) in c.iter().enumerate() {
                assert!(*l <= MAX_CHUNK + 1.0, "{len}/{parts}: chunk {i} is {l}");
                if c.len() > 1 {
                    assert!(*l >= MIN_CHUNK / 2.0 - 1.0, "{len}/{parts}: chunk {i} is {l}");
                }
            }
        }
    }

    #[test]
    fn list_lines_escape_quotes() {
        assert_eq!(list_line(Path::new("/tmp/a'b.mp4")), "file '/tmp/a'\\''b.mp4'\n");
    }

    /// Real download: parts, pause after one finishes, resume, check the result.
    /// Run with: cargo test real_download -- --ignored --nocapture
    #[test]
    #[ignore = "needs network, yt-dlp and ffmpeg"]
    fn real_download_in_parts_with_pause_and_resume() {
        let handle = crate::testing::TestHost::default();
        let out = std::env::temp_dir().join("trimly-e2e-out");
        std::fs::create_dir_all(&out).unwrap();
        let ytdlp = find_bin("yt-dlp").expect("yt-dlp");
        let ffmpeg = find_bin("ffmpeg").expect("ffmpeg");
        let template = out.join("%(title).40B [%(id)s] e2e.%(ext)s");
        let req = ClipRequest {
            id: "e2e-test".into(),
            url: "https://www.youtube.com/watch?v=x1ptQDaujyE".into(),
            start: 600.0,
            end: 780.0,
            out_dir: out.to_string_lossy().into(),
            kind: "video".into(),
            max_height: Some(360),
            is_live: false,
            audio_format: None,
            exact: true,
            cookies_browser: None,
            template: None,
            crop: None,
            parts: 4,
        };
        let run = |h: &crate::testing::TestHost| {
            h.state().jobs.lock().unwrap().entry(req.id.clone()).or_default();
            let r = run(h, &req, &ytdlp, &ffmpeg, &template, None);
            h.state().jobs.lock().unwrap().remove(&req.id);
            r
        };

        // First run: pause as soon as one part is finished.
        let h2 = handle.clone();
        let watcher = thread::spawn(move || {
            let dir = work_dir("e2e-test");
            let t = Instant::now();
            loop {
                let done = std::fs::read_dir(&dir)
                    .map(|rd| rd.flatten().filter(|e| {
                        let n = e.file_name().to_string_lossy().into_owned();
                        n.starts_with('c') && n.ends_with(".mp4") && !n.contains(".part")
                    }).count())
                    .unwrap_or(0);
                if done >= 1 || t.elapsed() > Duration::from_secs(180) {
                    return (pause_job(&h2, "e2e-test"), done);
                }
                thread::sleep(Duration::from_millis(200));
            }
        });
        let first = run(&handle);
        let (paused, done_at_pause) = watcher.join().unwrap();
        println!("paused={paused}, finished parts at pause={done_at_pause}, first={first:?}");
        assert!(paused, "pause_clip should accept a clip downloading in parts");
        assert_eq!(first, Err("Paused.".to_string()));
        assert!(work_dir("e2e-test").exists(), "paused parts must be kept");

        // Second run: resumes and finishes.
        let t = Instant::now();
        let path = run(&handle).expect("resume should finish");
        println!("resumed and finished in {:?}: {path}", t.elapsed());
        assert!(!work_dir("e2e-test").exists(), "work folder is cleaned up");
        let dur = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", &path])
            .output()
            .unwrap();
        let secs: f64 = String::from_utf8_lossy(&dur.stdout).trim().parse().unwrap();
        println!("final duration {secs}s");
        let _ = std::fs::remove_file(&path);
        assert!((secs - 180.0).abs() < 2.0, "expected ~180s, got {secs}");
    }
}
