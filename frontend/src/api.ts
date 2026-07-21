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

export function prepare(config: CaptureConfig): Promise<void> {
  return invoke("prepare", { config });
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
  event: "prepare:progress" | "capture:progress" | "card:progress",
  handler: (p: ProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<ProgressPayload>(event, (e) => handler(e.payload));
}
