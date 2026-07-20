import { useCallback, useEffect, useState } from "react";
import {
  capture,
  connectScope,
  onProgress,
  prepare,
  scopeStatus,
  type CaptureConfig,
  type CaptureResult,
  type ProgressPayload,
  type ScopeStatus,
} from "./api";
import { ControlPanel } from "./components/ControlPanel";
import { WaveformView } from "./components/WaveformView";

const DEFAULT_CONFIG: CaptureConfig = {
  channels: [1, 2],
  maxFreqHz: 1_000_000,
  samplesPerCycle: 20,
  depth: "40k",
  protocol: "none",
  clockChannel: 1,
  dataChannel: 2,
};

export function App() {
  const [status, setStatus] = useState<ScopeStatus>({ connected: false });
  const [config, setConfig] = useState<CaptureConfig>(DEFAULT_CONFIG);
  const [prepared, setPrepared] = useState(false);
  const [busy, setBusy] = useState<null | "connect" | "prepare" | "capture">(null);
  const [progress, setProgress] = useState<ProgressPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CaptureResult | null>(null);

  // Learn the connection state the backend already established at startup.
  useEffect(() => {
    scopeStatus().then(setStatus).catch(() => {});
  }, []);

  // Changing the configuration invalidates a prior prepare — the scope must be set up
  // again before the next capture reflects the new settings.
  const updateConfig = useCallback((patch: Partial<CaptureConfig>) => {
    setConfig((prev) => ({ ...prev, ...patch }));
    setPrepared(false);
  }, []);

  const doConnect = useCallback(async () => {
    setBusy("connect");
    setError(null);
    try {
      setStatus(await connectScope());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }, []);

  const doPrepare = useCallback(async () => {
    setBusy("prepare");
    setError(null);
    setProgress(null);
    const unlisten = await onProgress("prepare:progress", setProgress);
    try {
      await prepare(config);
      setPrepared(true);
    } catch (e) {
      setError(String(e));
      setPrepared(false);
    } finally {
      unlisten();
      setBusy(null);
    }
  }, [config]);

  const doCapture = useCallback(async () => {
    setBusy("capture");
    setError(null);
    setProgress(null);
    const unlisten = await onProgress("capture:progress", setProgress);
    try {
      setResult(await capture());
    } catch (e) {
      setError(String(e));
    } finally {
      unlisten();
      setBusy(null);
    }
  }, []);

  return (
    <div className="app">
      <div className="brand">
        <div className="logo" />
        <div>
          openmso5202D
          <div className="sub">Hantek MSO5202D</div>
        </div>
      </div>

      <div className="topbar">
        <div className={`conn ${status.connected ? "on" : ""}`}>
          <span className="dot" />
          {status.connected ? status.location ?? "connected" : "not connected"}
        </div>
        {!status.connected && (
          <button className="btn" disabled={busy === "connect"} onClick={doConnect}>
            {busy === "connect" ? "Connecting…" : "Connect"}
          </button>
        )}
        <TopProgress busy={busy} progress={progress} />
        {result && (
          <span className="stat">
            {result.channels.reduce((n, c) => Math.max(n, c.volts.length), 0).toLocaleString()} samples
            {result.decoded.length > 0 && ` · ${result.decoded.filter((d) => d.kind === "byte" || d.kind === "address").length} bytes`}
          </span>
        )}
      </div>

      <ControlPanel
        config={config}
        onChange={updateConfig}
        connected={status.connected}
        prepared={prepared}
        busy={busy}
        error={error}
        onPrepare={doPrepare}
        onCapture={doCapture}
      />

      <div className="main">
        <WaveformView result={result} />
      </div>
    </div>
  );
}

/** Compact progress in the topbar, shown only while a phase is running. */
function TopProgress({
  busy,
  progress,
}: {
  busy: null | "connect" | "prepare" | "capture";
  progress: ProgressPayload | null;
}) {
  if (!busy) return <div className="spacer" />;
  const failed = progress?.state === "failed";
  const pct = Math.round((progress?.fraction ?? 0) * 100);
  const phase = busy === "prepare" ? "Preparing" : busy === "capture" ? "Capturing" : "Connecting";
  return (
    <div className="topbar-progress">
      <span className="label">{progress?.label ?? `${phase}…`}</span>
      <div className="track">
        <div className={`fill ${failed ? "failed" : ""}`} style={{ width: `${failed ? 100 : pct}%` }} />
      </div>
      {progress && (
        <span className="count">
          {progress.index + 1}/{progress.total}
        </span>
      )}
    </div>
  );
}
