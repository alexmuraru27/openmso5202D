import { useEffect, useMemo, useRef, useState } from "react";
import type { CaptureResult, DecodedItem } from "../api";
import { COLORS, channelColor } from "../theme";

const GUTTER = 104; // left label rail
const AXIS = 32; // top time-axis strip
const LANE_PAD = 10; // vertical padding inside a lane
const DECODE_STRIP = 32; // height of the per-lane decode (pill) strip — two lines: hex + dec
const HEX_FONT = "600 11px ui-monospace, monospace"; // top line: 0x-prefixed hex
const DEC_FONT = "500 10px ui-monospace, monospace"; // bottom line: decimal

/** Pixels of pointer movement below which a press counts as a click, not a drag. */
const CLICK_SLOP = 4;

/** How close the pointer must be to a cursor's dot to pick it up. */
const GRAB_RADIUS = 10;

/** A time window in seconds, over the record's absolute sample time. */
interface View {
  t0: number;
  t1: number;
}

/** A measurement cursor: a point on a trace, in record time and volts. */
export interface Cursor {
  /** Time within the record, seconds. */
  t: number;
  /** The trace's value there, volts. */
  v: number;
  /** Which lane it was placed on. */
  lane: number;
}

/** A request to zoom to a span and bracket it with cursors. */
export interface FocusRequest {
  startS: number;
  endS: number;
  /** Channel the span belongs to, so the cursors land on the right lane. */
  channel: number;
  /** Changes on every request, so selecting the same item twice re-applies it. */
  nonce: number;
}

/** How much of the view a focused span should occupy — the rest is context either side. */
const FOCUS_ZOOM = 8;

export function WaveformView({
  result,
  onCursors,
  focus,
}: {
  result: CaptureResult | null;
  /** Reports cursor placement/movement, so a side panel can follow it. */
  onCursors?: (cursors: Cursor[]) => void;
  /** Zoom to a span and bracket it with cursors (e.g. a byte picked from the list). */
  focus?: FocusRequest | null;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const viewRef = useRef<View>({ t0: 0, t1: 1 });
  const dragRef = useRef<{ x: number; y: number; moved: boolean; grabbed: number | null } | null>(
    null,
  );
  // Cursors live in both a ref (the canvas draws from it) and state (the readout renders
  // from it), so a placement updates the picture and the numbers together.
  const cursorsRef = useRef<Cursor[]>([]);
  const [cursors, setCursors] = useState<Cursor[]>([]);

  // Per-channel vertical scale + the trigger-centred time origin, recomputed per capture.
  const model = useMemo(() => buildModel(result), [result]);

  // Reset the view to the whole record whenever a new capture arrives.
  useEffect(() => {
    viewRef.current = { t0: 0, t1: Math.max(model.duration, 1e-9) };
    draw();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [model]);

  const draw = () => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    render(canvas, model, viewRef.current, cursorsRef.current);
  };

  /** Place/replace cursors, keeping the canvas and the readout in step. */
  const putCursors = (next: Cursor[]) => {
    cursorsRef.current = next;
    setCursors(next);
    onCursors?.(next);
    draw();
  };

  // Keep the canvas backing store matched to its display size.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const observer = new ResizeObserver(draw);
    observer.observe(canvas);
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // --- interaction ---------------------------------------------------------

  const clampView = (v: View): View => {
    const span = Math.min(v.t1 - v.t0, model.duration);
    let t0 = v.t0;
    if (span >= model.duration) t0 = 0;
    else t0 = Math.max(0, Math.min(t0, model.duration - span));
    return { t0, t1: t0 + span };
  };

  const onWheel = (e: React.WheelEvent) => {
    if (!model.duration) return;
    e.preventDefault();
    const canvas = canvasRef.current!;
    const rect = canvas.getBoundingClientRect();
    const plotW = rect.width - GUTTER;
    const frac = Math.max(0, (e.clientX - rect.left - GUTTER) / plotW);
    const v = viewRef.current;
    const span = v.t1 - v.t0;
    const center = v.t0 + frac * span;
    const factor = e.deltaY > 0 ? 1.18 : 1 / 1.18;
    const minSpan = model.dt * 8; // don't zoom past a few samples
    const newSpan = Math.max(minSpan, Math.min(model.duration, span * factor));
    viewRef.current = clampView({
      t0: center - frac * newSpan,
      t1: center - frac * newSpan + newSpan,
    });
    draw();
  };

  /** A trace-snapped cursor for a lane at a given record time. */
  const cursorAtTime = (time: number, laneIndex: number): Cursor | null => {
    const lane = model.lanes[laneIndex];
    if (!lane) return null;
    const t = Math.min(model.duration, Math.max(0, time));
    const index = Math.min(lane.volts.length - 1, Math.max(0, Math.round(t / model.dt)));
    if (!Number.isFinite(lane.volts[index])) return null;
    return { t, v: lane.volts[index], lane: laneIndex };
  };

  /** The same, for a pointer position on the canvas. */
  const cursorAt = (x: number, rect: DOMRect, laneIndex: number): Cursor | null => {
    const view = viewRef.current;
    const t = view.t0 + ((x - GUTTER) / (rect.width - GUTTER)) * (view.t1 - view.t0);
    return cursorAtTime(t, laneIndex);
  };

  /** Index of a cursor whose dot is within the grab radius of (x, y), if any. */
  const cursorNear = (x: number, y: number, rect: DOMRect): number | null => {
    const view = viewRef.current;
    const plotW = rect.width - GUTTER;
    const laneH = (rect.height - AXIS) / Math.max(1, model.lanes.length);
    // Last placed wins, so the cursor drawn on top is the one you grab.
    for (let i = cursorsRef.current.length - 1; i >= 0; i--) {
      const cursor = cursorsRef.current[i];
      const lane = model.lanes[cursor.lane];
      if (!lane) continue;
      const cx = GUTTER + ((cursor.t - view.t0) / (view.t1 - view.t0)) * plotW;
      const { bandY, bandH } = laneBand(lane, AXIS + cursor.lane * laneH, laneH);
      const cy = voltToY(lane, bandY, bandH, cursor.v);
      if (Math.hypot(x - cx, y - cy) <= GRAB_RADIUS) return i;
    }
    return null;
  };

  const onPointerDown = (e: React.PointerEvent) => {
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    const rect = canvasRef.current!.getBoundingClientRect();
    // Grabbing a cursor takes precedence over panning; anywhere else pans (or, without
    // movement, places a new cursor on release).
    const grabbed = cursorNear(e.clientX - rect.left, e.clientY - rect.top, rect);
    dragRef.current = { x: e.clientX, y: e.clientY, moved: false, grabbed };
    canvasRef.current?.classList.add(grabbed === null ? "dragging" : "grabbing");
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const drag = dragRef.current;
    const canvas = canvasRef.current;
    if (!canvas || !model.duration) return;
    const rect = canvas.getBoundingClientRect();

    if (!drag) {
      // Idle hover: show that a cursor can be picked up.
      const over = cursorNear(e.clientX - rect.left, e.clientY - rect.top, rect) !== null;
      canvas.classList.toggle("over-cursor", over);
      return;
    }

    if (Math.abs(e.clientX - drag.x) > CLICK_SLOP || Math.abs(e.clientY - drag.y) > CLICK_SLOP) {
      drag.moved = true;
    }

    if (drag.grabbed !== null) {
      // Move the grabbed cursor along time; its voltage keeps following the trace, and it
      // stays in its own lane so a slight vertical wobble cannot fling it to the other one.
      const held = cursorsRef.current[drag.grabbed];
      const next = cursorAt(e.clientX - rect.left, rect, held.lane);
      if (next) {
        const all = [...cursorsRef.current];
        all[drag.grabbed] = next;
        putCursors(all);
      }
      drag.x = e.clientX;
      return;
    }

    const plotW = rect.width - GUTTER;
    const v = viewRef.current;
    const span = v.t1 - v.t0;
    const deltaT = ((drag.x - e.clientX) / plotW) * span;
    viewRef.current = clampView({ t0: v.t0 + deltaT, t1: v.t1 + deltaT });
    drag.x = e.clientX;
    draw();
  };

  /** A press that did not turn into a drag places a measurement cursor. */
  const onPointerUp = (e: React.PointerEvent) => {
    const drag = dragRef.current;
    endDrag();
    // Dragging an existing cursor is a move, never a placement.
    if (!drag || drag.moved || drag.grabbed !== null || model.lanes.length === 0) return;

    const rect = canvasRef.current!.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    if (x < GUTTER || y < AXIS) return;

    const laneH = (rect.height - AXIS) / model.lanes.length;
    const laneIndex = Math.min(model.lanes.length - 1, Math.max(0, Math.floor((y - AXIS) / laneH)));
    const placed = cursorAt(x, rect, laneIndex);
    if (!placed) return;
    // Two cursors at a time — a third starts a fresh pair.
    putCursors(cursorsRef.current.length >= 2 ? [placed] : [...cursorsRef.current, placed]);
  };

  const endDrag = () => {
    dragRef.current = null;
    canvasRef.current?.classList.remove("dragging");
    canvasRef.current?.classList.remove("grabbing");
  };

  // Zoom to a requested span and bracket it with cursors. Bracketing rather than dropping a
  // single marker means the readout immediately gives the span's own duration — for a byte,
  // that is its bit time — and both ends stay draggable afterwards.
  useEffect(() => {
    if (!focus || model.lanes.length === 0 || !model.duration) return;
    const laneIndex = Math.max(
      0,
      model.lanes.findIndex((lane) => lane.channel === focus.channel),
    );
    const width = Math.max(focus.endS - focus.startS, model.dt);
    const span = Math.min(model.duration, Math.max(width * FOCUS_ZOOM, model.dt * 16));
    const centre = (focus.startS + focus.endS) / 2;
    viewRef.current = clampView({ t0: centre - span / 2, t1: centre + span / 2 });
    const ends = [cursorAtTime(focus.startS, laneIndex), cursorAtTime(focus.endS, laneIndex)];
    putCursors(ends.filter((c): c is Cursor => c !== null));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus?.nonce]);
  const resetView = () => {
    viewRef.current = { t0: 0, t1: Math.max(model.duration, 1e-9) };
    draw();
  };

  return (
    <>
      {!result && (
        <div className="plot-empty">
          <div className="big">No capture yet</div>
          <div>Prepare the scope, then capture to see the waveform.</div>
        </div>
      )}
      <div className="plot-toolbar">
        {cursors.length > 0 && (
          <button className="icon-btn" title="Clear cursors" onClick={() => putCursors([])}>
            ✕
          </button>
        )}
        <button className="icon-btn" title="Fit to record" onClick={resetView}>
          ⤢
        </button>
      </div>
      {result && <Measurements cursors={cursors} model={model} />}
      <canvas
        ref={canvasRef}
        className="plot-canvas"
        onWheel={onWheel}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={endDrag}
        onDoubleClick={resetView}
      />
    </>
  );
}

/** The cursor readout: each point, and the differences between them. */
function Measurements({ cursors, model }: { cursors: Cursor[]; model: Model }) {
  if (cursors.length === 0) {
    return <div className="measure hint-only">Click the trace to measure · click again for Δ</div>;
  }
  const [a, b] = cursors;
  const label = (c: Cursor) => model.lanes[c.lane]?.label ?? `CH${c.lane + 1}`;
  return (
    <div className="measure">
      <div className="row">
        <span className="tag a">A</span>
        <span className="who">{label(a)}</span>
        <span className="val">{formatSeconds(a.t - model.triggerS)}</span>
        <span className="val">{formatVolts(a.v)}</span>
      </div>
      {b && (
        <>
          <div className="row">
            <span className="tag b">B</span>
            <span className="who">{label(b)}</span>
            <span className="val">{formatSeconds(b.t - model.triggerS)}</span>
            <span className="val">{formatVolts(b.v)}</span>
          </div>
          <div className="row delta">
            <span className="tag">Δ</span>
            <span className="who">{/* frequency implied by the interval */}
              {Math.abs(b.t - a.t) > 0 ? `${formatHz(1 / Math.abs(b.t - a.t))}` : "—"}
            </span>
            <span className="val">{formatSeconds(Math.abs(b.t - a.t), true)}</span>
            <span className="val">{formatVolts(b.v - a.v, true)}</span>
          </div>
        </>
      )}
    </div>
  );
}

// --- model ------------------------------------------------------------------

interface Lane {
  channel: number;
  label: string;
  color: string;
  volts: number[];
  vMin: number;
  vMax: number;
  decoded: DecodedItem[];
}

interface Model {
  dt: number;
  duration: number;
  /** Time of the trigger, for a centred, signed axis. */
  triggerS: number;
  lanes: Lane[];
}

/** Where the trigger sits in a record — its midpoint, as the acquisition centres it. */
export function triggerTime(result: CaptureResult | null): number {
  if (!result || result.channels.length === 0) return 0;
  const dt = result.sampleIntervalS || 1e-9;
  const maxLen = result.channels.reduce((n, c) => Math.max(n, c.volts.length), 0);
  return (maxLen * dt) / 2;
}

function buildModel(result: CaptureResult | null): Model {
  if (!result || result.channels.length === 0) {
    return { dt: 1, duration: 0, triggerS: 0, lanes: [] };
  }
  const dt = result.sampleIntervalS || 1e-9;
  const maxLen = result.channels.reduce((n, c) => Math.max(n, c.volts.length), 0);
  const duration = maxLen * dt;
  const lanes: Lane[] = result.channels.map((c) => {
    let vMin = Infinity;
    let vMax = -Infinity;
    for (const v of c.volts) {
      if (v < vMin) vMin = v;
      if (v > vMax) vMax = v;
    }
    if (!Number.isFinite(vMin)) {
      vMin = 0;
      vMax = 1;
    }
    if (vMax - vMin < 0.1) vMax = vMin + 0.1;
    return {
      channel: c.channel,
      label: c.label,
      color: channelColor(c.channel),
      volts: c.volts,
      vMin,
      vMax,
      decoded: result.decoded.filter((d) => d.channel === c.channel),
    };
  });
  return { dt, duration, triggerS: duration / 2, lanes };
}

// --- rendering --------------------------------------------------------------

function render(canvas: HTMLCanvasElement, model: Model, view: View, cursors: Cursor[]) {
  const dpr = window.devicePixelRatio || 1;
  const rect = canvas.getBoundingClientRect();
  const W = Math.max(1, Math.floor(rect.width));
  const H = Math.max(1, Math.floor(rect.height));
  if (canvas.width !== W * dpr || canvas.height !== H * dpr) {
    canvas.width = W * dpr;
    canvas.height = H * dpr;
  }
  const ctx = canvas.getContext("2d")!;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  ctx.fillStyle = COLORS.bgPlot;
  ctx.fillRect(0, 0, W, H);

  const plotX = GUTTER;
  const plotW = W - GUTTER;
  const plotY = AXIS;
  const plotH = H - AXIS;
  if (model.lanes.length === 0 || plotW <= 0) return;

  const span = view.t1 - view.t0;
  const timeToX = (t: number) => plotX + ((t - view.t0) / span) * plotW;

  drawAxis(ctx, model, view, timeToX, plotX, plotW, H);

  // Byte-slice guides span ALL lanes, so each decoded byte's boundaries line up across every
  // waveform (e.g. the SPI data bytes drawn through the clock trace too). Drawn once here,
  // before the lanes, so the traces stay crisp on top. The decoded items live on one channel
  // but the guides are placed by time, so they apply to all.
  const decoded = model.lanes.flatMap((lane) => lane.decoded);
  if (decoded.length) {
    const topStrip = model.lanes[0].decoded.length ? DECODE_STRIP : 0;
    const top = plotY + LANE_PAD + topStrip;
    const bottom = plotY + plotH - LANE_PAD;
    drawByteSlices(ctx, decoded, timeToX, plotX, plotW, top, bottom - top);
  }

  const laneH = plotH / model.lanes.length;
  model.lanes.forEach((lane, i) => {
    const y0 = plotY + i * laneH;
    drawLane(ctx, lane, model, view, timeToX, plotX, plotW, y0, laneH, cursors, i);
  });
}

function drawAxis(
  ctx: CanvasRenderingContext2D,
  model: Model,
  view: View,
  timeToX: (t: number) => number,
  plotX: number,
  plotW: number,
  H: number,
) {
  const span = view.t1 - view.t0;
  const step = niceStep(span, plotW / 90);
  // Anchor ticks to the TRIGGER (time origin), not to the record start, so 0 always lands on
  // a gridline and every label is a clean multiple of the step.
  const rel0 = view.t0 - model.triggerS;
  const rel1 = view.t1 - model.triggerS;
  const first = Math.ceil(rel0 / step) * step;
  // One unit + decimal count for the whole axis, derived from its extent and the STEP — so
  // adjacent ticks a step apart never round to the same text (the "−28 µs ×5" bug).
  const label = timeAxisFormat(Math.max(Math.abs(rel0), Math.abs(rel1)), step);
  ctx.font = "11px ui-monospace, monospace";
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";
  for (let rel = first; rel <= rel1 + step * 1e-6; rel += step) {
    const x = timeToX(model.triggerS + rel);
    ctx.strokeStyle = COLORS.grid;
    ctx.beginPath();
    ctx.moveTo(x, AXIS);
    ctx.lineTo(x, H);
    ctx.stroke();
    ctx.fillStyle = COLORS.axisText;
    ctx.fillText(label(rel), x, AXIS / 2);
  }
  // The trigger marker (time origin).
  const tx = timeToX(model.triggerS);
  if (tx >= plotX && tx <= plotX + plotW) {
    ctx.strokeStyle = COLORS.cursor;
    ctx.globalAlpha = 0.5;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(tx, AXIS);
    ctx.lineTo(tx, H);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.globalAlpha = 1;
  }
}

function drawLane(
  ctx: CanvasRenderingContext2D,
  lane: Lane,
  model: Model,
  view: View,
  timeToX: (t: number) => number,
  plotX: number,
  plotW: number,
  y0: number,
  laneH: number,
  cursors: Cursor[],
  laneIndex: number,
) {
  // Separator + gutter label.
  ctx.strokeStyle = COLORS.laneBorder;
  ctx.beginPath();
  ctx.moveTo(0, y0);
  ctx.lineTo(plotX + plotW, y0);
  ctx.stroke();

  ctx.fillStyle = lane.color;
  ctx.fillRect(14, y0 + 16, 10, 10);
  ctx.fillStyle = COLORS.laneLabel;
  ctx.font = "600 13px Inter, system-ui, sans-serif";
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText(lane.label, 32, y0 + 21);
  if (lane.decoded.length) {
    ctx.fillStyle = COLORS.axisText;
    ctx.font = "11px Inter, system-ui, sans-serif";
    ctx.fillText(`${lane.decoded.filter(isByte).length} bytes`, 32, y0 + 38);
  }

  // Signal band: leave room for a decode strip at the top when this lane has a decode.
  // Shared with the pointer hit-test, so a cursor is grabbed exactly where it is drawn.
  const { bandY, bandH } = laneBand(lane, y0, laneH);
  const decodeStrip = lane.decoded.length ? DECODE_STRIP : 0;
  const yOf = (v: number) => voltToY(lane, bandY, bandH, v);

  // H / L rails.
  ctx.strokeStyle = COLORS.rail;
  ctx.globalAlpha = 0.35;
  for (const v of [lane.vMax, lane.vMin]) {
    const y = yOf(v);
    ctx.beginPath();
    ctx.moveTo(plotX, y);
    ctx.lineTo(plotX + plotW, y);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;

  // Voltage grid: 1-2-5 steps across the lane's range, labelled in the gutter, so an analog
  // trace can be read off quantitatively rather than only as a shape.
  const vStep = niceStep(lane.vMax - lane.vMin, bandH / 34);
  ctx.font = "10px ui-monospace, monospace";
  ctx.textBaseline = "middle";
  for (let v = Math.ceil(lane.vMin / vStep) * vStep; v <= lane.vMax; v += vStep) {
    const y = yOf(v);
    ctx.strokeStyle = COLORS.grid;
    ctx.globalAlpha = 0.7;
    ctx.beginPath();
    ctx.moveTo(plotX, y);
    ctx.lineTo(plotX + plotW, y);
    ctx.stroke();
    ctx.globalAlpha = 1;
    ctx.fillStyle = COLORS.axisText;
    ctx.textAlign = "right";
    ctx.fillText(formatVolts(v), plotX - 6, y);
  }

  // The lane's full swing, for a quick read of the signal's amplitude.
  ctx.fillStyle = COLORS.axisText;
  ctx.font = "10px ui-monospace, monospace";
  ctx.textAlign = "left";
  ctx.fillText(
    `${formatVolts(lane.vMin)} … ${formatVolts(lane.vMax)}  pk-pk ${formatVolts(lane.vMax - lane.vMin)}`,
    32,
    y0 + (lane.decoded.length ? 54 : 38),
  );

  // Trace: one vertical min/max segment per pixel column (fast at any depth).
  ctx.strokeStyle = lane.color;
  ctx.lineWidth = 1.4;
  ctx.beginPath();
  const span = view.t1 - view.t0;
  const n = lane.volts.length;
  let started = false;
  for (let px = 0; px <= plotW; px++) {
    const tA = view.t0 + (px / plotW) * span;
    const tB = view.t0 + ((px + 1) / plotW) * span;
    let iA = Math.floor(tA / model.dt);
    let iB = Math.ceil(tB / model.dt);
    if (iB <= 0 || iA >= n) continue;
    iA = Math.max(0, iA);
    iB = Math.min(n, iB);
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = iA; i < iB; i++) {
      const v = lane.volts[i];
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    if (!Number.isFinite(lo)) continue;
    const x = plotX + px;
    if (!started) {
      ctx.moveTo(x, yOf(hi));
      started = true;
    }
    ctx.lineTo(x, yOf(hi));
    ctx.lineTo(x, yOf(lo));
  }
  ctx.stroke();
  ctx.lineWidth = 1;

  // Measurement cursors: a full-height time marker (so two lanes can be compared) plus a
  // dot at the sampled value on the lane the cursor was placed in.
  cursors.forEach((cursor, index) => {
    const x = timeToX(cursor.t);
    if (x < plotX || x > plotX + plotW) return;
    ctx.strokeStyle = index === 0 ? COLORS.cursorA : COLORS.cursorB;
    ctx.fillStyle = ctx.strokeStyle;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(x, y0);
    ctx.lineTo(x, y0 + laneH);
    ctx.stroke();
    ctx.setLineDash([]);
    if (cursor.lane === laneIndex) {
      const y = yOf(cursor.v);
      ctx.beginPath();
      ctx.arc(x, y, 3.5, 0, Math.PI * 2);
      ctx.fill();
      ctx.beginPath();               // level line, so ΔV is visible against the other cursor
      ctx.moveTo(plotX, y);
      ctx.lineTo(plotX + plotW, y);
      ctx.globalAlpha = 0.45;
      ctx.setLineDash([3, 3]);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;
      ctx.font = "600 10px ui-monospace, monospace";
      ctx.textAlign = "left";
      ctx.textBaseline = "bottom";
      ctx.fillText(index === 0 ? "A" : "B", x + 5, y - 4);
    }
  });

  // Decode strip.
  if (lane.decoded.length) {
    drawDecode(ctx, lane.decoded, timeToX, plotX, plotW, y0 + LANE_PAD, decodeStrip);
  }
}

/** Vertical byte-boundary guides across the signal band — the "byte slicing" that ties each
 * decoded byte to the stretch of waveform it was read from. A faint alternating fill sets
 * neighbouring bytes apart. Culls slices too narrow to read (the pills still convey those)
 * and anything off-screen, so it stays clean at any zoom. */
function drawByteSlices(
  ctx: CanvasRenderingContext2D,
  items: DecodedItem[],
  timeToX: (t: number) => number,
  plotX: number,
  plotW: number,
  bandY: number,
  bandH: number,
) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(plotX, bandY, plotW, bandH);
  ctx.clip();
  ctx.lineWidth = 1;
  for (const item of items) {
    if (!isByte(item)) continue;
    const x0 = timeToX(item.startS);
    const x1 = timeToX(item.endS);
    if (x1 < plotX || x0 > plotX + plotW) continue;
    const w = x1 - x0;
    if (w < 3) continue; // too thin to slice cleanly — the decode pill already stands in

    ctx.fillStyle = COLORS.byteGuideFill;
    ctx.fillRect(x0, bandY, w, bandH);
    ctx.strokeStyle = COLORS.byteGuide;
    ctx.beginPath();
    ctx.moveTo(Math.round(x0) + 0.5, bandY);
    ctx.lineTo(Math.round(x0) + 0.5, bandY + bandH);
    ctx.moveTo(Math.round(x1) + 0.5, bandY);
    ctx.lineTo(Math.round(x1) + 0.5, bandY + bandH);
    ctx.stroke();
  }
  ctx.restore();
}

function drawDecode(
  ctx: CanvasRenderingContext2D,
  items: DecodedItem[],
  timeToX: (t: number) => number,
  plotX: number,
  plotW: number,
  y: number,
  h: number,
) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(plotX, y - 2, plotW, h + 4);
  ctx.clip();
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";
  ctx.font = "600 11px ui-monospace, monospace";

  for (const item of items) {
    const x0 = timeToX(item.startS);
    const x1 = timeToX(item.endS);
    if (x1 < plotX - 40 || x0 > plotX + plotW + 40) continue;

    if (item.kind === "start" || item.kind === "repeated-start" || item.kind === "stop") {
      ctx.strokeStyle = COLORS.cursor;
      ctx.beginPath();
      ctx.moveTo(x0, y);
      ctx.lineTo(x0, y + h);
      ctx.stroke();
      ctx.fillStyle = COLORS.cursor;
      ctx.fillText(item.text, x0, y + h / 2);
      continue;
    }

    const cx = (x0 + x1) / 2;
    const bad = item.text.includes("!");

    if (item.value != null) {
      // Two lines: 0x-prefixed hex on top, decimal below.
      const hex = `0x${item.value.toString(16).toUpperCase().padStart(2, "0")}${bad ? "!" : ""}`;
      const dec = String(item.value);
      ctx.font = HEX_FONT;
      const wHex = ctx.measureText(hex).width;
      ctx.font = DEC_FONT;
      const wDec = ctx.measureText(dec).width;
      const w = Math.max(x1 - x0 - 2, Math.max(wHex, wDec) + 12);
      const px = cx - w / 2;
      pill(ctx, px, y, w, h, COLORS.decodeFill, bad);
      ctx.fillStyle = COLORS.decodeInk;
      ctx.font = HEX_FONT;
      ctx.fillText(hex, cx, y + h * 0.33);
      ctx.font = DEC_FONT;
      ctx.fillText(dec, cx, y + h * 0.71);
    } else {
      const label = item.text.replace("!", "");
      const w = Math.max(x1 - x0 - 2, ctx.measureText(label).width + 12);
      const px = cx - w / 2;
      pill(ctx, px, y, w, h, COLORS.decodeFill, bad);
      ctx.fillStyle = COLORS.decodeInk;
      ctx.fillText(label, cx, y + h / 2 + 0.5);
    }
  }
  ctx.restore();
}

function pill(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  fill: string,
  bad: boolean,
) {
  const r = Math.min(5, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
  ctx.fillStyle = fill;
  ctx.fill();
  if (bad) {
    ctx.strokeStyle = "#ef5a6f";
    ctx.lineWidth = 1.5;
    ctx.stroke();
    ctx.lineWidth = 1;
  }
}

// --- helpers ----------------------------------------------------------------

const isByte = (d: DecodedItem) => d.kind === "byte" || d.kind === "address";

/** A 1-2-5 "nice" step at least `minPixels` apart in the current view. */
function niceStep(span: number, targetTicks: number): number {
  const raw = span / Math.max(1, targetTicks);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / mag;
  const step = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
  return step * mag;
}

/** The signal band inside a lane — the strip the trace is actually drawn in. */
function laneBand(lane: Lane, y0: number, laneH: number): { bandY: number; bandH: number } {
  const decodeStrip = lane.decoded.length ? DECODE_STRIP : 0;
  return {
    bandY: y0 + LANE_PAD + decodeStrip,
    bandH: laneH - LANE_PAD * 2 - decodeStrip,
  };
}

/** Where a voltage sits vertically within a lane's band. */
function voltToY(lane: Lane, bandY: number, bandH: number, v: number): number {
  return bandY + bandH - ((v - lane.vMin) / (lane.vMax - lane.vMin)) * bandH;
}

/** Volts at a readable scale: `3.3 V`, `500 mV`, `-1.2 V`. */
function formatVolts(v: number, signed = false): string {
  const sign = signed && v > 0 ? "+" : "";
  if (Math.abs(v) < 1) return `${sign}${(v * 1000).toFixed(0)} mV`;
  return `${sign}${v.toFixed(2)} V`;
}

/** A duration for the readout, always with a unit. */
function formatSeconds(s: number, signed = false): string {
  const sign = signed ? "" : s < 0 ? "\u2212" : "+";
  const a = Math.abs(s);
  if (a < 1e-6) return `${sign}${(a * 1e9).toFixed(1)} ns`;
  if (a < 1e-3) return `${sign}${(a * 1e6).toFixed(2)} \u00b5s`;
  if (a < 1) return `${sign}${(a * 1e3).toFixed(3)} ms`;
  return `${sign}${a.toFixed(4)} s`;
}

/** The frequency implied by an interval. */
function formatHz(hz: number): string {
  if (hz >= 1e6) return `${(hz / 1e6).toFixed(2)} MHz`;
  if (hz >= 1e3) return `${(hz / 1e3).toFixed(2)} kHz`;
  return `${hz.toFixed(1)} Hz`;
}

const TIME_UNITS: [number, string][] = [
  [1, "s"],
  [1e-3, "ms"],
  [1e-6, "µs"],
  [1e-9, "ns"],
];

/** Build a time-axis label formatter that uses ONE unit for the whole axis (chosen from its
 * extent) and just enough decimals to tell adjacent `step`-apart ticks apart — so a fine
 * zoom never collapses several ticks onto the same rounded label. */
function timeAxisFormat(maxAbs: number, step: number): (s: number) => string {
  // Largest unit whose scale does not exceed the axis extent (falls back to ns).
  let scale = 1e-9;
  let name = "ns";
  for (const [s, n] of TIME_UNITS) {
    if (maxAbs >= s) {
      scale = s;
      name = n;
      break;
    }
  }
  const stepInUnit = step / scale;
  const decimals = Math.max(0, Math.min(3, -Math.floor(Math.log10(stepInUnit) + 1e-9)));
  return (s: number) => {
    if (Math.abs(s) < step / 2) return "0";
    const sign = s < 0 ? "−" : "+";
    return `${sign}${Math.abs(s / scale).toFixed(decimals)} ${name}`;
  };
}
