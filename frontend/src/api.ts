// Typed wrappers over the Tauri backend-API commands, plus the shapes they exchange.
// These mirror the serde types in `src-tauri/src/api.rs` (camelCase).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Protocol = "none" | "uart" | "spi" | "i2c";
export type Depth = "4k" | "40k" | "512k" | "1m";

export interface CaptureConfig {
  channels: number[];
  maxFreqHz: number;
  samplesPerCycle: number;
  depth: Depth;
  protocol: Protocol;
  clockChannel?: number;
  dataChannel?: number;
}

export interface ScopeStatus {
  connected: boolean;
  location?: string;
}

export interface ChannelData {
  channel: number;
  label: string;
  volts: number[];
  /** The instrument's vertical scale for this trace, mV per division (`#voltbase`). */
  voltsPerDivMv?: number;
}

export interface DecodedItem {
  startS: number;
  endS: number;
  text: string;
  kind: string;
  /** Raw byte value for a byte/address event; absent for a bus marker. */
  value?: number;
  channel: number;
}

export interface CaptureResult {
  sampleIntervalS: number;
  channels: ChannelData[];
  decoded: DecodedItem[];
}

export interface ProgressPayload {
  index: number;
  total: number;
  label: string;
  state: "started" | "advanced" | "completed" | "failed";
  fraction: number;
  detail?: string;
}

export function scopeStatus(): Promise<ScopeStatus> {
  return invoke("scope_status");
}

export function connectScope(): Promise<ScopeStatus> {
  return invoke("connect_scope");
}

/**
 * Configure the scope for a capture, including its trigger.
 *
 * The trigger goes in here rather than being applied on its own because prepare starts with
 * a factory Default Setup, which would undo it.
 */
export function prepare(
  config: CaptureConfig,
  trigger: TriggerConfig | null,
): Promise<void> {
  return invoke("prepare", { config, trigger });
}

export function capture(): Promise<CaptureResult> {
  return invoke("capture");
}

/** A waveform CSV on the scope's memory card. */
export interface CardFile {
  name: string;
  size: number;
}

/** A card file after it has been pulled onto this machine. */
export interface DownloadedFile {
  name: string;
  path: string;
  bytes: number;
}

export function listCardFiles(): Promise<CardFile[]> {
  return invoke("list_card_files");
}

/**
 * Copy card files onto this machine.
 *
 * `dest` is what a native dialog returned: for one file the full target path, for several
 * the directory to fill. Omit it to fall back to `~/openmso5202D/`.
 */
export function downloadCardFiles(
  names: string[],
  dest?: string,
): Promise<DownloadedFile[]> {
  return invoke("download_card_files", { names, dest: dest ?? null });
}

/**
 * Re-run the decoder over the traces already on screen and return the new annotation.
 *
 * The samples stay in the backend, so switching protocol or swapping the clock/data lines
 * re-decodes without another capture and without moving megabytes of samples.
 */
export function redecode(config: CaptureConfig): Promise<DecodedItem[]> {
  return invoke("redecode", { config });
}

/** One CSV to plot, and the channel it becomes. */
export interface CsvSlot {
  /** Channel this file is plotted as: 1 or 2. */
  channel: number;
  /** `card` — a filename on the scope's card; `local` — a path on this machine. */
  source: "card" | "local";
  value: string;
}

/**
 * Plot CSVs on explicitly chosen channels.
 *
 * Nothing in a CSV says which channel it came from, so the mapping is stated rather than
 * guessed. Local files are read straight off disk and need no scope connection.
 */
export function loadCsvs(
  slots: CsvSlot[],
  config: CaptureConfig,
): Promise<CaptureResult> {
  return invoke("load_csvs", { slots, config });
}

/** Irreversible: deletes every `WaveData*.csv` on the card. */
export function clearCardFiles(): Promise<void> {
  return invoke("clear_card_files");
}

/** Subscribe to a progress stream. */
export function onProgress(
  event: "prepare:progress" | "capture:progress" | "card:progress" | "trigger:progress",
  handler: (p: ProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<ProgressPayload>(event, (e) => handler(e.payload));
}

// --- trigger ---------------------------------------------------------------

export type TriggerKind = "edge" | "video" | "pulse" | "slope" | "overtime" | "alter";
export type AlterKind = "edge" | "video" | "pulse" | "overtime";
export type TriggerSource = "ch1" | "ch2" | "ext" | "ext5" | "acline";

/** The trigger configuration, mirroring `TriggerConfig` in `src-tauri/src/api.rs`. */
export interface TriggerConfig {
  kind: TriggerKind;
  source: TriggerSource;
  mode: "auto" | "normal";
  coupling: "dc" | "ac" | "noise" | "hfreject" | "lfreject";
  polarity: "positive" | "negative";
  videoStandard: "ntsc" | "pal";
  videoSync: "alllines" | "linenumber" | "oddfield" | "evenfield" | "allfields";
  qualifier: "equal" | "notequal" | "greater" | "less";
  /** Level in the scope's 1/25-division units, relative to screen centre. */
  level: number;
  /** Targets for the knob-only values, keyed by `TriggerValue.id`. Walked to by Prepare. */
  valueTargets: Record<string, number>;
  /** Level in millivolts — reported by the scope, never sent. */
  levelMv?: number;
  /** Millivolts per unit of `level` (the source's volts/div ÷ 25), so an edit can be shown
   *  in volts before it is applied. Absent when the source is not an analog channel. */
  levelMvPerUnit?: number;
  /** The `level` at which the trigger sits on the source's ground. */
  levelZero: number;
  /** Whether the level applies at all — false for Slope, whose level knob is inert. */
  levelApplies: boolean;
  /** Alter only: CH1's own trigger. */
  alterCh1: AlterChannelConfig;
  /** Alter only: CH2's own trigger. */
  alterCh2: AlterChannelConfig;
}

/** One channel's trigger inside Alter mode. */
export interface AlterChannelConfig {
  kind: AlterKind;
  polarity: "positive" | "negative";
  coupling: TriggerConfig["coupling"];
  qualifier: TriggerConfig["qualifier"];
  videoStandard: TriggerConfig["videoStandard"];
  videoSync: TriggerConfig["videoSync"];
}


/** What a trigger level is worth in volts, once Prepare has set the channel scales. */
export interface LevelScale {
  /** Millivolts per unit of `level` — the volts/division Prepare sets, divided by 25. */
  millivoltsPerUnit: number;
  /** The level at which the source sits on ground. */
  zero: number;
}

/**
 * The scale a trigger level will be measured in after Prepare.
 *
 * Not read from the instrument: Prepare sets each channel's volts/division and re-centres
 * it, so the scale that matters is the one Prepare establishes — and asking for it needs no
 * connection, which is when most of the setting-up happens.
 */
export function levelScale(): Promise<LevelScale> {
  return invoke("level_scale");
}


