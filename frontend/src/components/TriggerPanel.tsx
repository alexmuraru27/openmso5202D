import { useEffect, useState } from "react";
import { formatDuration } from "../timebase";
import {
  levelScale,
  type AlterChannelConfig,
  type AlterKind,
  type TriggerConfig,
  type TriggerKind,
} from "../api";

interface Props {
  /** True while a plan is running; its settings are about to be read, so they are locked. */
  busy: boolean;
  /**
   * Probe attenuation on the trigger's source channel.
   *
   * It multiplies every voltage on that channel — measured: the same level read 20 mV at
   * 1× and 20.0 V at 1000×, while the settings block reported the 1× figure throughout. So
   * a level shown here has to be scaled by it or it understates by up to a thousandfold.
   */
  probeFactor: number;
  /** The configuration being edited. Applied to the scope by Prepare, not by this panel. */
  value: TriggerConfig;
  onChange: (next: TriggerConfig) => void;
}

/** Everything the trigger offers, by the wire names the backend speaks. */
const KINDS: { id: TriggerKind; label: string }[] = [
  { id: "edge", label: "Edge" },
  { id: "pulse", label: "Pulse" },
  { id: "slope", label: "Slope" },
  { id: "video", label: "Video" },
  { id: "overtime", label: "Overtime" },
  { id: "alter", label: "Alter" },
];

/** The sub-types an Alter channel offers — Slope and Alter itself are not among them. */
const ALTER_KINDS: { id: AlterKind; label: string }[] = [
  { id: "edge", label: "Edge" },
  { id: "pulse", label: "Pulse" },
  { id: "video", label: "Video" },
  { id: "overtime", label: "O.T." },
];

type SourceOption = { id: TriggerConfig["source"]; label: string };

const ANALOG: SourceOption[] = [
  { id: "ch1", label: "CH1" },
  { id: "ch2", label: "CH2" },
];
const WITH_EXTERNAL: SourceOption[] = [
  ...ANALOG,
  { id: "ext", label: "EXT" },
  { id: "ext5", label: "EXT/5" },
];

/**
 * Which sources each trigger type offers.
 *
 * Not a UI nicety — the scope genuinely refuses the rest, and its source softkey cycles a
 * shorter ring accordingly. Offering a value the instrument will not take would turn a
 * click into an error message.
 */
const SOURCES: Record<TriggerKind, SourceOption[]> = {
  edge: [...WITH_EXTERNAL, { id: "acline", label: "AC line" }],
  video: WITH_EXTERNAL,
  pulse: WITH_EXTERNAL,
  slope: WITH_EXTERNAL,
  overtime: ANALOG,
  // Alter drives both channels in turn; there is no single source to choose.
  alter: ANALOG,
};

/** The polarity control means a different thing per type, so it is labelled per type. */
const POLARITY: Record<string, { title: string; positive: string; negative: string }> = {
  edge: { title: "Slope", positive: "Rising", negative: "Falling" },
  pulse: { title: "Polarity", positive: "Positive", negative: "Negative" },
  slope: { title: "Slope", positive: "Rising", negative: "Falling" },
  video: { title: "Polarity", positive: "Normal", negative: "Inverted" },
  overtime: { title: "Polarity", positive: "Positive", negative: "Negative" },
};

const COUPLINGS: { id: TriggerConfig["coupling"]; label: string }[] = [
  { id: "dc", label: "DC" },
  { id: "ac", label: "AC" },
  { id: "noise", label: "Noise rej" },
  { id: "hfreject", label: "HF rej" },
  { id: "lfreject", label: "LF rej" },
];

const QUALIFIERS: { id: TriggerConfig["qualifier"]; label: string }[] = [
  { id: "equal", label: "=" },
  { id: "notequal", label: "≠" },
  { id: "greater", label: ">" },
  { id: "less", label: "<" },
];

const VIDEO_SYNCS: { id: TriggerConfig["videoSync"]; label: string }[] = [
  { id: "alllines", label: "All lines" },
  { id: "linenumber", label: "Line no." },
  { id: "oddfield", label: "Odd field" },
  { id: "evenfield", label: "Even field" },
  { id: "allfields", label: "All fields" },
];

/** A channel's Alter trigger, as it starts out. */
export const DEFAULT_ALTER_CHANNEL: AlterChannelConfig = {
  kind: "edge",
  polarity: "positive",
  coupling: "dc",
  qualifier: "greater",
  videoStandard: "ntsc",
  videoSync: "alllines",
};

/** The trigger the app starts with: rising edge on CH1, level just above the baseline. */
export const DEFAULT_TRIGGER: TriggerConfig = {
  kind: "edge",
  source: "ch1",
  mode: "auto",
  coupling: "dc",
  polarity: "positive",
  videoStandard: "ntsc",
  videoSync: "alllines",
  qualifier: "greater",
  level: 13,
  levelZero: 0,
  levelApplies: true,
  alterCh1: DEFAULT_ALTER_CHANNEL,
  alterCh2: DEFAULT_ALTER_CHANNEL,
  valueTargets: {},
};

/**
 * Level movement per click, in the scope's 1/25-division units.
 *
 * **One**, because that is what the instrument's own level knob moves — measured at 20, 100
 * and 500 mV per division, one unit every press. A click here should be the same size as a
 * click there, or the panel counts in a different currency from the scope: at 1 V/division
 * the knob steps 40 mV, and this stepping 5 units made the panel jump 200 mV at a time.
 */
const LEVEL_STEP = 1;

/**
 * What one unit of `level` is worth, before the backend has been asked.
 *
 * Mirrors `CaptureSpec::default()` — 1 V/division ÷ 25 units — because that is the scale
 * Prepare sets, and its Default Setup centres the channel so ground sits at zero. It is a
 * property of the plan, not of the instrument, so it is knowable without asking anything.
 *
 * Held as a constant rather than waiting on `levelScale()` so the level reads in volts from
 * the very first render. Deriving a number this fixed from a call that can fail — an older
 * binary without the command, a rejected promise — meant one broken call silently demoted
 * every level to divisions, which is exactly what happened.
 */
const DEFAULT_LEVEL_SCALE = { perUnit: 1000 / 25, zero: 0 };

/**
 * Every knob-only trigger value, with what it starts from.
 *
 * `factory` is the value the field holds after a **Default Setup**, read off the instrument.
 * That is the right starting point rather than whatever the scope shows now, because Prepare
 * opens with a Default Setup and then walks the knob — so the distance travelled is always
 * measured from here.
 *
 * Held in the UI so the rows render, and the targets can be set, with nothing attached. The
 * `step` values are measured: the times move 10 ns a press, thresholds and counts one unit.
 */
type ValueUnit = "time" | "count" | "level";

const VALUE_CATALOGUE: Record<
  string,
  { label: string; unit: ValueUnit; step: number; factory: number }
> = {
  pulseWidth: { label: "Pulse width", unit: "time", step: 10_000, factory: 500_000 },
  slopeV1: { label: "Threshold V1", unit: "level", step: 1, factory: 50 },
  slopeV2: { label: "Threshold V2", unit: "level", step: 1, factory: -50 },
  slopeTime: { label: "Slope time", unit: "time", step: 10_000, factory: 500_000 },
  overtimeTime: { label: "Overtime", unit: "time", step: 10_000, factory: 500_000 },
  videoLine: { label: "Line number", unit: "count", step: 1, factory: 1 },
  alterCh1PulseWidth: { label: "CH1 pulse width", unit: "time", step: 10_000, factory: 400_000 },
  alterCh1OvertimeTime: { label: "CH1 overtime", unit: "time", step: 10_000, factory: 500_000 },
  alterCh1VideoLine: { label: "CH1 line number", unit: "count", step: 1, factory: 1 },
  alterCh2PulseWidth: { label: "CH2 pulse width", unit: "time", step: 10_000, factory: 500_000 },
  alterCh2OvertimeTime: { label: "CH2 overtime", unit: "time", step: 10_000, factory: 500_000 },
  alterCh2VideoLine: { label: "CH2 line number", unit: "count", step: 1, factory: 0 },
};

/**
 * Which values a configuration offers — the mirror of `TriggerSetup::adjustables()`.
 *
 * A pure function of the draft, so the rows appear whether or not a scope is attached. Only
 * the sub-type that owns a value exposes it: a channel on Edge has none, and a line number
 * exists only while Sync is set to trigger on one.
 */
function valuesFor(config: TriggerConfig): string[] {
  switch (config.kind) {
    case "pulse":
      return ["pulseWidth"];
    case "slope":
      return ["slopeV1", "slopeV2", "slopeTime"];
    case "overtime":
      return ["overtimeTime"];
    case "video":
      return config.videoSync === "linenumber" ? ["videoLine"] : [];
    case "alter":
      return ([1, 2] as const).flatMap((ch) => {
        const channel = ch === 1 ? config.alterCh1 : config.alterCh2;
        const prefix = `alterCh${ch}`;
        if (channel.kind === "pulse") return [`${prefix}PulseWidth`];
        if (channel.kind === "overtime") return [`${prefix}OvertimeTime`];
        if (channel.kind === "video" && channel.videoSync === "linenumber") {
          return [`${prefix}VideoLine`];
        }
        return [];
      });
    default:
      return [];
  }
}

/** Sources whose trigger level the scope reports as a voltage. */
const VOLTAGE_SOURCES: TriggerConfig["source"][] = ["ch1", "ch2"];

/**
 * Trigger setup.
 *
 * These are **settings, not commands**: nothing here touches the scope as you click. The
 * configuration is applied by Prepare, which has to own it — Prepare begins with a factory
 * Default Setup, so a trigger applied beforehand would simply be wiped.
 *
 * The one exception is the continuous values (pulse width, slope thresholds, overtime). The
 * scope offers no keyed entry for those, only its multipurpose knob, so they cannot be part
 * of a configuration that is replayed later — they are live nudges, and act at once.
 */
export function TriggerPanel({ busy, probeFactor, value, onChange }: Props) {
  const [scale, setScale] = useState(DEFAULT_LEVEL_SCALE);

  // What a level will be worth in volts, from the plan rather than the instrument — Prepare
  // sets the channel scales itself, so this is right whether or not a scope is attached.
  useEffect(() => {
    levelScale()
      .then((s) => setScale({ perUnit: s.millivoltsPerUnit, zero: s.zero }))
      .catch(() => {
        /* keep the default — it is the same number, just not confirmed */
      });
  }, []);

  const patch = (next: Partial<TriggerConfig>) => {
    const merged = { ...value, ...next };
    // Changing the type can strand the source on a value the new type does not offer.
    if (!SOURCES[merged.kind].some((s) => s.id === merged.source)) {
      merged.source = SOURCES[merged.kind][0].id;
    }
    // Slope compares between two thresholds rather than against a level.
    merged.levelApplies = merged.kind !== "slope";
    onChange(merged);
  };

  // Nothing here talks to the scope, so the only reason to lock is that a plan is running
  // and its settings are about to be read.
  // The stored scale is at the probe tip's 1× value; the attenuation multiplies it.
  const scaled = { perUnit: scale.perUnit * probeFactor, zero: scale.zero };

  const locked = busy;
  const isAlter = value.kind === "alter";
  const polarity = POLARITY[value.kind];
  const hasQualifier = value.kind === "pulse" || value.kind === "slope";
  // Video puts Standard and Sync where the others put Mode and Coupling.
  const hasMode = value.kind !== "video";
  const hasCoupling = value.kind !== "video";

  return (
    <>
      <Row name="Type">
        <Segmented
          options={KINDS}
          value={value.kind}
          disabled={locked}
          onChange={(kind) => patch({ kind })}
        />
      </Row>

      {isAlter ? (
        <>
          <span className="hint">
            Alternating: each channel triggers on its own terms, taken in turn.
          </span>
          {([1, 2] as const).map((ch) => (
            <AlterEditor
              key={ch}
              channel={ch}
              value={ch === 1 ? value.alterCh1 : value.alterCh2}
              disabled={locked}
              onChange={(next) => patch(ch === 1 ? { alterCh1: next } : { alterCh2: next })}
            />
          ))}
        </>
      ) : (
        <>
          <Row name="Source">
            <Segmented
              options={SOURCES[value.kind]}
              value={value.source}
              disabled={locked}
              onChange={(source) => patch({ source })}
            />
          </Row>

          <Row name={polarity.title}>
            <Segmented
              options={[
                { id: "positive" as const, label: polarity.positive },
                { id: "negative" as const, label: polarity.negative },
              ]}
              value={value.polarity}
              disabled={locked}
              onChange={(p) => patch({ polarity: p })}
            />
          </Row>

          {hasMode && (
            <Row name="Mode">
              <Segmented
                options={[
                  { id: "auto" as const, label: "Auto" },
                  { id: "normal" as const, label: "Normal" },
                ]}
                value={value.mode}
                disabled={locked}
                onChange={(mode) => patch({ mode })}
              />
            </Row>
          )}

          {hasCoupling && (
            <Row name="Coupling">
              <Segmented
                options={COUPLINGS}
                value={value.coupling}
                disabled={locked}
                onChange={(coupling) => patch({ coupling })}
              />
            </Row>
          )}

          {hasQualifier && (
            <Row name={value.kind === "slope" ? "Slope time is" : "Pulse width is"}>
              <Segmented
                options={QUALIFIERS}
                value={value.qualifier}
                disabled={locked}
                onChange={(qualifier) => patch({ qualifier })}
              />
            </Row>
          )}

          {value.kind === "video" && (
            <>
              <Row name="Standard">
                <Segmented
                  options={[
                    { id: "ntsc" as const, label: "NTSC" },
                    { id: "pal" as const, label: "PAL/SECAM" },
                  ]}
                  value={value.videoStandard}
                  disabled={locked}
                  onChange={(videoStandard) => patch({ videoStandard })}
                />
              </Row>
              <Row name="Sync on">
                <Segmented
                  options={VIDEO_SYNCS}
                  value={value.videoSync}
                  disabled={locked}
                  onChange={(videoSync) => patch({ videoSync })}
                />
              </Row>
            </>
          )}
        </>
      )}

      {value.kind === "slope" && (
        <span className="hint">
          Slope triggers between two thresholds rather than on a level — the level knob does
          nothing in this mode.
        </span>
      )}

      {value.levelApplies && (
        <Row name={value.kind === "alter" ? "CH1 level" : "Level"}>
          <div className="level-row">
            <button
              className="btn"
              disabled={locked}
              onClick={() => patch({ level: value.level - LEVEL_STEP })}
              title="Lower the trigger level"
            >
              −
            </button>
            <span className="level-value">{levelText(value, scaled)}</span>
            <button
              className="btn"
              disabled={locked}
              onClick={() => patch({ level: value.level + LEVEL_STEP })}
              title="Raise the trigger level"
            >
              +
            </button>
            <button
              className="btn"
              disabled={locked}
              onClick={() => patch({ level: 0 })}
              title="Put the level at screen centre"
            >
              0
            </button>
          </div>
        </Row>
      )}

      {/* Knob-only values. Edited here as ordinary settings — Prepare walks the scope's
          multipurpose knob to them, which is the only way the instrument accepts them. */}
      {valuesFor(value).map((id) => {
        const spec = VALUE_CATALOGUE[id];
        const target = value.valueTargets[id] ?? spec.factory;
        const move = (by: number) =>
          patch({ valueTargets: { ...value.valueTargets, [id]: target + by } });
        return (
          <Row key={id} name={spec.label}>
            <div className="level-row">
              <button className="btn" disabled={locked} onClick={() => move(-spec.step)}>
                −
              </button>
              <span className="level-value">{formatValue(target, spec.unit, scaled)}</span>
              <button className="btn" disabled={locked} onClick={() => move(spec.step)}>
                +
              </button>
            </div>
          </Row>
        );
      })}
      {valuesFor(value).length > 0 && (
        <span className="hint">
          The scope cannot be told these — Prepare walks its knob to them, one step at a time.
        </span>
      )}
      {value.kind === "alter" && (
        <span className="hint">
          Only CH1's level is reachable: the panel's level knob moves CH1 whichever channel
          page is open, and CH2's stays put.
        </span>
      )}
      {showsLineNumber(value) && (
        <span className="hint">Line numbers run 1–525 on NTSC and 1–625 on PAL/SECAM.</span>
      )}

      <span className="hint">Applied to the scope by ① Prepare.</span>
    </>
  );
}

/**
 * A knob-only value, in the words the scope uses.
 *
 * Formatted here rather than by the backend because the panel shows *targets* — values the
 * instrument has never held — and the current value and the target have to read the same way.
 */
function formatValue(
  raw: number,
  unit: ValueUnit,
  scale: { perUnit: number; zero: number },
): string {
  switch (unit) {
    // Stored as picoseconds.
    case "time":
      return formatDuration(raw * 1e-12);
    case "count":
      return String(raw);
    // A threshold is in the same 1/25-division units as the trigger level, against the same
    // ground.
    case "level":
      return formatVolts((raw - scale.zero) * scale.perUnit);
  }
}

/** Whether this configuration puts a video line number on screen. */
function showsLineNumber(config: TriggerConfig): boolean {
  if (config.kind === "video") return config.videoSync === "linenumber";
  if (config.kind !== "alter") return false;
  return [config.alterCh1, config.alterCh2].some(
    (c) => c.kind === "video" && c.videoSync === "linenumber",
  );
}

/** One channel's trigger inside Alter mode. */
function AlterEditor({
  channel,
  value,
  disabled,
  onChange,
}: {
  channel: 1 | 2;
  value: AlterChannelConfig;
  disabled: boolean;
  onChange: (next: AlterChannelConfig) => void;
}) {
  const patch = (next: Partial<AlterChannelConfig>) => onChange({ ...value, ...next });
  const polarity = POLARITY[value.kind];
  return (
    <div className="alter-channel">
      <div className={`alter-head ch${channel}`}>
        <span className="sw" />
        CH{channel}
      </div>
      <Row name="Type">
        <Segmented
          options={ALTER_KINDS}
          value={value.kind}
          disabled={disabled}
          onChange={(kind) => patch({ kind })}
        />
      </Row>
      <Row name={polarity.title}>
        <Segmented
          options={[
            { id: "positive" as const, label: polarity.positive },
            { id: "negative" as const, label: polarity.negative },
          ]}
          value={value.polarity}
          disabled={disabled}
          onChange={(p) => patch({ polarity: p })}
        />
      </Row>
      {value.kind === "pulse" && (
        <Row name="Pulse width is">
          <Segmented
            options={QUALIFIERS}
            value={value.qualifier}
            disabled={disabled}
            onChange={(qualifier) => patch({ qualifier })}
          />
        </Row>
      )}
      {value.kind === "video" && (
        <>
          <Row name="Standard">
            <Segmented
              options={[
                { id: "ntsc" as const, label: "NTSC" },
                { id: "pal" as const, label: "PAL/SECAM" },
              ]}
              value={value.videoStandard}
              disabled={disabled}
              onChange={(videoStandard) => patch({ videoStandard })}
            />
          </Row>
          <Row name="Sync on">
            <Segmented
              options={VIDEO_SYNCS}
              value={value.videoSync}
              disabled={disabled}
              onChange={(videoSync) => patch({ videoSync })}
            />
          </Row>
        </>
      )}
      {/* Video's channel page has no Coupling box — the scope offers none for it. */}
      {value.kind !== "video" && (
        <Row name="Coupling">
          <Segmented
            options={COUPLINGS}
            value={value.coupling}
            disabled={disabled}
            onChange={(coupling) => patch({ coupling })}
          />
        </Row>
      )}
    </div>
  );
}

/** A one-line summary for the collapsed section header. */
export function triggerSummary(config: TriggerConfig): string {
  const kind = KINDS.find((k) => k.id === config.kind)?.label ?? config.kind;
  if (config.kind === "alter") {
    const name = (c: AlterChannelConfig) =>
      ALTER_KINDS.find((k) => k.id === c.kind)?.label ?? c.kind;
    return `${kind} · CH1 ${name(config.alterCh1)} · CH2 ${name(config.alterCh2)}`;
  }
  const source = SOURCES[config.kind].find((s) => s.id === config.source)?.label ?? config.source;
  const edge = config.polarity === "positive" ? "↑" : "↓";
  return `${kind} · ${source} ${edge} · ${config.mode === "auto" ? "Auto" : "Normal"}`;
}

/**
 * The level as volts wherever that is meaningful.
 *
 * The conversion is done here rather than read from the scope, because the level being
 * edited has not been applied yet — and by the time it is, Prepare will have set the
 * channel's volts/division itself. The scale that matters is the one Prepare establishes.
 */
function levelText(
  config: TriggerConfig,
  scale: { perUnit: number; zero: number },
): string {
  // EXT, EXT/5 and the mains have no calibrated volts figure on this instrument, so for
  // those the level stays in the divisions the knob actually moves in.
  if (!VOLTAGE_SOURCES.includes(config.source)) {
    return `${(config.level / 25).toFixed(2)} div`;
  }
  return formatVolts((config.level - scale.zero) * scale.perUnit);
}

/** Millivolts as a scope would show them, always signed so a negative level is obvious. */
function formatVolts(millivolts: number): string {
  const sign = millivolts < 0 ? "−" : "";
  const magnitude = Math.abs(millivolts);
  if (magnitude >= 1000) return `${sign}${(magnitude / 1000).toFixed(2)} V`;
  return `${sign}${Math.round(magnitude)} mV`;
}

function Row({ name, children }: { name: string; children: React.ReactNode }) {
  return (
    <div className="field">
      <span className="name">{name}</span>
      {children}
    </div>
  );
}

/** A segmented control over a list of options, generic in the option id. */
function Segmented<T extends string>({
  options,
  value,
  disabled,
  onChange,
}: {
  options: readonly { id: T; label: string }[];
  value: T;
  disabled: boolean;
  onChange: (id: T) => void;
}) {
  return (
    <div className="segmented wrap">
      {options.map((option) => (
        <button
          key={option.id}
          className={value === option.id ? "active" : ""}
          disabled={disabled}
          onClick={() => onChange(option.id)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
