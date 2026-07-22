import { useCallback, useEffect, useRef, useState } from "react";
import {
  capture,
  connectScope,
  onProgress,
  prepare,
  redecode,
  scopeStatus,
  type CaptureConfig,
  type CaptureResult,
  type DecodedItem,
  type ProgressPayload,
  type ScopeStatus,
} from "./api";
import { ControlPanel } from "./components/ControlPanel";
import {
  triggerTime,
  WaveformView,
  type Cursor,
  type FocusRequest,
} from "./components/WaveformView";
import { ByteList } from "./components/ByteList";
import { clearConfig, DEFAULT_CONFIG, loadConfig, saveConfig } from "./settings";

export function App() {
  const [status, setStatus] = useState<ScopeStatus>({ connected: false });
  // Restore the last configuration, so the app comes back set up as it was left.
  const [config, setConfig] = useState<CaptureConfig>(loadConfig);
  const [prepared, setPrepared] = useState(false);
  const [busy, setBusy] = useState<null | "connect" | "prepare" | "capture" | "card">(null);
  const [progress, setProgress] = useState<ProgressPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CaptureResult | null>(null);
  // Reported by the plot so the byte list can highlight whatever a cursor sits on.
  const [cursors, setCursors] = useState<Cursor[]>([]);
  // A byte picked from the list; the plot zooms to it and brackets it with cursors.
  const [focus, setFocus] = useState<FocusRequest | null>(null);
  const focusByte = useCallback((item: DecodedItem) => {
    setFocus({
      startS: item.startS,
      endS: item.endS,
      channel: item.channel,
      // A fresh nonce so picking the same byte again re-applies the zoom.
      nonce: performance.now(),
    });
  }, []);

  // Learn the connection state the backend already established at startup.
  useEffect(() => {
    scopeStatus().then(setStatus).catch(() => {});
  }, []);

  // Remember every settings change for the next run.
  useEffect(() => {
    saveConfig(config);
  }, [config]);

  /** Put every setting back to its default and forget the stored one. */
  const resetSettings = useCallback(() => {
    setConfig({ ...DEFAULT_CONFIG });
    clearConfig();
    // The scope is still set up for the old configuration, so it must be prepared again.
    setPrepared(false);
  }, []);

  // Card work streams its own progress. Subscribed for the app's lifetime rather than per
  // operation, because a card job can start from the panel without going through here.
  useEffect(() => {
    const pending = onProgress("card:progress", setProgress);
    return () => {
      pending.then((unlisten) => unlisten());
    };
  }, []);

  // Stable identities: CardFiles keys effects off these, so fresh closures each render
  // would re-fire its auto-listing continuously (and stomp `busy` back to null mid-run).
  const setCardBusy = useCallback((on: boolean) => {
    setBusy((prev) => (on ? "card" : prev === "card" ? null : prev));
    if (on) setProgress(null);
  }, []);

  // Changing an ACQUISITION setting invalidates a prior prepare — the scope must be set up
  // again before the next capture reflects it. Decode-only settings (protocol, line
  // assignment) change nothing on the instrument, so they must NOT force a re-prepare;
  // they just re-annotate what is already on screen.
  const updateConfig = useCallback((patch: Partial<CaptureConfig>) => {
    setConfig((prev) => ({ ...prev, ...patch }));
    const decodeOnly = Object.keys(patch).every((key) =>
      (["protocol", "clockChannel", "dataChannel"] as string[]).includes(key),
    );
    if (!decodeOnly) setPrepared(false);
  }, []);

  // --- live decode ---------------------------------------------------------
  // Every decode-affecting setting re-runs the decoder against the traces already loaded,
  // so the annotation follows the controls immediately instead of waiting for a capture.
  // `maxFreqHz` is in here because it feeds the UART decoder's baud hint.
  const decodeKey = [
    config.protocol,
    config.clockChannel,
    config.dataChannel,
    config.maxFreqHz,
  ].join("|");
  const decodeKeyRef = useRef(decodeKey);
  decodeKeyRef.current = decodeKey;
  // What the annotation on screen was produced with, so a fresh capture (already decoded by
  // the backend) does not trigger a redundant second decode.
  const decodedWith = useRef<string | null>(null);

  const applyResult = useCallback((next: CaptureResult) => {
    setResult(next);
    decodedWith.current = decodeKeyRef.current;
  }, []);

  useEffect(() => {
    if (!result || decodedWith.current === decodeKey) return;
    decodedWith.current = decodeKey;
    let cancelled = false;
    redecode(config)
      .then((decoded) => {
        if (!cancelled) setResult((prev) => (prev ? { ...prev, decoded } : prev));
      })
      .catch((e) => setError(String(e)));
    return () => {
      cancelled = true;
    };
    // `config` is read whole but only the decode fields should re-trigger this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [decodeKey, result]);

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
      applyResult(await capture());
    } catch (e) {
      setError(String(e));
    } finally {
      unlisten();
      setBusy(null);
    }
  }, [applyResult]);

  return (
    <div className="app">
      <div className="brand">
        <div className="logo" />
        <div className="ident">
          <div className="name">openmso5202D</div>
          {/* The connection state lives with the identity rather than out in the topbar —
              it answers "what am I talking to", which is the same question as the title. */}
          <div className={`conn ${status.connected ? "on" : ""}`}>
            <span className="dot" />
            <span className="where">
              {status.connected ? status.location ?? "connected" : "not connected"}
            </span>
          </div>
        </div>
        {!status.connected && (
          <button
            className="btn primary connect"
            disabled={busy === "connect"}
            onClick={doConnect}
          >
            {busy === "connect" ? "…" : "Connect"}
          </button>
        )}
      </div>

      <div className="topbar">
        {error ? (
          <div className="topbar-error" role="alert" title={error}>
            <span className="icon">!</span>
            <span className="msg">{error}</span>
            <button className="dismiss" onClick={() => setError(null)} title="Dismiss">
              ×
            </button>
          </div>
        ) : (
          <TopProgress busy={busy} progress={progress} />
        )}
        {result && (
          <span className="stat">
            {result.channels.reduce((n, c) => Math.max(n, c.volts.length), 0).toLocaleString()} samples
            {result.decoded.length > 0 && ` · ${result.decoded.filter((d) => d.kind === "byte" || d.kind === "address").length} bytes`}
          </span>
        )}
        <button
          className="btn subtle"
          disabled={busy !== null}
          onClick={resetSettings}
          title="Reset all settings to their defaults"
        >
          Reset settings
        </button>
      </div>

      <ControlPanel
        config={config}
        onChange={updateConfig}
        connected={status.connected}
        prepared={prepared}
        busy={busy}
        onPrepare={doPrepare}
        onCapture={doCapture}
        onCardBusy={setCardBusy}
        onResult={applyResult}
      />

      <div className="main">
        <div className="plot-area">
          <WaveformView result={result} onCursors={setCursors} focus={focus} />
        </div>
        {result && (
          <ByteList
            decoded={result.decoded}
            cursors={cursors}
            triggerS={triggerTime(result)}
            onSelect={focusByte}
          />
        )}
      </div>
    </div>
  );
}

/** Compact progress in the topbar, shown only while a phase is running. */
function TopProgress({
  busy,
  progress,
}: {
  busy: null | "connect" | "prepare" | "capture" | "card";
  progress: ProgressPayload | null;
}) {
  if (!busy) return <div className="spacer" />;
  const failed = progress?.state === "failed";
  const pct = Math.round((progress?.fraction ?? 0) * 100);
  const phase =
    busy === "prepare"
      ? "Preparing"
      : busy === "capture"
        ? "Capturing"
        : busy === "card"
          ? "Working on card"
          : "Connecting";
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
