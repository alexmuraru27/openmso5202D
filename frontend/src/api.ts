// Typed wrappers over the Tauri backend-API commands, plus the shapes they exchange.
// These mirror the serde types in `src-tauri/src/api.rs` (camelCase).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Protocol = "none" | "uart" | "spi" | "i2c";
export type Depth = "4k" | "40k" | "512k";

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

/** Subscribe to a progress stream (`prepare:progress` / `capture:progress`). */
export function onProgress(
  event: "prepare:progress" | "capture:progress",
  handler: (p: ProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<ProgressPayload>(event, (e) => handler(e.payload));
}
