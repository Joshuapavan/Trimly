import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { CropEditor } from './crop-editor';
import { Preview } from './preview';
import { statusView } from './status';
import { QueueService } from './queue.service';
import { ThemeService } from './theme.service';
import { DEFAULT_TEMPLATE, RecentClip, Settings, SettingsService } from './settings.service';
import { CropSpec, BackendService, ToolStatus, VideoInfo } from './backend.service';
import { Timeline } from './timeline';
import { clamp, formatTime, parseTime } from './time';

type Screen = 'start' | 'loaded' | 'busy' | 'done' | 'settings' | 'queue';
type Frame = 'original' | '9:16' | '1:1' | '4:5';
type QueueFilter = 'all' | 'active' | 'done' | 'failed';

const FRAMES: { id: Frame; label: string; hint: string; w: number; h: number }[] = [
  { id: 'original', label: 'Original', hint: 'No crop', w: 0, h: 0 },
  { id: '9:16', label: '9:16 vertical', hint: 'Reels, Shorts, TikTok', w: 9, h: 16 },
  { id: '1:1', label: '1:1 square', hint: 'Feed posts', w: 1, h: 1 },
  { id: '4:5', label: '4:5 portrait', hint: 'Instagram feed', w: 4, h: 5 },
];

const QUALITIES: { label: string; value: number | null }[] = [
  { label: 'Best available', value: null },
  { label: '2160p', value: 2160 },
  { label: '1440p', value: 1440 },
  { label: '1080p', value: 1080 },
  { label: '720p', value: 720 },
  { label: '480p', value: 480 },
  { label: '360p', value: 360 },
];

@Component({
  selector: 'app-root',
  imports: [FormsModule, Timeline, CropEditor, Preview],
  templateUrl: './app.html',
  styleUrl: './app.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App implements OnInit {
  readonly backend = inject(BackendService);
  readonly store = inject(SettingsService);
  readonly queue = inject(QueueService);
  readonly themeService = inject(ThemeService);

  readonly qualities = QUALITIES;
  readonly defaultTemplate = DEFAULT_TEMPLATE;
  readonly browsers = ['chrome', 'firefox', 'safari', 'edge', 'brave'];
  readonly frames = FRAMES;

  readonly screen = signal<Screen>('start');
  readonly tools = signal<ToolStatus | null>(null);
  readonly toolsChecking = signal(false);

  readonly url = signal('');
  readonly urlError = signal('');
  readonly loading = signal(false);
  readonly info = signal<VideoInfo | null>(null);

  readonly start = signal(0);
  readonly end = signal(60);
  readonly kind = signal<'video' | 'audio'>('video');
  readonly quality = signal<number | null>(1080);
  readonly audioFormat = signal<'mp3' | 'm4a' | 'opus'>('mp3');
  readonly folder = signal('');
  readonly timeError = signal('');

  readonly previewSrc = signal<string | null>(null);
  readonly previewLoading = signal(false);
  readonly previewTime = signal(0);
  private previewToken = 0;

  readonly frame = signal<Frame>('original');
  readonly cropX = signal(0.5);
  readonly cropY = signal(0.5);

  readonly jobId = signal<string | null>(null);
  readonly job = computed(() => this.queue.get(this.jobId()));
  readonly filter = signal<QueueFilter>('all');
  readonly toast = signal('');
  private toastTimer: ReturnType<typeof setTimeout> | null = null;
  readonly resultFile = signal('');
  readonly failure = signal<string | null>(null);

  readonly draft = signal<Settings>(this.store.settings());

  readonly missing = computed(() => {
    const t = this.tools();
    return t ? [!t.ytdlp && 'yt-dlp', !t.ffmpeg && 'ffmpeg'].filter(Boolean) as string[] : [];
  });
  readonly needsSetup = computed(() => this.missing().length > 0);
  readonly length = computed(() => Math.max(0, this.end() - this.start()));
  readonly maxEnd = computed(() => this.info()?.duration ?? null);
  readonly longClock = computed(() => (this.maxEnd() ?? this.end()) >= 3600);
  readonly percent = computed(() => this.job()?.percent ?? 0);
  readonly step = computed(() => {
    const st = this.job()?.stage;
    return st === 'downloading' ? 2 : st === 'finishing' || st === 'cropping' ? 3 : 1;
  });
  /** Ticks once a second so elapsed times and "still working" hints stay current. */
  readonly now = signal(Date.now());
  readonly busyStatus = computed(() => {
    const j = this.job();
    return j ? statusView(j, this.now()) : null;
  });
  statusOf = (j: Parameters<typeof statusView>[0]) => statusView(j, this.now());

  readonly frameDef = computed(() => FRAMES.find((f) => f.id === this.frame()) ?? FRAMES[0]);
  readonly sourceAspect = computed(() => {
    const i = this.info();
    return i?.width && i?.height ? i.width / i.height : 16 / 9;
  });
  readonly cropRatio = computed(() => {
    const f = this.frameDef();
    return f.w ? f.w / f.h : 0;
  });
  /** The crop only applies to video. */
  readonly cropSpec = computed<CropSpec | null>(() => {
    const f = this.frameDef();
    if (!f.w || this.kind() !== 'video') return null;
    return { aspectW: f.w, aspectH: f.h, x: this.cropX(), y: this.cropY() };
  });
  /** Output picture size, when the source size is known. */
  readonly outputSize = computed(() => {
    const i = this.info();
    const f = this.frameDef();
    if (!i?.width || !i?.height || !f.w) return '';
    const even = (n: number) => Math.floor(n / 2) * 2;
    const w = even(Math.min(i.width, (i.height * f.w) / f.h));
    const h = even(Math.min(i.height, (i.width * f.h) / f.w));
    return `${w}×${h}`;
  });

  readonly visibleJobs = computed(() => {
    const f = this.filter();
    return this.queue.jobs().filter((j) =>
      f === 'all' ? true : f === 'active' ? ['active', 'waiting', 'pausing', 'paused', 'cancelling'].includes(j.status) : j.status === f,
    );
  });
  readonly queueSummary = computed(() => {
    const parts = [
      this.queue.activeCount() && `${this.queue.activeCount()} downloading`,
      this.queue.pausedCount() && `${this.queue.pausedCount()} paused`,
      this.queue.waitingCount() && `${this.queue.waitingCount()} waiting`,
    ].filter(Boolean);
    return `${parts.length ? parts.join(' · ') : 'Nothing running'} · runs ${this.store.settings().concurrent} at a time`;
  });

  readonly failureView = computed(() => {
    const raw = this.failure();
    if (!raw) return null;
    const signIn = /sign in|log in|login|cookies|bot|private|members/i.test(raw);
    return {
      raw,
      signIn,
      title: "Couldn't download this clip",
      body: signIn
        ? "This video needs a sign-in, so it can't be saved. Choose a browser to borrow its sign-in in Settings, then try again."
        : 'Something went wrong while saving the clip. Check the link and the times, then try again.',
    };
  });

  constructor() {
    const tick = setInterval(() => this.now.set(Date.now()), 1000);
    inject(DestroyRef).onDestroy(() => clearInterval(tick));

    // The Downloading screen follows its job: finished, failed or cancelled.
    effect(() => {
      if (this.screen() !== 'busy') return;
      const job = this.job();
      if (!job) {
        this.screen.set('loaded');
      } else if (job.status === 'done') {
        this.resultFile.set(job.file ?? '');
        this.screen.set('done');
      } else if (job.status === 'failed') {
        this.failure.set(job.error);
        this.queue.remove(job.id);
        this.screen.set('loaded');
      }
    });
  }

  readonly ready = signal(false);
  readonly isWeb = computed(() => this.backend.mode() === 'web');
  readonly isDemo = computed(() => this.backend.mode() === 'demo');
  readonly keyInput = signal('');
  readonly keyError = signal('');

  async ngOnInit() {
    await this.backend.init();
    if (!this.backend.locked()) await this.begin();
    this.ready.set(true);
  }

  /** Ask the server for the access key it needs. */
  async submitKey() {
    this.keyError.set('');
    if (await this.backend.unlock(this.keyInput())) await this.begin();
    else this.keyError.set("That key didn't work. Check it and try again.");
  }

  private async begin() {
    const s = this.store.settings();
    await this.backend.setToolPaths(s.ytdlpPath, s.ffmpegPath);
    this.quality.set(s.quality);
    this.audioFormat.set(s.audioFormat);
    this.folder.set(s.folder || (await this.backend.defaultDir()));
    await this.recheckTools();
  }

  fmt = (s: number) => formatTime(s, this.longClock(), true);

  async recheckTools() {
    this.toolsChecking.set(true);
    this.tools.set(await this.backend.checkTools());
    this.toolsChecking.set(false);
  }

  async load() {
    const link = this.url().trim();
    this.urlError.set('');
    if (!/^https?:\/\/\S+$/i.test(link)) {
      this.urlError.set('Paste a full link that starts with http:// or https://');
      return;
    }
    this.loading.set(true);
    try {
      const info = await this.backend.fetchInfo(link);
      this.info.set(info);
      this.start.set(0);
      this.end.set(info.duration ? Math.min(info.duration, 60) : 60);
      this.failure.set(null);
      this.timeError.set('');
      this.cropX.set(0.5);
      this.cropY.set(0.5);
      this.previewTime.set(0);
      this.screen.set('loaded');
      this.loadPreview(link, info.isLive);
    } catch (e) {
      this.urlError.set(String(e).replace(/^ERROR:\s*/, ''));
    } finally {
      this.loading.set(false);
    }
  }

  /** Look up a playable stream for the scrub preview, without holding up the page. */
  private loadPreview(link: string, isLive: boolean) {
    const token = ++this.previewToken;
    this.previewSrc.set(null);
    if (isLive) {
      this.previewLoading.set(false);
      return;
    }
    this.previewLoading.set(true);
    void this.backend
      .previewUrl(link, this.store.settings().cookies || null)
      .then((u) => token === this.previewToken && this.previewSrc.set(u))
      .finally(() => token === this.previewToken && this.previewLoading.set(false));
  }

  changeVideo() {
    this.previewToken++;
    this.previewSrc.set(null);
    this.info.set(null);
    this.failure.set(null);
    this.screen.set('start');
  }

  setTime(which: 'start' | 'end', value: string, input: HTMLInputElement) {
    const secs = parseTime(value);
    const max = this.maxEnd();
    const bad = secs === null || (max !== null && secs > max);
    if (bad) {
      this.timeError.set(`Enter a time like 12:45 or 1:02:03${max !== null ? ` up to ${formatTime(max, max >= 3600)}` : ''}.`);
    } else if (which === 'start' && secs >= this.end()) {
      this.timeError.set('Start must be before the end.');
    } else if (which === 'end' && secs <= this.start()) {
      this.timeError.set('End must be after the start.');
    } else {
      this.timeError.set('');
      (which === 'start' ? this.start : this.end).set(secs);
      this.previewTime.set(secs);
    }
    input.value = this.fmt(which === 'start' ? this.start() : this.end());
  }

  async chooseFolder() {
    const picked = await this.backend.chooseFolder();
    if (picked) this.folder.set(picked);
  }

  async chooseDraftFolder() {
    const picked = await this.backend.chooseFolder();
    if (picked) this.patch({ folder: picked });
  }

  private buildSpec() {
    const info = this.info()!;
    const s = this.store.settings();
    return {
      url: this.url().trim(),
      start: this.start(),
      end: this.end(),
      outDir: this.folder(),
      kind: this.kind(),
      maxHeight: this.quality(),
      isLive: info.isLive,
      audioFormat: this.audioFormat(),
      exact: s.exact,
      cookiesBrowser: s.cookies || null,
      template: s.template || null,
      crop: this.cropSpec(),
      parts: s.parts,
    };
  }

  /** Download now: queue the clip and follow it on the Downloading screen. */
  download() {
    const info = this.info();
    if (!info || this.length() <= 0) return;
    this.failure.set(null);
    this.jobId.set(this.queue.add(this.buildSpec(), info.title, info.thumbnail));
    this.screen.set('busy');
  }

  /** Line it up behind other clips and get ready for the next link. */
  addToQueue() {
    const info = this.info();
    if (!info || this.length() <= 0) return;
    this.failure.set(null);
    this.queue.add(this.buildSpec(), info.title, info.thumbnail);
    this.showToast('Added to queue');
    this.another();
  }

  cancel() {
    const id = this.jobId();
    if (id) this.queue.cancel(id);
  }

  pauseOrResume() {
    const id = this.jobId();
    const job = this.job();
    if (!id || !job) return;
    if (job.status === 'paused') this.queue.resume(id);
    else void this.queue.pause(id);
  }

  showToast(msg: string) {
    this.toast.set(msg);
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => this.toast.set(''), 3500);
  }

  goClip() {
    this.screen.set(this.info() ? 'loaded' : 'start');
  }

  setFrame(f: Frame) {
    this.frame.set(f);
    this.cropX.set(0.5);
    this.cropY.set(0.5);
  }

  jobRange(j: { spec: { start: number; end: number } }) {
    const long = j.spec.end >= 3600;
    return `${formatTime(j.spec.start, long)} → ${formatTime(j.spec.end, long)}`;
  }

  jobFormat(j: { spec: { kind: string; audioFormat: string; maxHeight: number | null; crop: CropSpec | null } }) {
    if (j.spec.kind === 'audio') return j.spec.audioFormat.toUpperCase();
    return `MP4 ${j.spec.maxHeight ? j.spec.maxHeight + 'p' : 'best'}`;
  }

  reveal(file: string) {
    void this.backend.reveal(file);
  }

  fileName(path: string) {
    return this.backend.displayName(path);
  }

  another() {
    this.info.set(null);
    this.url.set('');
    this.screen.set('start');
  }

  clipLabel(c: RecentClip) {
    return `${formatTime(c.start, c.end >= 3600)} → ${formatTime(c.end, c.end >= 3600)} · ${c.kind === 'audio' ? 'Audio' : 'Video'}`;
  }

  // ---- settings
  openSettings() {
    this.draft.set({ ...this.store.settings(), folder: this.store.settings().folder || this.folder() });
    this.screen.set('settings');
  }

  patch(p: Partial<Settings>) {
    this.draft.update((d) => ({ ...d, ...p }));
  }

  async saveSettings() {
    const d = this.draft();
    this.store.save(d);
    this.folder.set(d.folder || this.folder());
    this.quality.set(d.quality);
    this.audioFormat.set(d.audioFormat);
    await this.backend.setToolPaths(d.ytdlpPath, d.ffmpegPath);
    await this.recheckTools();
    this.closeSettings();
  }

  closeSettings() {
    this.screen.set(this.info() ? 'loaded' : 'start');
  }

  qualityValue(v: string) {
    return v === 'best' ? null : clamp(Number(v), 144, 4320);
  }
}
