import { TestBed } from '@angular/core/testing';
import { beforeEach, describe, expect, it } from 'vitest';
import { QueueService, JobSpec } from './queue.service';
import { SettingsService } from './settings.service';

const spec = (over: Partial<JobSpec> = {}): JobSpec => ({
  url: 'https://example.com/v',
  start: 0,
  end: 10,
  outDir: '/tmp',
  kind: 'video',
  maxHeight: 1080,
  isLive: false,
  audioFormat: 'mp3',
  exact: true,
  cookiesBrowser: null,
  template: null,
  crop: null,
  parts: 4,
  ...over,
});

describe('QueueService', () => {
  let queue: QueueService;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    const settings = TestBed.inject(SettingsService);
    settings.save({ ...settings.settings(), concurrent: 2 });
    queue = TestBed.inject(QueueService);
  });

  it('runs at most `concurrent` clips and queues the rest', () => {
    queue.add(spec(), 'one', null);
    queue.add(spec(), 'two', null);
    queue.add(spec(), 'three', null);
    expect(queue.activeCount()).toBe(2);
    expect(queue.waitingCount()).toBe(1);
  });

  it('does not start anything while held, then starts on resume', () => {
    queue.setHeld(true);
    queue.add(spec(), 'one', null);
    expect(queue.activeCount()).toBe(0);
    queue.setHeld(false);
    expect(queue.activeCount()).toBe(1);
  });

  it('removes a waiting clip when cancelled', () => {
    queue.setHeld(true);
    const id = queue.add(spec(), 'one', null);
    queue.cancel(id);
    expect(queue.jobs().length).toBe(0);
  });

  it('cancels a clip that is still reading its link', async () => {
    const id = queue.add(spec(), 'one', null);
    expect(queue.get(id)?.status).toBe('active');
    queue.cancel(id);
    expect(queue.get(id)?.status).toBe('cancelling');
    await new Promise((r) => setTimeout(r, 50));
    expect(queue.get(id)).toBeNull();
    expect(queue.busyCount()).toBe(0);
  });

  it('marks a failing clip as failed and lets it be retried', async () => {
    const id = queue.add(spec({ url: 'https://example.com/fail' }), 'bad', null);
    await new Promise((r) => setTimeout(r, 1000));
    expect(queue.get(id)?.status).toBe('failed');
    expect(queue.get(id)?.error).toContain('Sign in');
    queue.retry(id);
    expect(queue.get(id)?.status).toBe('active');
  });

  it('pauses a long clip, keeps its progress, and resumes it', async () => {
    // A long video clip downloads in parts, so it can be paused.
    const id = queue.add(spec({ start: 0, end: 600, exact: true }), 'long', null);
    await new Promise((r) => setTimeout(r, 3700)); // past "reading the link", a percentage is flowing
    expect(queue.get(id)?.resumable).toBe(true);
    await queue.pause(id);
    await new Promise((r) => setTimeout(r, 50));
    const paused = queue.get(id)!;
    expect(paused.status).toBe('paused');
    expect(queue.busyCount()).toBe(0); // a paused clip frees its slot
    const kept = paused.percent;
    expect(kept).toBeGreaterThan(0);

    queue.resume(id);
    expect(queue.get(id)?.status).toBe('active');
    expect(queue.get(id)!.percent).toBe(kept); // carries on, doesn't restart
    queue.cancel(id);
    await new Promise((r) => setTimeout(r, 50));
    expect(queue.get(id)).toBeNull();
  });

  it('cannot pause a short clip', async () => {
    const id = queue.add(spec({ start: 0, end: 30 }), 'short', null);
    await new Promise((r) => setTimeout(r, 1900));
    expect(queue.get(id)?.resumable).toBe(false);
    await queue.pause(id);
    expect(queue.get(id)?.status).toBe('active');
  });

  it('removes a paused clip when cancelled', async () => {
    const id = queue.add(spec({ start: 0, end: 600 }), 'long', null);
    await new Promise((r) => setTimeout(r, 1900));
    await queue.pause(id);
    await new Promise((r) => setTimeout(r, 50));
    queue.cancel(id);
    expect(queue.get(id)).toBeNull();
  });
});
