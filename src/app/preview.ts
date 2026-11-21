import { ChangeDetectionStrategy, Component, DestroyRef, effect, inject, input, model, signal } from '@angular/core';
import { BackendService } from './backend.service';
import { formatTime } from './time';

/** Frames are cached by half-second, so moving back and forth is instant. */
const BUCKET = 0.5;
/** Wait for the pointer or handle to settle before fetching a frame. */
const SETTLE_MS = 300;

/**
 * Shows the picture at the moment `time` (the timeline moves it), so you can
 * see what you're cutting. One frame is fetched at a time, only after you stop
 * moving; far into a long video it can take a few seconds.
 */
@Component({
  selector: 'app-preview',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="screen">
      @if (frame() ?? thumbnail(); as img) { <img [src]="img" alt="Video frame at {{ fmt(time()) }}" /> }
      @if (loading()) {
        <div class="over" role="status"><span class="spinner" aria-hidden="true"></span><span>Finding the video…</span></div>
      } @else if (!src()) {
        <div class="over">Preview isn't available for this link.<br />You can still set the times.</div>
      } @else if (!frame() && busy()) {
        <div class="over" role="status"><span class="spinner" aria-hidden="true"></span><span>Loading the first frame…</span></div>
      }
      <div class="stamp">{{ fmt(time()) }}</div>
      @if (busy() && frame()) {
        <div class="busy" role="status"><span class="spinner sm" aria-hidden="true"></span>Loading frame…</div>
      }
    </div>
    @if (missed()) {
      <div class="miss">
        Couldn't load a picture at {{ fmt(time()) }}.
        <button type="button" class="link" (click)="retry()">Try again</button>
        @if (backend.lastPreviewError()) { <div class="why">{{ backend.lastPreviewError() }}</div> }
      </div>
    }
    <div class="ctl">
      <button type="button" class="btn" [disabled]="!src()" (click)="time.set(start())">Show start</button>
      <button type="button" class="btn" [disabled]="!src()" (click)="time.set(end())">Show end</button>
    </div>
  `,
  styles: `
    :host { display: block; }
    .screen { position: relative; width: 100%; aspect-ratio: 16 / 9; border-radius: 10px; overflow: hidden; background: #111;
      display: flex; align-items: center; justify-content: center; }
    img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; background: #111; }
    .over { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px;
      color: #f1efe9; font-size: 13px; background: rgba(17, 17, 17, 0.55); text-align: center; padding: 12px; }
    .stamp { position: absolute; left: 8px; bottom: 8px; background: rgba(17, 17, 17, 0.78); color: #fff; border-radius: 6px;
      padding: 2px 8px; font: 500 12px var(--mono); }
    .busy { position: absolute; right: 8px; bottom: 8px; display: flex; align-items: center; gap: 6px; background: rgba(17, 17, 17, 0.78);
      color: #fff; border-radius: 6px; padding: 2px 8px; font-size: 12px; }
    .miss { margin-top: 8px; font-size: 12px; color: var(--muted); }
    .why { margin-top: 2px; font: 11px var(--mono); color: var(--muted-2); word-break: break-word; }
    .link { border: 0; background: none; padding: 0; text-decoration: underline; color: inherit; font-size: inherit; }
    .ctl { display: flex; gap: 6px; margin-top: 8px; }
    .btn { height: 32px; padding: 0 12px; font-size: 13px; border-radius: 8px; border: 1px solid var(--line-strong); background: var(--surface); font-weight: 500; }
    .btn:disabled { background: var(--fill); color: var(--muted-2); border-color: var(--line-2); }
    .spinner { width: 24px; height: 24px; border-radius: 50%; border: 3px solid rgba(255, 255, 255, 0.3); border-top-color: #fff;
      animation: spin 0.8s linear infinite; }
    .spinner.sm { width: 12px; height: 12px; border-width: 2px; }
    @keyframes spin { to { transform: rotate(360deg); } }
    @media (prefers-reduced-motion: reduce) { .spinner { animation-duration: 3s; } }
  `,
})
export class Preview {
  /** Address of a small version of the video that frames are read from. */
  readonly src = input<string | null>(null);
  readonly thumbnail = input<string | null>(null);
  /** True while that address is still being looked up. */
  readonly loading = input(false);
  readonly start = input.required<number>();
  readonly end = input.required<number>();
  readonly clock = input(false);
  /** The moment shown. The timeline sets it. */
  readonly time = model.required<number>();

  readonly frame = signal<string | null>(null);
  readonly busy = signal(false);
  readonly missed = signal(false);

  readonly backend = inject(BackendService);
  private readonly cache = new Map<number, string>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private token = 0;
  private lastSrc: string | null = null;
  /** The moment being fetched right now, so the same one isn't started twice. */
  private inflightKey: number | null = null;

  readonly fmt = (t: number) => formatTime(t, this.clock(), true);

  constructor() {
    inject(DestroyRef).onDestroy(() => this.timer && clearTimeout(this.timer));

    effect(() => {
      const src = this.src();
      const t = this.time();
      if (src !== this.lastSrc) {
        // A different video: start clean.
        this.lastSrc = src;
        this.cache.clear();
        this.frame.set(null);
        this.missed.set(false);
      }
      if (!src) return;
      this.schedule(src, t);
    });
  }

  retry() {
    const src = this.src();
    if (src) this.fetch(src, this.time());
  }

  private schedule(src: string, t: number) {
    if (this.timer) clearTimeout(this.timer);
    const key = this.bucket(t);
    const hit = this.cache.get(key);
    if (hit) {
      this.token++;
      this.busy.set(false);
      this.missed.set(false);
      this.frame.set(hit);
      return;
    }
    // Already loading this very moment: let that finish.
    if (this.busy() && this.inflightKey === key) return;
    // Keep showing the last frame while the pointer is still moving.
    this.timer = setTimeout(() => this.fetch(src, t), this.frame() ? SETTLE_MS : 0);
  }

  private fetch(src: string, t: number, attempt = 0) {
    const key = this.bucket(t);
    const mine = ++this.token;
    this.inflightKey = key;
    this.busy.set(true);
    this.missed.set(false);
    void this.backend.previewFrame(src, key).then((img) => {
      if (mine !== this.token) return; // a newer request replaced this one
      if (!img && attempt < 1) {
        // One more try before telling the person: a read can fail for passing reasons.
        this.timer = setTimeout(() => {
          if (mine === this.token) this.fetch(src, t, attempt + 1);
        }, 700);
        return;
      }
      this.busy.set(false);
      this.inflightKey = null;
      if (img) {
        this.cache.set(key, img);
        if (this.cache.size > 60) this.cache.delete(this.cache.keys().next().value as number);
        this.frame.set(img);
      } else {
        this.missed.set(true);
      }
    });
  }

  private bucket(t: number) {
    return Math.round(t / BUCKET) * BUCKET;
  }
}
