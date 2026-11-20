import { ChangeDetectionStrategy, Component, ElementRef, computed, input, model, signal, viewChild } from '@angular/core';
import { clamp } from './time';

/**
 * Shows the video frame with a movable box in the chosen shape. The box is
 * as large as the shape allows, so dragging only slides it along the free
 * side (left/right for vertical crops, up/down for wide ones).
 */
@Component({
  selector: 'app-crop-editor',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div
      #stage
      class="stage"
      [style.aspect-ratio]="sourceAspect()"
      (pointerdown)="down($event)"
      (pointermove)="move($event)"
      (pointerup)="up($event)"
      (pointercancel)="up($event)"
    >
      @if (thumbnail(); as src) { <img [src]="src" alt="" draggable="false" /> }
      <div
        class="box"
        tabindex="0"
        role="group"
        aria-label="Crop area. Use the arrow keys to move it."
        [style.left.%]="left()"
        [style.top.%]="top()"
        [style.width.%]="boxW() * 100"
        [style.height.%]="boxH() * 100"
        (keydown)="key($event)"
      ></div>
    </div>
  `,
  styles: `
    :host { display: block; }
    .stage { position: relative; width: 100%; border-radius: 10px; overflow: hidden; touch-action: none; cursor: grab;
      background: repeating-linear-gradient(135deg, var(--strip-a) 0 14px, var(--strip-b) 14px 28px); user-select: none; }
    .stage:active { cursor: grabbing; }
    img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; pointer-events: none; }
    .box { position: absolute; border: 3px solid #fff; box-shadow: 0 0 0 2px var(--accent), 0 0 0 9999px rgba(27, 27, 26, 0.62);
      box-sizing: border-box; outline: none;
      background:
        linear-gradient(90deg, transparent calc(33.3% - 0.5px), rgba(255,255,255,.4) calc(33.3% - 0.5px) calc(33.3% + 0.5px), transparent calc(33.3% + 0.5px) calc(66.6% - 0.5px), rgba(255,255,255,.4) calc(66.6% - 0.5px) calc(66.6% + 0.5px), transparent calc(66.6% + 0.5px)),
        linear-gradient(0deg, transparent calc(33.3% - 0.5px), rgba(255,255,255,.4) calc(33.3% - 0.5px) calc(33.3% + 0.5px), transparent calc(33.3% + 0.5px) calc(66.6% - 0.5px), rgba(255,255,255,.4) calc(66.6% - 0.5px) calc(66.6% + 0.5px), transparent calc(66.6% + 0.5px)); }
    .box:focus-visible { box-shadow: 0 0 0 2px var(--ink), 0 0 0 9999px rgba(27, 27, 26, 0.62); }
  `,
})
export class CropEditor {
  /** Width divided by height of the video. */
  readonly sourceAspect = input.required<number>();
  /** Width divided by height of the crop shape, e.g. 9/16. */
  readonly ratio = input.required<number>();
  readonly thumbnail = input<string | null>(null);
  readonly px = model.required<number>();
  readonly py = model.required<number>();

  private readonly stage = viewChild.required<ElementRef<HTMLElement>>('stage');
  private drag: { x: number; y: number; px: number; py: number } | null = null;

  /** Box size as a fraction of the stage. */
  readonly boxW = computed(() => Math.min(1, this.ratio() / this.sourceAspect()));
  readonly boxH = computed(() => Math.min(1, this.sourceAspect() / this.ratio()));
  readonly left = computed(() => this.px() * (1 - this.boxW()) * 100);
  readonly top = computed(() => this.py() * (1 - this.boxH()) * 100);

  down(ev: PointerEvent) {
    const r = this.stage().nativeElement.getBoundingClientRect();
    // Clicking outside the box centres it on the pointer.
    const inside = (ev.target as HTMLElement).classList.contains('box');
    if (!inside) {
      const fx = (ev.clientX - r.left) / r.width - this.boxW() / 2;
      const fy = (ev.clientY - r.top) / r.height - this.boxH() / 2;
      this.setFromOffsets(fx, fy);
    }
    this.drag = { x: ev.clientX, y: ev.clientY, px: this.px(), py: this.py() };
    this.stage().nativeElement.setPointerCapture(ev.pointerId);
  }

  move(ev: PointerEvent) {
    if (!this.drag) return;
    const r = this.stage().nativeElement.getBoundingClientRect();
    const freeX = 1 - this.boxW();
    const freeY = 1 - this.boxH();
    if (freeX > 0.001) this.px.set(clamp(this.drag.px + (ev.clientX - this.drag.x) / r.width / freeX, 0, 1));
    if (freeY > 0.001) this.py.set(clamp(this.drag.py + (ev.clientY - this.drag.y) / r.height / freeY, 0, 1));
  }

  up(ev: PointerEvent) {
    this.drag = null;
    this.stage().nativeElement.releasePointerCapture?.(ev.pointerId);
  }

  key(ev: KeyboardEvent) {
    const step = ev.shiftKey ? 0.2 : 0.05;
    const d: Record<string, [number, number]> = {
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
      ArrowUp: [0, -step],
      ArrowDown: [0, step],
    };
    const delta = d[ev.key];
    if (!delta) return;
    ev.preventDefault();
    if (1 - this.boxW() > 0.001) this.px.set(clamp(this.px() + delta[0], 0, 1));
    if (1 - this.boxH() > 0.001) this.py.set(clamp(this.py() + delta[1], 0, 1));
  }

  private setFromOffsets(fx: number, fy: number) {
    const freeX = 1 - this.boxW();
    const freeY = 1 - this.boxH();
    if (freeX > 0.001) this.px.set(clamp(fx / freeX, 0, 1));
    if (freeY > 0.001) this.py.set(clamp(fy / freeY, 0, 1));
  }
}
