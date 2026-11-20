//! A tiny local web server that sits between ffmpeg and a video server.
//!
//! Video servers slow down a single long read (which is what ffmpeg makes),
//! but they answer bounded requests for a few megabytes quickly, and several
//! of those can run at once. This proxy lets ffmpeg keep seeking and reading
//! normally while it fetches the data in parallel pieces behind the scenes
//! and keeps recent pieces in memory.

use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

/// Size of each piece fetched from the server.
const PIECE: u64 = 4 * 1024 * 1024;
/// How much fetched data is kept in memory.
const CACHE_LIMIT: usize = 96 * 1024 * 1024;

type Data = Arc<Vec<u8>>;

struct Pending {
    result: Mutex<Option<Result<Data, String>>>,
    ready: Condvar,
}

enum Slot {
    Ready(Data),
    Pending(Arc<Pending>),
}

#[derive(Default)]
struct Cache {
    map: HashMap<u64, Slot>,
    /// Ready pieces, oldest first, for eviction.
    order: VecDeque<u64>,
    bytes: usize,
}

struct Inner {
    url: String,
    headers: Vec<(String, String)>,
    len: u64,
    agent: ureq::Agent,
    cache: Mutex<Cache>,
    tx: Mutex<mpsc::Sender<u64>>,
    /// How many pieces to line up ahead of the one being read.
    window: u64,
    stop: Arc<AtomicBool>,
}

pub struct Proxy {
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it can notice.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Proxy {
    /// Start serving `url` on a free local port. `fetchers` pieces are
    /// downloaded at the same time.
    pub fn start(url: &str, headers: &[(String, String)], fetchers: usize) -> Result<Proxy, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(40))
            .build();
        let len = probe_length(&agent, url, headers)?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<u64>();
        let fetchers = fetchers.clamp(1, 16);
        let inner = Arc::new(Inner {
            url: url.to_string(),
            headers: headers.to_vec(),
            len,
            agent,
            cache: Mutex::new(Cache::default()),
            tx: Mutex::new(tx),
            window: (fetchers as u64).max(2),
            stop: stop.clone(),
        });

        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..fetchers {
            let (inner, rx) = (inner.clone(), rx.clone());
            thread::spawn(move || worker(inner, rx));
        }
        let accept_inner = inner.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                if accept_inner.stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(stream) = conn {
                    let inner = accept_inner.clone();
                    thread::spawn(move || {
                        let _ = serve(inner, stream);
                    });
                }
            }
        });
        Ok(Proxy { port, stop })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/stream", self.port)
    }
}

fn request(agent: &ureq::Agent, url: &str, headers: &[(String, String)], range: &str) -> Result<ureq::Response, String> {
    let mut req = agent.get(url).set("Range", range);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    req.call().map_err(|e| e.to_string())
}

/// Ask for one byte to learn the total size.
fn probe_length(agent: &ureq::Agent, url: &str, headers: &[(String, String)]) -> Result<u64, String> {
    let resp = request(agent, url, headers, "bytes=0-0")?;
    resp.header("Content-Range")
        .and_then(|v| v.rsplit('/').next())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .ok_or_else(|| "The server doesn't support ranges.".to_string())
}

fn worker(inner: Arc<Inner>, rx: Arc<Mutex<mpsc::Receiver<u64>>>) {
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let idx = {
            let rx = rx.lock().unwrap();
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(i) => i,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        let result = fetch_piece(&inner, idx);
        let pending = {
            let mut cache = inner.cache.lock().unwrap();
            match (&result, cache.map.get(&idx)) {
                (Ok(data), Some(Slot::Pending(p))) => {
                    let p = p.clone();
                    let data = data.clone();
                    cache.bytes += data.len();
                    cache.map.insert(idx, Slot::Ready(data));
                    cache.order.push_back(idx);
                    while cache.bytes > CACHE_LIMIT {
                        let Some(old) = cache.order.pop_front() else { break };
                        if let Some(Slot::Ready(d)) = cache.map.remove(&old) {
                            cache.bytes -= d.len();
                        }
                    }
                    Some(p)
                }
                (Err(_), Some(Slot::Pending(p))) => {
                    let p = p.clone();
                    cache.map.remove(&idx); // let a later request try again
                    Some(p)
                }
                _ => None,
            }
        };
        if let Some(p) = pending {
            *p.result.lock().unwrap() = Some(result);
            p.ready.notify_all();
        }
    }
}

fn fetch_piece(inner: &Inner, idx: u64) -> Result<Data, String> {
    let start = idx * PIECE;
    let end = (start + PIECE).min(inner.len) - 1;
    let want = (end - start + 1) as usize;
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(300 * attempt));
        }
        if inner.stop.load(Ordering::SeqCst) {
            return Err("Stopped.".into());
        }
        match request(&inner.agent, &inner.url, &inner.headers, &format!("bytes={start}-{end}")) {
            Ok(resp) => {
                let mut buf = Vec::with_capacity(want);
                match resp.into_reader().take(want as u64 + 1).read_to_end(&mut buf) {
                    Ok(_) if buf.len() == want => return Ok(Arc::new(buf)),
                    Ok(_) => last = format!("Short read: {} of {want} bytes.", buf.len()),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Make sure a piece is being fetched (or is ready) without waiting for it.
fn schedule(inner: &Inner, idx: u64) {
    if idx * PIECE >= inner.len {
        return;
    }
    let mut cache = inner.cache.lock().unwrap();
    if cache.map.contains_key(&idx) {
        return;
    }
    cache.map.insert(
        idx,
        Slot::Pending(Arc::new(Pending {
            result: Mutex::new(None),
            ready: Condvar::new(),
        })),
    );
    let _ = inner.tx.lock().unwrap().send(idx);
}

/// Get a piece, waiting for it if it's still being fetched.
fn get_piece(inner: &Inner, idx: u64) -> Result<Data, String> {
    for _ in 0..3 {
        schedule(inner, idx);
        let pending = match inner.cache.lock().unwrap().map.get(&idx) {
            Some(Slot::Ready(d)) => return Ok(d.clone()),
            Some(Slot::Pending(p)) => p.clone(),
            None => continue,
        };
        let mut guard = pending.result.lock().unwrap();
        loop {
            if let Some(r) = guard.as_ref() {
                match r {
                    Ok(d) => return Ok(d.clone()),
                    Err(_) => break, // fetch failed: loop and schedule it again
                }
            }
            if inner.stop.load(Ordering::SeqCst) {
                return Err("Stopped.".into());
            }
            guard = pending.ready.wait_timeout(guard, Duration::from_millis(250)).unwrap().0;
        }
    }
    Err("Couldn't fetch that part of the video.".into())
}

fn serve(inner: Arc<Inner>, stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let mut range: Option<(u64, Option<u64>)> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range:") {
            if let Some(spec) = v.trim().strip_prefix("bytes=") {
                let mut it = spec.splitn(2, '-');
                let a = it.next().and_then(|x| x.trim().parse::<u64>().ok());
                let b = it.next().and_then(|x| x.trim().parse::<u64>().ok());
                if let Some(a) = a {
                    range = Some((a, b));
                }
            }
        }
    }

    let len = inner.len;
    let (a, b, status) = match range {
        Some((a, _)) if a >= len => {
            out.write_all(
                format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
            )?;
            return Ok(());
        }
        Some((a, b)) => (a, b.unwrap_or(len - 1).min(len - 1), "206 Partial Content"),
        None => (0, len - 1, "200 OK"),
    };
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n", b - a + 1);
    if status.starts_with("206") {
        head.push_str(&format!("Content-Range: bytes {a}-{b}/{len}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    if method == "HEAD" {
        return Ok(());
    }

    let mut pos = a;
    while pos <= b {
        if inner.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let idx = pos / PIECE;
        // Line up the next few pieces so they download while this one is sent.
        for ahead in 1..=inner.window {
            schedule(&inner, idx + ahead);
        }
        let data = get_piece(&inner, idx).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let off = (pos - idx * PIECE) as usize;
        let upto = ((b + 1).min((idx + 1) * PIECE) - idx * PIECE) as usize;
        out.write_all(&data[off..upto])?;
        pos = idx * PIECE + upto as u64;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in video server that answers range requests from memory.
    fn fake_server(data: Arc<Vec<u8>>) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let data = data.clone();
                thread::spawn(move || {
                    let mut reader = BufReader::new(conn.try_clone().unwrap());
                    let mut out = conn;
                    let mut line = String::new();
                    let mut range = (0u64, data.len() as u64 - 1);
                    reader.read_line(&mut line).unwrap();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                            break;
                        }
                        let l = line.to_ascii_lowercase();
                        if let Some(v) = l.strip_prefix("range: bytes=") {
                            let mut it = v.trim().splitn(2, '-');
                            let a: u64 = it.next().unwrap().parse().unwrap();
                            let b: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(data.len() as u64 - 1);
                            range = (a, b);
                        }
                    }
                    let (a, b) = range;
                    let body = &data[a as usize..=b as usize];
                    let _ = write!(
                        out,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len(),
                        body.len()
                    );
                    let _ = out.write_all(body);
                });
            }
        });
        port
    }

    fn get(port: u16, range: &str) -> (String, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /stream HTTP/1.1\r\nHost: x\r\nRange: {range}\r\n\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        (String::from_utf8_lossy(&all[..split]).into_owned(), all[split..].to_vec())
    }

    #[test]
    fn serves_exact_bytes_across_piece_boundaries() {
        let data: Arc<Vec<u8>> = Arc::new((0..10_000_000u64).map(|i| (i % 251) as u8).collect());
        let up = fake_server(data.clone());
        let proxy = Proxy::start(&format!("http://127.0.0.1:{up}/v"), &[], 4).unwrap();

        // Crosses the first 4 MiB boundary.
        let (head, body) = get(proxy.port, "bytes=4194300-4194310");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(head.contains("Content-Range: bytes 4194300-4194310/10000000"));
        assert_eq!(body, data[4194300..=4194310]);

        // Open-ended, like ffmpeg's reads; spans several pieces to the end.
        let (_, body) = get(proxy.port, "bytes=5000000-");
        assert_eq!(body.len(), 5_000_000);
        assert_eq!(body, data[5_000_000..]);

        // The very start.
        let (_, body) = get(proxy.port, "bytes=0-99");
        assert_eq!(body, data[..100]);
    }
}
