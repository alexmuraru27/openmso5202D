import { useEffect, useRef } from "react";
import type { DecodedItem } from "../api";
import type { Cursor } from "./WaveformView";

interface Props {
  decoded: DecodedItem[];
  /** Measurement cursors, used to highlight the byte each one sits on. */
  cursors: Cursor[];
  /** Record time of the trigger, so byte times read the same as the plot's axis. */
  triggerS: number;
  /** Selecting a byte zooms the plot to it and brackets it with cursors. */
  onSelect?: (item: DecodedItem) => void;
}

/** Whether an event carries a byte value rather than being a bus marker. */
const isByte = (item: DecodedItem) => item.kind === "byte" || item.kind === "address";

/** A byte's printable character, or `.` — handy for reading UART text at a glance. */
function ascii(value: number): string {
  return value >= 0x20 && value < 0x7f ? String.fromCharCode(value) : ".";
}

/** Byte time relative to the trigger, compact enough for a narrow column. */
function formatTime(s: number): string {
  const a = Math.abs(s);
  const sign = s < 0 ? "−" : "+";
  if (a < 1e-6) return `${sign}${(a * 1e9).toFixed(0)}ns`;
  if (a < 1e-3) return `${sign}${(a * 1e6).toFixed(1)}µs`;
  if (a < 1) return `${sign}${(a * 1e3).toFixed(2)}ms`;
  return `${sign}${a.toFixed(3)}s`;
}

/**
 * The decode as a plain list — every byte in hex, decimal and ASCII.
 *
 * The waveform shows bytes where they happened; this shows them as data, which is what you
 * want when checking a payload rather than its timing. Moving a measurement cursor
 * highlights the byte under it and scrolls it into view, so the two views stay tied
 * together.
 */
export function ByteList({ decoded, cursors, triggerS, onSelect }: Props) {
  const bytes = decoded.filter(isByte);
  const activeRef = useRef<HTMLDivElement>(null);

  /** Index of the byte spanning a cursor's instant, or -1. */
  const at = (time: number) =>
    bytes.findIndex((item) => time >= item.startS && time <= item.endS);
  const aIndex = cursors[0] ? at(cursors[0].t) : -1;
  const bIndex = cursors[1] ? at(cursors[1].t) : -1;

  // Follow the cursor: dragging along the trace should walk the list with it.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [aIndex, bIndex]);

  if (bytes.length === 0) return null;

  return (
    <div className="bytes-panel">
      <div className="bytes-head">
        <span className="label">Decoded</span>
        <span className="count">{bytes.length} bytes</span>
      </div>
      <div className="bytes-cols">
        <span className="i">#</span>
        <span className="hex">hex</span>
        <span className="dec">dec</span>
        <span className="chr">chr</span>
        <span className="t">time</span>
      </div>
      <div className="bytes-list">
        {bytes.map((item, index) => {
          const value = item.value ?? 0;
          const bad = item.text.includes("!");
          const mark = index === aIndex ? "a" : index === bIndex ? "b" : "";
          return (
            <div
              key={`${item.startS}-${index}`}
              ref={mark === "a" || (aIndex < 0 && mark === "b") ? activeRef : undefined}
              className={`byte-row ${mark} ${bad ? "bad" : ""} ${onSelect ? "pick" : ""}`}
              title={bad ? "framing/ACK error" : "Zoom the plot to this byte"}
              onClick={() => onSelect?.(item)}
            >
              <span className="i">{index}</span>
              <span className="hex">0x{value.toString(16).toUpperCase().padStart(2, "0")}</span>
              <span className="dec">{value}</span>
              <span className="chr">{ascii(value)}</span>
              <span className="t">{formatTime(item.startS - triggerS)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
