import { useEffect, useRef, useState } from "react";
import type { CaptureConfig, Depth, Protocol } from "../api";

interface Props {
  config: CaptureConfig;
  onChange: (patch: Partial<CaptureConfig>) => void;
  connected: boolean;
  prepared: boolean;
  busy: null | "connect" | "prepare" | "capture";
  error: string | null;
  onPrepare: () => void;
  onCapture: () => void;
}

const DEPTHS: Depth[] = ["4k", "40k", "512k"];
const PROTOCOLS: { id: Protocol; label: string }[] = [
  { id: "none", label: "None" },
  { id: "uart", label: "UART" },
  { id: "spi", label: "SPI" },
  { id: "i2c", label: "I²C" },
];

/** The line labels a protocol needs assigned to physical channels. */
const LINES: Record<Protocol, { clock?: string; data?: string }> = {
  none: {},
  uart: { data: "Data (TX)" },
  spi: { clock: "Clock (SCLK)", data: "Data (MOSI)" },
  i2c: { clock: "Clock (SCL)", data: "Data (SDA)" },
};

export function ControlPanel(props: Props) {
  const { config, onChange, connected, prepared, busy, error } = props;
  const toggleChannel = (ch: number) => {
    const has = config.channels.includes(ch);
    const next = has
      ? config.channels.filter((c) => c !== ch)
      : [...config.channels, ch].sort();
    onChange({ channels: next });
  };

  const lines = LINES[config.protocol];
  const dualNeeded = config.protocol === "spi" || config.protocol === "i2c";
  const missingChannels = dualNeeded && config.channels.length < 2;

  return (
    <div className="sidebar">
      <Acquisition config={config} onChange={onChange} toggleChannel={toggleChannel} />

      <Decoder config={config} onChange={onChange} lines={lines} />

      {missingChannels && (
        <div className="field">
          <span className="hint" style={{ color: "var(--warn)" }}>
            {config.protocol.toUpperCase()} needs both channels — enable CH1 and CH2.
          </span>
        </div>
      )}

      <div className="actions">
        <button
          className="btn block lg"
          disabled={!connected || busy !== null}
          onClick={props.onPrepare}
        >
          {busy === "prepare" ? "Preparing…" : "① Prepare"}
        </button>
        <button
          className="btn primary block lg"
          disabled={!connected || !prepared || busy !== null}
          onClick={props.onCapture}
        >
          {busy === "capture" ? "Capturing…" : "② Capture"}
        </button>
      </div>

      {error && (
        <div className="field">
          <span className="hint err" style={{ color: "var(--danger)" }}>
            {error}
          </span>
        </div>
      )}
    </div>
  );
}

function Acquisition({
  config,
  onChange,
  toggleChannel,
}: {
  config: CaptureConfig;
  onChange: (patch: Partial<CaptureConfig>) => void;
  toggleChannel: (ch: number) => void;
}) {
  return (
    <div className="group">
      <div className="label">Acquisition</div>

      <div className="field">
        <span className="name">Channels</span>
        <div className="chips">
          {[1, 2].map((ch) => (
            <div
              key={ch}
              className={`chip ch${ch} ${config.channels.includes(ch) ? "on" : ""}`}
              onClick={() => toggleChannel(ch)}
            >
              <span className="sw" style={{ background: ch === 1 ? "var(--ch1)" : "var(--ch2)" }} />
              CH{ch}
            </div>
          ))}
        </div>
      </div>

      <div className="field-row">
        <div className="field" style={{ flex: 1.4 }}>
          <span className="name">Max frequency</span>
          <FrequencyInput
            hz={config.maxFreqHz}
            onChange={(hz) => onChange({ maxFreqHz: hz })}
          />
        </div>
        <div className="field" style={{ flex: 1 }}>
          <span className="name">Samples / clock</span>
          <div className="input-suffix">
            <input
              type="number"
              min={4}
              max={500}
              value={config.samplesPerCycle}
              onChange={(e) => onChange({ samplesPerCycle: Number(e.target.value) })}
            />
            <span className="suffix">/bit</span>
          </div>
        </div>
      </div>
      <span className="hint">Higher samples/clock = more resolution, shorter captured window.</span>

      <div className="field">
        <span className="name">Memory depth</span>
        <div className="segmented">
          {DEPTHS.map((d) => (
            <button
              key={d}
              className={config.depth === d ? "active" : ""}
              onClick={() => onChange({ depth: d })}
            >
              {d.toUpperCase()}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

function Decoder({
  config,
  onChange,
  lines,
}: {
  config: CaptureConfig;
  onChange: (patch: Partial<CaptureConfig>) => void;
  lines: { clock?: string; data?: string };
}) {
  return (
    <div className="group">
      <div className="label">Protocol decode</div>
      <div className="segmented">
        {PROTOCOLS.map((p) => (
          <button
            key={p.id}
            className={config.protocol === p.id ? "active accent" : ""}
            onClick={() => onChange({ protocol: p.id })}
          >
            {p.label}
          </button>
        ))}
      </div>

      {lines.clock && (
        <ChannelPicker
          name={lines.clock}
          value={config.clockChannel}
          other={config.dataChannel}
          onChange={(ch) => onChange({ clockChannel: ch })}
        />
      )}
      {lines.data && (
        <ChannelPicker
          name={lines.data}
          value={config.dataChannel}
          other={config.clockChannel}
          onChange={(ch) => onChange({ dataChannel: ch })}
        />
      )}
    </div>
  );
}

/** Assign a protocol line to a physical channel, blocking a clash with `other`. */
function ChannelPicker({
  name,
  value,
  other,
  onChange,
}: {
  name: string;
  value?: number;
  other?: number;
  onChange: (ch: number) => void;
}) {
  return (
    <div className="field">
      <span className="name">{name}</span>
      <div className="segmented">
        {[1, 2].map((ch) => (
          <button
            key={ch}
            className={value === ch ? "active" : ""}
            disabled={other === ch}
            title={other === ch ? "already used by the other line" : undefined}
            onClick={() => onChange(ch)}
          >
            CH{ch}
          </button>
        ))}
      </div>
    </div>
  );
}

const UNIT_SCALE: Record<string, number> = { Hz: 1, kHz: 1e3, MHz: 1e6 };

/** A frequency input that carries its own unit, so the user types "2 MHz" not "2000000".
 *
 * Keeps its own text so the field can be cleared and retyped freely — a plain controlled
 * number input would coerce an empty string back to 0 mid-edit. The parent's value only
 * flows in when the field is not being edited. */
function FrequencyInput({ hz, onChange }: { hz: number; onChange: (hz: number) => void }) {
  const unit = hz >= 1e6 ? "MHz" : hz >= 1e3 ? "kHz" : "Hz";
  const scale = UNIT_SCALE[unit];
  const [text, setText] = useState(() => trimNumber(hz / scale));
  const editing = useRef(false);

  // Reflect external changes (e.g. a unit switch) unless the user is mid-edit.
  useEffect(() => {
    if (!editing.current) setText(trimNumber(hz / scale));
  }, [hz, scale]);

  const handleText = (v: string) => {
    setText(v);
    const n = parseFloat(v);
    if (Number.isFinite(n) && n >= 0) onChange(n * scale);
  };
  const setUnit = (u: string) => onChange((parseFloat(text) || 0) * UNIT_SCALE[u]);

  return (
    <div style={{ display: "flex", gap: 6 }}>
      <input
        type="text"
        inputMode="decimal"
        value={text}
        onFocus={() => (editing.current = true)}
        onBlur={() => {
          editing.current = false;
          setText(trimNumber(hz / scale));
        }}
        onChange={(e) => handleText(e.target.value)}
        style={{ flex: 1 }}
      />
      <select value={unit} onChange={(e) => setUnit(e.target.value)} style={{ width: 66, flexShrink: 0 }}>
        <option>Hz</option>
        <option>kHz</option>
        <option>MHz</option>
      </select>
    </div>
  );
}

/** Format a number without trailing float noise (1000000 → "1", 2.5 stays "2.5"). */
function trimNumber(n: number): string {
  if (!Number.isFinite(n)) return "";
  return String(+n.toFixed(6));
}

