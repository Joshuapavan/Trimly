/** Parse "90", "1:30", "01:02:03" or "1:30.5" into seconds. Returns null if invalid. */
export function parseTime(input: string): number | null {
  const s = input.trim();
  if (!/^\d+(\.\d+)?(:\d{1,2}(\.\d+)?){0,2}$/.test(s)) return null;
  const parts = s.split(':').map(Number);
  if (parts.length > 1 && parts.slice(1).some((p) => p >= 60)) return null;
  return parts.reduce((acc, p) => acc * 60 + p, 0);
}

const pad = (n: number) => String(n).padStart(2, '0');

/**
 * Format seconds as mm:ss, or hh:mm:ss when an hour or more (or `withHours`).
 * With `tenths`, a value that isn't a whole second also shows one decimal.
 */
export function formatTime(sec: number, withHours = false, tenths = false): string {
  if (tenths) {
    const v = Math.round(Math.max(0, sec) * 10) / 10;
    const whole = Math.floor(v);
    const tenth = Math.round((v - whole) * 10);
    if (tenth !== 0) return `${formatTime(whole, withHours)}.${tenth}`;
    return formatTime(whole, withHours);
  }
  const t = Math.max(0, Math.round(sec));
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = t % 60;
  return h > 0 || withHours ? `${pad(h)}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
