// Persisted UI settings.
//
// The capture configuration survives a restart, so the app comes back set up the way it was
// left. Stored in the webview's localStorage — it is per-app and needs no plugin, and the
// data is small and non-critical: if it is missing or malformed the defaults simply apply.

import type { CaptureConfig, Depth, Protocol } from "./api";

/** Bumped when a stored shape can no longer be merged sensibly, so old data is ignored. */
const KEY = "openmso5202d.config.v1";

export const DEFAULT_CONFIG: CaptureConfig = {
  channels: [1, 2],
  maxFreqHz: 1_000_000,
  samplesPerCycle: 20,
  depth: "40k",
  protocol: "none",
  clockChannel: 1,
  dataChannel: 2,
};

const DEPTHS: Depth[] = ["4k", "40k", "512k", "1m"];
const PROTOCOLS: Protocol[] = ["none", "uart", "spi", "i2c"];

/** Samples-per-clock bounds, mirroring the control's slider. */
const SPC_MIN = 4;
const SPC_MAX = 1000;

/**
 * Fold stored values over the defaults, dropping anything invalid.
 *
 * Stored settings are the one input the app cannot control the shape of — an older build, a
 * hand-edited value, or a half-written record would otherwise put the UI (and the backend's
 * validation) into a state the controls cannot express.
 */
export function sanitise(raw: Partial<CaptureConfig> | null | undefined): CaptureConfig {
  const merged = { ...DEFAULT_CONFIG, ...(raw ?? {}) };

  const channels = Array.isArray(merged.channels)
    ? [...new Set(merged.channels.filter((c) => c === 1 || c === 2))].sort()
    : [];
  merged.channels = channels.length ? channels : DEFAULT_CONFIG.channels;

  if (!DEPTHS.includes(merged.depth)) merged.depth = DEFAULT_CONFIG.depth;
  if (!PROTOCOLS.includes(merged.protocol)) merged.protocol = DEFAULT_CONFIG.protocol;

  if (!Number.isFinite(merged.maxFreqHz) || merged.maxFreqHz <= 0) {
    merged.maxFreqHz = DEFAULT_CONFIG.maxFreqHz;
  }
  const spc = Math.round(merged.samplesPerCycle);
  merged.samplesPerCycle = Number.isFinite(spc)
    ? Math.min(SPC_MAX, Math.max(SPC_MIN, spc))
    : DEFAULT_CONFIG.samplesPerCycle;

  for (const line of ["clockChannel", "dataChannel"] as const) {
    if (merged[line] !== 1 && merged[line] !== 2) merged[line] = DEFAULT_CONFIG[line];
  }

  // 1M uses the whole acquisition memory, so it only exists single-channel. A stored pair
  // would otherwise be rejected by the backend the moment Prepare ran.
  if (merged.depth === "1m" && merged.channels.length > 1) merged.depth = "512k";

  return merged;
}

/** The last saved configuration, or the defaults. */
export function loadConfig(): CaptureConfig {
  try {
    const stored = window.localStorage.getItem(KEY);
    return sanitise(stored ? (JSON.parse(stored) as Partial<CaptureConfig>) : null);
  } catch {
    return { ...DEFAULT_CONFIG };
  }
}

/** Remember the configuration for the next run. Failure here is never worth an error. */
export function saveConfig(config: CaptureConfig): void {
  try {
    window.localStorage.setItem(KEY, JSON.stringify(config));
  } catch {
    /* storage unavailable or full — the app works fine without persistence */
  }
}

/** Forget the stored configuration. */
export function clearConfig(): void {
  try {
    window.localStorage.removeItem(KEY);
  } catch {
    /* nothing to do */
  }
}
