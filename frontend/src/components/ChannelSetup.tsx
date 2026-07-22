import type { CaptureConfig, ChannelSetupConfig } from "../api";

interface Props {
  config: CaptureConfig;
  onChange: (patch: Partial<CaptureConfig>) => void;
  disabled: boolean;
}

const COUPLINGS: { id: ChannelSetupConfig["coupling"]; label: string }[] = [
  { id: "dc", label: "DC" },
  { id: "ac", label: "AC" },
  { id: "gnd", label: "GND" },
];

const PROBES: { id: ChannelSetupConfig["probe"]; label: string; factor: number }[] = [
  { id: "1x", label: "1×", factor: 1 },
  { id: "10x", label: "10×", factor: 10 },
  { id: "100x", label: "100×", factor: 100 },
  { id: "1000x", label: "1000×", factor: 1000 },
];

/** The attenuation a probe setting stands for. */
export function probeFactor(probe: ChannelSetupConfig["probe"]): number {
  return PROBES.find((p) => p.id === probe)?.factor ?? 1;
}

/** What a channel starts as — and what a decoder wants. */
export const DEFAULT_CHANNEL_SETUP: ChannelSetupConfig = {
  probe: "1x",
  coupling: "dc",
  bandwidthLimited: false,
  inverted: false,
};

/**
 * Per-channel vertical options: coupling, the 20 MHz bandwidth limit, and invert.
 *
 * Applied by Prepare, from the channel's own menu on the instrument. Two of these will
 * quietly ruin a decode, so each carries a warning rather than being offered flat: AC
 * coupling shifts a logic signal off its baseline, so a threshold placed for a 0–3.3 V
 * swing no longer sits in the middle of it; inverting turns every bit into its opposite.
 */
export function ChannelSetup({ config, onChange, disabled }: Props) {
  const setupFor = (channel: number): ChannelSetupConfig =>
    config.channelsSetup[channel - 1] ?? DEFAULT_CHANNEL_SETUP;

  const patch = (channel: number, next: Partial<ChannelSetupConfig>) => {
    const all = [1, 2].map((ch) =>
      ch === channel ? { ...setupFor(ch), ...next } : setupFor(ch),
    );
    onChange({ channelsSetup: all });
  };

  return (
    <>
      {[1, 2].map((channel) => {
        const setup = setupFor(channel);
        const on = config.channels.includes(channel);
        return (
          <div className="alter-channel" key={channel}>
            <div className={`alter-head ch${channel}`}>
              <span className="sw" />
              CH{channel}
              {!on && <span className="off-note">off</span>}
            </div>

            <div className="field">
              <span className="name">Probe</span>
              <div className="segmented wrap">
                {PROBES.map((option) => (
                  <button
                    key={option.id}
                    className={setup.probe === option.id ? "active" : ""}
                    disabled={disabled}
                    onClick={() => patch(channel, { probe: option.id })}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="field">
              <span className="name">Coupling</span>
              <div className="segmented wrap">
                {COUPLINGS.map((option) => (
                  <button
                    key={option.id}
                    className={setup.coupling === option.id ? "active" : ""}
                    disabled={disabled}
                    onClick={() => patch(channel, { coupling: option.id })}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="field">
              <span className="name">Bandwidth</span>
              <div className="segmented wrap">
                <button
                  className={!setup.bandwidthLimited ? "active" : ""}
                  disabled={disabled}
                  onClick={() => patch(channel, { bandwidthLimited: false })}
                >
                  Full
                </button>
                <button
                  className={setup.bandwidthLimited ? "active" : ""}
                  disabled={disabled}
                  onClick={() => patch(channel, { bandwidthLimited: true })}
                >
                  20 MHz
                </button>
              </div>
            </div>

            <div className="field">
              <span className="name">Invert</span>
              <div className="segmented wrap">
                <button
                  className={!setup.inverted ? "active" : ""}
                  disabled={disabled}
                  onClick={() => patch(channel, { inverted: false })}
                >
                  Off
                </button>
                <button
                  className={setup.inverted ? "active" : ""}
                  disabled={disabled}
                  onClick={() => patch(channel, { inverted: true })}
                >
                  On
                </button>
              </div>
            </div>

            {setup.coupling === "ac" && (
              <span className="hint" style={{ color: "var(--warn)" }}>
                AC coupling shifts a logic signal off its baseline — the decoder's threshold
                will no longer sit in the middle of the swing.
              </span>
            )}
            {setup.inverted && (
              <span className="hint" style={{ color: "var(--warn)" }}>
                Inverted — every decoded bit will come out the other way round.
              </span>
            )}
            {setup.coupling === "gnd" && (
              <span className="hint" style={{ color: "var(--warn)" }}>
                Grounded — this channel shows no signal at all.
              </span>
            )}
          </div>
        );
      })}
      <span className="hint">Applied to the scope by ① Prepare.</span>
    </>
  );
}

/** A one-line summary for the collapsed section header. */
export function channelSummary(config: CaptureConfig): string {
  return [1, 2]
    .map((channel) => {
      const setup = config.channelsSetup[channel - 1] ?? DEFAULT_CHANNEL_SETUP;
      const marks = [
        setup.probe.replace("x", "×"),
        setup.coupling.toUpperCase(),
        setup.bandwidthLimited ? "20M" : null,
        setup.inverted ? "inv" : null,
      ].filter(Boolean);
      return `CH${channel} ${marks.join(" ")}`;
    })
    .join(" · ");
}
