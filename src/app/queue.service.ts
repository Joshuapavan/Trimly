import { Injectable, computed, inject, signal } from '@angular/core';
import { SettingsService } from './settings.service';
import { ClipRequest, Progress, BackendService } from './backend.service';

export type JobStatus = 'waiting' | 'active' | 'pausing' | 'paused' | 'cancelling' | 'done' | 'failed';

/** What the user chose on the clip screen; the queue adds the id. */
export type JobSpec = Omit<ClipRequest, 'id'>;

export interface Job {
  id: string;
  spec: JobSpec;
  title: string;
  thumbnail: string | null;
  status: JobStatus;
  percent: number;
  detail: string;
  /** What it is doing: starting, reading, preparing, downloading, finishing or cropping. */
  stage: string;
  bytes: number | null;
  /** True once the backend says this clip can be paused. */
  resumable: boolean;
  /** Timestamps (ms) for the elapsed time and for noticing a long quiet spell. */
  startedAt: number | null;
  changedAt: number | null;
  file: string | null;
  error: string | null;
}

let counter = 0;
const newId = () => `job-${Date.now().toString(36)}-${++counter}`;

/**
 * Runs clips with a limit on how many go at once. Jobs live in memory for
 * this session; finished clips are also kept in the Recent clips list.
 */
@Injectable({ providedIn: 'root' })
export class QueueService {
  private readonly backend = inject(BackendService);
  private readonly store = inject(SettingsService);

  readonly jobs = signal<Job[]>([]);
  /** When true, waiting jobs stay put (running ones finish). */
  readonly held = signal(false);

  readonly activeCount = computed(() => this.jobs().filter((j) => j.status === 'active').length);
  /** Clips using a download slot: running, or still shutting down after Cancel. */
  readonly busyCount = computed(() => this.jobs().filter((j) => j.status === 'active' || j.status === 'pausing' || j.status === 'cancelling').length);
  readonly pausedCount = computed(() => this.jobs().filter((j) => j.status === 'paused').length);
  readonly waitingCount = computed(() => this.jobs().filter((j) => j.status === 'waiting').length);
  readonly doneCount = computed(() => this.jobs().filter((j) => j.status === 'done').length);
  readonly failedCount = computed(() => this.jobs().filter((j) => j.status === 'failed').length);
  /** Everything not finished yet, for the badge in the header. */
  readonly openCount = computed(() => this.busyCount() + this.waitingCount() + this.pausedCount());

  private listening: Promise<() => void> | null = null;

  add(spec: JobSpec, title: string, thumbnail: string | null): string {
    this.ensureListening();
    const job: Job = {
      id: newId(),
      spec,
      title,
      thumbnail,
      status: 'waiting',
      percent: 0,
      detail: '',
      stage: 'starting',
      bytes: null,
      resumable: false,
      startedAt: null,
      changedAt: null,
      file: null,
      error: null,
    };
    this.jobs.update((list) => [...list, job]);
    this.pump();
    return job.id;
  }

  get(id: string | null) {
    return id ? (this.jobs().find((j) => j.id === id) ?? null) : null;
  }

  cancel(id: string) {
    const job = this.get(id);
    if (!job) return;
    if (job.status === 'cancelling') return;
    if (job.status === 'paused') {
      // Nothing is running; just throw away the saved parts.
      void this.backend.discard(id);
      this.remove(id);
    } else if (job.status === 'pausing' || job.status === 'active') {
      // Show it straight away; the job disappears once the backend has stopped it.
      this.patch(id, { status: 'cancelling', detail: 'Cancelling…' });
      void this.backend.cancel(id);
    } else {
      this.remove(id);
    }
  }

  /** Pause a clip that is downloading in parts. Finished parts are kept. */
  async pause(id: string) {
    const job = this.get(id);
    if (!job || job.status !== 'active' || !job.resumable) return;
    this.patch(id, { status: 'pausing', detail: 'Pausing…' });
    const ok = await this.backend.pause(id);
    // If it couldn't be paused, it just carries on.
    if (!ok && this.get(id)?.status === 'pausing') this.patch(id, { status: 'active', resumable: false });
  }

  /** Carry on a paused clip; parts that finished aren't downloaded again. */
  resume(id: string) {
    const job = this.get(id);
    if (!job || job.status !== 'paused') return;
    this.patch(id, { status: 'waiting' });
    this.pump();
  }

  retry(id: string) {
    void this.backend.discard(id);
    this.patch(id, { status: 'waiting', percent: 0, detail: '', stage: 'starting', bytes: null, resumable: false, startedAt: null, changedAt: null, error: null, file: null });
    this.pump();
  }

  remove(id: string) {
    this.jobs.update((list) => list.filter((j) => j.id !== id));
  }

  clearFinished() {
    this.jobs.update((list) => list.filter((j) => ['active', 'pausing', 'paused', 'cancelling', 'waiting'].includes(j.status)));
  }

  setHeld(held: boolean) {
    this.held.set(held);
    this.pump();
  }

  /** Start waiting jobs until the concurrency limit is reached. */
  pump() {
    if (this.held()) return;
    const limit = Math.max(1, this.store.settings().concurrent);
    let running = this.busyCount();
    for (const job of this.jobs()) {
      if (running >= limit) break;
      if (job.status !== 'waiting') continue;
      running++;
      void this.run(job);
    }
  }

  private async run(job: Job) {
    const now = Date.now();
    // A resumed clip keeps the progress it had; a fresh one starts from nothing.
    const resumed = job.percent > 0;
    this.patch(job.id, { status: 'active', percent: resumed ? job.percent : 0, detail: 'Starting up…', stage: 'starting', startedAt: now, changedAt: now, error: null, ...(resumed ? {} : { bytes: null }) });
    try {
      const file = await this.backend.clip({ ...job.spec, id: job.id });
      this.patch(job.id, { status: 'done', percent: 100, detail: '', file });
      this.store.addRecent({
        title: job.title,
        file,
        start: job.spec.start,
        end: job.spec.end,
        kind: job.spec.kind,
      });
    } catch (e) {
      const msg = String(e);
      if (msg === 'Cancelled.') this.remove(job.id);
      else if (msg === 'Paused.' && this.get(job.id)?.status === 'cancelling') {
        // Cancelled while it was pausing: don't keep the saved parts.
        void this.backend.discard(job.id);
        this.remove(job.id);
      } else if (msg === 'Paused.') this.patch(job.id, { status: 'paused', detail: '' });
      else this.patch(job.id, { status: 'failed', error: msg, detail: '' });
    }
    this.pump();
  }

  private patch(id: string, p: Partial<Job>) {
    this.jobs.update((list) => list.map((j) => (j.id === id ? { ...j, ...p } : j)));
  }

  private ensureListening() {
    this.listening ??= this.backend.onProgress((p: Progress) => {
      // Late progress after Cancel must not undo the "Cancelling…" state.
      const job = this.get(p.id);
      if (job?.status !== 'active' && job?.status !== 'pausing') return;
      if (job.status === 'pausing') return;
      // No percentage yet (still reading the link) means "unchanged", e.g. after a resume.
      const percent = p.percent === null ? job.percent : Math.round(p.percent);
      const moved = percent !== job.percent || (p.bytes ?? 0) !== (job.bytes ?? 0) || p.stage !== job.stage;
      this.patch(p.id, {
        percent,
        detail: p.detail,
        stage: p.stage,
        bytes: p.bytes,
        resumable: p.resumable,
        changedAt: moved ? Date.now() : job.changedAt,
      });
    });
  }
}
