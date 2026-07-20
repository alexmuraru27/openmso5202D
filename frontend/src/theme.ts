// Canvas colours, kept in sync with the CSS variables in theme.css.

export const COLORS = {
  bgPlot: "#0a0c10",
  grid: "#1a1f2a",
  gridStrong: "#262d3a",
  axisText: "#626b7c",
  laneBorder: "#20232c",
  laneLabel: "#9aa3b4",
  rail: "#38414f",
  decodeFill: "#e88aa0",
  decodeInk: "#22131a",
  decodeSub: "#c0c6d2",
  cursor: "#17b8c9",
  // Byte-slice guides: boundary lines dropped from each decoded byte onto the waveform,
  // and a faint alternating fill so adjacent byte slices read apart.
  byteGuide: "#ffafc22f",
  byteGuideFill: "rgba(202, 79, 255, 0.09)",
};

/** Trace colour for a channel. */
export function channelColor(channel: number): string {
  return channel === 1 ? "#f5c542" : channel === 2 ? "#4bc0e0" : "#b58af0";
}
