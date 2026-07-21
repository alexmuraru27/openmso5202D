import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  loadCsvs,
  type CaptureConfig,
  type CaptureResult,
  type CardFile,
  type CsvSlot,
} from "../api";

interface Props {
  connected: boolean;
  busy: boolean;
  config: CaptureConfig;
  /** Files currently on the card, offered alongside the disk picker. */
  cardFiles: CardFile[];
  onBusyChange: (busy: boolean) => void;
  onResult: (result: CaptureResult) => void;
}

/** The channel slots a plot can hold. */
const SLOTS = [1, 2] as const;

type Assignment = { source: "card" | "local"; value: string };

/** Last path component, for a readable label. */
function basename(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

/**
 * Map CSVs onto channels and plot them — from the scope's card or from this machine.
 *
 * A CSV holds one channel and carries nothing that says *which*, so rather than guessing,
 * this asks. Assigning the clock file to one channel and the data file to the other is what
 * makes a saved SPI/I²C capture decodable after the fact, exactly like a live one.
 *
 * Local files are read straight off disk, so previously downloaded captures can be reviewed
 * with the scope unplugged.
 */
export function LoadCsv({
  connected,
  busy,
  config,
  cardFiles,
  onBusyChange,
  onResult,
}: Props) {
  const [slots, setSlots] = useState<Record<number, Assignment | undefined>>({});
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const assign = (channel: number, next: Assignment | undefined) =>
    setSlots((prev) => {
      const updated: Record<number, Assignment | undefined> = { ...prev, [channel]: next };
      // The same file on both channels would just plot one trace twice.
      for (const other of SLOTS) {
        if (other !== channel && next && updated[other]?.value === next.value) {
          updated[other] = undefined;
        }
      }
      return updated;
    });

  const browse = async (channel: number) => {
    const picked = await open({
      title: `Choose a CSV for CH${channel}`,
      multiple: false,
      directory: false,
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    if (typeof picked === "string") assign(channel, { source: "local", value: picked });
  };

  const load = async () => {
    const chosen: CsvSlot[] = SLOTS.filter((ch) => slots[ch]).map((ch) => ({
      channel: ch,
      source: slots[ch]!.source,
      value: slots[ch]!.value,
    }));
    if (chosen.length === 0) return;
    setWorking(true);
    onBusyChange(true);
    setError(null);
    try {
      onResult(await loadCsvs(chosen, config));
      setStatus(chosen.map((s) => `CH${s.channel}=${basename(s.value)}`).join("  "));
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
      onBusyChange(false);
    }
  };

  const chosen = SLOTS.filter((ch) => slots[ch]);
  // Only a card file needs the instrument; a disk-only load works offline.
  const needsScope = chosen.some((ch) => slots[ch]!.source === "card");
  const blocked = busy || working || (needsScope && !connected);

  return (
    <div className="group">
      <div className="label">Load CSV</div>

      {SLOTS.map((ch) => {
        const slot = slots[ch];
        return (
          <div className="field" key={ch}>
            <span className="name">CH{ch}</span>
            <div className="slot-row">
              <select
                value={slot?.source === "card" ? slot.value : ""}
                disabled={busy || working}
                onChange={(e) =>
                  assign(ch, e.target.value ? { source: "card", value: e.target.value } : undefined)
                }
              >
                <option value="">{cardFiles.length ? "— card file —" : "— none —"}</option>
                {cardFiles.map((file) => (
                  <option key={file.name} value={file.name}>
                    {file.name}
                  </option>
                ))}
              </select>
              <button
                className="btn"
                disabled={busy || working}
                onClick={() => browse(ch)}
                title="Choose a CSV from this machine"
              >
                Browse…
              </button>
            </div>
            {slot && (
              <span className="slot-chosen">
                <span className="src">{slot.source === "card" ? "card" : "disk"}</span>
                <span className="val" title={slot.value}>
                  {basename(slot.value)}
                </span>
                <button className="clear" onClick={() => assign(ch, undefined)} title="Clear">
                  ×
                </button>
              </span>
            )}
          </div>
        );
      })}

      <button className="btn block" disabled={blocked || chosen.length === 0} onClick={load}>
        {working ? "Loading…" : "Load traces"}
      </button>

      {needsScope && !connected && (
        <span className="hint" style={{ color: "var(--warn)" }}>
          A card file needs the scope connected — or pick files from disk.
        </span>
      )}
      {status && !error && <span className="hint">{status}</span>}
      {error && (
        <span className="hint" style={{ color: "var(--danger)" }}>
          {error}
        </span>
      )}
    </div>
  );
}
