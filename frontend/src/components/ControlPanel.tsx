import { useEffect, useRef, useState } from "react";
import type { CaptureConfig, Depth, Protocol } from "../api";
import { capturePlan, formatDuration } from "../timebase";
import { ChannelSetup, channelSummary, DEFAULT_CHANNEL_SETUP, probeFactor } from "./ChannelSetup";
import { Section } from "./Section";
import { TriggerPanel, triggerSummary } from "./TriggerPanel";
import type { TriggerConfig } from "../api";

/** Bounds for the samples-per-clock control (slider and typed input share them). */
const SPC_MIN = 4;
const SPC_MAX = 1000;

interface Props {
  config: CaptureConfig;
  onChange: (patch: Partial<CaptureConfig>) => void;
  connected: boolean;
  prepared: boolean;
  busy: null | "connect" | "prepare" | "capture" | "card";
  onPrepare: () => void;
  onCapture: () => void;
  /** Which sections are expanded, and how to fold one. */
  panels: Record<string, boolean>;
  onTogglePanel: (id: string) => void;
  /** The trigger configuration, applied to the scope by Prepare. */
  trigger: TriggerConfig;
  onTriggerChange: (next: TriggerConfig) => void;
}

/**
 * Which channel the trigger's level is measured against.
 *
 * Under Alter only CH1's level is reachable, so that is the channel whose probe scales it.
 * A non-analog source has no volts figure at all, and the level falls back to divisions.
 */
function triggerSourceChannel(trigger: TriggerConfig): number {
  if (trigger.kind === "alter") return 1;
  return trigger.source === "ch2" ? 2 : 1;
}

const DEPTHS: Depth[] = ["4k", "40k", "512k", "1m"];
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
  const { config, onChange, connected, prepared, busy } = props;
  const toggleChannel = (ch: number) => {
    const has = config.channels.includes(ch);
    const next = has
      ? config.channels.filter((c) => c !== ch)
      : [...config.channels, ch].sort();
    const patch: Partial<CaptureConfig> = { channels: next };
    // 1M memory depth is single-channel only, so adding a second channel drops it to 512K.
    if (next.length > 1 && config.depth === "1m") patch.depth = "512k";
    onChange(patch);
  };

  const lines = LINES[config.protocol];
  const dualNeeded = config.protocol === "spi" || config.protocol === "i2c";
  const missingChannels = dualNeeded && config.channels.length < 2;
  // A collapsed section still has to answer "how is this set", so each carries a summary.
  const open = (id: string) => props.panels[id] !== false;


  return (
    <div className="sidebar">
      {/* Settings scroll; the actions below stay pinned so Prepare/Capture are reachable
          however tall the panel grows. */}
      <div className="sidebar-scroll">
        <Section
          title="Acquisition"
          open={open("acquisition")}
          onToggle={() => props.onTogglePanel("acquisition")}
          summary={`${config.channels.map((c) => `CH${c}`).join("+") || "no channels"} · ${config.depth.toUpperCase()}`}
        >
          <Acquisition config={config} onChange={onChange} toggleChannel={toggleChannel} />
        </Section>

        <Section
          title="Channel setup"
          open={open("channels")}
          onToggle={() => props.onTogglePanel("channels")}
          summary={channelSummary(config)}
        >
          <ChannelSetup config={config} onChange={onChange} disabled={busy !== null} />
        </Section>

        <Section
          title="Trigger"
          open={open("trigger")}
          onToggle={() => props.onTogglePanel("trigger")}
          summary={triggerSummary(props.trigger)}
        >
          <TriggerPanel
            busy={busy !== null}
            probeFactor={probeFactor(
              (config.channelsSetup[triggerSourceChannel(props.trigger) - 1] ??
                DEFAULT_CHANNEL_SETUP).probe,
            )}
            value={props.trigger}
            onChange={props.onTriggerChange}
          />
        </Section>

        <Section
          title="Protocol decode"
          open={open("decode")}
          onToggle={() => props.onTogglePanel("decode")}
          summary={
            config.protocol === "none" ? (
              "off"
            ) : (
              <>
                {config.protocol.toUpperCase()}
                {lines.clock && ` · CLK CH${config.clockChannel}`}
                {lines.data && ` · DAT CH${config.dataChannel}`}
              </>
            )
          }
        >
          <Decoder config={config} onChange={onChange} lines={lines} />
          {missingChannels && (
            <span className="hint" style={{ color: "var(--warn)" }}>
              {config.protocol.toUpperCase()} needs both channels — enable CH1 and CH2.
            </span>
          )}
        </Section>
      </div>

      <div className="sidebar-footer">
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
            {busy === "capture" ? "Capturing…" : "② Arm capture"}
          </button>
        </div>
      </div>
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
    <>
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

      <div className="field">
        <span className="name">Signal frequency</span>
        <FrequencyInput
          hz={config.maxFreqHz}
          onChange={(hz) => onChange({ maxFreqHz: hz })}
        />
      </div>

      <div className="field">
        <span className="name">Samples / clock</span>
        <div className="slider-row">
          <input
            type="range"
            min={SPC_MIN}
            max={SPC_MAX}
            step={1}
            value={config.samplesPerCycle}
            onChange={(e) => onChange({ samplesPerCycle: Number(e.target.value) })}
          />
          <NumberInput
            value={config.samplesPerCycle}
            min={SPC_MIN}
            max={SPC_MAX}
            onChange={(n) => onChange({ samplesPerCycle: n })}
          />
        </div>
      </div>
      <CaptureWindow config={config} />

      <div className="field">
        <span className="name">Memory depth</span>
        <div className="segmented">
          {DEPTHS.map((d) => {
            // 1M uses the whole acquisition memory, so it is available only with a single
            // channel — disable it (and explain) while both channels are on.
            const disabled = d === "1m" && config.channels.length !== 1;
            return (
              <button
                key={d}
                className={config.depth === d ? "active" : ""}
                disabled={disabled}
                title={disabled ? "1M is single-channel only" : undefined}
                onClick={() => onChange({ depth: d })}
              >
                {d.toUpperCase()}
              </button>
            );
          })}
        </div>
        {config.depth === "1m" && (
          <span className="hint">1M uses full memory — single channel only.</span>
        )}
      </div>
    </>
  );
}

/** How much time the capture will actually cover, given the max frequency, the requested
 * samples/clock, the memory depth, and the scope's fixed SEC/DIV ladder. The ladder snap is
 * why the achieved samples/clock can differ from what was asked for. */
function CaptureWindow({ config }: { config: CaptureConfig }) {
  const plan = capturePlan(
    config.maxFreqHz,
    config.samplesPerCycle,
    config.depth,
    config.channels.length,
  );
  if (!plan) return <span className="hint">Set a frequency to size the capture.</span>;
  return (
    <span className="hint">
      Captures <strong>{formatDuration(plan.windowS)}</strong> at{" "}
      {formatDuration(plan.timePerDivNs * 1e-9)}/div — {Math.round(plan.samplesPerClock)}{" "}
      samples/clock actual
      {plan.rateLimited && (
        <>
          {" "}
          <span style={{ color: "var(--warn)" }}>
            (ADC-limited to {formatDuration(plan.sampleIntervalS)}/sample)
          </span>
        </>
      )}
      .
    </span>
  );
}

/** A number field that keeps its own text so it can be cleared and retyped freely, clamping
 * into range when the edit finishes. */
function NumberInput({
  value,
  min,
  max,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  onChange: (n: number) => void;
}) {
  const [text, setText] = useState(String(value));
  const editing = useRef(false);

  // Reflect changes from the slider unless the user is mid-edit.
  useEffect(() => {
    if (!editing.current) setText(String(value));
  }, [value]);

  // A plain text box (not type="number") so it carries no spinner arrows, matching the
  // frequency field.
  return (
    <input
      className="slider-num"
      type="text"
      inputMode="numeric"
      value={text}
      onFocus={() => (editing.current = true)}
      onBlur={() => {
        editing.current = false;
        const n = Math.min(max, Math.max(min, Math.round(Number(text)) || min));
        setText(String(n));
        onChange(n);
      }}
      onChange={(e) => {
        setText(e.target.value);
        const n = Math.round(Number(e.target.value));
        if (Number.isFinite(n) && n >= min && n <= max) onChange(n);
      }}
    />
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
  // Assigning a line to the channel the OTHER line is on swaps them, so clock and data stay
  // distinct yet can be freely switched between CH1 and CH2.
  const setClock = (ch: number) =>
    onChange(
      ch === config.dataChannel
        ? { clockChannel: ch, dataChannel: config.clockChannel }
        : { clockChannel: ch },
    );
  const setData = (ch: number) =>
    onChange(
      ch === config.clockChannel
        ? { dataChannel: ch, clockChannel: config.dataChannel }
        : { dataChannel: ch },
    );

  return (
    <>
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
        <ChannelPicker name={lines.clock} value={config.clockChannel} onChange={setClock} />
      )}
      {lines.data && (
        <ChannelPicker name={lines.data} value={config.dataChannel} onChange={setData} />
      )}
    </>
  );
}

/** Assign a protocol line to a physical channel. */
function ChannelPicker({
  name,
  value,
  onChange,
}: {
  name: string;
  value?: number;
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
      <select value={unit} onChange={(e) => setUnit(e.target.value)} style={{ width: 70, flexShrink: 0 }}>
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

