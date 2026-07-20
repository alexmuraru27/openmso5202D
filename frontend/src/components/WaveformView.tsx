import { useEffect, useMemo, useRef } from "react";
import type { CaptureResult, DecodedItem } from "../api";
import { COLORS, channelColor } from "../theme";

const GUTTER = 104; // left label rail
const AXIS = 32; // top time-axis strip
const LANE_PAD = 10; // vertical padding inside a lane

/** A time window in seconds, over the record's absolute sample time. */
interface View {
  t0: number;
  t1: number;
}

export function WaveformView({ result }: { result: CaptureResult | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const viewRef = useRef<View>({ t0: 0, t1: 1 });
  const dragRef = useRef<{ x: number } | null>(null);

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
    render(canvas, model, viewRef.current);
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

  const onPointerDown = (e: React.PointerEvent) => {
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = { x: e.clientX };
    canvasRef.current?.classList.add("dragging");
  };
  const onPointerMove = (e: React.PointerEvent) => {
    const drag = dragRef.current;
    if (!drag || !model.duration) return;
    const canvas = canvasRef.current!;
    const plotW = canvas.getBoundingClientRect().width - GUTTER;
    const v = viewRef.current;
    const span = v.t1 - v.t0;
    const deltaT = ((drag.x - e.clientX) / plotW) * span;
    viewRef.current = clampView({ t0: v.t0 + deltaT, t1: v.t1 + deltaT });
    drag.x = e.clientX;
    draw();
  };
  const endDrag = () => {
    dragRef.current = null;
    canvasRef.current?.classList.remove("dragging");
  };
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
        <button className="icon-btn" title="Fit to record" onClick={resetView}>
          ⤢
        </button>
      </div>
      <canvas
        ref={canvasRef}
        className="plot-canvas"
        onWheel={onWheel}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onDoubleClick={resetView}
      />
    </>
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

function render(canvas: HTMLCanvasElement, model: Model, view: View) {
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

  const laneH = plotH / model.lanes.length;
  model.lanes.forEach((lane, i) => {
    const y0 = plotY + i * laneH;
    drawLane(ctx, lane, model, view, timeToX, plotX, plotW, y0, laneH);
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
  const first = Math.ceil(view.t0 / step) * step;
  ctx.font = "11px ui-monospace, monospace";
  ctx.textBaseline = "middle";
  for (let t = first; t <= view.t1; t += step) {
    const x = timeToX(t);
    ctx.strokeStyle = COLORS.grid;
    ctx.beginPath();
    ctx.moveTo(x, AXIS);
    ctx.lineTo(x, H);
    ctx.stroke();
    ctx.fillStyle = COLORS.axisText;
    ctx.textAlign = "center";
    ctx.fillText(formatTime(t - model.triggerS), x, AXIS / 2);
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
  const decodeStrip = lane.decoded.length ? 26 : 0;
  const bandY = y0 + LANE_PAD + decodeStrip;
  const bandH = laneH - LANE_PAD * 2 - decodeStrip;
  const yOf = (v: number) =>
    bandY + bandH - ((v - lane.vMin) / (lane.vMax - lane.vMin)) * bandH;

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
  ctx.fillStyle = COLORS.axisText;
  ctx.font = "10px ui-monospace, monospace";
  ctx.textAlign = "left";
  ctx.fillText("H", plotX + 3, yOf(lane.vMax) + 7);
  ctx.fillText("L", plotX + 3, yOf(lane.vMin) - 6);

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

  // Decode strip.
  if (lane.decoded.length) {
    drawDecode(ctx, lane.decoded, timeToX, plotX, plotW, y0 + LANE_PAD, decodeStrip);
  }
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
    const label = item.text.replace("!", "");
    const w = Math.max(x1 - x0 - 2, ctx.measureText(label).width + 12);
    const px = cx - w / 2;
    pill(ctx, px, y, w, h, COLORS.decodeFill, item.text.includes("!"));
    ctx.fillStyle = COLORS.decodeInk;
    ctx.fillText(label, cx, y + h / 2 + 0.5);
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

function formatTime(s: number): string {
  const a = Math.abs(s);
  const sign = s < 0 ? "−" : "+";
  if (a < 1e-9) return "0";
  if (a < 1e-6) return `${sign}${(s * 1e9).toFixed(a < 1e-8 ? 1 : 0)} ns`;
  if (a < 1e-3) return `${sign}${(s * 1e6).toFixed(a < 1e-5 ? 1 : 0)} µs`;
  if (a < 1) return `${sign}${(s * 1e3).toFixed(a < 1e-2 ? 1 : 0)} ms`;
  return `${sign}${s.toFixed(2)} s`;
}
