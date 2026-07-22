// Scope timing math, mirroring the driver so the UI can say what a capture will actually
// cover *before* it runs.
//
// The authority is `control::capture::deep_tdiv_for_bit` + `settings::TB_TO_NS` in the
// backend; the ladder and row counts below must stay in step with it. (They describe fixed
// instrument hardware, so they do not drift in practice.)

import type { Depth } from "./api";

/** SEC/DIV ladder in nanoseconds — 2-4-8 per decade, index 0 = 2 ns/div. */
export const TB_TO_NS = [
  2, 4, 8, 20, 40, 80, 200, 400,
  800, 2_000, 4_000, 8_000, 20_000, 40_000,
  80_000, 200_000, 400_000, 800_000, 2_000_000,
  4_000_000, 8_000_000, 20_000_000, 40_000_000,
  80_000_000, 200_000_000, 400_000_000, 800_000_000,
  2_000_000_000, 4_000_000_000, 8_000_000_000,
  20_000_000_000, 40_000_000_000,
];

/** Rows an exported record holds at each depth (`4000 × mult + 64`). */
const DEPTH_ROWS: Record<Depth, number> = {
  "4k": 4_064,
  "40k": 40_064,
  "512k": 400_064,
  "1m": 800_064,
};

/**
 * Real-time sample-rate ceiling, in Sa/s, by channel count.
 *
 * Measured on hardware (`cargo run --bin rate_sweep`, 2026-07-22): with one channel the
 * exported record never samples faster than 800 MSa/s, and with two it never beats
 * 400 MSa/s — the ADC is shared, so a second channel halves it exactly.
 *
 * Below ~8 ns/div the scope reports a *faster* interval still (1 ns and 2 ns respectively),
 * which is equivalent-time sampling of a repetitive signal rather than a real-time rate. It
 * is not modelled: treating it as real would over-promise on a one-shot capture, and
 * under-stating there is the safe direction.
 */
function realtimeCeiling(channelCount: number): number {
  return channelCount > 1 ? 400e6 : 800e6;
}

/**
 * Snap a sample rate down to the ladder the instrument actually offers (1-2-4-8 per decade).
 *
 * The scope does not sample at whatever rate the division geometry implies; it picks a rung.
 * That is why a measured interval runs 1.25× the naive `time_per_div / samples_per_div` even
 * well below the ceiling — at 800 ns/div the geometry wants 250 MSa/s and the scope delivers
 * 200.
 */
function quantiseRate(rate: number): number {
  if (!(rate > 0)) return 0;
  const decade = Math.pow(10, Math.floor(Math.log10(rate)));
  for (const step of [8, 4, 2, 1]) {
    if (step * decade <= rate * 1.000001) return step * decade;
  }
  return decade;
}

/** What a capture at a given configuration will actually deliver. */
export interface CapturePlan {
  /** The SEC/DIV rung the scope will land on, in nanoseconds. */
  timePerDivNs: number;
  /** Total time the record covers — it spans exactly 20 divisions. */
  windowS: number;
  /** Interval between samples, in seconds. */
  sampleIntervalS: number;
  /** Samples per bit actually achieved — differs from the request after the ladder snap. */
  samplesPerClock: number;
  /** True when the ADC ceiling (or rate ladder), not the timebase, set the interval. */
  rateLimited: boolean;
}

/**
 * Predict the capture window for a configuration.
 *
 * The record spans exactly 20 divisions at `200 × mult` samples per division, so the ideal
 * time/div to put `samplesPerClock` samples on the fastest bit is
 * `bit_period × samples_per_div / samples_per_clock`. The scope only offers the fixed ladder,
 * so this snaps **down** to the nearest rung (meeting or exceeding the requested resolution);
 * that snap is what sets the real window and the achieved samples/bit.
 *
 * Returns `null` for an incoherent configuration.
 */
export function capturePlan(
  maxFreqHz: number,
  samplesPerClock: number,
  depth: Depth,
  channelCount = 1,
): CapturePlan | null {
  const rows = DEPTH_ROWS[depth];
  if (!rows || !(maxFreqHz > 0) || !(samplesPerClock > 0)) return null;

  const samplesPerDiv = (rows - 64) / 20;
  const bitNs = 1e9 / maxFreqHz;
  const ideal = (bitNs * samplesPerDiv) / samplesPerClock;

  // Largest rung not exceeding the ideal → at least the requested resolution.
  let index = 0;
  for (let i = 0; i < TB_TO_NS.length; i++) {
    if (TB_TO_NS[i] <= ideal) index = i;
  }
  const timePerDivNs = TB_TO_NS[index];

  // What the instrument will really sample at: the division geometry asks for a rate, the
  // ADC caps it, and the scope snaps to a rung. Without this the readout claims resolution
  // the hardware cannot deliver — at 8 ns/div the naive figure over-states it 25-fold.
  const wantedRate = samplesPerDiv / (timePerDivNs * 1e-9);
  const rate = quantiseRate(Math.min(wantedRate, realtimeCeiling(channelCount)));
  const sampleIntervalS = rate > 0 ? 1 / rate : 0;
  const dtNs = sampleIntervalS * 1e9;

  return {
    timePerDivNs,
    windowS: 20 * timePerDivNs * 1e-9,
    sampleIntervalS,
    samplesPerClock: dtNs > 0 ? bitNs / dtNs : 0,
    rateLimited: rate < wantedRate * 0.999,
  };
}

/** A compact duration: `80 µs`, `1.2 ms`, `2.5 s`. */
export function formatDuration(seconds: number): string {
  const units: [number, string][] = [
    [1, "s"],
    [1e-3, "ms"],
    [1e-6, "µs"],
    [1e-9, "ns"],
  ];
  for (const [scale, name] of units) {
    if (seconds >= scale) {
      const v = seconds / scale;
      const text = v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2);
      return `${parseFloat(text)} ${name}`;
    }
  }
  return `${parseFloat((seconds * 1e9).toFixed(2))} ns`;
}
