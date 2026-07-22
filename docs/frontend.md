# Frontend developer guide

`frontend/` is the openmso5202D desktop app: a [Tauri 2](https://tauri.app) shell wrapping a
React 18 + TypeScript webview, built by Vite. The Rust side (`frontend/src-tauri`) depends on
the `mso5202d` driver crate in `backend/` **directly**, so launching the app launches the
backend in-process — there is no separate server, socket or CLI. The webview never touches USB;
it calls Tauri commands, which drive the driver's control layer and hand back plain data
(volts, decoded events) to plot. Everything the app displays comes from one of two flows:
*prepare → capture* against a live scope, or *load CSVs* from the scope's card or from disk.

## File map

| Path | What it is |
| --- | --- |
| `frontend/index.html` | Vite entry document; mounts `#root`. |
| `frontend/vite.config.ts` | React plugin, fixed dev port 1420, build target `es2021`, outDir `dist`. |
| `frontend/package.json` | Workspace package `openmso5202d-frontend`; scripts `dev`/`build`/`preview`/`tauri`. |
| `frontend/tsconfig.json` | TypeScript config used by `tsc --noEmit` in the build script. |
| `frontend/src/main.tsx` | `createRoot` + `<React.StrictMode><App/></React.StrictMode>`, imports `theme.css`. |
| `frontend/src/App.tsx` | Root component: all app state, the connect/prepare/capture flows, layout. |
| `frontend/src/api.ts` | Typed wrappers over every Tauri command + the shared serde-mirroring types. |
| `frontend/src/settings.ts` | localStorage persistence for capture config, trigger and panel-open state. |
| `frontend/src/timebase.ts` | Capture planning maths: SEC/DIV ladder, `capturePlan`, `formatDuration`. |
| `frontend/src/theme.ts` | Canvas colour constants + `channelColor`, kept in sync with `theme.css`. |
| `frontend/src/theme.css` | The whole stylesheet: CSS custom properties + every component's rules. |
| `frontend/src/components/ControlPanel.tsx` | The sidebar: acquisition, channel setup, trigger, decode, action buttons. |
| `frontend/src/components/ChannelSetup.tsx` | Per-channel probe/coupling/bandwidth/invert editor. |
| `frontend/src/components/TriggerPanel.tsx` | Trigger editor (all six types, Alter sub-editors, knob-only values). |
| `frontend/src/components/WaveformView.tsx` | The canvas plot: lanes, zoom/pan, cursors, decode pills, byte slices. |
| `frontend/src/components/ByteList.tsx` | The decoded bytes as a hex/dec/ASCII list. |
| `frontend/src/components/FilesDialog.tsx` | Waveform library: card listing, download, clear, local files, channel assignment. |
| `frontend/src/components/Modal.tsx` | Generic dialog shell (backdrop, Escape, focus). |
| `frontend/src/components/Section.tsx` | Collapsible sidebar section with a collapsed summary. |
| `frontend/src-tauri/src/main.rs` | Binary entry point; calls `openmso5202d_lib::run()`. |
| `frontend/src-tauri/src/lib.rs` | Tauri builder: logging, dialog plugin, `AppState`, `invoke_handler`. |
| `frontend/src-tauri/src/api.rs` | The command surface, serde DTOs, progress sink, plan construction, decode. |
| `frontend/src-tauri/tauri.conf.json` | Product/window/bundle config and the before-dev/build hooks. |
| `frontend/src-tauri/Cargo.toml` | Crate `openmso5202d` (lib `openmso5202d_lib`); depends on `mso5202d` by path. |
| `frontend/src-tauri/capabilities/default.json` | Permissions for the `main` window: `core:default`, `dialog:default`. |
| `frontend/src-tauri/tests/prepare_applies_trigger.rs` | Hardware test starting from the verbatim JSON the webview sends. |

## Architecture

Three pieces, one process:

1. **React webview** (`frontend/src`) — all UI state and rendering. Talks to the shell only
   through `@tauri-apps/api`'s `invoke` and `listen`, wrapped in `src/api.ts`.
2. **Tauri shell** (`frontend/src-tauri/src/lib.rs`) — deliberately thin. It initialises file
   logging (`mso5202d::logging::init()`, kept alive for the session), registers the dialog
   plugin, `manage`s `api::AppState`, and lists the commands in `tauri::generate_handler!`.
3. **Backend-API layer** (`frontend/src-tauri/src/api.rs`) — the seam. It owns the serde DTOs
   the webview speaks, translates a `CaptureConfig`/`TriggerConfig` into the driver's
   `CaptureSpec`/`TriggerSetup`, runs `mso5202d::control::capture::{prepare, capture}`, parses
   the exported CSVs (`mso5202d::waveform::parse_csv`), runs the decoders
   (`mso5202d::decoder::{uart, spi, i2c}`) and returns plottable data.

`AppState` holds three mutexes:

| Field | Contents |
| --- | --- |
| `device` | `Option<Device>` — the open USB device, set by `connect_scope`. |
| `prepared` | `Option<CaptureConfig>` — the config the last successful `prepare` used; `capture` reads it. |
| `traces` | `Option<LoadedTraces>` — the parsed per-channel records behind the current plot, plus `sample_interval_s`, retained so `redecode` can re-annotate without re-capturing or shipping samples back over IPC. |

Every long-running command is `async` so Tauri runs it on a worker thread — blocking USB work
on the UI thread freezes the event loop (and a Wayland compositor kills an app that stops
answering pings).

Enum values cross the boundary as **strings** (`"edge"`, `"1x"`, `"512k"`), never as the
scope's numeric codes; `api.rs` maps them to driver enums with a defaulting `match`, so an
unknown string falls back rather than erroring — except the trigger `kind`/`source`, which
return `Err`.

## Command / IPC surface

All commands are registered in `lib.rs`. Errors are `String` and surface in the UI as the
topbar error banner (or the dialog's own error line).

| Command | Args | Returns | Does |
| --- | --- | --- | --- |
| `scope_status` | — | `ScopeStatus` | Reports whether a device is held and its `bus N address M`. Does not connect. |
| `connect_scope` | — | `ScopeStatus` | `Device::connect_without_reset()` (no USB reset, so the SD card stays available), `transport().resync()`, stores the device. |
| `prepare` | `config: CaptureConfig`, `trigger: Option<TriggerConfig>` | `()` | Validates the config, builds a `CaptureSpec` (channels, depth, timebase, per-channel setup), attaches the trigger setup and the knob-value targets, runs `control::capture::prepare`. Stores the config in `prepared`. Streams `prepare:progress`. |
| `capture` | — | `CaptureResult` | Requires a prior `prepare`. Runs `control::capture::capture` (arm single sequence, wait/force, export per channel to the card, read back), parses the outputs, decodes, retains the traces. Streams `capture:progress`. |
| `list_card_files` | — | `Vec<CardFile>` | `clear_link()`, `list_dir(control::CARD_PATH)`, filtered to `WaveData*.csv` by `control::csv::wavedata_files`. |
| `download_card_files` | `names: Vec<String>`, `dest: Option<String>` | `Vec<DownloadedFile>` | Copies card files to the host. One name + `dest` ⇒ that exact path; otherwise `dest` is a directory; no `dest` ⇒ `$HOME/openmso5202D/`. Streams `card:progress`. |
| `load_csvs` | `slots: Vec<CsvSlot>`, `config: CaptureConfig` | `CaptureResult` | Reads each slot (`local` from disk, `card` over USB), parses, assigns to the stated channel, decodes, retains. Takes the device lock only if a `card` slot is present, so local files work unplugged. Streams `card:progress` for card slots. |
| `redecode` | `config: CaptureConfig` | `Vec<DecodedItem>` | Re-runs the decoder over the retained traces. No instrument, no transfer. |
| `clear_card_files` | — | `()` | `control::execute(&context, &[Op::ClearCard])` — deletes every `WaveData*.csv` via the front-panel delete key. Irreversible. Streams `card:progress`. |

### TypeScript wrappers (`src/api.ts`)

One exported function per command, same order:
`scopeStatus`, `connectScope`, `prepare(config, trigger)`, `capture()`, `listCardFiles`,
`downloadCardFiles(names, dest?)` (passes `dest ?? null`), `loadCsvs(slots, config)`,
`redecode(config)`, `clearCardFiles`. Plus `onProgress(event, handler)`.

`api.ts` also exports `levelScale(): Promise<LevelScale>`, which invokes `level_scale` — a
command that is **not** defined in `api.rs` nor registered in `lib.rs`, so the call always
rejects. Its only caller, `TriggerPanel`, swallows the rejection and keeps
`DEFAULT_LEVEL_SCALE`, which is the same 1 V/div ÷ 25 figure. Similarly, `onProgress`'s event
union accepts `"trigger:progress"`, which nothing emits.

The exchanged shapes mirror the serde types (`#[serde(rename_all = "camelCase")]`):
`CaptureConfig`, `ChannelSetupConfig`, `ScopeStatus`, `ChannelData`, `DecodedItem`,
`CaptureResult`, `ProgressPayload`, `CardFile`, `DownloadedFile`, `CsvSlot`, `TriggerConfig`,
`AlterChannelConfig`, plus the string unions `Protocol`, `Depth`, `TriggerKind`, `AlterKind`,
`TriggerSource`.

Note the asymmetric trigger fields: `levelMv`, `levelMvPerUnit`, `levelZero`, `levelApplies`
and the Rust-only `values: Vec<TriggerValue>` are `#[serde(skip_deserializing)]` — they are
report-only and ignored on the way in.

### Progress events

`ProgressSink` implementor `EmitProgress { app, event_name }` forwards each driver
`ProgressEvent` to the webview. Payload (`ProgressPayload`, camelCase):

| Field | Type | Meaning |
| --- | --- | --- |
| `index` | `usize` | Zero-based step index. |
| `total` | `usize` | Number of steps in the plan. |
| `label` | `String` | The step's name. |
| `state` | `"started" \| "advanced" \| "completed" \| "failed"` | From `control::StepState`. |
| `fraction` | `f32` | `(index + within) / total`, where `within` is the in-step sub-progress (0 for started, `done/total` for advanced, 1 for completed/failed). |
| `detail` | `Option<String>` | `"done/total"`, `"NNN ms"`, or the error text. |

Channels: `prepare:progress` (from `prepare`), `capture:progress` (from `capture`),
`card:progress` (from `download_card_files`, `load_csvs` and `clear_card_files`). Card progress
events for downloads are emitted directly (not via `EmitProgress`) with `state: "advanced"` and
a byte-count `detail`.

Subscriptions in the UI: `App` listens to `card:progress` for its whole lifetime (a card job can
start from the dialog without going through `App`'s handlers) and subscribes to
`prepare:progress` / `capture:progress` only for the duration of the run — inside the `try`, so
a failed subscription still clears `busy`.

## State model (`App.tsx`)

| State | Type | Notes |
| --- | --- | --- |
| `status` | `ScopeStatus` | Seeded from `scopeStatus()` on mount; replaced by `connectScope()`. |
| `config` | `CaptureConfig` | Initialised from `loadConfig()`; saved on every change. |
| `trigger` | `TriggerConfig` | Initialised from `loadTrigger()`; saved on every change. |
| `prepared` | `boolean` | Whether the scope is currently set up for `config` + `trigger`. Gates the Capture button. |
| `busy` | `null \| "connect" \| "prepare" \| "capture" \| "card"` | One operation at a time; drives disabled states and the topbar phase label. |
| `progress` | `ProgressPayload \| null` | Latest event from whichever channel is active. |
| `error` | `string \| null` | Shown in the topbar in place of the progress bar, dismissible. |
| `result` | `CaptureResult \| null` | The traces + decode currently plotted. |
| `cursors` | `Cursor[]` | Reported up from `WaveformView` via `onCursors`. |
| `focus` | `FocusRequest \| null` | A zoom-to-span request pushed down to `WaveformView`. |
| `panels` | `Record<string, boolean>` | Which sidebar sections are expanded. |
| `filesOpen` | `boolean` | Whether the file library dialog is showing. |

### What invalidates a prepare

`updateConfig(patch)` clears `prepared` **unless every key in the patch is decode-only**. The
decode-only set is exactly `protocol`, `clockChannel`, `dataChannel` — those change nothing on
the instrument. Everything else (channels, `channelsSetup`, `maxFreqHz`, `samplesPerCycle`,
`depth`) is an acquisition setting and forces a re-prepare. `updateTrigger` always clears
`prepared` (the trigger is applied inside prepare, after its Default Setup). `resetSettings`
restores `DEFAULT_CONFIG`/`DEFAULT_TRIGGER`, clears storage, and clears `prepared`.

### Live re-decode

`decodeKey` is `protocol|clockChannel|dataChannel|maxFreqHz` joined (`maxFreqHz` is in there
because it is the UART baud hint). An effect keyed on `[decodeKey, result]` calls `redecode` and
merges the new `decoded` into `result`. `decodedWith` (a ref) records the key the current
annotation was produced with; `applyResult` — used by both `doCapture` and `FilesDialog`'s
`onResult` — sets it to the current key so a freshly captured result (already decoded by the
backend) does not trigger a redundant second decode.

### Results and cursors

`result` flows to `WaveformView` (`result` prop) and, when non-null, to `ByteList`
(`decoded`, `triggerS` from `triggerTime(result)`). `WaveformView` reports cursor placement up
via `onCursors` into `cursors`, which `ByteList` uses to highlight and scroll to the byte under
each cursor. Clicking a row in `ByteList` calls `focusByte`, which builds a `FocusRequest`
(`startS`, `endS`, `channel`, and a `performance.now()` nonce so re-picking the same byte
re-applies) and pushes it down as the `focus` prop.

### Persistence (`settings.ts`)

Three localStorage keys, versioned in the key name so an unmergeable shape can be dropped:
`openmso5202d.config.v1`, `openmso5202d.trigger.v1`, `openmso5202d.panels.v1`. Every read and
write is `try`-wrapped — storage failure is never an error, the defaults just apply.

`sanitise()` folds a stored config over `DEFAULT_CONFIG` and repairs it: channels filtered to
`{1,2}`, deduped and sorted (empty ⇒ default); `depth`/`protocol` checked against the allowed
lists; `maxFreqHz` positive; `samplesPerCycle` rounded and clamped to `[4, 1000]` (mirrors the
sidebar's `SPC_MIN`/`SPC_MAX`); `clockChannel`/`dataChannel` forced to 1 or 2; `channelsSetup`
rebuilt as exactly two entries merged over `DEFAULT_CHANNEL_SETUP`; and `1m` downgraded to
`512k` when more than one channel is on (the backend would otherwise reject it at prepare).
`loadTrigger()` merges over `DEFAULT_TRIGGER` and merges each Alter channel over
`DEFAULT_ALTER_CHANNEL`.

## Components

### `ControlPanel`

The sidebar. A scrolling body of four `Section`s over a pinned footer holding the two action
buttons ("① Prepare", "② Arm capture").

| Prop | Type | Purpose |
| --- | --- | --- |
| `config` | `CaptureConfig` | The configuration being edited. |
| `onChange` | `(patch: Partial<CaptureConfig>) => void` | Patch-style update (`App.updateConfig`). |
| `connected` | `boolean` | Gates both action buttons. |
| `prepared` | `boolean` | Gates the Capture button. |
| `busy` | `null \| "connect" \| "prepare" \| "capture" \| "card"` | Disables actions; sets button labels. |
| `onPrepare` / `onCapture` | `() => void` | The two flows. |
| `panels` | `Record<string, boolean>` | Section-open state; `open(id)` treats a missing key as open. |
| `onTogglePanel` | `(id: string) => void` | Folds a section. Section ids: `acquisition`, `channels`, `trigger`, `decode`. |
| `trigger` | `TriggerConfig` | Passed to `TriggerPanel`. |
| `onTriggerChange` | `(next: TriggerConfig) => void` | `App.updateTrigger`. |

Internal sub-components: `Acquisition` (channel chips, `FrequencyInput`, samples/clock
slider + `NumberInput`, `CaptureWindow`, depth segmented control), `CaptureWindow` (renders
`capturePlan`), `Decoder` (protocol segmented control + two `ChannelPicker`s), `ChannelPicker`,
`NumberInput` and `FrequencyInput` (both keep their own text state so the field can be cleared
mid-edit, and only reflect the parent value when not focused).

Rules encoded here: toggling on a second channel while depth is `1m` patches depth to `512k`;
the `1m` button is disabled unless exactly one channel is selected; assigning a protocol line to
the channel the other line occupies **swaps** them; SPI/I²C with fewer than two channels shows a
`--warn` hint. `triggerSourceChannel(trigger)` picks which channel's probe factor is handed to
`TriggerPanel` (CH1 under Alter, otherwise the trigger source, defaulting to CH1).

### `ChannelSetup`

Props: `config: CaptureConfig`, `onChange: (patch: Partial<CaptureConfig>) => void`,
`disabled: boolean`. Renders a block per channel (1 and 2) with segmented controls for probe
(`1x`/`10x`/`100x`/`1000x`), coupling (`dc`/`ac`/`gnd`), bandwidth (Full / 20 MHz) and invert.
Writes back the whole two-element `channelsSetup` array. Shows `--warn` hints for AC coupling,
invert and GND, since each of those quietly ruins a decode.

Exports: `DEFAULT_CHANNEL_SETUP`, `probeFactor(probe)` (1/10/100/1000), and
`channelSummary(config)` for the collapsed section header.

### `TriggerPanel`

Props: `busy: boolean`, `probeFactor: number`, `value: TriggerConfig`,
`onChange: (next: TriggerConfig) => void`. Nothing here touches the instrument — it edits a
configuration that **Prepare** applies (Prepare opens with a Default Setup, so a trigger applied
beforehand would be wiped).

Its `patch()` helper enforces two invariants: the source is re-snapped to the first allowed
option when the new type does not offer the current one (`SOURCES` per kind), and
`levelApplies` is set to `kind !== "slope"`.

Structure per kind: Type (`KINDS`, six), then either the Alter branch (two `AlterEditor`
blocks over `ALTER_KINDS`, four types each) or Source / polarity / Mode / Coupling / qualifier /
video Standard+Sync. Video replaces Mode and Coupling with Standard and Sync. Below that: the
Level row (− / value / + / 0, one `LEVEL_STEP` = 1 unit per click, matching the instrument's own
knob) when `levelApplies`, then one row per knob-only value from `valuesFor(value)` — a pure
mirror of the driver's `TriggerSetup::adjustables()`. Those rows edit `valueTargets[id]`, keyed
by the same ids `adjustable_from_id` in `api.rs` parses; `VALUE_CATALOGUE` supplies each one's
label, unit (`time` ps / `count` / `level` 1/25-div), step and post-Default-Setup `factory`
value.

Level display: `DEFAULT_LEVEL_SCALE` is 1000/25 mV per unit with zero at 0 — mirroring
`CaptureSpec::default()`'s 1 V/div, which is what Prepare sets — multiplied by `probeFactor`.
Non-analog sources (`ext`, `ext5`, `acline`) read in divisions instead.

Exports: `DEFAULT_TRIGGER`, `DEFAULT_ALTER_CHANNEL`, `triggerSummary(config)`.

### `WaveformView`

See the dedicated section below. Props: `result: CaptureResult | null`,
`onCursors?: (cursors: Cursor[]) => void`, `focus?: FocusRequest | null`. Exports the `Cursor`
and `FocusRequest` types and `triggerTime(result)`.

### `ByteList`

Props: `decoded: DecodedItem[]`, `cursors: Cursor[]`, `triggerS: number`,
`onSelect?: (item: DecodedItem) => void`. Filters `decoded` to `kind === "byte" || "address"`
and renders `# / hex / dec / chr / time` rows, time being `startS - triggerS`. The rows spanned
by cursor A and cursor B get the `a` / `b` marker classes; an effect keyed on those indices
calls `scrollIntoView({ block: "nearest" })` on the active row, so dragging a cursor walks the
list. Rows whose `text` contains `!` get the `bad` class (framing/ACK error). Returns `null`
when there are no bytes.

### `FilesDialog`

The waveform library. Props: `open`, `onClose`, `connected`, `busy` (true while another long
operation owns the USB link), `config`, `onBusyChange: (busy: boolean) => void`,
`onResult: (result: CaptureResult) => void`. Kept mounted by `App` (it returns `null` when
closed) so a listing and the channel assignment survive closing it.

Two sections — "Scope card" (Refresh / Download / Clear all, per-file tick boxes for download)
and "This computer" (Add files…, remove) — over a shared footer showing the CH1/CH2 assignment
and the "Plot traces" button. `AssignButtons` puts an `Entry` (`{source, value, size?}`) on a
channel; assigning the same file to the other channel clears the first, and re-clicking the
current channel takes it off. `plot()` builds `CsvSlot[]` from the assignment and calls
`loadCsvs(chosen, config)`, then `onResult` + `onClose`.

The initial listing fires when the dialog is *first opened* on a connected scope, guarded by a
`listed` ref (a result-keyed guard would re-fire forever after a flaky listing) and reset when
the connection drops. `run(job)` wraps every operation in `setWorking` + `onBusyChange` +
error capture. Download uses `@tauri-apps/plugin-dialog`'s `save` for one file and `open`
(directory) for several; downloaded paths join the local list. `clearAll` asks for a
`window.confirm` first.

### `Modal`

Props: `title`, `subtitle?`, `onClose`, `busy?`, `children`, `footer?`. Backdrop click
(`e.target === e.currentTarget`) and Escape dismiss, both suppressed while `busy`. Focuses the
panel itself (`tabIndex={-1}`) on mount rather than the first control. `role="dialog"`,
`aria-modal`, `aria-label={title}`.

### `Section`

Props: `title`, `summary?: ReactNode`, `open: boolean`, `onToggle: () => void`, `children`.
Header button carries `aria-expanded`; the summary renders in the header **only while
collapsed**, so folding a group never hides how it is configured. The body is unmounted when
closed.

## `WaveformView.tsx` in depth

### View model

`buildModel(result)` produces a `Model`:

- `dt` — `result.sampleIntervalS` (or `1e-9`).
- `duration` — `maxLen * dt`, `maxLen` being the longest channel.
- `triggerS` — `duration / 2`; the acquisition centres the trigger, so the axis is drawn
  signed about it. `triggerTime(result)` computes the same value for `ByteList`.
- `lanes: Lane[]` — one per channel, holding `channel`, `label`, `color`
  (`channelColor(channel)`), the `volts` array, the fitted `vMin`/`vMax` (widened to at least
  0.1 V), `voltsPerDiv` (from `voltsPerDivMv / 1000`, when the export reported it), and the
  `decoded` items filtered to that channel.

Layout constants: `GUTTER = 104` (left label rail), `AXIS = 32` (top time strip),
`LANE_PAD = 10`, `DECODE_STRIP = 32` (two text lines: hex + decimal). Lanes split the remaining
height evenly. `laneBand(lane, y0, laneH)` returns the strip the trace is actually drawn in —
it subtracts the decode strip when the lane has a decode, and is shared by rendering **and** the
pointer hit-test so a cursor is grabbed exactly where it is drawn.

Time window: `viewRef: View { t0, t1 }` in absolute record seconds. `clampView` keeps the span
within `[0, duration]`. Vertical: `voltViewsRef: VoltView[] { zoom, pan }` per lane
(`FIT = {zoom: 1, pan: 0}`); `laneRange(lane, view)` gives the shown range as
`centre ± (vMax−vMin)/2/zoom` with `centre` offset by `pan`; `voltToY` maps a voltage into the
band. Zoom is clamped to `[VOLT_ZOOM_MIN 0.2, VOLT_ZOOM_MAX 50]`.

Both the time window and the volt views live in **refs**, not state — the canvas draws from
them imperatively so a drag does not re-render React. Only `cursors` is duplicated into state,
because the `Measurements` readout renders from it. A new `model` resets both the window (to
the whole record) and the volt views.

`drawRef` holds the latest `draw` closure; the `ResizeObserver` effect (mounted once) calls
`drawRef.current()` so it never captures the first, empty model. Resizing the backing store
clears it, so every resize must redraw — which matters because decoding a capture opens the byte
list, narrowing the plot.

### Rendering passes

`render(canvas, model, view, cursors, voltViews)`, in order:

1. Size the backing store to `rect × devicePixelRatio`, `setTransform(dpr,…)`, clear, fill with
   `COLORS.bgPlot`.
2. `drawAxis` — 1-2-5 `niceStep` ticks anchored to `triggerS` (so 0 always lands on a
   gridline), full-height gridlines, labels from `timeAxisFormat(maxAbs, step)` which picks one
   unit for the whole axis and just enough decimals that adjacent ticks cannot round together.
   Then the dashed trigger marker at `triggerS`.
3. `drawByteSlices` — drawn **once for all lanes**, before them, from every lane's decoded items
   flattened, so a byte's boundaries line up through the clock trace too. Faint alternating fill
   (`COLORS.byteGuideFill`) plus boundary lines (`COLORS.byteGuide`); slices narrower than 3 px
   or off-screen are culled.
4. Per lane, `drawLane`: separator + gutter swatch/label/byte-count; H and L rails at the
   fitted `vMin`/`vMax`; the voltage grid — preferring the scope's own `voltsPerDiv`, halved or
   doubled until the spacing lands in 22–90 px, falling back to a `niceStep` when the export
   omitted it — labelled in the gutter; a summary line (`V/div`, `pk-pk`, and `×zoom` when not
   1); the trace as one min/max vertical segment per pixel column (constant cost at any depth),
   clipped to the band; the measurement cursors; then `drawDecode` for the pill strip.

`drawDecode` renders bus markers (`start`, `repeated-start`, `stop`) as a vertical line plus
label in `COLORS.cursor`, and byte/address items as a rounded `pill` at least as wide as its
text: `0xNN` in `HEX_FONT` on top, decimal in `DEC_FONT` below. An item whose `text` contains
`!` gets an appended `!` and a red pill outline.

### Pointer interaction

| Gesture | Effect |
| --- | --- |
| Wheel (deltaY + deltaX) | Pans time by `notches × span × WHEEL_PAN_FRACTION` (0.15). Zoom is not on the wheel. |
| Left-drag on empty canvas | Pans time (by pixel fraction of the span) **and** the starting lane's voltage `pan`. |
| Left-drag from within `GRAB_RADIUS` (10 px) of a cursor dot | Moves that cursor along time; its value keeps snapping to the trace and it stays in its own lane. |
| Right-drag | Zooms: horizontal movement scales time about the pointer (`zoomTimeAbout`, factor `exp(-dx × TIME_ZOOM_PER_PX)`), vertical movement scales the starting lane's voltage about the pointer (factor `exp(dy × VOLT_ZOOM_PER_PX)`, both 0.006/px). Both are anchored, so whatever is under the pointer stays put. |
| Left click (no movement) | Places a trace-snapped cursor in the clicked lane. Two at a time — a third starts a fresh pair. Ignored inside the gutter or the axis strip. |
| Double click, or the `⤢` toolbar button | `resetView` — whole record, all volt views back to FIT. |
| `✕` toolbar button (only with cursors) | Clears cursors. |
| Idle hover | Toggles the `over-cursor` class when a cursor is grabbable. |
| Context menu | Suppressed (`onContextMenu` preventDefault), since right-drag is a gesture. |

`dragRef` records `{x, y, moved, grabbed, volts, lane}`. A press only becomes a drag once the
pointer moves more than `CLICK_SLOP` (4 px) on either axis; `onPointerUp` places a cursor only
when `!moved && grabbed === null && !volts`. The canvas gets a `dragging` / `grabbing` /
`zooming-volts` class for the duration (cursor styling lives in `theme.css`).

`zoomTimeAbout` never lets the span fall below `model.dt * 8`.

### Focus / zoom-to-byte protocol

`ByteList` → `App.focusByte` → `focus: FocusRequest {startS, endS, channel, nonce}` →
`WaveformView`. An effect keyed **only on `focus?.nonce`** finds the lane whose `channel`
matches (falling back to lane 0), sets the window to
`max(width × FOCUS_ZOOM (8), dt × 16)` centred on the span, and places **two** cursors — one at
each end. Bracketing rather than dropping a single marker means the readout immediately reads
the span's own duration, and both ends stay draggable. The nonce is what makes picking the same
byte twice re-apply the zoom.

### Cursor readout

`Measurements` (rendered above the canvas when there is a result) shows row A, row B, and a Δ
row carrying `1/|Δt|` as a frequency, `|Δt|` and `Δv`. Times are shown relative to
`model.triggerS`.

## `timebase.ts`

Predicts what a capture will actually deliver, so the sidebar can say it before anything runs.
It mirrors the backend's `control::capture::deep_tdiv_for_bit` + `settings::TB_TO_NS`.

- `TB_TO_NS` — the SEC/DIV ladder in nanoseconds, 2-4-8 per decade, index 0 = 2 ns/div, 32 rungs
  up to 40 s/div.
- `DEPTH_ROWS` — exported rows per depth: 4 064 / 40 064 / 400 064 / 800 064 (`4000 × mult + 64`).
- `realtimeCeiling(channelCount)` — 800 MSa/s with one channel, 400 MSa/s with two (the ADC is
  shared). Equivalent-time sampling below ~8 ns/div is deliberately not modelled.
- `quantiseRate(rate)` — snaps a rate down to the 1-2-4-8-per-decade ladder the instrument
  actually offers.

`capturePlan(maxFreqHz, samplesPerClock, depth, channelCount = 1)`:

1. `samplesPerDiv = (rows − 64) / 20` — the record spans exactly 20 divisions.
2. `bitNs = 1e9 / maxFreqHz`; `ideal = bitNs × samplesPerDiv / samplesPerClock`.
3. Snap **down** to the largest rung `≤ ideal` ⇒ `timePerDivNs` (meets or exceeds the requested
   resolution rather than falling short — the same rule `CaptureConfig::timebase_ns()` in
   `api.rs` applies with `rposition`).
4. `wantedRate = samplesPerDiv / timePerDiv`; the real rate is
   `quantiseRate(min(wantedRate, realtimeCeiling))`.

Returns `CapturePlan | null` (`null` for an incoherent configuration):

| Field | Meaning |
| --- | --- |
| `timePerDivNs` | The SEC/DIV rung the scope will land on. |
| `windowS` | `20 × timePerDivNs`, the time the record covers. |
| `sampleIntervalS` | `1 / rate`. |
| `samplesPerClock` | Achieved samples per bit after the ladder snap — differs from the request. |
| `rateLimited` | True when the ADC ceiling or the rate ladder, not the timebase, set the interval. |

`formatDuration(seconds)` renders a compact `s`/`ms`/`µs`/`ns` figure and is also used by
`TriggerPanel` for picosecond values.

## Build and dev

Driven from the repo root (one Cargo workspace, one pnpm workspace):

| Command | What happens |
| --- | --- |
| `pnpm dev` | Root script → `pnpm --filter openmso5202d-frontend tauri dev`. Tauri runs `beforeDevCommand` (`pnpm dev` = `vite`), waits for `devUrl` `http://localhost:1420`, then builds and runs the Rust shell in debug, pointing the webview at the dev server. Frontend edits hot-reload; Rust edits need a restart. |
| `pnpm build` | Root script → `tauri build`. Tauri runs `beforeBuildCommand` (`pnpm build` = `tsc --noEmit && vite build`), which emits `frontend/dist` (`frontendDist: "../dist"`), then compiles the Rust release binary with the web assets embedded — no Node or Vite at run time. |
| `pnpm --filter openmso5202d-frontend tauri build --no-bundle` | Executable only, skipping the installers. |

The dev port is fixed at 1420 with `strictPort: true` (`vite.config.ts`) because
`tauri.conf.json` hard-codes `devUrl`; changing one requires changing the other.
`clearScreen: false` keeps Cargo's output visible.

Window config (`tauri.conf.json` → `app.windows[0]`): label `main`, title `openmso5202D`,
1600 × 1000, resizable, minimum 1000 × 1000, centred. `security.csp` is `null`. Bundle targets
are `all`, with the icons under `src-tauri/icons/`.

Outputs land in the **workspace-shared** `target/`:

| Output | Path |
| --- | --- |
| Executable | `target/release/openmso5202d` |
| Debian package | `target/release/bundle/deb/openmso5202D_0.1.0_amd64.deb` |
| RPM package | `target/release/bundle/rpm/openmso5202D-0.1.0-1.x86_64.rpm` |
| AppImage | `target/release/bundle/appimage/openmso5202D_0.1.0_amd64.AppImage` |

The crate is `openmso5202d` with lib name `openmso5202d_lib` and crate types
`["staticlib", "cdylib", "rlib"]`; `main.rs` is a six-line entry point calling
`openmso5202d_lib::run()`. `frontend/src-tauri/tests/prepare_applies_trigger.rs` is an
`#[ignore]`d hardware test that deserialises the verbatim webview JSON into `api::TriggerConfig`
and drives a real scope — run with
`cargo test -p openmso5202d --release -- --ignored --nocapture`.

## Conventions

**Theming.** All colours are CSS custom properties on `:root` in `theme.css`: surfaces
(`--bg-plot`, `--bg-app`, `--bg-panel`, `--bg-panel-2`, `--bg-elevated`, `--border`,
`--border-strong`), text (`--text`, `--text-dim`, `--text-faint`), accent and state
(`--accent`, `--accent-strong`, `--accent-ink`, `--danger`, `--ok`, `--warn`), signal colours
(`--ch1`, `--ch2`, `--decode`, `--decode-ink`) and `--radius` / `--radius-sm`. Use a variable,
never a literal hex, in CSS. Warnings inside the sidebar are written as
`style={{ color: "var(--warn)" }}` on a `.hint`.

**Canvas colours** cannot read CSS variables, so `theme.ts` restates them as the `COLORS`
object plus `channelColor(channel)` (CH1 `#f5c542`, CH2 `#4bc0e0`, anything else `#b58af0`).
`--ch1`/`--ch2`/`--bg-plot`/`--decode` and their `theme.ts` twins must be changed together.

**Class naming.** Channel-tinted elements carry a `ch1`/`ch2` class (`.chip.ch1`,
`.alter-head.ch2`, `.slot.ch1`); active segmented buttons carry `active` (plus `accent` for the
protocol picker); cursor-tinted byte rows carry `a`/`b`; error states carry `bad` or `danger`.

**Other rules a contributor must respect:**

- The webview never talks to USB or the filesystem directly — add a Tauri command in `api.rs`
  and a typed wrapper in `api.ts`, and register it in `lib.rs`.
- Anything that can block on the instrument is an `async` command.
- Enum values cross the boundary as lowercase strings; keep `api.ts`'s unions and `api.rs`'s
  `match` arms in step.
- Any new acquisition setting must invalidate `prepared` — add it outside the decode-only list
  in `App.updateConfig`, and to `sanitise()` in `settings.ts` if it is persisted.
- New settings that are persisted must survive a stored record that predates them; that is what
  the merge-over-defaults in `sanitise`/`loadTrigger` is for.
- `timebase.ts`'s ladder and depth-row table describe fixed instrument hardware and must stay in
  step with `settings::TB_TO_NS` and `control::capture::deep_tdiv_for_bit` in the backend.
- Trigger knob-value ids in `TriggerPanel`'s `VALUE_CATALOGUE` must match
  `adjustable_from_id()` in `api.rs`.
- Long-running commands should stream progress through a `ProgressSink`, on one of the three
  existing event names, so the topbar bar moves.
