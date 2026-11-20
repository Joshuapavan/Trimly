import { Injectable, signal } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';

declare global {
  interface Window {
    /** Address of the Trimly server when the web app is hosted apart from it (set in config.js). */
    TRIMLY_API?: string;
  }
}

/** Where the work happens: the desktop app's engine, a Trimly server, or a pretend one for demos. */
export type Mode = 'checking' | 'native' | 'web' | 'demo';

interface JobView {
  status: 'running' | 'paused' | 'done' | 'failed' | 'cancelled';
  stage: string;
  percent: number | null;
  detail: string;
  bytes: number | null;
  resumable: boolean;
  fileName: string | null;
  error: string | null;
}

export interface ToolStatus {
  ytdlp: string | null;
  ffmpeg: string | null;
}

export interface VideoInfo {
  title: string;
  width: number | null;
  height: number | null;
  duration: number | null;
  thumbnail: string | null;
  uploader: string | null;
  isLive: boolean;
}

export interface CropSpec {
  aspectW: number;
  aspectH: number;
  /** 0 = left edge, 1 = right edge of the free space. */
  x: number;
  /** 0 = top edge, 1 = bottom edge of the free space. */
  y: number;
}

export interface ClipRequest {
  id: string;
  url: string;
  start: number;
  end: number;
  outDir: string;
  kind: 'video' | 'audio';
  maxHeight: number | null;
  isLive: boolean;
  audioFormat: string;
  exact: boolean;
  cookiesBrowser: string | null;
  template: string | null;
  crop: CropSpec | null;
  /** How many parts download at once; 1 turns it off. */
  parts: number;
}

export interface Progress {
  id: string;
  stage: string;
  percent: number | null;
  detail: string;
  bytes: number | null;
  /** True when this clip can be paused and resumed. */
  resumable: boolean;
}

/**
 * Thin wrapper over the Tauri backend. When the page is opened in a plain
 * browser (`ng serve`), a small simulator stands in so the UI can be
 * developed and demoed without the desktop shell.
 */
@Injectable({ providedIn: 'root' })
export class BackendService {
  readonly native = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  readonly mode = signal<Mode>(this.native ? 'native' : 'checking');
  /** True when the server wants an access key and we don't have a working one. */
  readonly locked = signal(false);

  private key = this.readKey();
  /** Our clip ids -> the server's ids. */
  private serverIds = new Map<string, string>();
  private fileNames = new Map<string, string>();
  private cancelledEarly = new Set<string>();
  private listeners = new Set<(p: Progress) => void>();
  private mockJobs = new Map<string, { stop: () => void; reject: (r: string) => void; resumable: boolean }>();
  /** Progress kept across pauses, like the saved parts on disk. */
  private mockSaved = new Map<string, number>();

  get isWeb() {
    return this.mode() === 'web';
  }

  /** Work out which backend to use. Call once at startup. */
  async init(): Promise<void> {
    if (this.native) return;
    try {
      const h = await this.http<{ ok: boolean; authRequired: boolean }>('/health', undefined, true);
      if (h?.ok) {
        this.mode.set('web');
        this.locked.set(h.authRequired && !this.key);
        return;
      }
    } catch {
      /* no server here */
    }
    this.mode.set('demo');
  }

  /** Try an access key; returns true if the server accepts it. */
  async unlock(key: string): Promise<boolean> {
    this.key = key.trim();
    try {
      await this.http<ToolStatus>('/tools');
      this.saveKey();
      this.locked.set(false);
      return true;
    } catch {
      this.locked.set(true);
      return false;
    }
  }

  private readKey() {
    try {
      return localStorage.getItem('trimly.key') ?? '';
    } catch {
      return '';
    }
  }

  private saveKey() {
    try {
      localStorage.setItem('trimly.key', this.key);
    } catch {
      /* ignore */
    }
  }

  private url(path: string) {
    return (window.TRIMLY_API ?? '').replace(/\/$/, '') + '/api' + path;
  }

  /** Talk to the server. Errors are thrown as plain messages, like the desktop app's. */
  private async http<T>(path: string, body?: unknown, noAuth = false): Promise<T> {
    const headers: Record<string, string> = {};
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    if (this.key && !noAuth) headers['Authorization'] = 'Bearer ' + this.key;
    const res = await fetch(this.url(path), {
      method: body !== undefined || path.endsWith('/pause') || path.endsWith('/resume') || path.endsWith('/cancel') ? 'POST' : 'GET',
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401) {
      this.locked.set(true);
      throw 'A valid access key is needed.';
    }
    const data = await res.json().catch(() => null);
    if (!res.ok) throw (data && data.error) || res.statusText || 'The server had a problem.';
    return data as T;
  }

  /** The address to download a finished clip from. */
  private fileUrl(serverId: string) {
    return this.url(`/clips/${serverId}/file`) + (this.key ? `?key=${encodeURIComponent(this.key)}` : '');
  }

  /** A readable name for a finished clip's file or address. */
  displayName(path: string): string {
    return this.fileNames.get(path) ?? decodeURIComponent(path.split(/[\\/]/).pop()?.split('?')[0] ?? path);
  }

  async checkTools(): Promise<ToolStatus> {
    if (this.isWeb) return this.http<ToolStatus>('/tools');
    if (!this.native) {
      const missing = location.search.includes('missing');
      return { ytdlp: missing ? null : '2026.11.04', ffmpeg: 'ffmpeg version 7.1' };
    }
    return invoke<ToolStatus>('check_tools');
  }

  async setToolPaths(ytdlp: string, ffmpeg: string): Promise<void> {
    if (this.native) await invoke('set_tool_paths', { ytdlp: ytdlp || null, ffmpeg: ffmpeg || null });
  }

  async fetchInfo(url: string): Promise<VideoInfo> {
    if (this.isWeb) return this.http<VideoInfo>('/info', { url });
    if (!this.native) {
      await new Promise((r) => setTimeout(r, 700));
      const live = url.includes('live');
      return {
        title: live ? 'Sample live stream title' : 'Sample video title',
        width: 1920,
        height: 1080,
        duration: live ? null : 3492,
        thumbnail: null,
        uploader: 'Sample channel',
        isLive: live,
      };
    }
    return invoke<VideoInfo>('fetch_info', { url });
  }

  /** Address of a small version of the video, used to pull preview frames. Null if none. */
  async previewUrl(url: string, cookiesBrowser: string | null): Promise<string | null> {
    if (this.isWeb) {
      try {
        return (await this.http<{ previewId: string | null }>('/preview', { url })).previewId;
      } catch {
        return null;
      }
    }
    if (!this.native) return 'mock://video';
    try {
      return await invoke<string | null>('preview_url', { url, cookiesBrowser });
    } catch {
      return null;
    }
  }

  /** The picture at `time` seconds as an image address, or null if it couldn't be read. */
  async previewFrame(source: string, time: number): Promise<string | null> {
    if (this.isWeb) {
      try {
        return (await this.http<{ image: string }>(`/preview/${encodeURIComponent(source)}/frame?t=${time}`)).image;
      } catch {
        return null;
      }
    }
    if (!this.native) {
      await new Promise((r) => setTimeout(r, 400));
      const label = `${Math.floor(time / 60)}:${String(Math.floor(time % 60)).padStart(2, '0')}`;
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360"><rect width="640" height="360" fill="#2b2a27"/><text x="320" y="190" font-family="monospace" font-size="48" fill="#f1efe9" text-anchor="middle">${label}</text></svg>`;
      return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
    }
    try {
      return await invoke<string>('preview_frame', { source, time });
    } catch {
      return null;
    }
  }

  async clip(req: ClipRequest): Promise<string> {
    if (this.isWeb) return this.webClip(req);
    if (!this.native) return this.mockClip(req);
    return invoke<string>('clip', { req });
  }

  async cancel(id: string): Promise<void> {
    if (this.isWeb) {
      const sid = this.serverIds.get(id);
      if (sid) await this.http(`/clips/${sid}/cancel`).catch(() => undefined);
      else this.cancelledEarly.add(id); // not created yet: stop it as soon as it is
      return;
    }
    if (this.native) {
      await invoke('cancel_clip', { id });
    } else {
      const job = this.mockJobs.get(id);
      this.mockSaved.delete(id);
      if (job) {
        job.stop();
        this.mockJobs.delete(id);
        job.reject('Cancelled.');
      }
    }
  }

  /** Pause a clip that is downloading in parts. False if it can't be paused. */
  async pause(id: string): Promise<boolean> {
    if (this.isWeb) {
      const sid = this.serverIds.get(id);
      if (!sid) return false;
      try {
        return (await this.http<{ ok: boolean }>(`/clips/${sid}/pause`)).ok;
      } catch {
        return false;
      }
    }
    if (this.native) return invoke<boolean>('pause_clip', { id });
    const job = this.mockJobs.get(id);
    if (!job?.resumable) return false;
    job.stop();
    this.mockJobs.delete(id);
    job.reject('Paused.');
    return true;
  }

  /** Throw away the saved parts of a paused clip. */
  async discard(id: string): Promise<void> {
    if (this.isWeb) {
      const sid = this.serverIds.get(id);
      this.serverIds.delete(id);
      if (sid) await this.http(`/clips/${sid}/cancel`).catch(() => undefined);
      return;
    }
    if (this.native) await invoke('discard_clip', { id });
    else this.mockSaved.delete(id);
  }

  async onProgress(cb: (p: Progress) => void): Promise<() => void> {
    if (!this.native) {
      this.listeners.add(cb);
      return () => this.listeners.delete(cb);
    }
    return listen<Progress>('clip-progress', (e) => cb(e.payload));
  }

  async defaultDir(): Promise<string> {
    if (this.isWeb) return '';
    if (!this.native) return '~/Downloads';
    return (await invoke<string | null>('default_dir')) ?? '';
  }

  async chooseFolder(): Promise<string | null> {
    if (this.isWeb) return null;
    if (!this.native) return null;
    const picked = await open({ directory: true, multiple: false });
    return typeof picked === 'string' ? picked : null;
  }

  async reveal(path: string): Promise<void> {
    if (this.isWeb) {
      // On the web there is no folder to open: this saves the file.
      const a = document.createElement('a');
      a.href = path;
      a.download = this.displayName(path);
      a.rel = 'noopener';
      document.body.appendChild(a);
      a.click();
      a.remove();
      return;
    }
    if (this.native) await invoke('reveal', { path });
  }

  /** Start (or resume) a clip on the server, then follow it until it stops. */
  private async webClip(req: ClipRequest): Promise<string> {
    let sid = this.serverIds.get(req.id);
    let resumed = false;
    if (sid) {
      const r = await this.http<{ ok: boolean }>(`/clips/${sid}/resume`).catch(() => ({ ok: false }));
      resumed = r.ok;
    }
    if (!resumed) {
      const { id } = await this.http<{ id: string }>('/clips', {
        url: req.url,
        start: req.start,
        end: req.end,
        kind: req.kind,
        maxHeight: req.maxHeight,
        isLive: req.isLive,
        audioFormat: req.audioFormat,
        exact: req.exact,
        crop: req.crop,
        parts: req.parts,
      });
      sid = id;
      this.serverIds.set(req.id, id);
      if (this.cancelledEarly.delete(req.id)) await this.http(`/clips/${id}/cancel`).catch(() => undefined);
    }

    let failures = 0;
    for (;;) {
      await new Promise((r) => setTimeout(r, 1000));
      let v: JobView;
      try {
        v = await this.http<JobView>(`/clips/${sid}`);
        failures = 0;
      } catch (e) {
        // A hiccup is fine; a long silence isn't.
        if (++failures >= 15) throw 'Lost contact with the server.';
        continue;
      }
      this.listeners.forEach((l) =>
        l({ id: req.id, stage: v.stage, percent: v.percent, detail: v.detail, bytes: v.bytes, resumable: v.resumable }),
      );
      if (v.status === 'done') {
        const url = this.fileUrl(sid!);
        if (v.fileName) this.fileNames.set(url, v.fileName);
        return url;
      }
      if (v.status === 'failed') throw v.error ?? 'The clip failed.';
      if (v.status === 'paused') throw 'Paused.';
      if (v.status === 'cancelled') {
        this.serverIds.delete(req.id);
        throw 'Cancelled.';
      }
    }
  }

  private mockClip(req: ClipRequest): Promise<string> {
    return new Promise((resolve, reject) => {
      if (req.url.includes('fail')) {
        const t = setTimeout(() => reject("ERROR: Sign in to confirm you're not a bot"), 800);
        this.mockJobs.set(req.id, { stop: () => clearTimeout(t), reject, resumable: false });
        return;
      }
      const resumable = req.kind === 'video' && !req.isLive && req.exact && req.parts > 1 && req.end - req.start >= 90;
      let pct = this.mockSaved.get(req.id) ?? 0;
      let timer: ReturnType<typeof setInterval> | null = null;
      const send = (stage: string, percent: number | null, detail: string, bytes: number | null) =>
        this.listeners.forEach((l) => l({ id: req.id, stage, percent, detail, bytes, resumable }));
      // Like the real thing: read the link, connect, receive data with no
      // percentage for a while, then report real progress.
      send('reading', null, 'Reading the link…', null);
      let bytes = 0;
      const reading = setTimeout(() => {
        timer = setInterval(() => {
          if (pct === 0 && bytes < 6 * 1024 * 1024) {
            bytes += 1024 * 1024;
            send('downloading', null, 'Connecting to the video server…', bytes);
            return;
          }
          pct += 6;
          bytes += 2 * 1024 * 1024;
          this.mockSaved.set(req.id, pct);
          send('downloading', Math.min(pct, 99), resumable ? 'Part 2 of 8 done' : '4.2MiB/s  ETA 00:12', bytes);
          if (pct >= 100) {
            clearInterval(timer!);
            this.mockJobs.delete(req.id);
            this.mockSaved.delete(req.id);
            const ext = req.kind === 'audio' ? req.audioFormat : 'mp4';
            const crop = req.crop ? ` ${req.crop.aspectW}x${req.crop.aspectH}` : '';
            resolve(`${req.outDir}/Sample video title [sample] clip${crop}.${ext}`);
          }
        }, 250);
      }, 1500);
      this.mockJobs.set(req.id, {
        stop: () => {
          clearTimeout(reading);
          if (timer) clearInterval(timer);
        },
        reject,
        resumable,
      });
    });
  }
}
