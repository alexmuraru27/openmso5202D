import { useCallback, useEffect, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  clearCardFiles,
  downloadCardFiles,
  listCardFiles,
  loadCsvs,
  type CaptureConfig,
  type CaptureResult,
  type CardFile,
  type CsvSlot,
} from "../api";
import { Modal } from "./Modal";

interface Props {
  /** Rendered only when open — but kept mounted, so a listing survives closing the dialog. */
  open: boolean;
  onClose: () => void;
  connected: boolean;
  /** True while another long operation owns the scope; the card shares the USB link. */
  busy: boolean;
  config: CaptureConfig;
  onBusyChange: (busy: boolean) => void;
  onResult: (result: CaptureResult) => void;
}

/** The channels a plot can hold. */
const SLOTS = [1, 2] as const;

/** A file that can be plotted, wherever it came from. */
interface Entry {
  source: "card" | "local";
  /** Filename on the card, or full path on this machine. */
  value: string;
  size?: number;
}

/** Bytes as the card would have you read them: `78 KB`, `7.4 MB`. */
function formatBytes(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)} MB`;
  if (n >= 1e3) return `${Math.round(n / 1e3)} KB`;
  return `${n} B`;
}

/** Last path component, for a readable label. */
function basename(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

/**
 * The waveform library: everything that can be plotted, from the scope's card or this machine.
 *
 * Card management and CSV loading used to be two separate panels, which made the common job —
 * "capture, then look at it" — a trip through both. They are one list here, because the
 * question being answered is the same either way: *which trace goes on which channel*. A CSV
 * holds one channel and carries nothing saying which, so the mapping is stated rather than
 * guessed; assigning the clock file to one channel and the data file to the other is what
 * makes a saved SPI/I²C capture decodable after the fact, exactly like a live one.
 *
 * Local files need no scope, so downloaded captures can be reviewed with the instrument
 * unplugged.
 */
export function FilesDialog(props: Props) {
  const { open: isOpen, onClose, connected, busy, config, onBusyChange, onResult } = props;

  const [cardFiles, setCardFiles] = useState<CardFile[] | null>(null);
  const [localFiles, setLocalFiles] = useState<string[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [assigned, setAssigned] = useState<Record<number, Entry | undefined>>({});
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
        setCardFiles(list);
        setSelected((prev) => prev.filter((n) => list.some((f) => f.name === n)));
        setStatus(`${list.length} file(s) on card`);
      }),
    [run],
  );

  // List when the dialog is first opened on a connected scope, not on connect: the listing
  // runs a shell command over the USB link, and there is no reason to spend that until
  // someone actually looks. Guarded by a ref because the scope's listing is flaky — keying
  // off the result would re-fire on every render after a failure.
  const listed = useRef(false);
  useEffect(() => {
    if (!connected) {
      listed.current = false;
      return;
    }
    if (isOpen && !listed.current) {
      listed.current = true;
      refresh();
    }
  }, [isOpen, connected, refresh]);

  const toggleSelected = (name: string) =>
    setSelected((prev) =>
      prev.includes(name) ? prev.filter((n) => n !== name) : [...prev, name],
    );

  /** Put a file on a channel, or take it off if it is already there. */
  const assign = (channel: number, entry: Entry) =>
    setAssigned((prev) => {
      const next = { ...prev };
      const already = prev[channel];
      if (already?.value === entry.value && already.source === entry.source) {
        next[channel] = undefined;
        return next;
      }
      next[channel] = entry;
      // The same file on both channels would just plot one trace twice.
      for (const other of SLOTS) {
        if (other !== channel && next[other]?.value === entry.value) next[other] = undefined;
      }
      return next;
    });

  const assignedTo = (entry: Entry): number | undefined =>
    SLOTS.find((ch) => assigned[ch]?.value === entry.value && assigned[ch]?.source === entry.source);

  const addLocal = async () => {
    const picked = await open({
      title: "Add waveform CSVs",
      multiple: true,
      directory: false,
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    const paths = Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
    if (paths.length) setLocalFiles((prev) => [...new Set([...prev, ...paths])]);
  };

  /** Ask where to put the selected card file(s), then fetch them there. */
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
      // Downloaded files join the library, so they can be plotted without the scope later.
      setLocalFiles((prev) => [...new Set([...prev, ...saved.map((f) => f.path)])]);
      setStatus(`Saved ${saved.length} file(s) to ${saved[0]?.path ?? dest}`);
    });

  const clearAll = () =>
    run(async () => {
      if (!window.confirm("Delete every WaveData CSV on the card? This cannot be undone.")) {
        return;
      }
      await clearCardFiles();
      setSelected([]);
      setAssigned((prev) =>
        Object.fromEntries(
          SLOTS.map((ch) => [ch, prev[ch]?.source === "card" ? undefined : prev[ch]]),
        ),
      );
      setCardFiles(await listCardFiles());
      setStatus("Card cleared");
    });

  const chosen: CsvSlot[] = SLOTS.filter((ch) => assigned[ch]).map((ch) => ({
    channel: ch,
    source: assigned[ch]!.source,
    value: assigned[ch]!.value,
  }));

  const plot = () =>
    run(async () => {
      if (chosen.length === 0) return;
      onResult(await loadCsvs(chosen, config));
      onClose();
    });

  if (!isOpen) return null;

  const locked = busy || working;
  const cardLocked = locked || !connected;
  // Only a card file needs the instrument; a disk-only load works offline.
  const needsScope = chosen.some((slot) => slot.source === "card");

  return (
    <Modal
      title="Waveform files"
      subtitle="Pick traces to plot, or manage the scope's memory card"
      onClose={onClose}
      busy={working}
      footer={
        <>
          <div className="assign-summary">
            {SLOTS.map((ch) => (
              <span key={ch} className={`slot ch${ch} ${assigned[ch] ? "on" : ""}`}>
                <span className="sw" />
                CH{ch}
                <span className="val" title={assigned[ch]?.value}>
                  {assigned[ch] ? basename(assigned[ch]!.value) : "—"}
                </span>
              </span>
            ))}
          </div>
          <button
            className="btn primary lg"
            disabled={locked || chosen.length === 0 || (needsScope && !connected)}
            onClick={plot}
          >
            {working ? "Loading…" : "Plot traces"}
          </button>
        </>
      }
    >
      <div className="file-section">
        <div className="file-head">
          <span className="label">Scope card</span>
          <div className="tools">
            <button className="btn" disabled={cardLocked} onClick={refresh}>
              Refresh
            </button>
            <button
              className="btn"
              disabled={cardLocked || selected.length === 0}
              onClick={download}
              title="Choose where to save the ticked file(s)"
            >
              Download{selected.length > 0 ? ` (${selected.length})` : ""}
            </button>
            <button
              className="btn danger"
              disabled={cardLocked || (cardFiles?.length ?? 0) === 0}
              onClick={clearAll}
              title="Delete every WaveData CSV on the card — irreversible"
            >
              Clear all
            </button>
          </div>
        </div>

        <div className="file-list">
          {!connected ? (
            <div className="file-empty">Connect the scope to list its card</div>
          ) : cardFiles === null ? (
            <div className="file-empty">Reading card…</div>
          ) : cardFiles.length === 0 ? (
            <div className="file-empty">No exported CSVs — arm a capture to make one</div>
          ) : (
            cardFiles.map((file) => {
              const entry: Entry = { source: "card", value: file.name, size: file.size };
              return (
                <div key={file.name} className="file-row">
                  <label className="tick" title="Select for download">
                    <input
                      type="checkbox"
                      checked={selected.includes(file.name)}
                      disabled={cardLocked}
                      onChange={() => toggleSelected(file.name)}
                    />
                  </label>
                  <span className="nm" title={file.name}>
                    {file.name}
                  </span>
                  <span className="sz">{formatBytes(file.size)}</span>
                  <AssignButtons entry={entry} on={assignedTo(entry)} onAssign={assign} />
                </div>
              );
            })
          )}
        </div>
      </div>

      <div className="file-section">
        <div className="file-head">
          <span className="label">This computer</span>
          <div className="tools">
            <button className="btn" disabled={locked} onClick={addLocal}>
              Add files…
            </button>
          </div>
        </div>

        <div className="file-list">
          {localFiles.length === 0 ? (
            <div className="file-empty">
              Nothing added — downloaded files land here automatically
            </div>
          ) : (
            localFiles.map((path) => {
              const entry: Entry = { source: "local", value: path };
              return (
                <div key={path} className="file-row">
                  <span className="tick" />
                  <span className="nm" title={path}>
                    {basename(path)}
                  </span>
                  <button
                    className="drop"
                    title="Remove from the list"
                    onClick={() => {
                      setLocalFiles((prev) => prev.filter((p) => p !== path));
                      setAssigned((prev) =>
                        Object.fromEntries(
                          SLOTS.map((ch) => [ch, prev[ch]?.value === path ? undefined : prev[ch]]),
                        ),
                      );
                    }}
                  >
                    ×
                  </button>
                  <AssignButtons entry={entry} on={assignedTo(entry)} onAssign={assign} />
                </div>
              );
            })
          )}
        </div>
      </div>

      {needsScope && !connected && (
        <span className="hint" style={{ color: "var(--warn)" }}>
          A card file needs the scope connected — or plot files from this computer.
        </span>
      )}
      {error ? (
        <span className="hint" style={{ color: "var(--danger)" }}>
          {error}
        </span>
      ) : (
        status && <span className="hint">{status}</span>
      )}
    </Modal>
  );
}

/** The CH1/CH2 pair that puts a file on a channel. */
function AssignButtons({
  entry,
  on,
  onAssign,
}: {
  entry: Entry;
  on: number | undefined;
  onAssign: (channel: number, entry: Entry) => void;
}) {
  return (
    <div className="assign">
      {SLOTS.map((ch) => (
        <button
          key={ch}
          className={`ch${ch} ${on === ch ? "on" : ""}`}
          onClick={() => onAssign(ch, entry)}
          title={on === ch ? `Take off CH${ch}` : `Plot as CH${ch}`}
        >
          {ch}
        </button>
      ))}
    </div>
  );
}
