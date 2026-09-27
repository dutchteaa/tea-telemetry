import { describe, expect, it } from 'vitest';
import { formatLapTime } from './format';

describe('formatLapTime', () => {
  it('formats minutes, seconds and milliseconds', () => {
    expect(formatLapTime(123412)).toBe('2:03.412');
    expect(formatLapTime(59999)).toBe('0:59.999');
    expect(formatLapTime(0)).toBe('0:00.000');
    expect(formatLapTime(600000)).toBe('10:00.000');
  });

  it('rounds fractional milliseconds', () => {
    expect(formatLapTime(100000.6)).toBe('1:40.001');
  });

  it('shows a placeholder for invalid input', () => {
    expect(formatLapTime(-1)).toBe('--:--.---');
    expect(formatLapTime(Number.NaN)).toBe('--:--.---');
  });
});
