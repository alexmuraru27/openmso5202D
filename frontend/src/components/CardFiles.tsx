import { useCallback, useEffect, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { clearCardFiles, downloadCardFiles, listCardFiles, type CardFile } from "../api";

interface Props {
  connected: boolean;
  /** True while any other long operation owns the scope — the card shares the link. */
  busy: boolean;
  onBusyChange: (busy: boolean) => void;
  /** The current listing, lifted so the Load-CSV panel can offer the same files. */
  onFilesChange: (files: CardFile[]) => void;
}

/** Bytes as the card would have you read them: `78 KB`, `7.4 MB`. */
function formatBytes(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} KB`;
  return `${n} B`;
}

/**
 * The scope's memory card: list the exported CSVs, pull the selected ones onto this machine,
 * or wipe the card.
 *
 * Plotting lives in the Load-CSV panel instead, because that needs an explicit channel
 * mapping — nothing in a CSV says which channel it came from.
 */
export function CardFiles({ connected, busy, onBusyChange, onFilesChange }: Props) {
  const [files, setFiles] = useState<CardFile[] | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const run = useCallback(
    async (job: () => Promise<void>) => {
      setWorking(true);
      onBusyChange(true);
      setError(null);
      try {
        await job();
      } catch (e) {
        setError(String(e));
      } finally {
        setWorking(false);
        onBusyChange(false);
      }
    },
    [onBusyChange],
  );

  const refresh = useCallback(
    () =>
      run(async () => {
        const list = await listCardFiles();
        setFiles(list);
        onFilesChange(list);
        setSelected((prev) => prev.filter((n) => list.some((f) => f.name === n)));
        setStatus(`${list.length} file(s) on card`);
      }),
    [run, onFilesChange],
  );

  // List once per connection. Guarded by a ref rather than `files === null`: the scope's
  // shell listing is flaky, and keying off the result would re-fire the effect on every
  // render after a failure — an endless refresh loop that also reset the app's busy state.
  const listed = useRef(false);
  useEffect(() => {
    if (!connected) {
      listed.current = false;
      return;
    }
    if (!listed.current) {
      listed.current = true;
      refresh();
    }
  }, [connected, refresh]);

  const toggle = (name: string) =>
    setSelected((prev) =>
      prev.includes(name) ? prev.filter((n) => n !== name) : [...prev, name],
    );

  /** Ask where to put the file(s), then fetch them there. */
  const download = () =>
    run(async () => {
      let dest: string | null = null;
      if (selected.length === 1) {
        dest = await save({
          title: "Save waveform CSV",
          defaultPath: selected[0],
          filters: [{ name: "CSV", extensions: ["csv"] }],
        });
      } else {
        dest = await open({ title: "Save into folder", directory: true, multiple: false });
      }
      if (!dest) return; // cancelled
      const saved = await downloadCardFiles(selected, dest);
      setStatus(`Saved ${saved.map((f) => f.path).join(", ")}`);
    });

  const clearAll = () =>
    run(async () => {
      if (!window.confirm("Delete every WaveData CSV on the card? This cannot be undone.")) {
        return;
      }
      await clearCardFiles();
      setSelected([]);
      const list = await listCardFiles();
      setFiles(list);
      onFilesChange(list);
      setStatus("Card cleared");
    });

  const blocked = !connected || busy || working;

  return (
    <div className="group">
      <div className="label">SD card</div>

      <div className="card-list">
        {files === null ? (
          <div className="card-empty">{connected ? "…" : "connect to list files"}</div>
        ) : files.length === 0 ? (
          <div className="card-empty">no exported CSVs</div>
        ) : (
          files.map((file) => (
            <div
              key={file.name}
              className={`card-row ${selected.includes(file.name) ? "on" : ""}`}
              onClick={() => toggle(file.name)}
              title={file.name}
            >
              <span className="nm">{file.name}</span>
              <span className="sz">{formatBytes(file.size)}</span>
            </div>
          ))
        )}
      </div>

      <div className="card-actions">
        <button className="btn" disabled={blocked} onClick={refresh}>
          Refresh
        </button>
        <button
          className="btn"
          disabled={blocked || selected.length === 0}
          onClick={download}
          title="Choose where to save the selected file(s)"
        >
          Download{selected.length > 1 ? ` (${selected.length})` : ""}
        </button>
        <button
          className="btn danger"
          disabled={blocked || (files?.length ?? 0) === 0}
          onClick={clearAll}
          title="Delete every WaveData CSV on the card — irreversible"
        >
          Clear all
        </button>
      </div>

      {status && !error && <span className="hint">{status}</span>}
      {error && (
        <span className="hint" style={{ color: "var(--danger)" }}>
          {error}
        </span>
      )}
    </div>
  );
}
