/** What a running clip is doing, in plain words, so a quiet 0% never looks like a hang. */

export interface StatusInput {
  /** The clip's state; only paused and pausing change what is shown. */
  status?: string;
  stage: string;
  percent: number;
  bytes: number | null;
  /** When the clip started running (ms), and when progress or data last changed (ms). */
  startedAt: number | null;
  changedAt: number | null;
}

export interface StatusView {
  label: string;
  hint: string | null;
  /** True when we can't say how far along it is yet. */
  indeterminate: boolean;
  elapsed: string;
  /** True once nothing has changed for a long while. */
  worrying: boolean;
}

export function formatBytes(n: number): string {
  if (n < 1024 * 1024) return `${Math.max(1, Math.round(n / 1024))} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(n < 100 * 1024 * 1024 ? 1 : 0)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function formatElapsed(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

export function statusView(j: StatusInput, now: number): StatusView {
  if (j.status === 'paused') {
    return { label: 'Paused. The finished parts are saved. Press Resume to carry on.', hint: null, indeterminate: false, elapsed: '', worrying: false };
  }
  if (j.status === 'pausing') {
    return { label: 'Pausing…', hint: null, indeterminate: true, elapsed: '', worrying: false };
  }
  const elapsedSec = j.startedAt !== null ? (now - j.startedAt) / 1000 : 0;
  const quietSec = j.changedAt !== null ? (now - j.changedAt) / 1000 : elapsedSec;
  const early = j.stage === 'starting' || j.stage === 'reading' || j.stage === 'preparing';
  const hasBytes = (j.bytes ?? 0) > 0;
  const indeterminate = j.stage !== 'finishing' && j.percent <= 1;

  let label: string;
  switch (j.stage) {
    case 'starting':
      label = 'Starting up…';
      break;
    case 'reading':
      label = 'Reading the link…';
      break;
    case 'preparing':
      label = 'Preparing the download…';
      break;
    case 'finishing':
      label = 'Finishing the file…';
      break;
    case 'cropping':
      label = 'Cropping the picture…';
      break;
    default:
      label = j.percent > 1 ? 'Downloading the clip' : hasBytes ? `Downloading… ${formatBytes(j.bytes!)} received` : 'Connecting to the video server…';
  }

  let hint: string | null = null;
  let worrying = false;
  if (early && elapsedSec >= 15) {
    hint = 'Still reading the link. Some sites are slow to answer. This is normal, and nothing is stuck.';
  } else if (j.stage === 'downloading' && j.percent <= 1 && elapsedSec >= 20) {
    if (quietSec >= 60) {
      hint = `No new data for ${Math.floor(quietSec)} seconds. If this keeps up, cancel and try again.`;
      worrying = true;
    } else if (hasBytes) {
      hint = 'Data is arriving. For long videos the server first has to find the start of your clip, so the percentage can stay at 0% for a few minutes.';
    } else {
      hint = 'Waiting for the video server. A clip taken from late in a long video can take a few minutes to start.';
    }
  } else if (j.percent > 1 && j.stage !== 'finishing' && quietSec >= 45) {
    hint = `No new data for ${Math.floor(quietSec)} seconds. If this keeps up, cancel and try again.`;
    worrying = true;
  }

  return { label, hint, indeterminate, elapsed: formatElapsed(elapsedSec), worrying };
}
