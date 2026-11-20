import { describe, expect, it } from 'vitest';
import { formatBytes, formatElapsed, statusView } from './status';

const base = { stage: 'reading', percent: 0, bytes: null, startedAt: 0, changedAt: 0 };

describe('statusView', () => {
  it('names the stage while the percentage is unknown', () => {
    expect(statusView({ ...base, stage: 'reading' }, 2000).label).toBe('Reading the link…');
    expect(statusView({ ...base, stage: 'downloading' }, 2000).label).toBe('Connecting to the video server…');
    expect(statusView({ ...base, stage: 'downloading', bytes: 5 * 1024 * 1024 }, 2000).label).toContain('5.0 MB received');
  });

  it('says it is indeterminate until real progress arrives', () => {
    expect(statusView({ ...base, stage: 'downloading', percent: 0 }, 1000).indeterminate).toBe(true);
    expect(statusView({ ...base, stage: 'downloading', percent: 12 }, 1000).indeterminate).toBe(false);
  });

  it('reassures while reading takes a while', () => {
    expect(statusView(base, 5_000).hint).toBeNull();
    expect(statusView(base, 20_000).hint).toContain('nothing is stuck');
  });

  it('explains a long 0% when data is arriving, and warns when it goes quiet', () => {
    const dl = { ...base, stage: 'downloading', bytes: 1024 * 1024 };
    expect(statusView({ ...dl, changedAt: 28_000 }, 30_000).hint).toContain('Data is arriving');
    const quiet = statusView({ ...dl, changedAt: 0 }, 90_000);
    expect(quiet.hint).toContain('No new data');
    expect(quiet.worrying).toBe(true);
  });

  it('shows paused clips as paused, not stuck', () => {
    const v = statusView({ ...base, status: 'paused', stage: 'downloading', percent: 40 }, 999_000);
    expect(v.label).toContain('Paused');
    expect(v.hint).toBeNull();
    expect(v.indeterminate).toBe(false);
  });

  it('formats sizes and elapsed time', () => {
    expect(formatBytes(2048)).toBe('2 KB');
    expect(formatBytes(210 * 1024 * 1024)).toBe('210 MB');
    expect(formatElapsed(83)).toBe('1:23');
  });
});
