import { describe, expect, it } from 'vitest';
import { clamp, formatTime, parseTime } from './time';

describe('time helpers', () => {
  it('parses plain seconds and clock formats', () => {
    expect(parseTime('90')).toBe(90);
    expect(parseTime('1:30')).toBe(90);
    expect(parseTime('01:02:03')).toBe(3723);
    expect(parseTime('1:30.5')).toBe(90.5);
  });

  it('rejects invalid input', () => {
    expect(parseTime('')).toBeNull();
    expect(parseTime('abc')).toBeNull();
    expect(parseTime('1:75')).toBeNull();
    expect(parseTime('-5')).toBeNull();
    expect(parseTime('1:2:3:4')).toBeNull();
  });

  it('formats seconds', () => {
    expect(formatTime(65)).toBe('01:05');
    expect(formatTime(3723)).toBe('01:02:03');
    expect(formatTime(65, true)).toBe('00:01:05');
    expect(formatTime(-3)).toBe('00:00');
  });

  it('shows tenths only when needed', () => {
    expect(formatTime(65.5, false, true)).toBe('01:05.5');
    expect(formatTime(65, false, true)).toBe('01:05');
    expect(formatTime(3723.3, false, true)).toBe('01:02:03.3');
    expect(formatTime(59.96, false, true)).toBe('01:00');
  });

  it('clamps', () => {
    expect(clamp(5, 0, 3)).toBe(3);
    expect(clamp(-1, 0, 3)).toBe(0);
  });
});
