import { ChangeDetectionStrategy, Component, ElementRef, computed, input, model, output, signal, viewChild } from '@angular/core';
import { clamp, formatTime } from './time';

type Handle = 'start' | 'end';
/** The closest two handles may get, in seconds. */
const MIN_GAP = 0.5;
/** The most you can zoom in: this many seconds fill the whole bar. */
const MIN_SPAN = 5;

@Component({
  selector: 'app-timeline',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="wrap">
      <div
        #track
        class="track"
        (pointerdown)="down($event)"
        (pointermove)="move($event)"
        (pointerup)="up($event)"
        (pointercancel)="up($event)"
        (wheel)="wheel($event)"
      >
        <div class="dim" [style.left.%]="0" [style.width.%]="clampPct(startPct())"></div>
        <div class="dim" [style.left.%]="clampPct(endPct())" [style.right.%]="0"></div>
        @if (selWidth() > 0) {
          <div class="sel" [style.left.%]="clampPct(startPct())" [style.width.%]="selWidth()"></div>
        }
        @if (playheadPct(); as ph) { <div class="ph" [style.left.%]="ph.value"></div> }
        @if (startPct() >= 0 && startPct() <= 100) {
          <div
            class="hd"
            role="slider"
            tabindex="0"
            data-h="start"
            aria-label="Clip start"
            [attr.aria-valuemin]="0"
            [attr.aria-valuemax]="end()"
            [attr.aria-valuenow]="start()"
            [attr.aria-valuetext]="fmt(start())"
            [style.left.%]="startPct()"
            (keydown)="key('start', $event)"
          >
            <span class="tag">{{ fmt(start()) }}</span>
          </div>
        }
        @if (endPct() >= 0 && endPct() <= 100) {
          <div
            class="hd"
            role="slider"
            tabindex="0"
            data-h="end"
            aria-label="Clip end"
            [attr.aria-valuemin]="start()"
            [attr.aria-valuemax]="duration()"
            [attr.aria-valuenow]="end()"
            [attr.aria-valuetext]="fmt(end())"
            [style.left.%]="endPct()"
            (keydown)="key('end', $event)"
          >
            <span class="tag">{{ fmt(end()) }}</span>
          </div>
        }
      </div>

      @if (zoom() > 1) {
        <input
          class="pan"
          type="range"
          min="0"
          step="any"
          aria-label="Scroll the timeline"
          [max]="duration() - span()"
          [value]="vs()"
          (input)="viewStart.set(+$any($event.target).value)"
        />
      }

      <div class="bar">
        <span class="t">{{ fmt(vs()) }}</span>
        <div class="zoomctl" role="group" aria-label="Zoom timeline">
          <button type="button" aria-label="Zoom out" [disabled]="zoom() <= 1" (click)="zoomBy(0.5)">−</button>
          <span class="zl" aria-live="polite">×{{ zoomLabel() }}</span>
          <button type="button" aria-label="Zoom in" [disabled]="zoom() >= maxZoom()" (click)="zoomBy(2)">+</button>
          <button type="button" class="txt" (click)="fit()">Fit clip</button>
          <button type="button" class="txt" [disabled]="zoom() === 1" (click)="reset()">Reset</button>
        </div>
        <span class="t">{{ fmt(vs() + span()) }}</span>
      </div>
    </div>
  `,
  styles: `
    .wrap { padding-top: 44px; }
    .track { position: relative; height: 64px; border-radius: 8px; touch-action: none; cursor: pointer;
      background: repeating-linear-gradient(90deg, var(--strip-a) 0 2px, var(--strip-b) 2px 14px); }
    .dim { position: absolute; top: 0; bottom: 0; background: var(--dim); pointer-events: none; }
    .sel { position: absolute; top: -3px; bottom: -3px; border: 3px solid var(--accent); border-radius: 8px;
      background: rgba(194, 65, 12, 0.1); pointer-events: none; box-sizing: border-box; }
    .ph { position: absolute; top: -6px; bottom: -6px; width: 2px; margin-left: -1px; background: var(--ink); pointer-events: none; }
    .ph::before { content: ''; position: absolute; top: 0; left: -4px; border: 5px solid transparent; border-top-color: var(--ink); }
    .hd { position: absolute; top: 50%; width: 14px; height: 38px; margin: -19px 0 0 -7px; border-radius: 4px;
      background: var(--accent); cursor: ew-resize; outline-offset: 3px; }
    .tag { position: absolute; top: -34px; left: 50%; transform: translateX(-50%); background: var(--ink); color: var(--bg);
      border-radius: 6px; padding: 2px 8px; font: 500 12px var(--mono); white-space: nowrap; pointer-events: none; }
    .hd[data-h='start'] .tag { transform: translateX(-85%); }
    .hd[data-h='end'] .tag { transform: translateX(-15%); }
    .pan { width: 100%; margin: 10px 0 0; accent-color: var(--accent); }
    .bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 8px; font-size: 12px; color: var(--muted); }
    .t { font-family: var(--mono); min-width: 70px; }
    .t:last-child { text-align: right; }
    .zoomctl { display: flex; align-items: center; gap: 4px; }
    .zoomctl button { height: 28px; min-width: 28px; padding: 0 8px; border: 1px solid var(--line-strong); border-radius: 6px;
      background: var(--surface); font-weight: 500; }
    .zoomctl button.txt { padding: 0 10px; font-size: 12px; }
    .zoomctl button:disabled { color: var(--muted-2); background: var(--fill); }
    .zl { min-width: 48px; text-align: center; font: 500 12px var(--mono); color: var(--ink); }
  `,
})
export class Timeline {
  readonly duration = input.required<number>();
  readonly start = model.required<number>();
  readonly end = model.required<number>();
  /** Where the preview video is now, drawn as a thin line. */
  readonly playhead = input<number | null>(null);
  /** The time under the pointer or handle, so a preview can show that frame. */
  readonly scrub = output<number>();

  readonly zoom = signal(1);
  readonly viewStart = signal(0);

  private readonly track = viewChild.required<ElementRef<HTMLElement>>('track');
  private active = signal<Handle | null>(null);

  readonly maxZoom = computed(() => Math.max(1, this.duration() / MIN_SPAN));
  /** How many seconds the bar currently shows. */
  readonly span = computed(() => this.duration() / this.zoom());
  /** First second shown. */
  readonly vs = computed(() => clamp(this.viewStart(), 0, Math.max(0, this.duration() - this.span())));

  private readonly pct = (t: number) => ((t - this.vs()) / this.span()) * 100;
  readonly startPct = computed(() => this.pct(this.start()));
  readonly endPct = computed(() => this.pct(this.end()));
  readonly clampPct = (p: number) => clamp(p, 0, 100);
  readonly selWidth = computed(() => {
    if (this.endPct() < 0 || this.startPct() > 100) return 0;
    return this.clampPct(this.endPct()) - this.clampPct(this.startPct());
  });
  readonly playheadPct = computed(() => {
    const t = this.playhead();
    if (t === null) return null;
    const p = this.pct(t);
    return p >= 0 && p <= 100 ? { value: p } : null;
  });
  readonly zoomLabel = computed(() => {
    const z = this.zoom();
    return z >= 10 ? Math.round(z) : Math.round(z * 10) / 10;
  });
  readonly fmt = (s: number) => formatTime(s, this.duration() >= 3600, true);

  // ---- zoom and pan

  zoomBy(factor: number, anchor?: number) {
    // Buttons zoom around the chosen part when it's on screen, so it stays put.
    const mid = (this.start() + this.end()) / 2;
    const midVisible = mid >= this.vs() && mid <= this.vs() + this.span();
    const a = anchor ?? (midVisible ? mid : this.vs() + this.span() / 2);
    const rel = (a - this.vs()) / this.span();
    const z = clamp(this.zoom() * factor, 1, this.maxZoom());
    const span = this.duration() / z;
    this.zoom.set(z);
    // From the buttons, centre on the chosen part whenever it fits, so both handles stay visible.
    const fits = anchor === undefined && this.end() - this.start() <= span * 0.9;
    this.viewStart.set(clamp(fits ? mid - span / 2 : a - rel * span, 0, this.duration() - span));
  }

  /** Zoom so the chosen part fills most of the bar. */
  fit() {
    const len = this.end() - this.start();
    const span = clamp(len * 1.6, MIN_SPAN, this.duration());
    const z = clamp(this.duration() / span, 1, this.maxZoom());
    const w = this.duration() / z;
    this.zoom.set(z);
    this.viewStart.set(clamp(this.start() + len / 2 - w / 2, 0, this.duration() - w));
  }

  reset() {
    this.zoom.set(1);
    this.viewStart.set(0);
  }

  wheel(ev: WheelEvent) {
    if (ev.ctrlKey || ev.metaKey) {
      ev.preventDefault();
      this.zoomBy(ev.deltaY < 0 ? 1.25 : 0.8, this.timeAt(ev));
    } else if (this.zoom() > 1 && (ev.shiftKey || Math.abs(ev.deltaX) > Math.abs(ev.deltaY))) {
      ev.preventDefault();
      const d = (ev.deltaX || ev.deltaY) * (this.span() / 600);
      this.viewStart.set(clamp(this.vs() + d, 0, this.duration() - this.span()));
    }
  }

  // ---- handles

  down(ev: PointerEvent) {
    const t = this.timeAt(ev);
    const grabbed = (ev.target as HTMLElement).closest<HTMLElement>('[data-h]')?.dataset['h'] as Handle | undefined;
    const which: Handle = grabbed ?? (Math.abs(t - this.start()) <= Math.abs(t - this.end()) ? 'start' : 'end');
    this.active.set(which);
    this.track().nativeElement.setPointerCapture(ev.pointerId);
    this.set(which, t);
  }

  move(ev: PointerEvent) {
    const which = this.active();
    if (which) this.set(which, this.timeAt(ev));
    else this.scrub.emit(this.timeAt(ev)); // hovering: look at the frame under the pointer
  }

  up(ev: PointerEvent) {
    this.active.set(null);
    this.track().nativeElement.releasePointerCapture?.(ev.pointerId);
  }

  key(which: Handle, ev: KeyboardEvent) {
    const step = ev.shiftKey ? 10 : ev.altKey ? 0.1 : 1;
    const cur = which === 'start' ? this.start() : this.end();
    let next: number | null = null;
    if (ev.key === 'ArrowLeft' || ev.key === 'ArrowDown') next = cur - step;
    else if (ev.key === 'ArrowRight' || ev.key === 'ArrowUp') next = cur + step;
    else if (ev.key === 'Home') next = 0;
    else if (ev.key === 'End') next = this.duration();
    if (next === null) return;
    ev.preventDefault();
    this.set(which, next);
  }

  private timeAt(ev: { clientX: number }): number {
    const r = this.track().nativeElement.getBoundingClientRect();
    return this.vs() + clamp((ev.clientX - r.left) / r.width, 0, 1) * this.span();
  }

  private set(which: Handle, raw: number) {
    // Zoomed in far enough, land on tenths of a second; otherwise whole seconds.
    const precision = this.span() <= 120 ? 0.1 : 1;
    const t = Number((Math.round(raw / precision) * precision).toFixed(1));
    if (which === 'start') this.start.set(clamp(t, 0, this.end() - MIN_GAP));
    else this.end.set(clamp(t, this.start() + MIN_GAP, this.duration()));
    this.scrub.emit(which === 'start' ? this.start() : this.end());
  }
}
